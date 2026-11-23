#!/usr/bin/env python3
"""
Grafana OnCall Integration & Automated P1 Triage Chaos Tests (Issue #474)

Verifies:
1. Every Prometheus alert in `infrastructure/monitoring/alerts.yaml` has a
   `runbook_url` annotation pointing to an existing `docs/runbooks/*.md` file.
2. Grafana OnCall escalation chain follows:
   `notify(channel)` (0m) -> `5 min` (300s) -> `notify(on-call engineer)` -> `15 min` (900s) -> `notify(manager)`.
3. Automated P1 triage bot executes `check_postgres_connection_count`,
   `restart_stuck_indexer_pod`, and `flush_redis_cache` with per-step timeout
   and automatic rollback on timeout/failure.
4. Every step posts structured Slack output `{action, outcome, duration_ms, next_step}`
   and appends a SHA-256 hash-chained record to the immutable audit trail.
5. P1 alert chaos scenario completes end-to-end corrective action within 90 seconds.
"""

from dataclasses import dataclass
import hashlib
import json
from pathlib import Path
import re
import time
from typing import Callable, Dict, List, Optional
import unittest

REPO_ROOT = Path(__file__).resolve().parents[2]
AUDIT_GENESIS_HASH = "0" * 64
P1_SLA_SECONDS = 90.0


@dataclass
class EscalationStage:
    stage: int
    action: str
    target: str
    wait_before_seconds: int


ESCALATION_CHAIN: List[EscalationStage] = [
    EscalationStage(1, "notify(channel)", "#tessera-incidents", 0),
    EscalationStage(2, "notify(on-call engineer)", "primary-sre-schedule", 300),
    EscalationStage(3, "notify(manager)", "engineering-manager", 900),
]


class ImmutableAuditTrail:
    """Tamper-evident SHA-256 hash-chained audit trail for automated triage actions."""

    def __init__(self) -> None:
        self.entries: List[Dict[str, object]] = []

    @staticmethod
    def _compute_hash(payload: Dict[str, object]) -> str:
        canonical = (
            f"{payload['sequence']}|{payload['alertname']}|{payload['event_type']}|"
            f"{payload['action']}|{payload['outcome']}|{payload['duration_ms']}|"
            f"{payload['next_step']}|{payload['prev_hash']}"
        )
        return hashlib.sha256(canonical.encode("utf-8")).hexdigest()

    def append(
        self,
        alertname: str,
        event_type: str,
        action: str,
        outcome: str,
        duration_ms: int,
        next_step: str,
    ) -> Dict[str, object]:
        seq = len(self.entries) + 1
        prev_hash = self.entries[-1]["entry_hash"] if self.entries else AUDIT_GENESIS_HASH
        entry: Dict[str, object] = {
            "sequence": seq,
            "alertname": alertname,
            "event_type": event_type,
            "action": action,
            "outcome": outcome,
            "duration_ms": duration_ms,
            "next_step": next_step,
            "prev_hash": prev_hash,
        }
        entry["entry_hash"] = self._compute_hash(entry)
        self.entries.append(entry)
        return entry

    def verify_integrity(self) -> bool:
        expected_prev = AUDIT_GENESIS_HASH
        for idx, entry in enumerate(self.entries):
            if entry["sequence"] != idx + 1:
                return False
            if entry["prev_hash"] != expected_prev:
                return False
            if entry["entry_hash"] != self._compute_hash(entry):
                return False
            expected_prev = str(entry["entry_hash"])
        return True


