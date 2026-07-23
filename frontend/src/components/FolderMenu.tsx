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

// Choosing the folder a chat works in, from inside the composer's "+" menu —
// beside "Attach file", where connecting things to a turn already lives.
//
// On the desktop it can connect a new folder (the system picker, then the trust
// prompt); anywhere it lists the folders already connected on the paired
// machines and attaches one to this chat. A brand-new chat has no id to bind to
// yet, so the choice is held and rides the first message.

import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";

import { Icon } from "@/components/icons";
import { confirmDialog, toast } from "@/components/dialogs";
import { isShell } from "@/shell/detect";
import {
  bindChatWorkspace,
  unbindChatWorkspace,
  useChatWorkspace,
  useWorkspaces,
} from "@/api/client";
import { chooseFolder, connectFolder, listFolders } from "@/shell/folders";

/** One connectable folder in the menu — a path, its trust, and where it is. */
interface Choice {
  workspace_id: string;
  path: string;
  tier: string;
  device_name: string;
}

const TIER_WORDS: Record<string, string> = {
  ro: "read only",
  rw: "read, write and delete",
  rw_nd: "read and write, but not delete",
};

const lastTwo = (p: string) => {
  const sep = p.includes("\\") ? "\\" : "/";
  const parts = p.split(/[\\/]/).filter(Boolean);
  return parts.length <= 2 ? p : `…${sep}${parts.slice(-2).join(sep)}`;
};

/**
 * The folder section of the composer's "+" menu.
 *
 * `pending`/`setPending` hold a folder chosen before the chat exists: on a new
 * chat there is no id to bind to, so the parent sends the pending id with the
 * first message and clears it. On a saved chat, choosing binds immediately.
 */
export function FolderMenu({
  chatId,
  pending,
  setPending,
  close,
}: {
  chatId: string | null;
  pending: string | null;
  setPending: (id: string | null) => void;
  close: () => void;
}) {
  const qc = useQueryClient();
  const shell = isShell();
  const [busy, setBusy] = useState(false);

  const bound = useChatWorkspace(chatId ?? undefined);
  // The connectable folders. On the desktop these come from the client's own
  // record — the folders connected on THIS machine — because a chat here works
  // through this machine, and a folder on another of the owner's computers cannot
  // be reached from this socket (the tools would never be offered). In a browser
  // there is nothing to connect and nothing to work through; the list of what is
  // connected elsewhere is shown read-only.
  const deviceFolders = useQuery({
    queryKey: ["device-folders"],
    queryFn: () => listFolders(),
    enabled: shell,
  });
  const allWorkspaces = useWorkspaces();
  const live: Choice[] = shell
    ? (deviceFolders.data ?? []).map((f) => ({ workspace_id: f.workspace_id, path: f.path, tier: f.tier, device_name: "this computer" }))
    : (allWorkspaces.data ?? []).filter((w) => !w.revoked_at).map((w) => ({ workspace_id: w.id, path: w.path, tier: w.tier, device_name: w.device_name }));

  // What this chat is set to work in: the bound folder on a saved chat, or the
  // one held for a new chat's first message.
  const activeId = bound.data?.id ?? pending;
  const active = live.find((w) => w.workspace_id === activeId)
    ?? (bound.data ? { workspace_id: bound.data.id, path: bound.data.path, tier: bound.data.tier, device_name: bound.data.device_name } : null);

  // In a browser with nothing connected anywhere there is nothing to show and no
  // way to connect one (that happens on the desktop); the section is left out.
  if (!shell && live.length === 0) return null;

  async function refresh() {
    await Promise.all([
      qc.invalidateQueries({ queryKey: ["chat-workspace", chatId] }),
      qc.invalidateQueries({ queryKey: ["workspaces"] }),
      qc.invalidateQueries({ queryKey: ["device-folders"] }),
    ]);
  }

  async function choose(w: Choice) {
    close();
    if (!chatId) {
      // No chat yet: hold it, and it rides the first message.
      setPending(w.workspace_id);
      toast(`This chat will work in ${w.path} once you send a message.`, { variant: "info" });
      return;
    }
    setBusy(true);
    try {
      await bindChatWorkspace(chatId, w.workspace_id);
      await refresh();
    } catch (e) {
      toast(`Could not use that folder: ${(e as Error).message}`);
    } finally {
      setBusy(false);
    }
  }

  async function detach() {
    close();
    setPending(null);
    if (!chatId) return;
    setBusy(true);
    try {
      await unbindChatWorkspace(chatId);
      await refresh();
    } catch (e) {
      toast(`Could not detach the folder: ${(e as Error).message}`);
    } finally {
      setBusy(false);
    }
  }

  async function connectNew() {
    close();
    const path = await chooseFolder().catch(() => null);
    if (!path) return;
    const rw = await confirmDialog({
      title: "Connect this folder?",
      body: `${path}\n\nAn agent in this chat will be able to read it, and — if you allow it below — write, change and delete files in it. Every change is shown to you first, and can be undone. Nothing outside this folder can be reached.\n\nAllow changes, not only reading?`,
      confirmLabel: "Allow reading and changes",
      cancelLabel: "Reading only",
    });
    const tier = rw ? "rw" : "ro";
    setBusy(true);
    try {
      const folder = await connectFolder(path, tier as "ro" | "rw");
      await refresh();
      if (chatId) {
        await bindChatWorkspace(chatId, folder.workspace_id);
        await refresh();
        toast(`Connected ${path}`, { variant: "success" });
      } else {
        setPending(folder.workspace_id);
        toast(`Connected ${path}. It will be used once you send a message.`, { variant: "success" });
      }
    } catch (e) {
      toast(`Could not connect that folder: ${(e as Error).message}`);
    } finally {
      setBusy(false);
    }
  }

  const others = live.filter((w) => w.workspace_id !== activeId);

  return (
    <>
      <div className="divider" style={{ margin: "5px 0" }} />
      <div className="menu-label mono">Work in a folder</div>

      {active ? (
        <>
          <div className="menu-item" style={{ cursor: "default", flexDirection: "column", alignItems: "flex-start", gap: 1 }}>
            <span style={{ display: "flex", alignItems: "center", gap: 6 }}><Icon.Check size={13} /> {lastTwo(active.path)}</span>
            <span className="mono" style={{ color: "var(--ink-3)", fontSize: "0.68rem", paddingLeft: 19 }}>
              {active.device_name} · {TIER_WORDS[active.tier] ?? active.tier}
            </span>
          </div>
          <button className="menu-item" disabled={busy} onClick={() => void detach()}><Icon.Close size={14} /> Stop working in this folder</button>
        </>
      ) : null}

      {others.map((w) => (
        <button key={w.workspace_id} className="menu-item" disabled={busy} onClick={() => void choose(w)}>
          <Icon.Folder size={14} />
          <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", maxWidth: "13rem" }}>{lastTwo(w.path)}</span>
          {!shell ? <span className="mono" style={{ marginLeft: "auto", opacity: 0.55, fontSize: 10 }}>{w.device_name}</span> : null}
        </button>
      ))}

      {shell ? (
        <button className="menu-item" disabled={busy} onClick={() => void connectNew()}><Icon.Plus size={14} /> Connect a folder…</button>
      ) : live.length === 0 ? (
        <div className="menu-item" style={{ cursor: "default", color: "var(--ink-3)", fontSize: "0.78rem" }}>Connect one in the desktop app.</div>
      ) : null}
    </>
  );
}
