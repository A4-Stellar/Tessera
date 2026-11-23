//! Soroban Semantic Contract Security Scanner — Issue #430
//!
//! Extends the `syn` AST security scanner (`scripts/security/`) with full semantic
//! analysis capabilities:
//! 1. **Cross-Contract Call Graph Construction**: Resolves `env.invoke_contract` and
//!    Soroban `<Client>::new(&env, &addr).<method>(...)` invocations across contract files.
//! 2. **Taint Analysis (`SOROBAN-TAINT-001`)**: Tracks values derived from `env.invoker()`
//!    or `env.current_contract_address()` and flags any flow reaching `storage().set()`
//!    without prior sanitisation (`require_auth`, `require_admin`, `assert!`, etc.).
//! 3. **Same-Key Read-Invoke-Write Reentrancy (`SOROBAN-REENTRANCY-001`)**: Flags any
//!    execution path where `invoke_contract` (or an external cross-contract call) is
//!    called between `storage().get(&key)` and `storage().set(&key, ...)` on the same key.
//! 4. **Storage Key Collision Detection (`SOROBAN-COLLISION-001`)**: Flags when two
//!    `storage().set()` calls share the same key type / enum variant but differ only in
//!    a runtime-computed expression.
//! 5. **SARIF v2.1.0 Output (`generate_sarif`)**: Emits GitHub Code Scanning compatible
//!    SARIF v2.1.0 JSON reports.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use quote::ToTokens;
use serde::{Deserialize, Serialize};
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{
    Block, Expr, ExprAssign, ExprBinary, ExprCall, ExprMethodCall, ExprPath, File, ImplItem,
    ImplItemFn, Item, ItemFn, ItemImpl, Local, Pat, Stmt,
};
use walkdir::WalkDir;

// ---------------------------------------------------------------------------
// Rule IDs & Finding Types
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum RuleId {
    /// Tainted value from `env.invoker()` or `env.current_contract_address()`
    /// reaches `storage().set()` without prior sanitisation.
    TaintUnsanitisedStorageWrite,
    /// `invoke_contract` called between `storage().get()` and `storage().set()`
    /// on the same storage key.
    ReadCallWriteReentrancy,
    /// Two `storage().set()` calls share the same key type / variant but differ
    /// only in a runtime-computed part.
    StorageKeyCollision,
}

