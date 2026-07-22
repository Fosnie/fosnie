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

// A development-only way to point this SPA at a remote instance, so the
// cross-origin path can be exercised in a plain browser without a native shell.
// It is reached at `#/connect` and is compiled out of a production build (its
// only entry point sits behind `import.meta.env.DEV`).
//
// This is NOT the product's pairing surface. A real client pairs through its own
// shell and hands the SPA a ready token; nothing here ships to a user.

import { useState } from "react";
import { configureInstance } from "@/api/instance";

/** The prefix every platform token carries, so a pasted secret can be told apart
 *  from a pairing code without asking which one it is. */
const TOKEN_PREFIX = "sk-fosnie-";

/** Pairing codes are read off one screen and typed into another, so people group
 *  and lower-case them. Fold that back before sending. */
function normaliseCode(raw: string): string {
  return raw.replace(/[\s-]/g, "").toUpperCase();
}

/** Add a scheme when the operator typed a bare host, and drop trailing slashes. */
function normaliseBase(raw: string): string {
  const t = raw.trim().replace(/\/+$/, "");
  return /^https?:\/\//i.test(t) ? t : `https://${t}`;
}

function guessPlatform(): string {
  const ua = navigator.userAgent;
  if (/Windows/i.test(ua)) return "windows";
  if (/Mac OS X|Macintosh/i.test(ua)) return "macos";
  return "linux";
}

/** Redeem a pairing code for a device token. Public endpoint: the code is the
 *  credential. */
async function redeemCode(base: string, code: string): Promise<string> {
  const res = await fetch(`${base}/api/device/pair`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ code, name: "Browser (development)", platform: guessPlatform() }),
  });
  if (!res.ok) {
    const body = await res.text().catch(() => "");
    throw new Error(`${res.status} ${res.statusText}${body ? `: ${body.slice(0, 200)}` : ""}`);
  }
  const { token } = (await res.json()) as { token: string };
  return token;
}

/** Connect form. `onReady` fires once the instance is configured, so the caller
 *  can render the app without a reload. */
export function DevConnect({ onReady }: { onReady: () => void }) {
  const [instance, setInstance] = useState(
    () => new URLSearchParams(window.location.search).get("instance") ?? "",
  );
  const [secret, setSecret] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const connect = async () => {
    if (busy) return;
    setBusy(true);
    setErr(null);
    try {
      const baseUrl = normaliseBase(instance);
      const raw = secret.trim();
      if (!baseUrl || !raw) throw new Error("An instance URL and a code or token are both needed.");
      const token = raw.startsWith(TOKEN_PREFIX) ? raw : await redeemCode(baseUrl, normaliseCode(raw));
      configureInstance({ baseUrl, token });
      onReady();
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
      setBusy(false);
    }
  };

  return (
    <div className="signin-wrap">
      <div className="signin-card anim-on fade-up" style={{ maxWidth: 520 }}>
        <div className="eyebrow">Development</div>
        <h1 className="serif signin-title">Connect to an instance</h1>
        <p className="signin-sub" style={{ marginBottom: 18 }}>
          Point this build at a remote instance and authenticate as a paired device. Mint a pairing
          code from that instance under Profile → Devices, or paste a device token.
        </p>
        <form
          className="signin-form"
          style={{ display: "flex", flexDirection: "column", gap: 16, marginTop: 24 }}
          onSubmit={(e) => {
            e.preventDefault();
            void connect();
          }}
        >
          <input
            className="field"
            type="text"
            placeholder="Instance URL, e.g. https://ai.example.com"
            value={instance}
            onChange={(e) => setInstance(e.target.value)}
            autoComplete="off"
            required
          />
          <input
            className="field mono"
            type="text"
            placeholder="Pairing code or device token"
            value={secret}
            onChange={(e) => setSecret(e.target.value)}
            autoComplete="off"
            required
          />
          {err && <div style={{ color: "var(--color-danger, #e5484d)", fontSize: 13 }}>{err}</div>}
          <button type="submit" disabled={busy} className={"btn btn-gold signin-btn" + (busy ? " is-busy" : "")}>
            {busy ? "Connecting…" : "Connect"}
          </button>
        </form>
      </div>
    </div>
  );
}
