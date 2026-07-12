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

import { useState } from "react";
import { NeuralBackground } from "@/components/NeuralBackground";
import { Icon } from "@/components/icons";
import { useAuth } from "@/auth/AuthProvider";
import { authConfig } from "@/auth/config";

export function Login() {
  const { mode } = useAuth();

  return (
    <div className="signin-wrap">
      <div className="absolute inset-0 -z-10 opacity-60"><NeuralBackground /></div>

      <div className="signin-card anim-on fade-up">
        <div className="signin-mark has-logo">
          <img src="/logo.svg" alt="Private AI" className="signin-logo" onError={(e) => (e.currentTarget.style.display = "none")} />
        </div>
        <div className="eyebrow" style={{ marginTop: 18 }}>Fosnie</div>
        <h1 className="serif signin-title">Welcome, stranger</h1>

        {mode === "local" ? <LocalForm /> : <SsoButton />}

        <div className="signin-foot">Fosnie © 2026</div>
      </div>
    </div>
  );
}

function SsoButton() {
  const { login } = useAuth();
  const [busy, setBusy] = useState(false);
  const go = () => { setBusy(true); login(); };
  // Optional customer-IdP branding (Enterprise federated SSO). When an admin has
  // set a label/logo, the button names the IdP; otherwise it stays generic.
  const cfg = authConfig();
  const label = cfg?.sso_label?.trim() || null;
  const logo = cfg?.sso_logo_url?.trim() || null;
  return (
    <>
      <p className="signin-sub">Sign in with your credentials first.</p>
      <button onClick={go} disabled={busy} className={"btn btn-gold signin-btn" + (busy ? " is-busy" : "")}>
        {busy ? <span className="spin" /> : logo ? (
          <img src={logo} alt="" className="signin-idp-logo" style={{ height: 16, width: 16 }} onError={(e) => (e.currentTarget.style.display = "none")} />
        ) : <Icon.Lock size={16} />}
        {busy ? "Redirecting to SSO…" : label ? `Continue with ${label}` : "Continue with single sign-on"}
      </button>
    </>
  );
}

function LocalForm() {
  const { loginLocal, registerLocal, mfaVerify } = useAuth();
  const [register, setRegister] = useState(false);
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Two-step sign-in: once the password is accepted for an MFA-enabled account, the
  // backend returns a `pending` token and we swap the form for a code prompt.
  const [pending, setPending] = useState<string | null>(null);
  const [code, setCode] = useState("");

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      if (register) {
        await registerLocal(email.trim(), password, displayName.trim() || undefined);
      } else {
        const res = await loginLocal(email.trim(), password);
        if (res.mfaRequired) {
          setPending(res.pending);
          setBusy(false);
          return;
        }
      }
      // On success the AuthProvider flips `authenticated` and the app re-renders.
    } catch (err) {
      setError(err instanceof Error ? err.message : "Sign-in failed");
      setBusy(false);
    }
  };

  const submitCode = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!pending) return;
    setBusy(true);
    setError(null);
    try {
      await mfaVerify(pending, code.trim());
      // Success → AuthProvider flips `authenticated`.
    } catch (err) {
      setError(err instanceof Error ? err.message : "Verification failed");
      setBusy(false);
    }
  };

  const startOver = () => {
    setPending(null);
    setCode("");
    setPassword("");
    setError(null);
    setBusy(false);
  };

  if (pending) {
    return (
      <>
        <p className="signin-sub">Two-step verification — enter the code from your authenticator app, or a recovery code.</p>
        <form onSubmit={submitCode} className="signin-form" style={{ display: "flex", flexDirection: "column", gap: 16, marginTop: 24 }}>
          <input
            className="field"
            type="text"
            inputMode="text"
            placeholder="6-digit code or recovery code"
            value={code}
            onChange={(e) => setCode(e.target.value)}
            autoComplete="one-time-code"
            autoFocus
            required
          />
          {error && <div className="signin-error" style={{ color: "var(--color-danger, #e5484d)", fontSize: 13 }}>{error}</div>}
          <button type="submit" disabled={busy || code.trim().length === 0} className={"btn btn-gold signin-btn" + (busy ? " is-busy" : "")}>
            {busy ? <span className="spin" /> : <Icon.Lock size={16} />}
            {busy ? "Verifying…" : "Verify"}
          </button>
        </form>
        <button
          type="button"
          className="btn-link"
          style={{ marginTop: 14, background: "none", border: "none", cursor: "pointer", color: "var(--color-gold)" }}
          onClick={startOver}
        >
          Start over
        </button>
      </>
    );
  }

  return (
    <>
      <p className="signin-sub">{register ? "Create your account." : "Sign in to continue."}</p>
      <form onSubmit={submit} className="signin-form" style={{ display: "flex", flexDirection: "column", gap: 16, marginTop: 24 }}>
        {register && (
          <input
            className="field"
            type="text"
            placeholder="Display name (optional)"
            value={displayName}
            onChange={(e) => setDisplayName(e.target.value)}
            autoComplete="name"
          />
        )}
        <input
          className="field"
          type="email"
          placeholder="Email"
          value={email}
          onChange={(e) => setEmail(e.target.value)}
          autoComplete="username"
          required
        />
        <input
          className="field"
          type="password"
          placeholder="Password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          autoComplete={register ? "new-password" : "current-password"}
          required
        />
        {error && <div className="signin-error" style={{ color: "var(--color-danger, #e5484d)", fontSize: 13 }}>{error}</div>}
        <button type="submit" disabled={busy} className={"btn btn-gold signin-btn" + (busy ? " is-busy" : "")}>
          {busy ? <span className="spin" /> : <Icon.Lock size={16} />}
          {busy ? "Please wait…" : register ? "Create account" : "Sign in"}
        </button>
      </form>
      {/* Self-registration link — hidden when registration is closed (absent flag ⇒
          open, preserving older-backend behaviour). Keep it while in register mode so
          a user can toggle back to sign-in. */}
      {(authConfig()?.registration_open !== false || register) && (
        <button
          type="button"
          className="btn-link"
          style={{ marginTop: 14, background: "none", border: "none", cursor: "pointer", color: "var(--color-gold)" }}
          onClick={() => { setError(null); setRegister((r) => !r); }}
        >
          {register ? "Have an account? Sign in" : "New here? Create an account"}
        </button>
      )}
    </>
  );
}
