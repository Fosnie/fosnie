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

// Client-side error telemetry. Reports browser errors to the backend's
// intra-perimeter sink (POST /api/telemetry) so a client crash is visible to
// the operator instead of being silent. It goes to the instance the SPA is
// talking to and nowhere else — no egress.
//
// Deliberately self-contained: a BARE fetch with NO auth header (the endpoint
// is public, because errors frequently happen exactly when auth is unavailable
// — before login, during a token refresh, or when the identity provider is
// down; asking for a token here could itself trigger a login redirect out of a
// crash handler). Only the instance base is borrowed from the request layer, so
// a client that addresses a remote instance reports to that instance. Telemetry
// must never throw and must never spam itself, so every path is guarded and a
// throttle + dedupe sits in front of the network call.

import { apiUrl } from "@/api/instance";

export type ClientErrorKind = "error" | "unhandledrejection" | "react" | "chunk";

export interface ClientErrorReport {
  kind: ClientErrorKind;
  message: string;
  stack?: string;
  route?: string;
  user_agent?: string;
  release?: string;
  ts?: number;
}

const MAX_PER_WINDOW = 10;
const WINDOW_MS = 60_000;
const DEDUPE_MS = 10_000;

let windowStart = 0;
let windowCount = 0;
const recent = new Map<string, number>(); // signature -> last-sent epoch ms

function truncate(s: string | undefined, max: number): string | undefined {
  if (s == null) return undefined;
  return s.length > max ? s.slice(0, max) : s;
}

// Gate: drop identical reports within DEDUPE_MS, and cap overall volume per
// window so a render-loop crash cannot flood the endpoint.
function allow(signature: string, now: number): boolean {
  const last = recent.get(signature);
  if (last != null && now - last < DEDUPE_MS) return false;
  if (now - windowStart > WINDOW_MS) {
    windowStart = now;
    windowCount = 0;
  }
  if (windowCount >= MAX_PER_WINDOW) return false;
  windowCount += 1;
  recent.set(signature, now);
  if (recent.size > 100) {
    for (const [k, t] of recent) if (now - t > DEDUPE_MS) recent.delete(k);
  }
  return true;
}

export function reportClientError(input: { kind: ClientErrorKind; message: string; stack?: string }): void {
  try {
    const now = Date.now();
    const message = truncate(input.message, 1024) ?? "";
    const stack = truncate(input.stack, 8192);
    const signature = `${input.kind}|${message}|${stack ?? ""}`;
    if (!allow(signature, now)) return;
    const body: ClientErrorReport = {
      kind: input.kind,
      message,
      stack,
      route: typeof location !== "undefined" ? location.pathname : undefined,
      user_agent: typeof navigator !== "undefined" ? navigator.userAgent : undefined,
      release: typeof __APP_RELEASE__ !== "undefined" ? __APP_RELEASE__ : undefined,
      ts: now,
    };
    void fetch(apiUrl("/api/telemetry"), {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
      keepalive: true, // survive an unload / navigation following the crash
    }).catch(() => {
      /* telemetry must never throw */
    });
  } catch {
    /* swallow — telemetry must never break the app */
  }
}

let installed = false;

// Wire window-level handlers once. Render-time React errors are caught
// separately by the ErrorBoundary; these cover everything outside React.
export function installGlobalErrorHandlers(): void {
  if (installed || typeof window === "undefined") return;
  installed = true;

  window.addEventListener("error", (e: ErrorEvent) => {
    // Cross-origin script errors arrive as a bare "Script error." with no
    // detail, and resource-load errors carry no message — both are noise.
    const message = e.message || e.error?.message;
    if (!message || message === "Script error.") return;
    reportClientError({ kind: "error", message, stack: e.error?.stack });
  });

  window.addEventListener("unhandledrejection", (e: PromiseRejectionEvent) => {
    const reason: unknown = e.reason;
    const message =
      reason instanceof Error
        ? reason.message
        : typeof reason === "string"
          ? reason
          : "unhandled promise rejection";
    const stack = reason instanceof Error ? reason.stack : undefined;
    reportClientError({ kind: "unhandledrejection", message, stack });
  });
}
