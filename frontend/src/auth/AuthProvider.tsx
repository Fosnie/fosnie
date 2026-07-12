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

import { createContext, use, useEffect, useState, type ReactNode } from "react";
import { initKeycloak, keycloak } from "@/auth/keycloak";
import { authMode, loadAuthConfig, type AuthMode } from "@/auth/config";
import { queryClient } from "@/api/client";

interface AuthState {
  ready: boolean;
  authenticated: boolean;
  mode: AuthMode;
  /** Keycloak SSO redirect (keycloak mode only). */
  login: () => void;
  logout: () => void;
  /** Local email/password sign-in. When the account has a second factor enabled,
   *  resolves `{ mfaRequired: true, pending }` WITHOUT signing in — the caller must
   *  then call `mfaVerify`. Throws with a message on failure. */
  loginLocal: (email: string, password: string) => Promise<LoginResult>;
  /** Complete a two-step sign-in with a TOTP or recovery code. Throws on failure. */
  mfaVerify: (pending: string, code: string) => Promise<void>;
  /** Local registration (first user becomes admin). Throws on failure. */
  registerLocal: (email: string, password: string, displayName?: string) => Promise<void>;
}

/** Result of `loginLocal`: either signed in, or a pending two-step challenge. */
export type LoginResult = { mfaRequired: false } | { mfaRequired: true; pending: string };

const AuthContext = createContext<AuthState | null>(null);

/** True if a local session cookie is currently valid (whoami succeeds). */
async function hasLocalSession(): Promise<boolean> {
  try {
    const res = await fetch("/api/whoami", { credentials: "include" });
    return res.ok;
  } catch {
    return false;
  }
}

/** POST JSON with the session cookie; return the parsed body. Throws on non-ok. */
async function postAuthJson<T = unknown>(path: string, body: unknown): Promise<T> {
  const res = await fetch(path, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    credentials: "include",
    body: JSON.stringify(body),
  });
  if (!res.ok) {
    const text = await res.text().catch(() => "");
    throw new Error(text || `${res.status} ${res.statusText}`);
  }
  return (await res.json().catch(() => ({}))) as T;
}

async function postAuth(path: string, body: unknown): Promise<void> {
  await postAuthJson(path, body);
}

export function AuthProvider({ children }: { children: ReactNode }) {
  const [ready, setReady] = useState(false);
  const [authenticated, setAuthenticated] = useState(false);
  const [mode, setMode] = useState<AuthMode>("keycloak");

  useEffect(() => {
    let alive = true;
    loadAuthConfig().then((cfg) => {
      if (!alive) return;
      setMode(cfg.mode);
      if (cfg.mode === "keycloak") {
        initKeycloak()
          .then((auth) => alive && (setAuthenticated(auth), setReady(true)))
          .catch(() => alive && setReady(true));
      } else {
        hasLocalSession().then((ok) => alive && (setAuthenticated(ok), setReady(true)));
      }
    });
    return () => {
      alive = false;
    };
  }, []);

  const value: AuthState = {
    ready,
    authenticated,
    mode,
    login: () => void keycloak.login(),
    logout: () => {
      if (authMode() === "keycloak") {
        void keycloak.logout({ redirectUri: window.location.origin });
      } else {
        void postAuth("/api/auth/logout", {}).finally(() => {
          queryClient.clear();
          setAuthenticated(false);
        });
      }
    },
    loginLocal: async (email, password) => {
      const res = await postAuthJson<{ mfa_required?: boolean; pending?: string }>(
        "/api/auth/login",
        { email, password },
      );
      if (res.mfa_required && res.pending) {
        // Password verified, but no session yet — the caller must supply a code.
        return { mfaRequired: true, pending: res.pending };
      }
      queryClient.clear();
      setAuthenticated(true);
      return { mfaRequired: false };
    },
    mfaVerify: async (pending, code) => {
      await postAuth("/api/auth/mfa/verify", { pending, code });
      queryClient.clear();
      setAuthenticated(true);
    },
    registerLocal: async (email, password, displayName) => {
      await postAuth("/api/auth/register", { email, password, display_name: displayName });
      queryClient.clear();
      setAuthenticated(true);
    },
  };
  return <AuthContext value={value}>{children}</AuthContext>;
}

export function useAuth(): AuthState {
  const ctx = use(AuthContext);
  if (!ctx) throw new Error("useAuth outside AuthProvider");
  return ctx;
}
