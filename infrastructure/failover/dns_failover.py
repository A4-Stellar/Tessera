#!/usr/bin/env python3
"""Tessera multi-cloud DNS failover (AWS primary -> GCP secondary) — Issue #256.

Two roles, one file:

  agent       A lightweight probe agent deployed in several regions/clouds.
              ``GET /probe`` synthetically probes the AWS origin and returns
              the verdict as JSON. Agents are the "global monitoring agents".

  controller  Every ``FAILOVER_PROBE_INTERVAL_S`` it fans out one probe round
              (its own local probe + every remote agent, in parallel). When a
              strict majority of responding vantage points report the AWS
              origin down for ``FAILOVER_FAILURE_THRESHOLD`` consecutive
              rounds, and the GCP origin is healthy, it rewrites the
              Cloudflare or Route53 record to the GCP target and notifies
              PagerDuty + Slack. After ``FAILOVER_RECOVERY_THRESHOLD``
              consecutive healthy rounds it fails back and resolves the page.

Worst-case detection-to-DNS-update time is bounded at startup
(interval * failure_threshold + agent timeout + DNS timeout <= 60s).

Complexity per round with V vantage points: O(V) time (wall clock bounded by
one probe timeout because probes run concurrently), O(V) transient space.
Controller state is O(1): two streak counters and the active side.

Run the controller outside AWS (e.g. on GCP) so it survives the outage it
is meant to handle. Configuration is environment-only; see ``Config``.
"""

from __future__ import annotations

import argparse
import hmac
import json
import logging
import os
import signal
import sys
import threading
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
from enum import Enum
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Optional, Protocol

LOG = logging.getLogger("tessera.dns_failover")
USER_AGENT = "tessera-dns-failover/1.0"
FAILOVER_BUDGET_S = 60
CLOUDFLARE_API = "https://api.cloudflare.com/client/v4"
PAGERDUTY_EVENTS_URL = "https://events.pagerduty.com/v2/enqueue"


class ConfigError(ValueError):
    """Raised when the environment does not describe a valid deployment."""


# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------


def _env(name: str, default: Optional[str] = None) -> str:
    value = os.environ.get(name, default)
    if value is None or value == "":
        raise ConfigError(f"{name} is required")
    return value


def _env_int(name: str, default: int) -> int:
    try:
        return int(os.environ.get(name, default))
    except ValueError as exc:
        raise ConfigError(f"{name} must be an integer") from exc


