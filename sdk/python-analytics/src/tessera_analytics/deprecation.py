"""Client-side handling of Tessera API deprecation headers.

Deprecated-but-served responses carry ``Sunset`` and
``Link: <...>; rel="successor-version"`` headers. ``warn_if_deprecated`` turns
those into a :class:`DeprecationWarning`, de-duplicated per (path, sunset) so
analytics notebooks don't emit the same notice for every page of results.

Standard library only; the analytics SDK's heavy data dependencies are not
needed here.
"""

from __future__ import annotations

import re
import threading
import warnings
from email.message import Message
from http.client import HTTPResponse
from typing import Mapping, cast
from urllib.parse import urlsplit
from urllib.request import Request, urlopen

__all__ = [
    "parse_successor",
    "warn_if_deprecated",
    "request_with_deprecation_warning",
]

# (path, sunset) pairs already reported to the caller.
_warned: set[tuple[str, str]] = set()
_warned_lock = threading.Lock()

_SUCCESSOR_RE = re.compile(
    r"<([^>]+)>\s*;\s*rel\s*=\s*['\"]successor-version['\"]",
    re.IGNORECASE,
)


def parse_successor(link: str) -> str | None:
    """Return the successor URI from a ``Link`` header, or ``None``.

    Handles multiple comma-separated links, quoted ``rel`` values, and case
    variations. Pure and dependency-free for straightforward testing.
    """
    match = _SUCCESSOR_RE.search(link)
    return match.group(1) if match else None


def _header(headers: Mapping[str, str], name: str) -> str | None:
    """Case-insensitive header lookup (HTTP headers are case-insensitive)."""
    lowered = name.lower()
    for key, value in headers.items():
        if key.lower() == lowered:
            return value
    return None


def warn_if_deprecated(headers: Mapping[str, str], path: str) -> None:
    """Warn once per (path, sunset) when a response is deprecated.

    ``headers`` may be any string-to-string mapping, e.g. the headers of an
    ``urllib`` response or an HTTP framework's response object.
    """
    sunset = _header(headers, "sunset")
    link = _header(headers, "link") or ""
    successor = parse_successor(link)

    if sunset is None and successor is None:
        return

    key = (path, sunset or "")
    with _warned_lock:
        if key in _warned:
            return
        _warned.add(key)

    details = [f"Tessera API endpoint {path} is deprecated"]
    if sunset:
        details.append(f"sunset: {sunset}")
    if successor:
        details.append(f"successor: {successor}")
    warnings.warn(
        "; ".join(details) + ". Migrate before the sunset date.",
        DeprecationWarning,
        stacklevel=2,
    )


def request_with_deprecation_warning(
    url: str,
    *,
    method: str = "GET",
    headers: Mapping[str, str] | None = None,
    data: bytes | None = None,
    timeout: float | None = None,
) -> bytes:
    """GET/POST ``url`` with urllib, warning if the response is deprecated.

    Returns the raw response body. Network errors propagate as usual.
    """
    request = Request(url, data=data, headers=dict(headers or {}), method=method)
    if timeout is None:
        response = urlopen(request)
    else:
        response = urlopen(request, timeout=timeout)
    with response:
        http_response = cast(HTTPResponse, response)
        message = cast(Message, http_response.headers)
        warn_if_deprecated(
            {key: value for key, value in message.items()},
            urlsplit(url).path or "/",
        )
        return bytes(http_response.read())
