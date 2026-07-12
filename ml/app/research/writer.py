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

"""The per-section writer (step 3): each section
call sees the outline skeleton, ITS OWN bound notes, a rolling summary of the
sections already written, and an explicit no-repeat register. Citations are
emitted ONLY as source IDs ([W#]) — the model never writes URLs or titles from
memory. The pipeline owns headings; any the model re-emits are stripped."""

import logging
import re

from .. import llm
from . import progress
from .bank import Bank
from .budgets import ResearchBudgets
from .outline import Outline

_log = logging.getLogger("pai.research.writer")

_HEADING_RE = re.compile(r"^\s{0,3}#{1,6}\s+.*$", re.MULTILINE)


def _outline_view(outline: Outline) -> str:
    return "\n".join(f"{i + 1}. {s.heading} — {s.brief}" for i, s in enumerate(outline.sections))


def _notes_block(bank: Bank, note_ids: list[str], budget_tokens: int) -> str:
    parts: list[str] = []
    for rec in bank.resolve(note_ids):
        body = rec.note.text() if rec.note else ""
        parts.append(f"{rec.meta_line()}\n{body}")
    text = "\n\n".join(parts)
    return text[: budget_tokens * 4]


def strip_headings(text: str) -> str:
    """Remove any markdown headings the model emitted — the pipeline numbers
    and owns section headings."""
    return _HEADING_RE.sub("", text).strip()


async def _stream_body(system: str, user: str, max_tokens: int) -> str:
    """Stream a section body, emitting prose tokens live via `progress.emit_token`
    and returning the accumulated body. Reasoning (`<think>…</think>`) is filtered
    out so only prose streams — matching the non-streaming `llm.complete` body,
    which carries no reasoning. Returns "" on failure (caller degrades)."""
    acc: list[str] = []
    in_think = False
    llm.set_stage("research.write_section")
    async for ev in llm.stream_chat(
        [{"role": "system", "content": system}, {"role": "user", "content": user}],
        {"max_tokens": max_tokens, "temperature": 0},
    ):
        if ev.get("type") != "token":
            continue
        delta = ev.get("delta") or ""
        if delta == "<think>":
            in_think = True
            continue
        if delta == "</think>":
            in_think = False
            continue
        if in_think:
            continue
        acc.append(delta)
        await progress.emit_token(delta)
    return "".join(acc)


async def write_section(
    k: int,
    outline: Outline,
    bank: Bank,
    rolling_summary: str,
    register: list[str],
    instructions: str,
    b: ResearchBudgets,
    stream: bool = False,
) -> str:
    """Write section k (0-based). Returns body text (no heading). Any failure
    returns a minimal evidence-quoting body — never raises. When `stream` is set,
    the body's prose tokens are emitted live via `progress.emit_token` as it is
    written (the report types into the chat); the returned text is identical to the
    non-streaming path so downstream assembly is unaffected."""
    section = outline.sections[k]
    system = (
        f"{instructions}\n\n"
        "Hard rules:\n"
        "- Cite ONLY with source markers like [W3] (web source) or [D2] (the user's "
        "document), and ONLY IDs present in the notes below. NEVER write URLs, "
        "publication names, filenames or titles from memory.\n"
        "- Do not repeat anything in the already-covered register.\n"
        f"- Target {b.section_words_lo}-{b.section_words_hi} words.\n"
        "- Write body prose only — no headings, no reference list (both are added "
        "for you), no preamble about what you are doing."
    )
    register_block = "\n".join(f"- {r}" for r in register[-40:]) or "(nothing yet)"
    user = (
        f"Report outline:\n{_outline_view(outline)}\n\n"
        f"You are writing section {k + 1}: \"{section.heading}\" — {section.brief}\n\n"
        f"Evidence notes for THIS section:\n{_notes_block(bank, section.note_ids, b.writer_input_tokens)}\n\n"
        f"Summary of sections already written:\n{rolling_summary or '(none yet)'}\n\n"
        f"Already covered — do NOT repeat:\n{register_block}"
    )
    try:
        # Stream only when a token consumer is listening (the streaming run);
        # otherwise fall back to complete() so non-streaming callers and tests
        # (which stub complete, not stream_chat) are unaffected.
        if stream and progress.token_emitter_installed():
            out = await _stream_body(system, user, b.section_max_tokens)
        else:
            llm.set_stage("research.write_section")
            out = await llm.complete(system, user, max_tokens=b.section_max_tokens)
        body = strip_headings(out)
        if body:
            return body
    except Exception as e:  # noqa: BLE001
        _log.warning("section %d writer failed: %s", k + 1, e)
    # Degraded body: quote the evidence directly rather than deliver nothing.
    fallback = []
    for rec in bank.resolve(section.note_ids)[:4]:
        if rec.note and rec.note.claims:
            fallback.append(f"{rec.note.claims[0]} [{rec.sid}]")
    return "\n\n".join(fallback) or "(No usable evidence was gathered for this section.)"


async def update_rolling(summary: str, section_heading: str, section_text: str) -> str:
    """Fold a finished section into the rolling summary (one cheap LLM call;
    deterministic truncate-concat fallback)."""
    try:
        llm.set_stage("research.update_summary")
        out = await llm.complete(
            "Update the running summary of a report in ≤150 words: fold in the new "
            "section's key points. Plain prose, no headings.",
            f"Summary so far:\n{summary or '(none)'}\n\nNew section ({section_heading}):\n{section_text}",
            max_tokens=256,
        )
        if out.strip():
            return out.strip()
    except Exception as e:  # noqa: BLE001
        _log.debug("rolling-summary call failed (concat fallback): %s", e)
    lead = " ".join(section_text.split()[:60])
    return (f"{summary} {section_heading}: {lead}").strip()[-2400:]


def extend_register(register: list[str], section_heading: str, section_text: str) -> None:
    """Deterministic no-repeat register: the section heading + the first
    sentence of each paragraph."""
    register.append(section_heading)
    for para in section_text.split("\n\n"):
        first = para.strip().split(". ")[0].strip()
        if 20 <= len(first) <= 200:
            register.append(first)
