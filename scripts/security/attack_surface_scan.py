#!/usr/bin/env python3
"""Automated threat-modeling & attack-surface analyzer (issue #139).

Scans:
  1. Soroban Rust contracts for missing ``require_auth()`` checks,
     dangerous unchecked storage writes, and reentrancy vectors.
  2. REST API handlers (``api/src/routes/*.rs``) for unvalidated ``Path<>``
     inputs and missing rate-limit protection.

Emits a SARIF 2.1.0 report (``--output``) for upload via
``github/codeql-action/upload-sarif`` and prints a human-readable summary.
Stdlib only — no third-party dependencies.

Usage:
    python scripts/security/attack_surface_scan.py \
        --contracts-dir contracts --api-dir api/src/routes \
        --output attack-surface.sarif

    # Fail CI only on errors (default); use --fail-on warning for strict mode.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]

RULES = {
    "TESSERA001": {
        "name": "SorobanMissingAuth",
        "shortDescription": {"text": "State-changing contract fn without require_auth()"},
        "fullDescription": {
            "text": "Public contract function performs a storage write without calling "
            "require_auth()/require_admin(). Flagged as a missing-auth / un-guarded admin-call vector."
        },
        "defaultConfiguration": {"level": "error"},
        "properties": {"tags": ["security", "soroban", "auth"]},
    },
    "TESSERA002": {
        "name": "SorobanUncheckedStorageWrite",
        "shortDescription": {"text": "Storage write without input validation"},
        "fullDescription": {
            "text": "Storage write in a function that performs no visible input validation "
            "(assert/require/panic/checked-arith/bounds check) before writing."
        },
        "defaultConfiguration": {"level": "warning"},
        "properties": {"tags": ["security", "soroban", "storage"]},
    },
    "TESSERA003": {
        "name": "SorobanReentrancyVector",
        "shortDescription": {"text": "Possible reentrancy vector (external call before state write)"},
        "fullDescription": {
            "text": "Function performs an external/cross-contract call (transfer/invoke_contract/call) "
            "before a storage write with no reentrancy guard."
        },
        "defaultConfiguration": {"level": "warning"},
        "properties": {"tags": ["security", "soroban", "reentrancy"]},
    },
    "TESSERA004": {
        "name": "ApiUnvalidatedPathInput",
        "shortDescription": {"text": "REST handler uses Path<> input without validation"},
        "fullDescription": {
            "text": "Axum handler extracts a Path<> parameter but shows no validation "
            "(parse/validate/BadRequest/NotFound/bounds check) before use."
        },
        "defaultConfiguration": {"level": "warning"},
        "properties": {"tags": ["security", "api", "validation"]},
    },
    "TESSERA005": {
        "name": "ApiMissingRateLimit",
        "shortDescription": {"text": "API router without rate-limit protection"},
        "fullDescription": {
            "text": "No rate-limit middleware (rate_limit_middleware/GovernorRateLimiter/RateLimiter) "
            "wired into the API router; handlers are exposed without throttling."
        },
        "defaultConfiguration": {"level": "error"},
        "properties": {"tags": ["security", "api", "rate-limit"]},
    },
}

FN_RE = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+(\w+)\s*\(", re.MULTILINE)
AUTH_RE = re.compile(r"require_auth\s*\(|require_admin\s*\(|require_auth_for_args")
STORAGE_WRITE_RE = re.compile(r"\.storage\s*\(\s*\)[\s\S]{0,120}?\.(set|remove|update)\s*\(")
VALIDATION_RE = re.compile(
    r"assert|panic_with_error|panic!|require_|ensure_|checked_|"
    r"if\s+.*(return|panic|Err)|==|!=|matches!|validate"
)
EXTERNAL_CALL_RE = re.compile(
    r"\.transfer\s*\(|invoke_contract|call_contract|token_client|"
    r"cross_contract|TokenClient|execute\s*\("
)
GUARD_RE = re.compile(r"reentrancy|non_reentrant|nonReentrant|_guard|locked|mutex|CEI\b", re.IGNORECASE)
PATH_PARAM_RE = re.compile(r"Path\s*<\s*([^>]+)\s*>")
PATH_VALIDATION_RE = re.compile(
    r"\.parse::<|parse\(\)|from_str|validate|BadRequest|NotFound|"
    r"is_none|checked|len\(\)|is_empty|starts_with|trim|allowed|"
    r"Uuid::|Address::|deserialize|JsonSchema"
)
RATE_LIMIT_RE = re.compile(r"rate_limit_middleware|GovernorRateLimiter|RateLimiter|governor::|tower_governor")


@dataclass
class Finding:
    rule_id: str
    level: str
    message: str
    path: Path
    line: int
    snippet: str = ""


def _iter_fns(source: str):
    """Yield (name, body, start_line, is_pub, offset) using brace matching."""
    for match in FN_RE.finditer(source):
        name = match.group(1)
        line = source.count("\n", 0, match.start()) + 1
        is_pub = match.group(0).strip().startswith("pub")
        # Find opening brace of the body.
        brace = source.find("{", match.end())
        if brace == -1:
            continue
        # Skip trait declarations / signatures ending with ';' before '{'.
        semi = source.find(";", match.end(), brace)
        if semi != -1:
            continue
        depth = 0
        i = brace
        while i < len(source):
            if source[i] == "{":
                depth += 1
            elif source[i] == "}":
                depth -= 1
                if depth == 0:
                    yield name, source[brace : i + 1], line, is_pub, match.start()
                    break
            i += 1


def _contractimpl_ranges(source: str) -> list[tuple[int, int]]:
    """Return (start, end) offsets of ``#[contractimpl]`` impl bodies."""
    ranges: list[tuple[int, int]] = []
    for match in re.finditer(r"#\s*\[\s*contractimpl\s*\]", source):
        brace = source.find("{", match.end())
        if brace == -1:
            continue
        depth = 0
        i = brace
        while i < len(source):
            if source[i] == "{":
                depth += 1
            elif source[i] == "}":
                depth -= 1
                if depth == 0:
                    ranges.append((brace, i + 1))
                    break
            i += 1
    return ranges


