#!/usr/bin/env python3
"""OWASP ZAP automated penetration testing for Tessera (Issue #214).

Drives OWASP ZAP (Zed Attack Proxy) against tessera-api and tessera-docs
endpoints, running both passive and active scan rules to detect OWASP Top 10
vulnerabilities.  Produces a SARIF 2.1.0 report uploadable to the GitHub
Security tab and exits non-zero when High or Critical findings are present.

Usage (with ZAP running in daemon mode):
    python tests/pen-test/zap_scan.py \
        --zap-url http://localhost:8090 \
        --api-target http://localhost:8080 \
        --docs-target http://localhost:3000 \
        --output zap-report.sarif \
        --fail-on high

Environment variables:
    ZAP_API_KEY      ZAP API key (or pass --zap-api-key)
    ZAP_API_URL      ZAP daemon address (or pass --zap-url)
"""

from __future__ import annotations

import argparse
import json
import logging
import os
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass, field
from enum import Enum
from pathlib import Path
from typing import Any, Dict, List, Optional

logger = logging.getLogger("tessera-zap-scan")


# ---------------------------------------------------------------------------
# Risk classification
# ---------------------------------------------------------------------------

class RiskLevel(str, Enum):
    CRITICAL = "Critical"
    HIGH = "High"
    MEDIUM = "Medium"
    LOW = "Low"
    INFO = "Informational"

    def sarif_level(self) -> str:
        if self in (RiskLevel.CRITICAL, RiskLevel.HIGH):
            return "error"
        if self == RiskLevel.MEDIUM:
            return "warning"
        return "note"

    @classmethod
    def from_zap(cls, risk: str) -> "RiskLevel":
        mapping = {
            "0": cls.INFO,
            "1": cls.LOW,
            "2": cls.MEDIUM,
            "3": cls.HIGH,
            "informational": cls.INFO,
            "low": cls.LOW,
            "medium": cls.MEDIUM,
            "high": cls.HIGH,
        }
        return mapping.get(risk.strip().lower(), cls.INFO)


# ---------------------------------------------------------------------------
# ZAP scan policy — OWASP Top 10 rule mapping
# ---------------------------------------------------------------------------

OWASP_SCAN_POLICY = {
    "sqli": {
        "zap_ids": [40018, 40019, 40020, 40021, 40022, 40027],
        "rule_id": "TESSERA-ZAP-001",
        "name": "SQL Injection",
        "cwe": "CWE-89",
    },
    "xss": {
        "zap_ids": [40012, 40014, 40016, 40017],
        "rule_id": "TESSERA-ZAP-002",
        "name": "Cross-Site Scripting (XSS)",
        "cwe": "CWE-79",
    },
    "ssrf": {
        "zap_ids": [40046],
        "rule_id": "TESSERA-ZAP-003",
        "name": "Server-Side Request Forgery (SSRF)",
        "cwe": "CWE-918",
    },
    "auth": {
        "zap_ids": [10101, 10105, 40036],
        "rule_id": "TESSERA-ZAP-004",
        "name": "Broken Authentication",
        "cwe": "CWE-287",
    },
    "cors": {
        "zap_ids": [40040],
        "rule_id": "TESSERA-ZAP-005",
        "name": "CORS Misconfiguration",
        "cwe": "CWE-942",
    },
    "path_traversal": {
        "zap_ids": [6],
        "rule_id": "TESSERA-ZAP-006",
        "name": "Path Traversal",
        "cwe": "CWE-22",
    },
    "command_injection": {
        "zap_ids": [90020, 90037],
        "rule_id": "TESSERA-ZAP-007",
        "name": "Remote OS Command Injection",
        "cwe": "CWE-78",
    },
    "insecure_headers": {
        "zap_ids": [10015, 10020, 10021, 10037, 10038, 10063],
        "rule_id": "TESSERA-ZAP-008",
        "name": "Security Misconfiguration (Headers)",
        "cwe": "CWE-693",
    },
}


