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

// The whole of what this application may ask the desktop client to do.
//
// Two directions: commands the window calls, and events the client emits. Both
// go through the desktop runtime, which is loaded on demand — a browser build
// never reaches this module, so nothing of the client rides in the bundle a
// browser downloads.
//
// The names here have to match the client's; that is why they are written once,
// in this file, and imported everywhere else.

/** Where the application should send its requests, and the credential to use.
 *  Held in memory for the life of the process, exactly as in a browser. */
export interface InstanceConfig {
  base_url: string;
  token: string;
}

/** What was found at an address, before anyone types a pairing code into it. */
export interface InstanceInfo {
  base_url: string;
  auth_mode: string;
}

/** This client's own version and platform, for the About panel. */
export interface ShellInfo {
  app_version: string;
  platform: string;
}

/** Events the client emits. */
export const SHELL_EVENTS = {
  /** One frame from the socket, as JSON, exactly as the instance sent it. */
  frame: "ws:frame",
  /** Connection state: `connecting` | `open` | `closed`. */
  status: "ws:status",
  /** The instance no longer accepts this device. The pairing is over. */
  unpaired: "shell:unpaired",
  /** A notification was clicked: open this chat. */
  openChat: "shell:open-chat",
  /** A newer release has been downloaded and verified, and is waiting for a yes. */
  updateReady: "shell:update-ready",
} as const;

/** A release the client has already fetched and is holding. */
export interface UpdateReady {
  version: string;
  notes?: string | null;
}

/** The desktop runtime, imported only when the client is what is hosting us. */
async function api() {
  const [core, event] = await Promise.all([
    import("@tauri-apps/api/core"),
    import("@tauri-apps/api/event"),
  ]);
  return { invoke: core.invoke, listen: event.listen };
}

export async function shellInfo(): Promise<ShellInfo> {
  return (await api()).invoke<ShellInfo>("shell_info");
}

/** The pairing this client already holds, or `null` when it has none. */
export async function instanceConfig(): Promise<InstanceConfig | null> {
  return (await api()).invoke<InstanceConfig | null>("instance_config");
}

/** Check an address is a reachable instance. Rejects with a sentence to show. */
export async function validateInstance(url: string): Promise<InstanceInfo> {
  return (await api()).invoke<InstanceInfo>("validate_instance", { url });
}

/** Redeem a pairing code. Rejects with a sentence to show. */
export async function pair(url: string, code: string): Promise<InstanceConfig> {
  return (await api()).invoke<InstanceConfig>("pair", { url, code });
}

/** Sign this machine out and forget its credential. */
export async function unpair(): Promise<void> {
  await (await api()).invoke("unpair");
}

/** Put a frame on the socket. Resolves false when there is no connection. */
export async function wsSend(frame: string): Promise<boolean> {
  return (await api()).invoke<boolean>("ws_send", { frame });
}

/** The release the client has already fetched, if any. Asked once at startup:
 *  the client checks the moment it starts and can finish before this window is
 *  listening, and an event nobody heard is an update nobody is offered. */
export async function pendingUpdate(): Promise<UpdateReady | null> {
  return (await api()).invoke<UpdateReady | null>("pending_update");
}

/** Install the waiting release and restart into it. Does not return: the client
 *  hands over to the installer. */
export async function installUpdate(): Promise<void> {
  await (await api()).invoke("install_update");
}

/** Open a link belonging to the connected instance in the user's own browser. */
export async function openExternal(url: string): Promise<void> {
  await (await api()).invoke("open_external", { url });
}

/** Subscribe to one of the client's events. Resolves to an unsubscribe. */
export async function onShellEvent<T>(
  name: string,
  handler: (payload: T) => void,
): Promise<() => void> {
  return (await api()).listen<T>(name, (e) => handler(e.payload));
}
