#!/usr/bin/env python3
"""Loader and Prometheus exporter for http2_profiler.c (issue #392).

Compiles the XDP program, attaches it to a Kubernetes pod veth interface and
exports HTTP/2 request latency straight from kernel space — no application or
sidecar instrumentation is required:

  tessera_http2_request_duration_seconds{method,status}
      time from the client HEADERS frame (:method/:path) to the server HEADERS
      frame (:status) carrying the response, or to RST_STREAM.
  tessera_http2_events_total{kind,status}
      completed requests/resets as observed at the driver hook.
  tessera_http2_frames_total{type}
      HEADERS/DATA/RST_STREAM frames seen in the ring buffer.

The eBPF program emits its tuples into a `perf_event_array` (`http2_events`)
and raw frame summaries into a BPF ring buffer (`http2_frames`). Both are
drained off the scrape path; Prometheus only reads the aggregator's in-memory
counters.

Requires root (or CAP_BPF + CAP_NET_ADMIN), BCC (python3-bpfcc), a kernel with
XDP + BPF ring buffer support (>= 5.8), and prometheus-client.

  sudo ./http2_profiler.py --interface cali1234abcd --listen-port 8080
"""

from __future__ import annotations

import argparse
import os
import signal
import sys
import threading
from collections import defaultdict
from dataclasses import dataclass, field
from typing import Any, Callable

from prometheus_client import REGISTRY, start_http_server
from prometheus_client.core import CounterMetricFamily, HistogramMetricFamily
from prometheus_client.utils import floatToGoString

# Keep in sync with `enum` constants in http2_profiler.c.
FRAME_NAMES = {0x0: "DATA", 0x1: "HEADERS", 0x3: "RST_STREAM"}
METHOD_NAMES = {0: "UNKNOWN", 1: "GET", 2: "POST"}
COMPLETION = 0
RESET = 1

# bpf_log2l() slot k means a latency in [2^(k-1), 2^k - 1] ns (slot 1: 0..1).
MAX_SLOT = 36
BUCKET_BOUNDS = [floatToGoString(((1 << k) - 1) / 1e9) for k in range(1, MAX_SLOT + 1)]


def slot_for(nanoseconds: int) -> int:
    """Bucket index used by the kernel's bpf_log2l() helper."""
    if nanoseconds <= 1:
        return 1
    return min(MAX_SLOT, nanoseconds.bit_length())


def cumulative_buckets(slots: dict[int, int]) -> list[tuple[str, int]]:
    """Convert log2 slot counts into cumulative Prometheus buckets."""
    buckets: list[tuple[str, int]] = []
    running = 0
    for slot, bound in enumerate(BUCKET_BOUNDS, start=1):
        running += slots.get(slot, 0)
        buckets.append((bound, running))
    buckets.append(("+Inf", sum(slots.values())))
    return buckets


@dataclass
class Http2Event:
    timestamp_ns: int
    latency_ns: int
    pid: int
    path_hash: int
    method: int
    status: int
    kind: int


@dataclass
class AggregatedMetrics:
    """Everything exported to Prometheus; updated from the perf-buffer thread."""

    latencies: dict[tuple[str, str], dict[int, int]] = field(default_factory=dict)
    latency_sums: dict[tuple[str, str], float] = field(default_factory=dict)
    events: dict[tuple[str, str], int] = field(default_factory=dict)
    frames: dict[str, int] = field(default_factory=lambda: defaultdict(int))

    def add_event(self, event: Http2Event) -> None:
        method = METHOD_NAMES.get(event.method, "UNKNOWN")
        status = "RST_STREAM" if event.kind == RESET else str(event.status)
        key = (method, status)
        slots = self.latencies.setdefault(key, {})
        bucket = slot_for(event.latency_ns)
        slots[bucket] = slots.get(bucket, 0) + 1
        self.latency_sums[key] = self.latency_sums.get(key, 0.0) + event.latency_ns / 1e9
        kind = "reset" if event.kind == RESET else "response"
        event_key = (kind, status)
        self.events[event_key] = self.events.get(event_key, 0) + 1

    def add_frame(self, frame_type: int) -> None:
        self.frames[FRAME_NAMES.get(frame_type, f"type_{frame_type}")] += 1


class Http2Collector:
    """prometheus_client custom collector reading only the aggregator."""

    def __init__(self, metrics: AggregatedMetrics):
        self.metrics = metrics

    def collect(self):
        latency = HistogramMetricFamily(
            "tessera_http2_request_duration_seconds",
            "HTTP/2 request latency measured in XDP from client HEADERS to "
            "server HEADERS/RST_STREAM, without application instrumentation.",
            labels=["method", "status"],
        )
        for (method, status), slots in sorted(self.metrics.latencies.items()):
            latency.add_metric(
                [method, status],
                cumulative_buckets(slots),
                sum_value=self.metrics.latency_sums.get((method, status), 0.0),
            )
        yield latency

        events = CounterMetricFamily(
            "tessera_http2_events_total",
            "Completed HTTP/2 requests and resets observed at the veth XDP hook.",
            labels=["kind", "status"],
        )
        for (kind, status), count in sorted(self.metrics.events.items()):
            events.add_metric([kind, status], count)
        yield events

        frames = CounterMetricFamily(
            "tessera_http2_frames_total",
            "HTTP/2 frame counts parsed from raw packets in the eBPF ring buffer.",
            labels=["type"],
        )
        for frame_type, count in sorted(self.metrics.frames.items()):
            frames.add_metric([frame_type], count)
        yield frames


