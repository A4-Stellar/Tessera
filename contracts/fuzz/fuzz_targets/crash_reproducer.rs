//! Crash reproducer target for fuzz artifacts (Issue #370).
//!
//! Takes a corpus file path from CLI arguments, configures RUST_BACKTRACE=full,
//! and runs the byte payload through the XDR parser/fuzz target to reproduce panics.

use std::env;
use std::fs;
use std::process;

#[path = "../../../api/src/indexer/xdr_parser.rs"]
mod xdr_parser;

fn main() {
    env::set_var("RUST_BACKTRACE", "full");

    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <path-to-corpus-file>", args[0]);
        process::exit(1);
    }

    let file_path = &args[1];
    println!("[crash_reproducer] Loading crash artifact from: {}", file_path);

    let bytes = match fs::read(file_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Failed to read corpus file {}: {}", file_path, e);
            process::exit(1);
        }
    };

    println!("[crash_reproducer] Executing parser over {} bytes...", bytes.len());

    // Run production parser
    match xdr_parser::ContractEventView::from_bytes(&bytes) {
        Ok(view) => {
            println!("[crash_reproducer] Successfully parsed without crash: {:?}", view);
        }
        Err(e) => {
            println!("[crash_reproducer] Handled parser error: {:?}", e);
        }
    }
}
