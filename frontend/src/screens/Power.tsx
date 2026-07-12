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

import { confirmDialog, toast } from "@/components/dialogs";
import { useMemo, useState } from "react";
import { useParams, useNavigate } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import {
  addGroupMember,
  createGroup,
  deleteGroup,
  fmtTokens,
  removeGroupMember,
  useGroup,
  useGroups,
  usePowerAnalytics,
  usePowerDirectory,
  useWhoami,
  type AgentRollup,
  type UserRollup,
} from "@/api/client";
import { Icon } from "@/components/icons";
import { Dropdown } from "@/components/Dropdown";
import { Workflows } from "@/screens/Workflows";

// The Power-user "lead" console. A power_user is a team-lead tier between user and
// admin (admins use the Admin panel; a user is either admin or power_user, never
// both). It gives a lead surfaces scoped to the teams they lead: the RBAC groups
// they created (member management), usage analytics, and — when the feature is on —
// the event-driven Workflows they own. The backend scopes every read/write here.

type PowerTab = "teams" | "analytics" | "workflows";
const BASE_TABS: { id: PowerTab; label: string; icon: keyof typeof Icon }[] = [
  { id: "teams", label: "Teams", icon: "Team" },
  { id: "analytics", label: "Analytics", icon: "Activity" },
];

