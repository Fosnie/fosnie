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

// The browser's socket behaviour, pinned.
//
// A second way of reaching the instance now exists, and the way that has always
// worked must not have shifted an inch to make room for it: the same ticket
// call, the same URL, the same opening frame, the same reconnect. These are the
// details that are invisible until a real deployment breaks.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const apiFetch = vi.fn();
vi.mock("@/api/client", () => ({ apiFetch: (...args: unknown[]) => apiFetch(...args) }));
vi.mock("@/api/instance", () => ({
  deviceMode: () => false,
  wsBase: () => "wss://ai.example.com/ws",
}));

/** A stand-in for the browser's WebSocket that records what was sent. */
class FakeSocket {
  static last: FakeSocket | null = null;
  static OPEN = 1;
  readyState = 1;
  sent: string[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((e: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  constructor(public url: string) {
    FakeSocket.last = this;
  }
  send(payload: string) {
    this.sent.push(payload);
  }
  close() {
    this.readyState = 3;
    this.onclose?.();
  }
}

beforeEach(() => {
  vi.stubGlobal("WebSocket", FakeSocket);
  vi.stubGlobal("__APP_RELEASE__", "9.9.9");
  apiFetch.mockReset();
  apiFetch.mockResolvedValue({ ticket: "tick et" });
  FakeSocket.last = null;
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.resetModules();
});

async function started() {
  const { browserTransport } = await import("@/ws/transport-browser");
  const transport = browserTransport();
  const frames: string[] = [];
  const statuses: string[] = [];
  transport.onFrame((raw) => frames.push(raw));
  transport.onStatus((s) => statuses.push(s));
  transport.start();
  // Let the ticket call settle.
  await vi.waitFor(() => expect(FakeSocket.last).not.toBeNull());
  return { transport, frames, statuses, socket: FakeSocket.last! };
}

describe("the browser transport", () => {
  it("mints a ticket and puts it in the URL, never the token", async () => {
    const { socket } = await started();
    expect(apiFetch).toHaveBeenCalledWith("/api/ws-ticket", { method: "POST" });
    expect(socket.url).toBe("wss://ai.example.com/ws?ticket=tick%20et");
  });

  it("opens with the same identifying frame it always sent", async () => {
    const { socket } = await started();
    socket.onopen?.();
    expect(JSON.parse(socket.sent[0])).toEqual({
      version: 1,
      type: "client.hello",
      client_kind: "web",
      client_version: "9.9.9",
      capabilities: [],
    });
  });

  it("comes back on a resume token rather than a fresh ticket", async () => {
    const { socket } = await started();
    socket.onopen?.();
    socket.onmessage?.({
      data: JSON.stringify({ version: 1, type: "hello", resume_token: "r-1" }),
    });
    vi.useFakeTimers();
    socket.close();
    await vi.advanceTimersByTimeAsync(1000);
    vi.useRealTimers();
    await vi.waitFor(() => expect(FakeSocket.last!.url).toContain("resume=r-1"));
    // The second connection did not ask for another ticket.
    expect(apiFetch).toHaveBeenCalledTimes(1);
  });

  it("hands every frame on as the instance wrote it", async () => {
    const { socket, frames } = await started();
    const raw = JSON.stringify({ version: 1, type: "chat.token", turn_id: "t", delta: "hi" });
    socket.onmessage?.({ data: raw });
    expect(frames).toEqual([raw]);
  });

  it("reports the states the indicator is written against", async () => {
    const { socket, statuses } = await started();
    socket.onopen?.();
    socket.close();
    expect(statuses).toEqual(["connecting", "open", "closed"]);
  });
});

describe("the desktop transport", () => {
  it("sends through the client and delivers what it emits, untouched", async () => {
    const wsSend = vi.fn().mockResolvedValue(true);
    const handlers: Record<string, (payload: string) => void> = {};
    vi.doMock("@/shell/bridge", () => ({
      SHELL_EVENTS: { frame: "ws:frame", status: "ws:status" },
      wsSend,
      onShellEvent: (name: string, handler: (payload: string) => void) => {
        handlers[name] = handler;
        return Promise.resolve(() => {});
      },
    }));

    const { shellTransport } = await import("@/ws/transport-shell");
    const transport = shellTransport();
    const frames: string[] = [];
    const statuses: string[] = [];
    transport.onFrame((raw) => frames.push(raw));
    transport.onStatus((s) => statuses.push(s));
    transport.start();

    const raw = JSON.stringify({ version: 1, type: "chat.token", turn_id: "t", delta: "hi" });
    handlers["ws:frame"](raw);
    handlers["ws:status"]("open");
    expect(frames).toEqual([raw]);
    expect(statuses).toEqual(["open"]);

    transport.send('{"version":1,"type":"ping"}');
    expect(wsSend).toHaveBeenCalledWith('{"version":1,"type":"ping"}');
  });

  it("stops delivering to a window that has stopped listening", async () => {
    const handlers: Record<string, (payload: string) => void> = {};
    vi.doMock("@/shell/bridge", () => ({
      SHELL_EVENTS: { frame: "ws:frame", status: "ws:status" },
      wsSend: vi.fn(),
      onShellEvent: (name: string, handler: (payload: string) => void) => {
        handlers[name] = handler;
        return Promise.resolve(() => {});
      },
    }));

    const { shellTransport } = await import("@/ws/transport-shell");
    const transport = shellTransport();
    const frames: string[] = [];
    transport.onFrame((raw) => frames.push(raw));
    transport.start();
    transport.stop();
    handlers["ws:frame"]("{}");
    expect(frames).toEqual([]);
  });
});
