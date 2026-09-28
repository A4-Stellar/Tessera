use aya_build::{cargo_metadata, Toolchain};

fn main() {
    let cargo_metadata = cargo_metadata().expect("Failed to get cargo metadata");
    let mut toolchain = Toolchain::default();
    toolchain.features = cargo_metadata.features;
    
    aya_build::build_ebpf([toolchain], "xdp_drop")
        .expect("Failed to build eBPF program");
    
    println!("cargo:rerun-if-changed=xdp_drop.c");
}