@dataclass
class Finding:
    rule_id: str
    name: str
    risk: RiskLevel
    cwe: str
    url: str
    method: str
    param: str = ""
    evidence: str = ""
    description: str = ""
    solution: str = ""
    zap_alert_ref: str = ""

    def to_sarif_result(self) -> Dict[str, Any]:
        return {
            "ruleId": self.rule_id,
            "level": self.risk.sarif_level(),
            "message": {
                "text": (
                    f"[{self.risk.value}] {self.name}: {self.method} {self.url}"
                    + (f" (param: {self.param})" if self.param else "")
                    + f" — {self.description}"
                ),
            },
            "locations": [
                {
                    "physicalLocation": {
                        "artifactLocation": {"uri": self.url, "uriBaseId": "%SRCROOT%"},
                        "region": {"startLine": 1, "startColumn": 1},
                    }
                }
            ],
            "properties": {
                "risk": self.risk.value,
                "cwe": self.cwe,
                "method": self.method,
                "parameter": self.param,
                "solution": self.solution,
                "zap_alert_ref": self.zap_alert_ref,
            },
        }


# ---------------------------------------------------------------------------
# ZAP REST client
# ---------------------------------------------------------------------------

class ZapClient:
    """Thin wrapper around the OWASP ZAP JSON API."""

    def __init__(self, base_url: str, api_key: str = ""):
        self.base_url = base_url.rstrip("/")
        self.api_key = api_key

    def _get(self, path: str, params: Optional[Dict[str, str]] = None) -> Any:
        params = dict(params or {})
        if self.api_key:
            params["apikey"] = self.api_key
        qs = urllib.parse.urlencode(params)
        url = f"{self.base_url}{path}" + (f"?{qs}" if qs else "")
        req = urllib.request.Request(
            url, headers={"Accept": "application/json", "User-Agent": "Tessera-ZAP/1.0"}
        )
        with urllib.request.urlopen(req, timeout=30) as resp:
            return json.loads(resp.read().decode())

    # -- connectivity -------------------------------------------------------

    def version(self) -> str:
        return self._get("/JSON/core/view/version/").get("version", "")

    def is_available(self) -> bool:
        try:
            return bool(self.version())
        except Exception:
            return False

    # -- context ------------------------------------------------------------

    def new_context(self, name: str) -> str:
        return self._get("/JSON/context/action/newContext/", {"contextName": name}).get(
            "contextId", ""
        )

    def include_in_context(self, ctx_id: str, regex: str) -> None:
        self._get(
            "/JSON/context/action/includeInContext/",
            {"contextId": ctx_id, "regex": regex},
        )

    # -- spider -------------------------------------------------------------

    def spider_scan(self, url: str, ctx_id: str = "") -> str:
        params: Dict[str, str] = {"url": url, "recurse": "true"}
        if ctx_id:
            params["contextId"] = ctx_id
        return self._get("/JSON/spider/action/scan/", params).get("scan", "")

    def spider_status(self, scan_id: str) -> int:
        return int(self._get("/JSON/spider/view/status/", {"scanId": scan_id}).get("status", "0"))

    def wait_spider(self, scan_id: str, timeout: int = 120) -> bool:
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.spider_status(scan_id) >= 100:
                return True
            time.sleep(2)
        return False

    # -- passive scan -------------------------------------------------------

    def pscan_records_to_scan(self) -> int:
        return int(
            self._get("/JSON/pscan/view/recordsToScan/").get("recordsToScan", "0")
        )

    def wait_passive_scan(self, timeout: int = 120) -> bool:
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.pscan_records_to_scan() == 0:
                return True
            time.sleep(2)
        return False

    # -- active scan --------------------------------------------------------

    def active_scan(self, url: str, ctx_id: str = "", policy: str = "") -> str:
        params: Dict[str, str] = {"url": url, "recurse": "true"}
        if ctx_id:
            params["contextId"] = ctx_id
        if policy:
            params["scanPolicyName"] = policy
        return self._get("/JSON/ascan/action/scan/", params).get("scan", "")

    def active_scan_status(self, scan_id: str) -> int:
        return int(
            self._get("/JSON/ascan/view/status/", {"scanId": scan_id}).get("status", "0")
        )

    def wait_active_scan(self, scan_id: str, timeout: int = 300) -> bool:
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.active_scan_status(scan_id) >= 100:
                return True
            time.sleep(3)
        return False

    # -- scan policy --------------------------------------------------------

    def add_scan_policy(self, name: str) -> None:
        try:
            self._get("/JSON/ascan/action/addScanPolicy/", {"scanPolicyName": name})
        except urllib.error.HTTPError:
            pass

    def enable_scanners(self, policy: str, ids: List[int]) -> None:
        id_str = ",".join(str(i) for i in ids)
        try:
            self._get(
                "/JSON/ascan/action/enableScanners/",
                {"ids": id_str, "scanPolicyName": policy},
            )
        except urllib.error.HTTPError:
            pass

    # -- alerts -------------------------------------------------------------

    def alerts(self, base_url: str = "", start: int = 0, count: int = 500) -> List[Dict[str, Any]]:
        params: Dict[str, str] = {"start": str(start), "count": str(count)}
        if base_url:
            params["baseurl"] = base_url
        return self._get("/JSON/alert/view/alerts/", params).get("alerts", [])


