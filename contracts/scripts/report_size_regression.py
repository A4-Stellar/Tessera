#!/usr/bin/env python3
"""Compare two WASM size reports and produce a Markdown regression summary.

The current report comes from the PR build (``check_wasm_size.py --json``), the
baseline from the same pipeline run on ``main``. This script is intentionally
side-effect free unless ``--comment`` is passed: ``build_report`` is a pure
function so it can be imported and tested without touching the filesystem.

Examples
--------
    python3 contracts/scripts/report_size_regression.py \
        --current contracts/size-report.json \
        --baseline baseline/size-report.json \
        --comment regression.md

    python3 contracts/scripts/report_size_regression.py \
        --current current.json --baseline baseline.json --fail-on-regression

Exit codes
----------
0
    Report produced successfully (the default, even when regressions exist).
1
    An input could not be read, or ``--fail-on-regression`` was passed and at
    least one contract grew beyond the tolerance.
2
    Command-line usage error.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any, Dict, List, Optional, Sequence, Tuple

COMMENT_MARKER = "<!-- tessera-wasm-size-regression -->"

# Accepted aliases across report schema versions. The canonical names written
# by check_wasm_size.py are "name" and "size_bytes".
_NAME_KEYS = ("name", "contract", "crate", "package")
_SIZE_KEYS = ("size_bytes", "bytes", "size", "current_bytes", "current_size")
_BASELINE_SIZE_KEYS = ("baseline_bytes", "baseline_size", "before_bytes", "before_size", "size_bytes", "bytes", "size")


# --------------------------------------------------------------------------- #
# Pure comparison helpers
# --------------------------------------------------------------------------- #


def _normalise_name(name: Any) -> str:
    """Normalise a crate name so ``tessera_foo`` == ``tessera-foo``."""
    return str(name).strip().lower().replace("_", "-")


def _first_present(entry: Dict[str, Any], keys: Sequence[str]) -> Optional[int]:
    for key in keys:
        if key in entry and entry[key] is not None:
            try:
                return int(entry[key])
            except (TypeError, ValueError):
                continue
    return None


def _contracts(report: Dict[str, Any], size_keys: Sequence[str]) -> Dict[str, int]:
    """Extract a ``{normalised name: size}`` map from a size-report object.

    Accepts both the list-of-objects schema written by ``check_wasm_size.py``
    and a plain ``{"name": bytes}`` mapping for hand-written baselines.
    """
    raw = report.get("contracts", {}) if isinstance(report, dict) else {}
    result: Dict[str, int] = {}

    if isinstance(raw, dict):
        for name, value in raw.items():
            if isinstance(value, dict):
                size = _first_present(value, size_keys)
            else:
                try:
                    size = int(value)
                except (TypeError, ValueError):
                    size = None
            if size is not None:
                result[_normalise_name(name)] = size
        return result

    if isinstance(raw, list):
        for entry in raw:
            if not isinstance(entry, dict):
                continue
            name = next((entry[key] for key in _NAME_KEYS if key in entry), None)
            size = _first_present(entry, size_keys)
            if name is not None and size is not None:
                result[_normalise_name(name)] = size
    return result


def _signed(value: int) -> str:
    return f"+{value:,}" if value > 0 else f"{value:,}"


def regressions(
    current: Dict[str, Any],
    baseline: Dict[str, Any],
    tolerance_bytes: int = 0,
) -> List[Dict[str, Any]]:
    """Pure function returning one dict per contract that grew.

    ``tolerance_bytes`` suppresses regressions smaller than or equal to it,
    which is useful for absorbing compiler-version noise.
    """
    current_map = _contracts(current, _SIZE_KEYS)
    baseline_map = _contracts(baseline, _BASELINE_SIZE_KEYS)
    found: List[Dict[str, Any]] = []
    for name, after in current_map.items():
        before = baseline_map.get(name)
        if before is None:
            continue
        delta = after - before
        if delta > tolerance_bytes:
            found.append({"name": name, "before": before, "after": after, "delta": delta})
    found.sort(key=lambda item: (-item["delta"], item["name"]))
    return found


def build_report(
    current: Dict[str, Any],
    baseline: Dict[str, Any],
    tolerance_bytes: int = 0,
) -> str:
    """Build the Markdown regression report (pure; no file or network I/O).

    Parameters
    ----------
    current, baseline:
        Parsed JSON objects from ``check_wasm_size.py --json``. Either may be
        empty, in which case a graceful "no data" report is produced.
    tolerance_bytes:
        Growths of at most this many bytes are reported as unchanged.

    Returns
    -------
    str
        A Markdown document beginning with the PR-comment marker.
    """
    current_map = _contracts(current, _SIZE_KEYS)
    baseline_map = _contracts(baseline, _BASELINE_SIZE_KEYS)
    has_baseline = bool(baseline_map)

    lines: List[str] = [
        COMMENT_MARKER,
        "## WASM size regression report",
        "",
    ]

    if not current_map:
        lines.append("_No current size report data was available; nothing to compare._")
        return "\n".join(lines) + "\n"

    rows: List[Dict[str, Any]] = []
    for name in sorted(set(current_map) | set(baseline_map)):
        before = baseline_map.get(name)
        after = current_map.get(name)

        if not has_baseline:
            status = "NO BASELINE"
            delta: Optional[int] = None
            is_regression = False
        elif after is None:
            status = "REMOVED"
            delta = -int(before or 0)
            is_regression = False
        elif before is None:
            status = "NEW"
            delta = after
            is_regression = False
        else:
            delta = after - before
            if delta > tolerance_bytes:
                status = "REGRESSION"
                is_regression = True
            elif delta < -tolerance_bytes:
                status = "IMPROVED"
                is_regression = False
            else:
                status = "UNCHANGED"
                is_regression = False

        rows.append(
            {
                "name": name,
                "before": before,
                "after": after,
                "delta": delta,
                "status": status,
                "is_regression": is_regression,
            }
        )

    # Regressions first (largest growth at the top), then deterministic by name.
    rows.sort(key=lambda row: (not row["is_regression"], -abs(row["delta"] or 0), row["name"]))

    lines.extend(
        [
            "| Contract | Before (B) | After (B) | Δ (B) | Status |",
            "| --- | ---: | ---: | ---: | --- |",
        ]
    )
    for row in rows:
        before = f"{row['before']:,}" if row["before"] is not None else "—"
        after = f"{row['after']:,}" if row["after"] is not None else "—"
        delta = _signed(row["delta"]) if row["delta"] is not None else "—"
        status = row["status"]
        if status == "REGRESSION":
            status = f"⚠️ **{status}**"
        elif status in {"IMPROVED", "NEW"}:
            status = f"✅ {status}"
        elif status == "REMOVED":
            status = f"➖ {status}"
        lines.append(f"| `{row['name']}` | {before} | {after} | {delta} | {status} |")

    count_regressions = sum(1 for row in rows if row["is_regression"])
    count_improved = sum(1 for row in rows if row["status"] == "IMPROVED")
    count_new = sum(1 for row in rows if row["status"] == "NEW")
    known = [row["delta"] for row in rows if row["delta"] is not None and row["before"] is not None]
    net_delta = sum(known)

    lines.extend([""])
    if has_baseline:
        if count_regressions:
            lines.append(f"⚠️ **{count_regressions} regression(s)** detected beyond a {tolerance_bytes}-byte tolerance.")
        else:
            lines.append("✅ No size regressions detected.")
        parts = [f"{count_improved} improved", f"{count_new} new"]
        lines.append(
            "Other changes: "
            + ", ".join(parts)
            + f". Net change across compared contracts: **{_signed(net_delta)} bytes**."
        )
    else:
        lines.append("_No baseline artifact was available for this run; showing current sizes only._")
    lines.append(
        "_Budgets: `contracts/size_budget.json`. Generated by "
        "`contracts/scripts/report_size_regression.py`._"
    )
    return "\n".join(lines) + "\n"


# --------------------------------------------------------------------------- #
# I/O + CLI
# --------------------------------------------------------------------------- #


def load_report(path: Path) -> Tuple[Dict[str, Any], Optional[str]]:
    """Load a report JSON, returning ``(data, error_message)``."""
    try:
        with path.open("r", encoding="utf-8") as handle:
            data = json.load(handle)
    except FileNotFoundError:
        return {}, f"report not found: {path}"
    except json.JSONDecodeError as exc:
        return {}, f"report is not valid JSON ({path}): {exc}"
    if not isinstance(data, dict):
        return {}, f"report must be a JSON object: {path}"
    return data, None


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Compare current and baseline WASM size reports and emit a Markdown summary.",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    parser.add_argument("--current", type=Path, required=True, help="Current (PR) size report JSON")
    parser.add_argument("--baseline", type=Path, required=True, help="Baseline (main) size report JSON")
    parser.add_argument(
        "--comment",
        type=Path,
        default=None,
        help="Write the Markdown to this file (for an actions/github-script PR comment)",
    )
    parser.add_argument(
        "--tolerance-bytes",
        type=int,
        default=0,
        help="Ignore growths of at most this many bytes",
    )
    parser.add_argument(
        "--fail-on-regression",
        action="store_true",
        help="Exit 1 when at least one regression is found (default: never fail on regressions)",
    )
    return parser


def main(argv: Optional[Sequence[str]] = None) -> int:
    args = build_parser().parse_args(argv)
    if args.tolerance_bytes < 0:
        print("report_size_regression.py: error: --tolerance-bytes must be >= 0", file=sys.stderr)
        return 2

    current, current_error = load_report(args.current)
    baseline, baseline_error = load_report(args.baseline)
    if current_error or baseline_error:
        for error in (current_error, baseline_error):
            if error:
                print(f"report_size_regression.py: error: {error}", file=sys.stderr)
        return 1

    markdown = build_report(current, baseline, args.tolerance_bytes)

    if args.comment is not None:
        try:
            args.comment.parent.mkdir(parents=True, exist_ok=True)
            args.comment.write_text(markdown, encoding="utf-8")
        except OSError as exc:
            print(f"report_size_regression.py: error: cannot write comment file: {exc}", file=sys.stderr)
            return 1
        print(f"Wrote regression report to {args.comment}")

    print(markdown, end="")

    if args.fail_on_regression and regressions(current, baseline, args.tolerance_bytes):
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
