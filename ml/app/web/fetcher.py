# Copyright 2026 Private AI Ltd (SC881079)
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""SSRF-hardened tiered page fetcher.

Tier 1 is plain httpx with an honest, stable User-Agent (never a spoofed
browser — resolved decision 4). Every request: validate the URL, resolve DNS,
validate EVERY resolved address, then PIN the connection to a validated IP
(request the IP directly with `Host` + `sni_hostname` carrying the real
hostname, so TLS still verifies against the hostname) — closing the DNS-
rebinding TOCTOU. Redirects are followed manually, re-validated and re-pinned
per hop, capped at 5. Bodies stream in under a hard byte cap.

Tier 2 (optional, off by default) escalates near-empty extractions to a
Playwright render. The import is lazy so the service boots without Chromium;
subresource requests to private/localhost hosts are aborted via route
interception."""

import ipaddress
import logging
from dataclasses import dataclass
from urllib.parse import urljoin, urlsplit, urlunsplit

import httpx

from ..config import settings
from . import ssrf
from .pacing import pacer

_log = logging.getLogger("pai.web.fetch")

_REDIRECT_CODES = {301, 302, 303, 307, 308}
_TEXT_TYPES = ("text/html", "application/xhtml", "text/plain")


class FetchError(Exception):
    """The page could not be fetched within policy/budget. Snippet-only fallback."""


@dataclass
class FetchResult:
    final_url: str
    status: int
    body: str  # decoded text (html or plain); empty for binary (PDF) responses
    content_type: str
    # Raw bytes for the PDF branch (body stays empty) — same SSRF/redirect/size
    # rules as text; the pipeline routes these through the document extraction
    # path (pypdf + OCR fallback).
    raw: bytes | None = None


def _host_key(url: str) -> str:
    return f"host:{(urlsplit(url).hostname or '').lower()}"


def _pinned_url(scheme: str, ip: str, port: int, parts) -> str:
    ip_for_url = f"[{ip}]" if ":" in ip else ip
    default = 443 if scheme == "https" else 80
    netloc = ip_for_url if port == default else f"{ip_for_url}:{port}"
    return urlunsplit((scheme, netloc, parts.path or "/", parts.query, ""))


def _decode(raw: bytes, content_type: str) -> str:
    charset = "utf-8"
    if "charset=" in content_type:
        charset = content_type.split("charset=", 1)[1].split(";")[0].strip() or "utf-8"
    try:
        return raw.decode(charset, errors="replace")
    except LookupError:
        return raw.decode("utf-8", errors="replace")


async def fetch_page(url: str) -> FetchResult:
    """Tier-1 guarded fetch. Raises FetchError on any policy/transport failure
    (the pipeline then keeps the SERP snippet as snippet-only evidence)."""
    await pacer.acquire(_host_key(url), settings.web_host_rps, settings.web_pacing_burst)

    headers_base = {
        "User-Agent": settings.web_user_agent,
        "Accept": "text/html,application/xhtml+xml,text/plain;q=0.9,*/*;q=0.1",
        "Accept-Language": "en-GB,en;q=0.8",
    }
    timeout = httpx.Timeout(
        settings.web_fetch_timeout, connect=settings.web_connect_timeout
    )

    current = url
    try:
        async with httpx.AsyncClient(timeout=timeout, follow_redirects=False) as client:
            for _hop in range(settings.web_max_redirects + 1):
                scheme, host, port = ssrf.validate_url(current)
                ip = await ssrf.resolve_and_validate(host)
                parts = urlsplit(current)
                default = 443 if scheme == "https" else 80
                host_hdr = host if port == default else f"{host}:{port}"
                req = client.build_request(
                    "GET",
                    _pinned_url(scheme, ip, port, parts),
                    headers={**headers_base, "Host": host_hdr},
                    extensions={"sni_hostname": host} if scheme == "https" else {},
                )
                resp = await client.send(req, stream=True)
                try:
                    if resp.status_code in _REDIRECT_CODES:
                        location = resp.headers.get("location")
                        if not location:
                            raise FetchError(f"redirect without Location from {current}")
                        current = urljoin(current, location)
                        continue  # next hop re-validates + re-pins
                    if resp.status_code != 200:
                        raise FetchError(f"HTTP {resp.status_code} from {current}")
                    ctype = (resp.headers.get("content-type") or "").lower()
                    is_pdf = ctype.startswith("application/pdf")
                    if ctype and not is_pdf and not ctype.startswith(_TEXT_TYPES):
                        raise FetchError(f"unsupported content-type {ctype} from {current}")
                    raw = b""
                    async for chunk in resp.aiter_bytes():
                        raw += chunk
                        if len(raw) > settings.web_fetch_max_bytes:
                            raise FetchError(f"body over {settings.web_fetch_max_bytes} bytes: {current}")
                    if is_pdf:
                        # PDF branch: hand the bytes to the document extraction
                        # path (pypdf native text + OCR for scanned).
                        return FetchResult(
                            final_url=current, status=resp.status_code, body="",
                            content_type=ctype, raw=raw,
                        )
                    return FetchResult(
                        final_url=current,
                        status=resp.status_code,
                        body=_decode(raw, ctype),
                        content_type=ctype,
                    )
                finally:
                    await resp.aclose()
            raise FetchError(f"redirect cap ({settings.web_max_redirects}) exceeded: {url}")
    except (ssrf.SsrfBlocked, FetchError):
        raise
    except httpx.HTTPError as e:
        raise FetchError(f"transport error fetching {current}: {e}") from e


# Hostnames a rendered page must never touch as subresources, beyond IP checks.
_BLOCKED_RENDER_HOSTS = {"localhost", "metadata.google.internal"}


def _render_request_allowed(req_url: str) -> bool:
    """Cheap private-target screen for rendered-page subresources. IP-literal
    and well-known-internal hosts are rejected; hostname subresources are NOT
    re-resolved in Phase 1 (documented residual — render is off by default)."""
    try:
        parts = urlsplit(req_url)
        if parts.scheme not in ("http", "https"):
            return False
        host = (parts.hostname or "").lower()
        if not host or host in _BLOCKED_RENDER_HOSTS:
            return False
        try:
            ssrf.validate_ip(ipaddress.ip_address(host))
        except ssrf.SsrfBlocked:
            return False
        except ValueError:
            pass  # a hostname, allowed (Phase-1 residual)
        return True
    except Exception:  # noqa: BLE001 — when unsure, block
        return False


async def render_page(url: str) -> FetchResult:
    """Tier-2 Playwright render for near-empty extractions. Only called for
    URLs that already passed the SSRF guard, and only when web_render_enabled.
    Raises FetchError when Playwright/Chromium is not installed."""
    ssrf.validate_url(url)
    await ssrf.resolve_and_validate(urlsplit(url).hostname or "")
    await pacer.acquire(_host_key(url), settings.web_host_rps, settings.web_pacing_burst)

    try:
        from playwright.async_api import async_playwright
    except ImportError as e:
        raise FetchError("Playwright is not installed (web_render_enabled needs it)") from e

    try:
        async with async_playwright() as pw:
            browser = await pw.chromium.launch(headless=True)
            try:
                page = await browser.new_page(user_agent=settings.web_user_agent)

                async def _route(route):
                    if _render_request_allowed(route.request.url):
                        await route.continue_()
                    else:
                        await route.abort()

                await page.route("**/*", _route)
                await page.goto(url, timeout=int(settings.web_render_timeout * 1000), wait_until="domcontentloaded")
                html = await page.content()
                return FetchResult(final_url=page.url, status=200, body=html, content_type="text/html")
            finally:
                await browser.close()
    except FetchError:
        raise
    except Exception as e:  # noqa: BLE001 — rendering is best-effort
        raise FetchError(f"render failed for {url}: {e}") from e