# ---------------------------------------------------------------------------
# Alert → Finding normalisation
# ---------------------------------------------------------------------------

_ZAP_ID_TO_POLICY: Dict[int, Dict[str, Any]] = {}
for _cat in OWASP_SCAN_POLICY.values():
    for _zid in _cat["zap_ids"]:
        _ZAP_ID_TO_POLICY[_zid] = _cat


def _classify_alert(alert: Dict[str, Any]) -> Finding:
    plugin_id = int(alert.get("pluginId", alert.get("alertRef", "0")))
    policy = _ZAP_ID_TO_POLICY.get(plugin_id)
    if policy:
        rule_id = policy["rule_id"]
        name = policy["name"]
        cwe = policy["cwe"]
    else:
        rule_id = f"TESSERA-ZAP-{plugin_id}"
        name = alert.get("name", alert.get("alert", "ZAP Finding"))
        cwe = f"CWE-{alert.get('cweid', '0')}"

    return Finding(
        rule_id=rule_id,
        name=name,
        risk=RiskLevel.from_zap(alert.get("risk", "0")),
        cwe=cwe,
        url=alert.get("url", ""),
        method=alert.get("method", "GET"),
        param=alert.get("param", ""),
        evidence=alert.get("evidence", alert.get("otherinfo", "")),
        description=alert.get("description", name),
        solution=alert.get("solution", ""),
        zap_alert_ref=str(plugin_id),
    )


# ---------------------------------------------------------------------------
# SARIF report generation
# ---------------------------------------------------------------------------

def _sarif_rules() -> List[Dict[str, Any]]:
    rules = []
    for cat in OWASP_SCAN_POLICY.values():
        rules.append(
            {
                "id": cat["rule_id"],
                "name": cat["name"],
                "shortDescription": {"text": cat["name"]},
                "fullDescription": {
                    "text": f"OWASP ZAP scan rule for {cat['name']} ({cat['cwe']})."
                },
                "defaultConfiguration": {"level": "error"},
                "help": {"text": f"See {cat['cwe']} for details."},
                "properties": {"tags": ["security", "owasp-top-10", cat["cwe"].lower()]},
            }
        )
    return rules


