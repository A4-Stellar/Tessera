//! Automated Semantic Security Scanner for Soroban Smart Contracts
//! Issue #49 / #430:
//! (1) Call graph extraction by resolving cross-contract invoke_contract calls.
//! (2) Taint analysis: tracking env.invoker() and env.current_contract_address() reaching storage without sanitisation.
//! (3) Reentrancy detection: flag paths where invoke_contract occurs between storage.get() and storage.set() on the same key.
//! (4) Storage key collision detection: flag storage.set() calls sharing key type but differing in runtime-computed parts.
//! (5) SARIF v2.1.0 export for GitHub Code Scanning.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use quote::ToTokens;
use serde::{Deserialize, Serialize};
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{Block, Expr, ExprCall, ExprMethodCall, File, ImplItemFn, ItemFn, Pat, Stmt};
use walkdir::WalkDir;

// ---------------------------------------------------------------------------
// Findings and Diagnostic Data Structures
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum VulnerabilityKind {
    ReentrancyStorageRace,
    UnsanitizedTaintStorageSink,
    StorageKeyCollision,
    UnresolvedCrossContractCall,
}

impl VulnerabilityKind {
    pub fn rule_id(&self) -> &'static str {
        match self {
            Self::ReentrancyStorageRace => "SOROBAN-001-REENTRANCY",
            Self::UnsanitizedTaintStorageSink => "SOROBAN-002-TAINT-STORAGE",
            Self::StorageKeyCollision => "SOROBAN-003-KEY-COLLISION",
            Self::UnresolvedCrossContractCall => "SOROBAN-004-CROSS-CONTRACT-CALL",
        }
    }

    pub fn title(&self) -> &'static str {
        match self {
            Self::ReentrancyStorageRace => "Reentrancy: External contract call between storage read and write on same key",
            Self::UnsanitizedTaintStorageSink => "Taint: Unsanitized value from env.invoker() or env.current_contract_address() reaches storage sink",
            Self::StorageKeyCollision => "Storage Collision: Multiple storage writes share key type with differing runtime-computed arguments",
            Self::UnresolvedCrossContractCall => "Call Graph: Cross-contract invoke_contract target resolution",
        }
    }

    pub fn level(&self) -> &'static str {
        match self {
            Self::ReentrancyStorageRace => "error",
            Self::UnsanitizedTaintStorageSink => "error",
            Self::StorageKeyCollision => "warning",
            Self::UnresolvedCrossContractCall => "note",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SecurityFinding {
    pub kind: VulnerabilityKind,
    pub file_path: PathBuf,
    pub fn_name: String,
    pub line: usize,
    pub col: usize,
    pub description: String,
    pub snippet: String,
    pub related_line: Option<usize>,
    pub related_snippet: Option<String>,
}

impl SecurityFinding {
    pub fn emit_clippy_diagnostic(&self) {
        eprintln!(
            "{}: [{}] {}\n  --> {}:{}:{}\n   |\n{:4} |     {}\n   |     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^",
            self.kind.level(),
            self.kind.rule_id(),
            self.description,
            self.file_path.display(),
            self.line,
            self.col,
            self.line,
            self.snippet.trim()
        );
        if let (Some(rel_line), Some(rel_snippet)) = (self.related_line, &self.related_snippet) {
            eprintln!(
                "   = note: related occurrence at line {}: `{}`",
                rel_line,
                rel_snippet.trim()
            );
        }
        eprintln!();
    }
}

// ---------------------------------------------------------------------------
// 1. Call Graph Data Model & Resolution
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CallGraphEdge {
    pub caller_contract: String,
    pub caller_fn: String,
    pub callee_contract_expr: String,
    pub callee_fn_symbol: String,
    pub line: usize,
    pub col: usize,
}

#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct CallGraph {
    pub edges: Vec<CallGraphEdge>,
}

