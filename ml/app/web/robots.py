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

"""Robots.txt policy. The default
posture is "user_triggered" — single user-requested fetches proceed, the way
the major assistants' fetchers behave (resolved decision 4). The "respect"
policy honours per-host robots.txt: fetched through the SAME guarded fetcher
(SSRF + pacing apply), parsed with urllib.robotparser, cached per host.

Fail-open on robots-fetch errors: robots is politeness, not security — the
SSRF guard is the security layer, and a host whose robots.txt 500s should not
become unreachable for the user."""

import logging
from urllib.parse import urlsplit
from urllib.robotparser import RobotFileParser

from ..config import settings
from .cache import TTLCache

_log = logging.getLogger("pai.web.robots")

# host -> RobotFileParser | None (None = robots unavailable ⇒ allow).
_robots_cache = TTLCache(settings.web_robots_cache_ttl)

_ALLOW_ALL = object()  # cache sentinel for "no usable robots ⇒ allow"


def _agent_token() -> str:
    """First product token of the configured UA (e.g. "PAIPlatform/1.0 (+url)"
    → "PAIPlatform") — what robots.txt user-agent matching expects."""
    ua = settings.web_user_agent.strip()
    return ua.split("/")[0].split(" ")[0] or "*"


async def allowed(url: str) -> bool:
    """May `url` be fetched under the active robots policy? `user_triggered`
    short-circuits to True."""
    from ..rag_ctx import cfg

    policy = str(cfg("web_robots_policy", settings.web_robots_policy)).lower()
    if policy != "respect":
        return True

    parts = urlsplit(url)
    host = (parts.hostname or "").lower()
    if not host:
        return False
    key = f"{parts.scheme}://{host}"

    parser = _robots_cache.get(key)
    if parser is None:
        parser = await _fetch_robots(key)
        _robots_cache.set(key, parser)
    if parser is _ALLOW_ALL:
        return True
    return parser.can_fetch(_agent_token(), url)


async def _fetch_robots(origin: str):
    """Fetch + parse {origin}/robots.txt via the guarded fetcher. Any failure
    (404, block, transport, SSRF) ⇒ allow-all sentinel."""
    from . import fetcher

    try:
        page = await fetcher.fetch_page(f"{origin}/robots.txt")
        parser = RobotFileParser()
        parser.parse(page.body.splitlines())
        return parser
    except Exception as e:  # noqa: BLE001 — fail-open by design
        _log.debug("robots.txt unavailable for %s (allowing): %s", origin, e)
        return _ALLOW_ALL
