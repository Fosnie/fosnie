// Copyright 2026 Private AI Ltd (SC881079)
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

// The Deep Research roadmap: the report's full ordered section list, known once
// the outline is built, ticked off (✓) as each section is written. Two surfaces:
// a right-docked LIVE panel during a run, and a
// static "Research steps" recap folded under the finished report (read from the
// message's persisted activity).

import { Icon } from "@/components/icons";

type Step = "done" | "active" | "todo";

/** Human labels for the DR macro-phases emitted as `research.progress`. */
export const STAGE_LABELS: Record<string, string> = {
  census: "Reading the corpus",
  plan: "Planning",
  collect: "Gathering sources",
  notes: "Reading & noting",
  outline: "Building outline",
  write: "Writing",
  cohere: "Refining coherence",
  check: "Final checks",
  verify: "Checking citations",
  deliver: "Finalising the report",
};

/** Ordered macro-stages shown in the roadmap, by run source. Skipped stages still
 * render but resolve to "done" once the run moves past them (graceful). */
export function stagesFor(source: string | undefined): string[] {
  if (source === "files") return ["census", "notes", "outline", "write", "cohere", "check", "deliver"];
  if (source === "hybrid") return ["census", "plan", "collect", "notes", "outline", "write", "cohere", "check", "deliver"];
  return ["plan", "collect", "notes", "outline", "write", "cohere", "check", "deliver"];
}

/** The bubble's one-line current-stage label: the active stage, or on `write`
 * "Writing · {section} · N/total". */
export function currentLabel(
  phase: string | undefined,
  sections: string[],
  done: number,
): string {
  if (!phase) return "Deep research";
  const label = STAGE_LABELS[phase] ?? phase;
  if (phase === "write" && sections.length) {
    const heading = sections[Math.min(done, sections.length - 1)];
    return `Writing · ${heading} · ${Math.min(done + 1, sections.length)}/${sections.length}`;
  }
  return label;
}

/** The composer pill's compact micro-step: the macro label plus the finer
 * `detail`/counter the bubble omits ("Gathering sources · 12 web sources",
 * "Writing · Recommendations"). Finer granularity than `currentLabel`, so the two
 * do not duplicate. */
export function stepLabel(
  phase: string | undefined,
  detail?: string,
  sourcesRead?: number,
): string {
  if (!phase) return "Deep research";
  const label = STAGE_LABELS[phase] ?? phase;
  if (detail) return `${label} · ${detail}`;
  if (sourcesRead != null) return `${label} · ${sourcesRead} sources`;
  return label;
}

/** One section's state derived from the running `done` count. */
function mark(index: number, done: number): Step {
  if (index < done) return "done";
  if (index === done) return "active";
  return "todo";
}

/** A macro-stage's state from the current phase's position in the ordered list. */
function stageState(stageIdx: number, currentIdx: number): Step {
  if (currentIdx < 0) return "todo";
  if (stageIdx < currentIdx) return "done";
  if (stageIdx === currentIdx) return "active";
  return "todo";
}

function Mark({ state, sub }: { state: Step; sub?: boolean }) {
  return (
    <span className={sub ? "roadmap-mark roadmap-mark-sub" : "roadmap-mark"}>
      {state === "done" ? <Icon.Check size={sub ? 12 : 13} /> : state === "active" ? <span className="think-dots"><span /><span /><span /></span> : <span className="roadmap-dot" />}
    </span>
  );
}

function SectionRow({ heading, state }: { heading: string; state: Step }) {
  return (
    <div className={`roadmap-row roadmap-sub roadmap-${state}`}>
      <Mark state={state} sub />
      <span className="roadmap-heading">{heading}</span>
    </div>
  );
}

/** The live, right-docked panel shown while a run streams. Renders the macro-stage
 * roadmap from the first progress event; the `write` stage expands into the section
 * list (ticked by `done`/`sections_done`) once the outline is known. `sources` =
 * attached KB names (corpus runs). */
export function ResearchRoadmapPanel({
  stages,
  phase,
  sections,
  done,
  sources,
}: {
  stages: string[];
  phase?: string;
  sections: string[];
  done: number;
  sources?: string[];
}) {
  const currentIdx = phase ? stages.indexOf(phase) : -1;
  return (
    <aside className="roadmap-panel glass glass--bar">
      <div className="roadmap-head mono">
        <Icon.Research size={14} /> Research roadmap
      </div>
      {sources && sources.length > 0 && (
        <div className="roadmap-sources">Sources: {sources.map((s) => `“${s}”`).join(", ")}</div>
      )}
      <div className="roadmap-list">
        {stages.map((stage, i) => {
          const state = stageState(i, currentIdx);
          return (
            <div key={stage}>
              <div className={`roadmap-row roadmap-${state}`}>
                <Mark state={state} />
                <span className="roadmap-heading">{STAGE_LABELS[stage] ?? stage}</span>
              </div>
              {/* Expand the Write stage into the section checklist once known. */}
              {stage === "write" && sections.length > 0 && (
                <div className="roadmap-sublist">
                  {sections.map((h, si) => (
                    <SectionRow key={si} heading={h} state={mark(si, done)} />
                  ))}
                </div>
              )}
            </div>
          );
        })}
      </div>
    </aside>
  );
}

/** The static recap folded under a finished report (all sections done). Reads the
 * persisted roadmap from the message's activity. */
export function ResearchSteps({
  sections,
  phases,
}: {
  sections: string[];
  phases?: { phase: string; detail?: string | null; at: number }[];
}) {
  if (!sections.length) return null;
  return (
    <details className="research-steps">
      <summary className="mono">Research steps · {sections.length} sections</summary>
      <div className="roadmap-list mt-2">
        {sections.map((h, i) => (
          <SectionRow key={i} heading={h} state="done" />
        ))}
      </div>
      {phases && phases.length > 0 && (
        <div className="roadmap-timeline mono">
          {phases.map((p, i) => (
            <div key={i} className="roadmap-tl-row">
              <span className="roadmap-tl-at">{p.at}s</span>
              <span className="roadmap-tl-phase">{p.phase}{p.detail ? ` · ${p.detail}` : ""}</span>
            </div>
          ))}
        </div>
      )}
    </details>
  );
}