@dataclass(frozen=True)
class Config:
    primary_health_url: str
    probe_timeout_s: int
    # Controller-only fields (empty/zero for agents).
    secondary_health_url: str = ""
    primary_target: str = ""
    secondary_target: str = ""
    dns_provider: str = ""
    agent_urls: tuple[str, ...] = ()
    agent_token: str = ""
    probe_interval_s: int = 10
    failure_threshold: int = 3
    recovery_threshold: int = 30
    dns_timeout_s: int = 5
    slack_webhook_url: str = ""
    pagerduty_routing_key: str = ""

    @property
    def agent_timeout_s(self) -> int:
        # An agent needs one full probe timeout plus network overhead.
        return self.probe_timeout_s + 2

    @classmethod
    def for_agent(cls) -> "Config":
        return cls(
            primary_health_url=_env("FAILOVER_PRIMARY_HEALTH_URL"),
            probe_timeout_s=_env_int("FAILOVER_PROBE_TIMEOUT_S", 5),
            agent_token=_env("FAILOVER_AGENT_TOKEN"),
        )

    @classmethod
    def for_controller(cls) -> "Config":
        agents = os.environ.get("FAILOVER_AGENT_URLS", "")
        agent_urls = tuple(u.strip() for u in agents.split(",") if u.strip())
        cfg = cls(
            primary_health_url=_env("FAILOVER_PRIMARY_HEALTH_URL"),
            secondary_health_url=_env("FAILOVER_SECONDARY_HEALTH_URL"),
            primary_target=_env("FAILOVER_PRIMARY_TARGET"),
            secondary_target=_env("FAILOVER_SECONDARY_TARGET"),
            dns_provider=_env("FAILOVER_DNS_PROVIDER").lower(),
            agent_urls=agent_urls,
            agent_token=_env("FAILOVER_AGENT_TOKEN") if agent_urls else "",
            probe_interval_s=_env_int("FAILOVER_PROBE_INTERVAL_S", 10),
            probe_timeout_s=_env_int("FAILOVER_PROBE_TIMEOUT_S", 5),
            failure_threshold=_env_int("FAILOVER_FAILURE_THRESHOLD", 3),
            recovery_threshold=_env_int("FAILOVER_RECOVERY_THRESHOLD", 30),
            dns_timeout_s=_env_int("FAILOVER_DNS_TIMEOUT_S", 5),
            slack_webhook_url=os.environ.get("SLACK_WEBHOOK_URL", ""),
            pagerduty_routing_key=os.environ.get("PAGERDUTY_ROUTING_KEY", ""),
        )
        cfg.validate()
        return cfg

    def validate(self) -> None:
        if self.dns_provider not in ("cloudflare", "route53"):
            raise ConfigError("FAILOVER_DNS_PROVIDER must be 'cloudflare' or 'route53'")
        if min(self.probe_interval_s, self.probe_timeout_s, self.failure_threshold,
               self.recovery_threshold, self.dns_timeout_s) < 1:
            raise ConfigError("intervals, timeouts and thresholds must be >= 1")
        if self.probe_timeout_s >= self.probe_interval_s:
            raise ConfigError("FAILOVER_PROBE_TIMEOUT_S must be < FAILOVER_PROBE_INTERVAL_S")
        worst_case = (self.probe_interval_s * self.failure_threshold
                      + self.agent_timeout_s + self.dns_timeout_s)
        if worst_case > FAILOVER_BUDGET_S:
            raise ConfigError(
                f"worst-case failover time {worst_case}s exceeds the "
                f"{FAILOVER_BUDGET_S}s budget; lower the interval or threshold"
            )


# ---------------------------------------------------------------------------
# HTTP plumbing (stdlib only)
# ---------------------------------------------------------------------------


def http_request(method: str, url: str, *, timeout: float,
                 headers: Optional[dict[str, str]] = None,
                 body: Optional[dict] = None) -> tuple[int, bytes]:
    """Return (status, body). HTTP error statuses are returned, not raised;
    transport failures (DNS, TCP, TLS, timeout) raise OSError."""
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(url, data=data, method=method)
    request.add_header("User-Agent", USER_AGENT)
    if data is not None:
        request.add_header("Content-Type", "application/json")
    for name, value in (headers or {}).items():
        request.add_header(name, value)
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            return response.status, response.read()
    except urllib.error.HTTPError as exc:
        return exc.code, exc.read()


@dataclass(frozen=True)
class ProbeResult:
    healthy: bool
    latency_ms: int
    detail: str


def probe_http(url: str, timeout: float) -> ProbeResult:
    """Synthetic probe: healthy iff the endpoint answers 2xx within timeout."""
    started = time.monotonic()
    try:
        status, _ = http_request("GET", url, timeout=timeout)
        healthy, detail = 200 <= status < 300, f"HTTP {status}"
    except OSError as exc:  # URLError, socket.timeout and ssl errors are OSErrors
        healthy, detail = False, f"{type(exc).__name__}: {exc}"
    return ProbeResult(healthy, int((time.monotonic() - started) * 1000), detail)


# ---------------------------------------------------------------------------
# Vantage points
# ---------------------------------------------------------------------------


class Vantage(Protocol):
    name: str

    def check(self) -> Optional[bool]:
        """True = primary healthy, False = down, None = vantage unavailable."""


@dataclass(frozen=True)
class LocalVantage:
    name: str
    url: str
    timeout_s: int

    def check(self) -> Optional[bool]:
        result = probe_http(self.url, self.timeout_s)
        LOG.debug("%s -> %s (%sms)", self.name, result.detail, result.latency_ms)
        return result.healthy