impl CallGraph {
    pub fn add_edge(&mut self, edge: CallGraphEdge) {
        self.edges.push(edge);
    }
}

// ---------------------------------------------------------------------------
// AST Pattern Extractors
// ---------------------------------------------------------------------------

/// Detects if an expression represents a cross-contract invocation:
/// - `env.invoke_contract(&target, &func, args)`
/// - `Client::new(&env, &target).func(args)`
pub fn extract_cross_contract_call(expr: &Expr) -> Option<(String, String, usize, usize)> {
    let span = expr.span();
    let line = span.start().line;
    let col = span.start().column;

    match expr {
        Expr::MethodCall(ExprMethodCall {
            receiver,
            method,
            args,
            ..
        }) => {
            let method_name = method.to_string();
            let receiver_text = receiver.to_token_stream().to_string();

            if method_name == "invoke_contract" || receiver_text.contains("invoke_contract") {
                let target_expr = args
                    .first()
                    .map(|a| a.to_token_stream().to_string())
                    .unwrap_or_else(|| "unknown_contract".to_string());
                let symbol_expr = args
                    .get(1)
                    .map(|a| {
                        let text = a.to_token_stream().to_string();
                        if let Some(first_quote) = text.find('"') {
                            if let Some(second_quote) = text[first_quote + 1..].find('"') {
                                return text[first_quote + 1..first_quote + 1 + second_quote].to_string();
                            }
                        }
                        text
                    })
                    .unwrap_or_else(|| "unknown_fn".to_string());
                return Some((target_expr, symbol_expr, line, col));
            }

            // Client method invocations: `token_client.transfer(...)`
            if (receiver_text.contains("client") || receiver_text.contains("Client"))
                && (method_name == "transfer"
                    || method_name == "transfer_from"
                    || method_name == "burn"
                    || method_name == "mint"
                    || method_name == "on_flash_loan"
                    || method_name == "is_allowed"
                    || method_name == "check")
            {
                return Some((receiver_text, method_name, line, col));
            }
            None
        }
        Expr::Call(ExprCall { func, args, .. }) => {
            let func_text = func.to_token_stream().to_string();
            if func_text.contains("invoke_contract") {
                let target_expr = args
                    .first()
                    .map(|a| a.to_token_stream().to_string())
                    .unwrap_or_else(|| "unknown_contract".to_string());
                let symbol_expr = args
                    .get(1)
                    .map(|a| {
                        let text = a.to_token_stream().to_string();
                        if let Some(first_quote) = text.find('"') {
                            if let Some(second_quote) = text[first_quote + 1..].find('"') {
                                return text[first_quote + 1..first_quote + 1 + second_quote].to_string();
                            }
                        }
                        text
                    })
                    .unwrap_or_else(|| "unknown_fn".to_string());
                return Some((target_expr, symbol_expr, line, col));
            }
            None
        }
        _ => None,
    }
}

