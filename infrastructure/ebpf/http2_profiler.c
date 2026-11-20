// SPDX-License-Identifier: GPL-2.0
//
// Tessera — XDP HTTP/2 request profiler (issue #392).
//
// Zero-instrumentation, kernel-only HTTP/2 observability for the K8s pod veth
// interface. `xdp_http2` is attached at the driver ingress point; it parses raw
// Ethernet/IPv4/TCP packets, skips the HTTP/2 client preface, walks the frame
// header (length/type/flags/stream id) and decodes the static-table subset of
// HPACK needed for `:method`, `:path`, `:status` and `content-length`.
//
// Two maps are exported:
//
//   * `http2_frames` — BPF_MAP_TYPE_RINGBUF; one record per HEADERS/DATA/
//     RST_STREAM frame, consumed by the loader for debugging/replay.
//   * `http2_events` — BPF_PERF_OUTPUT (a perf_event_array); completed request
//     tuples (timestamp_ns, pid, path_hash, status, latency_ns) consumed by
//     `http2_profiler.py` and exported as Prometheus histograms.
//
// A kprobe on tcp_connect learns socket-cookie -> pid; XDP has no current
// process context, so the cookie is the only stable flow identity available at
// the driver hook. Events whose socket predates the profiler carry pid 0 and
// are still attributed by path_hash/status.
//
// Compile-time parameters supplied by the loader:
//   TARGET_PORT      container port serving HTTP/2 (default 8080)
//   MAX_HPACK_SCAN   max header-fragment bytes scanned per frame (default 256)
//   MAX_FRAMES       max frames parsed per packet (default 8)

#include <linux/bpf.h>
#include <linux/if_ether.h>
#include <linux/in.h>
#include <linux/ip.h>
#include <linux/sched.h>
#include <linux/tcp.h>
#include <uapi/linux/ptrace.h>

#ifndef TARGET_PORT
#define TARGET_PORT 8080
#endif
#ifndef MAX_HPACK_SCAN
#define MAX_HPACK_SCAN 256
#endif
#ifndef MAX_FRAMES
#define MAX_FRAMES 8
#endif

#define HTTP2_FRAME_DATA      0x0
#define HTTP2_FRAME_HEADERS   0x1
#define HTTP2_FRAME_RST_STREAM 0x3
#define HTTP2_FLAG_END_HEADERS 0x4
#define HTTP2_FLAG_PADDED      0x08
#define HTTP2_FLAG_PRIORITY    0x20
#define HTTP2_MAX_FRAGMENT     512

// (socket cookie, stream id, direction) -> request start time in ns.
struct stream_key {
    u64 cookie;
    u32 stream_id;
    u32 direction;  // 0 = client HEADERS, 1 = server HEADERS
};

struct frame_event {
    u64 timestamp_ns;
    u64 cookie;
    u32 stream_id;
    u32 length;
    u32 path_hash;
    u16 status;
    u8 type;
    u8 flags;
};

struct http2_event {
    u64 timestamp_ns;
    u64 latency_ns;
    u32 pid;
    u32 path_hash;
    u32 method;    // HPACK method code: 1 GET, 2 POST, 0 unknown
    u16 status;    // 0 for RST_STREAM
    u8 kind;       // 0 request/response completion, 1 RST_STREAM
    u8 reserved;
};

struct hpack_result {
    u32 path_hash;
    u32 content_length;
    u16 status;
    u8 method;
    u8 has_path;
    u8 has_status;
    u8 has_method;
    u8 has_length;
};

BPF_TABLE("lru_hash", struct stream_key, u64, streams, 65536);
BPF_TABLE("hash", u64, u32, socket_pid, 65536);
BPF_RINGBUF_OUTPUT(http2_frames, 1 << 20);
BPF_PERF_OUTPUT(http2_events);

static __always_inline u32 fnv1a(const u8 *data, u32 len, u32 seed)
{
    u32 hash = seed;
    for (u32 i = 0; i < len; i++) {
        hash ^= data[i];
        hash *= 16777619u;
    }
    return hash;
}

// Decode an HPACK integer with the given prefix width. `buf` points at the
// bytes after the first one; at most 5 continuation bytes are consumed so the
// loop is bounded for the verifier.
static __always_inline int hpack_int(u8 first, u8 prefix_bits, u8 *buf,
                                     u32 len, u32 *pos, u32 *out)
{
    u32 mask = (1u << prefix_bits) - 1;
    u32 value = first & mask;
    if (value < mask) {
        *out = value;
        return 0;
    }
    u32 shift = 0;
#pragma unroll
    for (u32 i = 0; i < 5; i++) {
        if (*pos >= len || shift > 28)
            return -1;
        u8 byte = buf[*pos];
        *pos += 1;
        value += (u32)(byte & 0x7f) << shift;
        if (!(byte & 0x80)) {
            *out = value;
            return 0;
        }
        shift += 7;
    }
    return -1;
}

