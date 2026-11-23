#!/usr/bin/env python3
"""Audit Rust/npm lockfiles and (optionally) open automated remediation PRs.

Plain mode (default) preserves the original behaviour of this script: scan every
``Cargo.lock`` with ``cargo audit``, report unaudited unsafe code, and return a
non-zero exit code when a problem is found.

Bot mode (``--bot``) turns the script into a dependency-remediation bot:

* HIGH/CRITICAL advisories with a known fix get a pull request that bumps the
  manifest version and (for Rust) denies the vulnerable crate version in
  ``deny.toml`` via the Git Data API.
* HIGH/CRITICAL advisories with no fix get a ``security``-labelled issue that is
  assigned to the maintainer.
* Open issues/PRs with the advisory id in the title are skipped (dedupe).

Environment variables (CLI flags take precedence):

* ``GITHUB_REPOSITORY`` -- ``OWNER/REPO`` used for API calls (``--repo``).
* ``GITHUB_TOKEN`` -- token used for the GraphQL/REST calls (``--token``).
* ``SECURITY_MAINTAINER`` -- assignee for unfixed-advisory issues
  (``--maintainer``); defaults to the repository owner.

Required GitHub token permissions for ``--bot`` (see .github/workflows/security.yml):

* ``contents: write`` -- create blobs, trees, commits and branches for fix PRs.
* ``pull-requests: write`` -- open the fix PRs.
* ``issues: write`` -- open issues and apply the ``security`` label/assignee.

Without a token (and outside ``--dry-run``) all GitHub writes are disabled; the
script still scans locally and prints the actions it *would* take. Use
``--dry-run`` to print planned actions without any network access at all.
All diagnostic and dry-run output goes to stderr; stdout is reserved for the
final JSON summary so it can be parsed by CI.
"""
from __future__ import annotations

import argparse
import difflib
import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

REST_BASE = "https://api.github.com"
GRAPHQL_URL = "https://api.github.com/graphql"
DEFAULT_NPM_DIRS = ("docs", "scripts/synthetic-monitor", "infrastructure/cloudflare")
DEFAULT_BASE = "main"
ACTIONABLE = {"HIGH", "CRITICAL"}
SEVERITY_RANK = {"INFO": 0, "LOW": 1, "MODERATE": 2, "HIGH": 3, "CRITICAL": 4}
DENY_MARKER = "# machine-managed by audit_crates.py --bot: DENY known-vulnerable crate versions"
HTTP_TIMEOUT = 20
USER_AGENT = "tessera-security-bot/1.0"


# --------------------------------------------------------------------------- #
# Small helpers
# --------------------------------------------------------------------------- #
def _warn(message: str) -> None:
    print(f"audit_crates: {message}", file=sys.stderr)


def _info(message: str, dry_run: bool = False) -> None:
    prefix = "[dry-run] " if dry_run else ""
    print(f"audit_crates: {prefix}{message}", file=sys.stderr)


def normalize_severity(value: Any) -> str:
    """Normalise a severity from cargo-audit / GHSA / npm into upper case."""
    text = str(value or "INFO").strip().upper()
    aliases = {
        "MODERATE": "MODERATE",
        "MEDIUM": "MODERATE",
        "NONE": "INFO",
        "UNKNOWN": "INFO",
    }
    text = aliases.get(text, text)
    return text if text in SEVERITY_RANK else "INFO"


def is_actionable(advisory: dict[str, Any]) -> bool:
    """Only HIGH and CRITICAL advisories trigger automated remediation."""
    return normalize_severity(advisory.get("severity")) in ACTIONABLE


def max_severity(*values: Any) -> str:
    """Return the most severe of the supplied severity strings."""
    normalized = [normalize_severity(v) for v in values if v]
    return max(normalized, key=lambda s: SEVERITY_RANK[s]) if normalized else "INFO"


def extract_version(value: Any) -> str | None:
    """Pull the first semantic-ish version out of a string/list/dict value.

    Handles cargo-audit patched ranges such as ``>=0.2.5, <1.0.0`` and GHSA
    ``firstPatchedVersion`` strings.
    """
    if value is None:
        return None
    if isinstance(value, dict):
        for key in ("version", "identifier", "patched", "patched_versions", "value"):
            if key in value:
                found = extract_version(value[key])
                if found:
                    return found
        return None
    if isinstance(value, (list, tuple)):
        for item in value:
            found = extract_version(item)
            if found:
                return found
        return None
    match = re.search(r"\d+(?:\.\d+){0,3}(?:[-+][0-9A-Za-z.\-]+)?", str(value))
    return match.group(0) if match else None


def advisory_link(advisory: dict[str, Any]) -> str:
    """NVD link when a CVE is known, otherwise the advisory's own URL."""
    cve = advisory.get("cve")
    if cve:
        return f"https://nvd.nist.gov/vuln/detail/{cve}"
    if advisory.get("url"):
        return str(advisory["url"])
    if advisory.get("ghsa"):
        return f"https://github.com/advisories/{advisory['ghsa']}"
    return ""


