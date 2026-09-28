// SPDX-License-Identifier: GPL-2.0 OR BSD-3-Clause
/*
 * Tessera Distributed Rate-Limiting Edge Defense
 * eBPF/XDP program for high-speed packet filtering at the network driver level
 */

#include <linux/bpf.h>
#include <linux/if_ether.h>
#include <linux/ip.h>
#include <linux/ipv6.h>
#include <linux/tcp.h>
#include <linux/udp.h>
#include <linux/in.h>
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_endian.h>

#define MAX_ENTRIES 100000
#define RATE_LIMIT_WINDOW_SEC 1
#define MAX_PACKETS_PER_WINDOW 1000

struct rate_limit_key {
    __u32 ip;
    __u16 port;
    __u8 protocol;
    __u8 pad[3];
};

struct rate_limit_value {
    __u64 packet_count;
    __u64 window_start;
    __u8 blocked;
    __u8 pad[7];
};

struct {
    __uint(type, BPF_MAP_TYPE_LRU_HASH);
    __uint(max_entries, MAX_ENTRIES);
    __type(key, struct rate_limit_key);
    __type(value, struct rate_limit_value);
} rate_limit_map SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 10000);
    __type(key, __u32);
    __type(value, __u8);
} blocked_ips SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, 1);
    __type(key, __u32);
    __type(value, __u64);
} config_map SEC(".maps");

SEC("xdp")
int xdp_rate_limit(struct xdp_md *ctx) {
    void *data_end = (void *)(long)ctx->data_end;
    void *data = (void *)(long)ctx->data;
    struct ethhdr *eth = data;
    __u64 now = bpf_ktime_get_ns() / 1000000000;

    if ((void *)(eth + 1) > data_end)
        return XDP_PASS;

    __u32 src_ip = 0;
    __u16 src_port = 0;
    __u8 protocol = 0;

    if (eth->h_proto == bpf_htons(ETH_P_IP)) {
        struct iphdr *ip = (void *)(eth + 1);
        if ((void *)(ip + 1) > data_end)
            return XDP_PASS;

        src_ip = ip->saddr;
        protocol = ip->protocol;

        if (ip->protocol == IPPROTO_TCP) {
            struct tcphdr *tcp = (void *)ip + (ip->ihl * 4);
            if ((void *)(tcp + 1) > data_end)
                return XDP_PASS;
            src_port = bpf_ntohs(tcp->source);
        } else if (ip->protocol == IPPROTO_UDP) {
            struct udphdr *udp = (void *)ip + (ip->ihl * 4);
            if ((void *)(udp + 1) > data_end)
                return XDP_PASS;
            src_port = bpf_ntohs(udp->source);
        }
    } else if (eth->h_proto == bpf_htons(ETH_P_IPV6)) {
        struct ipv6hdr *ip6 = (void *)(eth + 1);
        if ((void *)(ip6 + 1) > data_end)
            return XDP_PASS;

        src_ip = ip6->saddr.s6_addr32[3];
        protocol = ip6->nexthdr;

        if (ip6->nexthdr == IPPROTO_TCP) {
            struct tcphdr *tcp = (void *)ip6 + sizeof(*ip6);
            if ((void *)(tcp + 1) > data_end)
                return XDP_PASS;
            src_port = bpf_ntohs(tcp->source);
        } else if (ip6->nexthdr == IPPROTO_UDP) {
            struct udphdr *udp = (void *)ip6 + sizeof(*ip6);
            if ((void *)(udp + 1) > data_end)
                return XDP_PASS;
            src_port = bpf_ntohs(udp->source);
        }
    } else {
        return XDP_PASS;
    }

    __u32 *config = bpf_map_lookup_elem(&config_map, &(__u32){0});
    __u64 max_packets = config ? *config : MAX_PACKETS_PER_WINDOW;

    struct rate_limit_key key = {
        .ip = src_ip,
        .port = src_port,
        .protocol = protocol,
    };

    __u8 *blocked = bpf_map_lookup_elem(&blocked_ips, &src_ip);
    if (blocked && *blocked) {
        return XDP_DROP;
    }

    struct rate_limit_value *val = bpf_map_lookup_elem(&rate_limit_map, &key);
    if (val) {
        if (now - val->window_start >= RATE_LIMIT_WINDOW_SEC) {
            val->window_start = now;
            val->packet_count = 1;
        } else {
            val->packet_count++;
            if (val->packet_count > max_packets) {
                val->blocked = 1;
                __u8 one = 1;
                bpf_map_update_elem(&blocked_ips, &src_ip, &one, BPF_ANY);
                return XDP_DROP;
            }
        }
    } else {
        struct rate_limit_value new_val = {
            .packet_count = 1,
            .window_start = now,
            .blocked = 0,
        };
        bpf_map_update_elem(&rate_limit_map, &key, &new_val, BPF_ANY);
    }

    return XDP_PASS;
}

char _license[] SEC("license") = "GPL";