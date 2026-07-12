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

"""Politeness pacing — token buckets + cooldowns (the no-IP-ban guarantee)."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

import asyncio
import time

from app.web.pacing import Pacer


def _run(coro):
    return asyncio.new_event_loop().run_until_complete(coro)


def test_burst_then_rate_limited():
    async def scenario():
        p = Pacer()
        start = time.monotonic()
        # Burst of 2 passes immediately; the 3rd must wait ~1/rate seconds.
        await p.acquire("engine:x", rate=10.0, burst=2.0)
        await p.acquire("engine:x", rate=10.0, burst=2.0)
        burst_elapsed = time.monotonic() - start
        await p.acquire("engine:x", rate=10.0, burst=2.0)
        total_elapsed = time.monotonic() - start
        return burst_elapsed, total_elapsed

    burst_elapsed, total_elapsed = _run(scenario())
    assert burst_elapsed < 0.05, "burst tokens are immediate"
    assert total_elapsed >= 0.08, "third acquire waited for a token (~0.1s at 10 rps)"


def test_keys_are_independent():
    async def scenario():
        p = Pacer()
        await p.acquire("host:a", rate=10.0, burst=1.0)
        start = time.monotonic()
        await p.acquire("host:b", rate=10.0, burst=1.0)  # different key — no wait
        return time.monotonic() - start

    assert _run(scenario()) < 0.05


def test_cooldown_set_and_expiry():
    p = Pacer()
    assert not p.cooling("engine:x")
    p.set_cooldown("engine:x", 0.05)
    assert p.cooling("engine:x")
    time.sleep(0.08)
    assert not p.cooling("engine:x"), "cooldown self-heals after the deadline"


def test_zero_rate_guarded():
    async def scenario():
        p = Pacer()
        # A misconfigured rate of 0 must not divide-by-zero or hang forever
        # (clamped to a slow-but-positive rate); burst covers the first call.
        await p.acquire("engine:x", rate=0.0, burst=1.0)

    _run(scenario())
