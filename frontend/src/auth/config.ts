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

// Auth mode is decided by the backend: `local` =
// email/password with an httpOnly session cookie; `keycloak` = SSO (PKCE Bearer).
// Fetched once at boot from the public `GET /api/auth/config`, before keycloak-js
// is touched, so the SPA renders the right login UI and the API client knows
// whether to attach a Bearer token.

import { apiUrl, credentialsMode } from "@/api/instance";

export type AuthMode = "local" | "keycloak";

export interface AuthConfig {
  mode: AuthMode;
  local_enabled: boolean;
  keycloak_url?: string | null;
  /** Optional label for the SSO button (Enterprise federated SSO), e.g. the
   *  customer IdP name. Absent ⇒ the generic "Sign in with SSO". */
  sso_label?: string | null;
  /** Optional logo URL shown on the SSO button. */
  sso_logo_url?: string | null;
  /** Whether a second factor is mandatory for everyone. Local mode
   *  only; lets the register screen warn before sign-up. */
  require_mfa?: boolean;
  /** Authoritative minimum local-password length (backend `password_min_len`),
   *  so the SPA's hints match what registration/change-password enforce. */
  password_min_len?: number;
  /** Whether the login screen should offer "create an account": true when the
   *  instance is empty (first registrant bootstraps admin) or an admin has opened
   *  self-registration. False ⇒ sign-in only. Absent (older backend) ⇒ treated as
   *  open, preserving prior behaviour. */
  registration_open?: boolean;
}

let cached: AuthConfig | null = null;

/** Load (and memoise) the auth config. On any failure, fall back to `keycloak`
 *  so an older/SSO deployment keeps working. */
export async function loadAuthConfig(): Promise<AuthConfig> {
  if (cached) return cached;
  try {
    const res = await fetch(apiUrl("/api/auth/config"), { credentials: credentialsMode() });
    cached = res.ok ? ((await res.json()) as AuthConfig) : { mode: "keycloak", local_enabled: false };
  } catch {
    cached = { mode: "keycloak", local_enabled: false };
  }
  return cached;
}

/** The mode this deployment serves, after `loadAuthConfig()`. Defaults to
 *  `keycloak` until then.
 *
 *  This is the *server's* answer, which a paired device overrides — a device
 *  authenticates with its own token whatever the deployment's login flow is. Use
 *  `authMode()` from `@/api/instance` wherever that distinction matters; this one
 *  is for deciding what the deployment itself supports. */
export function serverAuthMode(): AuthMode {
  return cached?.mode ?? "keycloak";
}

/** The memoised auth config (or `null` before `loadAuthConfig()` resolves). */
export function authConfig(): AuthConfig | null {
  return cached;
}
