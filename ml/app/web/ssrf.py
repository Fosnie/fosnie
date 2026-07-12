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

"""SSRF guard for the server-side web fetcher, correctness-critical and unit-tested.

Policy: http(s) only; the host must resolve exclusively to globally-routable
addresses (RFC1918, loopback, link-local 169.254/16 cloud metadata, IPv6 ULA
fc00::/7, fe80::/10, multicast, reserved, unspecified, CGNAT — all rejected,
including IPv4-mapped IPv6 forms); only ports 80/443; no credentials in the
URL. The caller then PINS the connection to a validated IP (defeating DNS
rebinding) and re-validates every redirect hop.

The pure-logic core (`validate_ip`, `validate_url`) has no I/O so it tests
without a network; `resolve_and_validate` adds the DNS step."""

import asyncio
import ipaddress
import socket
from urllib.parse import urlsplit

_ALLOWED_SCHEMES = {"http", "https"}
_ALLOWED_PORTS = {80, 443}
_MAX_REDIRECTS = 5  # re-validated per hop by the fetcher


class SsrfBlocked(Exception):
    """The URL/host/IP failed the SSRF policy. Never fetched."""


def validate_ip(ip: str | ipaddress.IPv4Address | ipaddress.IPv6Address) -> None:
    """Reject any address that is not globally routable. Raises SsrfBlocked."""
    try:
        addr = ipaddress.ip_address(ip) if isinstance(ip, str) else ip
    except ValueError as e:
        raise SsrfBlocked(f"unparseable IP address: {ip!r}") from e

    # IPv4-mapped IPv6 (::ffff:10.0.0.1) — judge the embedded IPv4.
    if isinstance(addr, ipaddress.IPv6Address) and addr.ipv4_mapped is not None:
        addr = addr.ipv4_mapped

    # Explicit checks first (clear error messages), then the is_global
    # belt-and-braces which also covers CGNAT 100.64/10, ULA fc00::/7, TEST-NETs
    # and the rest of the IANA special-purpose registries.
    if addr.is_loopback:
        raise SsrfBlocked(f"loopback address blocked: {addr}")
    if addr.is_private:
        raise SsrfBlocked(f"private address blocked: {addr}")
    if addr.is_link_local:
        raise SsrfBlocked(f"link-local address blocked: {addr}")
    if addr.is_multicast:
        raise SsrfBlocked(f"multicast address blocked: {addr}")
    if addr.is_unspecified:
        raise SsrfBlocked(f"unspecified address blocked: {addr}")
    if addr.is_reserved:
        raise SsrfBlocked(f"reserved address blocked: {addr}")
    if not addr.is_global:
        raise SsrfBlocked(f"non-global address blocked: {addr}")


def validate_url(url: str) -> tuple[str, str, int]:
    """Validate scheme/host/port shape (no DNS). Returns (scheme, host, port).
    Raises SsrfBlocked on any policy violation."""
    parts = urlsplit(url)
    scheme = (parts.scheme or "").lower()
    if scheme not in _ALLOWED_SCHEMES:
        raise SsrfBlocked(f"scheme not allowed: {scheme or '(none)'}")
    if parts.username is not None or parts.password is not None:
        raise SsrfBlocked("credentials in URL blocked")
    host = parts.hostname
    if not host:
        raise SsrfBlocked("URL has no host")
    try:
        port = parts.port  # raises ValueError on out-of-range
    except ValueError as e:
        raise SsrfBlocked(f"invalid port in URL") from e
    port = port or (443 if scheme == "https" else 80)
    if port not in _ALLOWED_PORTS:
        raise SsrfBlocked(f"port not allowed: {port}")

    # IP-literal host — validate it directly (no DNS will happen).
    try:
        literal = ipaddress.ip_address(host)
    except ValueError:
        pass  # a hostname; DNS validation happens in resolve_and_validate
    else:
        validate_ip(literal)

    return scheme, host, port


async def resolve_and_validate(host: str) -> str:
    """Resolve `host` and validate EVERY address it resolves to; reject the URL
    if any is non-global (an attacker controls the DNS answer set). Returns one
    validated IP for the caller to pin the connection to."""
    try:
        literal = ipaddress.ip_address(host)
    except ValueError:
        pass
    else:
        validate_ip(literal)
        return str(literal)

    loop = asyncio.get_running_loop()
    try:
        infos = await loop.getaddrinfo(host, None, type=socket.SOCK_STREAM)
    except socket.gaierror as e:
        raise SsrfBlocked(f"DNS resolution failed for {host}: {e}") from e
    addrs = list(dict.fromkeys(info[4][0] for info in infos))
    if not addrs:
        raise SsrfBlocked(f"no addresses for {host}")
    for a in addrs:
        validate_ip(a)
    # Prefer IPv4 for the pinned connection (broadest reachability).
    for a in addrs:
        if isinstance(ipaddress.ip_address(a), ipaddress.IPv4Address):
            return a
    return addrs[0]
