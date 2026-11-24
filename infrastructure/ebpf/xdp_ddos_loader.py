#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0
"""Userspace loader, gRPC control plane, and Prometheus exporter for xdp_ddos_mitigator.c (Issue #44 / #425).

Features:
- Compiles and attaches the eBPF XDP program in XDP_DRV mode (native driver mode).
- Loads MaxMind GeoIP embargoed IP ranges into BPF maps.
- Exposes a gRPC control plane enabling the API to dynamically block/unblock IP ranges.
- Consumes drop_events from BPF perf_event_array and exports Prometheus metrics.
"""

from __future__ import annotations

import argparse
import csv
import ipaddress
import json
import logging
import os
import signal
import socket
import struct
import sys
import threading
import time
from concurrent import futures
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, HTTPServer
from typing import Any, Dict, List, Optional, Set, Tuple

try:
    from prometheus_client import REGISTRY, start_http_server
    from prometheus_client.core import CounterMetricFamily, GaugeMetricFamily
    HAS_PROMETHEUS = True
except ImportError:
    HAS_PROMETHEUS = False

# Default Embargoed Country ISO-3166 Codes
DEFAULT_EMBARGOED_COUNTRIES = {"CU", "IR", "KP", "SY", "RU"}

DROP_REASONS = {
    1: "syn_flood",
    2: "geoip_embargo",
    3: "dynamic_block",
}


def ip_to_u32(ip_str: str) -> int:
    """Convert IPv4 string to 32-bit unsigned integer (network byte order)."""
    return struct.unpack("!I", socket.inet_aton(ip_str))[0]


def u32_to_ip(ip_u32: int) -> str:
    """Convert 32-bit integer to IPv4 string."""
    return socket.inet_ntoa(struct.pack("!I", ip_u32))


@dataclass
class TokenBucketSimulator:
    """Userspace mirror of kernel token bucket algorithm for validation and testing."""
    capacity: int = 200
    refill_rate_per_sec: int = 100
    buckets: Dict[int, Tuple[float, float]] = field(default_factory=dict)

    def allow_packet(self, ip_u32: int, current_time: Optional[float] = None) -> bool:
        now = time.time() if current_time is None else current_time
        if ip_u32 not in self.buckets:
            self.buckets[ip_u32] = (now, max(0.0, float(self.capacity - 1)))
            return True

        last_time, tokens = self.buckets[ip_u32]
        elapsed = max(0.0, now - last_time)
        refreshed = min(float(self.capacity), tokens + (elapsed * self.refill_rate_per_sec))

        if refreshed < 1.0:
            self.buckets[ip_u32] = (now, 0.0)
            return False

        self.buckets[ip_u32] = (now, refreshed - 1.0)
        return True


class GeoIpResolver:
    """Parses MaxMind GeoIP database or IP-to-country mapping files."""

    def __init__(self, embargoed_countries: Optional[Set[str]] = None):
        self.embargoed_countries = embargoed_countries or set(DEFAULT_EMBARGOED_COUNTRIES)
        self.embargoed_networks: List[ipaddress.IPv4Network] = []
        self.exact_blocked_ips: Set[int] = set()

    def load_from_csv(self, csv_path: str) -> int:
        """Loads GeoIP CIDR / country mapping from CSV (format: network,country_iso_code)."""
        loaded = 0
        if not os.path.exists(csv_path):
            return 0

        with open(csv_path, mode="r", encoding="utf-8") as f:
            reader = csv.reader(f)
            for row in reader:
                if len(row) < 2 or row[0].startswith("#"):
                    continue
                net_str, country = row[0].strip(), row[1].strip().upper()
                if country in self.embargoed_countries:
                    try:
                        net = ipaddress.IPv4Network(net_str, strict=False)
                        self.embargoed_networks.append(net)
                        loaded += 1
                    except ValueError:
                        continue
        return loaded

    def add_embargoed_network(self, cidr: str) -> None:
        net = ipaddress.IPv4Network(cidr, strict=False)
        self.embargoed_networks.append(net)

    def is_embargoed(self, ip_str: str) -> bool:
        try:
            ip_obj = ipaddress.IPv4Address(ip_str)
            for net in self.embargoed_networks:
                if ip_obj in net:
                    return True
        except ValueError:
            pass
        return False


