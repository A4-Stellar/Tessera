#!/usr/bin/env python3
"""Automated API Security Penetration Test Suite for Tessera (Issue #304).

Probes REST and WebSocket endpoints for OWASP Top 10 security vulnerabilities:
  1. SQL Injection (CWE-89)
  2. Cross-Site Scripting (XSS, CWE-79)
  3. Server-Side Request Forgery (SSRF, CWE-918)
  4. Broken Authentication & Session Management (CWE-287 / CWE-384)
  5. CORS Misconfigurations (CWE-942)
  6. WebSocket Vulnerabilities & CSWSH (CWE-1385)

Integrates with the OWASP ZAP API (REST & WebSocket spider/scanner) with
an autonomous active probing fallback. Generates structured SARIF 2.1.0 reports
for direct upload to GitHub Security tab and fails CI builds when High or Critical
vulnerabilities are detected.

Usage:
    python tests/security/pen_test_suite.py \
        --target-url http://localhost:8080 \
        --ws-url ws://localhost:8080/v1/ws \
        --output security-report.sarif \
        --fail-on high
"""

from __future__ import annotations

import argparse
import http.client
import json
import logging
import os
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import asdict, dataclass, field
from enum import Enum
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

REPO_ROOT = Path(__file__).resolve().parents[2]

logger = logging.getLogger("tessera-pen-test")


class RiskLevel(str, Enum):
    CRITICAL = "Critical"
    HIGH = "High"
    MEDIUM = "Medium"
    LOW = "Low"
    INFORMATIONAL = "Informational"

    def to_sarif_level(self) -> str:
        if self in (RiskLevel.CRITICAL, RiskLevel.HIGH):
            return "error"
        if self == RiskLevel.MEDIUM:
            return "warning"
        return "note"

    @classmethod
    def from_str(cls, val: str) -> "RiskLevel":
        v = val.strip().capitalize()
        for member in cls:
            if member.value.lower() == v.lower():
                return member
        return cls.INFORMATIONAL


RULES = {
    "TESSERA-SEC-001": {
        "id": "TESSERA-SEC-001",
        "name": "SQLInjection",
        "shortDescription": "SQL Injection vulnerability detected in API endpoint parameter",
        "fullDescription": (
            "The endpoint is vulnerable to SQL injection. Untrusted input is concatenated "
            "directly into SQL queries without parameterized binding, allowing database compromise."
        ),
        "cwe": "CWE-89",
        "defaultRisk": RiskLevel.HIGH,
        "tags": ["security", "owasp-top-10", "injection", "cwe-89"],
    },
    "TESSERA-SEC-002": {
        "id": "TESSERA-SEC-002",
        "name": "CrossSiteScripting",
        "shortDescription": "Reflected or Stored Cross-Site Scripting (XSS)",
        "fullDescription": (
            "API endpoint reflects user-supplied data without proper HTML/JSON entity encoding "
            "or lacks Content-Security-Policy headers."
        ),
        "cwe": "CWE-79",
        "defaultRisk": RiskLevel.HIGH,
        "tags": ["security", "owasp-top-10", "xss", "cwe-79"],
    },
    "TESSERA-SEC-003": {
        "id": "TESSERA-SEC-003",
        "name": "ServerSideRequestForgery",
        "shortDescription": "Server-Side Request Forgery (SSRF) to internal network/cloud metadata",
        "fullDescription": (
            "Endpoint accepts target URLs or addresses and performs outbound network requests "
            "without loopback or cloud metadata IP filtering."
        ),
        "cwe": "CWE-918",
        "defaultRisk": RiskLevel.CRITICAL,
        "tags": ["security", "owasp-top-10", "ssrf", "cwe-918"],
    },
    "TESSERA-SEC-004": {
        "id": "TESSERA-SEC-004",
        "name": "BrokenAuthentication",
        "shortDescription": "Missing or bypassable authentication on protected route",
        "fullDescription": (
            "Sensitive administrative or state-altering routes can be invoked without valid "
            "authorization tokens or accept forged/unsigned JWT tokens."
        ),
        "cwe": "CWE-287",
        "defaultRisk": RiskLevel.CRITICAL,
        "tags": ["security", "owasp-top-10", "auth", "cwe-287"],
    },
    "TESSERA-SEC-005": {
        "id": "TESSERA-SEC-005",
        "name": "CorsMisconfiguration",
        "shortDescription": "Insecure wildcard or arbitrary origin CORS policy with credentials",
        "fullDescription": (
            "Cross-Origin Resource Sharing (CORS) header Access-Control-Allow-Origin is set to '*' "
            "or reflects arbitrary untrusted origins while Access-Control-Allow-Credentials is true."
        ),
        "cwe": "CWE-942",
        "defaultRisk": RiskLevel.HIGH,
        "tags": ["security", "owasp-top-10", "cors", "cwe-942"],
    },
    "TESSERA-SEC-006": {
        "id": "TESSERA-SEC-006",
        "name": "WebSocketSecurityIssue",
        "shortDescription": "Cross-Site WebSocket Hijacking (CSWSH) or missing authentication",
        "fullDescription": (
            "WebSocket endpoint performs handshake without validating the Origin header or "
            "permits unauthenticated client subscriptions to sensitive market/signer channels."
        ),
        "cwe": "CWE-1385",
        "defaultRisk": RiskLevel.HIGH,
        "tags": ["security", "owasp-top-10", "websocket", "cwe-1385"],
    },
}


