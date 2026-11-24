#!/usr/bin/env python3
"""Every Prometheus alert links to a runbook in docs/runbooks/ (issue #474).

The triage bot chooses its automated steps from the runbook file name in the
`runbook_url` annotation, and Grafana OnCall shows the link to the on-call
engineer, so each alert must have one and the file must exist. Each alert also
needs a `priority` label or a `severity` the bot can map to one.

Run from anywhere:  python3 infrastructure/monitoring/tests/check_runbooks.py
"""
import pathlib
import re
import sys

import yaml

monitoring = pathlib.Path(__file__).resolve().parent.parent
repo = monitoring.parent.parent
runbooks = repo / "docs" / "runbooks"
RULE_FILES = ["alerts.yaml", "slo_rules.yml"]
URL = re.compile(
    r"^https://github\.com/A4-Stellar/Tessera/blob/main/docs/runbooks/([a-z0-9-]+)\.md(#[a-z0-9-]+)?$"
)
SEVERITIES = {"critical", "page", "warning", "ticket"}

errors = []
checked = 0
for name in RULE_FILES:
    doc = yaml.safe_load((monitoring / name).read_text())
    for group in doc["groups"]:
        for rule in group["rules"]:
            if "alert" not in rule:
                continue
            checked += 1
            where = f"{name}: {rule['alert']}"
            annotations = rule.get("annotations") or {}
            labels = rule.get("labels") or {}
            url = annotations.get("runbook_url")
            if not url:
                errors.append(f"{where} has no runbook_url annotation")
                continue
            match = URL.match(url)
            if not match:
                errors.append(f"{where} runbook_url {url} is not a docs/runbooks/*.md link")
                continue
            if not (runbooks / f"{match.group(1)}.md").is_file():
                errors.append(f"{where} links to missing docs/runbooks/{match.group(1)}.md")
            priority = labels.get("priority")
            if priority is not None and priority not in {"P1", "P2", "P3", "P4"}:
                errors.append(f"{where} has invalid priority label {priority!r}")
            if priority is None and labels.get("severity") not in SEVERITIES:
                errors.append(f"{where} has neither a priority label nor a known severity")

if errors:
    print("\n".join(errors))
    sys.exit(1)
print(f"ok: {checked} alerts link to existing runbooks")
