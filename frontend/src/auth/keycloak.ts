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

import Keycloak from "keycloak-js";
import { authMode } from "@/auth/config";

// Single keycloak-js instance (public SPA client, PKCE). Tokens live in memory
// only — no localStorage for sensitive data.
export const keycloak = new Keycloak({
  url: import.meta.env.VITE_KEYCLOAK_URL ?? "http://localhost:8081",
  realm: import.meta.env.VITE_KEYCLOAK_REALM ?? "fosnie",
  clientId: import.meta.env.VITE_KEYCLOAK_CLIENT_ID ?? "fosnie-spa",
});

let initialised: Promise<boolean> | null = null;

/** Initialise once (check-sso + PKCE). Resolves to whether a session exists. */
export function initKeycloak(): Promise<boolean> {
  if (!initialised) {
    initialised = keycloak.init({
      onLoad: "check-sso",
      pkceMethod: "S256",
      silentCheckSsoRedirectUri: `${window.location.origin}/silent-check-sso.html`,
    });
  }
  return initialised;
}

/** A fresh access token, refreshed if it expires within 30s. Throws if absent.
 *  In local mode there is no Bearer token (the session is a cookie) — return an
 *  empty string so callers can attach a harmless header the backend ignores. */
export async function freshToken(): Promise<string> {
  if (authMode() === "local") return "";
  try {
    await keycloak.updateToken(30);
  } catch {
    // refresh failed — force re-login
    await keycloak.login();
  }
  if (!keycloak.token) throw new Error("not authenticated");
  return keycloak.token;
}