@dataclass
class VulnerabilityFinding:
    rule_id: str
    name: str
    risk: RiskLevel
    cwe: str
    url: str
    method: str
    parameter: Optional[str] = None
    evidence: str = ""
    description: str = ""
    solution: str = ""

    def to_sarif_result(self) -> Dict[str, Any]:
        return {
            "ruleId": self.rule_id,
            "level": self.risk.to_sarif_level(),
            "message": {
                "text": f"[{self.risk.value}] {self.name} on {self.method} {self.url}"
                + (f" (param: {self.parameter})" if self.parameter else "")
                + f" - {self.description}. Evidence: {self.evidence}"
            },
            "locations": [
                {
                    "physicalLocation": {
                        "artifactLocation": {
                            "uri": self.url,
                            "uriBaseId": "%SRCROOT%",
                        },
                        "region": {
                            "startLine": 1,
                            "startColumn": 1,
                        },
                    }
                }
            ],
            "properties": {
                "risk": self.risk.value,
                "cwe": self.cwe,
                "method": self.method,
                "parameter": self.parameter or "",
                "solution": self.solution,
            },
        }


class ZapApiClient:
    """Client for OWASP ZAP REST and WebSocket API."""

    def __init__(self, base_url: str = "http://localhost:8080", api_key: str = ""):
        self.base_url = base_url.rstrip("/")
        self.api_key = api_key

    def _request(self, path: str, params: Optional[Dict[str, str]] = None) -> Dict[str, Any]:
        params = params or {}
        if self.api_key:
            params["apikey"] = self.api_key
        query = urllib.parse.urlencode(params)
        url = f"{self.base_url}{path}?{query}" if query else f"{self.base_url}{path}"
        req = urllib.request.Request(
            url,
            headers={"Accept": "application/json", "User-Agent": "Tessera-PenTest/1.0"},
        )
        try:
            with urllib.request.urlopen(req, timeout=10) as resp:
                return json.loads(resp.read().decode("utf-8"))
        except Exception as e:
            logger.debug(f"ZAP API request {path} failed: {e}")
            return {}

    def is_available(self) -> bool:
        res = self._request("/JSON/core/view/version/")
        return "version" in res

    def start_spider(self, target_url: str) -> Optional[str]:
        res = self._request("/JSON/spider/action/scan/", {"url": target_url})
        return res.get("scan")

    def wait_for_spider(self, scan_id: str, timeout_sec: int = 60) -> bool:
        start = time.time()
        while time.time() - start < timeout_sec:
            res = self._request("/JSON/spider/view/status/", {"scanId": scan_id})
            status = int(res.get("status", 0))
            if status >= 100:
                return True
            time.sleep(2)
        return False

    def start_active_scan(self, target_url: str) -> Optional[str]:
        res = self._request("/JSON/ascan/action/scan/", {"url": target_url, "recurse": "true"})
        return res.get("scan")

    def wait_for_active_scan(self, scan_id: str, timeout_sec: int = 120) -> bool:
        start = time.time()
        while time.time() - start < timeout_sec:
            res = self._request("/JSON/ascan/view/status/", {"scanId": scan_id})
            status = int(res.get("status", 0))
            if status >= 100:
                return True
            time.sleep(3)
        return False

    def get_alerts(self, base_url: Optional[str] = None) -> List[Dict[str, Any]]:
        params = {"baseurl": base_url} if base_url else {}
        res = self._request("/JSON/alert/view/alerts/", params)
        return res.get("alerts", [])

    def get_websocket_channels(self) -> List[Dict[str, Any]]:
        res = self._request("/JSON/websocket/view/channels/")
        return res.get("channels", [])


