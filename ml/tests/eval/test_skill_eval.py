"""Skill eval harness.

Runs each document/artefact skill end-to-end through the LLM N≥5 times against a
fixed prompt, builds the artefact (which validates it), and reports **pass-rate +
variance** of a quality metric. Gated like the other evals — it needs the live LLM:

    PAI_SKILL_EVAL=1 uv run pytest tests/eval/test_skill_eval.py -q -s

The deterministic, no-LLM half of the QA (the generate→validate pipeline is stable
across N runs) lives in tests/test_document_skills.py and runs in the default suite.
"""

import asyncio
import os
import re
import statistics
import tempfile
from pathlib import Path

import pytest

from app import generate, llm

pytestmark = pytest.mark.skipif(
    os.getenv("PAI_SKILL_EVAL") != "1",
    reason="skill eval needs the live LLM (set PAI_SKILL_EVAL=1)",
)

REPO_ROOT = Path(__file__).resolve().parents[3]
SKILLS_DIR = REPO_ROOT / "skills"
N = 5  # runs per case


def load_skill_body(slug: str) -> str:
    """The SKILL.md instruction body (frontmatter stripped) — fed as the system prompt."""
    md = (SKILLS_DIR / slug / "SKILL.md").read_text(encoding="utf-8")
    m = re.match(r"^---\n.*?\n---\n(.*)$", md, re.DOTALL)
    return (m.group(1) if m else md).strip()


# (slug, kind, sample user prompt, quality metric over the produced content)
CASES = [
    ("docx-report", "docx", "Draft a one-page confidentiality memo about handling client data.",
     lambda c: len(c)),
    ("pdf-report", "pdf", "Write a short briefing on on-premise LLM deployment, with sections.",
     lambda c: c.count("#")),
    ("dashboard", "html", "Build a dashboard of these figures: Litigation 21, Regulatory 13, Contract 8.",
     lambda c: c.count("pai-")),
    ("report-to-page", "html", "## Findings\n\nExposure rose to 4.2m [1].\n\n## References\n1. Register",
     lambda c: c.lower().count("<section") + c.lower().count("<div")),
    ("xlsx-tables", "xlsx", "Make a spreadsheet of Q2 costs: Licences 12 x 250, Support 1 x 4000, with a total.",
     lambda c: c.count("=")),
    ("pptx-deck", "pptx", "Prepare a short board deck on Q2 compliance findings: three controls failed, "
     "£1.2m exposed, overdue reviews rose from 9 in January to 19 in June.",
     lambda c: c.count("layout")),
]


def _run_once(system: str, kind: str, prompt: str, metric) -> tuple[bool, float]:
    content = asyncio.run(llm.complete(system, prompt, max_tokens=4096))
    out = tempfile.mktemp(suffix=f".{kind}")
    try:
        generate.generate_artefact(kind, "Eval", content, out)  # builds + validates
        return True, float(metric(content))
    except Exception as e:  # validation or build failure = a fail for this run
        print(f"    [{kind}] fail: {str(e)[:120]}")
        return False, float(metric(content))
    finally:
        Path(out).unlink(missing_ok=True)


def test_skill_eval():
    overall = []
    print("\n--- skill eval (N=%d each) ---" % N)
    for slug, kind, prompt, metric in CASES:
        system = load_skill_body(slug)
        results = [_run_once(system, kind, prompt, metric) for _ in range(N)]
        passes = [ok for ok, _ in results]
        metrics = [m for ok, m in results if ok]
        rate = sum(passes) / len(passes)
        mean = statistics.mean(metrics) if metrics else 0.0
        stdev = statistics.pstdev(metrics) if len(metrics) > 1 else 0.0
        overall.append(rate)
        print(f"  {slug:16s} pass={rate:.0%}  metric mean={mean:.1f} stdev={stdev:.1f}")
        # Per-skill floor — most runs must produce a valid artefact.
        assert rate >= 0.6, f"{slug} pass-rate {rate:.0%} below floor"
    agg = statistics.mean(overall)
    print(f"  {'OVERALL':16s} pass={agg:.0%}")
    assert agg >= 0.8, f"overall skill pass-rate {agg:.0%} below 0.8"