def branch_name(crate: str, advisory_id: str) -> str:
    """Build a safe git branch name for a fix PR."""
    raw = f"security/bump-{crate}-{advisory_id}".lower()
    return re.sub(r"[^a-z0-9._/-]+", "-", raw).strip("-") or "security/bump"


# --------------------------------------------------------------------------- #
# Advisory normalisation
# --------------------------------------------------------------------------- #
def normalize_rustsec_advisory(entry: Any, lock: Path | None = None) -> dict[str, Any]:
    """Normalise one cargo-audit ``vulnerabilities``/``advisories`` entry.

    cargo-audit has shipped two JSON shapes: a flat advisory object and a
    wrapper containing a nested ``advisory`` object. Both are accepted, as are
    ``id``/``advisory_id``, ``crate``/``package`` and
    ``patched_versions``/``patched``/``fix`` keys.
    """
    if not isinstance(entry, dict):
        entry = {"id": str(entry)}

    data: dict[str, Any] = dict(entry)
    nested = entry.get("advisory")
    if isinstance(nested, dict):
        data = dict(nested)
        # Outer fields (package/version/id) win over the nested advisory copy.
        for key, value in entry.items():
            if key != "advisory" and value not in (None, "", [], {}):
                data[key] = value

    advisory_id = data.get("id") or data.get("advisory_id") or "unknown"
    crate = data.get("crate") or data.get("package") or ""
    if isinstance(crate, dict):
        crate = crate.get("name", "")

    description = data.get("description") or data.get("title") or ""
    if data.get("title") and data.get("description"):
        description = f"{data['title']} — {data['description']}"

    patched = data.get("patched_versions")
    if patched is None:
        patched = data.get("patched")
    if patched is None and isinstance(data.get("fix"), dict):
        fix = data["fix"]
        patched = fix.get("versions") or fix.get("version") or fix.get("patched")
    fixed_version = extract_version(patched)

    vulnerable_version = extract_version(
        data.get("version") or data.get("vulnerable_version") or data.get("affected")
    )
    if not vulnerable_version and lock is not None:
        vulnerable_version = version_from_lock(lock, str(crate))

    aliases = data.get("aliases") or []
    if isinstance(aliases, str):
        aliases = [aliases]
    cve = data.get("cve")
    ghsa = data.get("ghsa")
    for alias in aliases:
        text = str(alias)
        if not cve and text.upper().startswith("CVE-"):
            cve = text
        if not ghsa and text.upper().startswith("GHSA-"):
            ghsa = text

    return {
        "id": str(advisory_id),
        "crate": str(crate),
        "severity": normalize_severity(data.get("severity")),
        "description": str(description),
        "fixed_version": fixed_version,
        "vulnerable_version": vulnerable_version,
        "cve": cve,
        "ghsa": ghsa,
        "url": data.get("url") or data.get("link") or "",
        "aliases": [str(a) for a in aliases],
        "ecosystem": "rust",
        "source": "rustsec",
    }


def normalize_ghsa_advisory(node: Any, crate_hint: str | None = None) -> dict[str, Any] | None:
    """Normalise a GitHub ``securityAdvisory`` GraphQL node."""
    if not isinstance(node, dict):
        return None
    vulnerabilities = (node.get("vulnerabilities") or {}).get("nodes") or []
    if isinstance(vulnerabilities, dict):
        vulnerabilities = [vulnerabilities]

    fixed_version = None
    vulnerable_version = None
    crate = crate_hint or ""
    for vuln in vulnerabilities:
        if not isinstance(vuln, dict):
            continue
        package = vuln.get("package") or {}
        name = package.get("name") if isinstance(package, dict) else package
        if crate_hint and name and str(name).lower().replace("_", "-") != str(crate_hint).lower().replace("_", "-"):
            continue
        if not crate and name:
            crate = str(name)
        if not vulnerable_version:
            vulnerable_version = extract_version(vuln.get("vulnerableVersionRange"))
        patched = vuln.get("firstPatchedVersion") or {}
        if not fixed_version:
            fixed_version = extract_version(patched)

    ghsa = node.get("ghsaId") or node.get("ghsa")
    return {
        "id": ghsa or node.get("cveId") or "unknown",
        "crate": str(crate),
        "severity": normalize_severity(node.get("severity")),
        "description": str(node.get("description") or ""),
        "fixed_version": fixed_version,
        "vulnerable_version": vulnerable_version,
        "cve": node.get("cveId") or node.get("cve"),
        "ghsa": ghsa,
        "url": node.get("permalink") or "",
        "permalink": node.get("permalink") or "",
        "aliases": [v for v in (node.get("cveId"), ghsa) if v],
        "ecosystem": "rust",
        "source": "github",
    }