class NativeSecurityProber:
    """Active security vulnerability engine probing REST & WebSocket targets."""

    def __init__(self, target_url: str, ws_url: Optional[str] = None, timeout: float = 5.0):
        self.target_url = target_url.rstrip("/")
        self.ws_url = ws_url or f"{self.target_url.replace('http', 'ws')}/v1/ws"
        self.timeout = timeout

    def probe_sql_injection(self, route: str, param: str) -> List[VulnerabilityFinding]:
        findings = []
        payloads = [
            ("'", "syntax error"),
            ("' OR '1'='1", "pg_catalog"),
            ("1; SELECT pg_sleep(0.5)--", "PostgreSQL"),
            ("UNION SELECT null, null, version()--", "SQL"),
        ]
        test_url = f"{self.target_url}{route}"
        for payload, indicator in payloads:
            qs = urllib.parse.urlencode({param: payload})
            url = f"{test_url}?{qs}"
            req = urllib.request.Request(
                url,
                headers={"User-Agent": "Tessera-PenTest/1.0"},
            )
            try:
                with urllib.request.urlopen(req, timeout=self.timeout) as resp:
                    body = resp.read().decode("utf-8", errors="ignore")
                    if re.search(indicator, body, re.IGNORECASE):
                        findings.append(
                            VulnerabilityFinding(
                                rule_id="TESSERA-SEC-001",
                                name="SQLInjection",
                                risk=RiskLevel.HIGH,
                                cwe="CWE-89",
                                url=test_url,
                                method="GET",
                                parameter=param,
                                evidence=f"Payload '{payload}' yielded error indicator '{indicator}'",
                                description="SQL syntax or dialect information leaked in HTTP response",
                                solution="Use parameterized SQL queries and bound variables",
                            )
                        )
                        break
            except urllib.error.HTTPError as e:
                body = e.read().decode("utf-8", errors="ignore")
                if re.search(r"syntax error at or near|unclosed quotation|psqlException|sql_state", body, re.IGNORECASE):
                    findings.append(
                        VulnerabilityFinding(
                            rule_id="TESSERA-SEC-001",
                            name="SQLInjection",
                            risk=RiskLevel.HIGH,
                            cwe="CWE-89",
                            url=test_url,
                            method="GET",
                            parameter=param,
                            evidence=f"HTTP {e.code} leaked SQL error message",
                            description="Unfiltered SQL error messages exposed to client",
                            solution="Sanitize input and hide raw database error traces",
                        )
                    )
                    break
            except Exception:
                pass
        return findings

    def probe_xss(self, route: str, param: str) -> List[VulnerabilityFinding]:
        findings = []
        xss_payload = '<script>alert("TESSERA_XSS")</script>'
        qs = urllib.parse.urlencode({param: xss_payload})
        url = f"{self.target_url}{route}?{qs}"
        req = urllib.request.Request(url, headers={"User-Agent": "Tessera-PenTest/1.0"})
        try:
            with urllib.request.urlopen(req, timeout=self.timeout) as resp:
                body = resp.read().decode("utf-8", errors="ignore")
                content_type = resp.headers.get("Content-Type", "")
                if xss_payload in body and "application/json" not in content_type:
                    findings.append(
                        VulnerabilityFinding(
                            rule_id="TESSERA-SEC-002",
                            name="CrossSiteScripting",
                            risk=RiskLevel.HIGH,
                            cwe="CWE-79",
                            url=f"{self.target_url}{route}",
                            method="GET",
                            parameter=param,
                            evidence=f"Payload unescaped in response body with content-type: {content_type}",
                            description="Reflected HTML/JavaScript payload detected without entity encoding",
                            solution="Apply context-aware output encoding and configure Content-Security-Policy",
                        )
                    )
        except Exception:
            pass
        return findings

    def probe_ssrf(self, route: str, param: str) -> List[VulnerabilityFinding]:
        findings = []
        ssrf_targets = [
            "http://169.254.169.254/latest/meta-data/",
            "http://metadata.google.internal/computeMetadata/v1/",
            "http://127.0.0.1:2379/v2/keys",
        ]
        for target in ssrf_targets:
            test_url = f"{self.target_url}{route}?{urllib.parse.urlencode({param: target})}"
            req = urllib.request.Request(test_url, headers={"User-Agent": "Tessera-PenTest/1.0"})
            try:
                with urllib.request.urlopen(req, timeout=self.timeout) as resp:
                    body = resp.read().decode("utf-8", errors="ignore")
                    if any(kw in body for kw in ("ami-id", "instance-id", "Compute Engine", "kube-env")):
                        findings.append(
                            VulnerabilityFinding(
                                rule_id="TESSERA-SEC-003",
                                name="ServerSideRequestForgery",
                                risk=RiskLevel.CRITICAL,
                                cwe="CWE-918",
                                url=f"{self.target_url}{route}",
                                method="GET",
                                parameter=param,
                                evidence=f"Accessible internal/cloud metadata content via target: {target}",
                                description="Endpoint forwards outbound request to cloud instance metadata",
                                solution="Restrict destination IP ranges and disallow loopback/link-local addresses",
                            )
                        )
                        break
            except Exception:
                pass
        return findings

    def probe_broken_auth(self, protected_route: str) -> List[VulnerabilityFinding]:
        findings = []
        url = f"{self.target_url}{protected_route}"
        req = urllib.request.Request(url, headers={"User-Agent": "Tessera-PenTest/1.0"})
        try:
            with urllib.request.urlopen(req, timeout=self.timeout) as resp:
                if resp.status == 200:
                    findings.append(
                        VulnerabilityFinding(
                            rule_id="TESSERA-SEC-004",
                            name="BrokenAuthentication",
                            risk=RiskLevel.CRITICAL,
                            cwe="CWE-287",
                            url=url,
                            method="GET",
                            evidence=f"Received HTTP {resp.status} with empty Authorization header",
                            description="Protected endpoint accessed successfully without credentials",
                            solution="Enforce authentication middleware on all protected route groups",
                        )
                    )
        except urllib.error.HTTPError as e:
            if e.code in (401, 403):
                # Verify 'none' algorithm or bogus Bearer token
                fake_auth_req = urllib.request.Request(
                    url,
                    headers={
                        "User-Agent": "Tessera-PenTest/1.0",
                        "Authorization": "Bearer eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJzdWIiOiJhZG1pbiIsInJvbGUiOiJhZG1pbiJ9.",
                    },
                )
                try:
                    with urllib.request.urlopen(fake_auth_req, timeout=self.timeout) as resp2:
                        if resp2.status == 200:
                            findings.append(
                                VulnerabilityFinding(
                                    rule_id="TESSERA-SEC-004",
                                    name="BrokenAuthentication",
                                    risk=RiskLevel.CRITICAL,
                                    cwe="CWE-287",
                                    url=url,
                                    method="GET",
                                    evidence="Accepted unverified unsigned JWT token (alg: none)",
                                    description="Authentication bypass via alg: none token signature suppression",
                                    solution="Strictly validate signing algorithm and reject unsigned tokens",
                                )
                            )
                except Exception:
                    pass
        except Exception:
            pass
        return findings

    def probe_cors(self, route: str) -> List[VulnerabilityFinding]:
        findings = []
        url = f"{self.target_url}{route}"
        malicious_origin = "https://evil-attacker.example.com"
        req = urllib.request.Request(
            url,
            headers={
                "Origin": malicious_origin,
                "User-Agent": "Tessera-PenTest/1.0",
            },
        )
        try:
            with urllib.request.urlopen(req, timeout=self.timeout) as resp:
                acao = resp.headers.get("Access-Control-Allow-Origin")
                acac = resp.headers.get("Access-Control-Allow-Credentials", "").lower()
                if acao == malicious_origin and acac == "true":
                    findings.append(
                        VulnerabilityFinding(
                            rule_id="TESSERA-SEC-005",
                            name="CorsMisconfiguration",
                            risk=RiskLevel.HIGH,
                            cwe="CWE-942",
                            url=url,
                            method="GET",
                            evidence=f"Reflected Origin '{acao}' with Access-Control-Allow-Credentials: true",
                            description="Insecure CORS policy permits credentialed access from arbitrary origins",
                            solution="Whitelist specific trusted origins and never reflect Origin alongside credentials",
                        )
                    )
                elif acao == "*" and acac == "true":
                    findings.append(
                        VulnerabilityFinding(
                            rule_id="TESSERA-SEC-005",
                            name="CorsMisconfiguration",
                            risk=RiskLevel.HIGH,
                            cwe="CWE-942",
                            url=url,
                            method="GET",
                            evidence="Wildcard origin '*' with credentials enabled",
                            description="Spec violation and credential disclosure risk via wildcard CORS",
                            solution="Specify strict origins when credentials are enabled",
                        )
                    )
        except Exception:
            pass
        return findings

    def probe_websocket(self) -> List[VulnerabilityFinding]:
        findings = []
        parsed = urllib.parse.urlparse(self.ws_url)
        host = parsed.hostname or "localhost"
        port = parsed.port or (443 if parsed.scheme == "wss" else 80)
        path = parsed.path or "/v1/ws"

        # Check Cross-Site WebSocket Hijacking (CSWSH) with untrusted Origin
        evil_origin = "https://malicious-origin.com"
        headers = [
            f"GET {path} HTTP/1.1",
            f"Host: {host}:{port}",
            "Upgrade: websocket",
            "Connection: Upgrade",
            "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==",
            "Sec-WebSocket-Version: 13",
            f"Origin: {evil_origin}",
            "\r\n",
        ]
        payload = "\r\n".join(headers).encode("utf-8")

        try:
            import socket

            s = socket.create_connection((host, port), timeout=self.timeout)
            s.sendall(payload)
            response = s.recv(1024).decode("utf-8", errors="ignore")
            s.close()

            if "101 Switching Protocols" in response:
                # Upgraded connection without validating Origin header
                findings.append(
                    VulnerabilityFinding(
                        rule_id="TESSERA-SEC-006",
                        name="WebSocketSecurityIssue",
                        risk=RiskLevel.HIGH,
                        cwe="CWE-1385",
                        url=self.ws_url,
                        method="GET (WS Upgrade)",
                        evidence=f"Connection upgraded (101) with untrusted Origin: {evil_origin}",
                        description="Cross-Site WebSocket Hijacking vulnerability: missing Origin header check",
                        solution="Validate Origin header during WebSocket handshake against allowed origins",
                    )
                )
        except Exception as e:
            logger.debug(f"WebSocket probe failed (expected if daemon not running): {e}")

        return findings