def generate_sarif(findings: List[Finding]) -> Dict[str, Any]:
    return {
        "$schema": "https://raw.githubusercontent.com/oasis-tcs/sarif-spec/master/Schemata/sarif-schema-2.1.0.json",
        "version": "2.1.0",
        "runs": [
            {
                "tool": {
                    "driver": {
                        "name": "Tessera-OWASP-ZAP",
                        "organization": "A4-Stellar",
                        "semanticVersion": "1.0.0",
                        "informationUri": "https://github.com/A4-Stellar/Tessera",
                        "rules": _sarif_rules(),
                    }
                },
                "results": [f.to_sarif_result() for f in findings],
                "columnKind": "utf16CodeUnits",
            }
        ],
    }


# ---------------------------------------------------------------------------
# Scan orchestrator
# ---------------------------------------------------------------------------

SCAN_POLICY_NAME = "tessera-owasp-top10"

API_SEED_URLS = [
    "/health",
    "/stats",
    "/assets",
    "/v1/ws",
]

DOCS_SEED_URLS = [
    "/",
    "/en",
    "/en/getting-started",
]


def configure_scan_policy(zap: ZapClient) -> None:
    zap.add_scan_policy(SCAN_POLICY_NAME)
    all_ids: List[int] = []
    for cat in OWASP_SCAN_POLICY.values():
        all_ids.extend(cat["zap_ids"])
    zap.enable_scanners(SCAN_POLICY_NAME, all_ids)
    logger.info("Configured ZAP scan policy '%s' with %d scanner IDs", SCAN_POLICY_NAME, len(all_ids))


def scan_target(
    zap: ZapClient,
    target_url: str,
    label: str,
    seed_paths: List[str],
    spider_timeout: int = 120,
    passive_timeout: int = 120,
    active_timeout: int = 300,
) -> List[Finding]:
    target = target_url.rstrip("/")
    logger.info("[%s] Creating scan context for %s", label, target)

    ctx_id = zap.new_context(f"tessera-{label}")
    zap.include_in_context(ctx_id, f"{target}.*")

    # Seed ZAP with known endpoints so the spider has starting points.
    for path in seed_paths:
        seed_url = f"{target}{path}"
        try:
            urllib.request.urlopen(
                urllib.request.Request(seed_url, headers={"User-Agent": "Tessera-ZAP/1.0"}),
                timeout=5,
            )
        except Exception:
            pass

    # Phase 1: Spider
    logger.info("[%s] Starting spider ...", label)
    spider_id = zap.spider_scan(target, ctx_id)
    if spider_id:
        zap.wait_spider(spider_id, spider_timeout)
    logger.info("[%s] Spider complete", label)

    # Phase 2: Passive scan (runs automatically on spidered traffic)
    logger.info("[%s] Waiting for passive scan to finish ...", label)
    zap.wait_passive_scan(passive_timeout)
    logger.info("[%s] Passive scan complete", label)

    # Phase 3: Active scan with OWASP Top 10 policy
    logger.info("[%s] Starting active scan ...", label)
    ascan_id = zap.active_scan(target, ctx_id, SCAN_POLICY_NAME)
    if ascan_id:
        zap.wait_active_scan(ascan_id, active_timeout)
    logger.info("[%s] Active scan complete", label)

    # Collect alerts
    raw_alerts = zap.alerts(base_url=target)
    findings = [_classify_alert(a) for a in raw_alerts]
    logger.info("[%s] Collected %d findings", label, len(findings))
    return findings