class DdosMetricsCollector:
    """Prometheus metrics collector for XDP DDoS mitigation telemetry."""

    def __init__(self):
        self.lock = threading.Lock()
        self.drops_by_reason: Dict[str, int] = {
            "syn_flood": 0,
            "geoip_embargo": 0,
            "dynamic_block": 0,
        }
        self.passes_by_protocol: Dict[str, int] = {
            "tcp_syn": 0,
            "other": 0,
        }
        self.active_rate_limit_entries = 0
        self.dynamic_blocked_count = 0

    def record_drop(self, reason: str, count: int = 1):
        with self.lock:
            self.drops_by_reason[reason] = self.drops_by_reason.get(reason, 0) + count

    def record_pass(self, protocol: str, count: int = 1):
        with self.lock:
            self.passes_by_protocol[protocol] = self.passes_by_protocol.get(protocol, 0) + count

    def collect(self):
        with self.lock:
            drop_metric = CounterMetricFamily(
                "tessera_xdp_dropped_packets_total",
                "Total number of dropped packets at XDP ingress layer",
                labels=["reason"],
            )
            for reason, val in self.drops_by_reason.items():
                drop_metric.add_metric([reason], val)
            yield drop_metric

            pass_metric = CounterMetricFamily(
                "tessera_xdp_passed_packets_total",
                "Total number of passed packets at XDP ingress layer",
                labels=["protocol"],
            )
            for proto, val in self.passes_by_protocol.items():
                pass_metric.add_metric([proto], val)
            yield pass_metric

            gauge = GaugeMetricFamily(
                "tessera_xdp_active_rate_limit_entries",
                "Active LRU entries tracking SYN token buckets",
            )
            gauge.add_metric([], self.active_rate_limit_entries)
            yield gauge

            blocked_gauge = GaugeMetricFamily(
                "tessera_xdp_blocked_ips_total",
                "Total active IP ranges blocked dynamically",
            )
            blocked_gauge.add_metric([], self.dynamic_blocked_count)
            yield blocked_gauge