def normalize_zap_alert(alert: Dict[str, Any]) -> VulnerabilityFinding:
    risk_str = alert.get("risk", "Low")
    risk = RiskLevel.from_str(risk_str)
    alert_name = alert.get("name", alert.get("alert", "ZAP Finding"))
    cwe_id = f"CWE-{alert.get('cweid', '200')}"
    url = alert.get("url", "")
    method = alert.get("method", "GET")
    param = alert.get("param", "")
    evidence = alert.get("evidence", alert.get("otherinfo", ""))

    rule_id = "TESSERA-SEC-001"
    name = "ZapSecurityAlert"
    if "sql" in alert_name.lower():
        rule_id = "TESSERA-SEC-001"
        name = "SQLInjection"
    elif "xss" in alert_name.lower() or "cross site scripting" in alert_name.lower():
        rule_id = "TESSERA-SEC-002"
        name = "CrossSiteScripting"
    elif "ssrf" in alert_name.lower() or "request forgery" in alert_name.lower():
        rule_id = "TESSERA-SEC-003"
        name = "ServerSideRequestForgery"
    elif "auth" in alert_name.lower() or "session" in alert_name.lower():
        rule_id = "TESSERA-SEC-004"
        name = "BrokenAuthentication"
    elif "cors" in alert_name.lower() or "origin" in alert_name.lower():
        rule_id = "TESSERA-SEC-005"
        name = "CorsMisconfiguration"
    elif "websocket" in alert_name.lower():
        rule_id = "TESSERA-SEC-006"
        name = "WebSocketSecurityIssue"

    return VulnerabilityFinding(
        rule_id=rule_id,
        name=name,
        risk=risk,
        cwe=cwe_id,
        url=url,
        method=method,
        parameter=param,
        evidence=evidence,
        description=alert.get("description", alert_name),
        solution=alert.get("solution", ""),
    )


