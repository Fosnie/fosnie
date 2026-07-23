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

// What this application may ask the desktop client to do about folders on the
// machine it is running on.
//
// Read the list: choose a folder, connect one, see them, forget one, see and
// undo what was done in a turn, stop a running command, show a proposed change,
// point the file manager at a file. There is no "read this file" and no "run
// this" — the window cannot ask for work in a folder, and does not need to. Work
// arrives at the client from the instance, on the socket, for a conversation
// somebody bound to a folder.
//
// Every name here has to match the client's, which is why they are written once,
// in this file, exactly as the rest of the client's surface is in `bridge.ts`.

/** A folder this machine has been told it may work in. */
export interface Folder {
  workspace_id: string;
  path: string;
  /** "ReadOnly" | "ReadWrite" | "ReadWriteNoDelete" as the client serialises it. */
  tier: "ro" | "rw" | "rw_nd";
  base_url: string;
}

/** One file the agent changed, and whether it has been put back. */
export interface Change {
  id: string;
  session: string;
  turn: string;
  path: string;
  /** "write" | "delete" */
  op: string;
  existed: boolean;
  at: number;
  restored: boolean;
}

/** What a proposed write would do to the file as it stands on this machine. */
export interface Preview {
  existed: boolean;
  unified: string;
  added: number;
  removed: number;
  binary: boolean;
}

/** The desktop runtime, imported only when the client is what is hosting us. */
async function invoke<T>(name: string, args?: Record<string, unknown>): Promise<T> {
  const core = await import("@tauri-apps/api/core");
  return core.invoke<T>(name, args);
}

/** Ask the person which folder, with the system's own picker. A path comes back
 *  and nothing else: choosing is not yet agreeing. */
export function chooseFolder(): Promise<string | null> {
  return invoke<string | null>("choose_folder");
}

/** Connect a folder at an agreed level of trust: tell the instance, and record
 *  it on this machine as somewhere work may happen. */
export function connectFolder(path: string, tier: Folder["tier"]): Promise<Folder> {
  return invoke<Folder>("connect_folder", { path, tier });
}

/** The folders this machine holds, after checking which the instance still has. */
export function listFolders(): Promise<Folder[]> {
  return invoke<Folder[]>("list_folders");
}

/** Stop working in a folder on this machine. */
export function forgetFolder(workspaceId: string): Promise<void> {
  return invoke("forget_folder", { workspaceId });
}

/** What one turn changed on this machine. */
export function turnChanges(turnId: string): Promise<Change[]> {
  return invoke<Change[]>("turn_changes", { turnId });
}

/** Put one file back the way it was; resolves with the path that was restored.
 *  Rejects with a "changed-since" message when the file was edited after the
 *  agent touched it, unless `force`. */
export function restoreChange(id: string, force = false): Promise<string> {
  return invoke<string>("restore_change", { id, force });
}

/** Put back everything one turn changed; resolves with [restored, skipped] —
 *  skipped are files edited since, left alone unless `force`. */
export function restoreTurn(turnId: string, force = false): Promise<[number, number]> {
  return invoke<[number, number]>("restore_turn", { turnId, force });
}

/** Stop the command running for a turn. False when it has already finished. */
export function cancelLocalCall(turnId: string): Promise<boolean> {
  return invoke<boolean>("cancel_local_call", { turnId });
}

/** The difference a proposed write would make, computed where the file is. */
export function previewChange(
  workspaceId: string,
  path: string,
  newContent: string,
): Promise<Preview> {
  return invoke<Preview>("preview_change", { workspaceId, path, newContent });
}

/** Show a file in the operating system's file manager. */
export function revealPath(workspaceId: string, path: string): Promise<void> {
  return invoke("reveal_path", { workspaceId, path });
}
