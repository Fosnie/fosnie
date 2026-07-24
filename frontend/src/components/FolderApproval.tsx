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

// Agreeing to a change in a connected folder, as the change itself.
//
// A write is shown as the difference it would make; a command as the command and
// where it runs, with the option to stop being asked about it; a deletion as
// what would go. The point of showing the change rather than a sentence about it
// is the difference between agreeing to something and waving it through.
//
// On the desktop the difference is real: the file is on this machine, and the
// client computes it. In a browser the same card shows the intended contents and
// says plainly that the difference is on the desktop — approving still works from
// anywhere, which is the whole symbiosis.

import { useEffect, useState } from "react";

import { Icon } from "@/components/icons";
import { toast } from "@/components/dialogs";
import { isShell } from "@/shell/detect";
import { allowCommandPrefix } from "@/api/client";
import { previewChange, type Preview } from "@/shell/folders";
import { asFolderDetail, type FolderDetail } from "@/shell/folderDetail";

export { asFolderDetail };
export type { FolderDetail };

/** A few lines of a unified diff, collapsed past a threshold. */
function Diff({ text, added, removed }: { text: string; added: number; removed: number }) {
  const lines = text.split("\n");
  const [open, setOpen] = useState(lines.length <= 40);
  const shown = open ? lines : lines.slice(0, 32);
  return (
    <div>
      <div className="mono" style={{ fontSize: "0.72rem", color: "var(--ink-3)", marginBottom: 4 }}>
        +{added} −{removed}
      </div>
      <pre className="mono" style={{ margin: 0, maxHeight: open ? "none" : 360, overflow: "auto", fontSize: "0.74rem", lineHeight: 1.45, background: "var(--bg-1)", border: "1px solid var(--line-2)", borderRadius: 6, padding: "8px 10px" }}>
        {shown.map((l, i) => (
          <div key={i} style={{ color: l.startsWith("+") ? "var(--green, #3ba55d)" : l.startsWith("-") ? "var(--red)" : "var(--ink-2)", whiteSpace: "pre-wrap", wordBreak: "break-word" }}>
            {l || " "}
          </div>
        ))}
      </pre>
      {!open && lines.length > 32 ? (
        <button className="btn btn-line sm" style={{ marginTop: 6 }} onClick={() => setOpen(true)}>Show all {lines.length} lines</button>
      ) : null}
    </div>
  );
}

function DiffCard({ detail }: { detail: FolderDetail }) {
  const shell = isShell();
  const [preview, setPreview] = useState<Preview | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let live = true;
    if (shell && detail.workspace_id && detail.path != null && detail.new_content != null) {
      previewChange(detail.workspace_id, detail.path, detail.new_content)
        .then((p) => { if (live) setPreview(p); })
        .catch(() => { if (live) setFailed(true); });
    }
    return () => { live = false; };
  }, [shell, detail.workspace_id, detail.path, detail.new_content]);

  return (
    <div>
      <div className="mono" style={{ fontSize: "0.76rem", color: "var(--ink-2)", marginBottom: 6 }}>{detail.path}</div>
      {preview && !preview.binary ? (
        preview.unified.trim()
          ? <Diff text={preview.unified} added={preview.added} removed={preview.removed} />
          : <div style={{ fontSize: "0.8rem", color: "var(--ink-3)" }}>No change: the file already has these contents.</div>
      ) : preview?.binary ? (
        <div style={{ fontSize: "0.8rem", color: "var(--ink-3)" }}>This file is not text, so a line-by-line difference cannot be shown.</div>
      ) : shell && !failed ? (
        <div style={{ fontSize: "0.8rem", color: "var(--ink-3)" }}>Working out the difference…</div>
      ) : (
        // A browser cannot see the file: show what would be written, and say so.
        <div>
          <pre className="mono" style={{ margin: 0, maxHeight: 300, overflow: "auto", fontSize: "0.74rem", background: "var(--bg-1)", border: "1px solid var(--line-2)", borderRadius: 6, padding: "8px 10px", whiteSpace: "pre-wrap", wordBreak: "break-word" }}>
            {detail.new_content}
          </pre>
          <div style={{ fontSize: "0.72rem", color: "var(--ink-3)", marginTop: 5 }}>
            The exact difference is shown on the desktop, where the file is. This is the intended contents.
          </div>
        </div>
      )}
    </div>
  );
}