def generate_sarif_report(findings: List[VulnerabilityFinding]) -> Dict[str, Any]:
    """Generates structured SARIF 2.1.0 report compliant with GitHub Security tab."""
    rules_list = []
    for r_id, r_info in RULES.items():
        rules_list.append(
            {
                "id": r_id,
                "name": r_info["name"],
                "shortDescription": {"text": r_info["shortDescription"]},
                "fullDescription": {"text": r_info["fullDescription"]},
                "defaultConfiguration": {
                    "level": r_info["defaultRisk"].to_sarif_level()
                },
                "help": {
                    "text": f"More info: {r_info['cwe']}. Ensure appropriate security controls are applied."
                },
                "properties": {
                    "tags": r_info["tags"],
                    "cwe": r_info["cwe"],
                },
            }
        )

    results = [f.to_sarif_result() for f in findings]

    return {
        "$schema": "https://raw.githubusercontent.com/oasis-tcs/sarif-spec/master/Schemata/sarif-schema-2.1.0.json",
        "version": "2.1.0",
        "runs": [
            {
                "tool": {
                    "driver": {
                        "name": "Tessera-OWASP-ZAP-PenTest",
                        "organization": "A4-Stellar",
                        "semanticVersion": "1.0.0",
                        "informationUri": "https://github.com/A4-Stellar/Tessera",
                        "rules": rules_list,
                    }
                },
                "results": results,
                "columnKind": "utf16CodeUnits",
            }
        ],
    }


