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

// How frames get to and from the instance.
//
// In a browser this application opens the socket itself, as it always has. In
// the desktop client it does not: web views are unreliable at holding a
// long-lived connection, so the client holds it and the frames arrive as events.
//
// The difference stops at this interface. Above it, one store, one set of
// handlers, one status; below it, two implementations that both deliver the
// instance's own JSON and take back what the application composed.

import type { WsStatus } from "@/ws/protocol";

export interface Transport {
  /** Begin connecting, or reconnecting after `stop`. Idempotent. */
  start(): void;
  /** Stop and stay stopped until `start` is called again. */
  stop(): void;
  /** Send an already-serialised frame. Dropped when nothing is connected. */
  send(payload: string): void;
  /** Called with the raw JSON of each frame that arrives. */
  onFrame(handler: (raw: string) => void): void;
  /** Called whenever the connection state changes. */
  onStatus(handler: (status: WsStatus) => void): void;
}
