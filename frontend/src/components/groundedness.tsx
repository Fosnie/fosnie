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

// Inline groundedness block shown under a RAG answer (Mode A). A score pill
// (grounded fraction) +
// the claims flagged as unsupported by the retrieved sources. Honest framing:
// groundedness, NOT truth — it flags claims your own sources don't support, for a
// human to check. Reuses the agent-activity card pattern + design.css keyframes.
//
// v1 scope: the score + a list quoting each flagged span. True inline char-range
// highlighting over the markdown-rendered answer is deferred (it rides Mode B's
// span→char remap) — so we list the spans rather than fake an overlay.

import { useState } from "react";
import { Icon } from "@/components/icons";
import { useLatestVerification, type MsgGroundedness } from "@/api/client";
import { ReportExport } from "@/components/verificationReport";

// Classify by the *verdict mix*, never by the raw fraction — `not_mentioned` (the
// source is silent) is provenance, not a quality failure, and must not wear the same
// red as `contradicted` (the source disagrees).
//   grounded  — nothing flagged (green shield)
//   conflict  — at least one claim contradicts a source (the only loud/red state)
//   offcorpus — nothing supported, nothing contradicted: the answer came from the
//               model's general knowledge, not your documents (neutral, no %)
//   partial   — some claims supported, some merely not in the corpus (calm amber)
type GdState = "grounded" | "conflict" | "offcorpus" | "partial";
const classify = (flagged: number, contradicted: number, supported: number): GdState =>
  flagged === 0 ? "grounded"
    : contradicted > 0 ? "conflict"
    : supported === 0 ? "offcorpus"
    : "partial";

/** Resolve this message's latest verification run and offer report export. */
function MessageReportExport({ messageId }: { messageId: string }) {
  const latest = useLatestVerification("message", messageId);
  if (!latest.data?.id) return null;
  return <ReportExport runId={latest.data.id} />;
}

export function Groundedness({ groundedness, messageId }: { groundedness?: MsgGroundedness | null; messageId?: string }) {
  const [open, setOpen] = useState(false);

  // Render only a real verdict (score present). Verifier-down turns carry no score.
  if (!groundedness || groundedness.score == null) return null;

  const { score, total, flagged, spans } = groundedness;
  const pct = Math.round(score * 100);
  const contradicted = spans.filter((s) => s.label === "contradicted").length;
  const unsupported = flagged - contradicted; // = not_mentioned (source silent)
  const supported = Math.max(total - flagged, 0);
  const state = classify(flagged, contradicted, supported);
  // Only conflict/partial expose a subset of flagged claims worth expanding;
  // off-corpus has nothing actionable (the whole answer is general knowledge).
  const expandable = state === "conflict" || state === "partial";
  const plural = (n: number) => (n === 1 ? "" : "s");

  const icon =
    state === "grounded" ? <Icon.Shield size={12} />
      : state === "offcorpus" ? <Icon.Info size={12} />
      : <Icon.Alert size={12} />;
  // No percentage on the off-corpus chip — it is always 0 and reads as a grade.
  const badgeText = state === "offcorpus" ? "Not from your sources" : `${pct}% grounded`;
  const summary =
    state === "grounded" ? `All ${total} claim${plural(total)} supported by sources`
      : state === "offcorpus" ? "Answered from general knowledge — not found in your documents. Verify independently."
      : state === "conflict"
        ? `${contradicted} claim${plural(contradicted)} conflict with your sources${unsupported ? ` · ${unsupported} not found` : ""}`
        : `${supported} of ${total} claim${plural(total)} supported · ${unsupported} from general knowledge`;

  return (
    <div className={"groundedness fade-up" + (state === "offcorpus" ? " offcorpus" : "")}>
      <button
        className="gd-head"
        onClick={() => expandable && setOpen((v) => !v)}
        type="button"
        style={{ cursor: expandable ? "pointer" : "default" }}
      >
        <span className={"groundedness-badge " + state}>
          {icon}
          {badgeText}
        </span>
        <span className="gd-sub">{summary}</span>
        {expandable ? (
          <Icon.Chevron size={13} className="gd-chev" style={{ transform: open ? "rotate(180deg)" : "none" }} />
        ) : null}
      </button>

      {open && expandable && (
        <div className="gd-body">
          <div className="gd-note mono">Checked against your retrieved sources — review the flagged claims.</div>
          <ul className="gd-spans">
            {spans.map((s, i) => {
              const contradicts = s.label === "contradicted";
              return (
                <li key={i} className="gd-span">
                  <span className={"gd-tag " + (contradicts ? "contradicted" : "unsupported")}>
                    {contradicts ? "contradicted" : "unsupported"}
                  </span>
                  <span className={contradicts ? "claim-contradicted" : "claim-unsupported"}>{s.text}</span>
                </li>
              );
            })}
          </ul>
          {messageId && <MessageReportExport messageId={messageId} />}
        </div>
      )}
    </div>
  );
}