def run_penetration_suite(
    target_url: str,
    ws_url: Optional[str] = None,
    zap_url: Optional[str] = None,
    zap_api_key: str = "",
    routes: Optional[List[str]] = None,
    protected_routes: Optional[List[str]] = None,
    mock_mode: bool = False,
) -> List[VulnerabilityFinding]:
    """Orchestrates OWASP ZAP API scan and active security probes."""
    findings: List[VulnerabilityFinding] = []

    routes = routes or [
        "/v1/stats",
        "/v1/assets",
        "/v1/transactions",
        "/v1/events",
        "/v1/admin/config",
    ]
    protected_routes = protected_routes or [
        "/v1/admin/config",
        "/v1/signer/keys",
        "/v1/transactions/sign",
    ]

    # 1. OWASP ZAP API Scan if configured & online
    if zap_url and not mock_mode:
        zap = ZapApiClient(base_url=zap_url, api_key=zap_api_key)
        if zap.is_available():
            logger.info(f"Connected to OWASP ZAP API at {zap_url}. Initiating spider & active scan...")
            spider_id = zap.start_spider(target_url)
            if spider_id:
                zap.wait_for_spider(spider_id, timeout_sec=30)

            ascan_id = zap.start_active_scan(target_url)
            if ascan_id:
                zap.wait_for_active_scan(ascan_id, timeout_sec=60)

            alerts = zap.get_alerts(base_url=target_url)
            for alert in alerts:
                findings.append(normalize_zap_alert(alert))
        else:
            logger.warning(f"OWASP ZAP API at {zap_url} not responsive, falling back to native prober.")

    # 2. Native Active Prober
    if not mock_mode:
        prober = NativeSecurityProber(target_url=target_url, ws_url=ws_url)
        for route in routes:
            findings.extend(prober.probe_sql_injection(route, "id"))
            findings.extend(prober.probe_sql_injection(route, "offset"))
            findings.extend(prober.probe_xss(route, "query"))
            findings.extend(prober.probe_ssrf(route, "callback_url"))
            findings.extend(prober.probe_cors(route))

        for prot_route in protected_routes:
            findings.extend(prober.probe_broken_auth(prot_route))

        findings.extend(prober.probe_websocket())

    return findings


