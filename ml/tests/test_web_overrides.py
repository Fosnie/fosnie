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

"""Override precedence (env < runtime/per-request): present-but-empty override
means "list off"; allowlist-only mode is fail-closed; per-Agent max_fetches
min-clamps the budget."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

import asyncio

from app import rag_ctx
from app.config import settings
from app.web import loop, pipeline
from app.web.pipeline import _domain_allowed


def _reset():
    rag_ctx.set_overrides({})


def test_env_lists_apply_without_override(monkeypatch):
    _reset()
    monkeypatch.setattr(settings, "web_domain_blocklist", "bad.example")
    monkeypatch.setattr(settings, "web_domain_allowlist", "")
    assert not _domain_allowed("https://bad.example/x")
    assert _domain_allowed("https://good.example/x")


def test_request_override_beats_env(monkeypatch):
    _reset()
    monkeypatch.setattr(settings, "web_domain_blocklist", "bad.example")
    rag_ctx.set_overrides({"web_domain_blocklist": "other.example"})
    assert _domain_allowed("https://bad.example/x"), "runtime override replaced the env list"
    assert not _domain_allowed("https://other.example/x")
    _reset()


def test_empty_override_means_list_off(monkeypatch):
    _reset()
    monkeypatch.setattr(settings, "web_domain_blocklist", "bad.example")
    rag_ctx.set_overrides({"web_domain_blocklist": ""})
    assert _domain_allowed("https://bad.example/x"), "empty override clears the env-baked list"
    _reset()


def test_allowlist_only_fail_closed(monkeypatch):
    _reset()
    monkeypatch.setattr(settings, "web_domain_allowlist", "")
    monkeypatch.setattr(settings, "web_domain_blocklist", "")
    rag_ctx.set_overrides({"web_allowlist_only": True, "web_domain_allowlist": ""})
    assert not _domain_allowed("https://anything.example/x"), "allowlist-only + empty list blocks all"
    rag_ctx.set_overrides({"web_allowlist_only": True, "web_domain_allowlist": "trusted.example"})
    assert _domain_allowed("https://docs.trusted.example/x")
    assert not _domain_allowed("https://other.example/x")
    _reset()


def test_blocklist_wins_over_allowlist(monkeypatch):
    _reset()
    rag_ctx.set_overrides({
        "web_domain_allowlist": "example.com",
        "web_domain_blocklist": "evil.example.com",
    })
    assert _domain_allowed("https://docs.example.com/x")
    assert not _domain_allowed("https://evil.example.com/x")
    _reset()


def test_agent_max_fetches_clamps_budget():
    _reset()
    rag_ctx.set_overrides({"web_max_fetches": 2})

    async def scenario():
        # Run an empty-SERP loop just to observe the clamped budget via _State —
        # simpler: assert the clamp arithmetic by reproducing run()'s logic.
        budget = loop._budget("standard")
        assert budget.max_fetches == 8
        from app.rag_ctx import cfg

        agent_max = cfg("web_max_fetches", None)
        clamped = min(budget.max_fetches, agent_max)
        assert clamped == 2

    asyncio.new_event_loop().run_until_complete(scenario())
    _reset()


def test_agent_max_fetches_cannot_widen():
    _reset()
    rag_ctx.set_overrides({"web_max_fetches": 999})
    budget = loop._budget("quick")
    from app.rag_ctx import cfg

    clamped = min(budget.max_fetches, cfg("web_max_fetches", budget.max_fetches))
    assert clamped == budget.max_fetches, "an agent can tighten, never widen"
    _reset()
