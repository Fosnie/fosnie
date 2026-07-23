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

import { useSyncExternalStore } from "react";
import { isShell } from "@/shell/detect";
import type { ClientFrame, ServerFrame, WsStatus } from "@/ws/protocol";
import type { Transport } from "@/ws/transport";
import { browserTransport } from "@/ws/transport-browser";

// One multiplexed socket per user. Status is exposed via useSyncExternalStore;
// frames are pushed to registered handlers.
//
// Who actually holds the socket is decided once, at startup: a browser holds its
// own, the desktop client holds one on the window's behalf. Everything above
// this line — every screen, every handler, the status indicator — is written
// against this store and cannot tell which it got.

type FrameHandler = (f: ServerFrame) => void;

let status: WsStatus = "idle";
let serverVersion: string | null = null;
const statusListeners = new Set<() => void>();
const frameHandlers = new Set<FrameHandler>();

const transport: Transport = isShell() ? shellTransportLazily() : browserTransport();

// The desktop transport reaches the client's runtime, which a browser build has
// no business loading. Selecting it is synchronous, so the module is pulled in
// through a thin proxy that queues the handful of calls made before it lands.
function shellTransportLazily(): Transport {
  let real: Transport | null = null;
  const pending: Array<(t: Transport) => void> = [];
  const withTransport = (fn: (t: Transport) => void) => (real ? fn(real) : pending.push(fn));

  void import("@/ws/transport-shell").then(({ shellTransport }) => {
    real = shellTransport();
    real.onFrame(deliver);
    real.onStatus(setStatus);
    for (const fn of pending) fn(real);
    pending.length = 0;
  });

  return {
    start: () => withTransport((t) => t.start()),
    stop: () => withTransport((t) => t.stop()),
    send: (payload) => withTransport((t) => t.send(payload)),
    // Handlers are wired straight to the store above; the proxy does not need to
    // forward the registrations.
    onFrame: () => {},
    onStatus: () => {},
  };
}

function setStatus(s: WsStatus) {
  status = s;
  statusListeners.forEach((l) => l());
}

function deliver(raw: string) {
  let frame: ServerFrame;
  try {
    frame = JSON.parse(raw) as ServerFrame;
  } catch {
    return;
  }
  // The instance names its own build in the opening frame. Kept because a client
  // that ships separately from the instance is often several releases away from
  // it, and the pair of numbers is the first thing worth knowing about a report.
  if (frame.type === "hello") {
    const version = (frame as { server_version?: string }).server_version;
    if (version) {
      serverVersion = version;
      statusListeners.forEach((l) => l());
    }
  }
  frameHandlers.forEach((h) => h(frame));
}

transport.onFrame(deliver);
transport.onStatus(setStatus);

export const wsStore = {
  start() {
    transport.start();
  },
  stop() {
    transport.stop();
  },
  send(frame: ClientFrame) {
    transport.send(JSON.stringify({ version: 1, ...frame }));
  },
  onFrame(h: FrameHandler): () => void {
    frameHandlers.add(h);
    return () => frameHandlers.delete(h);
  },
  subscribe(l: () => void): () => void {
    statusListeners.add(l);
    return () => statusListeners.delete(l);
  },
  getStatus: (): WsStatus => status,
  /** The connected instance's version, once it has said hello. */
  getServerVersion: (): string | null => serverVersion,
};

export function useWsStatus(): WsStatus {
  return useSyncExternalStore(wsStore.subscribe, wsStore.getStatus);
}

/** The connected instance's version, or `null` before the socket has opened. */
export function useServerVersion(): string | null {
  return useSyncExternalStore(wsStore.subscribe, wsStore.getServerVersion);
}