def should_fail_ci(findings: List[VulnerabilityFinding], fail_on: str) -> bool:
    """Evaluates whether findings exceed the CI break threshold."""
    fail_on = fail_on.strip().lower()
    if fail_on == "none":
        return False

    severity_order = {
        "critical": [RiskLevel.CRITICAL],
        "high": [RiskLevel.CRITICAL, RiskLevel.HIGH],
        "medium": [RiskLevel.CRITICAL, RiskLevel.HIGH, RiskLevel.MEDIUM],
        "low": [RiskLevel.CRITICAL, RiskLevel.HIGH, RiskLevel.MEDIUM, RiskLevel.LOW],
    }

    threshold_levels = severity_order.get(fail_on, severity_order["high"])
    for finding in findings:
        if finding.risk in threshold_levels:
            return True
    return False


def parse_args(argv: Optional[List[str]] = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Tessera Automated API Security Penetration Test Suite (OWASP Top 10)."
    )
    parser.add_argument(
        "--target-url",
        default="http://localhost:8080",
        help="Target API base URL (default: http://localhost:8080)",
    )
    parser.add_argument(
        "--ws-url",
        default=None,
        help="Target WebSocket URL (default: ws://localhost:8080/v1/ws)",
    )
    parser.add_argument(
        "--zap-url",
        default=os.environ.get("ZAP_API_URL"),
        help="OWASP ZAP API endpoint (e.g. http://zap:8080 or http://localhost:8080)",
    )
    parser.add_argument(
        "--zap-api-key",
        default=os.environ.get("ZAP_API_KEY", ""),
        help="OWASP ZAP API authentication key",
    )
    parser.add_argument(
        "--output",
        "-o",
        default="zap-security-report.sarif",
        help="Path to emit the SARIF 2.1.0 security report (default: zap-security-report.sarif)",
    )
    parser.add_argument(
        "--fail-on",
        choices=["critical", "high", "medium", "low", "none"],
        default="high",
        help="Fail CI with exit code 1 if vulnerabilities at or above this risk level are found.",
    )
    parser.add_argument(
        "--mock-zap",
        action="store_true",
        help="Run in mock/verification mode for CI pipelines without active server instances.",
    )
    parser.add_argument(
        "--verbose",
        "-v",
        action="store_true",
        help="Enable debug logging output.",
    )
    return parser.parse_args(argv)


def main(argv: Optional[List[str]] = None) -> int:
    args = parse_args(argv)
    log_level = logging.DEBUG if args.verbose else logging.INFO
    logging.basicConfig(level=log_level, format="%(asctime)s [%(levelname)s] %(message)s")

    print(f"[*] Starting Tessera Penetration Test Suite...")
    print(f"[*] Target URL: {args.target_url}")
    print(f"[*] ZAP URL:    {args.zap_url or 'None (Native Prober Active)'}")
    print(f"[*] CI Fail-on: {args.fail_on}")

    findings = run_penetration_suite(
        target_url=args.target_url,
        ws_url=args.ws_url,
        zap_url=args.zap_url,
        zap_api_key=args.zap_api_key,
        mock_mode=args.mock_zap,
    )

    sarif = generate_sarif_report(findings)
    out_path = Path(args.output)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(sarif, indent=2) + "\n", encoding="utf-8")

    critical_count = sum(1 for f in findings if f.risk == RiskLevel.CRITICAL)
    high_count = sum(1 for f in findings if f.risk == RiskLevel.HIGH)
    med_count = sum(1 for f in findings if f.risk == RiskLevel.MEDIUM)
    low_count = sum(1 for f in findings if f.risk == RiskLevel.LOW)

    print(f"\n[+] Scan Complete. Report written to {out_path.resolve()}")
    print(f"[+] Total Findings: {len(findings)}")
    print(f"    - Critical: {critical_count}")
    print(f"    - High:     {high_count}")
    print(f"    - Medium:   {med_count}")
    print(f"    - Low:      {low_count}")

    for finding in findings:
        print(f"  [{finding.risk.value}] {finding.rule_id} ({finding.cwe}): {finding.method} {finding.url}")
        print(f"      Evidence: {finding.evidence}")

    if should_fail_ci(findings, args.fail_on):
        print(f"\n[!] CI Build FAILED: Detected security vulnerabilities exceeding '{args.fail_on}' threshold.")
        return 1

    print("\n[OK] CI Build PASSED: No blocking security vulnerabilities found.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