def merge_github_advisory(advisory: dict[str, Any], ghsa: dict[str, Any] | None) -> dict[str, Any]:
    """Enrich a RustSec/npm advisory with GitHub Advisory Database data."""
    if not ghsa:
        # Explicitly mark the fallback source.
        advisory["source"] = advisory.get("source", "rustsec")
        return advisory
    advisory["severity"] = max_severity(advisory.get("severity"), ghsa.get("severity"))
    for key in ("description", "cve", "fixed_version", "vulnerable_version", "url", "ghsa", "permalink"):
        if not advisory.get(key) and ghsa.get(key):
            advisory[key] = ghsa[key]
    if not advisory.get("url") and ghsa.get("permalink"):
        advisory["url"] = ghsa["permalink"]
    if not advisory.get("crate") and ghsa.get("crate"):
        advisory["crate"] = ghsa["crate"]
    advisory["source"] = "github"
    return advisory


def normalize_npm_vuln(name: str, vuln: Any, npm_dir: str) -> dict[str, Any]:
    """Normalise one entry of npm audit's ``vulnerabilities`` map."""
    if not isinstance(vuln, dict):
        vuln = {"via": [str(vuln)]}

    via = vuln.get("via") or []
    if isinstance(via, (str, dict)):
        via = [via]

    titles: list[str] = []
    urls: list[str] = []
    advisory_ids: list[str] = []
    for item in via:
        if isinstance(item, str):
            titles.append(item)
            continue
        if not isinstance(item, dict):
            continue
        if item.get("title"):
            titles.append(str(item["title"]))
        if item.get("url"):
            urls.append(str(item["url"]))
        # Some npm advisories only expose the CVE/GHSA in the human title.
        title_text = str(item.get("title") or "")
        title_match = re.search(r"(GHSA-[0-9a-zA-Z-]+|CVE-\d{4}-\d+)", title_text, re.IGNORECASE)
        if title_match:
            advisory_ids.append(title_match.group(1))
        source = item.get("source") or item.get("name") or ""
        text = str(source)
        if text.upper().startswith(("GHSA-", "CVE-")):
            advisory_ids.append(text)
        url_text = str(item.get("url") or "")
        match = re.search(r"(GHSA-[0-9a-zA-Z-]+|CVE-\d{4}-\d+)", url_text, re.IGNORECASE)
        if match:
            advisory_ids.append(match.group(1))
        if item.get("range"):
            titles.append(f"affected range: {item['range']}")

    cve = next((i for i in advisory_ids if i.upper().startswith("CVE-")), None)
    ghsa = next((i for i in advisory_ids if i.upper().startswith("GHSA-")), None)

    fix_available = vuln.get("fixAvailable")
    fixed_version = None
    if isinstance(fix_available, dict):
        fixed_version = extract_version(fix_available.get("version"))
        if fix_available.get("name"):
            name = str(fix_available["name"])

    description = " ".join(dict.fromkeys(titles)) or str(vuln.get("title") or "")
    if vuln.get("range"):
        description = f"{description} (affected range: {vuln['range']})".strip()

    return {
        "id": ghsa or cve or f"npm:{name}",
        "crate": str(name),
        "severity": normalize_severity(vuln.get("severity")),
        "description": description,
        "fixed_version": fixed_version,
        "fix_available": bool(fix_available),
        "vulnerable_version": extract_version(vuln.get("range")),
        "cve": cve,
        "ghsa": ghsa,
        "url": urls[0] if urls else "",
        "aliases": advisory_ids,
        "npm_dir": npm_dir,
        "ecosystem": "npm",
        "source": "npm",
    }


def version_from_lock(lock: Path, crate: str) -> str | None:
    """Read the installed version of ``crate`` from a Cargo.lock as a fallback."""
    try:
        text = Path(lock).read_text(encoding="utf-8", errors="ignore")
    except OSError:
        return None
    crate_lc = crate.lower().replace("_", "-")
    for block in text.split("[[package]]")[1:]:
        name_match = re.search(r'(?m)^\s*name\s*=\s*"([^"]+)"', block)
        if not name_match or name_match.group(1).lower().replace("_", "-") != crate_lc:
            continue
        version_match = re.search(r'(?m)^\s*version\s*=\s*"([^"]+)"', block)
        if version_match:
            return version_match.group(1)
    return None


# --------------------------------------------------------------------------- #
# HTTP / GitHub API helpers
# --------------------------------------------------------------------------- #
def _request(method: str, url: str, token: str | None, payload: Any | None = None,
             timeout: int = HTTP_TIMEOUT) -> tuple[int, Any | None]:
    """Minimal JSON HTTP helper. Returns ``(status_code, parsed_json)``.

    Any network/HTTP/parse error is logged and converted to ``(0, None)`` so the
    bot degrades gracefully instead of crashing the CI job.
    """
    headers = {
        "Accept": "application/vnd.github+json",
        "User-Agent": USER_AGENT,
        "X-GitHub-Api-Version": "2022-11-28",
    }
    if token:
        headers["Authorization"] = f"Bearer {token}"
    body = None
    if payload is not None:
        body = json.dumps(payload).encode("utf-8")
        headers["Content-Type"] = "application/json"
    request = urllib.request.Request(url, data=body, headers=headers, method=method)
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            status = int(response.status)
            raw = response.read()
    except urllib.error.HTTPError as exc:
        status = int(exc.code)
        try:
            raw = exc.read()
        except OSError:
            raw = b""
    except (urllib.error.URLError, OSError, ValueError) as exc:
        _warn(f"HTTP {method} {url} failed: {exc}")
        return 0, None

    if not raw:
        return status, None
    try:
        return status, json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        _warn(f"could not decode JSON from {url}: {exc}")
        return status, None


