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

// What a turn changed in a connected folder, and putting it back.
//
// Reads the client's own record of what it wrote and deleted, so it exists only
// on the desktop; a browser cannot undo a change made on somebody's own computer
// and does not pretend to. A file each, and the whole turn at once.
//
// The first time anybody restores anything, they are told the one thing this
// does not cover: files a command changed. A command is an arbitrary program,
// and the honest thing is not to claim to know what it touched.

import { useEffect, useState } from "react";

import { Icon } from "@/components/icons";
import { confirmDialog, toast } from "@/components/dialogs";
import { isShell } from "@/shell/detect";
import { restoreChange, restoreTurn, revealPath, turnChanges, type Change } from "@/shell/folders";

/** Rust surfaces a stale restore as an error message starting with this marker. */
const STALE = "changed-since";

const CAVEAT_KEY = "fosnie.restore.caveat.seen";

function noteCaveatOnce() {
  try {
    if (localStorage.getItem(CAVEAT_KEY)) return;
    localStorage.setItem(CAVEAT_KEY, "1");
  } catch {
    // A machine that will not store the flag simply shows the note again; no harm.
  }
  toast(
    "Restored. Note: changes made by commands the agent ran are not covered by undo — only files it wrote or deleted directly.",
    { variant: "info", duration: 9000 },
  );
}

export function RestoreBlock({ turnId, workspaceId }: { turnId: string | undefined; workspaceId?: string }) {
  const [changes, setChanges] = useState<Change[]>([]);
  const [busy, setBusy] = useState(false);

  const reload = () => {
    if (!turnId || !isShell()) return;
    turnChanges(turnId).then(setChanges).catch(() => setChanges([]));
  };

  useEffect(reload, [turnId]);

  // Only the desktop holds the record, and only a turn that actually changed a
  // file has anything to offer.
  if (!isShell() || !turnId || changes.length === 0) return null;
  const undone = changes.filter((c) => c.restored).length;

  async function undoOne(c: Change) {
    setBusy(true);
    try {
      await restoreChange(c.id);
      noteCaveatOnce();
      reload();
    } catch (e) {
      const msg = (e as Error).message;
      // The file moved on since the agent's edit. Ask before discarding that.
      if (msg.startsWith(STALE)) {
        const ok = await confirmDialog({
          title: "This file changed since",
          body: `${c.path}\n\nIt has been edited after the agent's change. Put the older version back anyway? The later edit will be lost.`,
          confirmLabel: "Restore older version",
          danger: true,
        });
        if (ok) {
          try {
            await restoreChange(c.id, true);
            noteCaveatOnce();
            reload();
          } catch (e2) {
            toast(`Could not put it back: ${(e2 as Error).message}`);
          }
        }
      } else {
        toast(`Could not put it back: ${msg}`);
      }
    } finally {
      setBusy(false);
    }
  }

  async function undoAll() {
    setBusy(true);
    try {
      const [restored, skipped] = await restoreTurn(turnId!);
      noteCaveatOnce();
      if (skipped > 0) {
        const ok = await confirmDialog({
          title: `${skipped} file${skipped === 1 ? "" : "s"} changed since`,
          body: `${restored} restored. ${skipped} ${skipped === 1 ? "file was" : "files were"} edited after the agent's change and left alone. Put the older versions back too? Those later edits will be lost.`,
          confirmLabel: "Restore older versions",
          danger: true,
        });
        if (ok) {
          const [more] = await restoreTurn(turnId!, true);
          toast(`Put ${restored + more} file${restored + more === 1 ? "" : "s"} back.`, { variant: "success" });
        } else {
          toast(`Put ${restored} file${restored === 1 ? "" : "s"} back; left ${skipped} changed alone.`, { variant: "success" });
        }
      } else {
        toast(`Put ${restored} file${restored === 1 ? "" : "s"} back.`, { variant: "success" });
      }
      reload();
    } catch (e) {
      toast(`Could not put them back: ${(e as Error).message}`);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="aa-restore" style={{ marginTop: 8, borderTop: "1px solid var(--line-2)", paddingTop: 8 }}>
      <div className="menu-label mono" style={{ padding: "0 0 4px" }}>
        Changed {changes.length} file{changes.length === 1 ? "" : "s"} in this folder
      </div>
      {changes.map((c) => (
        <div key={c.id} style={{ display: "flex", alignItems: "center", gap: 8, fontSize: "0.78rem", padding: "2px 0" }}>
          {c.op === "delete" ? <Icon.Close size={13} /> : <Icon.Edit size={13} />}
          <span className="mono" style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", color: c.restored ? "var(--ink-3)" : "var(--ink-2)", textDecoration: c.restored ? "line-through" : "none" }}>
            {c.path}
          </span>
          <span style={{ marginLeft: "auto", display: "flex", gap: 6 }}>
            {workspaceId && c.op !== "delete" ? (
              <button className="btn btn-line sm" title="Show in folder" disabled={busy} onClick={() => revealPath(workspaceId, c.path).catch((e) => toast((e as Error).message))}>
                <Icon.Folder size={12} />
              </button>
            ) : null}
            {c.restored ? (
              <span className="mono" style={{ fontSize: "0.7rem", color: "var(--ink-3)" }}>restored</span>
            ) : (
              <button className="btn btn-line sm" disabled={busy} onClick={() => void undoOne(c)}>Restore this file</button>
            )}
          </span>
        </div>
      ))}
      {changes.length - undone > 1 ? (
        <button className="btn btn-line sm" style={{ marginTop: 6 }} disabled={busy} onClick={() => void undoAll()}>
          <Icon.Refresh size={13} /> Restore everything from this turn
        </button>
      ) : null}
    </div>
  );
}
