#!/usr/bin/env python3
"""Offline unit tests for scripts/security/audit_crates.py.

Run with:

    python3 -m unittest scripts/security/test_audit_crates.py -v
"""
from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path

# Load the module by path so these tests work whether or not ``scripts`` is a
# Python package (the repository does not ship an __init__.py there).
_MODULE_PATH = Path(__file__).with_name("audit_crates.py")
_SPEC = importlib.util.spec_from_file_location("audit_crates", _MODULE_PATH)
assert _SPEC is not None and _SPEC.loader is not None
audit_crates = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(audit_crates)


class RustSecNormalizationTests(unittest.TestCase):
    def test_vulnerabilities_shape_with_nested_advisory(self):
        entry = {
            "id": "RUSTSEC-2024-0001",
            "package": "time",
            "version": "0.3.20",
            "advisory": {
                "id": "RUSTSEC-2024-0001",
                "title": "Denial of service in time",
                "description": "Parsing untrusted input can panic.",
                "severity": "high",
                "patched_versions": [">=0.3.35"],
                "aliases": ["CVE-2024-12345", "GHSA-aaaa-bbbb-cccc"],
                "url": "https://rustsec.org/advisories/RUSTSEC-2024-0001",
            },
        }
        advisory = audit_crates.normalize_rustsec_advisory(entry)
        self.assertEqual(advisory["id"], "RUSTSEC-2024-0001")
        self.assertEqual(advisory["crate"], "time")
        self.assertEqual(advisory["severity"], "HIGH")
        self.assertEqual(advisory["fixed_version"], "0.3.35")
        self.assertEqual(advisory["vulnerable_version"], "0.3.20")
        self.assertEqual(advisory["cve"], "CVE-2024-12345")
        self.assertEqual(advisory["ghsa"], "GHSA-aaaa-bbbb-cccc")
        self.assertIn("Parsing untrusted input", advisory["description"])
        self.assertEqual(advisory["source"], "rustsec")
        self.assertEqual(advisory["ecosystem"], "rust")

    def test_advisories_shape_with_flat_keys(self):
        entry = {
            "advisory_id": "RUSTSEC-2021-0002",
            "crate": "regex",
            "version": "1.5.4",
            "severity": "critical",
            "title": "Regex stack overflow",
            "patched": ">=1.5.5, <2.0.0",
            "link": "https://rustsec.org/advisories/RUSTSEC-2021-0002",
        }
        advisory = audit_crates.normalize_rustsec_advisory(entry)
        self.assertEqual(advisory["id"], "RUSTSEC-2021-0002")
        self.assertEqual(advisory["crate"], "regex")
        self.assertEqual(advisory["severity"], "CRITICAL")
        self.assertEqual(advisory["fixed_version"], "1.5.5")
        self.assertEqual(advisory["vulnerable_version"], "1.5.4")
        self.assertEqual(advisory["url"], "https://rustsec.org/advisories/RUSTSEC-2021-0002")

    def test_fix_dict_and_string_id(self):
        entry = {
            "id": "RUSTSEC-2020-0003",
            "package": "smallvec",
            "severity": "moderate",
            "fix": {"versions": [">=1.6.1"]},
        }
        advisory = audit_crates.normalize_rustsec_advisory(entry)
        self.assertEqual(advisory["fixed_version"], "1.6.1")
        self.assertEqual(advisory["severity"], "MODERATE")
        # A non-dict entry must not crash normalisation.
        self.assertEqual(audit_crates.normalize_rustsec_advisory("RUSTSEC-X")["id"], "RUSTSEC-X")

    def test_vulnerable_version_falls_back_to_lockfile(self):
        with tempfile.TemporaryDirectory() as tmp:
            lock = Path(tmp) / "Cargo.lock"
            lock.write_text(
                '[[package]]\nname = "time"\nversion = "0.3.20"\nsource = "registry"\n',
                encoding="utf-8",
            )
            advisory = audit_crates.normalize_rustsec_advisory(
                {"id": "RUSTSEC-1", "package": "time"}, lock
            )
            self.assertEqual(advisory["vulnerable_version"], "0.3.20")


class GhsaNormalizationTests(unittest.TestCase):
    def test_ghsa_node_and_merge(self):
        node = {
            "ghsaId": "GHSA-aaaa-bbbb-cccc",
            "cveId": "CVE-2024-12345",
            "severity": "CRITICAL",
            "description": "Remote code execution via crafted payloads.",
            "permalink": "https://github.com/advisories/GHSA-aaaa-bbbb-cccc",
            "vulnerabilities": {
                "nodes": [
                    {
                        "package": {"name": "time"},
                        "vulnerableVersionRange": "< 0.3.35",
                        "firstPatchedVersion": {"identifier": "0.3.35"},
                    }
                ]
            },
        }
        ghsa = audit_crates.normalize_ghsa_advisory(node, crate_hint="time")
        self.assertEqual(ghsa["severity"], "CRITICAL")
        self.assertEqual(ghsa["fixed_version"], "0.3.35")
        self.assertEqual(ghsa["cve"], "CVE-2024-12345")
        self.assertEqual(ghsa["ghsa"], "GHSA-aaaa-bbbb-cccc")
        self.assertEqual(ghsa["permalink"], "https://github.com/advisories/GHSA-aaaa-bbbb-cccc")

        base = {
            "id": "RUSTSEC-2024-0001", "crate": "time", "severity": "HIGH",
            "description": "", "fixed_version": None, "cve": None, "url": "",
            "source": "rustsec",
        }
        merged = audit_crates.merge_github_advisory(dict(base), ghsa)
        self.assertEqual(merged["severity"], "CRITICAL")  # max of HIGH/CRITICAL
        self.assertEqual(merged["fixed_version"], "0.3.35")
        self.assertEqual(merged["source"], "github")
        self.assertEqual(merged["url"], ghsa["permalink"])

    def test_merge_without_ghsa_keeps_rustsec_source(self):
        advisory = {"source": "rustsec", "severity": "HIGH"}
        self.assertEqual(audit_crates.merge_github_advisory(dict(advisory), None)["source"], "rustsec")