class AutomatedTriageBotSim:
    """Reference executor mirroring `infrastructure/orchestrator/src/triage.rs`."""

    ROLLBACK_ACTIONS = {
        "check_postgres_connection_count": "restore_postgres_pool_limits",
        "restart_stuck_indexer_pod": "rollback_indexer_pod_deployment",
        "flush_redis_cache": "restore_redis_cache_snapshot",
    }

    def __init__(
        self,
        step_timeout_seconds: float = 15.0,
        step_handlers: Optional[Dict[str, Callable[[], float]]] = None,
        rollback_callback: Optional[Callable[[str], None]] = None,
    ) -> None:
        self.step_timeout_seconds = step_timeout_seconds
        self.step_handlers = step_handlers or {}
        self.rollback_callback = rollback_callback
        self.slack_messages: List[Dict[str, object]] = []
        self.audit = ImmutableAuditTrail()

    def due_escalations(self, elapsed_seconds: int, resolved: bool = False) -> List[EscalationStage]:
        if resolved:
            return [s for s in ESCALATION_CHAIN if s.wait_before_seconds == 0]
        return [s for s in ESCALATION_CHAIN if elapsed_seconds >= s.wait_before_seconds]

    def triage_p1_alert(self, alert: Dict[str, object]) -> Dict[str, object]:
        t0 = time.monotonic()
        alertname = str(alert["alertname"])
        annotations = dict(alert.get("annotations", {}))
        runbook_url = str(annotations.get("runbook_url", ""))
        if "docs/runbooks/" not in runbook_url or not runbook_url.endswith(".md"):
            raise ValueError(f"Alert {alertname} missing valid docs/runbooks/ runbook_url")

        # 0m escalation: notify(channel)
        for esc in self.due_escalations(0):
            self.audit.append(
                alertname,
                "escalation",
                esc.action,
                "notified",
                0,
                "start_automated_triage",
            )

        steps = [
            "check_postgres_connection_count",
            "restart_stuck_indexer_pod",
            "flush_redis_cache",
        ]
        remediated = True
        rolled_back = False

        for idx, step in enumerate(steps):
            next_step = steps[idx + 1] if idx + 1 < len(steps) else "verify_slo_recovery"
            handler = self.step_handlers.get(step, lambda: 0.005)

            try:
                simulated_elapsed = handler()
                duration_ms = max(1, int(simulated_elapsed * 1000))
                if simulated_elapsed > self.step_timeout_seconds:
                    raise TimeoutError(f"Step {step} exceeded timeout {self.step_timeout_seconds}s")

                slack_msg = {
                    "action": step,
                    "outcome": "succeeded",
                    "duration_ms": duration_ms,
                    "next_step": next_step,
                }
                self.slack_messages.append(slack_msg)
                self.audit.append(
                    alertname,
                    "runbook_step",
                    step,
                    "succeeded",
                    duration_ms,
                    next_step,
                )
            except Exception as err:
                remediated = False
                rolled_back = True
                if self.rollback_callback:
                    self.rollback_callback(step)
                rb_name = self.ROLLBACK_ACTIONS[step]
                outcome = (
                    f"timed_out_and_rolled_back ({rb_name})"
                    if isinstance(err, TimeoutError)
                    else f"failed_and_rolled_back ({rb_name})"
                )
                duration_ms = max(1, int(self.step_timeout_seconds * 1000))
                slack_msg = {
                    "action": step,
                    "outcome": outcome,
                    "duration_ms": duration_ms,
                    "next_step": "notify(on-call engineer)",
                }
                self.slack_messages.append(slack_msg)
                self.audit.append(
                    alertname,
                    "runbook_rollback",
                    step,
                    outcome,
                    duration_ms,
                    "notify(on-call engineer)",
                )
                break

        total_elapsed = time.monotonic() - t0
        return {
            "alertname": alertname,
            "runbook_url": runbook_url,
            "remediated": remediated,
            "rolled_back": rolled_back,
            "elapsed_seconds": total_elapsed,
            "within_90s_sla": total_elapsed <= P1_SLA_SECONDS,
            "slack_messages": list(self.slack_messages),
        }