/// Detects storage operations: `(is_write, key_text, line, col, full_expr_text)`
pub fn extract_storage_op(expr: &Expr) -> Option<(bool, String, usize, usize, String)> {
    if let Expr::MethodCall(ExprMethodCall {
        receiver,
        method,
        args,
        ..
    }) = expr
    {
        let method_name = method.to_string();
        let receiver_text = receiver.to_token_stream().to_string();

        let is_storage = receiver_text.contains("storage")
            || receiver_text.contains("persistent")
            || receiver_text.contains("instance")
            || receiver_text.contains("temporary");

        if !is_storage {
            return None;
        }

        let span = expr.span();
        let line = span.start().line;
        let col = span.start().column;
        let full_text = expr.to_token_stream().to_string();

        if method_name == "set" || method_name == "update" || method_name == "remove" {
            let key_text = args
                .first()
                .map(|a| a.to_token_stream().to_string())
                .unwrap_or_default();
            return Some((true, key_text, line, col, full_text));
        } else if method_name == "get" || method_name == "has" {
            let key_text = args
                .first()
                .map(|a| a.to_token_stream().to_string())
                .unwrap_or_default();
            return Some((false, key_text, line, col, full_text));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// 2. Semantic Analyzer Visitor
// ---------------------------------------------------------------------------

pub struct SorobanSemanticAnalyzer {
    pub current_file: PathBuf,
    pub findings: Vec<SecurityFinding>,
    pub call_graph: CallGraph,
}

impl SorobanSemanticAnalyzer {
    pub fn new(current_file: PathBuf) -> Self {
        Self {
            current_file,
            findings: Vec::new(),
            call_graph: CallGraph::default(),
        }
    }

    pub fn analyze_function_block(&mut self, block: &Block, fn_name: &str) {
        // Data flow tracking for Taint Analysis
        let mut tainted_variables: HashSet<String> = HashSet::new();
        let mut sanitized_variables: HashSet<String> = HashSet::new();

        // Sequence tracking for Reentrancy Detection: (key_text, line, expr_text)
        let mut storage_reads: HashMap<String, (usize, String)> = HashMap::new();
        let mut pending_external_calls: Vec<(String, String, usize, usize, String)> = Vec::new();

        // Storage write tracking for Key Collision Detection: Vec<(key_type_base, raw_key_text, line, full_text)>
        let mut storage_writes: Vec<(String, String, usize, String)> = Vec::new();

        for stmt in &block.stmts {
            self.analyze_statement(
                stmt,
                fn_name,
                &mut tainted_variables,
                &mut sanitized_variables,
                &mut storage_reads,
                &mut pending_external_calls,
                &mut storage_writes,
            );
        }

        // Post-block Storage Key Collision Detection (Issue #49 requirement 4)
        self.detect_storage_key_collisions(&storage_writes, fn_name);
    }

    fn analyze_statement(
        &mut self,
        stmt: &Stmt,
        fn_name: &str,
        tainted_vars: &mut HashSet<String>,
        sanitized_vars: &mut HashSet<String>,
        storage_reads: &mut HashMap<String, (usize, String)>,
        external_calls: &mut Vec<(String, String, usize, usize, String)>,
        storage_writes: &mut Vec<(String, String, usize, String)>,
    ) {
        match stmt {
            Stmt::Local(local) => {
                // Track assignments: let <pat> = <init>;
                let var_name = match &local.pat {
                    Pat::Ident(pat_ident) => pat_ident.ident.to_string(),
                    _ => local.pat.to_token_stream().to_string(),
                };

                if let Some(init) = &local.init {
                    let init_text = init.expr.to_token_stream().to_string();

                    // Check Taint Sources: env.invoker() or env.current_contract_address()
                    if init_text.contains("invoker")
                        || init_text.contains("current_contract_address")
                    {
                        tainted_vars.insert(var_name.clone());
                    } else {
                        // Propagate taint if RHS mentions any already tainted variable
                        for tainted in tainted_vars.iter() {
                            if init_text.contains(tainted) {
                                tainted_vars.insert(var_name.clone());
                                break;
                            }
                        }
                    }

                    // Recursively inspect init expression
                    self.analyze_expression(
                        &init.expr,
                        fn_name,
                        tainted_vars,
                        sanitized_vars,
                        storage_reads,
                        external_calls,
                        storage_writes,
                    );
                }
            }
            Stmt::Expr(expr, _) => {
                self.analyze_expression(
                    expr,
                    fn_name,
                    tainted_vars,
                    sanitized_vars,
                    storage_reads,
                    external_calls,
                    storage_writes,
                );
            }
            _ => {}
        }
    }

    fn analyze_expression(
        &mut self,
        expr: &Expr,
        fn_name: &str,
        tainted_vars: &mut HashSet<String>,
        sanitized_vars: &mut HashSet<String>,
        storage_reads: &mut HashMap<String, (usize, String)>,
        external_calls: &mut Vec<(String, String, usize, usize, String)>,
        storage_writes: &mut Vec<(String, String, usize, String)>,
    ) {
        let expr_text = expr.to_token_stream().to_string();

        // 1. Detect Sanitizers: e.g. `x.require_auth()` or `require_auth_for_args`
        if expr_text.contains("require_auth") {
            for tainted in tainted_vars.iter() {
                if expr_text.contains(tainted) {
                    sanitized_vars.insert(tainted.clone());
                }
            }
        }

        // 2. Cross-contract call detection (Issue #49 requirement 1 & 3)
        if let Some((target, callee_fn, line, col)) = extract_cross_contract_call(expr) {
            self.call_graph.add_edge(CallGraphEdge {
                caller_contract: self.current_file.file_stem().map_or("contract".into(), |s| s.to_string_lossy().into()),
                caller_fn: fn_name.to_string(),
                callee_contract_expr: target.clone(),
                callee_fn_symbol: callee_fn.clone(),
                line,
                col,
            });

            external_calls.push((target, callee_fn, line, col, expr_text.clone()));
        }

        // 3. Storage operation detection
        if let Some((is_write, key_text, line, col, full_op_text)) = extract_storage_op(expr) {
            let normalized_key = key_text.replace('&', "").trim().to_string();

            if is_write {
                storage_writes.push((
                    self.extract_base_key_type(&normalized_key),
                    normalized_key.clone(),
                    line,
                    full_op_text.clone(),
                ));

                // 3a. Reentrancy check (Issue #49 requirement 3):
                // Flag any path where invoke_contract is called between storage.get() and storage.set() on the same key!
                if let Some((read_line, read_expr)) = storage_reads.get(&normalized_key) {
                    if let Some((_, _, call_line, _, call_text)) = external_calls
                        .iter()
                        .find(|(_, _, call_line, _, _)| *call_line > *read_line && line > *call_line)
                    {
                        self.findings.push(SecurityFinding {
                            kind: VulnerabilityKind::ReentrancyStorageRace,
                            file_path: self.current_file.clone(),
                            fn_name: fn_name.to_string(),
                            line,
                            col,
                            description: format!(
                                "Reentrancy: storage.set() on key `{}` executed after external call `{}` which followed storage.get() at line {}",
                                normalized_key, call_text, read_line
                            ),
                            snippet: full_op_text.clone(),
                            related_line: Some(*read_line),
                            related_snippet: Some(read_expr.clone()),
                        });
                    }
                }

                // 3b. Taint Analysis check (Issue #49 requirement 2):
                // Flag if any value derived from env.invoker() or env.current_contract_address() reaches storage().set() without sanitization!
                for tainted in tainted_vars.iter() {
                    if full_op_text.contains(tainted) && !sanitized_vars.contains(tainted) {
                        self.findings.push(SecurityFinding {
                            kind: VulnerabilityKind::UnsanitizedTaintStorageSink,
                            file_path: self.current_file.clone(),
                            fn_name: fn_name.to_string(),
                            line,
                            col,
                            description: format!(
                                "Taint: unsanitized variable `{}` derived from env.invoker()/address reaches storage write without `require_auth()`",
                                tainted
                            ),
                            snippet: full_op_text.clone(),
                            related_line: None,
                            related_snippet: None,
                        });
                    }
                }
            } else {
                // Storage read: record key read location
                storage_reads.insert(normalized_key, (line, full_op_text));
            }
        }

        // Sub-block recursion
        match expr {
            Expr::If(expr_if) => {
                self.analyze_function_block(&expr_if.then_branch, fn_name);
                if let Some((_, else_branch)) = &expr_if.else_branch {
                    self.analyze_expression(
                        else_branch,
                        fn_name,
                        tainted_vars,
                        sanitized_vars,
                        storage_reads,
                        external_calls,
                        storage_writes,
                    );
                }
            }
            Expr::Block(expr_block) => {
                self.analyze_function_block(&expr_block.block, fn_name);
            }
            Expr::Loop(expr_loop) => {
                self.analyze_function_block(&expr_loop.body, fn_name);
            }
            Expr::ForLoop(expr_for) => {
                self.analyze_function_block(&expr_for.body, fn_name);
            }
            Expr::While(expr_while) => {
                self.analyze_function_block(&expr_while.body, fn_name);
            }
            Expr::Match(expr_match) => {
                for arm in &expr_match.arms {
                    self.analyze_expression(
                        &arm.body,
                        fn_name,
                        tainted_vars,
                        sanitized_vars,
                        storage_reads,
                        external_calls,
                        storage_writes,
                    );
                }
            }
            _ => {}
        }
    }

    /// Extract key type constructor: e.g. `DataKey::Balance(user)` -> `DataKey::Balance`
    fn extract_base_key_type(&self, key_text: &str) -> String {
        if let Some(pos) = key_text.find('(') {
            key_text[..pos].trim().to_string()
        } else {
            key_text.trim().to_string()
        }
    }

    /// Detect storage key collisions (Issue #49 requirement 4):
    /// Flag if two storage.set() calls share the same key type but differ only in a runtime-computed part.
    fn detect_storage_key_collisions(
        &mut self,
        writes: &[(String, String, usize, String)],
        fn_name: &str,
    ) {
        for i in 0..writes.len() {
            for j in (i + 1)..writes.len() {
                let (base_i, raw_i, line_i, text_i) = &writes[i];
                let (base_j, raw_j, line_j, text_j) = &writes[j];

                // If they share base key type but differ in expression arguments
                if !base_i.is_empty() && base_i == base_j && raw_i != raw_j {
                    self.findings.push(SecurityFinding {
                        kind: VulnerabilityKind::StorageKeyCollision,
                        file_path: self.current_file.clone(),
                        fn_name: fn_name.to_string(),
                        line: *line_j,
                        col: 1,
                        description: format!(
                            "Storage Collision: key `{}` and `{}` share base key constructor `{}` with differing runtime arguments",
                            raw_j, raw_i, base_i
                        ),
                        snippet: text_j.clone(),
                        related_line: Some(*line_i),
                        related_snippet: Some(text_i.clone()),
                    });
                }
            }
        }
    }
}

impl<'ast> Visit<'ast> for SorobanSemanticAnalyzer {
    fn visit_item_fn(&mut self, i: &'ast ItemFn) {
        let fn_name = i.sig.ident.to_string();
        self.analyze_function_block(&i.block, &fn_name);
        syn::visit::visit_item_fn(self, i);
    }

    fn visit_impl_item_fn(&mut self, i: &'ast ImplItemFn) {
        let fn_name = i.sig.ident.to_string();
        self.analyze_function_block(&i.block, &fn_name);
        syn::visit::visit_impl_item_fn(self, i);
    }
}

// ---------------------------------------------------------------------------
// 5. SARIF v2.1.0 JSON Serializer (for GitHub Code Scanning)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct SarifLog<'a> {
    #[serde(rename = "$schema")]
    pub schema: &'static str,
    pub version: &'static str,
    pub runs: Vec<SarifRun<'a>>,
}

#[derive(Serialize)]
pub struct SarifRun<'a> {
    pub tool: SarifTool<'a>,
    pub results: Vec<SarifResult>,
}

#[derive(Serialize)]
pub struct SarifTool<'a> {
    pub driver: SarifDriver<'a>,
}