def gh_get(path: str, token: str | None) -> Any | None:
    """GET a GitHub REST path, returning parsed JSON or ``None``."""
    status, data = _request("GET", f"{REST_BASE}{path}", token)
    return data if 200 <= status < 300 else None


def gh_post(path: str, payload: dict[str, Any], token: str | None) -> tuple[int, Any | None]:
    """POST JSON to a GitHub REST path, returning ``(status, data)``."""
    return _request("POST", f"{REST_BASE}{path}", token, payload)


def github_advisory(cve_or_ghsa: str | None, token: str | None) -> dict[str, Any] | None:
    """Cross-reference an id in the GitHub Advisory Database via GraphQL.

    ``GHSA-...`` ids are looked up with ``securityAdvisory(ghsaId:)``; CVEs use
    ``securityAdvisories(identifier:)``. GraphQL requires a token; on any error
    ``None`` is returned so callers fall back to source-native advisory data.
    """
    if not cve_or_ghsa or not token:
        return None

    is_ghsa = str(cve_or_ghsa).upper().startswith("GHSA-")
    if is_ghsa:
        query = """
        query($id: String!) {
          securityAdvisory(ghsaId: $id) {
            ghsaId cveId severity description permalink
            vulnerabilities(first: 10) {
              nodes {
                package { name }
                vulnerableVersionRange
                firstPatchedVersion { identifier }
              }
            }
          }
        }
        """
    else:
        query = """
        query($id: String!) {
          securityAdvisories(identifier: $id, first: 1) {
            nodes {
              ghsaId cveId severity description permalink
              vulnerabilities(first: 10) {
                nodes {
                  package { name }
                  vulnerableVersionRange
                  firstPatchedVersion { identifier }
                }
              }
            }
          }
        }
        """

    _status, data = _request("POST", GRAPHQL_URL, token, {"query": query, "variables": {"id": str(cve_or_ghsa)}})
    if not isinstance(data, dict):
        return None
    if data.get("errors"):
        _warn(f"GitHub Advisory GraphQL error for {cve_or_ghsa}: {data['errors']}")
        return None
    payload = data.get("data") or {}
    if is_ghsa:
        node = payload.get("securityAdvisory")
    else:
        connection = payload.get("securityAdvisories") or {}
        nodes = connection.get("nodes") or []
        node = nodes[0] if nodes else None
    normalized = normalize_ghsa_advisory(node)
    return normalized


def load_existing_advisory_ids(repo: str, token: str | None, dry_run: bool) -> set[str]:
    """Collect ids already mentioned in open security issues/PRs (dedupe)."""
    if dry_run:
        return set()
    data = gh_get(f"/repos/{repo}/issues?state=open&labels=security&per_page=100", token)
    if not isinstance(data, list):
        return set()
    titles = " \n ".join(str(issue.get("title", "")) for issue in data if isinstance(issue, dict))
    return {advisory_id.lower() for advisory_id in re.findall(r"(?:GHSA|CVE|RUSTSEC)-[0-9A-Za-z\-]+", titles, re.IGNORECASE)}


def is_duplicate(advisory: dict[str, Any], seen: set[str]) -> bool:
    candidates = [advisory.get("id"), advisory.get("cve"), advisory.get("ghsa")]
    candidates.extend(advisory.get("aliases") or [])
    return any(str(value).lower() in seen for value in candidates if value)


# --------------------------------------------------------------------------- #
# Manifest / deny.toml editing
# --------------------------------------------------------------------------- #
def replace_dependency_version(content: str, crate: str, old_version: str, new_version: str) -> str | None:
    """Conservatively replace a Cargo.toml dependency version spec.

    Returns the new content or ``None`` when no confident replacement exists.
    """
    if not crate or not old_version or not new_version or old_version == new_version:
        return None
    name_pattern = re.escape(crate).replace(r"\-", r"[-_]")
    pattern = re.compile(
        r'(?m)^(\s*["\']?' + name_pattern + r'["\']?\s*=\s*(?:\{\s*version\s*=\s*)?)["\']([^"\']*)["\']'
    )

    def repl(match: re.Match[str]) -> str:
        spec = match.group(2)
        if old_version not in spec:
            return match.group(0)
        return match.group(1) + '"' + spec.replace(old_version, new_version, 1) + '"'

    updated, count = pattern.subn(repl, content)
    return updated if count and updated != content else None


def replace_package_version(content: str, name: str, new_version: str) -> str | None:
    """Replace a package.json dependency spec with ``^<new_version>``."""
    if not name or not new_version:
        return None
    pattern = re.compile(r'(?m)^(\s+["\']' + re.escape(name) + r'["\']\s*:\s*)["\'][^"\']*["\']')

    def repl(match: re.Match[str]) -> str:
        return match.group(1) + '"^' + new_version + '"'

    updated, count = pattern.subn(repl, content)
    return updated if count and updated != content else None


