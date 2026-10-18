//! Automated Smart Contract Reentrancy & Storage Mutation Static Analyzer (Issue #246)
//!
//! Parses Soroban contract source files into Rust ASTs using the `syn` crate and
//! detects Checks-Effects-Interactions (CEI) reentrancy vulnerabilities:
//! flags state mutations (persistent/instance storage writes) occurring after external
//! cross-contract calls.
//!
//! Emits compiler/clippy-style warning diagnostics with exact file, line, and column numbers.
//!
//! Complexity:
//! - Time: O(N) where N is total AST nodes across scanned files. Single-pass visitor.
//! - Space: O(D) where D is maximum AST tree depth (stack) + O(F) diagnostic findings.

use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use quote::ToTokens;
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{Block, Expr, ExprCall, ExprMethodCall, File, ImplItemFn, ItemFn, Stmt};
use walkdir::WalkDir;

/// Record of an external cross-contract invocation.
#[derive(Clone, Debug)]
pub struct ExternalCallInfo {
    pub line: usize,
    pub col: usize,
    pub expr_text: String,
}

/// Diagnostic finding representing a reentrancy / CEI violation.
#[derive(Clone, Debug)]
pub struct ReentrancyFinding {
    pub file_path: PathBuf,
    pub fn_name: String,
    pub storage_write_line: usize,
    pub storage_write_col: usize,
    pub storage_write_text: String,
    pub external_call_line: usize,
    pub external_call_col: usize,
    pub external_call_text: String,
}

impl ReentrancyFinding {
    /// Formats finding matching `rustc` / `cargo clippy` diagnostic output.
    pub fn emit_clippy_diagnostic(&self) {
        eprintln!(
            "warning: [REENTRANCY] storage mutation after external cross-contract call\n  --> {}:{}:{}\n   |\n{:4} |     {}\n   |     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ state mutation occurs here\n   |\n   = note: external contract call occurred earlier at line {}:{}: `{}`\n   = help: apply Checks-Effects-Interactions (CEI) pattern: update contract storage before invoking external contracts\n",
            self.file_path.display(),
            self.storage_write_line,
            self.storage_write_col,
            self.storage_write_line,
            self.storage_write_text.trim(),
            self.external_call_line,
            self.external_call_col,
            self.external_call_text.trim()
        );
    }
}

// ---------------------------------------------------------------------------
// AST Pattern Matchers
// ---------------------------------------------------------------------------

/// Detects if an expression represents an external cross-contract invocation in Soroban:
/// - `env.invoke_contract(...)`
/// - Contract client calls: `client.method(...)`, `token::Client::new(...).transfer(...)`
/// - Known cross-contract method names: `.transfer(...)`, `.transfer_from(...)`, `.burn(...)`
pub fn is_external_cross_contract_call(expr: &Expr) -> Option<String> {
    match expr {
        Expr::MethodCall(ExprMethodCall {
            receiver,
            method,
            ..
        }) => {
            let method_name = method.to_string();
            let receiver_text = receiver.to_token_stream().to_string();

            // 1. env.invoke_contract(...)
            if receiver_text.contains("invoke_contract") || method_name == "invoke_contract" {
                return Some(expr.to_token_stream().to_string());
            }

            // 2. Client method invocations: client.transfer, token_client.transfer, etc.
            let is_client_receiver = receiver_text.contains("client")
                || receiver_text.contains("Client")
                || receiver_text.contains("token");
            let is_cross_contract_method = method_name == "transfer"
                || method_name == "transfer_from"
                || method_name == "burn"
                || method_name == "mint"
                || method_name == "on_flash_loan"
                || method_name == "check";

            if is_client_receiver && is_cross_contract_method {
                return Some(expr.to_token_stream().to_string());
            }

            // 3. Chain calls: token::Client::new(...).transfer(...)
            let is_transfer = method_name == "transfer" || method_name == "transfer_from";
            let is_contract_target = receiver_text.contains("Client") || receiver_text.contains("env");
            if is_transfer && is_contract_target {
                return Some(expr.to_token_stream().to_string());
            }

            None
        }
        Expr::Call(ExprCall { func, .. }) => {
            let func_text = func.to_token_stream().to_string();
            if func_text.contains("invoke_contract") {
                return Some(expr.to_token_stream().to_string());
            }
            None
        }
        _ => None,
    }
}