#[derive(Serialize)]
pub struct SarifDriver<'a> {
    pub name: &'static str,
    pub version: &'static str,
    pub information_uri: &'static str,
    pub rules: Vec<SarifRule<'a>>,
}

#[derive(Serialize)]
pub struct SarifRule<'a> {
    pub id: &'a str,
    pub name: &'a str,
    #[serde(rename = "shortDescription")]
    pub short_description: SarifMessage<'a>,
    #[serde(rename = "defaultConfiguration")]
    pub default_configuration: SarifConfig<'a>,
}

#[derive(Serialize)]
pub struct SarifConfig<'a> {
    pub level: &'a str,
}

#[derive(Serialize)]
pub struct SarifResult {
    #[serde(rename = "ruleId")]
    pub rule_id: String,
    pub level: String,
    pub message: SarifMessageOwned,
    pub locations: Vec<SarifLocation>,
}

#[derive(Serialize)]
pub struct SarifMessage<'a> {
    pub text: &'a str,
}

#[derive(Serialize)]
pub struct SarifMessageOwned {
    pub text: String,
}

#[derive(Serialize)]
pub struct SarifLocation {
    #[serde(rename = "physicalLocation")]
    pub physical_location: SarifPhysicalLocation,
}

#[derive(Serialize)]
pub struct SarifPhysicalLocation {
    #[serde(rename = "artifactLocation")]
    pub artifact_location: SarifArtifactLocation,
    pub region: SarifRegion,
}