def should_fail(findings: List[Finding], threshold: str) -> bool:
    threshold = threshold.strip().lower()
    if threshold == "none":
        return False
    levels = {
        "critical": {RiskLevel.CRITICAL},
        "high": {RiskLevel.CRITICAL, RiskLevel.HIGH},
        "medium": {RiskLevel.CRITICAL, RiskLevel.HIGH, RiskLevel.MEDIUM},
        "low": {RiskLevel.CRITICAL, RiskLevel.HIGH, RiskLevel.MEDIUM, RiskLevel.LOW},
    }
    blocking = levels.get(threshold, levels["high"])
    return any(f.risk in blocking for f in findings)


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def parse_args(argv: Optional[List[str]] = None) -> argparse.Namespace:
    p = argparse.ArgumentParser(description="Tessera OWASP ZAP penetration testing suite.")
    p.add_argument("--zap-url", default=os.environ.get("ZAP_API_URL", "http://localhost:8090"))
    p.add_argument("--zap-api-key", default=os.environ.get("ZAP_API_KEY", ""))
    p.add_argument("--api-target", default="http://localhost:8080", help="tessera-api base URL")
    p.add_argument("--docs-target", default="", help="tessera-docs base URL (omit to skip)")
    p.add_argument("--output", "-o", default="zap-report.sarif")
    p.add_argument(
        "--fail-on",
        choices=["critical", "high", "medium", "low", "none"],
        default="high",
    )
    p.add_argument("--spider-timeout", type=int, default=120)
    p.add_argument("--active-timeout", type=int, default=300)
    p.add_argument("--verbose", "-v", action="store_true")
    return p.parse_args(argv)


def main(argv: Optional[List[str]] = None) -> int:
    args = parse_args(argv)
    logging.basicConfig(
        level=logging.DEBUG if args.verbose else logging.INFO,
        format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
    )

    print(f"[*] Tessera OWASP ZAP Penetration Test")
    print(f"[*] ZAP:         {args.zap_url}")
    print(f"[*] API target:  {args.api_target}")
    print(f"[*] Docs target: {args.docs_target or '(skipped)'}")
    print(f"[*] Fail-on:     {args.fail_on}")

    zap = ZapClient(args.zap_url, args.zap_api_key)
    if not zap.is_available():
        print(f"[!] ZAP not reachable at {args.zap_url} — aborting.")
        return 2

    print(f"[+] Connected to ZAP {zap.version()}")
    configure_scan_policy(zap)

    all_findings: List[Finding] = []

    # Scan tessera-api
    all_findings.extend(
        scan_target(
            zap,
            args.api_target,
            label="api",
            seed_paths=API_SEED_URLS,
            spider_timeout=args.spider_timeout,
            active_timeout=args.active_timeout,
        )
    )

    # Scan tessera-docs (if provided)
    if args.docs_target:
        all_findings.extend(
            scan_target(
                zap,
                args.docs_target,
                label="docs",
                seed_paths=DOCS_SEED_URLS,
                spider_timeout=args.spider_timeout,
                active_timeout=args.active_timeout,
            )
        )

    # Deduplicate by (rule_id, url, param)
    seen: set = set()
    unique: List[Finding] = []
    for f in all_findings:
        key = (f.rule_id, f.url, f.param)
        if key not in seen:
            seen.add(key)
            unique.append(f)
    all_findings = unique

    sarif = generate_sarif(all_findings)
    out = Path(args.output)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(sarif, indent=2) + "\n", encoding="utf-8")

    by_risk = {r: 0 for r in RiskLevel}
    for f in all_findings:
        by_risk[f.risk] += 1

    print(f"\n[+] Scan complete — {len(all_findings)} unique findings")
    print(f"    Critical: {by_risk[RiskLevel.CRITICAL]}")
    print(f"    High:     {by_risk[RiskLevel.HIGH]}")
    print(f"    Medium:   {by_risk[RiskLevel.MEDIUM]}")
    print(f"    Low:      {by_risk[RiskLevel.LOW]}")
    print(f"    Info:     {by_risk[RiskLevel.INFO]}")
    print(f"[+] SARIF report: {out.resolve()}")

    for f in all_findings:
        if f.risk in (RiskLevel.CRITICAL, RiskLevel.HIGH):
            print(f"  [{f.risk.value}] {f.rule_id}: {f.method} {f.url}")

    if should_fail(all_findings, args.fail_on):
        print(f"\n[!] FAILED: findings at or above '{args.fail_on}' threshold.")
        return 1

    print(f"\n[OK] PASSED: no blocking findings.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
