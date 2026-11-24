//! `tessera` binary alias for the one-command sandbox UX.
//!
//! Both `tessera` and `tessera-cli` run the same CLI implementation; this
//! thin shim avoids the duplicate-target warning of pointing two `[[bin]]`
//! entries at `src/main.rs`.

fn main() {
    if let Err(error) = tessera_cli::run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}
