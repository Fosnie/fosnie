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

"""Ground-or-cut repair unit checks (groundedness §4.6, §12.6). The decisive
invariant: a regenerated span is only proposed when its NEW citation re-verifies
as `supported`; otherwise the span is CUT, never trusted. No KB / no node / a CUT
reply also cut. Async functions are driven with `asyncio.run` (no pytest-asyncio in
this venv); the three external calls (re-retrieve / regenerate / re-verify) are
monkeypatched. Run from ml/: `uv run python -m pytest tests/test_repair.py`."""

import asyncio
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from app import repair  # noqa: E402

CLAIM = {
    "text": "The tenant may sublet freely.",
    "source_text": "The tenant may sublet freely.",
    "verdict": "contradicted",
    "score": 0.1,
}
GROUNDED = "The tenant must seek consent before subletting."


def _hit(text=GROUNDED, ref="4.1"):
    return {"payload": {"chunk_text": text, "clause_section_ref": ref}}


def _run(claim, kb_ids=("kb1",)):
    async def go():
        return await repair._repair_one(dict(claim), list(kb_ids), asyncio.Semaphore(1))

    return asyncio.run(go())


def _patch(monkeypatch, *, hits, complete=None, verify=None):
    async def _hits(_q, _kb, _sem):
        return hits

    monkeypatch.setattr(repair.retrieve_mod, "_search_one", _hits)
    if complete is not None:

        async def _complete(_system, _user, max_tokens=400):
            return complete

        monkeypatch.setattr(repair.llm, "complete", _complete)
    if verify is not None:

        async def _verify(_pairs, hhem_filter=False):
            return verify

        monkeypatch.setattr(repair.verify_mod, "verify_claims", _verify)


def test_no_kb_cuts():
    assert _run(CLAIM, kb_ids=[])["action"] == "cut"


def test_no_supporting_node_cuts(monkeypatch):
    _patch(monkeypatch, hits=[])
    assert _run(CLAIM)["action"] == "cut"


def test_reverify_failure_cuts_never_trusts_new_citation(monkeypatch):
    # §12.6 — the regenerated span's new citation does NOT re-verify ⇒ CUT.
    _patch(monkeypatch, hits=[_hit()], complete=GROUNDED,
           verify=[{"verdict": "not_mentioned", "score": 0.2}])
    r = _run(CLAIM)
    assert r["action"] == "cut"
    assert r["replacement"] is None


def test_reverify_pass_regenerates(monkeypatch):
    _patch(monkeypatch, hits=[_hit()], complete=GROUNDED,
           verify=[{"verdict": "supported", "score": 0.9}])
    r = _run(CLAIM)
    assert r["action"] == "regenerated"
    assert r["replacement"] == GROUNDED
    assert r["reverify_verdict"] == "supported"


def test_cut_reply_cuts(monkeypatch):
    _patch(monkeypatch, hits=[_hit()], complete="CUT")
    assert _run(CLAIM)["action"] == "cut"


def test_unchanged_but_verified_is_kept(monkeypatch):
    same = CLAIM["source_text"]
    _patch(monkeypatch, hits=[_hit()], complete=same,
           verify=[{"verdict": "supported", "score": 0.8}])
    r = _run(CLAIM)
    assert r["action"] == "kept"
    assert r["replacement"] is None


def test_is_cut_helper():
    assert repair._is_cut("CUT")
    assert repair._is_cut(" cut. ")
    assert repair._is_cut('"CUT"')
    assert not repair._is_cut("The tenant must seek consent.")


if __name__ == "__main__":  # allow `python tests/test_repair.py`
    import pytest

    raise SystemExit(pytest.main([__file__, "-q"]))