class SeverityFilteringTests(unittest.TestCase):
    def test_normalize_severity_mapping(self):
        self.assertEqual(audit_crates.normalize_severity("high"), "HIGH")
        self.assertEqual(audit_crates.normalize_severity("Critical"), "CRITICAL")
        self.assertEqual(audit_crates.normalize_severity("moderate"), "MODERATE")
        self.assertEqual(audit_crates.normalize_severity("medium"), "MODERATE")
        self.assertEqual(audit_crates.normalize_severity(None), "INFO")
        self.assertEqual(audit_crates.normalize_severity("bogus"), "INFO")

    def test_actionable_only_high_and_critical(self):
        for severity in ("HIGH", "CRITICAL"):
            self.assertTrue(audit_crates.is_actionable({"severity": severity}))
        for severity in ("LOW", "MODERATE", "INFO", "NONE"):
            self.assertFalse(audit_crates.is_actionable({"severity": severity}))
        self.assertEqual(audit_crates.max_severity("low", "critical", "high"), "CRITICAL")


class PrBodyTests(unittest.TestCase):
    def test_render_pr_body_contains_required_parts(self):
        advisory = {
            "id": "RUSTSEC-2024-0001",
            "crate": "time",
            "severity": "HIGH",
            "description": "Parsing untrusted input can panic.",
            "fixed_version": "0.3.35",
            "cve": "CVE-2024-12345",
            "url": "https://rustsec.org/advisories/RUSTSEC-2024-0001",
        }
        old = '[dependencies]\ntime = "0.3.20"\n'
        new = '[dependencies]\ntime = "0.3.35"\n'
        body = audit_crates.render_pr_body(advisory, old, new, "Cargo.toml")
        self.assertIn("Parsing untrusted input can panic.", body)
        self.assertIn("0.3.35", body)
        self.assertIn("```diff", body)
        self.assertIn("-time = \"0.3.20\"", body)
        self.assertIn("+time = \"0.3.35\"", body)
        self.assertIn("https://nvd.nist.gov/vuln/detail/CVE-2024-12345", body)

    def test_render_pr_body_falls_back_to_advisory_url(self):
        advisory = {"id": "RUSTSEC-X", "crate": "x", "description": "desc", "fixed_version": "2.0.0",
                    "url": "https://rustsec.org/advisories/RUSTSEC-X"}
        body = audit_crates.render_pr_body(advisory, "a = 1\n", "a = 2\n", "Cargo.toml")
        self.assertIn("https://rustsec.org/advisories/RUSTSEC-X", body)
        self.assertNotIn("nvd.nist.gov", body)
        self.assertIn("```diff", body)