@dataclass(frozen=True)
class RemoteAgentVantage:
    name: str
    agent_url: str
    token: str
    timeout_s: int

    def check(self) -> Optional[bool]:
        # An unreachable agent says nothing about AWS: abstain instead of
        # voting "down", so agent outages can never trigger a failover.
        try:
            status, body = http_request(
                "GET", self.agent_url, timeout=self.timeout_s,
                headers={"Authorization": f"Bearer {self.token}"},
            )
            if status != 200:
                LOG.warning("agent %s answered HTTP %s; abstaining", self.name, status)
                return None
            return bool(json.loads(body)["healthy"])
        except (OSError, ValueError, KeyError) as exc:
            LOG.warning("agent %s unavailable (%s); abstaining", self.name, exc)
            return None


def majority_down(votes: list[Optional[bool]]) -> bool:
    """Strict majority of the vantage points that answered report "down"."""
    answered = [v for v in votes if v is not None]
    failing = answered.count(False)
    return bool(answered) and failing > len(answered) // 2


# ---------------------------------------------------------------------------
# DNS providers
# ---------------------------------------------------------------------------


class DnsError(RuntimeError):
    pass


class DnsProvider(Protocol):
    record_name: str

    def current_target(self) -> str: ...

    def point_to(self, target: str) -> None: ...


def _same_host(a: str, b: str) -> bool:
    return a.rstrip(".").lower() == b.rstrip(".").lower()


class CloudflareDns:
    """PATCH /zones/{zone_id}/dns_records/{record_id}.

    The record's type, name, TTL and proxy flag are read once and re-sent
    unchanged so only ``content`` moves. For a proxied record resolvers only
    ever cache Cloudflare's anycast IPs, so the origin change is applied by
    Cloudflare rather than waiting on resolver TTLs; an unproxied record is
    bounded by its TTL.
    """

    def __init__(self, zone_id: str, record_id: str, api_token: str, timeout_s: int):
        self._url = f"{CLOUDFLARE_API}/zones/{zone_id}/dns_records/{record_id}"
        self._headers = {"Authorization": f"Bearer {api_token}"}
        self._timeout = timeout_s
        record = self._call("GET")
        self.record_name: str = record["name"]
        self._static = {k: record[k] for k in ("type", "name", "ttl", "proxied")}

    def _call(self, method: str, body: Optional[dict] = None) -> dict:
        try:
            status, raw = http_request(method, self._url, timeout=self._timeout,
                                       headers=self._headers, body=body)
            payload = json.loads(raw)
        except (OSError, ValueError) as exc:
            raise DnsError(f"Cloudflare {method} failed: {exc}") from exc
        if status != 200 or not payload.get("success"):
            raise DnsError(f"Cloudflare {method} HTTP {status}: {payload.get('errors')}")
        return payload["result"]

    def current_target(self) -> str:
        return self._call("GET")["content"]

    def point_to(self, target: str) -> None:
        self._call("PATCH", {**self._static, "content": target})