def find_rust_manifest_update(lock: Path, crate: str, old_version: str | None,
                              new_version: str, root: Path) -> tuple[Path, str, str] | None:
    """Search the lockfile's directory and workspace ancestors for the manifest."""
    if not old_version:
        return None
    candidates: list[Path] = []
    current = lock.parent.resolve()
    root_resolved = root.resolve()
    while True:
        candidates.append(current / "Cargo.toml")
        try:
            candidates.extend(sorted(current.glob("*/Cargo.toml")))
        except OSError:
            pass
        if current == root_resolved or root_resolved not in current.parents:
            break
        current = current.parent
        if len(candidates) > 250:  # avoid pathological scans
            break

    seen: set[Path] = set()
    for candidate in candidates:
        resolved = candidate.resolve()
        if resolved in seen or not candidate.is_file():
            continue
        seen.add(resolved)
        try:
            original = candidate.read_text(encoding="utf-8", errors="ignore")
        except OSError:
            continue
        updated = replace_dependency_version(original, crate, old_version, new_version)
        if updated is not None:
            return candidate, original, updated
    return None


def _compute_deny_update(text: str, crate: str, version: str) -> str | None:
    """Return new deny.toml content with a deny entry added, or ``None``.

    Idempotent: returns ``None`` when the exact entry is already present.
    """
    entry = '{ name = "%s", version = "=%s" }' % (crate, version)
    if entry in text:
        return None
    match = re.search(r'(?m)^([ \t]*deny[ \t]*=[ \t]*)\[(.*?)\]', text, re.DOTALL)
    if not match:
        return None
    inner = match.group(2)
    if entry in inner:
        return None
    additions = []
    if DENY_MARKER not in inner:
        additions.append("    " + DENY_MARKER)
    additions.append("    " + entry + ",")
    if inner.strip():
        new_inner = inner.rstrip() + "\n" + "\n".join(additions) + "\n"
    else:
        new_inner = "\n" + "\n".join(additions) + "\n"
    return text[:match.start(2)] + new_inner + text[match.end(2):]


def update_deny_toml(path: str | Path, crate: str, version: str, *, dry_run: bool = False) -> bool:
    """Add ``{ name = "<crate>", version = "=<version>" }`` to ``[bans].deny``.

    Idempotent; returns ``True`` when the file was changed (or would be changed
    in ``--dry-run``). The marker comment makes machine-managed entries easy to
    spot and keeps repeat bot runs from piling up duplicate entries.
    """
    target = Path(path)
    try:
        original = target.read_text(encoding="utf-8")
    except OSError as exc:
        _warn(f"cannot read {target}: {exc}")
        return False
    updated = _compute_deny_update(original, crate, version)
    if updated is None:
        return False
    if not dry_run:
        try:
            target.write_text(updated, encoding="utf-8")
        except OSError as exc:
            _warn(f"cannot write {target}: {exc}")
            return False
    _info(f"{'would update' if dry_run else 'updated'} {target} to deny {crate} ={version}", dry_run)
    return True


# --------------------------------------------------------------------------- #
# PR / issue bodies and GitHub write helpers
# --------------------------------------------------------------------------- #
def render_pr_body(advisory: dict[str, Any], old_content: str, new_content: str,
                   manifest_path: str, *, extra_notes: str | None = None) -> str:
    """Render the required PR body: description, fix version, diff and CVE link."""
    diff = "\n".join(
        difflib.unified_diff(
            old_content.splitlines(),
            new_content.splitlines(),
            fromfile=f"a/{manifest_path}",
            tofile=f"b/{manifest_path}",
            lineterm="",
        )
    )
    parts = [
        "## Automated security dependency bump",
        "",
        f"- **Advisory:** {advisory.get('id', 'unknown')}",
        f"- **Package:** {advisory.get('crate', 'unknown')}",
        f"- **Severity:** {advisory.get('severity', 'INFO')}",
        f"- **Fix version:** {advisory.get('fixed_version')}",
        f"- **Reference:** {advisory_link(advisory)}",
        "",
        "### Advisory description",
        "",
        advisory.get("description") or "No description provided by the advisory source.",
        "",
        "### Proposed manifest change",
        "",
        f"```diff\n{diff}\n```",
    ]
    if extra_notes:
        parts.extend(["", "### Additional changes", "", extra_notes])
    parts.extend([
        "",
        "---",
        "Opened automatically by `scripts/security/audit_crates.py --bot`.",
    ])
    return "\n".join(parts)


def render_issue_body(advisory: dict[str, Any], *, note: str | None = None) -> str:
    """Render the body of an unpatched (or non-auto-fixable) advisory issue."""
    parts = [
        "## Unpatched security advisory",
        "",
        f"- **Advisory:** {advisory.get('id', 'unknown')}",
        f"- **Package:** {advisory.get('crate', 'unknown')}",
        f"- **Severity:** {advisory.get('severity', 'INFO')}",
        f"- **Reference:** {advisory_link(advisory)}",
        "",
        "### Advisory description",
        "",
        advisory.get("description") or "No description provided by the advisory source.",
        "",
        "**No fixed version available** for this advisory at the time of the scan.",
    ]
    if note:
        parts.extend(["", note])
    parts.extend([
        "",
        "---",
        "Opened automatically by `scripts/security/audit_crates.py --bot`.",
    ])
    return "\n".join(parts)


