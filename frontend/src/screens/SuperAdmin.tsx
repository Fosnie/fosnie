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

// Super-admin panel — lives entirely inside an ephemeral break-glass session.
// No Keycloak login: you mint a grant on the
// terminal (`fosnie-backend breakglass issue`) and paste it here. The grant is held
// in memory only (never localStorage) and sent as `X-Break-Glass` on every call;
// when its TTL runs out the panel re-locks. Reachable without Keycloak by design.

import { apiUrl } from "@/api/instance";
import { promptDialog } from "@/components/dialogs";
import { BreakGlass } from "@/components/custom-icons";
import { Dropdown } from "@/components/Dropdown";
import { useEffect, useMemo, useRef, useState } from "react";

const HDR = "X-Break-Glass";

async function bg(grant: string, path: string, init?: RequestInit): Promise<Response> {
  const headers: Record<string, string> = { [HDR]: grant, ...(init?.headers as Record<string, string>) };
  if (init?.body) headers["Content-Type"] = "application/json";
  const res = await fetch(apiUrl(path), { ...init, headers });
  if (!res.ok) {
    const body = await res.text().catch(() => "");
    throw new Error(`${res.status} ${res.statusText}${body ? ": " + body.slice(0, 200) : ""}`);
  }
  return res;
}

interface Session {
  grant: { grant_id: string; label: string | null; reason: string | null; ttl_secs: number } | null;
}

export function SuperAdmin() {
  const [grant, setGrant] = useState<string | null>(null);
  const [session, setSession] = useState<Session["grant"]>(null);

  if (!grant || !session) {
    return <Gate onUnlock={(g, s) => { setGrant(g); setSession(s); }} />;
  }
  return <Panel grant={grant} session={session} onLock={() => { setGrant(null); setSession(null); }} />;
}

function Gate({ onUnlock }: { onUnlock: (grant: string, s: Session["grant"]) => void }) {
  const [val, setVal] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  async function unlock() {
    const g = val.trim();
    if (!g || busy) return;
    setBusy(true); setErr(null);
    try {
      const s: Session = await (await bg(g, "/api/admin/super/session")).json();
      onUnlock(g, s.grant);
    } catch (e) {
      setErr((e as Error).message);
      setBusy(false);
    }
  }

  return (
    <div className="sa-shell" style={{ display: "grid", placeItems: "center" }}>
      <div className="signin-card" style={{ width: 460 }}>
        <div className="signin-mark"><BreakGlass size={26} /></div>
        <div className="signin-sso" style={{ marginTop: 14 }}>Break-glass · super-admin</div>
        <h1 className="signin-title" style={{ fontSize: 30 }}>Restricted</h1>
        <p className="signin-sub">Do not enter.</p>
        <input
          className="field mono"
          style={{ marginTop: 20, textAlign: "center" }}
          placeholder="grant id"
          value={val}
          onChange={(e) => setVal(e.target.value)}
          onKeyDown={(e) => { if (e.key === "Enter") void unlock(); }}
          autoFocus
        />
        {err && <div className="ed-hint" style={{ color: "var(--red)", marginTop: 8 }}>{err}</div>}
        <button className="btn btn-gold signin-btn" onClick={unlock} disabled={busy || !val.trim()}>{busy ? "Validating…" : "Unlock"}</button>
        <div className="signin-foot"><a href="/" className="underline">← back to app</a></div>
      </div>
    </div>
  );
}

const SECTIONS = [
  { id: "settings", label: "Settings", ready: true },
  { id: "integrations", label: "Integrations", ready: true },
  { id: "chats", label: "All chats", ready: true },
  { id: "accounts", label: "Accounts", ready: true },
] as const;

interface UserRow { id: string; email: string; display_name: string; role: string; deactivated: boolean; }
interface ChatRow { id: string; title: string; created_at: string; }
interface MsgRow { role: string; content: string; created_at: string; }