/// Detects if an expression represents a persistent, instance, or temporary storage write:
/// - `env.storage().persistent().set(...)`
/// - `env.storage().instance().set(...)`
/// - `env.storage().temporary().set(...)`
/// - `env.storage().*.remove(...)`
/// - `env.storage().*.update(...)`
pub fn is_storage_write(expr: &Expr) -> Option<String> {
    if let Expr::MethodCall(ExprMethodCall {
        receiver,
        method,
        ..
    }) = expr
    {
        let method_name = method.to_string();
        let receiver_text = receiver.to_token_stream().to_string();

        if (method_name == "set" || method_name == "remove" || method_name == "update")
            && (receiver_text.contains("storage")
                || receiver_text.contains("persistent")
                || receiver_text.contains("instance")
                || receiver_text.contains("temporary"))
        {
            return Some(expr.to_token_stream().to_string());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Block Analyzer & Visitor
// ---------------------------------------------------------------------------

pub struct SorobanReentrancyAnalyzer {
    pub current_file: PathBuf,
    pub findings: Vec<ReentrancyFinding>,
}

impl SorobanReentrancyAnalyzer {
    pub fn new(current_file: PathBuf) -> Self {
        Self {
            current_file,
            findings: Vec::new(),
        }
    }

    /// Analyzes a sequential block of statements, tracking control-flow state mutations after external calls.
    pub fn analyze_block(&mut self, block: &Block, fn_name: &str) {
        let mut external_call: Option<ExternalCallInfo> = None;

        for stmt in &block.stmts {
            self.analyze_stmt(stmt, fn_name, &mut external_call);
        }
    }

    fn analyze_stmt(
        &mut self,
        stmt: &Stmt,
        fn_name: &str,
        external_call: &mut Option<ExternalCallInfo>,
    ) {
        match stmt {
            Stmt::Expr(expr, _) => {
                self.analyze_expr(expr, fn_name, external_call);
            }
            Stmt::Local(local) => {
                if let Some(init) = &local.init {
                    self.analyze_expr(&init.expr, fn_name, external_call);
                }
            }
            _ => {}
        }
    }

    fn analyze_expr(
        &mut self,
        expr: &Expr,
        fn_name: &str,
        external_call: &mut Option<ExternalCallInfo>,
    ) {
        // Check if current expression is a storage write
        if let Some(write_text) = is_storage_write(expr) {
            if let Some(call_info) = external_call {
                let span = expr.span();
                self.findings.push(ReentrancyFinding {
                    file_path: self.current_file.clone(),
                    fn_name: fn_name.to_string(),
                    storage_write_line: span.start().line,
                    storage_write_col: span.start().column,
                    storage_write_text: write_text,
                    external_call_line: call_info.line,
                    external_call_col: call_info.col,
                    external_call_text: call_info.expr_text.clone(),
                });
            }
        }

        // Check if current expression is an external cross-contract call
        if let Some(call_text) = is_external_cross_contract_call(expr) {
            let span = expr.span();
            *external_call = Some(ExternalCallInfo {
                line: span.start().line,
                col: span.start().column,
                expr_text: call_text,
            });
        }

        // Recurse into sub-blocks / control structures
        match expr {
            Expr::If(expr_if) => {
                self.analyze_block(&expr_if.then_branch, fn_name);
                if let Some((_, else_branch)) = &expr_if.else_branch {
                    self.analyze_expr(else_branch, fn_name, external_call);
                }
            }
            Expr::Block(expr_block) => {
                self.analyze_block(&expr_block.block, fn_name);
            }
            Expr::Loop(expr_loop) => {
                self.analyze_block(&expr_loop.body, fn_name);
            }
            Expr::ForLoop(expr_for) => {
                self.analyze_block(&expr_for.body, fn_name);
            }
            Expr::While(expr_while) => {
                self.analyze_block(&expr_while.body, fn_name);
            }
            Expr::Match(expr_match) => {
                for arm in &expr_match.arms {
                    self.analyze_expr(&arm.body, fn_name, external_call);
                }
            }
            _ => {}
        }
    }
}

impl<'ast> Visit<'ast> for SorobanReentrancyAnalyzer {
    fn visit_item_fn(&mut self, i: &'ast ItemFn) {
        let fn_name = i.sig.ident.to_string();
        self.analyze_block(&i.block, &fn_name);
        syn::visit::visit_item_fn(self, i);
    }

    fn visit_impl_item_fn(&mut self, i: &'ast ImplItemFn) {
        let fn_name = i.sig.ident.to_string();
        self.analyze_block(&i.block, &fn_name);
        syn::visit::visit_impl_item_fn(self, i);
    }
}

// ---------------------------------------------------------------------------
// File & Directory Scanner
// ---------------------------------------------------------------------------

pub fn scan_file(path: &Path) -> Result<Vec<ReentrancyFinding>, String> {
    let content = fs::read_to_string(path).map_err(|e| format!("IO error {}: {e}", path.display()))?;
    let ast: File = syn::parse_file(&content).map_err(|e| format!("Parse error {}: {e}", path.display()))?;

    let mut analyzer = SorobanReentrancyAnalyzer::new(path.to_path_buf());
    analyzer.visit_file(&ast);
    Ok(analyzer.findings)
}

pub fn scan_directory(dir: &Path) -> (Vec<ReentrancyFinding>, Vec<String>) {
    let mut all_findings = Vec::new();
    let mut errors = Vec::new();

    for entry in WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "rs"))
    {
        match scan_file(entry.path()) {
            Ok(findings) => all_findings.extend(findings),
            Err(err) => errors.push(err),
        }
    }

    (all_findings, errors)
}

// ---------------------------------------------------------------------------
// CLI Entry Point
// ---------------------------------------------------------------------------

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut target_dir = PathBuf::from("contracts");
    let mut strict_mode = false;
    let mut json_format = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dir" if i + 1 < args.len() => {
                target_dir = PathBuf::from(&args[i + 1]);
                i += 1;
            }
            "--strict" => strict_mode = true,
            "--json" => json_format = true,
            "--help" | "-h" => {
                println!("Soroban AST Smart Contract Reentrancy Analyzer");
                println!("Usage: analyze-soroban-ast [--dir <path>] [--strict] [--json]");
                process::exit(0);
            }
            _ => {}
        }
        i += 1;
    }

    eprintln!("[INFO] Analyzing Soroban contracts in `{}` for CEI/reentrancy patterns...", target_dir.display());

    let (findings, errors) = scan_directory(&target_dir);

    for err in &errors {
        eprintln!("[ERROR] {err}");
    }

    if json_format {
        // Emit JSON report
        let report = serde_json::json!({
            "scanned_directory": target_dir.to_string_lossy(),
            "findings_count": findings.len(),
            "findings": findings.iter().map(|f| {
                serde_json::json!({
                    "file": f.file_path.to_string_lossy(),
                    "function": f.fn_name,
                    "storage_write_line": f.storage_write_line,
                    "storage_write_col": f.storage_write_col,
                    "external_call_line": f.external_call_line,
                    "external_call_col": f.external_call_col,
                })
            }).collect::<Vec<_>>()
        });
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
    } else {
        // Emit Clippy-style compiler warnings
        for finding in &findings {
            finding.emit_clippy_diagnostic();
        }
    }

    if findings.is_empty() {
        eprintln!("[PASSED] No CEI reentrancy vulnerabilities detected in `{}`.", target_dir.display());
        process::exit(0);
    } else {
        eprintln!(
            "[WARNING] Found {} potential reentrancy pattern(s) in `{}`.",
            findings.len(),
            target_dir.display()
        );
        if strict_mode {
            process::exit(1);
        } else {
            process::exit(0);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vulnerable_reentrancy_pattern_detected() {
        let code = r#"
            impl MyContract {
                pub fn withdraw(env: Env, token: Address, to: Address, amount: i128) {
                    // External call occurs first
                    env.invoke_contract(&token, &symbol_short!("transfer"), (to, amount));
                    // State mutation occurs after external call (VULNERABLE)
                    env.storage().persistent().set(&DataKey::Balance(to), &0i128);
                }
            }
        "#;

        let ast: File = syn::parse_str(code).unwrap();
        let mut analyzer = SorobanReentrancyAnalyzer::new(PathBuf::from("test_contract.rs"));
        analyzer.visit_file(&ast);

        assert_eq!(analyzer.findings.len(), 1);
        let finding = &analyzer.findings[0];
        assert_eq!(finding.fn_name, "withdraw");
        assert!(finding.storage_write_line > finding.external_call_line);
    }

    #[test]
    fn test_cei_compliant_safe_pattern_passes() {
        let code = r#"
            impl MyContract {
                pub fn withdraw(env: Env, token: Address, to: Address, amount: i128) {
                    // State mutation occurs BEFORE external call (SAFE CEI pattern)
                    env.storage().persistent().set(&DataKey::Balance(to), &0i128);
                    // External call occurs last
                    env.invoke_contract(&token, &symbol_short!("transfer"), (to, amount));
                }
            }
        "#;

        let ast: File = syn::parse_str(code).unwrap();
        let mut analyzer = SorobanReentrancyAnalyzer::new(PathBuf::from("safe_contract.rs"));
        analyzer.visit_file(&ast);

        assert_eq!(analyzer.findings.len(), 0);
    }

    #[test]
    fn test_internal_calls_only_passes() {
        let code = r#"
            impl MyContract {
                pub fn update_settings(env: Env, new_val: u32) {
                    let old_val = env.storage().instance().get(&DataKey::Val).unwrap_or(0);
                    env.storage().instance().set(&DataKey::Val, &(old_val + new_val));
                }
            }
        "#;

        let ast: File = syn::parse_str(code).unwrap();
        let mut analyzer = SorobanReentrancyAnalyzer::new(PathBuf::from("internal_contract.rs"));
        analyzer.visit_file(&ast);

        assert_eq!(analyzer.findings.len(), 0);
    }
}