def open_issue(repo: str, title: str, body: str, maintainer: str,
               token: str | None, dry_run: bool = False) -> dict[str, Any] | None:
    """Open a security issue; retry without labels/assignees on a 422.

    NOTE: the repository must already have a ``security`` label and the
    maintainer must be assignable; otherwise the retry drops both.
    """
    payload: dict[str, Any] = {"title": title, "body": body, "labels": ["security"]}
    if maintainer:
        payload["assignees"] = [maintainer]

    if dry_run:
        _info(f"would open issue: {title}", dry_run)
        return {"number": 0, "title": title, "dry_run": True}

    status, data = gh_post(f"/repos/{repo}/issues", payload, token)
    if status == 422:
        _warn(f"issue creation with labels/assignees rejected (422); retrying without them: {title}")
        status, data = gh_post(
            f"/repos/{repo}/issues",
            {"title": title, "body": body},
            token,
        )
    if 200 <= status < 300 and isinstance(data, dict):
        _info(f"opened issue #{data.get('number')}: {title}")
        return data
    _warn(f"failed to open issue '{title}' (status {status})")
    return None


def open_fix_pr(repo: str, base: str, branch: str, title: str, body: str,
                file_updates: dict[str, str], token: str | None,
                dry_run: bool = False) -> dict[str, Any] | None:
    """Create a fix branch via the Git Data API and open a pull request.

    ``file_updates`` maps repo-relative paths to their new full file contents.
    """
    if dry_run:
        _info(f"would open PR '{title}' from {branch} -> {base} updating {sorted(file_updates)}", dry_run)
        return {"number": 0, "title": title, "dry_run": True}

    ref = gh_get(f"/repos/{repo}/git/ref/heads/{base}", token)
    if not isinstance(ref, dict) or not isinstance(ref.get("object"), dict):
        _warn(f"cannot resolve base branch '{base}' for {repo}")
        return None
    base_sha = ref["object"].get("sha")
    if not base_sha:
        _warn(f"base branch '{base}' has no commit sha")
        return None
    base_commit = gh_get(f"/repos/{repo}/git/commits/{base_sha}", token)
    base_tree_sha = (base_commit or {}).get("tree", {}).get("sha")

    tree_entries: list[dict[str, str]] = []
    for path, content in sorted(file_updates.items()):
        status, blob = gh_post(f"/repos/{repo}/git/blobs", {"content": content, "encoding": "utf-8"}, token)
        if not (200 <= status < 300 and isinstance(blob, dict) and blob.get("sha")):
            _warn(f"failed to create blob for {path} (status {status})")
            return None
        tree_entries.append({"path": path, "mode": "100644", "type": "blob", "sha": blob["sha"]})

    tree_payload: dict[str, Any] = {"tree": tree_entries}
    if base_tree_sha:
        tree_payload["base_tree"] = base_tree_sha
    status, tree = gh_post(f"/repos/{repo}/git/trees", tree_payload, token)
    if not (200 <= status < 300 and isinstance(tree, dict) and tree.get("sha")):
        _warn(f"failed to create git tree (status {status})")
        return None

    status, commit = gh_post(
        f"/repos/{repo}/git/commits",
        {"message": title, "tree": tree["sha"], "parents": [base_sha]},
        token,
    )
    if not (200 <= status < 300 and isinstance(commit, dict) and commit.get("sha")):
        _warn(f"failed to create git commit (status {status})")
        return None

    status, _new_ref = gh_post(
        f"/repos/{repo}/git/refs",
        {"ref": f"refs/heads/{branch}", "sha": commit["sha"]},
        token,
    )
    if not (200 <= status < 300):
        _warn(f"failed to create branch '{branch}' (status {status})")
        return None

    status, pr = gh_post(
        f"/repos/{repo}/pulls",
        {"title": title, "body": body, "head": branch, "base": base},
        token,
    )
    if 200 <= status < 300 and isinstance(pr, dict):
        _info(f"opened PR #{pr.get('number')}: {title}")
        return pr
    _warn(f"failed to open PR '{title}' (status {status})")
    return None


# --------------------------------------------------------------------------- #
# Scanners
# --------------------------------------------------------------------------- #
def cargo_audit_advisories(lock: Path) -> tuple[list[Any], bool]:
    """Run ``cargo audit --json`` for a lockfile; return (entries, fatal_error)."""
    try:
        result = subprocess.run(
            ["cargo", "audit", "--json", "--file", str(lock)],
            cwd=lock.parent,
            capture_output=True,
            text=True,
        )
    except (OSError, subprocess.SubprocessError) as exc:
        _warn(f"cargo audit failed for {lock}: {exc}")
        return [], True

    data: Any = None
    for stream in (result.stdout, result.stderr):
        if not stream or not stream.strip():
            continue
        try:
            data = json.loads(stream)
            break
        except json.JSONDecodeError:
            continue
    if data is None:
        _warn(f"could not parse cargo audit JSON for {lock}")
        return [], True

    if isinstance(data, list):
        return data, False
    if not isinstance(data, dict):
        _warn(f"unexpected cargo audit JSON shape for {lock}")
        return [], True
    entries = data.get("vulnerabilities")
    if entries is None:
        entries = data.get("advisories")
    if entries is None:
        return [], False
    return entries if isinstance(entries, list) else [entries], False