class Route53Dns:
    """ChangeResourceRecordSets UPSERT of a single non-alias record."""

    def __init__(self, hosted_zone_id: str, record_name: str, record_type: str,
                 ttl: int, timeout_s: int):
        import boto3  # Imported lazily: only Route53 deployments need it.
        from botocore.config import Config as BotoConfig

        self._client = boto3.client("route53", config=BotoConfig(
            connect_timeout=timeout_s, read_timeout=timeout_s,
            retries={"max_attempts": 2, "mode": "standard"},
        ))
        self._zone = hosted_zone_id
        self.record_name = record_name.rstrip(".") + "."
        self._type = record_type
        self._ttl = ttl

    def current_target(self) -> str:
        from botocore.exceptions import BotoCoreError, ClientError

        try:
            sets = self._client.list_resource_record_sets(
                HostedZoneId=self._zone, StartRecordName=self.record_name,
                StartRecordType=self._type, MaxItems="1",
            )["ResourceRecordSets"]
        except (BotoCoreError, ClientError) as exc:
            raise DnsError(f"Route53 list failed: {exc}") from exc
        if (not sets or not _same_host(sets[0]["Name"], self.record_name)
                or sets[0]["Type"] != self._type or "ResourceRecords" not in sets[0]):
            raise DnsError(f"{self._type} {self.record_name} not found (alias records are unsupported)")
        return sets[0]["ResourceRecords"][0]["Value"]

    def point_to(self, target: str) -> None:
        from botocore.exceptions import BotoCoreError, ClientError

        try:
            self._client.change_resource_record_sets(
                HostedZoneId=self._zone,
                ChangeBatch={
                    "Comment": f"tessera dns_failover -> {target}",
                    "Changes": [{
                        "Action": "UPSERT",
                        "ResourceRecordSet": {
                            "Name": self.record_name, "Type": self._type, "TTL": self._ttl,
                            "ResourceRecords": [{"Value": target}],
                        },
                    }],
                },
            )
        except (BotoCoreError, ClientError) as exc:
            raise DnsError(f"Route53 UPSERT failed: {exc}") from exc


def build_dns_provider(cfg: Config) -> DnsProvider:
    if cfg.dns_provider == "cloudflare":
        return CloudflareDns(_env("CLOUDFLARE_ZONE_ID"), _env("CLOUDFLARE_RECORD_ID"),
                             _env("CLOUDFLARE_API_TOKEN"), cfg.dns_timeout_s)
    return Route53Dns(_env("ROUTE53_HOSTED_ZONE_ID"), _env("FAILOVER_RECORD_NAME"),
                      os.environ.get("FAILOVER_RECORD_TYPE", "CNAME"),
                      _env_int("FAILOVER_RECORD_TTL", 30), cfg.dns_timeout_s)


# ---------------------------------------------------------------------------
# Incident notifications
# ---------------------------------------------------------------------------


class Notifier:
    """Fans incidents out to Slack and PagerDuty. Never raises: a notification
    outage must not interfere with traffic management."""

    def __init__(self, slack_webhook_url: str, pagerduty_routing_key: str,
                 dedup_key: str, timeout_s: int):
        self._slack = slack_webhook_url
        self._pd_key = pagerduty_routing_key
        self._dedup = dedup_key
        self._timeout = timeout_s

    def trigger(self, summary: str, details: dict) -> None:
        self._slack_post(f":rotating_light: *{summary}*", details)
        self._pagerduty("trigger", summary, details)

    def resolve(self, summary: str, details: dict) -> None:
        self._slack_post(f":white_check_mark: *{summary}*", details)
        self._pagerduty("resolve", summary, details)

    def _slack_post(self, headline: str, details: dict) -> None:
        if not self._slack:
            return
        lines = "\n".join(f"• {k}: `{v}`" for k, v in details.items())
        self._send("Slack", self._slack, {"text": f"{headline}\n{lines}"}, ok=(200,))

    def _pagerduty(self, action: str, summary: str, details: dict) -> None:
        if not self._pd_key:
            return
        event: dict = {"routing_key": self._pd_key, "event_action": action,
                       "dedup_key": self._dedup}
        if action == "trigger":
            event["payload"] = {
                "summary": summary[:1024], "source": details.get("record", "tessera"),
                "severity": "critical", "component": "dns-failover",
                "group": "tessera-infrastructure", "custom_details": details,
            }
        self._send("PagerDuty", PAGERDUTY_EVENTS_URL, event, ok=(202,))

    def _send(self, channel: str, url: str, body: dict, ok: tuple[int, ...]) -> None:
        try:
            status, raw = http_request("POST", url, timeout=self._timeout, body=body)
            if status not in ok:
                LOG.error("%s notification rejected: HTTP %s %s", channel, status, raw[:200])
        except OSError as exc:
            LOG.error("%s notification failed: %s", channel, exc)


# ---------------------------------------------------------------------------
# Controller
# ---------------------------------------------------------------------------


