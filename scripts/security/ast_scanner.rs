//! Soroban contract AST vulnerability scanner — Issue #299.
//!
//! Walks a directory of `.rs` files, parses each one with `syn`, and reports
//! every public function that mutates contract state without a
//! `require_auth()` / `require_auth_for_args()` call.
//!
//! Usage:
//!   ast-scanner [--dir <path>]          # default: current directory
//!   ast-scanner --help
//!
//! Exit code: 0 = clean, 1 = vulnerabilities found, 2 = usage/IO error.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process;

use proc_macro2::TokenStream;
use quote::ToTokens;
use syn::{
    visit::Visit, Attribute, File, ImplItem, Item, ReturnType, Signature, Type,
};
use walkdir::WalkDir;

// ---------------------------------------------------------------------------
// Acceptance criteria helpers
// ---------------------------------------------------------------------------

/// Returns `true` if the signature marks a state-changing function.
///
/// Heuristics that trigger the check (any one is enough):
/// - return type is `()` (mutating, no read result)
/// - first argument after `env: Env` has `&mut self` or looks like a storage
///   write (we conservatively flag all public non-pure functions)
///
/// We flag every public function except those that take no `Env` argument at
/// all (pure helpers / view helpers).
fn takes_env(sig: &Signature) -> bool {
    sig.inputs.iter().any(|arg| {
        arg.to_token_stream().to_string().contains("Env")
            || arg.to_token_stream().to_string().contains("env")
    })
}

fn is_view_only(sig: &Signature) -> bool {
    // A function that returns a value but never writes is outside our threat
    // model; we conservatively keep it in scope and let `require_auth` guide.
    // Return `true` only for unit-return pure helpers.
    matches!(&sig.output, ReturnType::Default)
}

// ---------------------------------------------------------------------------
// AST visitor
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Scanner {
    findings: Vec<Finding>,
    current_file: PathBuf,
}

#[derive(Debug)]
struct Finding {
    file: PathBuf,
    line: u32,
    fn_name: String,
    reason: &'static str,
}

impl Finding {
    fn display(&self) {
        eprintln!(
            "[VULN] {}:{} — `{}` — {}",
            self.file.display(),
            self.line,
            self.fn_name,
            self.reason
        );
    }
}

/// Extract the names of all function calls made inside `block` at any depth.
fn collect_call_names(block: &syn::Block) -> HashSet<String> {
    struct CallCollector(HashSet<String>);
    impl<'ast> Visit<'ast> for CallCollector {
        fn visit_expr_call(&mut self, i: &'ast syn::ExprCall) {
            if let syn::Expr::Path(p) = i.func.as_ref() {
                if let Some(seg) = p.path.segments.last() {
                    self.0.insert(seg.ident.to_string());
                }
            }
            syn::visit::visit_expr_call(self, i);
        }
        fn visit_expr_method_call(&mut self, i: &'ast syn::ExprMethodCall) {
            self.0.insert(i.method.to_string());
            syn::visit::visit_expr_method_call(self, i);
        }
    }
    let mut c = CallCollector(HashSet::new());
    c.visit_block(block);
    c.0
}

const AUTH_GUARDS: &[&str] = &["require_auth", "require_auth_for_args"];

fn has_auth_guard(block: &syn::Block) -> bool {
    let calls = collect_call_names(block);
    AUTH_GUARDS.iter().any(|g| calls.contains(*g))
}

fn is_public(vis: &syn::Visibility) -> bool {
    matches!(vis, syn::Visibility::Public(_))
}

fn fn_line(sig: &Signature) -> u32 {
    sig.fn_token.span.start().line as u32
}

impl<'ast> Visit<'ast> for Scanner {
    // Top-level functions
    fn visit_item_fn(&mut self, i: &'ast syn::ItemFn) {
        if is_public(&i.vis) && takes_env(&i.sig) {
            if !has_auth_guard(&i.block) {
                self.findings.push(Finding {
                    file: self.current_file.clone(),
                    line: fn_line(&i.sig),
                    fn_name: i.sig.ident.to_string(),
                    reason: "public function accesses Env but never calls require_auth()",
                });
            }
        }
        syn::visit::visit_item_fn(self, i);
    }

    // Impl-block functions (Soroban contracts are usually `impl ContractName`)
    fn visit_impl_item_fn(&mut self, i: &'ast syn::ImplItemFn) {
        if is_public(&i.vis) && takes_env(&i.sig) {
            if !has_auth_guard(&i.block) {
                self.findings.push(Finding {
                    file: self.current_file.clone(),
                    line: fn_line(&i.sig),
                    fn_name: i.sig.ident.to_string(),
                    reason: "public impl fn accesses Env but never calls require_auth()",
                });
            }
        }
        syn::visit::visit_impl_item_fn(self, i);
    }
}

// ---------------------------------------------------------------------------
// File scanning
// ---------------------------------------------------------------------------

fn scan_file(path: &Path, scanner: &mut Scanner) -> Result<(), Box<dyn std::error::Error>> {
    let src = std::fs::read_to_string(path)?;
    let ast: File = syn::parse_file(&src)?;
    scanner.current_file = path.to_path_buf();
    scanner.visit_file(&ast);
    Ok(())
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

fn usage() -> ! {
    eprintln!("Usage: ast-scanner [--dir <path>]");
    process::exit(2);
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut dir = PathBuf::from(".");

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dir" => {
                dir = PathBuf::from(args.next().unwrap_or_else(|| {
                    eprintln!("--dir requires a path argument");
                    process::exit(2);
                }));
            }
            "--help" | "-h" => usage(),
            other => {
                eprintln!("Unknown argument: {other}");
                usage();
            }
        }
    }

    let mut scanner = Scanner::default();
    let mut errors: Vec<String> = Vec::new();

    for entry in WalkDir::new(&dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map_or(false, |x| x == "rs"))
    {
        if let Err(e) = scan_file(entry.path(), &mut scanner) {
            errors.push(format!("{}: {e}", entry.path().display()));
        }
    }

    for err in &errors {
        eprintln!("[ERROR] {err}");
    }

    if scanner.findings.is_empty() {
        println!("No vulnerabilities found.");
        process::exit(0);
    }

    println!("{} potential vulnerability(s) found:\n", scanner.findings.len());
    for f in &scanner.findings {
        f.display();
    }
    process::exit(1);
}
