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

// Where this SPA sends its requests, and how it authenticates them.
//
// Two modes, decided once at boot:
//
//   * **Web** (nothing configured) — the bundle is served by the instance it
//     talks to. Paths stay relative, and authentication is whatever the
//     deployment uses: the session cookie, or a bearer token from the identity
//     provider. This is the default and is unchanged from before this module
//     existed.
//   * **Device** — the bundle is served from somewhere else entirely (a native
//     shell's own local origin) and addresses a remote instance chosen at
//     runtime, presenting a device token in the `Authorization` header. Cookies
//     are deliberately not sent: the token is the whole credential, and an
//     ambient cookie could only widen what a compromised client reaches.
//
// **The device token is held in memory and nowhere else.** It is never written
// to localStorage, sessionStorage, a cookie, or the URL — a token that outlives
// the process is a token that can be lifted off the disk of a shared machine.
// The caller supplies it to `configureInstance` at every start.
//
// Every request the SPA makes goes through here, so that the two modes differ in
// exactly one place rather than at each call site.

import { serverAuthMode } from "@/auth/config";

/** What the SPA needs to reach a remote instance: where it is, and the device
 *  token that speaks for the owner. */
export interface InstanceConfig {
  /** Absolute origin (optionally with a path prefix), e.g. `https://ai.example.com`. */
  baseUrl: string;
  /** A device token minted by pairing. Held in memory only. */
  token: string;
}

let runtime: InstanceConfig | null = null;

/** Strip whitespace and any trailing slashes so `apiUrl` can concatenate a
 *  leading-slash path without doubling the separator. */
function normaliseBase(raw: string): string {
  return raw.trim().replace(/\/+$/, "");
}

/** Point the SPA at a remote instance. Call before the app boots; calling it
 *  with `null` returns the SPA to web mode (used when a device signs out). */
export function configureInstance(cfg: InstanceConfig | null): void {
  runtime = cfg ? { baseUrl: normaliseBase(cfg.baseUrl), token: cfg.token } : null;
}

/** True once a remote instance + device token are configured. */
export function deviceMode(): boolean {
  return runtime !== null;
}

/** The instance's base: empty in web mode (so every path stays relative and
 *  same-origin), otherwise the normalised remote base. */
export function instanceBase(): string {
  return runtime?.baseUrl ?? "";
}

/** An absolute URL for an API path (`/api/...`, `/health/...`). In web mode this
 *  returns the path unchanged. */
export function apiUrl(path: string): string {
  return instanceBase() + path;
}

/** How the SPA authenticates, with the runtime device token taking precedence
 *  over whatever the deployment's own auth config says. */
export type EffectiveAuthMode = "local" | "keycloak" | "device";

/** The effective auth mode. `device` whenever a device token is configured —
 *  the SPA is then already authenticated and no login flow applies. */
export function authMode(): EffectiveAuthMode {
  return deviceMode() ? "device" : serverAuthMode();
}

/** Whether cookies ride along. Device mode omits them (cross-origin, token-only);
 *  web mode includes them, as it always has. */
export function credentialsMode(): RequestCredentials {
  return deviceMode() ? "omit" : "include";
}

/** The authorisation headers for the current mode, merged onto `init`:
 *  device → the device token; identity provider → a fresh access token; built-in
 *  accounts → nothing (the session cookie carries it). */
export async function authHeaders(init?: HeadersInit): Promise<Headers> {
  const headers = new Headers(init);
  if (runtime) {
    headers.set("Authorization", `Bearer ${runtime.token}`);
  } else if (serverAuthMode() === "keycloak") {
    // Loaded on demand so a client that authenticates with a device token never
    // reaches the identity-provider adapter at all.
    const { freshToken } = await import("@/auth/keycloak");
    headers.set("Authorization", `Bearer ${await freshToken()}`);
  }
  return headers;
}

/** An authenticated request to the instance, returned unread and unchecked.
 *
 *  This is the raw seam for the calls that cannot use `apiFetch`: binary bodies,
 *  blob/text responses, and the handful that read the status or headers
 *  themselves. Callers keep their own error handling — deliberately, because
 *  what a failure means differs (a 409 is a conflict to resolve, an over-long
 *  body is a download to offer instead). */
export function apiRequest(path: string, init: RequestInit = {}): Promise<Response> {
  return authHeaders(init.headers).then((headers) =>
    fetch(apiUrl(path), { ...init, headers, credentials: credentialsMode() }),
  );
}

/** The WebSocket endpoint: derived from the configured instance in device mode,
 *  from the serving origin in web mode. The scheme follows the base's (a TLS
 *  instance gets `wss:`). */
export function wsBase(): string {
  const base = instanceBase();
  if (base) return `${base.replace(/^http/, "ws")}/ws`;
  const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
  return `${proto}//${window.location.host}/ws`;
}

/** Save a blob to disk under `filename` via a transient anchor. Shared by every
 *  download path: the bytes always arrive through an authenticated fetch, so a
 *  plain link to the endpoint would not carry the credential. */
export function saveBlob(blob: Blob, filename: string): void {
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}