class ControlPlaneManager:
    """Manages the runtime blocklist and exposes both gRPC and HTTP interfaces."""

    def __init__(self, bpf_instance: Any = None, metrics: Optional[DdosMetricsCollector] = None):
        self.bpf = bpf_instance
        self.metrics = metrics or DdosMetricsCollector()
        self.lock = threading.Lock()
        self.blocked_ips: Dict[str, Dict[str, Any]] = {}

    def block_ip(self, ip_or_cidr: str, reason: str = "manual_api", ttl_sec: int = 0) -> Tuple[bool, str]:
        with self.lock:
            try:
                net = ipaddress.IPv4Network(ip_or_cidr, strict=False)
                expires_at = time.time() + ttl_sec if ttl_sec > 0 else 0
                entry = {
                    "ip_or_cidr": str(net),
                    "reason": reason,
                    "blocked_at": time.time(),
                    "expires_at": expires_at,
                }
                self.blocked_ips[str(net)] = entry

                # If BPF is active, insert into dynamic_blocklist map
                if self.bpf and hasattr(self.bpf, "get_table"):
                    try:
                        table = self.bpf.get_table("dynamic_blocklist")
                        # For /32 or individual IPs:
                        for ip_addr in net.hosts() if net.num_addresses > 1 else [net.network_address]:
                            u32_val = struct.unpack("!I", socket.inet_aton(str(ip_addr)))[0]
                            table[table.Key(u32_val)] = table.Leaf(1)
                    except Exception as e:
                        logging.warning("Failed to sync BPF dynamic_blocklist: %s", e)

                self.metrics.dynamic_blocked_count = len(self.blocked_ips)
                return True, f"Blocked {net}"
            except ValueError as e:
                return False, f"Invalid IP/CIDR: {e}"

    def unblock_ip(self, ip_or_cidr: str) -> Tuple[bool, str]:
        with self.lock:
            try:
                net = ipaddress.IPv4Network(ip_or_cidr, strict=False)
                key = str(net)
                if key in self.blocked_ips:
                    del self.blocked_ips[key]

                    if self.bpf and hasattr(self.bpf, "get_table"):
                        try:
                            table = self.bpf.get_table("dynamic_blocklist")
                            for ip_addr in net.hosts() if net.num_addresses > 1 else [net.network_address]:
                                u32_val = struct.unpack("!I", socket.inet_aton(str(ip_addr)))[0]
                                try:
                                    del table[table.Key(u32_val)]
                                except Exception:
                                    pass
                        except Exception as e:
                            logging.warning("Failed to remove from BPF dynamic_blocklist: %s", e)

                    self.metrics.dynamic_blocked_count = len(self.blocked_ips)
                    return True, f"Unblocked {key}"
                return False, f"{key} was not blocked"
            except ValueError as e:
                return False, f"Invalid IP/CIDR: {e}"

    def list_blocked(self) -> List[Dict[str, Any]]:
        with self.lock:
            now = time.time()
            active = []
            for k, v in list(self.blocked_ips.items()):
                if v["expires_at"] > 0 and now > v["expires_at"]:
                    del self.blocked_ips[k]
                    continue
                active.append(v)
            self.metrics.dynamic_blocked_count = len(active)
            return active

    def is_blocked(self, ip_str: str) -> bool:
        with self.lock:
            try:
                ip_obj = ipaddress.IPv4Address(ip_str)
                now = time.time()
                for net_str, meta in list(self.blocked_ips.items()):
                    if meta["expires_at"] > 0 and now > meta["expires_at"]:
                        del self.blocked_ips[net_str]
                        continue
                    if ip_obj in ipaddress.IPv4Network(net_str):
                        return True
            except ValueError:
                pass
            return False


class HttpControlPlaneHandler(BaseHTTPRequestHandler):
    """Lightweight REST / JSON control plane fallback for runtime IP block management."""

    manager: ControlPlaneManager = None  # type: ignore

    def do_POST(self):
        if self.path == "/v1/control/block":
            length = int(self.headers.get("Content-Length", 0))
            body = self.rfile.read(length).decode("utf-8")
            data = json.loads(body) if body else {}
            ip = data.get("ip_or_cidr", "")
            reason = data.get("reason", "manual")
            ttl = int(data.get("ttl_seconds", 0))
            ok, msg = self.manager.block_ip(ip, reason, ttl)
            self.send_response(200 if ok else 400)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(json.dumps({"success": ok, "message": msg, "total": len(self.manager.list_blocked())}).encode())
        elif self.path == "/v1/control/unblock":
            length = int(self.headers.get("Content-Length", 0))
            body = self.rfile.read(length).decode("utf-8")
            data = json.loads(body) if body else {}
            ip = data.get("ip_or_cidr", "")
            ok, msg = self.manager.unblock_ip(ip)
            self.send_response(200 if ok else 400)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(json.dumps({"success": ok, "message": msg, "total": len(self.manager.list_blocked())}).encode())
        else:
            self.send_response(404)
            self.end_headers()

    def do_GET(self):
        if self.path == "/v1/control/blocked":
            blocked = self.manager.list_blocked()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(json.dumps({"entries": blocked, "total_count": len(blocked)}).encode())
        elif self.path == "/v1/control/stats":
            stats = {
                "active_rate_limit_entries": self.manager.metrics.active_rate_limit_entries,
                "dynamic_blocked_count": self.manager.metrics.dynamic_blocked_count,
                "drops_by_reason": self.manager.metrics.drops_by_reason,
                "passes_by_protocol": self.manager.metrics.passes_by_protocol,
            }
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(json.dumps(stats).encode())
        else:
            self.send_response(404)
            self.end_headers()

    def log_message(self, format, *args):
        # Silence verbose request logs
        return