export function FolderApprovalCard({
  detail,
  terminalOut,
  resolved,
  onApprove,
  onReject,
  onAllowPrefix,
}: {
  detail: FolderDetail;
  terminalOut?: string;
  /** Set once the gate has been decided (here or on another device): the buttons
   *  give way to a settled label. */
  resolved?: "pending" | "approved" | "closed";
  onApprove: () => void;
  onReject: () => void;
  onAllowPrefix: (prefix: string) => Promise<void>;
}) {
  const [allowing, setAllowing] = useState(false);
  const isResolved = !!resolved && resolved !== "pending";

  const head = detail.kind === "diff" ? "Write a file"
    : detail.kind === "delete" ? "Delete a file"
    : "Run a command";
  const HeadIcon = detail.kind === "delete" ? Icon.Close : detail.kind === "command" ? Icon.Play : Icon.Edit;

  return (
    <div className={"approval-card aa-approval" + (detail.kind === "delete" ? " danger" : "")}
         style={detail.kind === "delete" ? { borderColor: "var(--red)" } : undefined}>
      <div className="approval-head"><HeadIcon size={13} /> {head}</div>

      {detail.kind === "diff" ? <DiffCard detail={detail} /> : null}

      {detail.kind === "delete" ? (
        <div className="mono" style={{ fontSize: "0.8rem", color: "var(--red)" }}>{detail.full_path ?? detail.path}</div>
      ) : null}

      {detail.kind === "command" ? (
        <div>
          <pre className="mono" style={{ margin: 0, fontSize: "0.78rem", background: "var(--bg-1)", border: "1px solid var(--line-2)", borderRadius: 6, padding: "8px 10px", whiteSpace: "pre-wrap", wordBreak: "break-word" }}>
            {detail.command}
          </pre>
          <div className="mono" style={{ fontSize: "0.7rem", color: "var(--ink-3)", marginTop: 4 }}>in {detail.cwd}</div>
        </div>
      ) : null}

      {isResolved ? (
        <div className="approval-resolved mono">
          {resolved === "approved"
            ? <><Icon.Check size={13} /> Approved</>
            : "This was resolved on another device"}
        </div>
      ) : (
        <div className="approval-actions" style={{ flexWrap: "wrap" }}>
          <button className="btn btn-gold sm" onClick={onApprove}><Icon.Check size={14} /> {detail.kind === "command" ? "Run once" : "Apply"}</button>
          {detail.kind === "command" && detail.prefix ? (
            <button
              className="btn btn-line sm"
              disabled={allowing}
              title={`Do not ask again for commands starting "${detail.prefix}" in this folder`}
              onClick={async () => {
                setAllowing(true);
                try {
                  await onAllowPrefix(detail.prefix!);
                  onApprove();
                } catch (e) {
                  toast(`Could not remember that: ${(e as Error).message}`);
                  setAllowing(false);
                }
              }}
            >
              Always allow “{detail.prefix}”
            </button>
          ) : null}
          <button className="btn btn-line sm" onClick={onReject}><Icon.Close size={14} /> Deny</button>
        </div>
      )}

      {detail.kind === "command" && terminalOut ? (
        <pre className="mono" style={{ marginTop: 8, maxHeight: 200, overflow: "auto", fontSize: "0.72rem", background: "var(--bg-1)", border: "1px solid var(--line-2)", borderRadius: 6, padding: "8px 10px", whiteSpace: "pre-wrap", wordBreak: "break-word" }}>
          {terminalOut}
        </pre>
      ) : null}
    </div>
  );
}

/** Everyone posts prefixes and reads previews the same way; a thin helper so the
 *  card does not import the api and the shell both. */
export async function rememberPrefix(workspaceId: string, prefix: string): Promise<void> {
  await allowCommandPrefix(workspaceId, prefix);
}