#[derive(Serialize)]
pub struct SarifArtifactLocation {
    pub uri: String,
}

#[derive(Serialize)]
pub struct SarifRegion {
    #[serde(rename = "startLine")]
    pub start_line: usize,
    #[serde(rename = "startColumn")]
    pub start_column: usize,
}

pub fn generate_sarif_report(findings: &[SecurityFinding]) -> String {
    let rules = vec![
        SarifRule {
            id: "SOROBAN-001-REENTRANCY",
            name: "ReentrancyStorageRace",
            short_description: SarifMessage {
                text: "External contract call between storage read and write on same key",
            },
            default_configuration: SarifConfig { level: "error" },
        },
        SarifRule {
            id: "SOROBAN-002-TAINT-STORAGE",
            name: "UnsanitizedTaintStorageSink",
            short_description: SarifMessage {
                text: "Unsanitized invoker or contract address reaching storage write sink",
            },
            default_configuration: SarifConfig { level: "error" },
        },
        SarifRule {
            id: "SOROBAN-003-KEY-COLLISION",
            name: "StorageKeyCollision",
            short_description: SarifMessage {
                text: "Storage writes sharing key type but differing in runtime-computed expressions",
            },
            default_configuration: SarifConfig { level: "warning" },
        },
        SarifRule {
            id: "SOROBAN-004-CROSS-CONTRACT-CALL",
            name: "UnresolvedCrossContractCall",
            short_description: SarifMessage {
                text: "Cross-contract call invocation",
            },
            default_configuration: SarifConfig { level: "note" },
        },
    ];

    let results = findings
        .iter()
        .map(|f| SarifResult {
            rule_id: f.kind.rule_id().to_string(),
            level: f.kind.level().to_string(),
            message: SarifMessageOwned {
                text: f.description.clone(),
            },
            locations: vec![SarifLocation {
                physical_location: SarifPhysicalLocation {
                    artifact_location: SarifArtifactLocation {
                        uri: f.file_path.to_string_lossy().replace('\\', "/"),
                    },
                    region: SarifRegion {
                        start_line: f.line,
                        start_column: f.col,
                    },
                },
            }],
        })
        .collect();

    let log = SarifLog {
        schema: "https://raw.githubusercontent.com/oasis-tcs/sarif-spec/master/Schemata/sarif-schema-2.1.0.json",
        version: "2.1.0",
        runs: vec![SarifRun {
            tool: SarifTool {
                driver: SarifDriver {
                    name: "tessera-soroban-scanner",
                    version: "0.2.0",
                    information_uri: "https://github.com/A4-Stellar/Tessera",
                    rules,
                },
            },
            results,
        }],
    };

    serde_json::to_string_pretty(&log).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// File & Directory Scanning
// ---------------------------------------------------------------------------

pub fn scan_contract_file(path: &Path) -> Result<(Vec<SecurityFinding>, CallGraph), String> {
    let content = fs::read_to_string(path).map_err(|e| format!("IO error {}: {e}", path.display()))?;
    let ast: File = syn::parse_file(&content).map_err(|e| format!("Parse error {}: {e}", path.display()))?;

    let mut analyzer = SorobanSemanticAnalyzer::new(path.to_path_buf());
    analyzer.visit_file(&ast);
    Ok((analyzer.findings, analyzer.call_graph))
}

pub fn scan_contracts_directory(dir: &Path) -> (Vec<SecurityFinding>, CallGraph, Vec<String>) {
    let mut all_findings = Vec::new();
    let mut aggregate_call_graph = CallGraph::default();
    let mut errors = Vec::new();

    for entry in WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "rs"))
    {
        match scan_contract_file(entry.path()) {
            Ok((findings, call_graph)) => {
                all_findings.extend(findings);
                aggregate_call_graph.edges.extend(call_graph.edges);
            }
            Err(err) => errors.push(err),
        }
    }

    (all_findings, aggregate_call_graph, errors)
}