static __always_inline int status_from_static_index(u8 index, u16 *status)
{
    switch (index) {
    case 8:  *status = 200; return 0;
    case 9:  *status = 204; return 0;
    case 10: *status = 206; return 0;
    case 11: *status = 304; return 0;
    case 12: *status = 400; return 0;
    case 13: *status = 404; return 0;
    case 14: *status = 500; return 0;
    default: return -1;
    }
}

// Decode the static-table subset of an HPACK fragment. Literal names with an
// indexed name are supported (index 2 :method, 4 :path, 8 :status,
// 28 content-length); fully literal names are skipped, which keeps the verifier
// budget small while still covering real h2load/hyper traffic.
static __always_inline void hpack_scan(struct xdp_md *ctx, u32 offset, u32 len,
                                       struct hpack_result *res)
{
    u8 value[64] = {};
    u32 i = 0;

    while (i < len && i < MAX_HPACK_SCAN) {
        u8 first = 0;
        if (bpf_skb_load_bytes(ctx, offset + i, &first, 1) != 0)
            return;
        i++;

        u8 prefix = 0;
        u8 width = 0;
        bool literal = false;
        if (first & 0x80) {
            prefix = first & 0x7f;
            width = 7;
        } else if (first & 0x40) {
            prefix = first & 0x3f;
            width = 6;
            literal = true;
        } else if (first & 0x20) {
            // Update table size — no header fields follow this integer.
            u32 ignored = 0;
            if (hpack_int(first & 0x1f, 5, (u8 *)ctx, 0, &i, &ignored) != 0)
                return;
            continue;
        } else {
            prefix = first & 0x0f;
            width = 4;
            literal = true;
        }

        u32 index = 0;
        // `buf` is only read by hpack_int when the prefix is saturated, so
        // pass a pointer to the packet via a small scratch buffer instead.
        u8 tail[8] = {};
#pragma unroll
        for (u32 k = 0; k < 8; k++) {
            if (i + k < len && k < sizeof(tail)) {
                bpf_skb_load_bytes(ctx, offset + i + k, &tail[k], 1);
            }
        }
        u32 tail_pos = 0;
        if (hpack_int(prefix, width, tail, sizeof(tail), &tail_pos, &index) != 0)
            return;
        // Consume only the continuation bytes hpack_int actually used.
        i += tail_pos;

        if (!literal) {
            if (index == 2) res->method = 1;
            else if (index == 3) res->method = 2;
            else {
                u16 status = 0;
                if (status_from_static_index((u8)index, &status) == 0) {
                    res->status = status;
                    res->has_status = 1;
                }
            }
            continue;
        }

        // Literal value: HPACK string prefix (7 bits + continuation).
        u8 len_byte = 0;
        if (bpf_skb_load_bytes(ctx, offset + i, &len_byte, 1) != 0)
            return;
        i++;
        u32 value_len = 0;
        u8 scratch[8] = {};
#pragma unroll
        for (u32 k = 0; k < 8; k++) {
            if (i + k < len && k < sizeof(scratch)) {
                bpf_skb_load_bytes(ctx, offset + i + k, &scratch[k], 1);
            }
        }
        u32 scratch_pos = 0;
        if (hpack_int(len_byte & 0x7f, 7, scratch, sizeof(scratch),
                      &scratch_pos, &value_len) != 0)
            return;
        i += scratch_pos;
        if (value_len > sizeof(value))
            value_len = sizeof(value);
        if (value_len > 0)
            bpf_skb_load_bytes(ctx, offset + i, value, value_len);
        i += value_len;

        if (index == 2) {
            res->method = (value_len >= 3 && value[0] == 'G' && value[1] == 'E' && value[2] == 'T') ? 1 : 2;
            res->has_method = 1;
        } else if (index == 4) {
            res->path_hash = fnv1a(value, value_len, 2166136261u);
            res->has_path = 1;
        } else if (index == 8 || index == 28) {
            u32 number = 0;
            for (u32 k = 0; k < value_len && k < 5; k++) {
                if (value[k] < '0' || value[k] > '9')
                    break;
                number = number * 10 + (value[k] - '0');
            }
            if (index == 8) {
                res->status = (u16)number;
                res->has_status = 1;
            } else {
                res->content_length = number;
                res->has_length = 1;
            }
        }
    }
}

static __always_inline void emit_completion(struct xdp_md *ctx, u64 cookie,
                                            u32 stream_id, u16 status,
                                            u8 kind, u32 path_hash, u32 method,
                                            u64 start, u64 now)
{
    struct http2_event ev = {};
    ev.timestamp_ns = now;
    ev.latency_ns = start ? (now - start) : 0;
    ev.path_hash = path_hash;
    ev.method = method;
    ev.status = status;
    ev.kind = kind;
    u32 *pid = socket_pid.lookup(&cookie);
    ev.pid = pid ? *pid : 0;
    http2_events.perf_submit(ctx, &ev, sizeof(ev));
}

