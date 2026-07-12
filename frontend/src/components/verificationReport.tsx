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

// "Verify draft" (groundedness Mode B) report. Polls a verification run; shows a
// live "verifying…" state, then the headline grounded score + the claims the
// verifier flagged (contradicted = red, unsupported = amber) with their evidence.
// Reuses the Mode-A .groundedness card + badge/tag styles. Honest framing:
// groundedness against the document's own sources, for a human to review.

import { downloadVerificationReport, useVerificationRun } from "@/api/client";
import { toast } from "@/components/dialogs";
import { Icon } from "@/components/icons";

const band = (score: number) => (score >= 0.85 ? "high" : score >= 0.6 ? "medium" : "low");
const VERDICT_ORDER: Record<string, number> = { contradicted: 0, not_mentioned: 1, supported: 2 };

/** Download the run as MD / PDF / DOCX. Shared by the draft report + live block. */
export function ReportExport({ runId }: { runId: string }) {
  return (
    <div className="gd-export">
      <Icon.Download size={12} />
      {(["md", "pdf", "docx"] as const).map((f) => (
        <button
          key={f}
          className="gd-export-btn mono"
          onClick={() => downloadVerificationReport(runId, f).catch((e) => toast(`Export failed: ${(e as Error).message}`, { variant: "error" }))}
        >
          {f}
        </button>
      ))}
    </div>
  );
}

export function VerificationReport({ runId }: { runId: string }) {
  const run = useVerificationRun(runId);

  if (run.isLoading || !run.data) {
    return <div className="gd-note mono" style={{ padding: "8px 2px" }}>Loading verification…</div>;
  }
  const r = run.data;

  if (r.status === "queued" || r.status === "running") {
    return (
      <div className="groundedness">
        <div className="gd-head">
          <span className="think-dots"><span /><span /><span /></span>
          <span className="gd-sub">Verifying draft — decomposing into claims and checking each against your sources…</span>
        </div>
      </div>
    );
  }

  if (r.status === "error") {
    return (
      <div className="groundedness">
        <div className="gd-head">
          <span className="groundedness-badge low"><Icon.Alert size={12} /> Verification failed</span>
          <span className="gd-sub">The verifier was unavailable — try again.</span>
        </div>
      </div>
    );
  }

  // succeeded
  const score = r.faithfulness_score ?? 0;
  const pct = Math.round(score * 100);
  const flagged = r.contradicted + r.not_mentioned;
  const flaggedClaims = [...r.claims]
    .filter((c) => c.verdict !== "supported")
    .sort((a, b) => (VERDICT_ORDER[a.verdict] ?? 9) - (VERDICT_ORDER[b.verdict] ?? 9));

  return (
    <div className="groundedness fade-up">
      <div className="gd-head">
        <span className={"groundedness-badge " + band(score)}>{pct}% grounded</span>
        <span className="gd-sub">
          {r.supported}/{r.total_claims} supported · {r.contradicted} contradicted · {r.not_mentioned} unsupported
        </span>
      </div>
      <div className="gd-body">
        {flagged === 0 ? (
          <div className="gd-note mono">Every claim is supported by your sources.</div>
        ) : (
          <ul className="gd-spans">
            {flaggedClaims.map((c, i) => {
              const tag = c.verdict === "contradicted" ? "contradicted" : "unsupported";
              const ev = c.evidence.slice(0, 240);
              return (
                <li key={i} className="vr-claim">
                  <div className="vr-claim-head">
                    <span className={"gd-tag " + tag}>{tag}</span>
                    {c.repair_action && c.repair_action !== "kept" && (
                      <span className="gd-repair mono">
                        {c.repair_action === "regenerated" ? "rewrite proposed" : "cut proposed"}
                      </span>
                    )}
                    <span className="vr-claim-text">{c.claim_text}</span>
                  </div>
                  {c.evidence ? (
                    <div className="vr-evidence mono">
                      {c.section}: “{ev}{c.evidence.length > 240 ? "…" : ""}”
                    </div>
                  ) : (
                    <div className="vr-evidence mono">{c.section}: no supporting source found</div>
                  )}
                </li>
              );
            })}
          </ul>
        )}
        <ReportExport runId={runId} />
      </div>
    </div>
  );
}
