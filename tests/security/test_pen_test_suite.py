#!/usr/bin/env python3
"""Unit and integration tests for pen_test_suite.py (Issue #304)."""

import json
import tempfile
import unittest
import urllib.parse
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
import threading

from tests.security.pen_test_suite import (
    RiskLevel,
    VulnerabilityFinding,
    generate_sarif_report,
    normalize_zap_alert,
    should_fail_ci,
    NativeSecurityProber,
    main,
)


class MockVulnerableHandler(BaseHTTPRequestHandler):
    def log_message(self, format, *args):
        pass  # Suppress server logs during test run

    def do_GET(self):
        path_unquoted = urllib.parse.unquote(self.path)
        # 1. SQL Injection simulation on /v1/assets?id=...
        if self.path.startswith("/v1/assets"):
            if "syntax error" in path_unquoted or "'" in path_unquoted:
                self.send_response(500)
                self.send_header("Content-Type", "text/plain")
                self.end_headers()
                self.wfile.write(b"ERROR: syntax error at or near 'SELECT' in psqlException")
                return
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(b'{"assets": []}')
            return

        # 2. XSS simulation on /v1/search
        if self.path.startswith("/v1/search"):
            self.send_response(200)
            self.send_header("Content-Type", "text/html")
            self.end_headers()
            # Reflect query directly without escaping
            query = path_unquoted.split("query=")[-1] if "query=" in path_unquoted else ""
            self.wfile.write(f"<html><body>Results for {query}</body></html>".encode("utf-8"))
            return

        # 3. CORS misconfiguration on /v1/stats
        if self.path.startswith("/v1/stats"):
            origin = self.headers.get("Origin", "")
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            if origin:
                self.send_header("Access-Control-Allow-Origin", origin)
                self.send_header("Access-Control-Allow-Credentials", "true")
            self.end_headers()
            self.wfile.write(b'{"status": "ok"}')
            return

        # 4. Broken Auth on /v1/admin/config
        if self.path.startswith("/v1/admin"):
            auth = self.headers.get("Authorization", "")
            if not auth:
                self.send_response(401)
                self.end_headers()
                return
            if "eyJhbGciOiJub25lI" in auth:  # alg: none
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.end_headers()
                self.wfile.write(b'{"admin_config": "secret"}')
                return
            self.send_response(403)
            self.end_headers()
            return

        self.send_response(404)
        self.end_headers()


class PenTestSuiteTestCase(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = HTTPServer(("127.0.0.1", 0), MockVulnerableHandler)
        cls.port = cls.server.server_port
        cls.server_thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.server_thread.start()

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()

    def test_sarif_generation_structure(self):
        finding = VulnerabilityFinding(
            rule_id="TESSERA-SEC-001",
            name="SQLInjection",
            risk=RiskLevel.HIGH,
            cwe="CWE-89",
            url="http://localhost:8080/v1/assets",
            method="GET",
            parameter="id",
            evidence="syntax error at or near",
            description="SQL query injection",
            solution="Use bound parameters",
        )
        sarif = generate_sarif_report([finding])
        self.assertEqual(sarif["version"], "2.1.0")
        self.assertEqual(len(sarif["runs"]), 1)
        run = sarif["runs"][0]
        self.assertEqual(run["tool"]["driver"]["name"], "Tessera-OWASP-ZAP-PenTest")
        self.assertEqual(len(run["results"]), 1)
        res = run["results"][0]
        self.assertEqual(res["ruleId"], "TESSERA-SEC-001")
        self.assertEqual(res["level"], "error")
        self.assertEqual(res["properties"]["risk"], "High")

    def test_normalize_zap_alerts(self):
        zap_alert = {
            "alert": "Cross Site Scripting (Reflected)",
            "risk": "High",
            "cweid": "79",
            "url": "http://example.com/search?q=1",
            "method": "GET",
            "param": "q",
            "evidence": "<script>alert(1)</script>",
            "description": "Reflected XSS",
            "solution": "Sanitize inputs",
        }
        finding = normalize_zap_alert(zap_alert)
        self.assertEqual(finding.rule_id, "TESSERA-SEC-002")
        self.assertEqual(finding.risk, RiskLevel.HIGH)
        self.assertEqual(finding.cwe, "CWE-79")

    def test_should_fail_ci_policy(self):
        critical_finding = VulnerabilityFinding(
            rule_id="TESSERA-SEC-003",
            name="SSRF",
            risk=RiskLevel.CRITICAL,
            cwe="CWE-918",
            url="http://test/api",
            method="GET",
        )
        low_finding = VulnerabilityFinding(
            rule_id="TESSERA-SEC-005",
            name="CORS",
            risk=RiskLevel.LOW,
            cwe="CWE-942",
            url="http://test/api",
            method="GET",
        )

        self.assertTrue(should_fail_ci([critical_finding], "critical"))
        self.assertTrue(should_fail_ci([critical_finding], "high"))
        self.assertFalse(should_fail_ci([low_finding], "high"))
        self.assertFalse(should_fail_ci([critical_finding], "none"))

    def test_native_prober_vulnerability_detection(self):
        prober = NativeSecurityProber(target_url=f"http://127.0.0.1:{self.port}")

        # 1. Probe SQLi
        sqli_findings = prober.probe_sql_injection("/v1/assets", "id")
        self.assertTrue(len(sqli_findings) > 0)
        self.assertEqual(sqli_findings[0].rule_id, "TESSERA-SEC-001")

        # 2. Probe XSS
        xss_findings = prober.probe_xss("/v1/search", "query")
        self.assertTrue(len(xss_findings) > 0)
        self.assertEqual(xss_findings[0].rule_id, "TESSERA-SEC-002")

        # 3. Probe CORS
        cors_findings = prober.probe_cors("/v1/stats")
        self.assertTrue(len(cors_findings) > 0)
        self.assertEqual(cors_findings[0].rule_id, "TESSERA-SEC-005")

        # 4. Probe Broken Auth with alg:none
        auth_findings = prober.probe_broken_auth("/v1/admin/config")
        self.assertTrue(len(auth_findings) > 0)
        self.assertEqual(auth_findings[0].rule_id, "TESSERA-SEC-004")

    def test_cli_execution_and_sarif_export(self):
        with tempfile.NamedTemporaryFile(suffix=".sarif", delete=False) as tmp:
            tmp_path = tmp.name

        exit_code = main(
            [
                "--target-url",
                f"http://127.0.0.1:{self.port}",
                "--output",
                tmp_path,
                "--fail-on",
                "critical",
            ]
        )
        self.assertEqual(exit_code, 1)  # Because broken auth is Critical

        sarif_data = json.loads(Path(tmp_path).read_text(encoding="utf-8"))
        self.assertEqual(sarif_data["version"], "2.1.0")
        self.assertTrue(len(sarif_data["runs"][0]["results"]) > 0)


if __name__ == "__main__":
    unittest.main()
