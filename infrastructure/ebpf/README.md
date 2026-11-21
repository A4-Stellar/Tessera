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

## Running Tests

```bash
python -m unittest infrastructure.ebpf.test_socket_profiler
python infrastructure/ebpf/test_http2_profiler.py
```
