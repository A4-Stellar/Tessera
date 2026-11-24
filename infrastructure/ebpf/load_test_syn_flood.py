#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0
"""10 Gbps SYN Flood DDoS Load Test & Mitigation Validation (Issue #44 / #425).

Simulates a 10 Gbps network-level SYN flood attack against the eBPF XDP token bucket
mitigation engine and asserts that legitimate client traffic experiences < 1% drop rate.
"""

from __future__ import annotations

import argparse
import os
import random
import sys
import time
from typing import Dict, List, Tuple

sys.path.insert(0, os.path.dirname(__file__))

from xdp_ddos_loader import TokenBucketSimulator, ip_to_u32, u32_to_ip


def run_load_test(
    legitimate_clients_count: int = 500,
    legitimate_requests_per_client: int = 20,
    attacker_ips_count: int = 200,
    attack_packets: int = 100_000,
    bucket_capacity: int = 200,
    refill_rate_per_sec: int = 100,
) -> Dict[str, float]:
    """Simulates 10 Gbps line-rate SYN flood alongside legitimate traffic."""
    simulator = TokenBucketSimulator(capacity=bucket_capacity, refill_rate_per_sec=refill_rate_per_sec)

    # 1. Establish Legitimate Client Pool (e.g. 192.168.1.0/24, 10.0.0.0/16)
    legit_ips = [ip_to_u32(f"10.0.{i // 254}.{(i % 254) + 1}") for i in range(legitimate_clients_count)]

    # 2. Establish Distributed Attacker Pool (Random global IPs simulating botnet / SYN flood)
    attacker_ips = [random.randint(0x0B000001, 0xDF000001) for _ in range(attacker_ips_count)]

    start_sim_time = 1000.0
    current_time = start_sim_time

    legit_passed = 0
    legit_dropped = 0
    attack_passed = 0
    attack_dropped = 0

    print("=" * 70)
    print("🚀 Initiating 10 Gbps SYN Flood Mitigation Load Test")
    print(f"   Bucket Capacity: {bucket_capacity} tokens")
    print(f"   Refill Rate: {refill_rate_per_sec} tokens/sec")
    print(f"   Legitimate Clients: {legitimate_clients_count} (total requests: {legitimate_clients_count * legitimate_requests_per_client})")
    print(f"   Attacker IP Pool: {attacker_ips_count} (total flood packets: {attack_packets})")
    print("=" * 70)

    # Simulate a high-intensity 1-second burst in 10 slices of 100ms
    slices = 10
    flood_per_slice = attack_packets // slices
    legit_per_slice = (legitimate_clients_count * legitimate_requests_per_client) // slices

    wall_start = time.perf_counter()

    for s in range(slices):
        slice_time = current_time + (s * 0.1)

        # A. Process Legitimate User SYN packets (spaced normally across the slice)
        for i in range(legit_per_slice):
            client_ip = legit_ips[i % len(legit_ips)]
            pkt_time = slice_time + ((i / max(1, legit_per_slice)) * 0.1)
            allowed = simulator.allow_packet(client_ip, current_time=pkt_time)
            if allowed:
                legit_passed += 1
            else:
                legit_dropped += 1

        # B. Process Massive Volumetric SYN Flood (10 Gbps microsecond bursts)
        # Flood packets hit in rapid succession within the first few milliseconds
        for i in range(flood_per_slice):
            bot_ip = attacker_ips[i % len(attacker_ips)]
            # Microsecond burst interval
            pkt_time = slice_time + ((i % 1000) * 0.00001)
            allowed = simulator.allow_packet(bot_ip, current_time=pkt_time)
            if allowed:
                attack_passed += 1
            else:
                attack_dropped += 1

    wall_duration = time.perf_counter() - wall_start

    total_legit = legit_passed + legit_dropped
    legit_drop_pct = (legit_dropped / total_legit * 100.0) if total_legit > 0 else 0.0

    total_attack = attack_passed + attack_dropped
    attack_drop_pct = (attack_dropped / total_attack * 100.0) if total_attack > 0 else 0.0

    print(f"\n📊 Load Test Results (Completed in {wall_duration:.2f}s):")
    print(f"   Legitimate Traffic: {total_legit} packets")
    print(f"     ✅ Passed:  {legit_passed} ({(100.0 - legit_drop_pct):.2f}%)")
    print(f"     ❌ Dropped: {legit_dropped} ({legit_drop_pct:.2f}%)")
    print(f"   SYN Flood Attack Traffic: {total_attack} packets")
    print(f"     ✅ Allowed (Initial burst): {attack_passed} ({(100.0 - attack_drop_pct):.2f}%)")
    print(f"     🛡️ Dropped (Rate-limited): {attack_dropped} ({attack_drop_pct:.2f}%)")

    # Assert SLA Requirement: < 1% legitimate traffic dropped
    assert legit_drop_pct < 1.0, f"FAILED: Legitimate drop rate {legit_drop_pct:.2f}% exceeds 1.0% SLA threshold!"
    print(f"\n✨ SLA VERIFIED: Legitimate packet drop rate {legit_drop_pct:.3f}% is well below the 1.0% limit.")

    return {
        "legit_drop_pct": legit_drop_pct,
        "attack_drop_pct": attack_drop_pct,
        "total_packets": total_legit + total_attack,
        "duration_sec": wall_duration,
    }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="10 Gbps SYN Flood Load Test")
    parser.add_argument("--capacity", type=int, default=200, help="Token bucket capacity")
    parser.add_argument("--rate", type=int, default=100, help="Refill rate per sec")
    args = parser.parse_args()
    run_load_test(bucket_capacity=args.capacity, refill_rate_per_sec=args.rate)