class DenyTomlTests(unittest.TestCase):
    def test_update_deny_toml_idempotent(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "deny.toml"
            original = (
                "[bans]\nmultiple-versions = \"warn\"\n"
                "deny = []  # machine-managed by audit_crates.py --bot: add { name = \"...\", version = \"=x.y.z\" }\n"
                "skip = []\n"
            )
            path.write_text(original, encoding="utf-8")

            self.assertTrue(audit_crates.update_deny_toml(path, "time", "0.3.20"))
            first = path.read_text(encoding="utf-8")
            self.assertIn('{ name = "time", version = "=0.3.20" }', first)
            self.assertIn(audit_crates.DENY_MARKER, first)
            self.assertIn("skip = []", first)

            # Second call must be a no-op.
            self.assertFalse(audit_crates.update_deny_toml(path, "time", "0.3.20"))
            self.assertEqual(path.read_text(encoding="utf-8"), first)

    def test_update_deny_toml_multiline_array(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "deny.toml"
            path.write_text(
                '[bans]\ndeny = [\n    { name = "other", version = "=1.0.0" },\n]\n',
                encoding="utf-8",
            )
            self.assertTrue(audit_crates.update_deny_toml(path, "time", "0.3.20"))
            text = path.read_text(encoding="utf-8")
            self.assertIn('{ name = "other", version = "=1.0.0" }', text)
            self.assertIn('{ name = "time", version = "=0.3.20" }', text)
            self.assertFalse(audit_crates.update_deny_toml(path, "time", "0.3.20"))

    def test_dry_run_does_not_write(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "deny.toml"
            path.write_text("[bans]\ndeny = []\n", encoding="utf-8")
            self.assertTrue(audit_crates.update_deny_toml(path, "x", "1.2.3", dry_run=True))
            self.assertEqual(path.read_text(encoding="utf-8"), "[bans]\ndeny = []\n")


class NpmNormalizationTests(unittest.TestCase):
    def test_fix_object_and_via_objects(self):
        vuln = {
            "severity": "critical",
            "range": ">=7.0.0 <7.5.2",
            "via": [
                {
                    "source": "GHSA-abcd-efgh-ijkl",
                    "name": "axios",
                    "title": "Prototype pollution in axios",
                    "url": "https://github.com/advisories/GHSA-abcd-efgh-ijkl",
                    "range": ">=7.0.0 <7.5.2",
                },
                {"source": 12345, "title": "CVE-2024-99999"},
            ],
            "fixAvailable": {"name": "axios", "version": "7.5.2", "isSemVerMajor": False},
            "nodes": ["node_modules/axios"],
        }
        advisory = audit_crates.normalize_npm_vuln("axios", vuln, "docs")
        self.assertEqual(advisory["severity"], "CRITICAL")
        self.assertEqual(advisory["fixed_version"], "7.5.2")
        self.assertTrue(advisory["fix_available"])
        self.assertEqual(advisory["ghsa"], "GHSA-abcd-efgh-ijkl")
        self.assertEqual(advisory["cve"], "CVE-2024-99999")
        self.assertIn("Prototype pollution", advisory["description"])
        self.assertIn("GHSA-abcd-efgh-ijkl", advisory["url"])
        self.assertEqual(advisory["ecosystem"], "npm")

    def test_fix_bool_and_via_strings(self):
        vuln = {
            "severity": "high",
            "via": ["semver-regex", "pug < 3.0.0"],
            "range": "<3.0.0",
            "fixAvailable": True,
        }
        advisory = audit_crates.normalize_npm_vuln("pug", vuln, "scripts/synthetic-monitor")
        self.assertEqual(advisory["severity"], "HIGH")
        self.assertTrue(advisory["fix_available"])
        self.assertIsNone(advisory["fixed_version"])  # bool gives no concrete version
        self.assertIn("semver-regex", advisory["description"])
        self.assertEqual(advisory["id"], "npm:pug")
        self.assertTrue(audit_crates.is_actionable(advisory))

    def test_no_fix_available(self):
        vuln = {"severity": "high", "via": ["x"], "fixAvailable": False}
        advisory = audit_crates.normalize_npm_vuln("x", vuln, "docs")
        self.assertFalse(advisory["fix_available"])
        self.assertIsNone(advisory["fixed_version"])


class ManifestEditTests(unittest.TestCase):
    def test_replace_dependency_version_variants(self):
        content = (
            "[dependencies]\n"
            "time = \"0.3.20\"\n"
            "serde = { version = \"=1.0.100\", features = [\"derive\"] }\n"
            "my_crate = \"0.3.20\"\n"
        )
        updated = audit_crates.replace_dependency_version(content, "time", "0.3.20", "0.3.35")
        self.assertIsNotNone(updated)
        self.assertIn('time = "0.3.35"', updated)
        self.assertIn('serde = { version = "=1.0.100"', updated)

        updated2 = audit_crates.replace_dependency_version(content, "serde", "1.0.100", "1.0.150")
        self.assertIn('serde = { version = "=1.0.150"', updated2)

        updated3 = audit_crates.replace_dependency_version(content, "my-crate", "0.3.20", "0.3.35")
        self.assertIn('my_crate = "0.3.35"', updated3)  # hyphen/underscore equivalence

        self.assertIsNone(audit_crates.replace_dependency_version(content, "absent", "1", "2"))

    def test_replace_package_version(self):
        content = '{\n  "name": "pug",\n  "dependencies": {\n    "pug": "^2.0.0"\n  }\n}\n'
        updated = audit_crates.replace_package_version(content, "pug", "3.0.1")
        self.assertIsNotNone(updated)
        self.assertIn('"pug": "^3.0.1"', updated)
        self.assertIn('"name": "pug"', updated)  # top-level name untouched
        self.assertIsNone(audit_crates.replace_package_version(content, "missing", "1.0.0"))


class HelperTests(unittest.TestCase):
    def test_branch_name_is_safe(self):
        name = audit_crates.branch_name("time", "RUSTSEC-2024-0001")
        self.assertEqual(name, "security/bump-time-rustsec-2024-0001")

    def test_cargo_audit_json_shape_selection(self):
        # Exercise the JSON shape selection without invoking cargo by calling
        # the normalisers on both supported top-level keys.
        for key in ("vulnerabilities", "advisories"):
            payload = {key: [{"id": "RUSTSEC-X", "package": "p", "severity": "high"}]}
            entry = payload.get("vulnerabilities") or payload.get("advisories")
            self.assertEqual(audit_crates.normalize_rustsec_advisory(entry[0])["crate"], "p")


if __name__ == "__main__":
    unittest.main()
