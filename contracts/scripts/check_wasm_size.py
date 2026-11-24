#!/usr/bin/env python3
"""Check built Soroban WASM artifacts against their per-contract size budgets.

The script is deliberately dependency-free (Python 3 standard library only) so
it can run in CI, in a pre-commit hook, or on a developer laptop with nothing
but ``python3`` installed.

Default paths are resolved relative to *this script*, not the current working
directory, so the following all behave identically::

    python3 contracts/scripts/check_wasm_size.py
    cd contracts && python3 scripts/check_wasm_size.py
    cd /tmp && python3 /abs/path/to/contracts/scripts/check_wasm_size.py

Examples
--------
Strict budget gate (fails with exit status 1 when any contract is over budget)::

    python3 contracts/scripts/check_wasm_size.py

Emit a machine-readable report for the regression job::

    python3 contracts/scripts/check_wasm_size.py --json contracts/size-report.json

List contracts within 15% of their budget (for the code-size issue automation)::

    python3 contracts/scripts/check_wasm_size.py --check-issues

Exit codes
----------
0
    All artifacts are within budget (always the case for ``--check-issues``
    when the artifacts could be read; that mode is informational).
1
    At least one artifact exceeds its budget, the WASM directory contains no
    artifacts, or an input file could not be read.
2
    Command-line usage error.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any, Dict, List, Optional, Sequence, Tuple

# --------------------------------------------------------------------------- #
# Defaults, resolved relative to this script (contracts/scripts/).
# --------------------------------------------------------------------------- #

SCRIPT_DIR = Path(__file__).resolve().parent
CONTRACTS_DIR = SCRIPT_DIR.parent
DEFAULT_WASM_DIR = CONTRACTS_DIR / "target" / "wasm32-unknown-unknown" / "release"
DEFAULT_BUDGET = CONTRACTS_DIR / "size_budget.json"
DEFAULT_LIMIT_BYTES = 65_536
DEFAULT_WARNING_PERCENT = 85

REPORT_SCHEMA_VERSION = 1


# --------------------------------------------------------------------------- #
# Budget / report helpers
# --------------------------------------------------------------------------- #


def _positive_int(value: Any, fallback: int) -> int:
    """Coerce ``value`` to a positive int, falling back when it is unusable."""
    try:
        number = int(value)
    except (TypeError, ValueError):
        return fallback
    return number if number > 0 else fallback


def load_budget(path: Path) -> Dict[str, Any]:
    """Load and lightly validate a ``size_budget.json`` file.

    Accepts both ``"crate-name": 65536`` and ``"crate-name": {"max_bytes":
    65536}`` shapes so older/newer budget files keep working.

    Raises ``ValueError`` with a human-readable message on malformed input.
    """
    try:
        with path.open("r", encoding="utf-8") as handle:
            data = json.load(handle)
    except FileNotFoundError as exc:
        raise ValueError(f"budget file not found: {path}") from exc
    except json.JSONDecodeError as exc:
        raise ValueError(f"budget file is not valid JSON ({path}): {exc}") from exc

    if not isinstance(data, dict):
        raise ValueError(f"budget file must contain a JSON object: {path}")

    raw_contracts = data.get("contracts", {})
    if not isinstance(raw_contracts, dict):
        raise ValueError(f"'contracts' must be a JSON object in {path}")

    contracts: Dict[str, int] = {}
    for name, entry in raw_contracts.items():
        if isinstance(entry, dict):
            entry = entry.get("max_bytes")
        value = _positive_int(entry, int(data.get("limit_bytes", DEFAULT_LIMIT_BYTES)))
        contracts[str(name)] = value

    warning_percent = _positive_int(data.get("warning_threshold_percent", DEFAULT_WARNING_PERCENT), DEFAULT_WARNING_PERCENT)
    if warning_percent > 100:
        warning_percent = DEFAULT_WARNING_PERCENT

    return {
        "version": data.get("version", 1),
        "limit_bytes": _positive_int(data.get("limit_bytes", DEFAULT_LIMIT_BYTES), DEFAULT_LIMIT_BYTES),
        "warning_threshold_percent": warning_percent,
        "contracts": contracts,
        "notes": data.get("notes", ""),
    }


def _contract_key(wasm_path: Path, budgets: Dict[str, int]) -> str:
    """Map a WASM filename to a budget key.

    Cargo replaces hyphens with underscores in crate artifact names, so
    ``tessera-asset-token`` builds ``tessera_asset_token.wasm``. Prefer an
    exact stem match, then the underscore-normalised form, then the lowercase
    form of both.
    """
    stem = wasm_path.stem
    candidates = [stem, stem.replace("_", "-"), stem.lower(), stem.lower().replace("_", "-")]
    for candidate in candidates:
        if candidate in budgets:
            return candidate
    return stem.replace("_", "-")


def _row_for(wasm_path: Path, budgets: Dict[str, int], limit: int, warning_percent: int) -> Dict[str, Any]:
    """Build one report row for a single WASM artifact."""
    name = _contract_key(wasm_path, budgets)
    size = wasm_path.stat().st_size
    if name in budgets:
        max_bytes = _positive_int(budgets[name], limit)
        budget_source = "contract"
    else:
        max_bytes = limit
        budget_source = "global-limit"

    # Integer arithmetic keeps the near-limit decision exact at the boundary.
    near_limit = size * 100 >= max_bytes * warning_percent
    return {
        "name": name,
        "wasm_file": wasm_path.name,
        "size_bytes": size,
        # Aliases keep the report easy to consume by other tools.
        "bytes": size,
        "max_bytes": max_bytes,
        "warning_bytes": (max_bytes * warning_percent) // 100,
        "percent": round(size * 100.0 / max_bytes, 2),
        "exceeded": size > max_bytes,
        "near_limit": near_limit,
        "budget_source": budget_source,
    }


def collect_rows(wasm_dir: Path, budget: Dict[str, Any]) -> List[Dict[str, Any]]:
    """Return one report row per ``*.wasm`` file in ``wasm_dir``, sorted by name."""
    if not wasm_dir.is_dir():
        raise ValueError(f"WASM directory does not exist: {wasm_dir}")
    wasm_files = sorted(wasm_dir.glob("*.wasm"), key=lambda path: path.name)
    rows = [
        _row_for(path, budget["contracts"], budget["limit_bytes"], budget["warning_threshold_percent"])
        for path in wasm_files
    ]
    return rows


def build_size_report(rows: Sequence[Dict[str, Any]], budget: Dict[str, Any]) -> Dict[str, Any]:
    """Assemble the JSON report object written by ``--json``."""
    exceeded = sum(1 for row in rows if row["exceeded"])
    near_limit = sum(1 for row in rows if row["near_limit"] and not row["exceeded"])
    return {
        "schema_version": REPORT_SCHEMA_VERSION,
        "limit_bytes": budget["limit_bytes"],
        "warning_threshold_percent": budget["warning_threshold_percent"],
        "contracts": list(rows),
        "summary": {
            "checked": len(rows),
            "exceeded": exceeded,
            "near_limit": near_limit,
            "total_size_bytes": sum(row["size_bytes"] for row in rows),
        },
    }


def _format_table(rows: Sequence[Dict[str, Any]]) -> str:
    """Render a fixed-width size table; includes an over-budget legend."""
    headers = ("Contract", "WASM", "Size (B)", "Budget (B)", "Usage", "Status")
    table_rows: List[Tuple[str, ...]] = []
    for row in rows:
        if row["exceeded"]:
            status = "OVER BUDGET"
        elif row["near_limit"]:
            status = "NEAR LIMIT"
        else:
            status = "OK"
        table_rows.append(
            (
                row["name"],
                row["wasm_file"],
                f"{row['size_bytes']:,}",
                f"{row['max_bytes']:,}",
                f"{row['percent']:.1f}%",
                status,
            )
        )

    widths = [len(header) for header in headers]
    for table_row in table_rows:
        for index, cell in enumerate(table_row):
            widths[index] = max(widths[index], len(cell))

    def line(cells: Sequence[str]) -> str:
        return "  ".join(cell.ljust(widths[index]) for index, cell in enumerate(cells)).rstrip()

    separator = "  ".join("-" * width for width in widths)
    lines = [line(headers), separator]
    lines.extend(line(cells) for cells in table_rows)
    return "\n".join(lines)


def _issues_markdown(rows: Sequence[Dict[str, Any]], warning_percent: int) -> str:
    """Render the markdown body used by the code-size issue automation."""
    flagged = [row for row in rows if row["near_limit"] or row["exceeded"]]
    lines = [
        "### WASM size watchlist",
        "",
        f"Contracts at or above **{warning_percent}%** of their size budget "
        "(or over budget) in the latest `wasm-size` CI run:",
        "",
    ]
    if not flagged:
        lines.append("_No contracts are currently near their WASM size budget._")
        return "\n".join(lines)

    lines.extend(
        [
            "| Contract | Size (B) | Budget (B) | Usage | Status |",
            "| --- | ---: | ---: | ---: | --- |",
        ]
    )
    for row in sorted(flagged, key=lambda item: (-item["percent"], item["name"])):
        status = "OVER BUDGET" if row["exceeded"] else "NEAR LIMIT"
        lines.append(
            f"| `{row['name']}` | {row['size_bytes']:,} | {row['max_bytes']:,} "
            f"| {row['percent']:.1f}% | {status} |"
        )
    lines.extend(
        [
            "",
            "Budgets are defined in `contracts/size_budget.json`. "
            "See `.github/workflows/wasm-size.yml` for the full report.",
        ]
    )
    return "\n".join(lines)


def _write_json(path: Path, report: Dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as handle:
        json.dump(report, handle, indent=2, sort_keys=False)
        handle.write("\n")


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Check Soroban WASM artifact sizes against per-contract budgets.",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    parser.add_argument(
        "--wasm-dir",
        type=Path,
        default=DEFAULT_WASM_DIR,
        help="Directory containing the built *.wasm files",
    )
    parser.add_argument(
        "--budget",
        type=Path,
        default=DEFAULT_BUDGET,
        help="Path to size_budget.json",
    )
    parser.add_argument(
        "--json",
        dest="json_path",
        type=Path,
        default=None,
        help="Write a machine-readable size report to this path",
    )
    parser.add_argument(
        "--check-issues",
        action="store_true",
        help="Print only the near-limit markdown used by the code-size issue automation (informational, exit 0)",
    )
    return parser


def main(argv: Optional[Sequence[str]] = None) -> int:
    args = build_parser().parse_args(argv)

    try:
        budget = load_budget(args.budget)
        rows = collect_rows(args.wasm_dir, budget)
    except ValueError as exc:
        print(f"check_wasm_size.py: error: {exc}", file=sys.stderr)
        return 1

    if not rows:
        print(
            f"check_wasm_size.py: error: no .wasm files found in {args.wasm_dir}. "
            "Build the contracts first with: "
            "cd contracts && cargo build --release --target wasm32-unknown-unknown",
            file=sys.stderr,
        )
        return 1

    report = build_size_report(rows, budget)
    if args.json_path is not None:
        try:
            _write_json(args.json_path, report)
        except OSError as exc:
            print(f"check_wasm_size.py: error: cannot write report: {exc}", file=sys.stderr)
            return 1
        print(f"Wrote size report to {args.json_path}")

    if args.check_issues:
        print(_issues_markdown(rows, budget["warning_threshold_percent"]))
        return 0

    print(_format_table(rows))
    summary = report["summary"]
    print(
        f"\n{summary['checked']} artifact(s) checked; "
        f"{summary['exceeded']} over budget, {summary['near_limit']} near limit "
        f"(>= {budget['warning_threshold_percent']}%). "
        f"Hard limit: {budget['limit_bytes']:,} bytes."
    )

    if summary["exceeded"]:
        print("\nOVER-BUDGET CONTRACTS:", file=sys.stderr)
        for row in rows:
            if row["exceeded"]:
                over = row["size_bytes"] - row["max_bytes"]
                print(
                    f"  {row['name']}: {row['size_bytes']:,} B > {row['max_bytes']:,} B "
                    f"({over:,} B over, {row['percent']:.1f}%)",
                    file=sys.stderr,
                )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
