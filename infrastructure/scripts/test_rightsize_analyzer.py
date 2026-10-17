#!/usr/bin/env python3
"""Unit tests for rightsize_analyzer.py (Issue #313)."""

import json
import tempfile
import unittest
from pathlib import Path

from infrastructure.scripts.rightsize_analyzer import (
    ContainerUsageProfile,
    MetricTimeSeries,
    ProvisioningStatus,
    RightSizeAnalyzer,
    calculate_container_monthly_cost,
    format_cpu_k8s,
    format_markdown_report,
    format_memory_k8s,
    generate_synthetic_14d_profile,
    parse_cpu_to_cores,
    parse_memory_to_mib,
)


class TestRightSizeAnalyzer(unittest.TestCase):
    def setUp(self):
        self.analyzer = RightSizeAnalyzer(
            cpu_safety_buffer_pct=15.0,
            memory_safety_buffer_pct=20.0,
            cpu_burst_headroom_pct=50.0,
            memory_limit_factor=1.25,
        )

    def test_unit_parsing_cpu(self):
        self.assertEqual(parse_cpu_to_cores("250m"), 0.25)
        self.assertEqual(parse_cpu_to_cores("1000m"), 1.0)
        self.assertEqual(parse_cpu_to_cores("2"), 2.0)
        self.assertEqual(parse_cpu_to_cores("0.5"), 0.5)

    def test_unit_parsing_memory(self):
        self.assertEqual(parse_memory_to_mib("512Mi"), 512.0)
        self.assertEqual(parse_memory_to_mib("2Gi"), 2048.0)
        self.assertEqual(parse_memory_to_mib("1024Ki"), 1.0)

    def test_unit_formatting_k8s(self):
        self.assertEqual(format_cpu_k8s(0.25), "250m")
        self.assertEqual(format_cpu_k8s(1.0), "1000m")
        self.assertEqual(format_memory_k8s(512.0), "512Mi")
        self.assertEqual(format_memory_k8s(2048.0), "2Gi")

    def test_metric_timeseries_percentiles(self):
        # 100 values from 1.0 to 100.0
        values = [float(i) for i in range(1, 101)]
        series = MetricTimeSeries(values=values)
        self.assertEqual(series.count(), 100)
        self.assertAlmostEqual(series.percentile(50), 50.5, delta=0.5)
        self.assertAlmostEqual(series.percentile(95), 95.05, delta=0.5)
        self.assertAlmostEqual(series.percentile(99), 99.01, delta=0.5)
        self.assertEqual(series.max_val(), 100.0)
        self.assertEqual(series.min_val(), 1.0)

    def test_overprovisioned_workload_analysis(self):
        profile = generate_synthetic_14d_profile(
            "tessera-system", "tessera-api", "tessera-api", "overprovisioned"
        )
        self.assertEqual(profile.observation_days, 14.0)
        res = self.analyzer.analyze_container(profile)

        # Current is 2000m CPU / 4096Mi RAM, while usage is ~250m / ~512Mi
        self.assertIn(
            res.status,
            [ProvisioningStatus.OVERPROVISIONED.value, ProvisioningStatus.SEVERELY_OVERPROVISIONED.value],
        )
        self.assertLess(parse_cpu_to_cores(res.recommended_resources["cpu_request"]), 1.0)
        self.assertLess(parse_memory_to_mib(res.recommended_resources["memory_request"]), 2048.0)
        self.assertGreater(res.monthly_savings_usd, 0.0)
        self.assertGreater(res.savings_percentage, 50.0)

    def test_idle_workload_analysis(self):
        profile = generate_synthetic_14d_profile(
            "tessera-system", "idle-worker", "idle-worker", "idle"
        )
        res = self.analyzer.analyze_container(profile)
        self.assertEqual(res.status, ProvisioningStatus.IDLE.value)
        self.assertGreater(res.savings_percentage, 75.0)

    def test_underprovisioned_workload_analysis(self):
        profile = generate_synthetic_14d_profile(
            "tessera-system", "heavy-worker", "heavy-worker", "underprovisioned"
        )
        res = self.analyzer.analyze_container(profile)
        self.assertEqual(res.status, ProvisioningStatus.UNDERPROVISIONED.value)

    def test_cost_calculation_math(self):
        # 1 vCPU and 1 GiB for 1 month
        cost = calculate_container_monthly_cost(1.0, 1024.0)
        # ~29.55 + ~3.25 = ~32.80
        self.assertGreater(cost, 30.0)
        self.assertLess(cost, 36.0)

    def test_full_report_and_markdown(self):
        profiles = [
            generate_synthetic_14d_profile("tessera-system", "api", "api", "overprovisioned"),
            generate_synthetic_14d_profile("tessera-system", "idle", "idle", "idle"),
        ]
        report = self.analyzer.analyze_all(profiles)
        self.assertEqual(report.total_containers_analyzed, 2)
        self.assertEqual(report.observation_period_days, 14.0)
        self.assertGreater(report.total_monthly_savings_usd, 0.0)

        md = format_markdown_report(report)
        self.assertIn("# Tessera FinOps Resource Right-Sizing & Optimization Report", md)
        self.assertIn("14.0 Days", md)
        self.assertIn("Projected Monthly Cost Savings", md)


if __name__ == "__main__":
    unittest.main()
