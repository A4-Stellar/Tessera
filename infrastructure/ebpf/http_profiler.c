// SPDX-License-Identifier: GPL-2.0
//
// Tessera — eBPF HTTP request latency profiler (Issue #258).
//
// BCC program, compiled and attached by http_profiler.py. It hooks the
// syscall tracepoints of the socket read/write paths of ONE target process
// (no application changes, no user-space instrumentation) and produces two
// log2 histograms that the loader exports to Prometheus:
//
//  1. http_hist     — HTTP/1.x request latency per connection: from the
//                     moment request bytes are handed to user space (exit of
//                     the read that returns "<METHOD> ") to the moment the
//                     application hands the response back to the kernel
//                     (entry of the write that starts with "HTTP/1.").
//  2. syscall_hist  — per-syscall in-kernel latency: sys_enter -> sys_exit,
//                     i.e. the user->kernel->user round trip of each socket
//                     read/write.
//
// Ingress: read(2) and recvfrom(2) — Rust's std/tokio TcpStream::read issues
// recv(), which is the recvfrom syscall on x86_64 and arm64.
// Egress:  write(2), writev(2) and sendto(2) — hyper writes responses with
// writev (vectored) or send() == sendto.
//
// Cost per syscall is O(1): one LRU-hash lookup/update and a <=16 byte user
// read. Space is O(T + C): T threads inside a traced syscall, C connections
// with a request in flight, both bounded by LRU map capacity.
//
// Compile-time parameters supplied by the loader:
//   TARGET_TGID  process id to trace (0 = match by TARGET_COMM instead)
//   TARGET_COMM  process (thread-group leader) name, e.g. "tessera-api"

#include <uapi/linux/ptrace.h>
#include <linux/sched.h>
#include <linux/uio.h>

enum sys_id { SYS_READ, SYS_RECVFROM, SYS_WRITE, SYS_WRITEV, SYS_SENDTO, SYS_COUNT };
#define SUM_HTTP SYS_COUNT  // index of the HTTP latency sum in `sums`

struct inflight_syscall {
    u64 start_ns;
    u64 buf;   // user buffer of an ingress syscall, read at exit
    u32 fd;
    u32 sys;
};

struct conn_key {
    u32 tgid;
    u32 fd;
};

struct sys_slot {
    u32 sys;
    u32 slot;
};

BPF_TABLE("lru_hash", u64, struct inflight_syscall, active, 16384);
BPF_TABLE("lru_hash", struct conn_key, u64, requests, 65536);
BPF_HISTOGRAM(http_hist, u64, 64);
BPF_HISTOGRAM(syscall_hist, struct sys_slot, 64 * SYS_COUNT);
BPF_ARRAY(sums, u64, SYS_COUNT + 1);

static __always_inline bool is_target(u64 pid_tgid)
{
#if TARGET_TGID
    return (pid_tgid >> 32) == TARGET_TGID;
#else
    // bpf_get_current_comm() returns the *thread* name, and runtimes rename
    // their I/O threads (tokio workers are "tokio-rt-worker"), so match the
    // thread-group leader, whose comm is the executable name.
    struct task_struct *task = (struct task_struct *)bpf_get_current_task();
    struct task_struct *leader = NULL;
    char comm[TASK_COMM_LEN] = {};
    const char want[] = TARGET_COMM;
    bpf_probe_read_kernel(&leader, sizeof(leader), &task->group_leader);
    bpf_probe_read_kernel(&comm, sizeof(comm), &leader->comm);
#pragma unroll
    for (u32 i = 0; i < sizeof(want); i++) {
        if (comm[i] != want[i])
            return false;
    }
    return true;
#endif
}

static __always_inline void add_sum(int idx, u64 value)
{
    u64 *sum = sums.lookup(&idx);
    if (sum)
        __sync_fetch_and_add(sum, value);
}

// Request lines start with one of the RFC 9110 methods followed by a space.
static __always_inline bool is_http_request(const char *p)
{
    switch (p[0]) {
    case 'G': return p[1] == 'E' && p[2] == 'T' && p[3] == ' ';
    case 'H': return p[1] == 'E' && p[2] == 'A' && p[3] == 'D' && p[4] == ' ';
    case 'P':
        return (p[1] == 'O' && p[2] == 'S' && p[3] == 'T' && p[4] == ' ') ||
               (p[1] == 'U' && p[2] == 'T' && p[3] == ' ') ||
               (p[1] == 'A' && p[2] == 'T' && p[3] == 'C' && p[4] == 'H' && p[5] == ' ');
    case 'D': return p[1] == 'E' && p[2] == 'L' && p[3] == 'E' && p[4] == 'T' && p[5] == 'E' && p[6] == ' ';
    case 'O': return p[1] == 'P' && p[2] == 'T' && p[3] == 'I' && p[4] == 'O' && p[5] == 'N' && p[6] == 'S' && p[7] == ' ';
    default:  return false;
    }
}