function Panel({ grant, session, onLock }: { grant: string; session: NonNullable<Session["grant"]>; onLock: () => void }) {
  const [section, setSection] = useState<string>("settings");
  // TTL counts down client-side from the value fetched at unlock (no polling —
  // each call would write an audited "break-glass use").
  const expiry = useRef(Date.now() + session.ttl_secs * 1000);
  const [left, setLeft] = useState(session.ttl_secs);
  useEffect(() => {
    const t = setInterval(() => {
      const s = Math.max(0, Math.round((expiry.current - Date.now()) / 1000));
      setLeft(s);
      if (s <= 0) onLock();
    }, 1000);
    return () => clearInterval(t);
  }, [onLock]);

  const mm = String(Math.floor(left / 60)).padStart(2, "0");
  const ss = String(left % 60).padStart(2, "0");

  return (
    <div className="sa-shell">
      <header className="sa-head">
        <div className="sa-head-l">
          <span className="sa-badge mono">SUPER-ADMIN · BREAK-GLASS</span>
          <span className="sa-meta mono">{session.label ?? "session"}{session.reason ? ` · ${session.reason}` : ""}</span>
        </div>
        <div className="sa-head-r">
          <span className="sa-ttl mono" style={left < 60 ? { color: "var(--red)" } : undefined}>expires in {mm}:{ss}</span>
          <button className="btn btn-line sm" onClick={onLock}>Lock</button>
        </div>
      </header>
      <div className="sa-body">
        <nav className="sa-nav">
          {SECTIONS.map((s) => (
            <button
              key={s.id}
              className={"sa-nav-item" + (section === s.id ? " on" : "")}
              disabled={!s.ready}
              onClick={() => s.ready && setSection(s.id)}
            >
              {s.label}{!s.ready && <span className="sa-soon mono">soon</span>}
            </button>
          ))}
        </nav>
        <main className="sa-main">
          {section === "settings" && <Settings grant={grant} />}
          {section === "integrations" && <Integrations grant={grant} />}
          {section === "chats" && <Chats grant={grant} />}
          {section === "accounts" && <Accounts grant={grant} />}
        </main>
      </div>
    </div>
  );
}

interface Knob {
  key: string;
  label: string;
  desc: string;
  value_type: string;
  value: string;
  is_default: boolean;
  min: number | null;
  max: number | null;
  /** Enum choices → a <select> input; absent for numeric/free-text knobs. */
  options?: string[];
}

function Settings({ grant }: { grant: string }) {
  const [knobs, setKnobs] = useState<Knob[] | null>(null);
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const [saving, setSaving] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);

  async function load() {
    try {
      const data: Knob[] = await (await bg(grant, "/api/admin/super/config")).json();
      setKnobs(data);
      setDrafts(Object.fromEntries(data.map((k) => [k.key, k.value])));
    } catch (e) { setErr((e as Error).message); }
  }
  useEffect(() => { void load(); /* eslint-disable-next-line */ }, []);

  async function save(k: Knob, value: string) {
    setSaving(k.key); setErr(null);
    try {
      await bg(grant, `/api/admin/super/config/${k.key}`, { method: "PUT", body: JSON.stringify({ value }) });
      await load();
    } catch (e) { setErr((e as Error).message); }
    finally { setSaving(null); }
  }

  // revert a knob to its default (removes the override row).
  async function reset(k: Knob) {
    setSaving(k.key); setErr(null);
    try {
      await bg(grant, `/api/admin/super/config/${k.key}`, { method: "DELETE" });
      await load();
    } catch (e) { setErr((e as Error).message); }
    finally { setSaving(null); }
  }

  const groups = useMemo(() => {
    const m = new Map<string, Knob[]>();
    (knobs ?? []).forEach((k) => {
      const g = k.key.split(".")[0];
      (m.get(g) ?? m.set(g, []).get(g)!).push(k);
    });
    return [...m.entries()];
  }, [knobs]);

  const GROUP_LABEL: Record<string, string> = { rag: "Retrieval (RAG)", chat: "Chat", ingest: "Ingestion", groundedness: "Groundedness", workflows: "Workflows", voice: "Voice", research: "Deep Research" };

  if (!knobs) return <div className="ed-hint mono">{err ? <span style={{ color: "var(--red)" }}>{err}</span> : "Loading…"}</div>;

  return (
    <div>
      <h2 className="sa-title">Tuning</h2>
      <p className="ed-hint" style={{ marginBottom: 18 }}>Live, audited knobs fed to the ML service per request. Changes apply immediately (ingestion knobs apply to new documents).</p>
      {err && <div className="ed-hint" style={{ color: "var(--red)", marginBottom: 12 }}>{err}</div>}
      {groups.map(([g, ks]) => (
        <section key={g} className="sa-card">
          <h3 className="sa-card-h">{GROUP_LABEL[g] ?? g}</h3>
          {ks.map((k) => (
            <div key={k.key} className="sa-knob">
              <div className="sa-knob-l">
                <div className="sa-knob-label">{k.label} {k.is_default && <span className="sa-default mono">default</span>}</div>
                <div className="field-help">{k.desc}</div>
              </div>
              <div className="sa-knob-r">
                {k.value_type === "bool" ? (
                  <button
                    className={"sa-toggle" + (drafts[k.key] === "true" ? " on" : "")}
                    disabled={saving === k.key}
                    onClick={() => save(k, drafts[k.key] === "true" ? "false" : "true")}
                  >
                    {drafts[k.key] === "true" ? "On" : "Off"}
                  </button>
                ) : k.options ? (
                  <Dropdown
                    value={drafts[k.key] ?? k.value}
                    disabled={saving === k.key}
                    onChange={(v) => save(k, v)}
                    ariaLabel={k.key}
                    options={k.options.map((o) => ({ value: o, label: o }))}
                  />
                ) : (
                  <>
                    <input
                      className="field sm"
                      type="number"
                      style={{ width: 96 }}
                      min={k.min ?? undefined}
                      max={k.max ?? undefined}
                      value={drafts[k.key] ?? ""}
                      onChange={(e) => setDrafts((d) => ({ ...d, [k.key]: e.target.value }))}
                    />
                    <button
                      className="btn btn-gold sm"
                      disabled={saving === k.key || drafts[k.key] === k.value || drafts[k.key] === ""}
                      onClick={() => save(k, drafts[k.key])}
                    >
                      {saving === k.key ? "…" : "Save"}
                    </button>
                  </>
                )}
                {!k.is_default && (
                  <button
                    className="btn btn-line sm"
                    disabled={saving === k.key}
                    title="Reset to default"
                    onClick={() => reset(k)}
                  >
                    Reset
                  </button>
                )}
              </div>
            </div>
          ))}
        </section>
      ))}
    </div>
  );
}

