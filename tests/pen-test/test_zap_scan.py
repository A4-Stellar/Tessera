#!/usr/bin/env python3
"""Unit tests for zap_scan.py (Issue #214)."""

import importlib
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import MagicMock, patch

# The directory is "pen-test" (hyphenated), which isn't a valid Python
# identifier.  Load the module by file path so the tests work regardless.
_zap_scan_path = Path(__file__).resolve().parent / "zap_scan.py"
_spec = importlib.util.spec_from_file_location("zap_scan", _zap_scan_path)
zap_scan = importlib.util.module_from_spec(_spec)
sys.modules["zap_scan"] = zap_scan
_spec.loader.exec_module(zap_scan)

Finding = zap_scan.Finding
RiskLevel = zap_scan.RiskLevel
ZapClient = zap_scan.ZapClient
_classify_alert = zap_scan._classify_alert
generate_sarif = zap_scan.generate_sarif
should_fail = zap_scan.should_fail


class TestRiskLevel(unittest.TestCase):
    def test_from_zap_numeric(self):
        self.assertEqual(RiskLevel.from_zap("3"), RiskLevel.HIGH)
        self.assertEqual(RiskLevel.from_zap("2"), RiskLevel.MEDIUM)
        self.assertEqual(RiskLevel.from_zap("1"), RiskLevel.LOW)
        self.assertEqual(RiskLevel.from_zap("0"), RiskLevel.INFO)

    def test_from_zap_string(self):
        self.assertEqual(RiskLevel.from_zap("High"), RiskLevel.HIGH)
        self.assertEqual(RiskLevel.from_zap("medium"), RiskLevel.MEDIUM)

    def test_from_zap_unknown(self):
        self.assertEqual(RiskLevel.from_zap("unknown"), RiskLevel.INFO)

    def test_sarif_levels(self):
        self.assertEqual(RiskLevel.CRITICAL.sarif_level(), "error")
        self.assertEqual(RiskLevel.HIGH.sarif_level(), "error")
        self.assertEqual(RiskLevel.MEDIUM.sarif_level(), "warning")
        self.assertEqual(RiskLevel.LOW.sarif_level(), "note")
        self.assertEqual(RiskLevel.INFO.sarif_level(), "note")


class TestClassifyAlert(unittest.TestCase):
    def test_sqli_alert(self):
        alert = {
            "pluginId": "40018",
            "name": "SQL Injection",
            "risk": "3",
            "cweid": "89",
            "url": "http://localhost:8080/assets?id=1",
            "method": "GET",
            "param": "id",
            "evidence": "syntax error",
            "description": "SQL injection via parameter",
            "solution": "Use parameterized queries",
        }
        f = _classify_alert(alert)
        self.assertEqual(f.rule_id, "TESSERA-ZAP-001")
        self.assertEqual(f.name, "SQL Injection")
        self.assertEqual(f.risk, RiskLevel.HIGH)
        self.assertEqual(f.cwe, "CWE-89")

    def test_xss_alert(self):
        alert = {
            "pluginId": "40012",
            "risk": "3",
            "url": "http://localhost:8080/search",
            "method": "GET",
        }
        f = _classify_alert(alert)
        self.assertEqual(f.rule_id, "TESSERA-ZAP-002")
        self.assertEqual(f.name, "Cross-Site Scripting (XSS)")

    def test_cors_alert(self):
        alert = {
            "pluginId": "40040",
            "risk": "2",
            "url": "http://localhost:8080/stats",
            "method": "GET",
        }
        f = _classify_alert(alert)
        self.assertEqual(f.rule_id, "TESSERA-ZAP-005")
        self.assertEqual(f.name, "CORS Misconfiguration")

    def test_unknown_plugin(self):
        alert = {
            "pluginId": "99999",
            "name": "Some New Check",
            "risk": "1",
            "cweid": "200",
            "url": "http://localhost:8080/other",
            "method": "GET",
        }
        f = _classify_alert(alert)
        self.assertEqual(f.rule_id, "TESSERA-ZAP-99999")
        self.assertEqual(f.risk, RiskLevel.LOW)


class TestSarifGeneration(unittest.TestCase):
    def test_empty_report(self):
        sarif = generate_sarif([])
        self.assertEqual(sarif["version"], "2.1.0")
        self.assertEqual(len(sarif["runs"]), 1)
        self.assertEqual(sarif["runs"][0]["results"], [])
        self.assertTrue(len(sarif["runs"][0]["tool"]["driver"]["rules"]) > 0)

    def test_report_with_findings(self):
        findings = [
            Finding(
                rule_id="TESSERA-ZAP-001",
                name="SQL Injection",
                risk=RiskLevel.HIGH,
                cwe="CWE-89",
                url="http://localhost:8080/assets",
                method="GET",
                param="id",
                evidence="error in SQL syntax",
                description="SQL injection detected",
                solution="Use parameterized queries",
            ),
        ]
        sarif = generate_sarif(findings)
        results = sarif["runs"][0]["results"]
        self.assertEqual(len(results), 1)
        self.assertEqual(results[0]["ruleId"], "TESSERA-ZAP-001")
        self.assertEqual(results[0]["level"], "error")

    def test_sarif_valid_json(self):
        findings = [
            Finding(
                rule_id="TESSERA-ZAP-003",
                name="SSRF",
                risk=RiskLevel.CRITICAL,
                cwe="CWE-918",
                url="http://localhost/api",
                method="POST",
            ),
        ]
        sarif = generate_sarif(findings)
        serialized = json.dumps(sarif)
        self.assertIsInstance(json.loads(serialized), dict)


class TestShouldFail(unittest.TestCase):
    def _finding(self, risk: RiskLevel) -> Finding:
        return Finding(
            rule_id="TEST",
            name="Test",
            risk=risk,
            cwe="CWE-0",
            url="http://test",
            method="GET",
        )

    def test_critical_blocks_on_high(self):
        self.assertTrue(should_fail([self._finding(RiskLevel.CRITICAL)], "high"))

    def test_high_blocks_on_high(self):
        self.assertTrue(should_fail([self._finding(RiskLevel.HIGH)], "high"))

    def test_medium_passes_on_high(self):
        self.assertFalse(should_fail([self._finding(RiskLevel.MEDIUM)], "high"))

    def test_none_never_fails(self):
        self.assertFalse(should_fail([self._finding(RiskLevel.CRITICAL)], "none"))

    def test_empty_findings_pass(self):
        self.assertFalse(should_fail([], "high"))

    def test_low_blocks_on_low(self):
        self.assertTrue(should_fail([self._finding(RiskLevel.LOW)], "low"))


class TestZapClientConnectivity(unittest.TestCase):
    def test_is_available_returns_false_on_connection_error(self):
        zap = ZapClient("http://127.0.0.1:1")
        self.assertFalse(zap.is_available())


if __name__ == "__main__":
    unittest.main()
