# SPDX-License-Identifier: GPL-2.0
"""Unit tests for the userspace half of the HTTP/2 XDP profiler (issue #392).

These tests deliberately avoid importing BCC: only the pure aggregation and
argument-parsing helpers are exercised, so they can run anywhere
prometheus-client is installed (no root, no kernel requirements).
"""

from __future__ import annotations

import importlib.util
import os
import unittest

MODULE_PATH = os.path.join(os.path.dirname(__file__), "http2_profiler.py")
_spec = importlib.util.spec_from_file_location("http2_profiler", MODULE_PATH)
http2_profiler = importlib.util.module_from_spec(_spec)
assert _spec.loader is not None
_spec.loader.exec_module(http2_profiler)


class BucketTests(unittest.TestCase):
    def test_log2_slot_boundaries(self):
        self.assertEqual(http2_profiler.slot_for(0), 1)
        self.assertEqual(http2_profiler.slot_for(1), 1)
        self.assertEqual(http2_profiler.slot_for(2), 2)
        self.assertEqual(http2_profiler.slot_for(1_000_000), 20)
        self.assertEqual(http2_profiler.slot_for(10**18), http2_profiler.MAX_SLOT)

    def test_buckets_are_cumulative_and_end_at_infinity(self):
        buckets = http2_profiler.cumulative_buckets({1: 2, 3: 1, 36: 1})
        self.assertEqual(len(buckets), http2_profiler.MAX_SLOT + 1)
        self.assertEqual(buckets[0][1], 2)
        self.assertEqual(buckets[2][1], 3)
        self.assertEqual(buckets[-1], ("+Inf", 4))
        counts = [count for _, count in buckets]
        self.assertEqual(counts, sorted(counts))


class AggregationTests(unittest.TestCase):
    def test_events_are_grouped_by_method_and_status(self):
        metrics = http2_profiler.AggregatedMetrics()
        metrics.add_event(http2_profiler.coerce_event({
            "timestamp_ns": 100, "latency_ns": 2_000_000, "pid": 42,
            "path_hash": 1234, "method": 1, "status": 200, "kind": 0,
        }))
        metrics.add_event(http2_profiler.coerce_event({
            "timestamp_ns": 200, "latency_ns": 8_000, "pid": 42,
            "path_hash": 1234, "method": 2, "status": 404, "kind": 0,
        }))
        self.assertIn(("GET", "200"), metrics.latencies)
        self.assertIn(("POST", "404"), metrics.latencies)
        self.assertAlmostEqual(metrics.latency_sums[("GET", "200")], 0.002)
        self.assertEqual(metrics.events[("response", "200")], 1)

    def test_reset_is_reported_separately(self):
        metrics = http2_profiler.AggregatedMetrics()
        metrics.add_event(http2_profiler.coerce_event({
            "timestamp_ns": 10, "latency_ns": 500, "pid": 0,
            "path_hash": 7, "method": 0, "status": 0, "kind": 1,
        }))
        self.assertIn(("UNKNOWN", "RST_STREAM"), metrics.latencies)
        self.assertEqual(metrics.events[("reset", "RST_STREAM")], 1)

    def test_frame_counts_use_h2_frame_names(self):
        metrics = http2_profiler.AggregatedMetrics()
        for frame_type in (0x0, 0x1, 0x1, 0x3):
            metrics.add_frame(frame_type)
        self.assertEqual(metrics.frames["HEADERS"], 2)
        self.assertEqual(metrics.frames["DATA"], 1)
        self.assertEqual(metrics.frames["RST_STREAM"], 1)


class CollectorTests(unittest.TestCase):
    def test_collector_emits_three_metric_families(self):
        metrics = http2_profiler.AggregatedMetrics()
        metrics.add_event(http2_profiler.coerce_event({
            "timestamp_ns": 1, "latency_ns": 10_000, "pid": 1,
            "path_hash": 1, "method": 1, "status": 200, "kind": 0,
        }))
        metrics.add_frame(0x1)
        families = list(http2_profiler.Http2Collector(metrics).collect())
        self.assertEqual(
            [family.name for family in families],
            [
                "tessera_http2_request_duration_seconds",
                "tessera_http2_events_total",
                "tessera_http2_frames_total",
            ],
        )


class ArgumentTests(unittest.TestCase):
    def test_veth_alias_and_defaults(self):
        args = http2_profiler.parse_args(["--veth", "cali1234abcd"])
        self.assertEqual(args.interface, "cali1234abcd")
        self.assertEqual(args.listen_port, 8080)
        self.assertEqual(args.port, 9437)

    def test_invalid_port_is_rejected(self):
        with self.assertRaises(SystemExit):
            http2_profiler.parse_args(["--interface", "eth0", "--listen-port", "0"])


if __name__ == "__main__":
    unittest.main()
