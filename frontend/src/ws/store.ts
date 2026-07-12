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
import { apiFetch } from "@/api/client";
import type { ClientFrame, ServerFrame, WsStatus } from "@/ws/protocol";

// One multiplexed socket per user (topology §6.3). Status is exposed via
// useSyncExternalStore; frames are pushed to registered handlers. Reconnect uses
// the server-issued resume token within its TTL, else a freshly-minted connect
// ticket — the access token is never placed in the socket URL.

type FrameHandler = (f: ServerFrame) => void;

let ws: WebSocket | null = null;
let status: WsStatus = "idle";
let resumeToken: string | null = null;
let attempts = 0;
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
let stopped = false;

const statusListeners = new Set<() => void>();
const frameHandlers = new Set<FrameHandler>();

function setStatus(s: WsStatus) {
  status = s;
  statusListeners.forEach((l) => l());
}

function wsBase(): string {
  const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
  return `${proto}//${window.location.host}/ws`;
}

async function open() {
  if (stopped) return;
  setStatus("connecting");
  let url: string;
  try {
    if (resumeToken) {
      url = `${wsBase()}?resume=${encodeURIComponent(resumeToken)}`;
    } else {
      // Mint a single-use ticket over the authenticated HTTP path (Bearer token
      // in the Authorization header) so the JWT never lands in the socket URL.
      const { ticket } = await apiFetch<{ ticket: string }>("/api/ws-ticket", { method: "POST" });
      url = `${wsBase()}?ticket=${encodeURIComponent(ticket)}`;
    }
  } catch {
    setStatus("closed");
    return;
  }
  const sock = new WebSocket(url);
  ws = sock;

  sock.onopen = () => {
    attempts = 0;
    setStatus("open");
  };
  sock.onmessage = (ev) => {
    let frame: ServerFrame;
    try {
      frame = JSON.parse(ev.data as string) as ServerFrame;
    } catch {
      return;
    }
    if (frame.type === "hello") resumeToken = (frame as { resume_token: string }).resume_token;
    frameHandlers.forEach((h) => h(frame));
  };
  sock.onclose = () => {
    if (ws === sock) ws = null;
    setStatus("closed");
    scheduleReconnect();
  };
  sock.onerror = () => sock.close();
}

function scheduleReconnect() {
  if (stopped || reconnectTimer) return;
  // Keep retrying indefinitely (capped backoff ≤30s) so the socket self-heals
  // after a backend restart — never permanently give up while mounted.
  const delay = Math.min(30_000, 500 * 2 ** Math.min(attempts, 6));
  attempts += 1;
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    void open();
  }, delay);
}

// Reconnect immediately (reset backoff) — fired on network/focus/visibility
// recovery so a dropped socket comes back at once instead of after the backoff.
function reconnectNow() {
  if (stopped) return;
  if (ws && (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING)) return;
  attempts = 0;
  if (reconnectTimer) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
  void open();
}

if (typeof window !== "undefined") {
  window.addEventListener("online", reconnectNow);
  window.addEventListener("focus", reconnectNow);
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") reconnectNow();
  });
}

export const wsStore = {
  start() {
    stopped = false;
    if (!ws && status !== "connecting") void open();
  },
  stop() {
    stopped = true;
    if (reconnectTimer) {
      clearTimeout(reconnectTimer);
      reconnectTimer = null;
    }
    ws?.close();
    ws = null;
  },
  send(frame: ClientFrame) {
    if (ws?.readyState === WebSocket.OPEN) ws.send(JSON.stringify({ version: 1, ...frame }));
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
};

export function useWsStatus(): WsStatus {
  return useSyncExternalStore(wsStore.subscribe, wsStore.getStatus);
}