def _mod_test_ranges(source: str) -> list[tuple[int, int]]:
    """Return (start, end) offsets of ``mod tests`` bodies to exclude."""
    ranges: list[tuple[int, int]] = []
    for match in re.finditer(r"\bmod\s+tests\s*\{", source):
        brace = source.find("{", match.start())
        depth = 0
        i = brace
        while i < len(source):
            if source[i] == "{":
                depth += 1
            elif source[i] == "}":
                depth -= 1
                if depth == 0:
                    ranges.append((brace, i + 1))
                    break
            i += 1
    return ranges


def _in_ranges(pos: int, ranges: list[tuple[int, int]]) -> bool:
    return any(start <= pos < end for start, end in ranges)


def _has_test_attr(source: str, fn_offset: int) -> bool:
    """Check whether a test attribute sits directly above the fn."""
    window = source[max(0, fn_offset - 400) : fn_offset].split("\n")[-6:]
    return any(re.search(r"#\s*\[\s*(test|tokio::test)", line) for line in window)


def _first_match_line(body: str, pattern: re.Pattern) -> int:
    m = pattern.search(body)
    if not m:
        return 0
    return body.count("\n", 0, m.start())


def scan_contract(path: Path, repo_root: Path) -> list[Finding]:
    findings: list[Finding] = []
    try:
        source = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return findings
    rel = path.relative_to(repo_root) if path.is_absolute() else path
    posix = rel.as_posix()
    # Skip unit-test scaffolding: findings there are noise, not attack surface.
    if "/tests/" in posix or posix.endswith("/test.rs") or posix.endswith("_test.rs"):
        return findings
    impl_ranges = _contractimpl_ranges(source)
    test_ranges = _mod_test_ranges(source)
    for name, body, start_line, is_pub, offset in _iter_fns(source):
        if not is_pub or name.startswith("__") or name in {"version"}:
            continue
        if _in_ranges(offset, test_ranges) or _has_test_attr(source, offset):
            continue
        # Only entry points inside #[contractimpl] are externally reachable;
        # helpers elsewhere (e.g. interest-rate math) inherit the caller's
        # auth, so they are not missing-auth findings (still checked for
        # unchecked writes / reentrancy below).
        in_contract = (not impl_ranges) or _in_ranges(offset, impl_ranges)
        write_match = STORAGE_WRITE_RE.search(body)
        if not write_match:
            continue
        write_offset = _first_match_line(body, STORAGE_WRITE_RE)
        has_auth = bool(AUTH_RE.search(body))
        if not has_auth and in_contract:
            findings.append(
                Finding(
                    rule_id="TESSERA001",
                    level="error",
                    message=(
                        f"State-changing fn `{name}` performs a storage write "
                        f"without require_auth()/require_admin()."
                    ),
                    path=rel,
                    line=start_line + write_offset,
                    snippet=f"fn {name} ... storage write without auth",
                )
            )
        if not VALIDATION_RE.search(body[: write_match.start()]):
            findings.append(
                Finding(
                    rule_id="TESSERA002",
                    level="warning",
                    message=(
                        f"Unchecked storage write in `{name}`: no input validation "
                        f"(assert/require/checked-arith/bounds check) before the write."
                    ),
                    path=rel,
                    line=start_line + write_offset,
                    snippet=f"fn {name} ... unchecked storage write",
                )
            )
        ext_match = EXTERNAL_CALL_RE.search(body)
        if ext_match and ext_match.start() < write_match.start() and not GUARD_RE.search(body):
            findings.append(
                Finding(
                    rule_id="TESSERA003",
                    level="warning",
                    message=(
                        f"Possible reentrancy vector in `{name}`: external call "
                        f"before storage write with no reentrancy guard."
                    ),
                    path=rel,
                    line=start_line + _first_match_line(body, EXTERNAL_CALL_RE),
                    snippet=f"fn {name} ... external call before state write",
                )
            )
    return findings