export function Power() {
  const who = useWhoami();
  const params = useParams();
  const nav = useNavigate();
  const wfOn = !!who.data?.capabilities.workflows;
  const TABS = wfOn ? [...BASE_TABS, { id: "workflows" as PowerTab, label: "Workflows", icon: "Workflows" as keyof typeof Icon }] : BASE_TABS;
  const tab: PowerTab =
    params.tab === "analytics" ? "analytics" : params.tab === "workflows" && wfOn ? "workflows" : "teams";

  // UX guard only (the API is the real boundary). Admins have no Power nav; if one
  // navigates here, send them to their own console.
  if (who.data && who.data.role !== "power_user") {
    return (
      <div className="legal-shell">
        <div className="legal-body">
          <div className="main-scroll">
            <div className="proj-panel">
              <div className="side-empty">Power users only.</div>
            </div>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="legal-shell">
      <div className="legal-tabs">
        <div className="legal-tabs-l">
          {TABS.map((t) => {
            const Glyph = Icon[t.icon];
            return (
              <button
                key={t.id}
                className={"legal-tab" + (tab === t.id ? " on" : "")}
                onClick={() => nav(`/power/${t.id}`)}
              >
                <Glyph size={15} /> {t.label}
              </button>
            );
          })}
        </div>
      </div>
      <div className="legal-body">
        {tab === "workflows" ? (
          // Workflows brings its own .main-scroll / EditorShell — don't double-wrap.
          <Workflows />
        ) : (
          <div className="main-scroll">
            {tab === "teams" ? <TeamsTab /> : <AnalyticsTab />}
          </div>
        )}
      </div>
    </div>
  );
}

// ── Teams ───────────────────────────────────────────────────────────────────────
function TeamsTab() {
  const qc = useQueryClient();
  const groups = useGroups();
  const dir = usePowerDirectory();
  const [selected, setSelected] = useState<string | null>(null);
  const [newName, setNewName] = useState("");
  const [addUser, setAddUser] = useState("");
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const group = useGroup(selected ?? undefined);

  const nameOf = useMemo(() => {
    const m = new Map<string, string>();
    dir.data?.forEach((u) => m.set(u.id, u.display_name || u.email));
    return m;
  }, [dir.data]);

  const refreshGroups = () => qc.invalidateQueries({ queryKey: ["groups"] });
  const refreshGroup = () => qc.invalidateQueries({ queryKey: ["group", selected] });

  async function guard(fn: () => Promise<unknown>) {
    if (busy) return;
    setBusy(true);
    try {
      await fn();
    } catch (e) {
      toast((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="proj-panel">
      <div className="proj-panel-head">
        <div>
          <div className="eyebrow">Power</div>
          <h2 className="serif" style={{ fontSize: 26 }}>Your teams</h2>
        </div>
      </div>
      <p className="ed-hint mono" style={{ marginBottom: 14 }}>
        Groups you created. Add anyone to build a team; deleting a group that grants
        project access is blocked until those shares are removed.
      </p>

      <div className="flex gap-6" style={{ display: "flex", gap: 24 }}>
        <div style={{ width: 240, flexShrink: 0 }}>
          <div style={{ display: "flex", gap: 8, marginBottom: 10 }}>
            <input
              className="field"
              style={{ flex: 1 }}
              placeholder="New group name"
              value={newName}
              onChange={(e) => setNewName(e.target.value)}
            />
            <button
              className="btn btn-gold sm"
              disabled={busy || !newName.trim()}
              onClick={() => guard(() => createGroup(newName.trim()).then((g) => { setNewName(""); setSelected(g.id); refreshGroups(); }))}
            >
              <Icon.Plus size={14} />
            </button>
          </div>
          {groups.isLoading ? (
            <div className="side-empty">Loading…</div>
          ) : (groups.data?.length ?? 0) === 0 ? (
            <div className="side-empty">No groups yet.</div>
          ) : (
            <div className="docs-list flush">
              {groups.data?.map((g) => (
                <div
                  key={g.id}
                  className={"docs-row" + (selected === g.id ? " on" : "")}
                  style={{ cursor: "pointer" }}
                  onClick={() => setSelected(g.id)}
                >
                  <span style={{ flex: 1 }}>{g.name}</span>
                </div>
              ))}
            </div>
          )}
        </div>

        <div style={{ minWidth: 0, flex: 1 }}>
          {!selected ? (
            <div className="side-empty">Select a group.</div>
          ) : (
            <>
              <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: 12 }}>
                <h3 className="serif" style={{ fontSize: 20 }}>{group.data?.name}</h3>
                <button
                  className="btn btn-line sm"
                  disabled={busy}
                  onClick={async () => {
                    if (await confirmDialog({ title: "Delete this group?", danger: true, confirmLabel: "Delete" }))
                      guard(() => deleteGroup(selected).then(() => { setSelected(null); refreshGroups(); }));
                  }}
                >
                  <Icon.Trash size={14} /> Delete group
                </button>
              </div>

              <div style={{ display: "flex", gap: 8, marginBottom: 12 }}>
                <div style={{ flex: 1 }}>
                  <Dropdown
                    value={addUser}
                    onChange={setAddUser}
                    ariaLabel="Add member"
                    fullWidth
                    options={[
                      { value: "", label: "Add member…" },
                      ...(dir.data ?? []).filter((u) => !group.data?.members.includes(u.id)).map((u) => ({ value: u.id, label: u.display_name || u.email })),
                    ]}
                  />
                </div>
                <button
                  className="btn btn-gold sm"
                  disabled={busy || !addUser}
                  onClick={() => guard(async () => {
                    const res = await addGroupMember(selected, addUser);
                    setAddUser("");
                    if (res.pending) {
                      setNotice("This group opens a matter you don't own — the add is awaiting the matter owner's approval.");
                    } else {
                      setNotice(null);
                      refreshGroup();
                    }
                  })}
                >
                  Add
                </button>
              </div>
              {notice && <div className="ed-hint mono" style={{ marginBottom: 12, color: "var(--gold)" }}>{notice}</div>}

              {group.data?.members.length === 0 ? (
                <div className="side-empty">No members.</div>
              ) : (
                <div className="docs-list flush">
                  {group.data?.members.map((uid) => (
                    <div key={uid} className="docs-row">
                      <span style={{ flex: 1 }}>{nameOf.get(uid) ?? uid}</span>
                      <button
                        className="btn btn-line sm"
                        disabled={busy}
                        onClick={() => guard(() => removeGroupMember(selected, uid).then(refreshGroup))}
                      >
                        Remove
                      </button>
                    </div>
                  ))}
                </div>
              )}
            </>
          )}
        </div>
      </div>
    </div>
  );
}

// ── Analytics ─────────────────────────────────────────────────────────────────
// Small self-contained bar list (the Admin charts aren't exported; the Power view
// stays deliberately lighter — tokens/messages for the lead's own teams).
function Bars({ rows }: { rows: { name: string; v: number; label: string }[] }) {
  const max = Math.max(1, ...rows.map((r) => r.v));
  if (rows.length === 0) return <div className="side-empty">No activity yet.</div>;
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
      {rows.map((r, i) => (
        <div key={i} style={{ display: "flex", alignItems: "center", gap: 10 }}>
          <span style={{ width: 160, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{r.name}</span>
          <div style={{ flex: 1, background: "var(--navy-lighter, #1b2435)", borderRadius: 4, height: 10 }}>
            <div style={{ width: `${Math.round((r.v / max) * 100)}%`, background: "var(--gold, #c9a24b)", height: 10, borderRadius: 4 }} />
          </div>
          <span className="mono" style={{ width: 70, textAlign: "right", fontSize: 12 }}>{r.label}</span>
        </div>
      ))}
    </div>
  );
}

function tokenBars<T extends { prompt_tokens: number; completion_tokens: number }>(
  rows: T[],
  name: (r: T) => string,
) {
  return rows
    .map((r) => ({ name: name(r), v: r.prompt_tokens + r.completion_tokens, label: fmtTokens(r.prompt_tokens + r.completion_tokens) }))
    .sort((x, y) => y.v - x.v)
    .slice(0, 6);
}

function AnalyticsTab() {
  const a = usePowerAnalytics();

  return (
    <div className="proj-panel">
      <div className="proj-panel-head">
        <div>
          <div className="eyebrow">Power</div>
          <h2 className="serif" style={{ fontSize: 26 }}>Team analytics</h2>
        </div>
      </div>
      <p className="ed-hint mono" style={{ marginBottom: 14 }}>
        Usage for the teams you lead — members of groups you created and projects you
        own. Not the whole firm.
      </p>

      {a.isLoading && <div className="side-empty">Loading…</div>}
      {a.error && <div className="side-empty">Could not load analytics. {(a.error as Error).message}</div>}
      {a.data && (
        <>
          <div className="stat-cards" style={{ display: "flex", gap: 16, marginBottom: 20 }}>
            <div className="stat-card">
              <span className="serif stat-card-v">{a.data.team_size}</span>
              <span className="stat-card-l">Team members</span>
            </div>
            <div className="stat-card">
              <span className="serif stat-card-v">{fmtTokens(a.data.total_prompt_tokens + a.data.total_completion_tokens)}</span>
              <span className="stat-card-l">Tokens</span>
            </div>
            <div className="stat-card">
              <span className="serif stat-card-v">{a.data.total_answers.toLocaleString()}</span>
              <span className="stat-card-l">Answers</span>
            </div>
          </div>

          <div className="chart-card" style={{ marginBottom: 18 }}>
            <div className="chart-head"><h4>Most-used agents</h4><span className="ed-hint mono">tokens</span></div>
            <Bars rows={tokenBars(a.data.per_agent, (r: AgentRollup) => r.agent_name ?? "(no agent)")} />
          </div>
          <div className="chart-card" style={{ marginBottom: 18 }}>
            <div className="chart-head"><h4>Tokens by member</h4><span className="ed-hint mono">tokens</span></div>
            <Bars rows={tokenBars(a.data.per_user, (r: UserRollup) => r.email ?? "—")} />
          </div>
          <div className="chart-card">
            <div className="chart-head"><h4>Answers by member</h4><span className="ed-hint mono">count</span></div>
            <Bars rows={a.data.per_user
              .map((u) => ({ name: u.email ?? "—", v: u.count, label: u.count.toLocaleString() }))
              .sort((x, y) => y.v - x.v)
              .slice(0, 6)} />
          </div>
        </>
      )}
    </div>
  );
}