class Side(Enum):
    PRIMARY = "aws"
    SECONDARY = "gcp"


class FailoverController:
    def __init__(self, cfg: Config, dns: DnsProvider, notifier: Notifier,
                 vantages: list[Vantage], secondary: Vantage):
        self._cfg = cfg
        self._dns = dns
        self._notify = notifier
        self._vantages = vantages
        self._secondary = secondary
        self._pool = ThreadPoolExecutor(max_workers=len(vantages) + 1,
                                        thread_name_prefix="probe")
        # Separate single worker so slow notification endpoints never delay probes.
        self._notify_pool = ThreadPoolExecutor(max_workers=1, thread_name_prefix="notify")
        self._active = self._reconcile()
        self._fail_streak = 0
        self._ok_streak = 0
        self._first_failure_at: Optional[float] = None
        self._failure_paged = False

    def _reconcile(self) -> Side:
        """Recover the active side from DNS so restarts never flap traffic."""
        current = self._dns.current_target()
        if _same_host(current, self._cfg.secondary_target):
            side = Side.SECONDARY
        elif _same_host(current, self._cfg.primary_target):
            side = Side.PRIMARY
        else:
            raise ConfigError(f"{self._dns.record_name} points to unmanaged target {current!r}")
        LOG.info("%s currently routes to %s (%s)", self._dns.record_name, side.value, current)
        return side

    def run(self, stop: threading.Event) -> None:
        LOG.info("controller started: %d vantage points, interval=%ss, threshold=%d",
                 len(self._vantages), self._cfg.probe_interval_s, self._cfg.failure_threshold)
        try:
            while not stop.is_set():
                started = time.monotonic()
                self.tick()
                stop.wait(max(0.0, self._cfg.probe_interval_s - (time.monotonic() - started)))
        finally:
            self._pool.shutdown(wait=False, cancel_futures=True)
            self._notify_pool.shutdown(wait=True)

    def tick(self) -> None:
        """One probe round: all vantages and the secondary probe run concurrently."""
        secondary_future = self._pool.submit(self._secondary.check)
        votes = list(self._pool.map(lambda v: v.check(), self._vantages))
        primary_down = majority_down(votes)
        secondary_ok = secondary_future.result() is True
        LOG.debug("votes=%s primary_down=%s secondary_ok=%s", votes, primary_down, secondary_ok)

        if primary_down:
            self._ok_streak = 0
            self._fail_streak += 1
            if self._first_failure_at is None:
                self._first_failure_at = time.monotonic()
        else:
            self._fail_streak = 0
            self._ok_streak += 1
            self._first_failure_at = None
            self._failure_paged = False

        if self._active is Side.PRIMARY and self._fail_streak >= self._cfg.failure_threshold:
            self._fail_over(votes, secondary_ok)
        elif self._active is Side.SECONDARY and self._ok_streak >= self._cfg.recovery_threshold:
            self._fail_back()

    def _details(self, votes: Optional[list[Optional[bool]]] = None) -> dict:
        details = {"record": self._dns.record_name, "primary": self._cfg.primary_target,
                   "secondary": self._cfg.secondary_target}
        if votes is not None:
            details["vantage_votes"] = {v.name: vote for v, vote in zip(self._vantages, votes)}
        return details

    def _fail_over(self, votes: list[Optional[bool]], secondary_ok: bool) -> None:
        details = self._details(votes)
        if not secondary_ok:
            LOG.critical("AWS is down but GCP is unhealthy; failover blocked")
            self._page_once("AWS primary down and GCP secondary unhealthy: "
                            "DNS failover blocked", details)
            return
        try:
            self._dns.point_to(self._cfg.secondary_target)
        except DnsError as exc:
            LOG.critical("failover DNS update failed (retrying next round): %s", exc)
            self._page_once("DNS failover to GCP FAILED", {**details, "error": str(exc)})
            return
        elapsed = time.monotonic() - (self._first_failure_at or time.monotonic())
        details["seconds_since_first_failed_round"] = round(elapsed, 1)
        self._active = Side.SECONDARY
        LOG.warning("failed over %s to GCP (%s) %.1fs after first failure",
                    self._dns.record_name, self._cfg.secondary_target, elapsed)
        self._notify_pool.submit(self._notify.trigger,
                                 "AWS outage: traffic failed over to GCP", details)

    def _fail_back(self) -> None:
        try:
            self._dns.point_to(self._cfg.primary_target)
        except DnsError as exc:
            LOG.error("failback DNS update failed (will retry): %s", exc)
            return
        self._active = Side.PRIMARY
        LOG.warning("failed back %s to AWS (%s)", self._dns.record_name, self._cfg.primary_target)
        self._notify_pool.submit(self._notify.resolve,
                                 "AWS recovered: traffic failed back to AWS", self._details())

    def _page_once(self, summary: str, details: dict) -> None:
        """Page once per outage for conditions that persist across rounds."""
        if not self._failure_paged:
            self._notify_pool.submit(self._notify.trigger, summary, details)
            self._failure_paged = True