def npm_audit(directory: Path) -> dict[str, Any] | None:
    """Run ``npm audit --json`` in a directory and parse stdout (or stderr)."""
    try:
        result = subprocess.run(
            ["npm", "audit", "--json", "--prefix", str(directory)],
            cwd=directory,
            capture_output=True,
            text=True,
            timeout=180,
        )
    except (OSError, subprocess.SubprocessError) as exc:
        _warn(f"npm audit failed for {directory}: {exc}")
        return None
    # npm audit exits non-zero when vulnerabilities exist; JSON is still valid.
    for stream in (result.stdout, result.stderr):
        if not stream or not stream.strip():
            continue
        try:
            data = json.loads(stream)
        except json.JSONDecodeError:
            continue
        if isinstance(data, dict):
            return data
    _warn(f"could not parse npm audit JSON for {directory}")
    return None


# --------------------------------------------------------------------------- #
# Bot orchestration
# --------------------------------------------------------------------------- #
def _relative(path: Path, root: Path) -> str:
    try:
        return str(Path(path).resolve().relative_to(root.resolve()))
    except ValueError:
        return str(path)


def _cross_reference(advisory: dict[str, Any], token: str | None) -> dict[str, Any]:
    identifiers = [advisory.get("cve"), advisory.get("ghsa")]
    identifiers.extend(advisory.get("aliases") or [])
    for identifier in identifiers:
        if not identifier or not str(identifier).upper().startswith(("CVE-", "GHSA-")):
            continue
        ghsa = github_advisory(str(identifier), token)
        if ghsa:
            return merge_github_advisory(advisory, ghsa)
    return merge_github_advisory(advisory, None)


def _process_advisory(advisory: dict[str, Any], *, root: Path, repo: str, base: str,
                      maintainer: str, token: str | None, dry_run: bool, seen: set[str],
                      summary: dict[str, int], deny_path: Path, deny_text: str,
                      manifest_info: tuple[Path, str, str] | None,
                      package_info: tuple[Path, str, str] | None) -> None:
    """Shared remediation logic for a normalised Rust or npm advisory."""
    if is_duplicate(advisory, seen):
        _info(f"skipping already-tracked advisory {advisory.get('id')}", dry_run)
        return
    summary["advisories_found"] += 1

    if not is_actionable(advisory):
        _info(f"ignoring {advisory.get('id')} (severity {advisory.get('severity')})", dry_run)
        return

    crate = advisory.get("crate") or "dependency"
    advisory_id = advisory.get("id") or "unknown"
    fix = advisory.get("fixed_version")
    if not fix:
        title = f"Unpatched advisory {advisory_id}: {crate}"
        if open_issue(repo, title, render_issue_body(advisory), maintainer, token, dry_run):
            summary["issues_opened"] += 1
        return

    # A fix exists: prefer a manifest-based PR, otherwise fall back to an issue.
    manifest = manifest_info or package_info
    if manifest is None:
        note = "Automatic manifest update was skipped: no confident version replacement was found."
        title = f"Unpatched advisory {advisory_id}: {crate}"
        if open_issue(repo, title, render_issue_body(advisory, note=note), maintainer, token, dry_run):
            summary["issues_opened"] += 1
        return

    manifest_path, old_text, new_text = manifest
    relative_manifest = _relative(manifest_path, root)

    file_updates: dict[str, str] = {relative_manifest: new_text}
    extra_notes = None

    # deny.toml is cargo-deny (Rust) configuration: only Rust advisories apply.
    if advisory.get("ecosystem") == "rust" and advisory.get("vulnerable_version"):
        new_deny = _compute_deny_update(deny_text, crate, advisory["vulnerable_version"])
        if new_deny is not None:
            file_updates[_relative(deny_path, root)] = new_deny
            summary["deny_updates"] += 1
            extra_notes = (
                f"`deny.toml` now denies `{crate} ={advisory['vulnerable_version']}` "
                f"until the dependency graph is updated."
            )
            if dry_run or not token:
                _info(f"would update deny.toml to deny {crate} ={advisory['vulnerable_version']}", dry_run)

    body = render_pr_body(advisory, old_text, new_text, relative_manifest, extra_notes=extra_notes)
    title = f"security: bump {crate} to {fix} ({advisory_id})"
    branch = branch_name(crate, advisory_id)
    if open_fix_pr(repo, base, branch, title, body, file_updates, token, dry_run):
        summary["prs_opened"] += 1