// --- Integrations (connector activation) -------------------------------------

interface Connector { kind: string; display_name: string; category: string; requires_egress: boolean; enabled: boolean; }

function Integrations({ grant }: { grant: string }) {
  const [conns, setConns] = useState<Connector[] | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);

  async function load() {
    try { setConns(await (await bg(grant, "/api/admin/super/integrations")).json()); }
    catch (e) { setErr((e as Error).message); }
  }
  useEffect(() => { void load(); /* eslint-disable-next-line */ }, []);

  async function toggle(c: Connector) {
    if (busy) return;
    setBusy(c.kind); setErr(null);
    try {
      await bg(grant, `/api/admin/integrations/${c.kind}`, { method: "PUT", body: JSON.stringify({ enabled: !c.enabled }) });
      await load();
    } catch (e) { setErr((e as Error).message); }
    finally { setBusy(null); }
  }

  return (
    <div>
      <h2 className="sa-title">Integrations</h2>
      <p className="ed-hint" style={{ marginBottom: 18 }}>External connectors ship dormant (zero-egress). Enabling one lifts the egress gate for that connector only. Every change is audited.</p>
      {err && <div className="ed-hint" style={{ color: "var(--red)", marginBottom: 12 }}>{err}</div>}
      {!conns ? <div className="ed-hint mono">Loading…</div> : (
        <section className="sa-card">
          {conns.map((c) => (
            <div key={c.kind} className="sa-knob">
              <div className="sa-knob-l">
                <div className="sa-knob-label">{c.display_name} <span className="sa-default mono">{c.category}</span>{c.requires_egress ? <span className="sa-default mono">egress</span> : null}</div>
                <div className="field-help mono">{c.kind}</div>
              </div>
              <div className="sa-knob-r">
                <button
                  className={"sa-toggle" + (c.enabled ? " on" : "")}
                  disabled={busy === c.kind}
                  onClick={() => toggle(c)}
                >
                  {busy === c.kind ? "…" : c.enabled ? "On" : "Off"}
                </button>
              </div>
            </div>
          ))}
        </section>
      )}
    </div>
  );
}