def run_controller(stop: threading.Event) -> None:
    cfg = Config.for_controller()
    vantages: list[Vantage] = [LocalVantage("controller", cfg.primary_health_url, cfg.probe_timeout_s)]
    vantages += [RemoteAgentVantage(url, url, cfg.agent_token, cfg.agent_timeout_s)
                 for url in cfg.agent_urls]
    dns = build_dns_provider(cfg)
    notifier = Notifier(cfg.slack_webhook_url, cfg.pagerduty_routing_key,
                        dedup_key=f"tessera-dns-failover-{dns.record_name}",
                        timeout_s=cfg.dns_timeout_s)
    secondary = LocalVantage("gcp-secondary", cfg.secondary_health_url, cfg.probe_timeout_s)
    FailoverController(cfg, dns, notifier, vantages, secondary).run(stop)


# ---------------------------------------------------------------------------
# Probe agent
# ---------------------------------------------------------------------------


def run_agent(stop: threading.Event) -> None:
    cfg = Config.for_agent()
    expected = f"Bearer {cfg.agent_token}".encode()
    name = os.environ.get("FAILOVER_AGENT_NAME", os.uname().nodename)

    class Handler(BaseHTTPRequestHandler):
        server_version = USER_AGENT

        def do_GET(self) -> None:  # noqa: N802 (http.server API)
            if self.path != "/probe":
                return self._reply(404, {"error": "not found"})
            supplied = self.headers.get("Authorization", "").encode()
            if not hmac.compare_digest(supplied, expected):
                return self._reply(401, {"error": "unauthorized"})
            result = probe_http(cfg.primary_health_url, cfg.probe_timeout_s)
            self._reply(200, {"agent": name, "healthy": result.healthy,
                              "latency_ms": result.latency_ms, "detail": result.detail})

        def _reply(self, status: int, body: dict) -> None:
            raw = json.dumps(body).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)

        def log_message(self, fmt: str, *args) -> None:
            LOG.debug("agent %s - " + fmt, self.address_string(), *args)

    port = _env_int("PORT", 8080)
    server = ThreadingHTTPServer(("0.0.0.0", port), Handler)
    server.daemon_threads = True
    threading.Thread(target=lambda: (stop.wait(), server.shutdown()), daemon=True).start()
    LOG.info("probe agent %s listening on :%d", name, port)
    server.serve_forever()


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    parser.add_argument("role", choices=("controller", "agent"))
    args = parser.parse_args(argv)
    logging.basicConfig(
        level=os.environ.get("FAILOVER_LOG_LEVEL", "INFO").upper(),
        format="%(asctime)s %(levelname)s %(name)s %(message)s",
    )
    stop = threading.Event()
    for sig in (signal.SIGTERM, signal.SIGINT):
        signal.signal(sig, lambda *_: stop.set())
    try:
        (run_controller if args.role == "controller" else run_agent)(stop)
    except (ConfigError, DnsError) as exc:
        LOG.critical("%s", exc)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
