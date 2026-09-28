//! User-space daemon for managing eBPF XDP rate-limiting filters
//!
//! This daemon loads the eBPF program, attaches it to network interfaces,
//! and provides a CLI for dynamically updating block lists and rate limits.

use std::net::Ipv4Addr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use aya::{
    maps::{HashMap, Array},
    programs::Xdp,
    Bpf,
};
use clap::{Parser, Subcommand};
use nix::ifaddrs::getifaddrs;
use tokio::signal;
use tracing::{error, info, warn};

#[derive(Parser, Debug)]
#[command(name = "tessera-ebpf-daemon", version, about = "eBPF XDP rate-limiting daemon")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Load and attach the XDP program to an interface
    Attach {
        /// Network interface name (e.g., eth0)
        #[arg(short, long)]
        interface: String,
        /// Maximum packets per second per IP:port
        #[arg(long, default_value = "1000")]
        max_pps: u64,
    },
    /// Detach the XDP program from an interface
    Detach {
        /// Network interface name
        #[arg(short, long)]
        interface: String,
    },
    /// Block an IP address
    BlockIp {
        /// IP address to block
        #[arg(short, long)]
        ip: String,
    },
    /// Unblock an IP address
    UnblockIp {
        /// IP address to unblock
        #[arg(short, long)]
        ip: String,
    },
    /// List currently blocked IPs
    ListBlocked,
    /// Update rate limit configuration
    SetRateLimit {
        /// Maximum packets per second per IP:port
        #[arg(short, long)]
        max_pps: u64,
    },
    /// Show current statistics
    Stats {
        /// Network interface name
        #[arg(short, long)]
        interface: String,
    },
    /// Run as a daemon with gRPC API for dynamic updates
    Daemon {
        /// Network interface name
        #[arg(short, long)]
        interface: String,
        /// Maximum packets per second per IP:port
        #[arg(long, default_value = "1000")]
        max_pps: u64,
        /// gRPC listen address
        #[arg(long, default_value = "127.0.0.1:50051")]
        grpc_addr: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    match args.command {
        Command::Attach { interface, max_pps } => {
            attach_xdp(&interface, max_pps).await?;
        }
        Command::Detach { interface } => {
            detach_xdp(&interface).await?;
        }
        Command::BlockIp { ip } => {
            block_ip(&ip).await?;
        }
        Command::UnblockIp { ip } => {
            unblock_ip(&ip).await?;
        }
        Command::ListBlocked => {
            list_blocked().await?;
        }
        Command::SetRateLimit { max_pps } => {
            set_rate_limit(max_pps).await?;
        }
        Command::Stats { interface } => {
            show_stats(&interface).await?;
        }
        Command::Daemon {
            interface,
            max_pps,
            grpc_addr,
        } => {
            run_daemon(&interface, max_pps, &grpc_addr).await?;
        }
    }

    Ok(())
}

async fn attach_xdp(interface: &str, max_pps: u64) -> anyhow::Result<()> {
    let mut bpf = Bpf::load(aya::include_bytes_aligned!(concat!(
        env!("OUT_DIR"),
        "/xdp_drop"
    )))?;
    
    let program: &mut Xdp = bpf.program_mut("xdp_rate_limit").unwrap().try_into()?;
    program.load()?;
    program.attach(interface, aya::programs::XdpFlags::default())
        .map_err(|e| anyhow::anyhow!("Failed to attach XDP program: {}", e))?;

    let mut config: Array<_, u64> = Array::new(bpf.map_mut("config_map").unwrap());
    config.set(0, max_pps, 0)?;

    info!("XDP program attached to {} with max_pps={}", interface, max_pps);

    bpf.detach(interface)?;
    Ok(())
}

async fn detach_xdp(interface: &str) -> anyhow::Result<()> {
    let mut bpf = Bpf::load(aya::include_bytes_aligned!(concat!(
        env!("OUT_DIR"),
        "/xdp_drop"
    )))?;
    
    bpf.detach(interface)?;
    info!("XDP program detached from {}", interface);
    Ok(())
}

