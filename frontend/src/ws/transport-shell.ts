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

// The socket as the desktop client holds it.
//
// This window opens no connection of its own. Frames arrive as events carrying
// the instance's own JSON, and what the application sends goes back through one
// command. Reconnecting, resuming and backing off all happen in the client,
// which is the point of the arrangement: a web view that is minimised, throttled
// or quietly dropping idle connections cannot take a turn's stream with it.
//
// So there is nothing here to retry. The status reported is the client's, and
// the application's existing connecting/reconnecting indicator reads it exactly
// as it reads a browser's.

import type { WsStatus } from "@/ws/protocol";
import type { Transport } from "@/ws/transport";
import { SHELL_EVENTS, onShellEvent, wsSend } from "@/shell/bridge";

export function shellTransport(): Transport {
  let onFrame: (raw: string) => void = () => {};
  let onStatus: (s: WsStatus) => void = () => {};
  let subscribed = false;
  let stopped = false;

  /** The client's connection words, in the application's. */
  function toStatus(reported: string): WsStatus {
    if (reported === "open") return "open";
    if (reported === "connecting") return "connecting";
    return "closed";
  }

  function subscribe() {
    if (subscribed) return;
    subscribed = true;
    void onShellEvent<string>(SHELL_EVENTS.frame, (raw) => {
      if (!stopped) onFrame(raw);
    });
    void onShellEvent<string>(SHELL_EVENTS.status, (reported) => {
      if (!stopped) onStatus(toStatus(reported));
    });
  }

  return {
    start() {
      stopped = false;
      subscribe();
    },
    stop() {
      // The connection belongs to the client and stays up: it is what lets a
      // finished answer raise a notification while this window is away. All that
      // stops is this window's interest in the frames.
      stopped = true;
    },
    send(payload: string) {
      void wsSend(payload);
    },
    onFrame(handler) {
      onFrame = handler;
    },
    onStatus(handler) {
      onStatus = handler;
    },
  };
}
