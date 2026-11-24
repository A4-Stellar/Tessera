# eBPF-Powered Kernel Socket Latency Profiler

Non-intrusive Linux eBPF kernel probes measuring socket read/write execution latencies and kernel-to-userspace context switch overhead (Issue #307).

## Architecture

- **eBPF Kernel Probes (`socket_profiler.c`)**:
  - Attached via tracepoints / kprobes to Linux network syscalls:
    - `sys_enter_sendto` and `sys_exit_sendto`
    - `sys_enter_recvfrom` and `sys_exit_recvfrom`
  - High-precision kernel nanosecond timestamps via `bpf_ktime_get_ns()`.
  - Log2 power-of-2 logarithmic latency histogram buckets (1us .. 1s+).
  - Measures execution duration and kernel-to-userspace context switch transitions without application-layer overhead.

- **Userspace Exporter (`socket_profiler_loader.py`)**:
  - Reads eBPF maps and publishes metrics at `:9102/metrics` in standard Prometheus exposition format.

## HTTP/2 XDP profiler (issue #392)

`http2_profiler.c` + `http2_profiler.py` add a second, HTTP/2-aware profiler
that attaches at the pod veth's XDP hook and needs **no code instrumentation**:

- Parses Ethernet → IPv4 → TCP from raw packets with `bpf_skb_load_bytes`;
  recognises the HTTP/2 client preface and walks `HEADERS`, `DATA` and
  `RST_STREAM` frames by their 9-byte frame header.
- Decodes the HPACK static-table subset for `:method`, `:path`, `:status` and
  `content-length`; paths are hashed with FNV-1a so no raw URL leaves the kernel.
- Emits `(timestamp_ns, pid, path_hash, status, latency_ns)` tuples through a
  `perf_event_array` (`BPF_PERF_OUTPUT`) and raw frame summaries through a BPF
  **ring buffer**. A `tcp_connect` kprobe maps the XDP socket cookie to a pid.
- The Python loader uses `BPF.attach_xdp` on the Kubernetes pod veth, drains
  both maps off the scrape path, and exports
  `tessera_http2_request_duration_seconds{method,status}` plus frame/event
  counters as Prometheus histograms/counters.

```bash
sudo ./http2_profiler.py --interface cali1234abcd --listen-port 8080 --port 9437
curl http://localhost:9437/metrics | grep tessera_http2
```

For a load test, generate cleartext h2 traffic through the pod veth (`h2load
-n1000 -c10 -m100 http://<pod-ip>:8080/stats`) and compare the exported p50/p99
with `strace -ttt -T -f -e trace=read,write` samples; both are measured at the
socket boundary and should agree within the 2 % measurement tolerance.

## Network-Level XDP DDoS Mitigation & GeoIP Blocker (Issue #44 / #425)

`xdp_ddos_mitigator.c` + `xdp_ddos_loader.py` provide kernel line-rate defense-in-depth against volumetric DDoS:

1. **SYN Flood Rate Limiting**:
   - Uses a 1,000,000-entry BPF `LRU_HASH` map (`syn_rate_limit`) with a token bucket algorithm.
   - Drops SYN packets when an IP's token bucket is exhausted, passing legitimate connections.
2. **GeoIP Embargo Enforcement**:
   - Parses MaxMind GeoIP subnet datasets into BPF lookup tables to reject incoming packets from embargoed jurisdictions (`CU`, `IR`, `KP`, `SY`, `RU`).
3. **gRPC Control Plane**:
   - Exposes dynamic block/unblock RPCs (`BlockIp`, `UnblockIp`, `ListBlocked`, `GetStats`) allowing backend services to mitigate threats in real time.
4. **Telemetry & Prometheus**:
   - Drop events are streamed via `perf_event_array` (`drop_events`) and exposed as `tessera_xdp_dropped_packets_total{reason}` and `tessera_xdp_passed_packets_total{protocol}`.
5. **Native Cloud Driver Execution**:
   - Runs in `XDP_DRV` native driver mode on cloud NICs for line-rate forwarding.

### Run it

```bash
sudo ./xdp_ddos_loader.py --interface eth0 --mode drv --control-port 50051 --metrics-port 9104
```

### 10 Gbps SYN Flood Load Test

```bash
python3 infrastructure/ebpf/load_test_syn_flood.py
```

## Running Tests

```bash
python3 -m unittest infrastructure/ebpf/test_socket_profiler.py
python3 -m unittest infrastructure/ebpf/test_xdp_ddos.py
```
