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

"""SSRF guard — the correctness-critical part of web search.
Pure-logic tests, no network."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

import asyncio

import pytest

from app.web import ssrf
from app.web.ssrf import SsrfBlocked, validate_ip, validate_url


# --- validate_ip: deny ranges -------------------------------------------------

@pytest.mark.parametrize(
    "ip",
    [
        # RFC1918
        "10.0.0.1",
        "172.16.0.1",
        "172.31.255.254",
        "192.168.1.1",
        # Loopback
        "127.0.0.1",
        "127.255.255.254",
        "::1",
        # Link-local (cloud metadata lives here)
        "169.254.169.254",
        "169.254.0.1",
        "fe80::1",
        # IPv6 ULA
        "fc00::1",
        "fd12:3456:789a::1",
        # Multicast
        "224.0.0.1",
        "ff02::1",
        # Unspecified
        "0.0.0.0",
        "::",
        # Reserved / special
        "240.0.0.1",
        # CGNAT (caught by is_global)
        "100.64.0.1",
        # IPv4-mapped IPv6 forms of private/loopback addresses
        "::ffff:10.0.0.1",
        "::ffff:127.0.0.1",
        "::ffff:192.168.0.1",
        "::ffff:169.254.169.254",
    ],
)
def test_denied_addresses(ip):
    with pytest.raises(SsrfBlocked):
        validate_ip(ip)


@pytest.mark.parametrize("ip", ["93.184.216.34", "1.1.1.1", "2606:4700:4700::1111"])
def test_global_addresses_allowed(ip):
    validate_ip(ip)  # must not raise


def test_unparseable_ip_blocked():
    with pytest.raises(SsrfBlocked):
        validate_ip("not-an-ip")


# --- validate_url: scheme / port / credentials / IP-literal hosts -------------

@pytest.mark.parametrize(
    "url",
    [
        "ftp://example.com/",
        "file:///etc/passwd",
        "gopher://example.com/",
        "javascript:alert(1)",
        "//example.com/",  # scheme-relative — no scheme
    ],
)
def test_non_http_schemes_blocked(url):
    with pytest.raises(SsrfBlocked):
        validate_url(url)


@pytest.mark.parametrize(
    "url",
    [
        "http://example.com:8080/",
        "https://example.com:8443/",
        "http://example.com:22/",
        "http://example.com:6379/",  # Redis
    ],
)
def test_non_web_ports_blocked(url):
    with pytest.raises(SsrfBlocked):
        validate_url(url)


def test_credentials_in_url_blocked():
    with pytest.raises(SsrfBlocked):
        validate_url("https://user:pass@example.com/")
    with pytest.raises(SsrfBlocked):
        validate_url("https://user@example.com/")


@pytest.mark.parametrize(
    "url",
    [
        "http://127.0.0.1/",
        "http://10.0.0.1/page",
        "https://192.168.1.1/",
        "http://169.254.169.254/latest/meta-data/",
        "http://[::1]/",
        "http://[fd00::1]/",
        "http://[::ffff:10.0.0.1]/",
        "http://0.0.0.0/",
    ],
)
def test_ip_literal_hosts_blocked(url):
    with pytest.raises(SsrfBlocked):
        validate_url(url)


def test_plain_https_allowed():
    assert validate_url("https://example.com/path?q=1") == ("https", "example.com", 443)
    assert validate_url("http://example.com/") == ("http", "example.com", 80)
    assert validate_url("https://example.com:443/") == ("https", "example.com", 443)


def test_empty_host_blocked():
    with pytest.raises(SsrfBlocked):
        validate_url("https:///nohost")


# --- resolve_and_validate: every resolved address must be global --------------

def _run(coro):
    return asyncio.new_event_loop().run_until_complete(coro)


def test_resolve_rejects_when_any_address_private(monkeypatch):
    async def fake_getaddrinfo(host, port, **kw):
        # A rebinding-style answer: one public, one private — must be rejected.
        return [
            (2, 1, 6, "", ("93.184.216.34", 0)),
            (2, 1, 6, "", ("10.0.0.5", 0)),
        ]

    class FakeLoop:
        getaddrinfo = staticmethod(fake_getaddrinfo)

    monkeypatch.setattr(ssrf.asyncio, "get_running_loop", lambda: FakeLoop())
    with pytest.raises(SsrfBlocked):
        _run(ssrf.resolve_and_validate("evil.example"))


def test_resolve_returns_validated_ipv4(monkeypatch):
    async def fake_getaddrinfo(host, port, **kw):
        return [
            (10, 1, 6, "", ("2606:4700:4700::1111", 0, 0, 0)),
            (2, 1, 6, "", ("1.1.1.1", 0)),
        ]

    class FakeLoop:
        getaddrinfo = staticmethod(fake_getaddrinfo)

    monkeypatch.setattr(ssrf.asyncio, "get_running_loop", lambda: FakeLoop())
    assert _run(ssrf.resolve_and_validate("good.example")) == "1.1.1.1"


def test_resolve_ip_literal_short_circuits():
    # No DNS for literals; valid global IP comes straight back.
    assert _run(ssrf.resolve_and_validate("1.1.1.1")) == "1.1.1.1"
    with pytest.raises(SsrfBlocked):
        _run(ssrf.resolve_and_validate("127.0.0.1"))