impl RuleId {
    pub fn as_str(&self) -> &'static str {
        match self {
            RuleId::TaintUnsanitisedStorageWrite => "SOROBAN-TAINT-001",
            RuleId::ReadCallWriteReentrancy => "SOROBAN-REENTRANCY-001",
            RuleId::StorageKeyCollision => "SOROBAN-COLLISION-001",
        }
    }

    pub fn short_description(&self) -> &'static str {
        match self {
            RuleId::TaintUnsanitisedStorageWrite => {
                "Unsanitised tainted value from env.invoker() or env.current_contract_address() written to contract storage"
            }
            RuleId::ReadCallWriteReentrancy => {
                "Same-key read-invoke-write reentrancy race across external contract invocation"
            }
            RuleId::StorageKeyCollision => {
                "Potential storage key collision between two storage().set() calls sharing a key type with runtime-computed parameters"
            }
        }
    }

    pub fn severity_level(&self) -> &'static str {
        match self {
            RuleId::TaintUnsanitisedStorageWrite => "error",
            RuleId::ReadCallWriteReentrancy => "error",
            RuleId::StorageKeyCollision => "warning",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RelatedLocation {
    pub message: String,
    pub file: PathBuf,
    pub line: usize,
    pub col: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SemanticFinding {
    pub rule_id: RuleId,
    pub file: PathBuf,
    pub contract_name: String,
    pub fn_name: String,
    pub line: usize,
    pub col: usize,
    pub message: String,
    pub snippet: String,
    pub related_locations: Vec<RelatedLocation>,
}

// ---------------------------------------------------------------------------
// 1. Cross-Contract Call Graph
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CallNode {
    pub contract: String,
    pub function: String,
    pub file: PathBuf,
    pub line: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CallEdge {
    pub caller_contract: String,
    pub caller_function: String,
    pub target_contract_expr: String,
    pub resolved_target_contract: Option<String>,
    pub target_method: String,
    pub file: PathBuf,
    pub line: usize,
    pub col: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CallGraph {
    pub nodes: Vec<CallNode>,
    pub edges: Vec<CallEdge>,
}

impl CallGraph {
    /// Returns all outgoing cross-contract edges from a given contract and function.
    pub fn outgoing_from(&self, contract: &str, function: &str) -> Vec<&CallEdge> {
        self.edges
            .iter()
            .filter(|e| e.caller_contract == contract && e.caller_function == function)
            .collect()
    }
}

// ---------------------------------------------------------------------------
// AST Expression Helpers
// ---------------------------------------------------------------------------

fn compact_tokens<T: ToTokens>(node: &T) -> String {
    node.to_token_stream()
        .to_string()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Strips outer references (`&`, `&mut`), parentheses, and `.clone()` calls.
fn peel_expr(mut expr: &Expr) -> &Expr {
    loop {
        match expr {
            Expr::Reference(r) => expr = &r.expr,
            Expr::Paren(p) => expr = &p.expr,
            Expr::MethodCall(mc) if mc.method == "clone" && mc.args.is_empty() => {
                expr = &mc.receiver;
            }
            _ => return expr,
        }
    }
}

/// Extracts string literal from `symbol_short!("foo")` or `Symbol::new(&env, "foo")`.
pub fn extract_symbol_literal(expr: &Expr) -> Option<String> {
    let peeled = peel_expr(expr);
    match peeled {
        Expr::Macro(mac) => {
            let path_str = compact_tokens(&mac.mac.path);
            if path_str.contains("symbol_short") {
                let tokens = mac.mac.tokens.to_string();
                let trimmed = tokens.trim().trim_matches('"');
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
            None
        }
        Expr::Call(call) => {
            let func_str = compact_tokens(&call.func);
            if func_str.contains("Symbol :: new") || func_str.ends_with("symbol_short") {
                for arg in &call.args {
                    if let Expr::Lit(lit) = peel_expr(arg) {
                        if let syn::Lit::Str(s) = &lit.lit {
                            return Some(s.value());
                        }
                    }
                }
            }
            None
        }
        Expr::Lit(lit) => {
            if let syn::Lit::Str(s) = &lit.lit {
                return Some(s.value());
            }
            None
        }
        _ => None,
    }
}

/// Detects `env.invoke_contract(&contract_addr, &func_sym, args)` or client cross-contract calls.
/// Returns `(target_contract_expr, target_method_name, full_call_text)`.
pub fn extract_cross_contract_invocation(
    expr: &Expr,
    symbol_aliases: &HashMap<String, String>,
) -> Option<(String, String, String)> {
    match expr {
        Expr::MethodCall(mc) => {
            let method_name = mc.method.to_string();
            let receiver_str = compact_tokens(&mc.receiver);

            // 1. Direct `env.invoke_contract(&target, &fn_sym, args)`
            if method_name == "invoke_contract" {
                let target_expr = mc
                    .args
                    .first()
                    .map(|a| compact_tokens(peel_expr(a)))
                    .unwrap_or_else(|| "unknown".to_string());
                let target_method = mc
                    .args
                    .iter()
                    .nth(1)
                    .and_then(|a| {
                        extract_symbol_literal(a).or_else(|| {
                            let ident = compact_tokens(peel_expr(a));
                            symbol_aliases.get(&ident).cloned().or(Some(ident))
                        })
                    })
                    .unwrap_or_else(|| "unknown".to_string());
                return Some((target_expr, target_method, compact_tokens(expr)));
            }

            // 2. Soroban generated client calls: `token::Client::new(&env, &addr).transfer(...)`
            //    or `client.transfer(...)`
            let is_client_receiver = receiver_str.contains("Client :: new")
                || receiver_str.contains("Client")
                || receiver_str.ends_with("client")
                || receiver_str.contains("_client");
            let is_known_cross_method = matches!(
                method_name.as_str(),
                "transfer"
                    | "transfer_from"
                    | "burn"
                    | "mint"
                    | "balance"
                    | "is_allowed"
                    | "is_allowed_with_hooks"
                    | "check"
                    | "check_velocity"
                    | "on_flash_loan"
                    | "swap_exact_tokens_for_tokens"
                    | "record_transfer"
            );
            if is_client_receiver && method_name != "new" && method_name != "clone" {
                return Some((receiver_str, method_name, compact_tokens(expr)));
            }
            if is_known_cross_method && receiver_str.contains("token") {
                return Some((receiver_str, method_name, compact_tokens(expr)));
            }
            None
        }
        Expr::Call(call) => {
            let func_str = compact_tokens(&call.func);
            if func_str.contains("invoke_contract") {
                let target_expr = call
                    .args
                    .iter()
                    .nth(1)
                    .or_else(|| call.args.first())
                    .map(|a| compact_tokens(peel_expr(a)))
                    .unwrap_or_else(|| "unknown".to_string());
                let target_method = call
                    .args
                    .iter()
                    .nth(2)
                    .or_else(|| call.args.iter().nth(1))
                    .and_then(|a| extract_symbol_literal(a))
                    .unwrap_or_else(|| "unknown".to_string());
                return Some((target_expr, target_method, compact_tokens(expr)));
            }
            None
        }
        _ => None,
    }
}

/// Checks if `expr` is `env.storage().{persistent,instance,temporary}().get(&key)`
/// and returns the key expression argument.
pub fn extract_storage_get_key(expr: &Expr) -> Option<&Expr> {
    if let Expr::MethodCall(mc) = expr {
        if mc.method == "get" && !mc.args.is_empty() {
            let receiver_str = compact_tokens(&mc.receiver);
            if receiver_str.contains("storage")
                && (receiver_str.contains("persistent")
                    || receiver_str.contains("instance")
                    || receiver_str.contains("temporary"))
            {
                return mc.args.first();
            }
        }
    }
    None
}

/// Checks if `expr` is `env.storage().{persistent,instance,temporary}().set(&key, &val)`
/// and returns `(key_expr, val_expr)`.
pub fn extract_storage_set_args(expr: &Expr) -> Option<(&Expr, Option<&Expr>)> {
    if let Expr::MethodCall(mc) = expr {
        let method_name = mc.method.to_string();
        if (method_name == "set" || method_name == "update") && !mc.args.is_empty() {
            let receiver_str = compact_tokens(&mc.receiver);
            if receiver_str.contains("storage")
                && (receiver_str.contains("persistent")
                    || receiver_str.contains("instance")
                    || receiver_str.contains("temporary"))
            {
                let key = mc.args.first()?;
                let val = mc.args.iter().nth(1);
                return Some((key, val));
            }
        }
    }
    None
}

/// Extracts bound variable names from a pattern (`let x = ...`, `let (a, b) = ...`).
fn extract_pat_idents(pat: &Pat, out: &mut Vec<String>) {
    match pat {
        Pat::Ident(pi) => out.push(pi.ident.to_string()),
        Pat::Type(pt) => extract_pat_idents(&pt.pat, out),
        Pat::Reference(pr) => extract_pat_idents(&pr.pat, out),
        Pat::Tuple(tup) => {
            for elem in &tup.elems {
                extract_pat_idents(elem, out);
            }
        }
        Pat::TupleStruct(ts) => {
            for elem in &ts.elems {
                extract_pat_idents(elem, out);
            }
        }
        Pat::Struct(ps) => {
            for field in &ps.fields {
                extract_pat_idents(&field.pat, out);
            }
        }
        _ => {}
    }
}

/// Collects all identifier names referenced anywhere inside `expr`.
fn collect_expr_idents(expr: &Expr, out: &mut HashSet<String>) {
    struct IdentVisitor<'a>(&'a mut HashSet<String>);
    impl<'ast, 'a> Visit<'ast> for IdentVisitor<'a> {
        fn visit_expr_path(&mut self, i: &'ast ExprPath) {
            if i.path.segments.len() == 1 {
                if let Some(seg) = i.path.segments.first() {
                    self.0.insert(seg.ident.to_string());
                }
            }
            syn::visit::visit_expr_path(self, i);
        }
    }
    let mut v = IdentVisitor(out);
    v.visit_expr(expr);
}

// ---------------------------------------------------------------------------
// 2. Taint Analysis (`SOROBAN-TAINT-001`)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct TaintOrigin {
    pub source_label: String,
    pub line: usize,
    pub col: usize,
}

/// Checks if `expr` directly invokes a taint source:
/// - `env.invoker()`
/// - `env.current_contract_address()`
pub fn detect_direct_taint_source(expr: &Expr) -> Option<TaintOrigin> {
    struct SourceDetector(Option<TaintOrigin>);
    impl<'ast> Visit<'ast> for SourceDetector {
        fn visit_expr_method_call(&mut self, i: &'ast ExprMethodCall) {
            let method = i.method.to_string();
            if method == "invoker" || method == "current_contract_address" {
                let span = i.span();
                self.0 = Some(TaintOrigin {
                    source_label: format!("env.{method}()"),
                    line: span.start().line,
                    col: span.start().column,
                });
                return;
            }
            syn::visit::visit_expr_method_call(self, i);
        }
    }
    let mut detector = SourceDetector(None);
    detector.visit_expr(expr);
    detector.0
}

/// Determines whether an expression depends on a direct taint source or any
/// currently tainted (and unsanitised) local variable.
fn evaluate_expr_taint(
    expr: &Expr,
    tainted_vars: &HashMap<String, TaintOrigin>,
    sanitised_vars: &HashSet<String>,
) -> Option<TaintOrigin> {
    if let Some(direct) = detect_direct_taint_source(expr) {
        return Some(direct);
    }
    let mut idents = HashSet::new();
    collect_expr_idents(expr, &mut idents);
    for ident in idents {
        if !sanitised_vars.contains(&ident) {
            if let Some(origin) = tainted_vars.get(&ident) {
                return Some(origin.clone());
            }
        }
    }
    None
}

/// Checks whether a single non-block expression (`require_auth`, `require_admin`, `assert!`)
/// sanitises a tainted variable at the current execution point.
fn collect_direct_sanitiser_in_expr(expr: &Expr, sanitised_vars: &mut HashSet<String>) {
    match expr {
        Expr::MethodCall(mc) => {
            let method = mc.method.to_string();
            if method == "require_auth" || method == "require_auth_for_args" {
                let mut idents = HashSet::new();
                collect_expr_idents(&mc.receiver, &mut idents);
                for id in idents {
                    sanitised_vars.insert(id);
                }
            }
        }
        Expr::Call(call) => {
            let func_name = compact_tokens(&call.func);
            if func_name.contains("require_auth")
                || func_name.contains("require_admin")
                || func_name.contains("verify_admin")
                || func_name.contains("check_auth")
                || func_name.contains("validate")
                || func_name.contains("ensure_authorized")
            {
                for arg in &call.args {
                    let mut idents = HashSet::new();
                    collect_expr_idents(arg, &mut idents);
                    for id in idents {
                        sanitised_vars.insert(id);
                    }
                }
            }
        }
        Expr::Macro(mac) => {
            let mac_name = compact_tokens(&mac.mac.path);
            if mac_name == "assert" || mac_name == "assert_eq" || mac_name == "assert_ne" {
                if let Ok(parsed_expr) = syn::parse2::<Expr>(mac.mac.tokens.clone()) {
                    let mut idents = HashSet::new();
                    collect_expr_idents(&parsed_expr, &mut idents);
                    for id in idents {
                        sanitised_vars.insert(id);
                    }
                } else {
                    for token in mac.mac.tokens.clone() {
                        if let proc_macro2::TokenTree::Ident(ident) = token {
                            sanitised_vars.insert(ident.to_string());
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// 3. Same-Key Read-Invoke-Write Reentrancy (`SOROBAN-REENTRANCY-001`)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct StorageReadRecord {
    canonical_key: String,
    key_variant: Option<String>,
    line: usize,
    col: usize,
    expr_text: String,
}

#[derive(Clone, Debug)]
struct ExposedStorageRead {
    read: StorageReadRecord,
    call_line: usize,
    call_col: usize,
    call_text: String,
}

/// Normalizes a storage key expression so `&DataKey::Balance(user.clone())` and
/// `let k = DataKey::Balance(user); ... &k` resolve to the same canonical string.
pub fn normalize_storage_key(expr: &Expr, key_aliases: &HashMap<String, String>) -> String {
    let peeled = peel_expr(expr);
    let raw = compact_tokens(peeled).replace(". clone ()", "");
    if let Some(aliased) = key_aliases.get(&raw) {
        return aliased.clone();
    }
    raw
}

/// Extracts the enum variant or structural prefix of a storage key, e.g.
/// `DataKey::Balance(user)` -> `Some("DataKey :: Balance")`.
pub fn extract_key_variant(canonical_key: &str) -> Option<String> {
    if let Some(paren_idx) = canonical_key.find('(') {
        let prefix = canonical_key[..paren_idx].trim();
        if !prefix.is_empty() {
            return Some(prefix.to_string());
        }
    }
    if canonical_key.contains("::") {
        return Some(canonical_key.to_string());
    }
    None
}

// ---------------------------------------------------------------------------
// 4. Storage Key Collision Detection (`SOROBAN-COLLISION-001`)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct StorageWriteKeyRecord {
    key_type: String,
    runtime_part: String,
    has_runtime_computation: bool,
    line: usize,
    col: usize,
    expr_text: String,
}

/// Checks if an expression contains a runtime-computed component (binary arithmetic,
/// dynamic function/method call other than `.clone()` or `symbol_short!`, etc.).
fn contains_runtime_computation(expr: &Expr) -> bool {
    struct RuntimeChecker(bool);
    impl<'ast> Visit<'ast> for RuntimeChecker {
        fn visit_expr_binary(&mut self, _i: &'ast ExprBinary) {
            self.0 = true;
        }
        fn visit_expr_call(&mut self, i: &'ast ExprCall) {
            let func_str = compact_tokens(&i.func);
            // Exclude static constructors like DataKey::Variant(...)
            if !func_str.starts_with("DataKey") && !func_str.starts_with("Key") {
                self.0 = true;
            }
            syn::visit::visit_expr_call(self, i);
        }
        fn visit_expr_method_call(&mut self, i: &'ast ExprMethodCall) {
            let m = i.method.to_string();
            if m != "clone" {
                self.0 = true;
            }
            syn::visit::visit_expr_method_call(self, i);
        }
    }
    let mut checker = RuntimeChecker(false);
    checker.visit_expr(expr);
    checker.0
}

/// Classifies a storage key expression into `(key_type, runtime_part, has_runtime_computation)`.
fn classify_storage_key(
    key_expr: &Expr,
    key_aliases: &HashMap<String, String>,
    key_ast_aliases: &HashMap<String, Expr>,
) -> Option<(String, String, bool)> {
    let mut resolved = peel_expr(key_expr);
    if let Expr::Path(ep) = resolved {
        if ep.path.segments.len() == 1 {
            let ident = ep.path.segments[0].ident.to_string();
            if let Some(aliased_expr) = key_ast_aliases.get(&ident) {
                resolved = peel_expr(aliased_expr);
            }
        }
    }

    match resolved {
        // E.g. `DataKey::Slot(base + 1)` or `DataKey::Balance(addr)`
        Expr::Call(call) => {
            let key_type = compact_tokens(&call.func);
            let args_str = call
                .args
                .iter()
                .map(|a| normalize_storage_key(a, key_aliases))
                .collect::<Vec<_>>()
                .join(", ");
            let has_runtime = call.args.iter().any(contains_runtime_computation);
            Some((key_type, args_str, has_runtime))
        }
        // E.g. `(symbol_short!("slot"), idx + 1)`
        Expr::Tuple(tup) if !tup.elems.is_empty() => {
            let first = &tup.elems[0];
            let prefix = extract_symbol_literal(first)
                .unwrap_or_else(|| compact_tokens(peel_expr(first)));
            let key_type = format!("Tuple({prefix})");
            let rest = tup
                .elems
                .iter()
                .skip(1)
                .map(|a| normalize_storage_key(a, key_aliases))
                .collect::<Vec<_>>()
                .join(", ");
            let has_runtime = tup.elems.iter().skip(1).any(contains_runtime_computation);
            Some((key_type, rest, has_runtime))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Function Semantic Analyzer
// ---------------------------------------------------------------------------

struct FunctionSemanticState {
    file: PathBuf,
    contract_name: String,
    fn_name: String,
    symbol_aliases: HashMap<String, String>,
    key_aliases: HashMap<String, String>,
    key_ast_aliases: HashMap<String, Expr>,
    tainted_vars: HashMap<String, TaintOrigin>,
    sanitised_vars: HashSet<String>,
    active_reads: Vec<StorageReadRecord>,
    exposed_reads: Vec<ExposedStorageRead>,
    write_keys: Vec<StorageWriteKeyRecord>,
    call_edges: Vec<CallEdge>,
    findings: Vec<SemanticFinding>,
}

impl FunctionSemanticState {
    fn new(file: PathBuf, contract_name: String, fn_name: String) -> Self {
        Self {
            file,
            contract_name,
            fn_name,
            symbol_aliases: HashMap::new(),
            key_aliases: HashMap::new(),
            key_ast_aliases: HashMap::new(),
            tainted_vars: HashMap::new(),
            sanitised_vars: HashSet::new(),
            active_reads: Vec::new(),
            exposed_reads: Vec::new(),
            write_keys: Vec::new(),
            call_edges: Vec::new(),
            findings: Vec::new(),
        }
    }

    fn analyze_block(&mut self, block: &Block) {
        for stmt in &block.stmts {
            self.analyze_stmt(stmt);
        }
    }

    fn analyze_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Local(local) => self.analyze_local(local),
            Stmt::Expr(expr, _) => self.analyze_expr(expr),
            Stmt::Macro(stmt_mac) => {
                let mac_name = compact_tokens(&stmt_mac.mac.path);
                if mac_name == "assert" || mac_name == "assert_eq" || mac_name == "assert_ne" {
                    for token in stmt_mac.mac.tokens.clone() {
                        if let proc_macro2::TokenTree::Ident(ident) = token {
                            self.sanitised_vars.insert(ident.to_string());
                        }
                    }
                }
            }
            Stmt::Item(_) => {}
        }
    }

    fn analyze_local(&mut self, local: &Local) {
        let mut bound_idents = Vec::new();
        extract_pat_idents(&local.pat, &mut bound_idents);

        if let Some(init) = &local.init {
            // First analyze the initializer expression (for reads, calls, writes)
            self.analyze_expr(&init.expr);

            // Track symbol aliases: `let sym = symbol_short!("transfer");`
            if let Some(sym_lit) = extract_symbol_literal(&init.expr) {
                for id in &bound_idents {
                    self.symbol_aliases.insert(id.clone(), sym_lit.clone());
                }
            }

            // Track storage key aliases: `let key = DataKey::Balance(user);`
            let init_peeled = peel_expr(&init.expr);
            let init_compact = compact_tokens(init_peeled).replace(". clone ()", "");
            if init_compact.contains("DataKey") || matches!(init_peeled, Expr::Tuple(_)) {
                for id in &bound_idents {
                    self.key_aliases.insert(id.clone(), init_compact.clone());
                    self.key_ast_aliases
                        .insert(id.clone(), (*init_peeled).clone());
                }
            }

            // Propagate taint from initializer to bound variables
            if let Some(origin) =
                evaluate_expr_taint(&init.expr, &self.tainted_vars, &self.sanitised_vars)
            {
                for id in bound_idents {
                    self.sanitised_vars.remove(&id);
                    self.tainted_vars.insert(id, origin.clone());
                }
            }
        }
    }

    fn analyze_expr(&mut self, expr: &Expr) {
        // 1. Handle control-flow and assignments in strict execution order
        match expr {
            Expr::Assign(ExprAssign { left, right, .. }) => {
                self.analyze_expr(right);
                if let Expr::Path(ep) = left.as_ref() {
                    if let Some(seg) = ep.path.segments.last() {
                        let var_name = seg.ident.to_string();
                        if let Some(origin) =
                            evaluate_expr_taint(right, &self.tainted_vars, &self.sanitised_vars)
                        {
                            self.sanitised_vars.remove(&var_name);
                            self.tainted_vars.insert(var_name, origin);
                        }
                    }
                }
                return;
            }
            Expr::If(expr_if) => {
                self.analyze_expr(&expr_if.cond);
                self.analyze_block(&expr_if.then_branch);
                if let Some((_, else_branch)) = &expr_if.else_branch {
                    self.analyze_expr(else_branch);
                }
                // After an early-exit guard `if caller != admin { panic!(...) }`, mark condition vars sanitised
                let branch_str = compact_tokens(&expr_if.then_branch);
                let is_guard = branch_str.contains("panic")
                    || branch_str.contains("panic_with_error")
                    || branch_str.contains("return")
                    || branch_str.contains("Err");
                if is_guard {
                    let mut idents = HashSet::new();
                    collect_expr_idents(&expr_if.cond, &mut idents);
                    for id in idents {
                        self.sanitised_vars.insert(id);
                    }
                }
                return;
            }
            Expr::Block(eb) => {
                self.analyze_block(&eb.block);
                return;
            }
            Expr::Loop(el) => {
                self.analyze_block(&el.body);
                return;
            }
            Expr::ForLoop(ef) => {
                self.analyze_expr(&ef.expr);
                self.analyze_block(&ef.body);
                return;
            }
            Expr::While(ew) => {
                self.analyze_expr(&ew.cond);
                self.analyze_block(&ew.body);
                return;
            }
            Expr::Match(em) => {
                self.analyze_expr(&em.expr);
                for arm in &em.arms {
                    self.analyze_expr(&arm.body);
                }
                return;
            }
            Expr::Return(ret) => {
                if let Some(e) = &ret.expr {
                    self.analyze_expr(e);
                }
                return;
            }
            Expr::MethodCall(mc) => {
                self.analyze_expr(&mc.receiver);
                for arg in &mc.args {
                    self.analyze_expr(arg);
                }
            }
            Expr::Call(call) => {
                for arg in &call.args {
                    self.analyze_expr(arg);
                }
            }
            Expr::Binary(bin) => {
                self.analyze_expr(&bin.left);
                self.analyze_expr(&bin.right);
            }
            Expr::Unary(un) => {
                self.analyze_expr(&un.expr);
            }
            Expr::Reference(r) => {
                self.analyze_expr(&r.expr);
            }
            Expr::Paren(p) => {
                self.analyze_expr(&p.expr);
            }
            Expr::Try(t) => {
                self.analyze_expr(&t.expr);
            }
            _ => {}
        }

        // 2. Check for direct sanitisers (`require_auth`, `require_admin`, `assert!`)
        collect_direct_sanitiser_in_expr(expr, &mut self.sanitised_vars);

        // 3. Check for storage reads: `env.storage().*().get(&key)`
        if let Some(key_expr) = extract_storage_get_key(expr) {
            let canonical_key = normalize_storage_key(key_expr, &self.key_aliases);
            let key_variant = extract_key_variant(&canonical_key);
            let span = expr.span();
            self.active_reads.push(StorageReadRecord {
                canonical_key,
                key_variant,
                line: span.start().line,
                col: span.start().column,
                expr_text: compact_tokens(expr),
            });
        }

        // 4. Check for cross-contract calls (`env.invoke_contract` or client calls)
        if let Some((target_contract_expr, target_method, call_text)) =
            extract_cross_contract_invocation(expr, &self.symbol_aliases)
        {
            let span = expr.span();
            let call_line = span.start().line;
            let call_col = span.start().column;

            self.call_edges.push(CallEdge {
                caller_contract: self.contract_name.clone(),
                caller_function: self.fn_name.clone(),
                target_contract_expr,
                resolved_target_contract: None,
                target_method,
                file: self.file.clone(),
                line: call_line,
                col: call_col,
            });

            // Promote all active storage reads to exposed reads across this external call
            for read in &self.active_reads {
                self.exposed_reads.push(ExposedStorageRead {
                    read: read.clone(),
                    call_line,
                    call_col,
                    call_text: call_text.clone(),
                });
            }
        }

        // 5. Check for storage writes: `env.storage().*().set(&key, &val)`
        if let Some((key_expr, val_expr_opt)) = extract_storage_set_args(expr) {
            let span = expr.span();
            let write_line = span.start().line;
            let write_col = span.start().column;
            let write_snippet = compact_tokens(expr);

            // 5a. Taint check (`SOROBAN-TAINT-001`) on key and value
            let key_taint =
                evaluate_expr_taint(key_expr, &self.tainted_vars, &self.sanitised_vars);
            let val_taint = val_expr_opt
                .and_then(|v| evaluate_expr_taint(v, &self.tainted_vars, &self.sanitised_vars));

            if let Some(origin) = key_taint.or(val_taint) {
                self.findings.push(SemanticFinding {
                    rule_id: RuleId::TaintUnsanitisedStorageWrite,
                    file: self.file.clone(),
                    contract_name: self.contract_name.clone(),
                    fn_name: self.fn_name.clone(),
                    line: write_line,
                    col: write_col,
                    message: format!(
                        "Tainted value derived from `{}` (line {}) reaches `storage().set()` in `{}` without prior `require_auth()` or sanitisation",
                        origin.source_label, origin.line, self.fn_name
                    ),
                    snippet: write_snippet.clone(),
                    related_locations: vec![RelatedLocation {
                        message: format!("Taint source `{}` introduced here", origin.source_label),
                        file: self.file.clone(),
                        line: origin.line,
                        col: origin.col,
                    }],
                });
            }

            // 5b. Same-key read-invoke-write reentrancy check (`SOROBAN-REENTRANCY-001`)
            let canonical_write_key = normalize_storage_key(key_expr, &self.key_aliases);
            let write_variant = extract_key_variant(&canonical_write_key);

            if let Some(exposed) = self.exposed_reads.iter().find(|exp| {
                exp.read.canonical_key == canonical_write_key
                    || (exp.read.key_variant.is_some()
                        && exp.read.key_variant == write_variant)
            }) {
                self.findings.push(SemanticFinding {
                    rule_id: RuleId::ReadCallWriteReentrancy,
                    file: self.file.clone(),
                    contract_name: self.contract_name.clone(),
                    fn_name: self.fn_name.clone(),
                    line: write_line,
                    col: write_col,
                    message: format!(
                        "Same-key read-invoke-write reentrancy on key `{}`: read at line {}, external call `{}` at line {}, overwritten at line {}",
                        canonical_write_key,
                        exposed.read.line,
                        exposed.call_text,
                        exposed.call_line,
                        write_line
                    ),
                    snippet: write_snippet.clone(),
                    related_locations: vec![
                        RelatedLocation {
                            message: format!(
                                "Storage key `{}` read before external call",
                                exposed.read.canonical_key
                            ),
                            file: self.file.clone(),
                            line: exposed.read.line,
                            col: exposed.read.col,
                        },
                        RelatedLocation {
                            message: format!(
                                "External cross-contract invocation `{}` occurs while read state is stale",
                                exposed.call_text
                            ),
                            file: self.file.clone(),
                            line: exposed.call_line,
                            col: exposed.call_col,
                        },
                    ],
                });
            }

            // 5c. Storage key collision check (`SOROBAN-COLLISION-001`)
            if let Some((key_type, runtime_part, has_runtime)) =
                classify_storage_key(key_expr, &self.key_aliases, &self.key_ast_aliases)
            {
                if let Some(prev) = self.write_keys.iter().find(|prev| {
                    prev.key_type == key_type
                        && prev.runtime_part != runtime_part
                        && (prev.has_runtime_computation || has_runtime)
                }) {
                    self.findings.push(SemanticFinding {
                        rule_id: RuleId::StorageKeyCollision,
                        file: self.file.clone(),
                        contract_name: self.contract_name.clone(),
                        fn_name: self.fn_name.clone(),
                        line: write_line,
                        col: write_col,
                        message: format!(
                            "Potential storage key collision on key type `{}`: written with runtime key `{}` at line {} and `{}` at line {}",
                            key_type, prev.runtime_part, prev.line, runtime_part, write_line
                        ),
                        snippet: write_snippet,
                        related_locations: vec![RelatedLocation {
                            message: format!(
                                "First storage().set() with key type `{}` and parameter `{}`",
                                prev.key_type, prev.runtime_part
                            ),
                            file: self.file.clone(),
                            line: prev.line,
                            col: prev.col,
                        }],
                    });
                }

                self.write_keys.push(StorageWriteKeyRecord {
                    key_type,
                    runtime_part,
                    has_runtime_computation: has_runtime,
                    line: write_line,
                    col: write_col,
                    expr_text: compact_tokens(key_expr),
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// File & Workspace Semantic Scanner
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SemanticScanReport {
    pub scanned_files: Vec<PathBuf>,
    pub call_graph: CallGraph,
    pub findings: Vec<SemanticFinding>,
    pub errors: Vec<String>,
}

/// Checks if an attribute list contains `#[cfg(test)]` so unit test modules are not
/// confused with production contract entrypoints.
fn has_cfg_test_attr(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && attr
                .meta
                .to_token_stream()
                .to_string()
                .contains("test")
    })
}

/// Scans a single parsed `syn::File` and appends call-graph nodes, edges, and findings.
pub fn scan_ast_semantics(path: &Path, ast: &File, include_cfg_test: bool) -> SemanticScanReport {
    let mut report = SemanticScanReport {
        scanned_files: vec![path.to_path_buf()],
        ..Default::default()
    };

    for item in &ast.items {
        scan_item(path, item, "Global", include_cfg_test, &mut report);
    }

    resolve_call_graph_targets(&mut report.call_graph);
    report
}

fn scan_item(
    path: &Path,
    item: &Item,
    default_contract: &str,
    include_cfg_test: bool,
    report: &mut SemanticScanReport,
) {
    match item {
        Item::Impl(ItemImpl {
            attrs,
            self_ty,
            items,
            ..
        }) => {
            if !include_cfg_test && has_cfg_test_attr(attrs) {
                return;
            }
            let contract_name = compact_tokens(self_ty);
            for impl_item in items {
                if let ImplItem::Fn(ImplItemFn {
                    attrs: fn_attrs,
                    sig,
                    block,
                    ..
                }) = impl_item
                {
                    if !include_cfg_test && has_cfg_test_attr(fn_attrs) {
                        continue;
                    }
                    let fn_name = sig.ident.to_string();
                    let line = sig.fn_token.span.start().line;
                    report.call_graph.nodes.push(CallNode {
                        contract: contract_name.clone(),
                        function: fn_name.clone(),
                        file: path.to_path_buf(),
                        line,
                    });

                    let mut state = FunctionSemanticState::new(
                        path.to_path_buf(),
                        contract_name.clone(),
                        fn_name,
                    );
                    state.analyze_block(block);
                    report.call_graph.edges.extend(state.call_edges);
                    report.findings.extend(state.findings);
                }
            }
        }
        Item::Fn(ItemFn {
            attrs, sig, block, ..
        }) => {
            if !include_cfg_test && has_cfg_test_attr(attrs) {
                return;
            }
            let fn_name = sig.ident.to_string();
            let line = sig.fn_token.span.start().line;
            report.call_graph.nodes.push(CallNode {
                contract: default_contract.to_string(),
                function: fn_name.clone(),
                file: path.to_path_buf(),
                line,
            });

            let mut state = FunctionSemanticState::new(
                path.to_path_buf(),
                default_contract.to_string(),
                fn_name,
            );
            state.analyze_block(block);
            report.call_graph.edges.extend(state.call_edges);
            report.findings.extend(state.findings);
        }
        Item::Mod(item_mod) => {
            if !include_cfg_test && has_cfg_test_attr(&item_mod.attrs) {
                return;
            }
            if let Some((_, items)) = &item_mod.content {
                for sub_item in items {
                    scan_item(path, sub_item, default_contract, include_cfg_test, report);
                }
            }
        }
        _ => {}
    }
}

/// Resolves `CallEdge::resolved_target_contract` by matching `target_method` and
/// `target_contract_expr` against all discovered `CallNode` definitions across the workspace.
pub fn resolve_call_graph_targets(call_graph: &mut CallGraph) {
    let mut method_to_contracts: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for node in &call_graph.nodes {
        method_to_contracts
            .entry(node.function.clone())
            .or_default()
            .push(node.contract.clone());
    }

    for edge in &mut call_graph.edges {
        if let Some(candidates) = method_to_contracts.get(&edge.target_method) {
            let expr_lower = edge.target_contract_expr.to_lowercase().replace('_', "");
            // Prefer a candidate whose contract name matches the target variable hint
            let matched = candidates
                .iter()
                .find(|c| {
                    let c_lower = c.to_lowercase().replace("contract", "").replace('_', "");
                    !c_lower.is_empty() && expr_lower.contains(&c_lower)
                })
                .or_else(|| candidates.iter().find(|c| *c != &edge.caller_contract))
                .or_else(|| candidates.first());
            if let Some(resolved) = matched {
                edge.resolved_target_contract = Some(resolved.clone());
            }
        }
    }
}

/// Scans a single Rust source string in memory.
pub fn scan_source_str(source: &str, virtual_path: &Path) -> Result<SemanticScanReport, String> {
    let ast: File =
        syn::parse_file(source).map_err(|e| format!("Parse error {}: {e}", virtual_path.display()))?;
    Ok(scan_ast_semantics(virtual_path, &ast, true))
}

/// Scans a single `.rs` file on disk.
pub fn scan_file_semantics(path: &Path) -> Result<SemanticScanReport, String> {
    let content =
        fs::read_to_string(path).map_err(|e| format!("IO error {}: {e}", path.display()))?;
    let ast: File =
        syn::parse_file(&content).map_err(|e| format!("Parse error {}: {e}", path.display()))?;
    Ok(scan_ast_semantics(path, &ast, false))
}

/// Recursively scans a directory of Soroban contracts (excluding `target/` and `fuzz/`),
/// builds the unified cross-contract call graph, and returns all semantic findings.
pub fn scan_directory_semantics(dir: &Path) -> SemanticScanReport {
    let mut combined = SemanticScanReport::default();

    for entry in WalkDir::new(dir)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            name != "target" && name != "node_modules" && name != ".git"
        })
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path().extension().is_some_and(|ext| ext == "rs")
                && e.file_name().to_string_lossy() != "test.rs"
        })
    {
        match scan_file_semantics(entry.path()) {
            Ok(file_report) => {
                combined.scanned_files.extend(file_report.scanned_files);
                combined.call_graph.nodes.extend(file_report.call_graph.nodes);
                combined.call_graph.edges.extend(file_report.call_graph.edges);
                combined.findings.extend(file_report.findings);
            }
            Err(err) => combined.errors.push(err),
        }
    }

    resolve_call_graph_targets(&mut combined.call_graph);
    combined
}

// ---------------------------------------------------------------------------
// 5. SARIF v2.1.0 Generator
// ---------------------------------------------------------------------------

pub fn generate_sarif(report: &SemanticScanReport) -> serde_json::Value {
    let rules = [
        RuleId::TaintUnsanitisedStorageWrite,
        RuleId::ReadCallWriteReentrancy,
        RuleId::StorageKeyCollision,
    ]
    .iter()
    .map(|rule| {
        serde_json::json!({
            "id": rule.as_str(),
            "shortDescription": {
                "text": rule.short_description()
            },
            "defaultConfiguration": {
                "level": rule.severity_level()
            }
        })
    })
    .collect::<Vec<_>>();

    let results = report
        .findings
        .iter()
        .map(|f| {
            let uri = f.file.to_string_lossy().replace('\\', "/");
            let related = f
                .related_locations
                .iter()
                .enumerate()
                .map(|(idx, rel)| {
                    serde_json::json!({
                        "id": idx + 1,
                        "message": { "text": rel.message },
                        "physicalLocation": {
                            "artifactLocation": {
                                "uri": rel.file.to_string_lossy().replace('\\', "/")
                            },
                            "region": {
                                "startLine": rel.line.max(1),
                                "startColumn": rel.col.max(1)
                            }
                        }
                    })
                })
                .collect::<Vec<_>>();

            serde_json::json!({
                "ruleId": f.rule_id.as_str(),
                "level": f.rule_id.severity_level(),
                "message": {
                    "text": f.message
                },
                "locations": [
                    {
                        "physicalLocation": {
                            "artifactLocation": {
                                "uri": uri
                            },
                            "region": {
                                "startLine": f.line.max(1),
                                "startColumn": f.col.max(1),
                                "snippet": {
                                    "text": f.snippet
                                }
                            }
                        }
                    }
                ],
                "relatedLocations": related
            })
        })
        .collect::<Vec<_>>();

    serde_json::json!({
        "$schema": "https://raw.githubusercontent.com/oasis-tcs/sarif-spec/master/Schemata/sarif-schema-2.1.0.json",
        "version": "2.1.0",
        "runs": [
            {
                "tool": {
                    "driver": {
                        "name": "tessera-soroban-semantic-scanner",
                        "version": "0.2.0",
                        "informationUri": "https://github.com/A4-Stellar/Tessera",
                        "rules": rules
                    }
                },
                "results": results,
                "properties": {
                    "scannedFilesCount": report.scanned_files.len(),
                    "callGraph": {
                        "nodesCount": report.call_graph.nodes.len(),
                        "edgesCount": report.call_graph.edges.len(),
                        "edges": report.call_graph.edges
                    }
                }
            }
        ]
    })
}

// ---------------------------------------------------------------------------
// Unit Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_call_graph_resolves_cross_contract_invoke_contract() {
        let source = r#"
            pub struct ComplianceContract;
            impl ComplianceContract {
                pub fn is_allowed(env: Env, addr: Address) -> bool {
                    true
                }
            }

            pub struct AssetTokenContract;
            impl AssetTokenContract {
                pub fn transfer(env: Env, compliance: Address, from: Address, to: Address) {
                    from.require_auth();
                    let ok: bool = env.invoke_contract(
                        &compliance,
                        &symbol_short!("is_allowed"),
                        (from.clone(),),
                    );
                }
            }
        "#;

        let report = scan_source_str(source, Path::new("call_graph_test.rs")).unwrap();
        assert_eq!(report.call_graph.nodes.len(), 2);
        assert_eq!(report.call_graph.edges.len(), 1);
        let edge = &report.call_graph.edges[0];
        assert_eq!(edge.caller_contract, "AssetTokenContract");
        assert_eq!(edge.caller_function, "transfer");
        assert_eq!(edge.target_method, "is_allowed");
        assert_eq!(
            edge.resolved_target_contract.as_deref(),
            Some("ComplianceContract")
        );
    }

    #[test]
    fn test_taint_analysis_flags_unsanitised_invoker_and_current_contract_address() {
        let source = r#"
            pub struct VulnerableVault;
            impl VulnerableVault {
                pub fn hijack_admin(env: Env) {
                    let caller = env.invoker();
                    // No require_auth or admin check before writing tainted caller to storage!
                    env.storage().instance().set(&DataKey::Admin, &caller);
                }

                pub fn record_self_unverified(env: Env) {
                    let self_addr = env.current_contract_address();
                    let key = DataKey::Owner(self_addr.clone());
                    env.storage().persistent().set(&key, &100i128);
                }
            }
        "#;

        let report = scan_source_str(source, Path::new("taint_vuln.rs")).unwrap();
        let taint_findings: Vec<_> = report
            .findings
            .iter()
            .filter(|f| f.rule_id == RuleId::TaintUnsanitisedStorageWrite)
            .collect();
        assert_eq!(taint_findings.len(), 2);
        assert!(taint_findings[0].message.contains("env.invoker()"));
        assert!(taint_findings[1]
            .message
            .contains("env.current_contract_address()"));
    }

    #[test]
    fn test_taint_analysis_allows_sanitised_values() {
        let source = r#"
            pub struct SafeVault;
            impl SafeVault {
                pub fn authorized_write(env: Env) {
                    let caller = env.invoker();
                    caller.require_auth();
                    env.storage().instance().set(&DataKey::Admin, &caller);
                }

                pub fn asserted_self_write(env: Env, expected: Address) {
                    let self_addr = env.current_contract_address();
                    assert_eq!(self_addr, expected);
                    env.storage().persistent().set(&DataKey::ContractAddr, &self_addr);
                }
            }
        "#;

        let report = scan_source_str(source, Path::new("taint_safe.rs")).unwrap();
        let taint_findings: Vec<_> = report
            .findings
            .iter()
            .filter(|f| f.rule_id == RuleId::TaintUnsanitisedStorageWrite)
            .collect();
        assert!(taint_findings.is_empty());
    }

    #[test]
    fn test_same_key_read_invoke_write_reentrancy_detected() {
        let source = r#"
            pub struct ReentrantPool;
            impl ReentrantPool {
                pub fn claim(env: Env, token: Address, user: Address, amount: i128) {
                    user.require_auth();
                    let current: i128 = env
                        .storage()
                        .persistent()
                        .get(&DataKey::Balance(user.clone()))
                        .unwrap_or(0);
                    env.invoke_contract(
                        &token,
                        &symbol_short!("transfer"),
                        (user.clone(), amount),
                    );
                    env.storage()
                        .persistent()
                        .set(&DataKey::Balance(user), &(current - amount));
                }
            }
        "#;

        let report = scan_source_str(source, Path::new("reentrant_pool.rs")).unwrap();
        let reentrancy_findings: Vec<_> = report
            .findings
            .iter()
            .filter(|f| f.rule_id == RuleId::ReadCallWriteReentrancy)
            .collect();
        assert_eq!(reentrancy_findings.len(), 1);
        assert_eq!(reentrancy_findings[0].fn_name, "claim");
        assert_eq!(reentrancy_findings[0].related_locations.len(), 2);
    }

    #[test]
    fn test_storage_key_collision_detected() {
        let source = r#"
            pub struct CollisionContract;
            impl CollisionContract {
                pub fn write_buckets(env: Env, admin: Address, base_slot: u64, offset: u64) {
                    admin.require_auth();
                    env.storage()
                        .persistent()
                        .set(&DataKey::Slot(base_slot + 1), &100i128);
                    env.storage()
                        .persistent()
                        .set(&DataKey::Slot(base_slot + offset), &200i128);
                }
            }
        "#;

        let report = scan_source_str(source, Path::new("collision.rs")).unwrap();
        let collision_findings: Vec<_> = report
            .findings
            .iter()
            .filter(|f| f.rule_id == RuleId::StorageKeyCollision)
            .collect();
        assert_eq!(collision_findings.len(), 1);
        assert!(collision_findings[0].message.contains("DataKey :: Slot"));
    }

    #[test]
    fn test_sarif_v2_1_0_generation_and_all_22_workspace_contracts() {
        let contracts_dir = Path::new("../../contracts");
        let report = scan_directory_semantics(contracts_dir);
        assert!(
            report.errors.is_empty(),
            "Expected zero parse errors across contracts/, got: {:?}",
            report.errors
        );
        assert!(
            report.scanned_files.len() >= 22,
            "Expected at least 22 contract source files scanned, got {}",
            report.scanned_files.len()
        );
        assert!(
            report.call_graph.nodes.len() >= 50,
            "Expected >= 50 contract functions in call graph, got {}",
            report.call_graph.nodes.len()
        );
        assert!(
            !report.call_graph.edges.is_empty(),
            "Expected cross-contract edges in workspace call graph"
        );

        let sarif = generate_sarif(&report);
        assert_eq!(sarif["version"], "2.1.0");
        assert_eq!(
            sarif["runs"][0]["tool"]["driver"]["name"],
            "tessera-soroban-semantic-scanner"
        );
        assert_eq!(
            sarif["runs"][0]["tool"]["driver"]["rules"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
    }
}