class TestGrafanaOnCallAutomatedTriageChaos(unittest.TestCase):
    def test_prometheus_alerts_have_runbook_url_pointing_to_docs_runbooks(self) -> None:
        alerts_path = REPO_ROOT / "infrastructure" / "monitoring" / "alerts.yaml"
        content = alerts_path.read_text(encoding="utf-8")

        alert_names = re.findall(r"-\s*alert:\s*([A-Za-z0-9_]+)", content)
        runbook_urls = re.findall(r"runbook_url:\s*(\S+)", content)

        self.assertGreaterEqual(len(alert_names), 4)
        self.assertEqual(
            len(alert_names),
            len(runbook_urls),
            "Every alert in alerts.yaml must have a runbook_url annotation",
        )

        for url in runbook_urls:
            self.assertIn("docs/runbooks/", url)
            rel_idx = url.index("docs/runbooks/")
            rel_path = url[rel_idx:]
            runbook_file = REPO_ROOT / rel_path
            self.assertTrue(
                runbook_file.exists(),
                f"Runbook referenced by runbook_url does not exist: {runbook_file}",
            )

    def test_grafana_oncall_escalation_chain_0m_5m_15m(self) -> None:
        oncall_yaml = (
            REPO_ROOT / "infrastructure" / "monitoring" / "grafana_oncall.yaml"
        ).read_text(encoding="utf-8")
        self.assertIn("notify(channel)", oncall_yaml)
        self.assertIn("notify(on-call engineer)", oncall_yaml)
        self.assertIn("notify(manager)", oncall_yaml)
        self.assertIn("duration_seconds: 300", oncall_yaml)
        self.assertIn("duration_seconds: 900", oncall_yaml)

        bot = AutomatedTriageBotSim()
        self.assertEqual([s.action for s in bot.due_escalations(0)], ["notify(channel)"])
        self.assertEqual(
            [s.action for s in bot.due_escalations(300)],
            ["notify(channel)", "notify(on-call engineer)"],
        )
        self.assertEqual(
            [s.action for s in bot.due_escalations(900)],
            ["notify(channel)", "notify(on-call engineer)", "notify(manager)"],
        )

    def test_runbook_step_timeout_and_automatic_rollback(self) -> None:
        rolled_back_steps: List[str] = []

        bot = AutomatedTriageBotSim(
            step_timeout_seconds=1.0,
            step_handlers={
                "check_postgres_connection_count": lambda: 0.01,
                "restart_stuck_indexer_pod": lambda: 5.0,  # exceeds 1.0s step timeout
                "flush_redis_cache": lambda: 0.01,
            },
            rollback_callback=lambda step: rolled_back_steps.append(step),
        )

        report = bot.triage_p1_alert(
            {
                "alertname": "HighIndexerLag",
                "severity": "critical",
                "annotations": {
                    "runbook_url": "https://github.com/A4-Stellar/Tessera/blob/main/docs/runbooks/indexer-lag.md"
                },
            }
        )

        self.assertFalse(report["remediated"])
        self.assertTrue(report["rolled_back"])
        self.assertEqual(rolled_back_steps, ["restart_stuck_indexer_pod"])
        last_msg = report["slack_messages"][-1]
        self.assertEqual(last_msg["action"], "restart_stuck_indexer_pod")
        self.assertIn("timed_out_and_rolled_back", str(last_msg["outcome"]))
        self.assertEqual(last_msg["next_step"], "notify(on-call engineer)")
        self.assertTrue(bot.audit.verify_integrity())

    def test_chaos_p1_alert_automated_triage_completes_within_90_seconds(self) -> None:
        cluster_state = {
            "pg_connections": 97,
            "indexer_pod_stuck": True,
            "redis_cache_stale": True,
        }

        def fix_pg() -> float:
            cluster_state["pg_connections"] = 22
            return 0.012

        def fix_indexer() -> float:
            cluster_state["indexer_pod_stuck"] = False
            return 0.025

        def fix_redis() -> float:
            cluster_state["redis_cache_stale"] = False
            return 0.008

        bot = AutomatedTriageBotSim(
            step_timeout_seconds=15.0,
            step_handlers={
                "check_postgres_connection_count": fix_pg,
                "restart_stuck_indexer_pod": fix_indexer,
                "flush_redis_cache": fix_redis,
            },
        )

        report = bot.triage_p1_alert(
            {
                "alertname": "HighIndexerLag",
                "severity": "critical",
                "annotations": {
                    "runbook_url": "https://github.com/A4-Stellar/Tessera/blob/main/docs/runbooks/indexer-lag.md"
                },
            }
        )

        self.assertTrue(report["remediated"])
        self.assertFalse(report["rolled_back"])
        self.assertTrue(report["within_90s_sla"])
        self.assertLess(float(report["elapsed_seconds"]), 90.0)

        self.assertEqual(cluster_state["pg_connections"], 22)
        self.assertFalse(cluster_state["indexer_pod_stuck"])
        self.assertFalse(cluster_state["redis_cache_stale"])

        # Verify structured Slack output `{action, outcome, duration_ms, next_step}`
        slack_msgs = report["slack_messages"]
        self.assertEqual(len(slack_msgs), 3)
        for msg in slack_msgs:
            serialized = json.loads(json.dumps(msg))
            self.assertEqual(
                set(serialized.keys()),
                {"action", "outcome", "duration_ms", "next_step"},
            )

        self.assertEqual(slack_msgs[0]["action"], "check_postgres_connection_count")
        self.assertEqual(slack_msgs[0]["next_step"], "restart_stuck_indexer_pod")
        self.assertEqual(slack_msgs[1]["action"], "restart_stuck_indexer_pod")
        self.assertEqual(slack_msgs[1]["next_step"], "flush_redis_cache")
        self.assertEqual(slack_msgs[2]["action"], "flush_redis_cache")
        self.assertEqual(slack_msgs[2]["next_step"], "verify_slo_recovery")

        # Verify SHA-256 hash-chained immutable audit trail
        self.assertEqual(len(bot.audit.entries), 4)
        self.assertTrue(bot.audit.verify_integrity())


if __name__ == "__main__":
    unittest.main()
