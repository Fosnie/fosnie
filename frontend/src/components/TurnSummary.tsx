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

// A finished turn's "what I did", under the answer: the files it changed and the
// commands it ran, drawn entirely from what is already recorded on the message so
// it costs no extra work and no model call. A turn that changed nothing shows
// nothing. On the desktop the files come with a way to put them back; in a browser
// the same list shows but the change lives on somebody else's computer, so it is
// read-only. It is deliberately facts, not prose: the answer already said what it
// was doing, this says what it touched.

import { useState } from "react";

import { Icon } from "@/components/icons";
import { RestoreBlock } from "@/components/RestoreBlock";
import { isShell } from "@/shell/detect";
import type { MsgActivity } from "@/api/client";

type CommandRun = NonNullable<MsgActivity["commands"]>[number];

/** A duration in ms as a short human string ("820ms", "3.4s", "1m 05s"). */
export function fmtDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  const s = ms / 1000;
  if (s < 60) return `${s.toFixed(s < 10 ? 1 : 0)}s`;
  const m = Math.floor(s / 60);
  return `${m}m ${String(Math.round(s % 60)).padStart(2, "0")}s`;
}

/** Whether a turn changed anything worth summarising. A turn that only talked has
 *  no summary at all. */
export function summaryHasContent(activity?: MsgActivity | null): boolean {
  return !!(activity?.commands?.length || activity?.files?.length);
}

export function TurnSummary({
  activity,
  turnId,
  startedAt,
  workspaceId,
}: {
  activity?: MsgActivity | null;
  turnId?: string;
  startedAt?: number;
  workspaceId?: string;
}) {
  const commands = activity?.commands ?? [];
  const files = activity?.files ?? [];
  const steps = activity?.steps ?? [];

  // A turn with no side effects has nothing to summarise: stay quiet.
  if (!summaryHasContent(activity)) return null;

  const done = steps.filter((s) => s.status === "done").length;
  // Duration is best-effort: we can measure it only while we still hold the turn's
  // start (a live or just-finished turn). A cold reload has no end time to compare
  // against, so the header simply omits it rather than inventing one.
  const durationMs = startedAt ? Math.max(0, Date.now() - startedAt) : null;

  return (
    <div className="turn-summary">
      <div className="turn-summary-head mono">
        What I did
        {steps.length ? <span className="turn-summary-meta"> · {done}/{steps.length} steps</span> : null}
        {durationMs != null ? <span className="turn-summary-meta"> · {fmtDuration(durationMs)}</span> : null}
      </div>

      {files.length ? (
        isShell() ? (
          // The desktop holds the real record of what changed and can put it back.
          <RestoreBlock turnId={turnId} workspaceId={workspaceId} />
        ) : (
          <div className="turn-summary-section">
            <div className="menu-label mono">Files</div>
            {files.map((f, i) => (
              <div key={i} className="turn-summary-file">
                {f.op === "delete" ? <Icon.Close size={13} /> : <Icon.Edit size={13} />}
                <span className="mono turn-summary-path">{f.path}</span>
                <button
                  className="btn btn-line sm"
                  disabled
                  title="This change is on the desktop; open or put it back there"
                >
                  <Icon.Folder size={12} />
                </button>
              </div>
            ))}
          </div>
        )
      ) : null}

      {commands.length ? (
        <div className="turn-summary-section">
          <div className="menu-label mono">Commands</div>
          {commands.map((c, i) => (
            <CommandRow key={i} run={c} />
          ))}
        </div>
      ) : null}
    </div>
  );
}

function CommandRow({ run }: { run: CommandRun }) {
  const [open, setOpen] = useState(false);
  const hasTail = !!run.stdout_tail;
  return (
    <div className="turn-summary-cmd">
      <button
        className="turn-summary-cmd-head"
        type="button"
        onClick={() => hasTail && setOpen((v) => !v)}
        style={{ cursor: hasTail ? "pointer" : "default" }}
      >
        <Icon.Code size={13} />
        <span className="mono turn-summary-cmd-line">{run.command || "code"}</span>
        {run.exit_code != null ? (
          <span className={"turn-summary-exit mono" + (run.exit_code === 0 ? " ok" : " bad")}>
            exit {run.exit_code}
          </span>
        ) : null}
        <span className="turn-summary-dur mono">
          <Icon.Clock size={11} /> {fmtDuration(run.duration_ms)}
        </span>
        {hasTail ? (
          <Icon.Chevron size={12} className="turn-summary-chev" style={{ transform: open ? "rotate(180deg)" : "none" }} />
        ) : null}
      </button>
      {open && hasTail ? <pre className="mono turn-summary-out">{run.stdout_tail}</pre> : null}
    </div>
  );
}
