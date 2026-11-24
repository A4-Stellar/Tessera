#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0
"""Unit and integration test suite for eBPF XDP DDoS mitigation and GeoIP blocking (Issue #44 / #425).

Can run on any machine without root or BCC kernel dependencies.
"""

from __future__ import annotations

import os
import sys
import tempfile
import time
import unittest

sys.path.insert(0, os.path.dirname(__file__))

from xdp_ddos_loader import (
    ControlPlaneManager,
    DdosMetricsCollector,
    GeoIpResolver,
    TokenBucketSimulator,
    ip_to_u32,
    u32_to_ip,
)
from load_test_syn_flood import run_load_test


class TokenBucketTests(unittest.TestCase):
    def test_ip_conversion(self):
        ip = "192.168.1.100"
        u32 = ip_to_u32(ip)
        self.assertEqual(u32_to_ip(u32), ip)

    def test_initial_burst_allowed(self):
        sim = TokenBucketSimulator(capacity=10, refill_rate_per_sec=10)
        ip = ip_to_u32("1.2.3.4")

        # Allow up to capacity
        for _ in range(10):
            self.assertTrue(sim.allow_packet(ip, current_time=100.0))

        # 11th packet in same millisecond must be dropped
        self.assertFalse(sim.allow_packet(ip, current_time=100.0))

    def test_token_refill_over_time(self):
        sim = TokenBucketSimulator(capacity=5, refill_rate_per_sec=10)
        ip = ip_to_u32("5.6.7.8")

        # Exhaust tokens at t = 10.0
        for _ in range(5):
            self.assertTrue(sim.allow_packet(ip, current_time=10.0))
        self.assertFalse(sim.allow_packet(ip, current_time=10.0))

        # After 0.5s, 5 tokens should have refilled (10 tokens/s * 0.5s = 5)
        self.assertTrue(sim.allow_packet(ip, current_time=10.5))
        self.assertTrue(sim.allow_packet(ip, current_time=10.5))


class GeoIpTests(unittest.TestCase):
    def test_embargoed_country_blocking(self):
        resolver = GeoIpResolver(embargoed_countries={"KP", "IR", "CU"})
        resolver.add_embargoed_network("175.45.176.0/22")  # North Korea allocated block
        resolver.add_embargoed_network("5.200.0.0/16")     # Iran block

        self.assertTrue(resolver.is_embargoed("175.45.176.1"))
        self.assertTrue(resolver.is_embargoed("5.200.12.34"))
        self.assertFalse(resolver.is_embargoed("8.8.8.8"))      # Legitimate US/Google DNS
        self.assertFalse(resolver.is_embargoed("1.1.1.1"))      # Cloudflare

    def test_csv_loading(self):
        with tempfile.NamedTemporaryFile("w+", delete=False, suffix=".csv") as tmp:
            tmp.write("# network,country_iso_code\n")
            tmp.write("190.0.0.0/16,CU\n")
            tmp.write("8.8.8.0/24,US\n")
            tmp.flush()
            tmp_path = tmp.name

        try:
            resolver = GeoIpResolver(embargoed_countries={"CU"})
            loaded = resolver.load_from_csv(tmp_path)
            self.assertEqual(loaded, 1)
            self.assertTrue(resolver.is_embargoed("190.0.5.1"))
            self.assertFalse(resolver.is_embargoed("8.8.8.8"))
        finally:
            os.remove(tmp_path)


class ControlPlaneTests(unittest.TestCase):
    def test_dynamic_block_and_unblock(self):
        metrics = DdosMetricsCollector()
        manager = ControlPlaneManager(metrics=metrics)

        # Block single IP
        ok, msg = manager.block_ip("203.0.113.5", reason="malicious_syn_flood")
        self.assertTrue(ok)
        self.assertTrue(manager.is_blocked("203.0.113.5"))
        self.assertFalse(manager.is_blocked("203.0.113.6"))
        self.assertEqual(len(manager.list_blocked()), 1)

        # Block CIDR range
        ok, _ = manager.block_ip("198.51.100.0/24", reason="compromised_asn")
        self.assertTrue(ok)
        self.assertTrue(manager.is_blocked("198.51.100.42"))
        self.assertEqual(len(manager.list_blocked()), 2)

        # Unblock IP
        ok, _ = manager.unblock_ip("203.0.113.5")
        self.assertTrue(ok)
        self.assertFalse(manager.is_blocked("203.0.113.5"))
        self.assertEqual(len(manager.list_blocked()), 1)

    def test_ttl_expiry(self):
        manager = ControlPlaneManager()
        # Block with 1 second TTL
        manager.block_ip("10.99.99.1", reason="temp_ban", ttl_sec=1)
        self.assertTrue(manager.is_blocked("10.99.99.1"))

        # Advance time artificially
        manager.blocked_ips["10.99.99.1/32"]["expires_at"] = time.time() - 1.0
        self.assertFalse(manager.is_blocked("10.99.99.1"))
        self.assertEqual(len(manager.list_blocked()), 0)


class SynFloodLoadTestVerification(unittest.TestCase):
    def test_10gbps_syn_flood_sla(self):
        res = run_load_test(
            legitimate_clients_count=100,
            legitimate_requests_per_client=10,
            attacker_ips_count=50,
            attack_packets=50000,
            bucket_capacity=200,
            refill_rate_per_sec=100,
        )
        self.assertLess(res["legit_drop_pct"], 1.0)
        self.assertGreater(res["attack_drop_pct"], 50.0)


if __name__ == "__main__":
    unittest.main()
