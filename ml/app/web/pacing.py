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

"""Politeness pacing, the no-IP-ban guarantee. Async token buckets keyed by string (`engine:<name>` /
`host:<host>`) bound how fast the platform talks to any one search engine or
web host, so the client's egress IP is never hammered into a ban. A 429 or a
blank SERP puts the key on a cooldown (the same self-healing shape as the
reranker's `_down_until`)."""

import asyncio
import time


class Pacer:
    """Token buckets + cooldowns, keyed by string. One process-wide instance.

    `acquire` blocks (sleeps) until a token is available for the key — callers
    therefore never exceed `rate` requests/second (with a small `burst`).
    Cooldowns are advisory: callers check `cooling(key)` and skip the backend
    (fall back) rather than queue behind a long sleep."""

    def __init__(self) -> None:
        self._lock = asyncio.Lock()
        self._buckets: dict[str, tuple[float, float]] = {}  # key -> (tokens, last_refill)
        self._cooldowns: dict[str, float] = {}  # key -> monotonic deadline

    async def acquire(self, key: str, rate: float, burst: float = 2.0) -> None:
        """Take one token for `key`, sleeping until one accrues at `rate`/s."""
        rate = max(rate, 0.01)  # guard a zero/negative misconfiguration
        while True:
            async with self._lock:
                now = time.monotonic()
                tokens, last = self._buckets.get(key, (burst, now))
                tokens = min(burst, tokens + (now - last) * rate)
                if tokens >= 1.0:
                    self._buckets[key] = (tokens - 1.0, now)
                    return
                self._buckets[key] = (tokens, now)
                wait = (1.0 - tokens) / rate
            # Sleep OUTSIDE the lock so other keys keep flowing.
            await asyncio.sleep(wait)

    def set_cooldown(self, key: str, seconds: float) -> None:
        """Back off `key` (429 / blank SERP / block page) for `seconds`."""
        self._cooldowns[key] = time.monotonic() + seconds

    def cooling(self, key: str) -> bool:
        deadline = self._cooldowns.get(key)
        if deadline is None:
            return False
        if time.monotonic() >= deadline:
            del self._cooldowns[key]
            return False
        return True


# Process-wide pacer shared by the SERP clients and the fetcher.
pacer = Pacer()