// --- All chats (cross-user viewer) -------------------------------------------

function Chats({ grant }: { grant: string }) {
  const [users, setUsers] = useState<UserRow[] | null>(null);
  const [filter, setFilter] = useState("");
  const [uid, setUid] = useState<string | null>(null);
  const [chats, setChats] = useState<ChatRow[] | null>(null);
  const [cid, setCid] = useState<string | null>(null);
  const [msgs, setMsgs] = useState<MsgRow[] | null>(null);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    bg(grant, "/api/admin/super/users").then((r) => r.json()).then(setUsers).catch((e) => setErr((e as Error).message));
    /* eslint-disable-next-line */
  }, []);

  async function pickUser(id: string) {
    setUid(id); setChats(null); setCid(null); setMsgs(null); setErr(null);
    try { setChats(await (await bg(grant, `/api/admin/super/users/${id}/chats`)).json()); }
    catch (e) { setErr((e as Error).message); }
  }
  async function pickChat(id: string) {
    setCid(id); setMsgs(null); setErr(null);
    try { setMsgs(await (await bg(grant, `/api/admin/super/chats/${id}/messages`)).json()); }
    catch (e) { setErr((e as Error).message); }
  }

  const shown = (users ?? []).filter((u) => (u.email + " " + u.display_name).toLowerCase().includes(filter.toLowerCase()));

  return (
    <div>
      <h2 className="sa-title">All chats</h2>
      <p className="ed-hint" style={{ marginBottom: 14 }}>Read any user's chats. Every view is risk-audited.</p>
      {err && <div className="ed-hint" style={{ color: "var(--red)", marginBottom: 8 }}>{err}</div>}
      <div className="sa-chatgrid">
        <div className="sa-col">
          <input className="field sm" placeholder="filter users…" value={filter} onChange={(e) => setFilter(e.target.value)} />
          <div className="sa-list">
            {shown.map((u) => (
              <button key={u.id} className={"sa-list-item" + (uid === u.id ? " on" : "")} onClick={() => pickUser(u.id)}>
                <div className="sa-li-main">{u.display_name || u.email}{u.deactivated && <span className="sa-default mono">off</span>}</div>
                <div className="sa-li-sub mono">{u.email}</div>
              </button>
            ))}
          </div>
        </div>
        <div className="sa-col">
          {chats === null ? <div className="ed-hint mono">{uid ? "Loading…" : "Pick a user"}</div>
            : chats.length === 0 ? <div className="ed-hint mono">No chats.</div>
            : <div className="sa-list">{chats.map((c) => (
                <button key={c.id} className={"sa-list-item" + (cid === c.id ? " on" : "")} onClick={() => pickChat(c.id)}>
                  <div className="sa-li-main">{c.title}</div>
                  <div className="sa-li-sub mono">{c.created_at.slice(0, 16)}</div>
                </button>
              ))}</div>}
        </div>
        <div className="sa-col sa-msgs">
          {msgs === null ? <div className="ed-hint mono">{cid ? "Loading…" : "Pick a chat"}</div>
            : msgs.length === 0 ? <div className="ed-hint mono">No messages.</div>
            : msgs.map((m, i) => (
                <div key={i} className="sa-msg">
                  <span className={"sa-msg-role mono " + m.role}>{m.role}</span>
                  <div className="sa-msg-body">{m.content}</div>
                </div>
              ))}
        </div>
      </div>
    </div>
  );
}

// --- Accounts (deactivate + GDPR erasure) ------------------------------------

