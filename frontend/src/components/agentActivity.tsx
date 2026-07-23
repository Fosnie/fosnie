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

// Inline agent-activity timeline shown directly under an assistant message: the
// model's track_steps plan flips pending→running→done with a gold pulse while it
// works, plus the tools it used and any approval gate. Live during the turn,
// collapses to "Agent activity · N steps" when done; the data is persisted on the
// message so it survives a reload. Animations reuse design.css keyframes.

import { useEffect, useState } from "react";
import { Icon } from "@/components/icons";
import type { MsgActivity } from "@/api/client";
import { FolderApprovalCard, asFolderDetail } from "@/components/FolderApproval";
import { isShell } from "@/shell/detect";

const TOOL_LABELS: Record<string, string> = {
  read_document: "Read a document",
  list_documents: "Listed documents",
  list_workspace_documents: "Listed documents",
  read_table_cells: "Read table cells",
  edit_document: "Edited a document",
  generate_artefact: "Generated a document",
  code_interpreter: "Ran code",
  web_search: "Searched the web",
  retrieve: "Searched the library",
  search_library: "Searched the library again",
  read_skill: "Read a skill",
  remember_fact: "Saved a memory",
  current_time: "Checked the time",
};
const prettyTool = (t: string) => TOOL_LABELS[t] ?? t.replace(/_/g, " ");
// Present-tense labels for the LIVE "using a tool" row — TOOL_LABELS above are
// past-tense (for the completed timeline), so reusing them yielded "Using Searched
// the library…". Falls back to the past-tense label, then the de-snaked name.
const ACTIVE_TOOL_LABELS: Record<string, string> = {
  read_document: "Reading a document",
  list_documents: "Listing documents",
  list_workspace_documents: "Listing documents",
  read_table_cells: "Reading table cells",
  edit_document: "Editing a document",
  generate_artefact: "Generating a document",
  code_interpreter: "Running code",
  web_search: "Searching the web",
  retrieve: "Searching the library",
  search_library: "Searching the library again",
  read_skill: "Reading a skill",
  remember_fact: "Saving a memory",
  current_time: "Checking the time",
};
const activeTool = (t: string) => ACTIVE_TOOL_LABELS[t] ?? prettyTool(t);
const mmss = (s: number) => `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;

export function AgentActivity({
  activity,
  live,
  startedAt,
  runningTool,
  runningDetail,
  approval,
  onApprove,
  onReject,
  onAllowPrefix,
  terminalOut,
  onKillTerminal,
  restore,
}: {
  activity?: MsgActivity | null;
  live?: boolean;
  startedAt?: number;
  runningTool?: string | null;
  /** Live progress detail from a streaming tool (e.g. "round 2: reading example.com"). */
  runningDetail?: string | null;
  approval?: { tool: string; summary: string; detail?: Record<string, unknown> | null } | null;
  onApprove?: () => void;
  onReject?: () => void;
  /** Agree a command prefix for this folder, so the same run is not asked about again. */
  onAllowPrefix?: (prefix: string) => Promise<void>;
  /** Output of a command running in a connected folder, as it arrives. */
  terminalOut?: string | null;
  /** Stop the command running in the folder. */
  onKillTerminal?: () => void;
  /** The end-of-turn "what changed, and undo it" block (desktop only). */
  restore?: React.ReactNode;
}) {
  const steps = activity?.steps ?? [];
  const tools = activity?.tools ?? [];
  const coverage = activity?.coverage ?? null;
  const count = steps.length || tools.length || (coverage ? 1 : 0);

  // Open while live; collapse to the summary once the turn finishes.
  const [open, setOpen] = useState(!!live);
  useEffect(() => {
    if (!live) setOpen(false);
  }, [live]);

  // Elapsed timer while live (same pattern as the Reasoning panel).
  const [secs, setSecs] = useState(startedAt ? Math.max(0, Math.round((Date.now() - startedAt) / 1000)) : 0);
  useEffect(() => {
    if (!live || !startedAt) return;
    const t = setInterval(() => setSecs(Math.max(0, Math.round((Date.now() - startedAt) / 1000))), 1000);
    return () => clearInterval(t);
  }, [live, startedAt]);

  // A live "using a tool…" row only when there's no track_steps plan (the plan's
  // own running marker conveys progress otherwise).
  const showRunning = !!live && !!runningTool && steps.length === 0;

  // A command running in a connected folder, with its output and a way to stop it.
  const runningInFolder = !!live && runningTool === "desktop.terminal_run";

  // Nothing to show: no plan, no tools, not working, no approval, no undo block.
  if (count === 0 && !live && !approval && !restore) return null;

  return (
    <div className="agent-activity fade-up">
      <button className="aa-head" onClick={() => setOpen((v) => !v)} type="button">
        {live ? (
          <span className="think-dots"><span /><span /><span /></span>
        ) : (
          <Icon.Automations size={13} />
        )}
        <span className={"aa-title" + (live ? " shimmer-text" : "")}>{live ? "Working…" : `Agent activity${count ? ` · ${count} step${count === 1 ? "" : "s"}` : ""}`}</span>
        {live && startedAt ? <span className="aa-time mono">{mmss(secs)}</span> : null}
        {!live && count > 0 ? (
          <Icon.Chevron size={13} className="aa-chev" style={{ transform: open ? "rotate(180deg)" : "none" }} />
        ) : null}
      </button>

      {open && (
        <div className="aa-body">
          {steps.length > 0 ? (
            <div className="aa-timeline">
              {steps.map((s, i) => <StepRow key={i} title={s.title} status={s.status} />)}
              {!!live && runningDetail ? (
                <div className="aa-step">
                  <span className="aa-dot running" />
                  <span className="aa-step-title mono">{runningDetail}</span>
                </div>
              ) : null}
            </div>
          ) : tools.length > 0 || showRunning ? (
            <div className="aa-timeline">
              {tools.map((t, i) => (
                <div key={i} className="aa-step">
                  <Icon.Check size={14} className="aa-check" />
                  <span className="aa-step-title">{prettyTool(t)}</span>
                </div>
              ))}
              {showRunning && (
                <div className="aa-step">
                  <span className="aa-dot running" />
                  <span className="aa-step-title">
                    {activeTool(runningTool!)}…
                    {runningDetail ? <span className="aa-detail mono"> — {runningDetail}</span> : null}
                  </span>
                </div>
              )}
            </div>
          ) : live && runningDetail ? (
            // no plan, no tool, just a moving phase label (RAG phases
            // / "Thinking · step k of N") — the currentLabel reuse so the UI never sits
            // on a static "Working…".
            <div className="aa-timeline">
              <div className="aa-step">
                <span className="aa-dot running" />
                <span className="aa-step-title mono">{runningDetail}</span>
              </div>
            </div>
          ) : null}

          {/*: the retrieval Coverage summary as a persistent completed
              step (live + on reload) — the acceptance channel for every retrieval TZ. */}
          {coverage ? (
            <div className="aa-timeline">
              <div className="aa-step">
                <Icon.Check size={14} className="aa-check" />
                <span className="aa-step-title">{coverage}</span>
              </div>
            </div>
          ) : null}

          {steps.length > 0 && tools.length > 0 && (
            <div className="aa-tools mono">Tools: {tools.map(prettyTool).join(", ")}</div>
          )}

          {approval ? (
            // A folder action is shown as the change itself; anything else keeps
            // the sentence — an older instance sends no detail, and a client that
            // meets one must still be able to ask the question.
            (() => {
              const folder = asFolderDetail(approval.detail);
              if (folder) {
                return (
                  <FolderApprovalCard
                    detail={folder}
                    terminalOut={terminalOut ?? undefined}
                    onApprove={() => onApprove?.()}
                    onReject={() => onReject?.()}
                    onAllowPrefix={async (p) => { await onAllowPrefix?.(p); }}
                  />
                );
              }
              return (
                <div className="approval-card aa-approval">
                  <div className="approval-head"><Icon.Shield size={13} /> Approval needed</div>
                  <div className="approval-summary">{approval.summary}</div>
                  <div className="approval-actions">
                    <button className="btn btn-gold sm" onClick={onApprove}><Icon.Check size={14} /> Approve</button>
                    <button className="btn btn-line sm" onClick={onReject}><Icon.Close size={14} /> Reject</button>
                  </div>
                </div>
              );
            })()
          ) : null}

          {/* A command's output as it runs, and — on the desktop — a way to stop
              it. In a browser the button is disabled: the command is on somebody's
              own computer, and stopping it is done there. */}
          {runningInFolder && (terminalOut || onKillTerminal) ? (
            <div className="aa-terminal">
              {terminalOut ? (
                <pre className="mono" style={{ margin: "6px 0 0", maxHeight: 200, overflow: "auto", fontSize: "0.72rem", background: "var(--bg-1)", border: "1px solid var(--line-2)", borderRadius: 6, padding: "8px 10px", whiteSpace: "pre-wrap", wordBreak: "break-word" }}>
                  {terminalOut}
                </pre>
              ) : null}
              <button
                className="btn btn-line sm"
                style={{ marginTop: 6 }}
                disabled={!isShell()}
                title={isShell() ? "Stop this command" : "The command is running on your desktop; stop it there"}
                onClick={() => onKillTerminal?.()}
              >
                <Icon.Stop size={13} /> Stop command
              </button>
            </div>
          ) : null}
        </div>
      )}

      {/* The undo block sits outside the collapsible body: a turn's activity
          folds away when it finishes, but the offer to put its changes back has
          to stay reachable. */}
      {restore ?? null}
    </div>
  );
}

function StepRow({ title, status }: { title: string; status: string }) {
  const done = status === "done";
  const running = status === "running";
  const skipped = status === "skipped";
  return (
    <div className="aa-step">
      {done ? (
        <Icon.Check size={14} className="aa-check" />
      ) : (
        <span className={"aa-dot " + (running ? "running" : skipped ? "skipped" : "pending")} />
      )}
      <span className={"aa-step-title" + (done ? " done" : "") + (skipped ? " skipped" : "")}>{title}</span>
    </div>
  );
}
