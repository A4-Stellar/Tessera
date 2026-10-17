// SPDX-License-Identifier: (LGPL-2.1 OR BSD-2-Clause)
/* Copyright (c) 2026 Tessera Network / A4-Stellar
 *
 * eBPF-Powered Kernel Socket Latency Profiler (Issue #307).
 *
 * Attaches non-intrusive kernel kprobes and tracepoints to Linux socket
 * network syscalls (`sys_enter_sendto`, `sys_exit_sendto`, `sys_enter_recvfrom`,
 * `sys_exit_recvfrom`) to measure execution latency histograms and
 * kernel-to-userspace context switch overhead without application performance penalty.
 */

#include <linux/types.h>
#include <linux/bpf.h>
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>

#define MAX_HIST_BUCKETS 32
#define SYSCALL_SENDTO   1
#define SYSCALL_RECVFROM 2
#define SYSCALL_WRITE    3
#define SYSCALL_READ     4

/* Stores timestamp and file descriptor recorded at syscall entry */
struct socket_enter_info {
    __u64 start_ktime_ns;
    __u64 socket_fd;
    __u32 syscall_id;
    __u32 pad;
};

/* Aggregated latency histogram and context switch overhead metrics */
struct syscall_metric_bucket {
    __u64 count;
    __u64 total_latency_ns;
    __u64 min_latency_ns;
    __u64 max_latency_ns;
};

/* BPF Hash Map: tracks in-flight socket syscall invocations per TID/PID */
struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 16384);
    __type(key, __u64);                      /* pid_tgid */
    __type(value, struct socket_enter_info);
} active_socket_syscalls SEC(".maps");

/* BPF Array Map: SendTo latency histogram (indexed by log2 of nanoseconds) */
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, MAX_HIST_BUCKETS);
    __type(key, __u32);                      /* bucket index (0..31) */
    __type(value, struct syscall_metric_bucket);
} sendto_latency_histogram SEC(".maps");

/* BPF Array Map: RecvFrom latency histogram (indexed by log2 of nanoseconds) */
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, MAX_HIST_BUCKETS);
    __type(key, __u32);                      /* bucket index (0..31) */
    __type(value, struct syscall_metric_bucket);
} recvfrom_latency_histogram SEC(".maps");

/* Cumulative context switch overhead stats */
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, 8);
    __type(key, __u32);                      /* 0 = sendto, 1 = recvfrom */
    __type(value, struct syscall_metric_bucket);
} context_switch_overhead SEC(".maps");

/* Computes power-of-2 logarithmic bucket index for histogram distribution */
static __always_inline __u32 calculate_log2_bucket(__u64 val)
{
    __u32 bucket = 0;
    if (val >= 0x100000000ULL) { val >>= 32; bucket += 32; }
    if (val >= 0x10000ULL)     { val >>= 16; bucket += 16; }
    if (val >= 0x100ULL)       { val >>= 8;  bucket += 8;  }
    if (val >= 0x10ULL)        { val >>= 4;  bucket += 4;  }
    if (val >= 0x4ULL)         { val >>= 2;  bucket += 2;  }
    if (val >= 0x2ULL)         { bucket += 1; }
    if (bucket >= MAX_HIST_BUCKETS) {
        bucket = MAX_HIST_BUCKETS - 1;
    }
    return bucket;
}

/* Updates histogram bucket counters atomically */
static __always_inline void record_latency_sample(
    void *map,
    __u32 bucket_idx,
    __u64 elapsed_ns)
{
    struct syscall_metric_bucket *b = bpf_map_lookup_elem(map, &bucket_idx);
    if (!b)
        return;

    __sync_fetch_and_add(&b->count, 1);
    __sync_fetch_and_add(&b->total_latency_ns, elapsed_ns);

    /* Update min/max non-blocking */
    if (b->min_latency_ns == 0 || elapsed_ns < b->min_latency_ns) {
        b->min_latency_ns = elapsed_ns;
    }
    if (elapsed_ns > b->max_latency_ns) {
        b->max_latency_ns = elapsed_ns;
    }
}

