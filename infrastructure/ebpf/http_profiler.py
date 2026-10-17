#!/usr/bin/env python3
"""Loader and Prometheus exporter for http_profiler.c (Issue #258).

Compiles the BCC program, attaches its syscall tracepoints to one process
and serves the in-kernel log2 histograms as native Prometheus histograms:

  tessera_http_request_duration_seconds              request read -> response write
  tessera_socket_syscall_duration_seconds{syscall}   syscall entry -> return to user

Maps are read only when Prometheus scrapes (no polling loop), so a scrape
costs O(S * B) for S series and B buckets; nothing runs between scrapes.

Requires root (or CAP_BPF + CAP_PERFMON), BCC (python3-bpfcc) and kernel >= 5.5.

  sudo ./http_profiler.py --comm tessera-api --port 9436
"""

from __future__ import annotations

import argparse
import os
import re
import signal
import threading
from collections import defaultdict

from bcc import BPF
from prometheus_client import REGISTRY, start_http_server
from prometheus_client.core import HistogramMetricFamily
from prometheus_client.utils import floatToGoString

# Order must match `enum sys_id` in http_profiler.c.
SYSCALLS = ("read", "recvfrom", "write", "writev", "sendto")
SUM_HTTP = len(SYSCALLS)
# bpf_log2l() slot k holds v in [2^(k-1), 2^k - 1] ns (slot 1 holds 0..1).
# A fixed slot range keeps the exported bucket set stable across scrapes;
# slot 36 tops out at ~68.7s, anything slower lands in +Inf only.
MAX_SLOT = 36
BUCKET_BOUNDS = [floatToGoString(((1 << k) - 1) / 1e9) for k in range(1, MAX_SLOT + 1)]
COMM_RE = re.compile(r"^[A-Za-z0-9_.-]{1,15}$")  # TASK_COMM_LEN - 1


def to_buckets(slots: dict[int, int]) -> list[tuple[str, int]]:
    """Convert per-slot counts into cumulative Prometheus buckets."""
    buckets, running = [], 0
    for slot, bound in enumerate(BUCKET_BOUNDS, start=1):
        running += slots.get(slot, 0)
        buckets.append((bound, running))
    buckets.append(("+Inf", sum(slots.values())))
    return buckets


class ProfilerCollector:
    def __init__(self, bpf: BPF):
        self._http = bpf["http_hist"]
        self._syscalls = bpf["syscall_hist"]
        self._sums = bpf["sums"]

    def collect(self):
        http = HistogramMetricFamily(
            "tessera_http_request_duration_seconds",
            "HTTP/1.x latency at the socket layer: request bytes delivered to "
            "the process until its response status line is written.",
        )
        http.add_metric([], to_buckets({k.value: v.value for k, v in self._http.items()}),
                        sum_value=self._sums[SUM_HTTP].value / 1e9)
        yield http

        per_syscall: dict[int, dict[int, int]] = defaultdict(dict)
        for key, count in self._syscalls.items():
            per_syscall[key.sys][key.slot] = count.value
        syscalls = HistogramMetricFamily(
            "tessera_socket_syscall_duration_seconds",
            "Time from syscall entry to return to user space for socket reads/writes.",
            labels=["syscall"],
        )
        for idx, name in enumerate(SYSCALLS):
            syscalls.add_metric([name], to_buckets(per_syscall[idx]),
                                sum_value=self._sums[idx].value / 1e9)
        yield syscalls


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    target = parser.add_mutually_exclusive_group(required=True)
    target.add_argument("--pid", type=int, help="trace this process id")
    target.add_argument("--comm", help="trace processes whose main-thread name is exactly "
                                       "this (all their threads), e.g. tessera-api")
    parser.add_argument("--addr", default="0.0.0.0", help="metrics listen address")
    parser.add_argument("--port", type=int, default=9436, help="metrics listen port")
    args = parser.parse_args()
    if args.pid is not None and args.pid <= 0:
        parser.error("--pid must be a positive process id")
    if args.comm is not None and not COMM_RE.match(args.comm):
        parser.error("--comm must be 1-15 characters of [A-Za-z0-9_.-]")

    source = os.path.join(os.path.dirname(os.path.abspath(__file__)), "http_profiler.c")
    bpf = BPF(src_file=source, cflags=[
        f"-DTARGET_TGID={args.pid or 0}",
        f'-DTARGET_COMM="{args.comm or ""}"',
    ])
    REGISTRY.register(ProfilerCollector(bpf))
    start_http_server(args.port, addr=args.addr)
    print(f"tracing {'pid ' + str(args.pid) if args.pid else 'comm ' + args.comm}; "
          f"metrics on http://{args.addr}:{args.port}/metrics", flush=True)

    stop = threading.Event()
    for sig in (signal.SIGINT, signal.SIGTERM):
        signal.signal(sig, lambda *_: stop.set())
    stop.wait()  # Tracepoints detach when the BPF object is released on exit.


if __name__ == "__main__":
    main()
