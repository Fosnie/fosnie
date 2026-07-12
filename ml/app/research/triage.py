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

"""Ambiguity triage: one cheap LLM call asks whether
the research question is unambiguous given the visible scope (libraries the user
can read, with document counts). If not, it proposes ≤3 quick questions as
tappable chips. The LLM references scope entries BY INDEX ONLY — the Rust caller
maps indices → kb_ids deterministically and never trusts an LLM-emitted UUID.

Any failure, or an unparseable response, degrades to "not ambiguous, no
questions" so a run is never blocked by triage."""

import json
import logging

from .. import guided, llm

_log = logging.getLogger("pai.research.triage")

MAX_QUESTIONS = 3
MAX_OPTIONS = 4

_SYSTEM = (
    "You triage a research request before a long, expensive run. Decide whether "
    "the question is unambiguous given the available scope. It is AMBIGUOUS if it "
    "could plausibly mean several different things — e.g. it names an entity that "
    "matches several libraries, omits a needed timeframe, or could target "
    "different document sets. Ask ONLY when a wrong guess would waste the run.\n\n"
    "If ambiguous, return up to 3 short questions, each with up to 4 tappable "
    "options. Reference libraries by their integer INDEX from the scope list "
    "(never by name or id) via `scope_indices`; an option that needs no scope "
    "narrowing (e.g. a timeframe) uses an empty `scope_indices`.\n"
    "Return ONLY JSON: {\"ambiguous\": true|false, \"questions\": [{\"id\": \"q1\", "
    "\"prompt\": \"...\", \"options\": [{\"label\": \"...\", \"scope_indices\": [0,2]}]}]}. "
    "No commentary."
)


def _clean(obj: dict, scope_len: int) -> dict:
    """Enforce the caps and drop out-of-range scope indices."""
    if not obj.get("ambiguous"):
        return {"ambiguous": False, "questions": []}
    questions = []
    for i, q in enumerate(obj.get("questions", [])[:MAX_QUESTIONS]):
        if not isinstance(q, dict):
            continue
        prompt = str(q.get("prompt", "")).strip()
        if not prompt:
            continue
        options = []
        for opt in q.get("options", [])[:MAX_OPTIONS]:
            if not isinstance(opt, dict):
                continue
            label = str(opt.get("label", "")).strip()
            if not label:
                continue
            idx = [
                n
                for n in opt.get("scope_indices", [])
                if isinstance(n, int) and 0 <= n < scope_len
            ]
            options.append({"label": label, "scope_indices": idx})
        if options:
            questions.append({"id": q.get("id") or f"q{i + 1}", "prompt": prompt, "options": options})
    return {"ambiguous": bool(questions), "questions": questions}


async def triage(question: str, source: str, scope: list[dict]) -> dict:
    """`scope[i]` = {index, name, kind, doc_count}. Returns
    {ambiguous, questions:[{id, prompt, options:[{label, scope_indices}]}]}."""
    scope_lines = "\n".join(
        f"[{s['index']}] {s.get('name', '?')} ({s.get('kind', 'library')}, "
        f"{s.get('doc_count', 0)} docs)"
        for s in scope
    )
    user = (
        f"Research source: {source}\n"
        f"Question: {question}\n\n"
        f"Available scope:\n{scope_lines or '(none)'}"
    )
    try:
        llm.set_stage("research.triage")
        llm.set_guided(guided.RESEARCH_TRIAGE)
        out = await llm.complete(_SYSTEM, user, max_tokens=512)
        start, end = out.find("{"), out.rfind("}")
        obj = json.loads(out[start : end + 1]) if start >= 0 else {}
        return _clean(obj, len(scope)) if isinstance(obj, dict) else {"ambiguous": False, "questions": []}
    except Exception as e:  # noqa: BLE001 — never block the run on triage
        _log.warning("triage failed (no questions): %s", e)
        return {"ambiguous": False, "questions": []}