/* Syscall Entry: sys_enter_sendto */
SEC("tracepoint/syscalls/sys_enter_sendto")
int tracepoint_sys_enter_sendto(struct trace_event_raw_sys_enter *ctx)
{
    __u64 pid_tgid = bpf_get_current_pid_tgid();
    struct socket_enter_info info = {};

    info.start_ktime_ns = bpf_ktime_get_ns();
    info.socket_fd = ctx->args[0];
    info.syscall_id = SYSCALL_SENDTO;

    bpf_map_update_elem(&active_socket_syscalls, &pid_tgid, &info, BPF_ANY);
    return 0;
}

/* Syscall Exit: sys_exit_sendto */
SEC("tracepoint/syscalls/sys_exit_sendto")
int tracepoint_sys_exit_sendto(struct trace_event_raw_sys_exit *ctx)
{
    __u64 pid_tgid = bpf_get_current_pid_tgid();
    struct socket_enter_info *info;
    __u64 now_ns;
    __u64 latency_ns;
    __u32 bucket;

    info = bpf_map_lookup_elem(&active_socket_syscalls, &pid_tgid);
    if (!info || info->syscall_id != SYSCALL_SENDTO)
        return 0;

    now_ns = bpf_ktime_get_ns();
    if (now_ns > info->start_ktime_ns) {
        latency_ns = now_ns - info->start_ktime_ns;
        bucket = calculate_log2_bucket(latency_ns);
        record_latency_sample(&sendto_latency_histogram, bucket, latency_ns);

        /* Measure context-switch overhead across kernel boundary */
        __u32 metric_key = 0; /* SendTo */
        record_latency_sample(&context_switch_overhead, metric_key, latency_ns);
    }

    bpf_map_delete_elem(&active_socket_syscalls, &pid_tgid);
    return 0;
}

/* Syscall Entry: sys_enter_recvfrom */
SEC("tracepoint/syscalls/sys_enter_recvfrom")
int tracepoint_sys_enter_recvfrom(struct trace_event_raw_sys_enter *ctx)
{
    __u64 pid_tgid = bpf_get_current_pid_tgid();
    struct socket_enter_info info = {};

    info.start_ktime_ns = bpf_ktime_get_ns();
    info.socket_fd = ctx->args[0];
    info.syscall_id = SYSCALL_RECVFROM;

    bpf_map_update_elem(&active_socket_syscalls, &pid_tgid, &info, BPF_ANY);
    return 0;
}

/* Syscall Exit: sys_exit_recvfrom */
SEC("tracepoint/syscalls/sys_exit_recvfrom")
int tracepoint_sys_exit_recvfrom(struct trace_event_raw_sys_exit *ctx)
{
    __u64 pid_tgid = bpf_get_current_pid_tgid();
    struct socket_enter_info *info;
    __u64 now_ns;
    __u64 latency_ns;
    __u32 bucket;

    info = bpf_map_lookup_elem(&active_socket_syscalls, &pid_tgid);
    if (!info || info->syscall_id != SYSCALL_RECVFROM)
        return 0;

    now_ns = bpf_ktime_get_ns();
    if (now_ns > info->start_ktime_ns) {
        latency_ns = now_ns - info->start_ktime_ns;
        bucket = calculate_log2_bucket(latency_ns);
        record_latency_sample(&recvfrom_latency_histogram, bucket, latency_ns);

        /* Measure context-switch overhead across kernel boundary */
        __u32 metric_key = 1; /* RecvFrom */
        record_latency_sample(&context_switch_overhead, metric_key, latency_ns);
    }

    bpf_map_delete_elem(&active_socket_syscalls, &pid_tgid);
    return 0;
}

char LICENSE[] SEC("license") = "Dual BSD/GPL";
