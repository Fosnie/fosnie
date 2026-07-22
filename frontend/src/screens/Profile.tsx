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

import { useEffect, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "react-router-dom";
import {
  changePassword,
  mfaStatus,
  mfaDisable,
  mfaRegenerate,
  clearMyProvider,
  deleteAccount,
  removeMyAvatar,
  setMyProvider,
  testMyProvider,
  createMyLlm,
  updateMyLlm,
  deleteMyLlm,
  testMyLlm,
  useMyLlmProviders,
  updateMyName,
  uploadMyAvatar,
  useMyProfile,
  useMyProviders,
  useWhoami,
  useConnectorConnections,
  connectConnector,
  disconnectConnector,
  useMyMcpConnections,
  connectMcpServer,
  disconnectMcpServer,
  useMyApiKeys,
  createApiKey,
  revokeApiKey,
  useMyDevices,
  createPairingCode,
  revokeDevice,
  type Device,
  type CreatedApiKey,
  type MyProvider,
  type LlmProviderOption,
  type ProviderTestResult,
  type ConnectorConnection,
} from "@/api/client";
import { Avatar } from "@/components/Avatar";
import { MfaEnrolFlow, RecoveryCodesPanel } from "@/components/MfaEnrol";
import { confirmDialog, toast } from "@/components/dialogs";
import { PanelHead } from "@/components/editor";
import { Icon } from "@/components/icons";
import { useAppearance } from "@/app/AppearanceContext";
import { useAuth } from "@/auth/AuthProvider";
import { authConfig } from "@/auth/config";
import { authMode } from "@/api/instance";

// Minimum new-password length. The backend (`password_min_len`) is authoritative
// and delivers it via /api/auth/config; fall back to 10 (the backend default)
// until that resolves.
const DEFAULT_PASSWORD_MIN = 10;

// Local-auth password change (current → new → confirm). Rendered only under
// AUTH_MODE=local; Keycloak users manage password/MFA in the KC account console.
function PasswordChangeForm() {
  const PASSWORD_MIN = authConfig()?.password_min_len ?? DEFAULT_PASSWORD_MIN;
  const [cur, setCur] = useState("");
  const [next, setNext] = useState("");
  const [confirm, setConfirm] = useState("");
  const [busy, setBusy] = useState(false);
  const tooShort = next.length > 0 && next.length < PASSWORD_MIN;
  const mismatch = confirm.length > 0 && next !== confirm;
  const canSubmit = cur.length > 0 && next.length >= PASSWORD_MIN && next === confirm && !busy;

  async function submit() {
    if (!canSubmit) return;
    setBusy(true);
    try {
      await changePassword(cur, next);
      toast("Password changed.", { variant: "success" });
      setCur(""); setNext(""); setConfirm("");
    } catch (e) {
      toast(`Failed: ${e instanceof Error ? e.message : String(e)}`, { variant: "error" });
    } finally {
      setBusy(false);
    }
  }

  return (
    <div style={{ marginTop: 14 }}>
      <label className="form-label">Change password</label>
      <div className="ed-hint" style={{ marginBottom: 8 }}>You manage your own password here.</div>
      <div className="row" style={{ gap: 8, flexWrap: "wrap", maxWidth: 460 }}>
        <input className="field" style={{ flex: "1 1 100%" }} type="password" autoComplete="current-password"
          placeholder="Current password" value={cur} onChange={(e) => setCur(e.target.value)} />
        <input className="field" style={{ flex: "1 1 100%" }} type="password" autoComplete="new-password"
          placeholder={`New password (min ${PASSWORD_MIN} chars)`} value={next} onChange={(e) => setNext(e.target.value)} />
        <input className="field" style={{ flex: "1 1 100%" }} type="password" autoComplete="new-password"
          placeholder="Confirm new password" value={confirm} onChange={(e) => setConfirm(e.target.value)}
          onKeyDown={(e) => { if (e.key === "Enter") submit(); }} />
        <button className="btn btn-gold sm" disabled={!canSubmit} onClick={submit}>
          <Icon.Key size={14} /> {busy ? "Saving…" : "Update password"}
        </button>
      </div>
      {tooShort && <div className="ed-hint" style={{ color: "var(--danger, #f87171)", marginTop: 6 }}>Password must be at least {PASSWORD_MIN} characters.</div>}
      {mismatch && <div className="ed-hint" style={{ color: "var(--danger, #f87171)", marginTop: 6 }}>Passwords do not match.</div>}
    </div>
  );
}

// Security tab: two-step verification + the local password change.
// Local-auth only (Keycloak owns its own OTP/password).
function SecuritySection() {
  const qc = useQueryClient();
  const status = useQuery({ queryKey: ["mfa-status"], queryFn: mfaStatus });
  const refresh = () => qc.invalidateQueries({ queryKey: ["mfa-status"] });

  return (
    <section className="prof-section">
      <label className="form-label">Two-step verification</label>
      {status.isLoading ? (
        <div className="ed-hint">Loading…</div>
      ) : status.data?.enabled ? (
        <MfaEnabledPanel recoveryRemaining={status.data.recovery_remaining} onChange={refresh} />
      ) : (
        <MfaEnrolFlow onDone={refresh} />
      )}
      <div style={{ marginTop: 24 }}>
        <PasswordChangeForm />
      </div>
    </section>
  );
}

// The enrolled state: status + regenerate recovery codes + disable. Both sensitive
// actions require the password AND a current factor (a stolen session can't weaken
// MFA — the backend enforces this too).
function MfaEnabledPanel({ recoveryRemaining, onChange }: { recoveryRemaining: number; onChange: () => void }) {
  const [mode, setMode] = useState<null | "disable" | "regen">(null);
  const [password, setPassword] = useState("");
  const [code, setCode] = useState("");
  const [busy, setBusy] = useState(false);
  const [newCodes, setNewCodes] = useState<string[] | null>(null);

  const reset = () => { setMode(null); setPassword(""); setCode(""); setBusy(false); };

  const submit = async () => {
    if (!mode || password.length === 0 || code.trim().length === 0) return;
    setBusy(true);
    try {
      if (mode === "disable") {
        await mfaDisable(password, code.trim());
        toast("Two-step verification disabled.", { variant: "success" });
        reset();
        onChange();
      } else {
        const res = await mfaRegenerate(password, code.trim());
        setNewCodes(res.recovery_codes);
        reset();
        onChange();
      }
    } catch (e) {
      toast(`Failed: ${e instanceof Error ? e.message : String(e)}`, { variant: "error" });
      setBusy(false);
    }
  };

  return (
    <div>
      <div className="ed-hint" style={{ marginBottom: 10 }}>
        <Icon.Check size={14} /> On. {recoveryRemaining} recovery {recoveryRemaining === 1 ? "code" : "codes"} remaining.
      </div>

      {newCodes && <RecoveryCodesPanel codes={newCodes} />}

      {mode ? (
        <div className="row" style={{ gap: 8, flexWrap: "wrap", maxWidth: 460, marginTop: 10 }}>
          <div className="ed-hint" style={{ flex: "1 1 100%" }}>
            {mode === "disable" ? "Confirm with your password and a current code to disable." : "Confirm with your password and a current code to issue a new set (old codes stop working)."}
          </div>
          <input className="field" style={{ flex: "1 1 100%" }} type="password" autoComplete="current-password"
            placeholder="Password" value={password} onChange={(e) => setPassword(e.target.value)} />
          <input className="field" style={{ flex: "1 1 100%" }} type="text" autoComplete="one-time-code"
            placeholder="6-digit or recovery code" value={code} onChange={(e) => setCode(e.target.value)} />
          <button className={mode === "disable" ? "btn btn-danger sm" : "btn btn-gold sm"} disabled={busy || !password || !code.trim()} onClick={submit}>
            {busy ? "Working…" : mode === "disable" ? "Disable" : "Regenerate"}
          </button>
          <button className="btn btn-line sm" disabled={busy} onClick={reset}>Cancel</button>
        </div>
      ) : (
        <div className="row" style={{ gap: 8, marginTop: 4 }}>
          <button className="btn btn-line sm" onClick={() => { setNewCodes(null); setMode("regen"); }}>
            <Icon.Refresh size={14} /> Regenerate recovery codes
          </button>
          <button className="btn btn-line sm" onClick={() => setMode("disable")}>
            <Icon.Lock size={14} /> Disable two-step
          </button>
        </div>
      )}
    </div>
  );
}

// Segmented single-choice control reusing the existing .btn-line `.on` styling.
function Seg<T extends string>({
  value, options, onChange,
}: { value: T; options: { value: T; label: string }[]; onChange: (v: T) => void }) {
  return (
    <div className="row" style={{ gap: 8, flexWrap: "wrap" }}>
      {options.map((o) => (
        <button
          key={o.value}
          type="button"
          className={"btn btn-line sm" + (value === o.value ? " on" : "")}
          aria-pressed={value === o.value}
          onClick={() => onChange(o.value)}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

const ROLE_LABEL: Record<string, string> = {
  super_admin: "Super admin",
  client_admin: "Admin",
  power_user: "Power user",
  user: "Member",
};

const MAX_AVATAR_BYTES = 2 * 1024 * 1024;

const PROVIDER_ROLES: [string, string][] = [
  ["llm", "LLM (chat)"],
  ["embed", "Embeddings"],
  ["rerank", "Reranker"],
  ["ocr", "OCR"],
  ["stt", "Speech-to-text"],
  ["tts", "Text-to-speech"],
  ["verify", "Verifier"],
];
const SOURCE_LABEL: Record<MyProvider["source"], string> = {
  user: "your key",
  deployment: "deployment",
  default: "built-in",
};

interface PDraft { base_url: string; model: string; api_key: string; enabled: boolean }

// Inline result of a provider "Test connection" probe: ✓ latency / ✗ reason.
function ProviderTestStatus({ s }: { s: ProviderTestResult | "loading" | undefined }) {
  if (!s) return null;
  if (s === "loading") return <span style={{ fontSize: 11, opacity: 0.6 }}>testing…</span>;
  if (s.ok) return <span style={{ fontSize: 11, color: "#34d399" }}>✓ {Math.round(s.latency_ms)} ms{s.detail ? ` · ${s.detail}` : ""}</span>;
  return <span style={{ fontSize: 11, color: "#f87171" }}>✗ {s.error ?? "failed"}</span>;
}

// Per-user BYOK: the LLM role as a LIST of the user's own named chat models
// (multi-LLM). Picked per conversation in the composer; deployment providers stay
// available and are managed by admins.
interface MyLlmDraft { label: string; base_url: string; model: string; api_key: string; enabled: boolean }
const blankMyLlm = (): MyLlmDraft => ({ label: "", base_url: "", model: "", api_key: "", enabled: true });

function MyLlmCard() {
  const qc = useQueryClient();
  const list = useMyLlmProviders(null);
  const own = (list.data?.providers ?? []).filter((p) => p.source === "user");
  const [busy, setBusy] = useState(false);
  const [edits, setEdits] = useState<Record<string, MyLlmDraft>>({});
  const [adding, setAdding] = useState<MyLlmDraft | null>(null);
  const [tests, setTests] = useState<Record<string, ProviderTestResult | "loading">>({});
  const refresh = () => qc.invalidateQueries({ queryKey: ["my-llm-providers"] });
  const toBody = (d: MyLlmDraft) => ({ label: d.label.trim(), base_url: d.base_url || undefined, model: d.model || undefined, api_key: d.api_key || undefined, enabled: d.enabled });
  async function act(fn: () => Promise<unknown>, msg: string) {
    setBusy(true);
    try { await fn(); refresh(); toast(msg, { variant: "success" }); }
    catch (e) { toast(`Failed: ${e instanceof Error ? e.message : String(e)}`, { variant: "error" }); }
    finally { setBusy(false); }
  }
  const startEdit = (p: LlmProviderOption) => setEdits((e) => ({ ...e, [p.id]: { label: p.label ?? "", base_url: p.base_url ?? "", model: p.model ?? "", api_key: "", enabled: p.enabled } }));
  const cancelEdit = (id: string) => setEdits((e) => { const n = { ...e }; delete n[id]; return n; });
  const field = (id: string, k: keyof MyLlmDraft, v: string | boolean) => setEdits((e) => ({ ...e, [id]: { ...e[id], [k]: v } }));
  const testRow = (key: string, d: MyLlmDraft, savedId?: string) => {
    setTests((t) => ({ ...t, [key]: "loading" }));
    testMyLlm({ id: savedId, base_url: d.base_url || undefined, model: d.model || undefined, api_key: d.api_key || undefined, enabled: d.enabled })
      .then((r) => setTests((t) => ({ ...t, [key]: r })))
      .catch((e) => setTests((t) => ({ ...t, [key]: { ok: false, latency_ms: 0, error: e instanceof Error ? e.message : "failed" } })));
  };
  const editor = (key: string, d: MyLlmDraft, apiKeySet: boolean, onField: (k: keyof MyLlmDraft, v: string | boolean) => void, onSave: () => void, onCancel: () => void) => (
    <div className="prof-card" style={{ marginBottom: 8, display: "block" }}>
      <div className="row" style={{ gap: 8, flexWrap: "wrap" }}>
        <input className="field" style={{ flex: "1 1 160px" }} placeholder="Display name" value={d.label} onChange={(e) => onField("label", e.target.value)} />
        <input className="field" style={{ flex: "1 1 160px" }} placeholder="Model (blank = default)" value={d.model} onChange={(e) => onField("model", e.target.value)} />
        <input className="field" style={{ flex: "1 1 200px" }} placeholder="Base URL (blank = default)" value={d.base_url} onChange={(e) => onField("base_url", e.target.value)} />
        <input className="field" style={{ flex: "1 1 160px" }} type="password" placeholder={apiKeySet ? "•••• set (blank = keep)" : "API key"} value={d.api_key} onChange={(e) => onField("api_key", e.target.value)} />
        <label className="row" style={{ gap: 6, fontSize: 13 }}><input type="checkbox" checked={d.enabled} onChange={(e) => onField("enabled", e.target.checked)} /> Enabled</label>
        <button type="button" className="btn btn-gold sm" disabled={busy || !d.label.trim()} onClick={onSave}><Icon.Save size={14} /> Save</button>
        <button type="button" className="btn btn-ghost sm" disabled={busy} onClick={onCancel}>Cancel</button>
        <button type="button" className="btn btn-line sm" onClick={() => testRow(key, d, key === "new" ? undefined : key)}>Test</button>
        <ProviderTestStatus s={tests[key]} />
      </div>
    </div>
  );

  return (
    <div style={{ marginBottom: 14 }}>
      <div className="row" style={{ justifyContent: "space-between", marginBottom: 8 }}>
        <div className="prof-card-title">My LLM providers <span className="mono" style={{ opacity: 0.5, fontSize: 11 }}>llm</span></div>
        {!adding && <button type="button" className="btn btn-line sm" disabled={busy} onClick={() => setAdding(blankMyLlm())}>＋ Add</button>}
      </div>
      <div className="ed-hint" style={{ marginBottom: 8 }}>Your own chat models — pick one per conversation in the composer. Deployment providers stay available.</div>
      {adding && editor("new", adding, false, (k, v) => setAdding((a) => (a ? { ...a, [k]: v } : a)), () => act(() => createMyLlm(toBody(adding)).then(() => setAdding(null)), "Provider added."), () => setAdding(null))}
      {own.map((p) => edits[p.id]
        ? editor(p.id, edits[p.id], p.api_key_set, (k, v) => field(p.id, k, v), () => act(() => updateMyLlm(p.id, toBody(edits[p.id])).then(() => cancelEdit(p.id)), "Provider saved."), () => cancelEdit(p.id))
        : (
          <div key={p.id} className="prof-card" style={{ marginBottom: 8, display: "flex", alignItems: "center", gap: 10 }}>
            <div style={{ minWidth: 0, flex: 1 }}>
              <div style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{p.label ?? "(unnamed)"}{!p.enabled && <span style={{ opacity: 0.5, fontSize: 11 }}> (disabled)</span>}</div>
              <div className="mono" style={{ opacity: 0.5, fontSize: 10, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{p.model ?? "(default)"}{p.base_url ? ` · ${p.base_url}` : ""}{p.api_key_set ? " · key set" : ""}</div>
            </div>
            <ProviderTestStatus s={tests[p.id]} />
            <button type="button" className="btn btn-line sm" onClick={() => testRow(p.id, { label: p.label ?? "", base_url: p.base_url ?? "", model: p.model ?? "", api_key: "", enabled: p.enabled }, p.id)}>Test</button>
            <button type="button" className="btn btn-line sm" disabled={busy} onClick={() => startEdit(p)}>Edit</button>
            <button type="button" className="btn btn-ghost sm" disabled={busy} onClick={() => act(() => deleteMyLlm(p.id), "Provider removed.")}><Icon.Trash size={14} /></button>
          </div>
        ))}
    </div>
  );
}

// Per-user BYOK editor. Rendered only when the deployment turns BYOK on
// (`user_byok_enabled`); otherwise it renders nothing (self-host default).
function MyProvidersSection() {
  const qc = useQueryClient();
  const q = useMyProviders();
  const [edits, setEdits] = useState<Record<string, PDraft>>({});
  const [busy, setBusy] = useState(false);
  const [tests, setTests] = useState<Record<string, ProviderTestResult | "loading">>({});
  const [saved, setSaved] = useState<Record<string, boolean>>({});
  const refresh = () => qc.invalidateQueries({ queryKey: ["my-providers"] });
  const runTest = (role: string, d: PDraft) => {
    setTests((t) => ({ ...t, [role]: "loading" }));
    testMyProvider(role, { base_url: d.base_url || undefined, model: d.model || undefined, api_key: d.api_key || undefined, enabled: d.enabled })
      .then((r) => setTests((t) => ({ ...t, [role]: r })))
      .catch((e) => setTests((t) => ({ ...t, [role]: { ok: false, latency_ms: 0, error: e instanceof Error ? e.message : "failed" } })));
  };

  if (q.isLoading || !q.data || !q.data.user_byok_enabled) return null;

  const byRole = new Map(q.data.providers.map((p) => [p.role, p]));
  const blankDraft = (role: string): PDraft => {
    const p = byRole.get(role);
    return { base_url: p?.base_url ?? "", model: p?.model ?? "", api_key: "", enabled: p?.enabled ?? true };
  };
  const draft = (role: string): PDraft => edits[role] ?? blankDraft(role);
  // Merge from the updater's `prev` + spread `...prev` so editing/saving one role
  // never disturbs another role's in-progress draft.
  const setField = (role: string, k: keyof PDraft, v: string | boolean) =>
    setEdits((prev) => ({ ...prev, [role]: { ...(prev[role] ?? blankDraft(role)), [k]: v } }));

  // Save/Clear ONE role: clear only that role's draft (not all), refresh, and toast.
  async function act(role: string, label: string, fn: () => Promise<unknown>) {
    setBusy(true);
    try {
      await fn();
      setEdits((prev) => { const n = { ...prev }; delete n[role]; return n; });
      refresh();
      toast(label, { variant: "success" });
      setSaved((s) => ({ ...s, [role]: true }));
      window.setTimeout(() => setSaved((s) => { const n = { ...s }; delete n[role]; return n; }), 2500);
    } catch (e) {
      toast(`Failed: ${e instanceof Error ? e.message : String(e)}`, { variant: "error" });
    } finally { setBusy(false); }
  }

  return (
    <section className="prof-section">
      <label className="form-label">My providers</label>
      <div className="ed-hint" style={{ marginBottom: 10 }}>
        Bring your own provider/key — your requests use it; everyone else falls back to the deployment or built-in default.
        Keys are write-only (stored encrypted, shown only as “•••• set”). Leave the key blank to keep the current one; Clear reverts the role.
      </div>
      {/* LLM is a list of the user's own named chat models (multi-LLM). */}
      <MyLlmCard />
      {/* Embed/rerank/verify are deployment-wide only (set in Admin → Providers), not per-user. LLM is handled by MyLlmCard above. */}
      {PROVIDER_ROLES.filter(([role]) => !["embed", "rerank", "verify", "llm"].includes(role)).map(([role, label]) => {
        const p = byRole.get(role);
        const d = draft(role);
        const dirty = role in edits;
        return (
          <div key={role} className="prof-card" style={{ marginBottom: 10, display: "block" }}>
            <div className="row" style={{ justifyContent: "space-between", marginBottom: 8 }}>
              <div className="prof-card-title">{label} <span className="mono" style={{ opacity: 0.5, fontSize: 11 }}>{role}</span></div>
              <span className="btn btn-line sm" style={{ pointerEvents: "none" }}>using: {SOURCE_LABEL[p?.source ?? "default"]}</span>
            </div>
            <div className="row" style={{ gap: 8, flexWrap: "wrap" }}>
              <input className="field" style={{ flex: "1 1 200px" }} placeholder="Base URL (blank = default)" value={d.base_url} onChange={(e) => setField(role, "base_url", e.target.value)} />
              <input className="field" style={{ flex: "1 1 160px" }} placeholder="Model (blank = default)" value={d.model} onChange={(e) => setField(role, "model", e.target.value)} />
              <input className="field" style={{ flex: "1 1 160px" }} type="password" placeholder={p?.api_key_set ? "•••• set (blank = keep)" : "API key"} value={d.api_key} onChange={(e) => setField(role, "api_key", e.target.value)} />
              <label className="row" style={{ gap: 6, fontSize: 13 }}><input type="checkbox" checked={d.enabled} onChange={(e) => setField(role, "enabled", e.target.checked)} /> Enabled</label>
              <button type="button" className="btn btn-gold sm" disabled={busy || !dirty} onClick={() => act(role, "Provider saved.", () => setMyProvider(role, { base_url: d.base_url || undefined, model: d.model || undefined, api_key: d.api_key || undefined, enabled: d.enabled }))}>
                <Icon.Save size={14} /> Save
              </button>
              <button type="button" className="btn btn-ghost sm" disabled={busy} onClick={() => act(role, "Provider cleared.", () => clearMyProvider(role))}>
                <Icon.Trash size={14} /> Clear
              </button>
              <button type="button" className="btn btn-line sm" onClick={() => runTest(role, d)}>Test</button>
              <span className="row" style={{ alignItems: "center", gap: 8 }}><ProviderTestStatus s={tests[role]} />{saved[role] && <span className="text-xs text-green-400">Saved ✓</span>}</span>
            </div>
          </div>
        );
      })}
    </section>
  );
}

// Keys that authenticate an external application AS this user. Deliberately a
// separate tab from Providers: those are the credentials Fosnie uses to reach a
// model provider, these are the credentials something else uses to reach Fosnie.
// Confusing the two would be an easy way to paste the wrong secret somewhere.
function ApiKeysSection() {
  const keys = useMyApiKeys();
  const [busy, setBusy] = useState(false);
  const [name, setName] = useState("");
  const [expiry, setExpiry] = useState("");
  const [fresh, setFresh] = useState<CreatedApiKey | null>(null);
  const refresh = () => keys.refetch();

  const live = (keys.data ?? []).filter((k) => !k.revoked_at);
  const revoked = (keys.data ?? []).filter((k) => k.revoked_at);

  const create = async () => {
    setBusy(true);
    try {
      const days = expiry.trim() ? Number(expiry.trim()) : undefined;
      if (days !== undefined && (!Number.isFinite(days) || days < 1)) {
        toast("Expiry must be a number of days, or blank for no expiry.", { variant: "error" });
        return;
      }
      const created = await createApiKey({ name: name.trim(), expires_in_days: days });
      setFresh(created);
      setName("");
      setExpiry("");
      refresh();
    } catch (e) {
      toast(`Failed: ${e instanceof Error ? e.message : String(e)}`, { variant: "error" });
    } finally {
      setBusy(false);
    }
  };

  const revoke = async (id: string, label: string) => {
    const ok = await confirmDialog({
      danger: true,
      title: "Revoke this key?",
      body: `Anything still using "${label}" will stop working immediately. This cannot be undone; mint a new key instead.`,
      confirmLabel: "Revoke key",
    });
    if (!ok) return;
    setBusy(true);
    try {
      await revokeApiKey(id);
      refresh();
      toast("Key revoked.", { variant: "success" });
    } catch (e) {
      toast(`Failed: ${e instanceof Error ? e.message : String(e)}`, { variant: "error" });
    } finally {
      setBusy(false);
    }
  };

  const when = (iso: string | null) => (iso ? new Date(iso).toLocaleDateString() : "—");

  return (
    <div>
      <div className="prof-card-title" style={{ marginBottom: 6 }}>Platform API keys</div>
      <div className="ed-hint" style={{ marginBottom: 10 }}>
        For connecting external applications to this instance. A key acts as you: it carries
        your permissions and reaches the same libraries and agents you do. Point any
        OpenAI-compatible client at this instance and use the key as its API key.
      </div>

      {fresh && (
        <div className="prof-card" style={{ marginBottom: 12, display: "block" }}>
          <div className="row" style={{ gap: 6, marginBottom: 6 }}>
            <Icon.Alert size={14} />
            <strong>Copy this key now. It is not shown again.</strong>
          </div>
          <div className="mono" style={{ wordBreak: "break-all", fontSize: 12, marginBottom: 8 }}>
            {fresh.token}
          </div>
          <div className="row" style={{ gap: 8 }}>
            <button
              type="button"
              className="btn btn-line sm"
              onClick={() => {
                navigator.clipboard.writeText(fresh.token).then(
                  () => toast("Key copied.", { variant: "success" }),
                  () => toast("Could not copy — select the key and copy it manually.", { variant: "error" }),
                );
              }}
            >
              <Icon.Copy size={14} /> Copy
            </button>
            <button type="button" className="btn btn-ghost sm" onClick={() => setFresh(null)}>
              Done
            </button>
          </div>
        </div>
      )}

      <div className="prof-card" style={{ marginBottom: 12, display: "block" }}>
        <div className="row" style={{ gap: 8, flexWrap: "wrap" }}>
          <input
            className="field"
            style={{ flex: "1 1 200px" }}
            placeholder="What is this key for?"
            value={name}
            onChange={(e) => setName(e.target.value)}
          />
          <input
            className="field"
            style={{ flex: "0 1 170px" }}
            placeholder="Expires in days (optional)"
            value={expiry}
            onChange={(e) => setExpiry(e.target.value)}
          />
          <button type="button" className="btn btn-gold sm" disabled={busy} onClick={create}>
            <Icon.Key size={14} /> Create key
          </button>
        </div>
      </div>

      {live.length === 0 && !keys.isLoading && (
        <div className="ed-hint">No keys yet.</div>
      )}
      {live.map((k) => (
        <div key={k.id} className="prof-card" style={{ marginBottom: 8, display: "flex", alignItems: "center", gap: 10 }}>
          <div style={{ minWidth: 0, flex: 1 }}>
            <div style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{k.name}</div>
            <div className="mono" style={{ opacity: 0.5, fontSize: 10 }}>
              {k.display_prefix}… · created {when(k.created_at)} · last used {when(k.last_used_at)}
              {k.expires_at ? ` · expires ${when(k.expires_at)}` : ""}
            </div>
          </div>
          <button type="button" className="btn btn-ghost sm" disabled={busy} onClick={() => revoke(k.id, k.name)}>
            <Icon.Trash size={14} />
          </button>
        </div>
      ))}

      {revoked.length > 0 && (
        <div style={{ marginTop: 14 }}>
          <div className="ed-hint" style={{ marginBottom: 6 }}>Revoked</div>
          {revoked.map((k) => (
            <div key={k.id} className="prof-card" style={{ marginBottom: 6, opacity: 0.55 }}>
              <div style={{ minWidth: 0, flex: 1 }}>
                <div>{k.name}</div>
                <div className="mono" style={{ opacity: 0.6, fontSize: 10 }}>
                  {k.display_prefix}… · revoked {when(k.revoked_at)}
                </div>
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

const PLATFORM_LABEL: Record<Device["platform"], string> = {
  windows: "Windows",
  macos: "macOS",
  linux: "Linux",
};

// Paired desktop machines. A sibling of the API-keys tab, deliberately apart: a
// device is minted by pairing (a code read into the app), not typed in here, and
// a device token reaches this instance's own surface rather than the
// OpenAI-compatible one a key is for.
function DevicesSection() {
  const devices = useMyDevices();
  const [busy, setBusy] = useState(false);
  const [code, setCode] = useState<{ code: string; expires_at: string } | null>(null);
  const [remaining, setRemaining] = useState<number>(0);
  const refresh = () => devices.refetch();

  const live = (devices.data ?? []).filter((d) => !d.revoked_at);
  const revoked = (devices.data ?? []).filter((d) => d.revoked_at);

  // Count the shown code down to its expiry, then clear it: a lapsed code is
  // useless and leaving it on screen only invites a failed pairing attempt.
  useEffect(() => {
    if (!code) return;
    const tick = () => {
      const secs = Math.max(0, Math.round((new Date(code.expires_at).getTime() - Date.now()) / 1000));
      setRemaining(secs);
      if (secs === 0) setCode(null);
    };
    tick();
    const h = setInterval(tick, 1000);
    return () => clearInterval(h);
  }, [code]);

  const pair = async () => {
    setBusy(true);
    try {
      setCode(await createPairingCode());
    } catch (e) {
      toast(`Failed: ${e instanceof Error ? e.message : String(e)}`, { variant: "error" });
    } finally {
      setBusy(false);
    }
  };

  const revoke = async (id: string, label: string) => {
    const ok = await confirmDialog({
      danger: true,
      title: "Sign this device out?",
      body: `"${label}" will be signed out immediately and will need to be paired again to reconnect.`,
      confirmLabel: "Sign out device",
    });
    if (!ok) return;
    setBusy(true);
    try {
      await revokeDevice(id);
      refresh();
      toast("Device signed out.", { variant: "success" });
    } catch (e) {
      toast(`Failed: ${e instanceof Error ? e.message : String(e)}`, { variant: "error" });
    } finally {
      setBusy(false);
    }
  };

  const when = (iso: string | null) => (iso ? new Date(iso).toLocaleDateString() : "—");
  // Group the code as two blocks of four for readability; the server ignores the
  // separator when the device redeems it.
  const grouped = (c: string) => (c.length === 8 ? `${c.slice(0, 4)}-${c.slice(4)}` : c);

  return (
    <div>
      <div className="prof-card-title" style={{ marginBottom: 6 }}>Connected devices</div>
      <div className="ed-hint" style={{ marginBottom: 10 }}>
        A paired desktop app signs in with its own token and acts as you. Generate a code
        below, then enter it in the app to pair it. You can sign any device out from here at
        any time, and it stops working at once.
      </div>

      {code && (
        <div className="prof-card" style={{ marginBottom: 12, display: "block" }}>
          <div className="row" style={{ gap: 6, marginBottom: 6 }}>
            <Icon.Desktop size={14} />
            <strong>Enter this code in the desktop app.</strong>
          </div>
          <div className="mono" style={{ fontSize: 26, letterSpacing: 3, marginBottom: 8 }}>
            {grouped(code.code)}
          </div>
          <div className="row" style={{ gap: 8, alignItems: "center" }}>
            <button
              type="button"
              className="btn btn-line sm"
              onClick={() => {
                navigator.clipboard.writeText(code.code).then(
                  () => toast("Code copied.", { variant: "success" }),
                  () => toast("Could not copy — read the code off the screen.", { variant: "error" }),
                );
              }}
            >
              <Icon.Copy size={14} /> Copy
            </button>
            <span className="ed-hint">Expires in {Math.floor(remaining / 60)}:{String(remaining % 60).padStart(2, "0")}</span>
            <button type="button" className="btn btn-ghost sm" style={{ marginLeft: "auto" }} onClick={() => setCode(null)}>
              Done
            </button>
          </div>
        </div>
      )}

      <div className="prof-card" style={{ marginBottom: 12, display: "block" }}>
        <button type="button" className="btn btn-gold sm" disabled={busy} onClick={pair}>
          <Icon.Desktop size={14} /> Pair a device
        </button>
      </div>

      {live.length === 0 && !devices.isLoading && (
        <div className="ed-hint">No devices paired yet.</div>
      )}
      {live.map((d) => (
        <div key={d.id} className="prof-card" style={{ marginBottom: 8, display: "flex", alignItems: "center", gap: 10 }}>
          <div style={{ minWidth: 0, flex: 1 }}>
            <div style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{d.name}</div>
            <div className="mono" style={{ opacity: 0.5, fontSize: 10 }}>
              {PLATFORM_LABEL[d.platform]} · paired {when(d.created_at)} · last seen{" "}
              {d.last_seen_at ? when(d.last_seen_at) : "never"}
            </div>
          </div>
          <button type="button" className="btn btn-ghost sm" disabled={busy} onClick={() => revoke(d.id, d.name)}>
            <Icon.Trash size={14} />
          </button>
        </div>
      ))}

      {revoked.length > 0 && (
        <div style={{ marginTop: 14 }}>
          <div className="ed-hint" style={{ marginBottom: 6 }}>Signed out</div>
          {revoked.map((d) => (
            <div key={d.id} className="prof-card" style={{ marginBottom: 6, opacity: 0.55 }}>
              <div style={{ minWidth: 0, flex: 1 }}>
                <div>{d.name}</div>
                <div className="mono" style={{ opacity: 0.6, fontSize: 10 }}>
                  {PLATFORM_LABEL[d.platform]} · signed out {when(d.revoked_at)}
                </div>
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

type ProfileTab = "account" | "security" | "providers" | "api-keys" | "devices" | "connections" | "appearance" | "danger";

export function Profile() {
  const nav = useNavigate();
  const { logout } = useAuth();
  const profile = useMyProfile();
  // Read once at the Profile level so the Providers TAB can be hidden entirely when
  // per-user BYOK is off (the section keeps its own guard too).
  const providers = useMyProviders();
  const byokEnabled = !!providers.data?.user_byok_enabled;
  // Connectors are an Enterprise capability; MCP one-click connections are a Core one.
  // The Connections tab appears when EITHER is on, and each block guards itself inside.
  const who = useWhoami();
  const connectorsEnabled = !!who.data?.capabilities?.enterprise_connectors;
  const mcpEnabled = !!who.data?.capabilities?.mcp;
  const connectionsTabEnabled = connectorsEnabled || mcpEnabled;
  // The key surface follows the programmatic API: with it switched off there is
  // nothing a key could be used for, so the tab disappears with it.
  const publicApiEnabled = !!who.data?.capabilities?.public_api;
  const isLocalAuth = authMode() === "local";
  const fileRef = useRef<HTMLInputElement>(null);
  const [name, setName] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [tab, setTab] = useState<ProfileTab>("account");

  const onDeleteAccount = async () => {
    const ok = await confirmDialog({
      danger: true,
      title: "Delete account?",
      body: "This archives your account and signs you out. Your content may be retained per your organisation's policy.",
      confirmLabel: "Delete account",
    });
    if (!ok) return;
    setBusy(true);
    setErr(null);
    try {
      await deleteAccount();
      logout();
    } catch (e) {
      setErr(e instanceof Error ? e.message : "Failed to delete account");
      setBusy(false);
    }
  };

  const look = useAppearance();
  const p = profile.data;
  // `name` is null until the user starts editing, then mirrors the field.
  const draft = name ?? p?.display_name ?? "";
  const dirty = p != null && draft.trim() !== (p.display_name ?? "") && draft.trim().length > 0;

  async function run(fn: () => Promise<void>) {
    setBusy(true);
    setErr(null);
    try {
      await fn();
    } catch (e) {
      setErr((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  function saveName() {
    if (!dirty) return;
    run(() => updateMyName(draft.trim()).then(() => setName(null)));
  }

  function onPickAvatar(e: React.ChangeEvent<HTMLInputElement>) {
    const file = e.target.files?.[0];
    e.target.value = ""; // allow re-picking the same file
    if (!file) return;
    if (!file.type.startsWith("image/")) {
      setErr("Please choose an image file (PNG, JPEG, WebP or GIF).");
      return;
    }
    if (file.size > MAX_AVATAR_BYTES) {
      setErr("That image is too large — the limit is 2 MB.");
      return;
    }
    run(() => uploadMyAvatar(file));
  }

  if (profile.isLoading || !p) {
    return (
      <div className="main-scroll">
        <div className="panel anim-on fade-in">
          <PanelHead title="Profile" />
          <div className="side-empty">Loading…</div>
        </div>
      </div>
    );
  }

  const created = new Date(p.created_epoch * 1000).toLocaleDateString(undefined, {
    day: "numeric",
    month: "long",
    year: "numeric",
  });

  const tabs: [ProfileTab, string][] = [
    ["account", "Account"],
    // Security (2FA + password) is a local-auth surface; Keycloak owns its own.
    ...(isLocalAuth ? ([["security", "Security"]] as [ProfileTab, string][]) : []),
    ...(byokEnabled ? ([["providers", "Providers"]] as [ProfileTab, string][]) : []),
    ...(publicApiEnabled ? ([["api-keys", "API keys"]] as [ProfileTab, string][]) : []),
    // Pairing a desktop app is a core capability, so the tab is always present.
    ["devices", "Connected devices"],
    ...(connectionsTabEnabled ? ([["connections", "Connections"]] as [ProfileTab, string][]) : []),
    ["appearance", "Appearance"],
    ["danger", "Danger zone"],
  ];

  return (
    <div className="main-scroll">
      <div className="panel anim-on fade-in">
        <PanelHead title="Profile" sub="How your team sees you, and your account details." />

        {err && <div className="form-err" style={{ marginBottom: 14 }}>{err}</div>}

        <div className="legal-tabs" style={{ marginBottom: 22 }}>
          <div className="legal-tabs-l" style={{ overflowX: "auto" }}>
            {tabs.map(([key, label]) => (
              <button key={key} className={"legal-tab" + (tab === key ? " on" : "")} onClick={() => setTab(key)}>
                {label}
              </button>
            ))}
          </div>
        </div>

        {tab === "account" && (<>
          {/* Avatar */}
          <section className="prof-card">
            <Avatar id={p.user_id} name={p.display_name} email={p.email} avatarUpdatedAt={p.avatar_updated_at} className="avatar lg" />
            <div className="prof-card-body">
              <div className="prof-card-title">Profile picture</div>
              <div className="ed-hint">Shown beside your name in chats and the sidebar. PNG, JPEG, WebP or GIF, up to 2 MB.</div>
              <div className="prof-actions">
                <button className="btn btn-line sm" disabled={busy} onClick={() => fileRef.current?.click()}>
                  <Icon.User size={14} /> {p.avatar_updated_at ? "Replace" : "Upload"}
                </button>
                {p.avatar_updated_at && (
                  <button className="btn btn-ghost sm" disabled={busy} onClick={() => run(removeMyAvatar)}>
                    <Icon.Trash size={14} /> Remove
                  </button>
                )}
                <input ref={fileRef} type="file" accept="image/*" hidden onChange={onPickAvatar} />
              </div>
            </div>
          </section>

          {/* Display name */}
          <section className="prof-section">
            <label className="form-label">Display name</label>
            <div className="prof-name-row">
              <input
                className="field"
                value={draft}
                maxLength={120}
                onChange={(e) => setName(e.target.value)}
                onKeyDown={(e) => { if (e.key === "Enter") saveName(); }}
                placeholder="e.g. Alice | COO | MSG ONLY 2–5"
              />
              <button className="btn btn-gold sm" disabled={!dirty || busy} onClick={saveName}>
                <Icon.Save size={14} /> Save
              </button>
            </div>
            <div className="ed-hint">This is how teammates see you across chats and mentions. It does not change your sign-in.</div>
          </section>

          {/* Account */}
          <section className="prof-section">
            <label className="form-label">Account</label>
            <div className="prof-facts">
              <div className="prof-fact"><span className="prof-fact-k"><Icon.At size={14} /> Email</span><span className="prof-fact-v mono">{p.email ?? "—"}</span></div>
              <div className="prof-fact"><span className="prof-fact-k"><Icon.Shield size={14} /> Role</span><span className="prof-fact-v">{ROLE_LABEL[p.role] ?? p.role}</span></div>
              <div className="prof-fact"><span className="prof-fact-k"><Icon.Calendar size={14} /> Member since</span><span className="prof-fact-v">{created}</span></div>
            </div>
            <div className="prof-actions" style={{ marginTop: 14 }}>
              <button className="btn btn-line sm" onClick={() => nav("/dm")}>
                <Icon.Chat size={14} /> Open direct messages
              </button>
              {/* Keycloak account console (password/MFA) — only under Keycloak auth. */}
              {!isLocalAuth && p.account_url && (
                <a className="btn btn-line sm" href={p.account_url} target="_blank" rel="noopener noreferrer">
                  <Icon.Key size={14} /> Manage password &amp; MFA <Icon.External size={13} />
                </a>
              )}
            </div>
            {/* Under Keycloak the org's IdP owns email/role/password. Under local auth
                the user manages password + two-step in the Security tab. */}
            {isLocalAuth
              ? <div className="ed-hint" style={{ marginTop: 8 }}>Manage your password and two-step verification in the <b>Security</b> tab.</div>
              : <div className="ed-hint" style={{ marginTop: 8 }}>Email, role and password are managed by your organisation's identity provider.</div>}
          </section>
        </>)}

        {/* Security — two-step verification + password (local auth only). */}
        {tab === "security" && <SecuritySection />}

        {/* My Providers — per-user BYOK, only when the deployment enables it. */}
        {tab === "providers" && <MyProvidersSection />}

        {/* Platform API keys — credentials for external applications. */}
        {tab === "api-keys" && <ApiKeysSection />}

        {/* Connected devices — paired desktop apps and their tokens. */}
        {tab === "devices" && <DevicesSection />}

        {/* Connections — the caller's own DMS/mailbox OAuth connections (Enterprise). */}
        {tab === "connections" && <ConnectionsSection />}

        {/* Appearance — per-user skin controls. Stored locally on this device. */}
        {tab === "appearance" && (
          <section className="prof-section">
            <label className="form-label">Appearance</label>

            <div style={{ marginBottom: 14 }}>
              <div className="ed-hint" style={{ marginBottom: 6 }}>
                Theme — the overall palette. Fosnie is the near-black and purple house
                skin; Classic is the original navy and gold look.
              </div>
              <Seg
                value={look.theme}
                onChange={(theme) => look.set({ theme })}
                options={[
                  { value: "fosnie", label: "Fosnie (purple)" },
                  { value: "gold", label: "Gold" },
                  { value: "classic", label: "Classic (navy & gold)" },
                ]}
              />
            </div>

            <div style={{ marginBottom: 14 }}>
              <div className="ed-hint" style={{ marginBottom: 6 }}>
                Surface style — how floating panels (composer, menus, dialogs) render.
                Tinted is the default; reduced transparency and high contrast drop the
                glass for clarity or weaker hardware.
              </div>
              <Seg
                value={look.glass}
                onChange={(glass) => look.set({ glass })}
                options={[
                  { value: "tinted", label: "Glass (tinted)" },
                  { value: "reduced", label: "Reduced transparency" },
                  { value: "contrast", label: "High contrast" },
                ]}
              />
            </div>

            <div style={{ marginBottom: 14 }}>
              <div className="ed-hint" style={{ marginBottom: 6 }}>Density — spacing of lists and panels.</div>
              <Seg
                value={look.density}
                onChange={(density) => look.set({ density })}
                options={[
                  { value: "comfortable", label: "Comfortable" },
                  { value: "compact", label: "Compact" },
                ]}
              />
            </div>

            <div>
              <div className="ed-hint" style={{ marginBottom: 6 }}>Motion — entrance and transition animations.</div>
              <Seg
                value={look.motion}
                onChange={(motion) => look.set({ motion })}
                options={[
                  { value: "full", label: "Full" },
                  { value: "reduced", label: "Reduced" },
                ]}
              />
            </div>
          </section>
        )}

        {/* Sign out + delete account */}
        {tab === "danger" && (
          <section className="prof-section" style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
            <button className="btn btn-line sm" onClick={logout}>
              <Icon.Logout size={14} /> Sign out
            </button>
            <button className="btn btn-danger sm" disabled={busy} onClick={onDeleteAccount}>
              <Icon.Trash size={14} /> Delete account
            </button>
          </section>
        )}
      </div>
    </div>
  );
}

// The connectable source kinds (labels match the backend `ConnectorKind`).
const CONNECTOR_KINDS: [string, string][] = [
  ["outlook", "Outlook"],
  ["gmail", "Gmail"],
  ["imanage", "iManage"],
  ["netdocuments", "NetDocuments"],
];

/** Profile → Connections: connect/disconnect the caller's own source accounts. */
function ConnectionsSection() {
  const qc = useQueryClient();
  const who = useWhoami();
  const connectorsEnabled = !!who.data?.capabilities?.enterprise_connectors;
  const mcpEnabled = !!who.data?.capabilities?.mcp;
  const conns = useConnectorConnections(connectorsEnabled);
  const [busyKind, setBusyKind] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);

  // Surface the callback's result once on mount (Enterprise connectors OR MCP OAuth).
  const [banner, setBanner] = useState<{ ok: boolean; text: string } | null>(() => {
    const q = new URLSearchParams(window.location.search);
    if (q.get("connected")) return { ok: true, text: `Connected ${q.get("connected")}.` };
    if (q.get("connector_error")) return { ok: false, text: q.get("connector_error") || "Connection failed." };
    if (q.get("mcp_connected")) return { ok: true, text: `Connected ${q.get("mcp_connected")}.` };
    if (q.get("mcp_connect_error")) return { ok: false, text: q.get("mcp_connect_error") || "Connection failed." };
    return null;
  });

  const byKind = new Map<string, ConnectorConnection[]>();
  for (const c of conns.data?.connections ?? []) {
    byKind.set(c.kind, [...(byKind.get(c.kind) ?? []), c]);
  }

  async function onConnect(kind: string) {
    setErr(null);
    setBusyKind(kind);
    try {
      const { authorize_url } = await connectConnector(kind);
      window.location.href = authorize_url; // hand off to the provider
    } catch (e) {
      setErr(e instanceof Error ? e.message : "Could not start the connection.");
      setBusyKind(null);
    }
  }

  async function onDisconnect(id: string) {
    const ok = await confirmDialog({
      title: "Disconnect?",
      body: "This removes the stored tokens and stops any sync using this connection.",
      confirmLabel: "Disconnect",
      danger: true,
    });
    if (!ok) return;
    try {
      await disconnectConnector(id);
      qc.invalidateQueries({ queryKey: ["connector-connections"] });
      toast("Disconnected.");
    } catch (e) {
      setErr(e instanceof Error ? e.message : "Disconnect failed.");
    }
  }

  return (
    <section className="prof-section">
      <label className="form-label">Connections</label>
      <div className="ed-hint" style={{ marginBottom: 12 }}>
        Connect your own document management and mailbox accounts. You see exactly
        what you can see in the source; nothing is shared until you import it.
      </div>
      {banner && (
        <div className={banner.ok ? "form-ok" : "form-err"} style={{ marginBottom: 12 }}>
          {banner.text}{" "}
          <button className="btn-link" onClick={() => setBanner(null)}>dismiss</button>
        </div>
      )}
      {err && <div className="form-err" style={{ marginBottom: 12 }}>{err}</div>}

      {mcpEnabled && <McpConnectionsBlock />}

      {connectorsEnabled && CONNECTOR_KINDS.map(([kind, label]) => {
        const list = byKind.get(kind) ?? [];
        return (
          <div key={kind} className="prof-card" style={{ marginBottom: 10 }}>
            <div className="prof-card-body">
              <div className="prof-card-title">{label}</div>
              {list.length === 0 ? (
                <div className="ed-hint">Not connected.</div>
              ) : (
                list.map((c) => (
                  <div key={c.id} className="row" style={{ alignItems: "center", gap: 8, marginTop: 4 }}>
                    <span>{c.display_name}</span>
                    {c.status === "reauth_required" && (
                      <span className="badge badge-warn">Re-authentication needed</span>
                    )}
                    {c.status === "active" && <span className="text-xs text-green-400">Active</span>}
                    <button className="btn btn-line sm" onClick={() => onDisconnect(c.id)}>
                      Disconnect
                    </button>
                    {c.status === "reauth_required" && (
                      <button className="btn btn-line sm" onClick={() => onConnect(kind)}>
                        Reconnect
                      </button>
                    )}
                  </div>
                ))
              )}
            </div>
            <button className="btn btn-primary sm" disabled={busyKind === kind} onClick={() => onConnect(kind)}>
              {busyKind === kind ? "Starting…" : list.length ? "Connect another" : "Connect"}
            </button>
          </div>
        );
      })}
    </section>
  );
}

/** The MCP one-click connections (OAuth 2.1) block inside Connections. */
function McpConnectionsBlock() {
  const qc = useQueryClient();
  const conns = useMyMcpConnections(true);
  const [busy, setBusy] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);

  async function onConnect(serverId: string) {
    setErr(null);
    setBusy(serverId);
    try {
      const { authorize_url } = await connectMcpServer(serverId);
      window.location.href = authorize_url; // full-page hand-off to the provider
    } catch (e) {
      setErr(e instanceof Error ? e.message : "Could not start the connection.");
      setBusy(null);
    }
  }
  async function onDisconnect(serverId: string) {
    const ok = await confirmDialog({
      title: "Disconnect?",
      body: "This removes your stored tokens for this server.",
      confirmLabel: "Disconnect",
      danger: true,
    });
    if (!ok) return;
    try {
      await disconnectMcpServer(serverId);
      qc.invalidateQueries({ queryKey: ["my-mcp-connections"] });
      toast("Disconnected.");
    } catch (e) {
      setErr(e instanceof Error ? e.message : "Disconnect failed.");
    }
  }

  const list = conns.data ?? [];
  if (list.length === 0) return null;

  return (
    <>
      <div className="form-label" style={{ marginTop: 4 }}>MCP servers</div>
      {err && <div className="form-err" style={{ marginBottom: 12 }}>{err}</div>}
      {list.map((s) => (
        <div key={s.server_id} className="prof-card" style={{ marginBottom: 10 }}>
          <div className="prof-card-body">
            <div className="prof-card-title">{s.name}</div>
            {s.status === "connected" ? (
              <div className="row" style={{ alignItems: "center", gap: 8, marginTop: 4 }}>
                <span className="text-xs text-green-400">Connected</span>
                {s.subject_label && <span className="ed-hint">{s.subject_label}</span>}
              </div>
            ) : s.status === "reauth_required" ? (
              <div className="ed-hint"><span className="badge badge-warn">Re-authentication needed</span></div>
            ) : (
              <div className="ed-hint">Not connected.</div>
            )}
          </div>
          {s.status === "connected" ? (
            <button className="btn btn-line sm" disabled={busy === s.server_id} onClick={() => onDisconnect(s.server_id)}>
              Disconnect
            </button>
          ) : (
            <button className="btn btn-primary sm" disabled={busy === s.server_id} onClick={() => onConnect(s.server_id)}>
              {busy === s.server_id ? "Starting…" : s.status === "reauth_required" ? "Reconnect" : "Connect"}
            </button>
          )}
        </div>
      ))}
    </>
  );
}
