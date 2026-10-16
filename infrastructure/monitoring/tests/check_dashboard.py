#!/usr/bin/env python3
"""Consistency check for the SLO dashboard (issue #312).

Every `tessera:*` series the dashboard queries must be produced by a
recording rule in slo_rules.yml, and every alert the runbook names must
exist. Run from anywhere:  python3 infrastructure/monitoring/tests/check_dashboard.py
"""
import json
import pathlib
import re
import sys

import yaml

here = pathlib.Path(__file__).resolve().parent.parent
rules = yaml.safe_load((here / "slo_rules.yml").read_text())
dashboard = json.loads((here / "slo_dashboard.json").read_text())
readme = (here / "README.md").read_text()

recorded = {r["record"] for g in rules["groups"] for r in g["rules"] if "record" in r}
alerts = {r["alert"] for g in rules["groups"] for r in g["rules"] if "alert" in r}

exprs = [t["expr"] for p in dashboard["panels"] for t in p.get("targets", [])]
exprs += [a["expr"] for a in dashboard["annotations"]["list"] if "expr" in a]
used = {m for e in exprs for m in re.findall(r"tessera:[a-z0-9_:]+", e)}

errors = [f"dashboard queries unrecorded series {m}" for m in sorted(used - recorded)]
errors += [f"README runbook does not cover alert {a}" for a in sorted(alerts) if a not in readme]
ids = [p["id"] for p in dashboard["panels"]]
if len(ids) != len(set(ids)):
    errors.append("duplicate panel ids")

for e in errors:
    print("ERROR:", e)
print(f"{len(used)} recorded series used, {len(alerts)} alerts, {len(dashboard['panels'])} panels")
sys.exit(1 if errors else 0)