def router_has_rate_limit(api_root: Path) -> bool:
    """Check the API router/middleware tree for wired rate limiting."""
    candidates = [
        api_root / "routes" / "mod.rs",
        api_root / "middleware" / "mod.rs",
        api_root / "middleware" / "rate_limit.rs",
        api_root / "main.rs",
    ]
    for candidate in candidates:
        if candidate.is_file():
            try:
                if RATE_LIMIT_RE.search(candidate.read_text(encoding="utf-8", errors="replace")):
                    return True
            except OSError:
                continue
    # Fallback: grep the whole api src tree (cheap, bounded).
    src_root = api_root.parent if api_root.name == "routes" else api_root
    for rs in src_root.rglob("*.rs"):
        try:
            if RATE_LIMIT_RE.search(rs.read_text(encoding="utf-8", errors="replace")):
                return True
        except OSError:
            continue
    return False


def scan_api_handler(path: Path, repo_root: Path, rate_limited: bool) -> list[Finding]:
    findings: list[Finding] = []
    try:
        source = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return findings
    rel = path.relative_to(repo_root) if path.is_absolute() else path
    test_ranges = _mod_test_ranges(source)
    for name, body, start_line, _is_pub, offset in _iter_fns(source):
        if _in_ranges(offset, test_ranges) or _has_test_attr(source, offset):
            continue
        # Extract Path<> param names from the handler signature. The body
        # starts at the opening brace, so look back at the source lines above
        # the body for the full signature.
        sig_lines = source.split("\n")[max(0, start_line - 1) : start_line + 8]
        sig_text = "\n".join(sig_lines)
        path_vars = re.findall(r"Path\s*\(\s*(\w+)\s*\)", sig_text)
        if not path_vars and "Path<" not in sig_text and "Path<" not in body[:400]:
            continue
        if not path_vars and "Path<" not in body[:400]:
            continue
        validated = bool(re.search(r"BadRequest|NotFound|ApiError", body))
        for var in path_vars:
            # Validation must touch the path variable itself (method call,
            # allowlist/validator mention) — incidental `.parse()` calls
            # elsewhere (e.g. sorting balances) or plain `stored == input`
            # comparisons don't count.
            if re.search(
                rf"\b{re.escape(var)}\b\s*\.\s*(parse|trim|to_lowercase|to_uppercase|len|is_empty|from_str)\s*\(?"
                rf"|validate\w*\s*\([^)]*\b{re.escape(var)}\b"
                rf"|\b{re.escape(var)}\b[^\n;{{}}]{{0,80}}\b(validate|allowed|whitelist|sanitize)\b",
                body,
            ):
                validated = True
        if not validated:
            findings.append(
                Finding(
                    rule_id="TESSERA004",
                    level="warning",
                    message=(
                        f"Handler `{name}` extracts Path<> input "
                        f"({', '.join(path_vars) if path_vars else 'path param'}) without visible "
                        f"validation (parse/BadRequest/NotFound/bounds check)."
                    ),
                    path=rel,
                    line=start_line,
                    snippet=f"async fn {name} ... unvalidated Path input",
                )
            )
    if not rate_limited:
        # One finding per file keeps SARIF actionable without spamming per-handler.
        findings.append(
            Finding(
                rule_id="TESSERA005",
                level="error",
                message="API router has no rate-limit middleware wired; handlers lack throttling.",
                path=rel,
                line=1,
                snippet="missing rate_limit_middleware",
            )
        )
    return findings