// Final responses only: "HTTP/1.x NNN" where NNN is not an interim 1xx.
static __always_inline bool is_final_http_response(const char *p)
{
    return p[0] == 'H' && p[1] == 'T' && p[2] == 'T' && p[3] == 'P' && p[4] == '/' &&
           p[5] == '1' && p[6] == '.' && p[8] == ' ' && p[9] >= '2' && p[9] <= '5';
}

static __always_inline void syscall_enter(u64 pid_tgid, u32 sys, u32 fd, u64 buf)
{
    struct inflight_syscall call = {
        .start_ns = bpf_ktime_get_ns(), .buf = buf, .fd = fd, .sys = sys,
    };
    active.update(&pid_tgid, &call);
}

static __always_inline void syscall_exit(long ret)
{
    u64 pid_tgid = bpf_get_current_pid_tgid();
    struct inflight_syscall *call = active.lookup(&pid_tgid);
    if (!call)
        return;  // not a traced thread
    u64 now = bpf_ktime_get_ns();
    u64 delta = now - call->start_ns;
    struct sys_slot key = { .sys = call->sys, .slot = bpf_log2l(delta) };
    syscall_hist.increment(key);
    add_sum(call->sys, delta);

    // Ingress: the first read of a request starts its latency clock.
    if (ret > 0 && (call->sys == SYS_READ || call->sys == SYS_RECVFROM)) {
        char head[8] = {};
        if (bpf_probe_read_user(&head, sizeof(head), (void *)call->buf) == 0 &&
            is_http_request(head)) {
            struct conn_key conn = { .tgid = pid_tgid >> 32, .fd = call->fd };
            requests.insert(&conn, &now);  // keeps the earliest of pipelined requests
        }
    }
    active.delete(&pid_tgid);
}

// Egress: the response status line stops the clock for that connection.
static __always_inline void response_start(u64 pid_tgid, u32 fd, const void *data)
{
    struct conn_key conn = { .tgid = pid_tgid >> 32, .fd = fd };
    u64 *start = requests.lookup(&conn);
    if (!start)
        return;
    char head[10] = {};
    if (bpf_probe_read_user(&head, sizeof(head), data) != 0 || !is_final_http_response(head))
        return;
    u64 delta = bpf_ktime_get_ns() - *start;
    http_hist.increment(bpf_log2l(delta));
    add_sum(SUM_HTTP, delta);
    requests.delete(&conn);
}

#define TARGET_OR_RETURN(id)          \
    u64 id = bpf_get_current_pid_tgid(); \
    if (!is_target(id))               \
        return 0

TRACEPOINT_PROBE(syscalls, sys_enter_read)
{
    TARGET_OR_RETURN(id);
    syscall_enter(id, SYS_READ, args->fd, (u64)args->buf);
    return 0;
}

TRACEPOINT_PROBE(syscalls, sys_enter_recvfrom)
{
    TARGET_OR_RETURN(id);
    syscall_enter(id, SYS_RECVFROM, args->fd, (u64)args->ubuf);
    return 0;
}

TRACEPOINT_PROBE(syscalls, sys_enter_write)
{
    TARGET_OR_RETURN(id);
    syscall_enter(id, SYS_WRITE, args->fd, 0);
    response_start(id, args->fd, args->buf);
    return 0;
}

TRACEPOINT_PROBE(syscalls, sys_enter_writev)
{
    TARGET_OR_RETURN(id);
    syscall_enter(id, SYS_WRITEV, args->fd, 0);
    struct iovec first = {};
    if (args->vlen > 0 && bpf_probe_read_user(&first, sizeof(first), args->vec) == 0)
        response_start(id, args->fd, first.iov_base);
    return 0;
}

TRACEPOINT_PROBE(syscalls, sys_enter_sendto)
{
    TARGET_OR_RETURN(id);
    syscall_enter(id, SYS_SENDTO, args->fd, 0);
    response_start(id, args->fd, args->buff);
    return 0;
}

TRACEPOINT_PROBE(syscalls, sys_exit_read)     { syscall_exit(args->ret); return 0; }
TRACEPOINT_PROBE(syscalls, sys_exit_recvfrom) { syscall_exit(args->ret); return 0; }
TRACEPOINT_PROBE(syscalls, sys_exit_write)    { syscall_exit(args->ret); return 0; }
TRACEPOINT_PROBE(syscalls, sys_exit_writev)   { syscall_exit(args->ret); return 0; }
TRACEPOINT_PROBE(syscalls, sys_exit_sendto)   { syscall_exit(args->ret); return 0; }

// A connection closed before its response must not leak its start time into
// the next connection that reuses the same fd number.
TRACEPOINT_PROBE(syscalls, sys_enter_close)
{
    TARGET_OR_RETURN(id);
    struct conn_key conn = { .tgid = id >> 32, .fd = args->fd };
    requests.delete(&conn);
    return 0;
}
