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

## Running Tests

```bash
python -m unittest infrastructure.ebpf.test_socket_profiler
```