def to_sarif(findings: list[Finding], repo_root: Path) -> dict:
    results = []
    for finding in findings:
        rule = RULES[finding.rule_id]
        results.append(
            {
                "ruleId": finding.rule_id,
                "level": rule["defaultConfiguration"]["level"],
                "message": {"text": finding.message},
                "locations": [
                    {
                        "physicalLocation": {
                            "artifactLocation": {"uri": finding.path.as_posix()},
                            "region": {"startLine": max(1, finding.line)},
                            "contextRegion": {"startLine": max(1, finding.line), "snippet": {"text": finding.snippet}},
                        }
                    }
                ],
            }
        )
    return {
        "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
        "version": "2.1.0",
        "runs": [
            {
                "tool": {
                    "driver": {
                        "name": "tessera-attack-surface-scan",
                        "version": "1.0.0",
                        "informationUri": "https://github.com/A4-Stellar/Tessera",
                        "rules": [
                            {
                                "id": rule_id,
                                "name": rule["name"],
                                "shortDescription": rule["shortDescription"],
                                "fullDescription": rule["fullDescription"],
                                "defaultConfiguration": rule["defaultConfiguration"],
                                "properties": rule["properties"],
                            }
                            for rule_id, rule in RULES.items()
                        ],
                    }
                },
                "results": results,
            }
        ],
    }


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Tessera attack-surface scanner (SARIF).")
    parser.add_argument("--contracts-dir", default=str(REPO_ROOT / "contracts"))
    parser.add_argument("--api-dir", default=str(REPO_ROOT / "api" / "src"))
    parser.add_argument("--output", "-o", default="attack-surface.sarif")
    parser.add_argument(
        "--fail-on",
        choices=["none", "warning", "error"],
        default="error",
        help="Exit 1 when findings at/above this severity exist.",
    )
    parser.add_argument("--text", action="store_true", help="Also print findings as text.")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    repo_root = REPO_ROOT
    contracts_dir = Path(args.contracts_dir)
    api_dir = Path(args.api_dir)
    if not contracts_dir.is_absolute():
        contracts_dir = (Path.cwd() / contracts_dir).resolve()
        repo_root = Path.cwd()
    if not api_dir.is_absolute():
        api_dir = (Path.cwd() / api_dir).resolve()

    findings: list[Finding] = []
    if contracts_dir.is_dir():
        for rs in sorted(contracts_dir.rglob("*.rs")):
            if "/target/" in rs.as_posix() or "/fuzz/" in rs.as_posix():
                continue
            findings.extend(scan_contract(rs.resolve(), repo_root))
    else:
        print(f"warning: contracts dir not found: {contracts_dir}", file=sys.stderr)

    routes_dir = api_dir / "routes" if (api_dir / "routes").is_dir() else api_dir
    rate_limited = router_has_rate_limit(api_dir)
    if routes_dir.is_dir():
        for rs in sorted(routes_dir.glob("*.rs")):
            if rs.name == "test_support.rs":
                continue
            findings.extend(scan_api_handler(rs.resolve(), repo_root, rate_limited))
    else:
        print(f"warning: api dir not found: {api_dir}", file=sys.stderr)

    sarif = to_sarif(findings, repo_root)
    output = Path(args.output)
    output.write_text(json.dumps(sarif, indent=2) + "\n", encoding="utf-8")

    errors = sum(1 for f in findings if f.level == "error")
    warnings = sum(1 for f in findings if f.level == "warning")
    print(f"Scanned contracts: {contracts_dir}, api: {api_dir}")
    print(f"Findings: {len(findings)} ({errors} error, {warnings} warning) -> {output}")
    if args.text or errors or warnings:
        for finding in findings:
            print(f"  [{finding.level}] {finding.rule_id} {finding.path}:{finding.line}: {finding.message}")

    if args.fail_on == "error" and errors:
        return 1
    if args.fail_on == "warning" and findings:
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
