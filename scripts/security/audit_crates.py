#!/usr/bin/env python3
"""Audit every Rust lockfile for RustSec advisories and unaudited unsafe code."""
from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path


def run(root: Path, allow: set[str]) -> int:
    locks = sorted(root.glob("**/Cargo.lock"))
    if not locks:
        print("No Cargo.lock files found", file=sys.stderr)
        return 2
    failed = False
    for lock in locks:
        crate_dir = lock.parent
        result = subprocess.run(["cargo", "audit", "--file", str(lock)], cwd=crate_dir, text=True)
        if result.returncode:
            failed = True
        metadata = subprocess.run(["cargo", "metadata", "--format-version", "1", "--locked"], cwd=crate_dir, capture_output=True, text=True)
        if metadata.returncode == 0:
            packages = json.loads(metadata.stdout).get("packages", [])
            for package in packages:
                source = package.get("manifest_path", "")
                if not source or package["name"] in allow:
                    continue
                text = Path(source).read_text(encoding="utf-8", errors="ignore")
                if "unsafe" in text and package["name"] not in allow:
                    print(f"unaudited unsafe code: {package['name']} ({source})", file=sys.stderr)
                    failed = True
    return 1 if failed else 0


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("root", nargs="?", type=Path, default=Path("."))
    parser.add_argument("--allow-unsafe", action="append", default=[])
    args = parser.parse_args()
    raise SystemExit(run(args.root.resolve(), set(args.allow_unsafe)))
