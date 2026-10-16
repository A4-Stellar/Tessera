#!/usr/bin/env python3
"""Unit tests for eBPF socket profiler and Prometheus exporter (Issue #307)."""

import unittest
from pathlib import Path

from infrastructure.ebpf.socket_profiler_loader import (
    HISTOGRAM_BUCKETS_US,
    SocketProfilerManager,
    run_exporter,
)

EBPF_DIR = Path(__file__).resolve().parent
BPF_SOURCE_FILE = EBPF_DIR / "socket_profiler.c"


class SocketProfilerTestCase(unittest.TestCase):
    def test_ebpf_c_source_structure(self):
        self.assertTrue(BPF_SOURCE_FILE.is_file(), "socket_profiler.c must exist")
        c_code = BPF_SOURCE_FILE.read_text(encoding="utf-8")

        # Verify probe attachment to required syscalls
        self.assertIn("sys_enter_sendto", c_code)
        self.assertIn("sys_exit_sendto", c_code)
        self.assertIn("sys_enter_recvfrom", c_code)
        self.assertIn("sys_exit_recvfrom", c_code)

        # Verify BPF maps defined
        self.assertIn("active_socket_syscalls", c_code)
        self.assertIn("sendto_latency_histogram", c_code)
        self.assertIn("recvfrom_latency_histogram", c_code)
        self.assertIn("context_switch_overhead", c_code)

        # Verify license
        self.assertIn("SEC(\"license\")", c_code)

    def test_latency_recording_and_histogram_distribution(self):
        manager = SocketProfilerManager(mock_mode=True)

        # Record samples of varying latencies
        manager.record_sample("sendto", latency_ns=1500, context_switch_ns=500)   # 1.5us -> bucket 2
        manager.record_sample("sendto", latency_ns=7500, context_switch_ns=800)   # 7.5us -> bucket 8
        manager.record_sample("recvfrom", latency_ns=25000, context_switch_ns=1200) # 25us -> bucket 32

        metrics = manager.sendto_metrics
        self.assertTrue(metrics.total_count >= 2)
        self.assertTrue(metrics.total_latency_ns > 0)
        self.assertTrue(metrics.context_switch_overhead_ns > 0)

    def test_prometheus_exposition_format(self):
        manager = SocketProfilerManager(mock_mode=True)
        metrics_output = manager.generate_prometheus_metrics()

        # Check Prometheus standard header and types
        self.assertIn("# HELP tessera_socket_syscall_latency_microseconds", metrics_output)
        self.assertIn("# TYPE tessera_socket_syscall_latency_microseconds histogram", metrics_output)

        # Check bucket exposition
        self.assertIn('tessera_socket_syscall_latency_microseconds_bucket{syscall="sendto",le="1"}', metrics_output)
        self.assertIn('tessera_socket_syscall_latency_microseconds_bucket{syscall="recvfrom",le="+Inf"}', metrics_output)

        # Check context switch overhead gauge
        self.assertIn("# HELP tessera_socket_context_switch_overhead_nanoseconds", metrics_output)
        self.assertIn('tessera_socket_context_switch_overhead_nanoseconds{syscall="sendto",metric="avg"}', metrics_output)

        # Check total syscalls counter
        self.assertIn('tessera_socket_syscalls_total{syscall="sendto"}', metrics_output)
        self.assertIn('tessera_socket_syscalls_total{syscall="recvfrom"}', metrics_output)

    def test_http_metrics_server(self):
        import threading
        import urllib.request

        server = run_exporter(port=0, mock_mode=True)
        port = server.server_port
        t = threading.Thread(target=server.serve_forever, daemon=True)
        t.start()

        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{port}/metrics", timeout=3.0) as resp:
                self.assertEqual(resp.status, 200)
                body = resp.read().decode("utf-8")
                self.assertIn("tessera_socket_syscall_latency_microseconds", body)
        finally:
            server.shutdown()
            server.server_close()


if __name__ == "__main__":
    unittest.main()