async fn block_ip(ip_str: &str) -> anyhow::Result<()> {
    let ip = Ipv4Addr::from_str(ip_str)?;
    let mut bpf = Bpf::load(aya::include_bytes_aligned!(concat!(
        env!("OUT_DIR"),
        "/xdp_drop"
    )))?;
    
    let mut blocked_ips: HashMap<_, u32, u8> = HashMap::new(bpf.map_mut("blocked_ips").unwrap());
    blocked_ips.insert(u32::from(ip), 1, 0)?;
    
    info!("Blocked IP: {}", ip);
    Ok(())
}

async fn unblock_ip(ip_str: &str) -> anyhow::Result<()> {
    let ip = Ipv4Addr::from_str(ip_str)?;
    let mut bpf = Bpf::load(aya::include_bytes_aligned!(concat!(
        env!("OUT_DIR"),
        "/xdp_drop"
    )))?;
    
    let mut blocked_ips: HashMap<_, u32, u8> = HashMap::new(bpf.map_mut("blocked_ips").unwrap());
    blocked_ips.remove(&u32::from(ip))?;
    
    info!("Unblocked IP: {}", ip);
    Ok(())
}

async fn list_blocked() -> anyhow::Result<()> {
    let mut bpf = Bpf::load(aya::include_bytes_aligned!(concat!(
        env!("OUT_DIR"),
        "/xdp_drop"
    )))?;
    
    let blocked_ips: HashMap<_, u32, u8> = HashMap::new(bpf.map("blocked_ips").unwrap());
    
    println!("Blocked IPs:");
    for entry in blocked_ips.iter() {
        let (ip, _) = entry?;
        println!("  {}", Ipv4Addr::from(ip));
    }
    Ok(())
}

async fn set_rate_limit(max_pps: u64) -> anyhow::Result<()> {
    let mut bpf = Bpf::load(aya::include_bytes_aligned!(concat!(
        env!("OUT_DIR"),
        "/xdp_drop"
    )))?;
    
    let mut config: Array<_, u64> = Array::new(bpf.map_mut("config_map").unwrap());
    config.set(0, max_pps, 0)?;
    
    info!("Rate limit updated to {} pps", max_pps);
    Ok(())
}

async fn show_stats(interface: &str) -> anyhow::Result<()> {
    let mut bpf = Bpf::load(aya::include_bytes_aligned!(concat!(
        env!("OUT_DIR"),
        "/xdp_drop"
    )))?;
    
    let rate_limit_map: HashMap<_, [u8; 12], [u8; 24]> = HashMap::new(bpf.map("rate_limit_map").unwrap());
    
    println!("Rate limit statistics for interface {}:", interface);
    println!("{:<15} {:<6} {:<8} {:<12} {:<10}", "IP", "Port", "Proto", "Packets", "Blocked");
    println!("{}", "-".repeat(60));
    
    for entry in rate_limit_map.iter() {
        let (key, val) = entry?;
        let ip = u32::from_be_bytes([key[0], key[1], key[2], key[3]]);
        let port = u16::from_be_bytes([key[4], key[5]]);
        let protocol = key[6];
        let packet_count = u64::from_be_bytes([
            val[0], val[1], val[2], val[3], val[4], val[5], val[6], val[7]
        ]);
        let blocked = val[23] != 0;
        
        println!("{:<15} {:<6} {:<8} {:<12} {:<10}", 
            Ipv4Addr::from(ip), port, protocol, packet_count, if blocked { "yes" } else { "no" });
    }
    Ok(())
}

async fn run_daemon(interface: &str, max_pps: u64, grpc_addr: &str) -> anyhow::Result<()> {
    info!("Starting eBPF daemon on interface {} with gRPC on {}", interface, grpc_addr);
    
    let mut bpf = Bpf::load(aya::include_bytes_aligned!(concat!(
        env!("OUT_DIR"),
        "/xdp_drop"
    )))?;
    
    let program: &mut Xdp = bpf.program_mut("xdp_rate_limit").unwrap().try_into()?;
    program.load()?;
    program.attach(interface, aya::programs::XdpFlags::default())
        .map_err(|e| anyhow::anyhow!("Failed to attach XDP program: {}", e))?;

    let mut config: Array<_, u64> = Array::new(bpf.map_mut("config_map").unwrap());
    config.set(0, max_pps, 0)?;

    info!("Daemon running. Press Ctrl+C to stop.");
    
    signal::ctrl_c().await?;
    
    info!("Shutting down...");
    bpf.detach(interface)?;
    Ok(())
}