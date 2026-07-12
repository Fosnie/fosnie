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

"""vLLM guided-decoding payload fragments.

Each constant is a dict merged verbatim into the `/chat/completions` body by
`llm.complete` — but ONLY when `settings.llm_guided_decoding` is on (the vLLM
profile). vLLM honours `guided_json` / `guided_choice` at the top level of the
request; Ollama / llama.cpp-server ignore the shape, so off the vLLM profile
these fragments never leave this module and the loops run their prompt-only
path unchanged.

The schemas are module constants on purpose: xgrammar caches the compiled
grammar per-schema (the same decompose schema / same 3-way grade every turn),
and a single source of truth means a future prompt edit can't drift the shape.
They mirror, field-for-field, the JSON each call site already parses defensively
— so the flag-off and flag-on paths produce the same downstream object."""

from typing import Any


def choice(options: list[str]) -> dict[str, Any]:
    """Constrain the decode to exactly one of `options` (vLLM `guided_choice`)."""
    return {"guided_choice": list(options)}


def json_schema(schema: dict[str, Any]) -> dict[str, Any]:
    """Constrain the decode to a JSON value matching `schema` (`guided_json`)."""
    return {"guided_json": schema}


def _array_of(item: dict[str, Any]) -> dict[str, Any]:
    return {"type": "array", "items": item}


def _str_array() -> dict[str, Any]:
    return {"type": "array", "items": {"type": "string"}}


# --- yes / partial / no sufficiency grade (retrieve._grade, web.loop._grade) --
GRADE = choice(["yes", "partial", "no"])

# --- retrieve._decompose: array of {subq, queries[], scope?, part?} -----------
# `scope` and `part` are OPTIONAL (not in `required`), so the flag-off prompt-only
# path and the salvage parser tolerate their absence unchanged. `scope` labels
# synthesis blocks; `part` is the 1-based index of the numbered
# question-part this sub-question serves (null for a non-numbered prompt) — the model
# tags coverage itself so `part_map` is asked, not guessed from token overlap
#.
DECOMPOSE = json_schema(
    _array_of(
        {
            "type": "object",
            "properties": {
                "subq": {"type": "string"},
                "queries": _str_array(),
                "scope": {"type": "string"},
                "part": {"type": ["integer", "null"]},
            },
            "required": ["subq", "queries"],
        }
    )
)

# --- retrieve._gap_check: {sufficient: bool, missing: [{need, sections[], query}]} ---
# after mini-answers + slice assembly, a bounded gap-check asks the
# fast model whether a part's evidence SUFFICES; if not it names the specific provisions/
# information still needed (explicit section numbers when known), which a deterministic fill
# then fetches. `sections` is optional (a topical gap may have none); `need`+`query` required.
GAP = json_schema(
    {
        "type": "object",
        "properties": {
            "sufficient": {"type": "boolean"},
            "missing": _array_of(
                {
                    "type": "object",
                    "properties": {
                        "need": {"type": "string"},
                        "sections": _str_array(),
                        "query": {"type": "string"},
                    },
                    "required": ["need", "query"],
                }
            ),
        },
        "required": ["sufficient", "missing"],
    }
)

# --- web.loop._plan: array of {subq, queries[], freshness-enum} ---------------
WEB_PLAN = json_schema(
    _array_of(
        {
            "type": "object",
            "properties": {
                "subq": {"type": "string"},
                "queries": _str_array(),
                "freshness": {"type": "string", "enum": ["any", "year", "month", "week", "day"]},
            },
            "required": ["subq", "queries", "freshness"],
        }
    )
)

# --- web.loop._detect_conflict: {conflict: bool, topic: str} ------------------
WEB_CONFLICT = json_schema(
    {
        "type": "object",
        "properties": {"conflict": {"type": "boolean"}, "topic": {"type": "string"}},
        "required": ["conflict", "topic"],
    }
)

# --- research.outline.build: array of {heading, brief, note_ids[]} ------------
RESEARCH_OUTLINE = json_schema(
    _array_of(
        {
            "type": "object",
            "properties": {
                "heading": {"type": "string"},
                "brief": {"type": "string"},
                "note_ids": _str_array(),
            },
            "required": ["heading", "brief", "note_ids"],
        }
    )
)

# --- research.triage: {ambiguous, questions:[{id, prompt, options:[…]}]} ------
RESEARCH_TRIAGE = json_schema(
    {
        "type": "object",
        "properties": {
            "ambiguous": {"type": "boolean"},
            "questions": _array_of(
                {
                    "type": "object",
                    "properties": {
                        "id": {"type": "string"},
                        "prompt": {"type": "string"},
                        "options": _array_of(
                            {
                                "type": "object",
                                "properties": {
                                    "label": {"type": "string"},
                                    "scope_indices": {"type": "array", "items": {"type": "integer"}},
                                },
                                "required": ["label", "scope_indices"],
                            }
                        ),
                    },
                    "required": ["id", "prompt", "options"],
                }
            ),
        },
        "required": ["ambiguous", "questions"],
    }
)

# --- research.census._build_struct_note: per-document catalogue note ----------
RESEARCH_CENSUS = json_schema(
    {
        "type": "object",
        "properties": {
            "doc_type": {"type": "string"},
            "themes": _str_array(),
            "claims": _str_array(),
            "entities": _str_array(),
            "dates": _str_array(),
            "open_questions": _str_array(),
            "quotes": _str_array(),
        },
    }
)

# --- research.notes._note_one: {claims:[str], quotes:[str]} -------------------
RESEARCH_NOTES = json_schema(
    {
        "type": "object",
        "properties": {"claims": _str_array(), "quotes": _str_array()},
        "required": ["claims", "quotes"],
    }
)

# --- research.pipeline._plan_subquestions / _gap_subquestions: [str] ----------
RESEARCH_SUBQS = json_schema(_str_array())