int xdp_http2(struct xdp_md *ctx)
{
    u64 now = bpf_ktime_get_ns();
    u64 cookie = bpf_get_socket_cookie(ctx);

    struct ethhdr eth = {};
    if (bpf_skb_load_bytes(ctx, 0, &eth, sizeof(eth)) != 0)
        return XDP_PASS;
    if (bpf_ntohs(eth.h_proto) != ETH_P_IP)
        return XDP_PASS;

    struct iphdr ip = {};
    if (bpf_skb_load_bytes(ctx, ETH_HLEN, &ip, sizeof(ip)) != 0)
        return XDP_PASS;
    if (ip.protocol != IPPROTO_TCP || ip.ihl < 5)
        return XDP_PASS;

    u32 ip_len = ((u32)ip.ihl) * 4;
    u32 tcp_off = ETH_HLEN + ip_len;
    struct tcphdr tcp = {};
    if (bpf_skb_load_bytes(ctx, tcp_off, &tcp, sizeof(tcp)) != 0)
        return XDP_PASS;
    if (tcp.doff < 5)
        return XDP_PASS;

    u32 payload_off = tcp_off + ((u32)tcp.doff) * 4;
    u32 total_len = bpf_ntohs(ip.tot_len);
    if (total_len < ip_len + ((u32)tcp.doff) * 4)
        return XDP_PASS;
    u32 payload_len = total_len - ip_len - ((u32)tcp.doff) * 4;
    if (payload_len < 9 || payload_off + payload_len > (u32)ctx->data_end)
        return XDP_PASS;

    // The HTTP/2 connection preface is the first 24 bytes of the client's
    // first packet. Later packets start directly with a frame header.
    u8 head[24] = {};
    if (bpf_skb_load_bytes(ctx, payload_off, head, sizeof(head)) == 0 &&
        head[0] == 'P' && head[1] == 'R' && head[2] == 'I' && head[3] == ' ') {
        payload_off += 24;
        payload_len -= 24;
    }

    u32 off = payload_off;
    u32 end = payload_off + payload_len;

#pragma unroll
    for (u32 f = 0; f < MAX_FRAMES; f++) {
        if (off + 9 > end)
            break;

        u8 hdr[9] = {};
        if (bpf_skb_load_bytes(ctx, off, hdr, sizeof(hdr)) != 0)
            break;
        u32 frame_len = ((u32)hdr[0] << 16) | ((u32)hdr[1] << 8) | hdr[2];
        u8 type = hdr[3];
        u8 flags = hdr[4];
        u32 stream_id = (((u32)hdr[5] & 0x7f) << 24) | ((u32)hdr[6] << 16) |
                        ((u32)hdr[7] << 8) | hdr[8];
        off += 9;
        if (frame_len == 0 || frame_len > HTTP2_MAX_FRAGMENT || off + frame_len > end)
            break;

        struct frame_event fe = {};
        fe.timestamp_ns = now;
        fe.cookie = cookie;
        fe.stream_id = stream_id;
        fe.length = frame_len;
        fe.type = type;
        fe.flags = flags;
        http2_frames.ringbuf_output(&fe, sizeof(fe), 0);

        if (type == HTTP2_FRAME_HEADERS) {
            u32 frag_off = off;
            u32 frag_len = frame_len;
            if (!(flags & HTTP2_FLAG_END_HEADERS) && frag_len >= 5) {
                frag_off += 5;  // priority fields
                frag_len -= 5;
            }
            if (flags & HTTP2_FLAG_PADDED && frag_len >= 1) {
                frag_off += 1;  // pad length
                frag_len -= 1;
            }
            struct hpack_result res = {};
            hpack_scan(ctx, frag_off, frag_len, &res);

            bool from_client = (bpf_ntohs(tcp.dest) == TARGET_PORT);
            struct stream_key key = { .cookie = cookie, .stream_id = stream_id,
                                      .direction = from_client ? 0 : 1 };
            if (from_client && res.has_method && res.has_path) {
                fe.path_hash = res.path_hash;
                fe.status = 0;
                streams.insert(&key, &now);
            } else if (!from_client && res.has_status) {
                fe.status = res.status;
                fe.path_hash = res.path_hash;
                u64 *start = streams.lookup(&key);
                emit_completion(ctx, cookie, stream_id, res.status, 0,
                                res.path_hash, res.method, start ? *start : 0, now);
                streams.delete(&key);
            }
        } else if (type == HTTP2_FRAME_RST_STREAM) {
            struct stream_key key = { .cookie = cookie, .stream_id = stream_id,
                                      .direction = 0 };
            u64 *start = streams.lookup(&key);
            emit_completion(ctx, cookie, stream_id, 0, 1, fe.path_hash, 0,
                            start ? *start : 0, now);
            streams.delete(&key);
        }

        off += frame_len;
    }
    return XDP_PASS;
}

// XDP cannot see a pid; learn it from the connect path. BCC auto-attaches
// functions prefixed `kprobe__`.
int kprobe__tcp_connect(struct pt_regs *ctx, struct sock *sk)
{
    u64 cookie = bpf_get_socket_cookie(sk);
    if (!cookie)
        return 0;
    u32 pid = bpf_get_current_pid_tgid() >> 32;
    socket_pid.update(&cookie, &pid);
    return 0;
}