function Accounts({ grant }: { grant: string }) {
  const [users, setUsers] = useState<UserRow[] | null>(null);
  const [filter, setFilter] = useState("");
  const [erasing, setErasing] = useState<UserRow | null>(null);
  const [confirmText, setConfirmText] = useState("");
  const [erReason, setErReason] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);

  async function load() {
    try { setUsers(await (await bg(grant, "/api/admin/super/users")).json()); }
    catch (e) { setErr((e as Error).message); }
  }
  useEffect(() => { void load(); /* eslint-disable-next-line */ }, []);

  async function deactivate(u: UserRow) {
    if (busy) return;
    const reason = (await promptDialog({ title: `Deactivate ${u.email}?`, label: "Reason (audited)", placeholder: "Reason for deactivation" }))?.trim();
    if (!reason) return;
    setBusy(true); setErr(null);
    try { await bg(grant, `/api/admin/super/users/${u.id}/deactivate?reason=${encodeURIComponent(reason)}`, { method: "POST" }); await load(); }
    catch (e) { setErr((e as Error).message); }
    finally { setBusy(false); }
  }
  async function erase() {
    if (!erasing || busy || !erReason.trim()) return;
    setBusy(true); setErr(null);
    try {
      const r = await (await bg(grant, `/api/admin/super/users/${erasing.id}/data?reason=${encodeURIComponent(erReason.trim())}`, { method: "DELETE" })).json();
      setNote(`Erased ${erasing.email}: ${JSON.stringify(r.erased ?? {})}`);
      setErasing(null); setConfirmText(""); setErReason("");
      await load();
    } catch (e) { setErr((e as Error).message); }
    finally { setBusy(false); }
  }

  const shown = (users ?? []).filter((u) => (u.email + " " + u.display_name).toLowerCase().includes(filter.toLowerCase()));

  return (
    <div>
      <h2 className="sa-title">Accounts</h2>
      <p className="ed-hint" style={{ marginBottom: 14 }}>Deactivate an account, or erase all data tied to it (GDPR). Erasure is irreversible and refused while data is under a legal hold.</p>
      {err && <div className="ed-hint" style={{ color: "var(--red)", marginBottom: 8 }}>{err}</div>}
      {note && <div className="ed-hint" style={{ color: "var(--green)", marginBottom: 8 }}>{note}</div>}
      <input className="field sm" style={{ marginBottom: 12, maxWidth: 320 }} placeholder="filter…" value={filter} onChange={(e) => setFilter(e.target.value)} />
      {!users ? <div className="ed-hint mono">Loading…</div> : (
        <div className="sa-card" style={{ padding: 0 }}>
          {shown.slice(0, 200).map((u) => (
            <div key={u.id} className="sa-acct">
              <div className="sa-acct-l">
                <div className="sa-li-main">{u.display_name || "—"}{u.deactivated && <span className="sa-default mono">deactivated</span>}</div>
                <div className="sa-li-sub mono">{u.email} · {u.role}</div>
              </div>
              <div className="sa-acct-r">
                <button className="btn btn-line sm" disabled={busy || u.deactivated} onClick={() => deactivate(u)}>Deactivate</button>
                <button className="sa-erase" disabled={busy} onClick={() => { setErasing(u); setConfirmText(""); setErReason(""); setNote(null); setErr(null); }}>Erase data</button>
              </div>
            </div>
          ))}
        </div>
      )}

      {erasing && (
        <div className="fixed inset-0 z-50 flex items-center justify-center" style={{ background: "rgba(0,0,0,0.6)" }} onClick={() => setErasing(null)}>
          <div className="sa-card" style={{ width: 460, margin: 0 }} onClick={(e) => e.stopPropagation()}>
            <h3 className="sa-card-h" style={{ color: "var(--red)" }}>Erase all data</h3>
            <p className="ed-hint" style={{ marginBottom: 12 }}>This permanently purges every chat, message, prompt, agent, memory, automation, file and grant for <b>{erasing.email}</b>, and anonymises the account. The audit trail is kept. This cannot be undone.</p>
            <label className="form-label">Reason <span className="opt">(audited)</span></label>
            <input className="field sm" value={erReason} onChange={(e) => setErReason(e.target.value)} placeholder="e.g. GDPR erasure request #1234" autoFocus />
            <label className="form-label" style={{ marginTop: 10 }}>Type the email to confirm</label>
            <input className="field sm" value={confirmText} onChange={(e) => setConfirmText(e.target.value)} placeholder={erasing.email} />
            {err && <div className="ed-hint" style={{ color: "var(--red)", marginTop: 10 }}>{err}</div>}
            <div style={{ display: "flex", gap: 8, marginTop: 14 }}>
              <button className="sa-erase" disabled={busy || confirmText.trim() !== erasing.email || !erReason.trim()} onClick={erase}>{busy ? "Erasing…" : "Erase permanently"}</button>
              <button className="btn btn-ghost sm" onClick={() => setErasing(null)}>Cancel</button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