def bot_run(root: Path, args: argparse.Namespace) -> int:
    """Run the remediation bot and emit the final JSON summary on stdout."""
    token = args.token or os.environ.get("GITHUB_TOKEN")
    repo = args.repo or os.environ.get("GITHUB_REPOSITORY")
    maintainer = (
        args.maintainer
        or os.environ.get("SECURITY_MAINTAINER")
        or (repo.split("/")[0] if repo else "")
    )
    dry_run = args.dry_run
    base = args.base
    npm_dirs = args.npm_dir if args.npm_dir is not None else list(DEFAULT_NPM_DIRS)
    deny_path = root / "deny.toml"

    if not repo:
        _warn("--repo or GITHUB_REPOSITORY is required in --bot mode")
        return 2
    if not token and not dry_run:
        _warn("no token supplied: GitHub writes are disabled (pass --token or GITHUB_TOKEN)")

    summary: dict[str, int] = {
        "advisories_found": 0,
        "prs_opened": 0,
        "issues_opened": 0,
        "deny_updates": 0,
    }
    seen = load_existing_advisory_ids(repo, token, dry_run)
    fatal_parse = False

    try:
        deny_text = deny_path.read_text(encoding="utf-8")
    except OSError:
        deny_text = ""

    if args.ecosystem in ("rust", "all"):
        for lock in sorted(root.glob("**/Cargo.lock")):
            _info(f"scanning {_relative(lock, root)}", dry_run)
            entries, fatal = cargo_audit_advisories(lock)
            fatal_parse = fatal_parse or fatal
            for entry in entries:
                advisory = normalize_rustsec_advisory(entry, lock)
                advisory = _cross_reference(advisory, token)
                manifest_info = find_rust_manifest_update(
                    lock,
                    advisory.get("crate", ""),
                    advisory.get("vulnerable_version"),
                    advisory.get("fixed_version") or "",
                    root,
                )
                _process_advisory(
                    advisory,
                    root=root, repo=repo, base=base, maintainer=maintainer, token=token,
                    dry_run=dry_run or not token, seen=seen, summary=summary,
                    deny_path=deny_path, deny_text=deny_text,
                    manifest_info=manifest_info, package_info=None,
                )

    if args.ecosystem in ("npm", "all"):
        for npm_dir in npm_dirs:
            directory = root / npm_dir
            if not (directory / "package-lock.json").is_file():
                _info(f"skipping {npm_dir}: no package-lock.json", dry_run)
                continue
            _info(f"scanning {npm_dir}/package-lock.json", dry_run)
            data = npm_audit(directory)
            if not isinstance(data, dict):
                continue
            vulnerabilities = data.get("vulnerabilities") or {}
            if not isinstance(vulnerabilities, dict):
                continue
            package_json = directory / "package.json"
            for name, vuln in vulnerabilities.items():
                advisory = normalize_npm_vuln(str(name), vuln, npm_dir)
                advisory = _cross_reference(advisory, token)
                package_info = None
                if advisory.get("fixed_version") and package_json.is_file():
                    try:
                        original = package_json.read_text(encoding="utf-8", errors="ignore")
                    except OSError:
                        original = ""
                    updated = replace_package_version(original, str(name), advisory["fixed_version"])
                    if updated is not None:
                        package_info = (package_json, original, updated)
                _process_advisory(
                    advisory,
                    root=root, repo=repo, base=base, maintainer=maintainer, token=token,
                    dry_run=dry_run or not token, seen=seen, summary=summary,
                    deny_path=deny_path, deny_text=deny_text,
                    manifest_info=None, package_info=package_info,
                )

    print(json.dumps(summary, sort_keys=True))
    return 2 if fatal_parse else 0


# --------------------------------------------------------------------------- #
# Plain (original) audit path
# --------------------------------------------------------------------------- #
def run(root: Path, allow: set[str]) -> int:
    """Original behaviour: cargo-audit every lockfile + unsafe-code scan."""
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
        metadata = subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--locked"],
            cwd=crate_dir, capture_output=True, text=True,
        )
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


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("root", nargs="?", type=Path, default=Path("."))
    parser.add_argument("--allow-unsafe", action="append", default=[])
    parser.add_argument("--bot", action="store_true", help="enable GitHub remediation mode")
    parser.add_argument("--repo", default=None, help="OWNER/REPO (default: GITHUB_REPOSITORY)")
    parser.add_argument("--token", default=None, help="GitHub token (default: GITHUB_TOKEN)")
    parser.add_argument("--maintainer", default=None,
                        help="issue assignee (default: SECURITY_MAINTAINER or repo owner)")
    parser.add_argument("--dry-run", action="store_true", help="print actions; no network writes")
    parser.add_argument("--ecosystem", choices=("rust", "npm", "all"), default="all")
    parser.add_argument("--npm-dir", action="append", default=None,
                        help="directory with package-lock.json (repeatable)")
    parser.add_argument("--base", default=DEFAULT_BASE, help="base branch for fix PRs")
    args = parser.parse_args(argv)
    root = args.root.resolve()
    if args.bot:
        return bot_run(root, args)
    return run(root, set(args.allow_unsafe))


if __name__ == "__main__":
    raise SystemExit(main())