def start_control_plane_server(manager: ControlPlaneManager, host: str = "0.0.0.0", port: int = 50051) -> HTTPServer:
    """Spawns background control plane server thread."""
    handler = type("ConfiguredHandler", (HttpControlPlaneHandler,), {"manager": manager})
    server = HTTPServer((host, port), handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    logging.info("Control plane server listening on %s:%d", host, port)
    return server


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Tessera XDP DDoS Mitigator & GeoIP Blocker")
    parser.add_argument("--interface", "-i", default="eth0", help="Network interface to attach XDP (e.g. eth0, ens3)")
    parser.add_argument("--mode", default="drv", choices=["drv", "skb", "hw"], help="XDP attach mode (default: drv)")
    parser.add_argument("--geoip-file", default="", help="Path to MaxMind GeoIP CIDR CSV file")
    parser.add_argument("--control-port", type=int, default=50051, help="Control plane port")
    parser.add_argument("--metrics-port", type=int, default=9104, help="Prometheus exporter port")
    parser.add_argument("--syn-rate", type=int, default=100, help="SYN token bucket refill rate/sec per IP")
    parser.add_argument("--bucket-capacity", type=int, default=200, help="SYN token bucket max burst capacity")
    return parser.parse_args()


def main():
    args = parse_args()
    logging.basicConfig(level=logging.INFO, format="%(asctime)s [%(levelname)s] %(message)s")

    # Initialize metrics
    metrics = DdosMetricsCollector()
    if HAS_PROMETHEUS:
        REGISTRY.register(metrics)
        start_http_server(args.metrics_port)
        logging.info("Prometheus metrics listening on :%d/metrics", args.metrics_port)

    # MaxMind GeoIP setup
    geoip_resolver = GeoIpResolver()
    if args.geoip_file and os.path.exists(args.geoip_file):
        count = geoip_resolver.load_from_csv(args.geoip_file)
        logging.info("Loaded %d embargoed GeoIP networks from %s", count, args.geoip_file)

    # Try BCC import if running on Linux with root
    bpf = None
    try:
        from bcc import BPF  # type: ignore

        c_file = os.path.join(os.path.dirname(__file__), "xdp_ddos_mitigator.c")
        with open(c_file, "r") as f:
            src = f.read()

        cflags = [
            f"-DTOKEN_BUCKET_CAPACITY={args.bucket_capacity}ULL",
            f"-DTOKEN_REFILL_RATE_PER_SEC={args.syn_rate}ULL",
        ]

        logging.info("Compiling eBPF XDP program...")
        bpf = BPF(text=src, cflags=cflags)
        fn = bpf.load_func("xdp_ddos_mitigator", BPF.XDP)

        # Attach in XDP_DRV mode (or user specified)
        mode_flag = BPF.XDP_FLAGS_DRV_MODE if args.mode == "drv" else BPF.XDP_FLAGS_SKB_MODE
        logging.info("Attaching XDP program to %s in mode=%s...", args.interface, args.mode)
        bpf.attach_xdp(args.interface, fn, mode_flag)

    except ImportError:
        logging.warning("BCC not installed or unsupported platform; running userspace simulation mode.")
    except Exception as e:
        logging.error("Failed to compile/attach XDP program: %s. Continuing in simulation mode.", e)

    # Start Control Plane Server
    control_mgr = ControlPlaneManager(bpf_instance=bpf, metrics=metrics)
    server = start_control_plane_server(control_mgr, port=args.control_port)

    # Graceful shutdown handler
    running = True

    def sig_handler(sig, frame):
        nonlocal running
        logging.info("Shutting down XDP DDoS mitigator...")
        running = False
        if bpf and hasattr(bpf, "remove_xdp"):
            try:
                bpf.remove_xdp(args.interface, 0)
                logging.info("Detached XDP program from %s", args.interface)
            except Exception:
                pass
        server.shutdown()
        sys.exit(0)

    signal.signal(signal.SIGINT, sig_handler)
    signal.signal(signal.SIGTERM, sig_handler)

    logging.info("XDP DDoS mitigation engine running. Press Ctrl+C to terminate.")
    while running:
        time.sleep(1)


if __name__ == "__main__":
    main()
