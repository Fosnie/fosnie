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

// Second-factor (TOTP) enrolment flow, shared by the Profile
// Security section and the mandatory-MFA full-screen gate. setup → scan QR /
// enter secret → confirm a code → show one-time recovery codes → done.

import { useState } from "react";
import QRCode from "react-qr-code";
import { mfaSetup, mfaConfirm, type MfaSetup } from "@/api/client";
import { Icon } from "@/components/icons";
import { toast } from "@/components/dialogs";

/** Show a set of one-time recovery codes with copy / download. Shown ONCE. */
export function RecoveryCodesPanel({ codes }: { codes: string[] }) {
  const text = codes.join("\n");
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text);
      toast("Recovery codes copied.", { variant: "success" });
    } catch {
      toast("Could not copy — select and copy manually.", { variant: "error" });
    }
  };
  const download = () => {
    const blob = new Blob([text + "\n"], { type: "text/plain" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = "fosnie-recovery-codes.txt";
    a.click();
    URL.revokeObjectURL(url);
  };
  return (
    <div style={{ marginTop: 12 }}>
      <div className="ed-hint" style={{ color: "var(--danger, #f87171)", marginBottom: 8 }}>
        <Icon.Alert size={14} /> Save these recovery codes now — each works once, and they are shown only this time. Use one if you lose your authenticator.
      </div>
      <div className="mono" style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: "4px 18px", padding: "12px 14px", borderRadius: 8, background: "var(--surface-2, rgba(120,120,120,0.08))", maxWidth: 360 }}>
        {codes.map((c) => <span key={c}>{c}</span>)}
      </div>
      <div className="row" style={{ gap: 8, marginTop: 10 }}>
        <button className="btn btn-line sm" onClick={copy}><Icon.Copy size={14} /> Copy all</button>
        <button className="btn btn-line sm" onClick={download}><Icon.Download size={14} /> Download</button>
      </div>
    </div>
  );
}

/** The enrolment wizard. Calls `onDone` after the user acknowledges the recovery
 *  codes (successful enrolment). */
export function MfaEnrolFlow({ onDone }: { onDone: () => void }) {
  const [setup, setSetup] = useState<MfaSetup | null>(null);
  const [code, setCode] = useState("");
  const [codes, setCodes] = useState<string[] | null>(null);
  const [busy, setBusy] = useState(false);

  const begin = async () => {
    setBusy(true);
    try {
      setSetup(await mfaSetup());
    } catch (e) {
      toast(`Could not start setup: ${e instanceof Error ? e.message : String(e)}`, { variant: "error" });
    } finally {
      setBusy(false);
    }
  };

  const confirm = async () => {
    if (code.trim().length === 0) return;
    setBusy(true);
    try {
      const res = await mfaConfirm(code.trim());
      setCodes(res.recovery_codes);
    } catch (e) {
      toast(`Incorrect code: ${e instanceof Error ? e.message : String(e)}`, { variant: "error" });
    } finally {
      setBusy(false);
    }
  };

  // Step 3: recovery codes, then done.
  if (codes) {
    return (
      <div>
        <div className="ed-hint" style={{ marginBottom: 4 }}><Icon.Check size={14} /> Two-step verification is on.</div>
        <RecoveryCodesPanel codes={codes} />
        <button className="btn btn-gold sm" style={{ marginTop: 14 }} onClick={onDone}>Done</button>
      </div>
    );
  }

  // Step 2: scan / enter the secret, then confirm a code.
  if (setup) {
    return (
      <div>
        <div className="ed-hint" style={{ marginBottom: 10 }}>
          Scan this with an authenticator app (Google Authenticator, Aegis, 1Password…), or enter the key manually, then type the 6-digit code it shows.
        </div>
        <div style={{ display: "inline-block", padding: 12, background: "#fff", borderRadius: 8 }}>
          <QRCode value={setup.otpauth_url} size={168} />
        </div>
        <div style={{ marginTop: 10, marginBottom: 12 }}>
          <div className="ed-hint" style={{ marginBottom: 4 }}>Or enter this key manually:</div>
          <code className="mono" style={{ userSelect: "all", wordBreak: "break-all" }}>{setup.secret}</code>
        </div>
        <div className="row" style={{ gap: 8, flexWrap: "wrap", maxWidth: 360 }}>
          <input
            className="field"
            style={{ flex: "1 1 100%" }}
            type="text"
            inputMode="numeric"
            autoComplete="one-time-code"
            placeholder="6-digit code"
            value={code}
            onChange={(e) => setCode(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter") confirm(); }}
          />
          <button className="btn btn-gold sm" disabled={busy || code.trim().length === 0} onClick={confirm}>
            <Icon.Shield size={14} /> {busy ? "Verifying…" : "Confirm & enable"}
          </button>
        </div>
      </div>
    );
  }

  // Step 1: intro.
  return (
    <div>
      <div className="ed-hint" style={{ marginBottom: 10 }}>
        Add a second step to sign-in using an authenticator app. You will still enter your password, then a rotating 6-digit code.
      </div>
      <button className="btn btn-gold sm" disabled={busy} onClick={begin}>
        <Icon.Shield size={14} /> {busy ? "Starting…" : "Set up two-step verification"}
      </button>
    </div>
  );
}
