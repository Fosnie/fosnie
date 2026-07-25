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

// The first thing a new installation shows: which instance, and prove you are
// allowed on it.
//
// Two steps rather than one form. Naming the instance is checked on its own, so
// a wrong address, an unreachable network or a release too old to pair a device
// is named as such, instead of surfacing later as a code that "did not work".
// The code itself is minted by the owner from a signed-in session on the web —
// this client never asks for a password, and has no way to accept one.

import { useState } from "react";
import { openExternal, pair, validateInstance, type InstanceConfig } from "@/shell/bridge";
import { normaliseBase, normaliseCode } from "@/shell/format";

/** Where the pairing code is minted, quoted as the user will see it. */
const WHERE_TO_LOOK = "Profile → Connected devices → Pair a device";

export function Pairing({
  onPaired,
  notice,
}: {
  onPaired: (cfg: InstanceConfig) => void;
  /** Why the user is back here, when they did not arrive by launching fresh. */
  notice?: string;
}) {
  const [step, setStep] = useState<"instance" | "code">("instance");
  const [instance, setInstance] = useState("");
  const [code, setCode] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const fail = (e: unknown) => {
    setErr(typeof e === "string" ? e : e instanceof Error ? e.message : String(e));
    setBusy(false);
  };

  const checkInstance = async () => {
    if (busy) return;
    setBusy(true);
    setErr(null);
    try {
      const info = await validateInstance(normaliseBase(instance));
      setInstance(info.base_url);
      setStep("code");
      setBusy(false);
    } catch (e) {
      fail(e);
    }
  };

  const redeem = async () => {
    if (busy) return;
    setBusy(true);
    setErr(null);
    try {
      onPaired(await pair(instance, normaliseCode(code)));
    } catch (e) {
      fail(e);
    }
  };

  return (
    <div className="signin-wrap">
      <div className="signin-card anim-on fade-up" style={{ maxWidth: 520 }}>
        <div className="eyebrow">Fosnie</div>
        <h1 className="serif signin-title">
          {step === "instance" ? "Connect to your instance" : "Pair this computer"}
        </h1>
        {notice && (
          <p className="signin-sub" style={{ marginBottom: 6 }}>
            {notice}
          </p>
        )}
        <p className="signin-sub" style={{ marginBottom: 18 }}>
          {step === "instance"
            ? "Enter the address of the instance you work in. Your administrator can tell you what it is."
            : `Open ${instance} in your browser, go to ${WHERE_TO_LOOK}, and enter the code it shows. Codes last ten minutes and work once.`}
        </p>

        <form
          className="signin-form"
          style={{ display: "flex", flexDirection: "column", gap: 16, marginTop: 24 }}
          onSubmit={(e) => {
            e.preventDefault();
            void (step === "instance" ? checkInstance() : redeem());
          }}
        >
          {step === "instance" ? (
            <input
              className="field"
              type="text"
              placeholder="Instance address, for example ai.example.com"
              value={instance}
              onChange={(e) => setInstance(e.target.value)}
              autoComplete="off"
              autoFocus
              required
            />
          ) : (
            <input
              className="field mono"
              type="text"
              placeholder="Pairing code"
              value={code}
              onChange={(e) => setCode(e.target.value)}
              autoComplete="off"
              autoFocus
              required
            />
          )}

          {err && <div style={{ color: "var(--color-danger, #e5484d)", fontSize: 13 }}>{err}</div>}

          <button
            type="submit"
            disabled={busy}
            className={"btn btn-gold signin-btn" + (busy ? " is-busy" : "")}
          >
            {busy
              ? step === "instance"
                ? "Checking…"
                : "Pairing…"
              : step === "instance"
                ? "Continue"
                : "Pair this computer"}
          </button>

          {step === "code" && (
            <div style={{ display: "flex", justifyContent: "space-between", fontSize: 13 }}>
              <button
                type="button"
                className="btn btn-ghost"
                onClick={() => {
                  setErr(null);
                  setStep("instance");
                }}
              >
                Change instance
              </button>
              <button
                type="button"
                className="btn btn-ghost"
                onClick={() => void openExternal(`${instance}/profile`).catch(fail)}
              >
                Open my profile page
              </button>
            </div>
          )}
        </form>
      </div>
    </div>
  );
}