def coerce_event(raw: Any) -> Http2Event:
    """Normalise a BCC perf-buffer struct (or a dict in unit tests)."""

    def get(name: str, default: int = 0) -> int:
        if isinstance(raw, dict):
            return int(raw.get(name, default))
        return int(getattr(raw, name, default))

    return Http2Event(
        timestamp_ns=get("timestamp_ns"),
        latency_ns=get("latency_ns"),
        pid=get("pid"),
        path_hash=get("path_hash"),
        method=get("method"),
        status=get("status"),
        kind=get("kind"),
    )


def _drain_perf_buffer(bpf, metrics: AggregatedMetrics, stop: threading.Event) -> None:
    table = bpf["http2_events"]

    def on_event(_cpu: int, data: Any, _size: int) -> None:
        try:
            metrics.add_event(coerce_event(table.event(data)))
        except Exception as exc:  # never let a malformed record kill the poller
            print(f"[http2-profiler] dropping event: {exc}", file=sys.stderr)

    table.open_perf_buffer(on_event, page_cnt=64)
    while not stop.is_set():
        bpf.perf_buffer_poll(timeout=500)


def _drain_ring_buffer(bpf, metrics: AggregatedMetrics, stop: threading.Event) -> None:
    """Consume raw frame records; failure here must not stop latency export."""
    try:
        table = bpf["http2_frames"]

        def on_frame(_cpu: int, data: Any, _size: int) -> None:
            try:
                frame = table.event(data)
                metrics.add_frame(int(getattr(frame, "type", 0)))
            except Exception:
                pass

        table.open_ring_buffer(on_frame)
        while not stop.is_set():
            bpf.ring_buffer_poll(timeout=500)
    except (AttributeError, KeyError, NotImplementedError) as exc:
        print(f"[http2-profiler] ring buffer unavailable: {exc}", file=sys.stderr)


def build_bpf(source: str, listen_port: int, max_scan: int, max_frames: int, bcc_module):
    cflags = [
        f"-DTARGET_PORT={listen_port}",
        f"-DMAX_HPACK_SCAN={max_scan}",
        f"-DMAX_FRAMES={max_frames}",
    ]
    return bcc_module.BPF(src_file=source, cflags=cflags)


def attach_xdp(bpf, interface: str, bcc_module) -> Any:
    fn = bpf.load_func("xdp_http2", bcc_module.XDP)
    bpf.attach_xdp(ifname=interface, fn=fn, flags=0)
    return fn


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    parser.add_argument(
        "--interface", "--veth", dest="interface", required=True,
        help="Kubernetes pod veth interface to attach the XDP program to",
    )
    parser.add_argument("--listen-port", type=int, default=8080,
                        help="container port serving cleartext HTTP/2")
    parser.add_argument("--max-hpack-scan", type=int, default=256,
                        help="maximum HEADERS fragment bytes scanned per frame")
    parser.add_argument("--max-frames", type=int, default=8,
                        help="maximum HTTP/2 frames parsed per packet")
    parser.add_argument("--addr", default="0.0.0.0", help="metrics listen address")
    parser.add_argument("--port", type=int, default=9437, help="metrics listen port")
    args = parser.parse_args(argv)
    if not 0 < args.listen_port < 65536:
        parser.error("--listen-port must be between 1 and 65535")
    if args.max_hpack_scan < 16 or args.max_hpack_scan > 4096:
        parser.error("--max-hpack-scan must be between 16 and 4096")
    if not 1 <= args.max_frames <= 32:
        parser.error("--max-frames must be between 1 and 32")
    return args


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    syspath = f"/sys/class/net/{args.interface}"
    if not os.path.isdir(syspath):
        print(f"[http2-profiler] interface {args.interface!r} does not exist", file=sys.stderr)
        return 2

    # BCC is imported lazily so unit tests can import the pure helpers.
    import bcc  # type: ignore[import-not-found]

    source = os.path.join(os.path.dirname(os.path.abspath(__file__)), "http2_profiler.c")
    bpf = build_bpf(source, args.listen_port, args.max_hpack_scan, args.max_frames, bcc)
    attach_xdp(bpf, args.interface, bcc)

    metrics = AggregatedMetrics()
    REGISTRY.register(Http2Collector(metrics))
    start_http_server(args.port, addr=args.addr)

    stop = threading.Event()
    threads = [
        threading.Thread(target=drain, args=(bpf, metrics, stop), daemon=True)
        for drain in (_drain_perf_buffer, _drain_ring_buffer)
    ]
    for thread in threads:
        thread.start()

    for sig in (signal.SIGINT, signal.SIGTERM):
        signal.signal(sig, lambda *_: stop.set())
    try:
        stop.wait()
    finally:
        bpf.remove_xdp(ifname=args.interface, flags=0)
        print(
            f"[http2-profiler] detached from {args.interface}; metrics on "
            f"http://{args.addr}:{args.port}/metrics",
            flush=True,
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
