#!/usr/bin/env python3
"""Unit tests for chaos_runner.py (Issue #211)."""

import json
import tempfile
import unittest
from pathlib import Path

from tests.chaos.chaos_runner import (
    ChaosResilienceRunner,
    ChaosScenario,
    ResilienceReport,
    ScenarioSummary,
    format_markdown_report,
)


class TestChaosRunner(unittest.TestCase):
    def setUp(self):
        self.runner = ChaosResilienceRunner(
            target_url="http://localhost:8080",
            simulated=True,
            probe_interval_sec=0.001,
        )

    def test_chaos_manifest_exists_and_valid(self):
        manifest_path = Path(__file__).parent / "chaos_experiments.yaml"
        self.assertTrue(manifest_path.exists(), "chaos_experiments.yaml must exist")
        content = manifest_path.read_text()
        self.assertIn("network-delay-200ms-2000ms", content)
        self.assertIn("packet-drop-10pct", content)
        self.assertIn("pod-kill-api-instances", content)
        self.assertIn("rpc-outage-503-simulation", content)
        self.assertIn("db-master-crash-failover", content)

    def test_baseline_scenario_probe(self):
        res = self.runner.run_scenario(ChaosScenario.BASELINE, probe_count=10)
        self.assertEqual(res.scenario, "baseline")
        self.assertEqual(res.availability_pct, 100.0)
        self.assertTrue(res.passed)
        self.assertGreaterEqual(res.resilience_score, 90.0)

    def test_network_delay_scenario(self):
        res = self.runner.run_scenario(ChaosScenario.NETWORK_DELAY, probe_count=15)
        self.assertEqual(res.scenario, "network_delay_200ms_2000ms")
        self.assertEqual(res.availability_pct, 100.0)
        self.assertGreaterEqual(res.p95_latency_ms, 200.0)
        self.assertTrue(res.passed)

    def test_packet_drop_scenario(self):
        res = self.runner.run_scenario(ChaosScenario.PACKET_DROP, probe_count=20)
        self.assertEqual(res.scenario, "packet_drop_10pct")
        self.assertGreaterEqual(res.availability_pct, 95.0)
        self.assertTrue(res.passed)

    def test_pod_kill_scenario(self):
        res = self.runner.run_scenario(ChaosScenario.POD_KILL, probe_count=12)
        self.assertEqual(res.scenario, "pod_kill_api_instances")
        self.assertEqual(res.availability_pct, 100.0)
        self.assertTrue(res.passed)

    def test_rpc_outage_fallback_assertion(self):
        # Asserts API fallback mechanisms maintain service availability during active chaos injection
        res = self.runner.run_scenario(ChaosScenario.RPC_OUTAGE, probe_count=15)
        self.assertEqual(res.scenario, "rpc_outage_503_simulation")
        self.assertGreaterEqual(res.fallback_success_rate_pct, 50.0)
        self.assertTrue(res.passed)

    def test_db_crash_read_replica_fallback(self):
        res = self.runner.run_scenario(ChaosScenario.DB_CRASH, probe_count=10)
        self.assertEqual(res.scenario, "db_master_crash_failover")
        self.assertEqual(res.availability_pct, 100.0)
        self.assertEqual(res.fallback_success_rate_pct, 100.0)
        self.assertTrue(res.passed)

    def test_run_all_and_score_generation(self):
        report = self.runner.run_all()
        self.assertIsInstance(report, ResilienceReport)
        self.assertEqual(len(report.scenarios), 6)
        self.assertGreaterEqual(report.overall_score, 85.0)
        self.assertIn(report.grade, ["A+", "A", "B"])
        self.assertTrue(report.passed)

        # Verify markdown output formatting
        md = format_markdown_report(report)
        self.assertIn("# Tessera Chaos Engineering Resilience Report", md)
        self.assertIn("network_delay_200ms_2000ms", md)
        self.assertIn("packet_drop_10pct", md)


if __name__ == "__main__":
    unittest.main()