// ---------------------------------------------------------------------------
// CLI Entry Point
// ---------------------------------------------------------------------------

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut target_dir = PathBuf::from("contracts");
    let mut strict_mode = false;
    let mut sarif_format = false;
    let mut json_format = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dir" if i + 1 < args.len() => {
                target_dir = PathBuf::from(&args[i + 1]);
                i += 1;
            }
            "--strict" => strict_mode = true,
            "--sarif" => sarif_format = true,
            "--json" => json_format = true,
            "--help" | "-h" => {
                println!("Soroban Smart Contract Semantic Security Scanner (Issue #49)");
                println!("Usage: analyze-soroban-ast [--dir <path>] [--sarif] [--json] [--strict]");
                process::exit(0);
            }
            _ => {}
        }
        i += 1;
    }

    if !sarif_format && !json_format {
        eprintln!(
            "[INFO] Scanning Soroban contracts in `{}` (Taint, Reentrancy, Collisions, Call Graph)...",
            target_dir.display()
        );
    }

    let (findings, call_graph, errors) = scan_contracts_directory(&target_dir);

    for err in &errors {
        eprintln!("[ERROR] {err}");
    }

    if sarif_format {
        println!("{}", generate_sarif_report(&findings));
    } else if json_format {
        let report = serde_json::json!({
            "scanned_directory": target_dir.to_string_lossy(),
            "findings_count": findings.len(),
            "call_graph_edges_count": call_graph.edges.len(),
            "findings": findings,
            "call_graph": call_graph,
        });
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
    } else {
        for finding in &findings {
            finding.emit_clippy_diagnostic();
        }
        eprintln!(
            "[INFO] Analysis completed. Call graph edges resolved: {}. Findings: {}.",
            call_graph.edges.len(),
            findings.len()
        );
    }

    if strict_mode && !findings.is_empty() {
        process::exit(1);
    } else {
        process::exit(0);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_taint_analysis_flags_unsanitized_invoker_reaching_storage() {
        let code = r#"
            impl Token {
                pub fn update_user_balance(env: Env, amount: i128) {
                    let sender = env.invoker();
                    // sender reaches storage without require_auth()
                    env.storage().persistent().set(&DataKey::Balance(sender), &amount);
                }
            }
        "#;
        let ast: File = syn::parse_str(code).unwrap();
        let mut analyzer = SorobanSemanticAnalyzer::new(PathBuf::from("test_taint.rs"));
        analyzer.visit_file(&ast);

        let taint_findings: Vec<_> = analyzer
            .findings
            .iter()
            .filter(|f| matches!(f.kind, VulnerabilityKind::UnsanitizedTaintStorageSink))
            .collect();
        assert_eq!(taint_findings.len(), 1);
        assert!(taint_findings[0].description.contains("sender"));
    }

    #[test]
    fn test_taint_analysis_passes_when_sanitized_with_require_auth() {
        let code = r#"
            impl Token {
                pub fn update_user_balance(env: Env, amount: i128) {
                    let sender = env.invoker();
                    sender.require_auth();
                    // sanitized via require_auth()
                    env.storage().persistent().set(&DataKey::Balance(sender), &amount);
                }
            }
        "#;
        let ast: File = syn::parse_str(code).unwrap();
        let mut analyzer = SorobanSemanticAnalyzer::new(PathBuf::from("test_taint_safe.rs"));
        analyzer.visit_file(&ast);

        let taint_findings: Vec<_> = analyzer
            .findings
            .iter()
            .filter(|f| matches!(f.kind, VulnerabilityKind::UnsanitizedTaintStorageSink))
            .collect();
        assert_eq!(taint_findings.len(), 0);
    }

    #[test]
    fn test_reentrancy_detection_same_storage_key() {
        let code = r#"
            impl Vault {
                pub fn withdraw(env: Env, token: Address, user: Address, amount: i128) {
                    let old_bal = env.storage().persistent().get(&DataKey::Balance(user)).unwrap_or(0);
                    // External call between get and set on DataKey::Balance(user)
                    env.invoke_contract(&token, &symbol_short!("transfer"), (user, amount));
                    // Reentrant mutation!
                    env.storage().persistent().set(&DataKey::Balance(user), &(old_bal - amount));
                }
            }
        "#;
        let ast: File = syn::parse_str(code).unwrap();
        let mut analyzer = SorobanSemanticAnalyzer::new(PathBuf::from("test_reentrancy.rs"));
        analyzer.visit_file(&ast);

        let reentrancy_findings: Vec<_> = analyzer
            .findings
            .iter()
            .filter(|f| matches!(f.kind, VulnerabilityKind::ReentrancyStorageRace))
            .collect();
        assert_eq!(reentrancy_findings.len(), 1);
        assert!(reentrancy_findings[0].description.contains("DataKey::Balance(user)"));
    }

    #[test]
    fn test_storage_key_collision_detection() {
        let code = r#"
            impl Registry {
                pub fn update(env: Env, user: Address, other: Address) {
                    env.storage().persistent().set(&DataKey::Record(user), &100);
                    // Sharing base constructor DataKey::Record but differing runtime arg
                    env.storage().persistent().set(&DataKey::Record(other), &200);
                }
            }
        "#;
        let ast: File = syn::parse_str(code).unwrap();
        let mut analyzer = SorobanSemanticAnalyzer::new(PathBuf::from("test_collision.rs"));
        analyzer.visit_file(&ast);

        let collision_findings: Vec<_> = analyzer
            .findings
            .iter()
            .filter(|f| matches!(f.kind, VulnerabilityKind::StorageKeyCollision))
            .collect();
        assert_eq!(collision_findings.len(), 1);
        assert!(collision_findings[0].description.contains("DataKey::Record"));
    }

    #[test]
    fn test_sarif_v210_generation() {
        let findings = vec![SecurityFinding {
            kind: VulnerabilityKind::ReentrancyStorageRace,
            file_path: PathBuf::from("contracts/vault/src/lib.rs"),
            fn_name: "withdraw".to_string(),
            line: 42,
            col: 9,
            description: "Test reentrancy finding".to_string(),
            snippet: "env.storage().set(...)".to_string(),
            related_line: Some(38),
            related_snippet: Some("env.storage().get(...)".to_string()),
        }];

        let sarif_json = generate_sarif_report(&findings);
        assert!(sarif_json.contains("SOROBAN-001-REENTRANCY"));
        assert!(sarif_json.contains("2.1.0"));
        assert!(sarif_json.contains("contracts/vault/src/lib.rs"));
    }
}
