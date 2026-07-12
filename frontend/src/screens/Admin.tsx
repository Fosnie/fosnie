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
import { Fragment, useEffect, useMemo, useState } from "react";
import { useNavigate, useParams } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import {
  GRANT_PERMISSIONS,
  GRANT_RESOURCE_TYPES,
  addGroupMember,
  clearGroupFlag,
  createGrant,
  createGroup,
  deactivateUser,
  deleteGroup,
  fmtTokens,
  type AdminFeedbackItem,
  reactivateUser,
  resetUserMfa,
  removeGroupMember,
  revokeGrant,
  setConfig,
  useProviders,
  setProvider,
  testProvider,
  createAdminLlm,
  updateAdminLlm,
  deleteAdminLlm,
  setAdminLlmDefault,
  testAdminLlm,
  type ProviderConfig,
  type ProviderTestResult,
  useVoiceLive,
  setVoiceLive,
  type VoiceLiveBody,
  useEmbeddingIndex,
  reindexEmbeddings,
  useAdminAnnouncements,
  createAnnouncement,
  updateAnnouncement,
  deleteAnnouncement,
  useAdminWelcome,
  setWelcome,
  type Announcement,
  type Severity,
  type WelcomeMessage,
  useAdminFeedback,
  useAdminConfig,
  useAdminIntegrations,
  useAdminMcpServers,
  registerMcpServer,
  approveMcpServer,
  deleteMcpServer,
  type McpServer,
  type McpAuthType,
  useToolCatalog,
  putNativeToolOverride,
  resetNativeTool,
  createCustomTool,
  updateCustomTool,
  enableCustomTool,
  disableCustomTool,
  deleteCustomTool,
  testRunCustomTool,
  type NativeToolEntry,
  type CustomToolEntry,
  type CustomToolInput,
  useAdminUsers,
  useAgents,
  useAnalytics,
  useGroundednessAnalytics,
  downloadVerificationReport,
  type GroundednessAnalytics,
  useAnomalies,
  useAuditEvents,
  useAutomations,
  useGrants,
  setGroupFlag,
  useGroup,
  useGroupFlags,
  useGroups,
  useProjects,
  usePrompts,
  useReadiness,
  useSkills,
  useWhoami,
} from "@/api/client";
import { AreaChart, Bars, Donut } from "@/components/charts";
import { Icon } from "@/components/icons";
import { Dropdown } from "@/components/Dropdown";
import { Workflows } from "@/screens/Workflows";
import { useBusy } from "@/components/useBusy";
import { BTN, BTN2, BTN_DANGER, Badge, H1, INPUT, LABEL, TD, TH } from "@/components/adminUi";
import { getAdminSections, registerAdminSection } from "@/ext/registry";

const ADMIN_ROLES = ["client_admin", "super_admin"];


export function Admin() {
  const { section } = useParams();
  const nav = useNavigate();
  const who = useWhoami();
  const isAdmin = ADMIN_ROLES.includes(who.data?.role ?? "");
  // Custom RBAC: a delegated admin holds specific permissions without being a full
  // admin. `holds(p)` is true for a full admin, or when whoami.permissions carries
  // `p` (or its `:scoped` variant, a narrowed holding). Core sends an empty list ⇒
  // this collapses to the plain `is_admin` gate (unchanged behaviour).
  const perms = who.data?.permissions ?? [];
  const holds = (p: string) => isAdmin || perms.includes(p) || perms.includes(`${p}:scoped`);
  const canAdmin = isAdmin || perms.length > 0;
  const active = section ?? "overview";
  // Verification dashboard rides on the groundedness capability (BACKLOG A1);
  // slotted right after Analytics as a sibling governance view.
  const gOn = !!who.data?.capabilities.groundedness;
  // Sections come from the extension registry (Core registers the host set below;
  // at the split Enterprise registers its own). Each is gated by its capability
  // (edition) AND its permission (delegated admin) — the endpoints 403 regardless
  // (defense-in-depth). A full admin sees every capability-enabled section.
  const caps = who.data?.capabilities;
  const visible = getAdminSections().filter(
    (s) => (!s.capability || !!caps?.[s.capability]) && (!s.permission || holds(s.permission)),
  );
  const headTabs: [string, string][] = gOn
    ? [["analytics", "Analytics"], ["verification", "Verification"]]
    : [["analytics", "Analytics"]];
  // The org-wide Analytics/Verification head tabs need unscoped analytics.view.
  const showAnalytics = holds("analytics.view");
  const tabs: [string, string][] = [
    ["overview", "Overview"],
    ...(showAnalytics ? headTabs : []),
    ...visible.map((s) => [s.key, s.label] as [string, string]),
  ];

  if (who.isLoading) return <div className="main-scroll"><div className="panel">Loading…</div></div>;
  if (!canAdmin) {
    return (
      <div className="flex h-full items-center justify-center text-sm text-slate/70">
        Administrators only.
      </div>
    );
  }

  return (
    <div className="legal-shell">
      <div className="legal-tabs">
        <div className="legal-tabs-l" style={{ overflowX: "auto" }}>
          {tabs.map(([key, label]) => (
            <button
              key={key}
              className={"legal-tab" + (active === key ? " on" : "")}
              onClick={() => nav(key === "overview" ? "/admin" : `/admin/${key}`)}
            >
              {label}
            </button>
          ))}
        </div>
        <div className="legal-tabs-r mono"><Icon.Shield size={13} /> {who.data?.role}</div>
      </div>

      <div className="legal-body">
        {(() => {
          const sec = visible.find((s) => s.key === active);
          // Full-bleed sections (Workflows) bring their own .main-scroll / shell.
          if (sec?.fullBleed) {
            const C = sec.component;
            return <C />;
          }
          const Sec = sec?.component;
          return (
            <div className="main-scroll">
              <div className="panel">
                {active === "overview" && <OverviewDashboard selfId={who.data?.user_id} />}
                {active === "analytics" && <AnalyticsView />}
                {active === "verification" && <VerificationView />}
                {Sec && <Sec />}
              </div>
            </div>
          );
        })()}
      </div>
    </div>
  );
}

// ── Overview dashboard: snapshots from the other sections ──
function OverviewDashboard({ selfId }: { selfId?: string }) {
  const ready = useReadiness();
  const who = useWhoami();
  const audit = useAuditEvents({ limit: 6 });
  const analytics = useAnalytics();
  void selfId;

  const stat = (ok: boolean | undefined, on: string, off: string) =>
    <span className={"sys-stat " + (ok ? "ready" : "degraded")}>{ok ? on : off}</span>;
  const topUsers = (analytics.data?.per_user ?? [])
    .slice().sort((a, b) => b.count - a.count).slice(0, 5)
    .map((u) => ({ name: u.email ?? u.user_id ?? "—", v: u.count }));
  const topAgents = (analytics.data?.per_agent ?? [])
    .slice().sort((a, b) => (b.prompt_tokens + b.completion_tokens) - (a.prompt_tokens + a.completion_tokens)).slice(0, 5)
    .map((g) => {
      const t = g.prompt_tokens + g.completion_tokens;
      return { name: g.agent_name ?? "(no agent)", v: t, label: fmtTokens(t) };
    });

  return (
    <div className="anim-on fade-in admin-grid">
      <div className="admin-card">
        <div className="admin-card-head"><h4>System</h4></div>
        <div className="sys-list">
          <div className="sys-row"><span className="sys-name"><Icon.Database size={14} /> Postgres</span>{ready.data?.checks ? stat(ready.data.checks.postgres, "up", "down") : "—"}</div>
          <div className="sys-row"><span className="sys-name"><Icon.Database size={14} /> Redis</span>{ready.data?.checks ? stat(ready.data.checks.redis, "up", "down") : "—"}</div>
          <div className="sys-row"><span className="sys-name"><Icon.Activity size={14} /> Readiness</span>{stat(ready.data?.status === "ready", "ready", ready.data?.status ?? "—")}</div>
          <div className="sys-row"><span className="sys-name"><Icon.Code size={14} /> Code interpreter</span>{stat(who.data?.capabilities.code_interpreter, "enabled", "off")}</div>
          <div className="sys-row"><span className="sys-name"><Icon.Send2 size={14} /> Voice</span>{stat(who.data?.capabilities.voice, "enabled", "off")}</div>
        </div>
      </div>

      <div className="admin-card">
        <div className="admin-card-head"><h4>Recent audit</h4><span className="ed-hint mono">last 6</span></div>
        <div className="audit">
          {(audit.data ?? []).slice(0, 6).map((e) => (
            <div key={e.id} className="audit-row mono">
              <span className="audit-t">{new Date(e.occurred_at).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })}</span>
              {e.actor_role} · {e.action_type}
            </div>
          ))}
          {(!audit.data || audit.data.length === 0) && <div className="audit-row mono">No recent events.</div>}
        </div>
      </div>

      <div className="admin-card">
        <div className="admin-card-head"><h4>Most-active users</h4><span className="ed-hint mono">messages</span></div>
        {topUsers.length ? <Bars data={topUsers} /> : <p className="text-sm text-slate/70">No usage yet.</p>}
      </div>

      <div className="admin-card">
        <div className="admin-card-head"><h4>Most-used agents</h4><span className="ed-hint mono">tokens</span></div>
        {topAgents.length ? <Bars data={topAgents} accentTop /> : <p className="text-sm text-slate/70">No usage yet.</p>}
      </div>
    </div>
  );
}


// ── Users ─────────────────────────────────────────────────────────────────────
function UsersSection({ selfId }: { selfId?: string }) {
  const qc = useQueryClient();
  const users = useAdminUsers();
  const { busy, run } = useBusy();
  const refresh = () => qc.invalidateQueries({ queryKey: ["admin-users"] });

  return (
    <div>
      <H1>Users</H1>
      <p className="mb-4 text-xs text-slate/70">
        Users + roles originate in Keycloak (created on first login). Here you can deactivate / reactivate.
      </p>
      {users.isLoading ? (
        <p className="text-sm text-slate">Loading…</p>
      ) : (
        <table className="w-full border-collapse text-sm">
          <thead>
            <tr>
              <th className={TH}>Email</th>
              <th className={TH}>Name</th>
              <th className={TH}>Role</th>
              <th className={TH}>Status</th>
              <th className={TH}>MFA</th>
              <th className={TH}></th>
            </tr>
          </thead>
          <tbody>
            {users.data?.map((u) => (
              <tr key={u.id}>
                <td className={TD}>{u.email}</td>
                <td className={TD}>
                  {u.display_name}
                  {u.managed_by === "scim" && <Badge tone="slate">Managed by IdP</Badge>}
                </td>
                <td className={TD}><Badge tone={u.role.includes("admin") ? "gold" : "slate"}>{u.role}</Badge></td>
                <td className={TD}>{u.deactivated ? <Badge tone="red">deactivated</Badge> : <Badge tone="green">active</Badge>}</td>
                <td className={TD}>{u.mfa_enabled ? <Badge tone="green">on</Badge> : <Badge tone="slate">off</Badge>}</td>
                <td className={TD}>
                  {u.id === selfId ? (
                    <span className="text-sm text-slate/60">you</span>
                  ) : u.managed_by === "scim" ? (
                    // Lifecycle owned by the customer IdP (SCIM) — deactivate there.
                    <span className="text-xs text-slate/50">directory-managed</span>
                  ) : u.deactivated ? (
                    <button className={BTN2} disabled={!!busy} onClick={() => run("Reactivate", () => reactivateUser(u.id).then(refresh))}>
                      Reactivate
                    </button>
                  ) : (
                    <span className="inline-flex gap-2">
                      {u.mfa_enabled && (
                        // Device lost with no recovery codes left: clear the factor so
                        // the user re-enrols (forced next login if MFA is mandatory).
                        <button className={BTN2} disabled={!!busy} onClick={async () => { if (await confirmDialog({ title: "Reset this user's MFA?", body: "Their second factor is removed and every session is signed out. They set it up again at next sign-in.", confirmLabel: "Reset MFA" })) run("Reset MFA", () => resetUserMfa(u.id).then(refresh), "MFA reset."); }}>
                          Reset MFA
                        </button>
                      )}
                      <button className={BTN_DANGER} disabled={!!busy} onClick={() => run("Deactivate", () => deactivateUser(u.id).then(refresh))}>
                        Deactivate
                      </button>
                    </span>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}

// ── Feedback (user 👍/👎 triage) ────────────────────────────────────────────────
function FeedbackSection() {
  const [filter, setFilter] = useState<"all" | "up" | "down">("all");
  const fb = useAdminFeedback(filter === "all" ? undefined : filter);
  const FILTERS: [typeof filter, string][] = [["all", "All"], ["up", "Positive"], ["down", "Negative"]];

  return (
    <div>
      <H1>User feedback</H1>
      <div className="chip-wrap" style={{ marginBottom: 14 }}>
        {FILTERS.map(([k, l]) => (
          <button key={k} className={"skill-chip" + (filter === k ? " on" : "")} onClick={() => setFilter(k)}>{l}</button>
        ))}
      </div>
      {fb.isLoading ? (
        <p className="text-sm text-slate">Loading…</p>
      ) : (fb.data?.length ?? 0) === 0 ? (
        <p className="text-sm text-slate/70">No feedback yet.</p>
      ) : (
        <div className="fb-list">
          {fb.data!.map((f: AdminFeedbackItem) => (
            <div key={f.id} className="fb-row">
              <span className={"fb-rating " + (f.rating === "up" ? "up" : "down")}>
                {f.rating === "up" ? <Icon.Like size={16} /> : <Icon.Dislike size={16} />}
              </span>
              <div className="fb-main">
                <div className="fb-meta mono">
                  {f.user_email ?? "—"} · {f.agent_name ?? "(no agent)"}{f.model ? ` · ${f.model}` : ""} · {new Date(f.created_at).toLocaleString()}
                </div>
                {f.comment && <div className="fb-comment">“{f.comment}”</div>}
                <div className="fb-excerpt">{f.message_excerpt}{f.message_excerpt.length >= 200 ? "…" : ""}</div>
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

// ── Sharing (access grants) ────────────────────────────────────────────────────
function SharingSection() {
  const qc = useQueryClient();
  const { busy, run } = useBusy();
  const projects = useProjects();
  const agents = useAgents();
  const skills = useSkills();
  const prompts = usePrompts();
  const automations = useAutomations();
  const users = useAdminUsers();
  const groups = useGroups();

  const [resourceType, setResourceType] = useState<string>("project");
  const [resourceId, setResourceId] = useState<string>("");
  const grants = useGrants(resourceType, resourceId || undefined);

  const [principalType, setPrincipalType] = useState<"user" | "group">("user");
  const [principalId, setPrincipalId] = useState<string>("");
  const [perms, setPerms] = useState<Set<string>>(new Set(["read"]));

  const nameOf = useMemo(() => {
    const m = new Map<string, string>();
    users.data?.forEach((u) => m.set(u.id, u.email));
    groups.data?.forEach((g) => m.set(g.id, g.name));
    return m;
  }, [users.data, groups.data]);

  const refresh = () => qc.invalidateQueries({ queryKey: ["admin-grants", resourceType, resourceId] });

  const resourceLists: Record<string, { id: string; name: string }[]> = {
    project: projects.data ?? [],
    agent: agents.data ?? [],
    skill: skills.data ?? [],
    prompt: prompts.data ?? [],
    automation: automations.data ?? [],
  };
  const pickerOpts = resourceLists[resourceType] ?? [];
  const principals = principalType === "user" ? users.data ?? [] : groups.data ?? [];

  const togglePerm = (p: string) =>
    setPerms((cur) => { const n = new Set(cur); if (n.has(p)) n.delete(p); else n.add(p); return n; });

  async function addGrants() {
    // One grant row per selected permission; tolerate already-existing ones.
    await Promise.allSettled(
      [...perms].map((permission) =>
        createGrant({ resource_type: resourceType, resource_id: resourceId, principal_type: principalType, principal_id: principalId, permission }),
      ),
    );
    setPrincipalId("");
    refresh();
  }

  return (
    <div>
      <H1>Sharing &amp; access grants</H1>
      <div className="mb-5 flex flex-wrap items-end gap-3">
        <div>
          <label className={LABEL}>Resource type</label>
          <Dropdown
            value={resourceType}
            onChange={(v) => { setResourceType(v); setResourceId(""); }}
            ariaLabel="Resource type"
            fullWidth
            icon={<Icon.Layers size={14} />}
            options={GRANT_RESOURCE_TYPES.map((t) => ({ value: t.value, label: t.label }))}
          />
        </div>
        <div className="min-w-[18rem] flex-1">
          <label className={LABEL}>Resource</label>
          <Dropdown
            value={resourceId}
            onChange={setResourceId}
            ariaLabel="Resource"
            fullWidth
            icon={<Icon.Folder size={14} />}
            options={[
              { value: "", label: "Select…" },
              ...pickerOpts.map((o) => ({ value: o.id, label: o.name })),
            ]}
          />
        </div>
      </div>

      {resourceId && (
        <>
          <div className="mb-2 text-xs uppercase tracking-[0.14em] text-slate">Current grants</div>
          {grants.isLoading ? (
            <p className="text-sm text-slate">Loading…</p>
          ) : grants.data?.length === 0 ? (
            <p className="mb-4 text-sm text-slate/70">No grants — owner + admins always have access.</p>
          ) : (
            <table className="mb-4 w-full border-collapse text-sm">
              <thead><tr><th className={TH}>Principal</th><th className={TH}>Type</th><th className={TH}>Permission</th><th className={TH}></th></tr></thead>
              <tbody>
                {grants.data?.map((g) => (
                  <tr key={g.id}>
                    <td className={TD}>{nameOf.get(g.principal_id) ?? g.principal_id}</td>
                    <td className={TD}>{g.principal_type === "user" ? "User" : "Group"}</td>
                    <td className={TD}><Badge tone="gold">{g.permission}</Badge></td>
                    <td className={TD}><button className={BTN_DANGER} disabled={!!busy} onClick={() => run("Revoke", () => revokeGrant(g.id).then(refresh))}>Revoke</button></td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}

          <div className="rounded-xl border border-navy-lighter bg-navy-light/40 p-4">
            <div className="mb-3 text-xs uppercase tracking-[0.14em] text-slate">Add grant</div>
            <div className="flex flex-wrap items-end gap-3">
              <div>
                <label className={LABEL}>Principal</label>
                <Dropdown
                  value={principalType}
                  onChange={(v) => { setPrincipalType(v); setPrincipalId(""); }}
                  ariaLabel="Principal type"
                  fullWidth
                  icon={<Icon.Team size={14} />}
                  options={[
                    { value: "user", label: "User" },
                    { value: "group", label: "Group" },
                  ]}
                />
              </div>
              <div className="min-w-[15rem]">
                <label className={LABEL}>{principalType === "user" ? "User" : "Group"}</label>
                <Dropdown
                  value={principalId}
                  onChange={setPrincipalId}
                  ariaLabel="Principal"
                  fullWidth
                  icon={<Icon.User size={14} />}
                  options={[
                    { value: "", label: "Select…" },
                    ...principals.map((p: { id: string; email?: string; name?: string }) => ({ value: p.id, label: p.email ?? p.name ?? p.id })),
                  ]}
                />
              </div>
              <div>
                <label className={LABEL}>Permissions</label>
                <div className="chip-wrap">
                  {GRANT_PERMISSIONS.map((p) => (
                    <button key={p.value} type="button" className={"skill-chip" + (perms.has(p.value) ? " on" : "")} onClick={() => togglePerm(p.value)}>
                      {p.label}
                    </button>
                  ))}
                </div>
              </div>
              <button
                className={BTN}
                disabled={!!busy || !principalId || perms.size === 0}
                onClick={() => run("Add grant", addGrants, "Grant added.")}
              >
                Add
              </button>
            </div>
          </div>
        </>
      )}
    </div>
  );
}

// ── Groups ──────────────────────────────────────────────────────────────────────
function GroupsSection() {
  const qc = useQueryClient();
  const { busy, run } = useBusy();
  const groups = useGroups();
  const users = useAdminUsers();
  const [selected, setSelected] = useState<string | null>(null);
  const [newName, setNewName] = useState("");
  const [addUser, setAddUser] = useState("");
  const group = useGroup(selected ?? undefined);

  const emailOf = useMemo(() => {
    const m = new Map<string, string>();
    users.data?.forEach((u) => m.set(u.id, u.email));
    return m;
  }, [users.data]);

  const refreshGroups = () => qc.invalidateQueries({ queryKey: ["groups"] });
  const refreshGroup = () => qc.invalidateQueries({ queryKey: ["group", selected] });

  return (
    <div>
      <H1>Groups</H1>
      <div className="flex flex-col gap-4 sm:flex-row sm:gap-6">
        <div className="w-full sm:w-64 sm:shrink-0">
          <div className="mb-2 flex gap-2">
            <input className={INPUT + " flex-1"} placeholder="New group name" value={newName} onChange={(e) => setNewName(e.target.value)} />
            <button className={BTN} disabled={!!busy || !newName.trim()} onClick={() => run("Create group", () => createGroup(newName.trim()).then(() => { setNewName(""); refreshGroups(); }), "Group created.")}>＋</button>
          </div>
          {groups.isLoading ? <p className="text-sm text-slate">Loading…</p> : groups.data?.length === 0 ? <p className="text-sm text-slate/70">No groups.</p> : (
            <ul className="space-y-1">
              {groups.data?.map((g) => (
                <li key={g.id}>
                  <button onClick={() => setSelected(g.id)} className={"block w-full truncate rounded px-2 py-1 text-left text-sm " + (selected === g.id ? "bg-navy-lighter text-slate-lightest" : "text-slate hover:text-slate-lightest")}>{g.name}</button>
                </li>
              ))}
            </ul>
          )}
        </div>

        <div className="min-w-0 flex-1">
          {!selected ? (
            <p className="text-sm text-slate/70">Select a group.</p>
          ) : (
            <>
              <div className="mb-3 flex items-center justify-between">
                <h2 className="text-lg text-slate-lightest">{group.data?.name}</h2>
                <button className={BTN_DANGER} disabled={!!busy} onClick={async () => { if (await confirmDialog({ title: "Delete group?", danger: true, confirmLabel: "Delete" })) run("Delete group", () => deleteGroup(selected).then(() => { setSelected(null); refreshGroups(); }), "Group deleted."); }}>Delete group</button>
              </div>
              <div className="mb-3 flex gap-2">
                <div className="flex-1">
                  <Dropdown
                    value={addUser}
                    onChange={setAddUser}
                    ariaLabel="Add member"
                    fullWidth
                    options={[
                      { value: "", label: "Add member…" },
                      ...(users.data ?? []).filter((u) => !group.data?.members.includes(u.id)).map((u) => ({ value: u.id, label: u.email })),
                    ]}
                  />
                </div>
                <button className={BTN} disabled={!!busy || !addUser} onClick={() => run("Add member", () => addGroupMember(selected, addUser).then(() => { setAddUser(""); refreshGroup(); }))}>Add</button>
              </div>
              <ul className="divide-y divide-navy-lighter">
                {group.data?.members.length === 0 && <p className="text-sm text-slate/70">No members.</p>}
                {group.data?.members.map((uid) => (
                  <li key={uid} className="flex items-center justify-between py-2 text-sm">
                    <span className="text-slate-lightest">{emailOf.get(uid) ?? uid}</span>
                    <button className={BTN2} disabled={!!busy} onClick={() => run("Remove member", () => removeGroupMember(selected, uid).then(refreshGroup))}>Remove</button>
                  </li>
                ))}
              </ul>

              <GroupFeatureFlags groupId={selected} />
            </>
          )}
        </div>
      </div>
    </div>
  );
}

// Per-group feature access (Tier-2 #8). Restrict-only: turning a feature OFF
// disables it for this group's members; ON inherits the global host setting.
function GroupFeatureFlags({ groupId }: { groupId: string }) {
  const qc = useQueryClient();
  const { busy, run } = useBusy();
  const flags = useGroupFlags(groupId);
  const FEATURES: { key: string; label: string }[] = [
    { key: "voice", label: "Voice" },
    { key: "code_interpreter", label: "Code interpreter" },
  ];
  const disabled = (k: string) => flags.data?.some((f) => f.feature === k && !f.enabled) ?? false;
  const toggle = (k: string, on: boolean) =>
    run(on ? "Enable" : "Disable", () =>
      (on ? clearGroupFlag(groupId, k) : setGroupFlag(groupId, k, false))
        .then(() => qc.invalidateQueries({ queryKey: ["group-flags", groupId] })));
  return (
    <div className="mt-6">
      <h3 className="mb-1 text-sm font-semibold text-slate-lightest">Feature access</h3>
      <p className="mb-3 text-xs text-slate/60">Turn a feature off for this group's members. A group can only restrict — it never enables a feature the deployment has turned off.</p>
      {flags.isLoading ? <p className="text-sm text-slate">Loading…</p> : (
        <div className="space-y-2">
          {FEATURES.map((f) => {
            const off = disabled(f.key);
            return (
              <div key={f.key} className="flex items-center justify-between rounded-md border border-navy-lighter bg-navy-light/40 px-3 py-2">
                <span className="text-sm text-slate-lightest">{f.label}</span>
                <button
                  className={off ? BTN_DANGER : BTN2}
                  disabled={!!busy}
                  onClick={() => toggle(f.key, off)}
                >
                  {off ? "Disabled for group" : "Enabled"}
                </button>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

// ── Analytics (charts) ────────────────────────────────────────────────────────
function AnalyticsView() {
  const a = useAnalytics();
  if (a.isLoading) return <p className="text-sm text-slate">Loading analytics…</p>;
  const d = a.data;
  if (!d) return <p className="text-sm text-urgency-red">Could not load analytics. {(a.error as Error | undefined)?.message}</p>;

  const tokensMonth = d.series.reduce((s, p) => s + p.tokens, 0);
  const messagesMonth = d.series.reduce((s, p) => s + p.messages, 0);
  const pct = (n: number) => (d.total_users > 0 ? Math.min(100, Math.round((n / d.total_users) * 100)) : 0);
  const top = <T,>(rows: T[], v: (r: T) => number, name: (r: T) => string) =>
    rows.map((r) => ({ name: name(r), v: v(r) })).sort((x, y) => y.v - x.v).slice(0, 6);
  // Token bars keep the RAW value for the bar width and a human label, so small
  // counts still render a visible bar (not "0M").
  const tokenBars = <T,>(rows: T[], tok: (r: T) => number, name: (r: T) => string) =>
    rows.map((r) => { const t = tok(r); return { name: name(r), v: t, label: fmtTokens(t) }; })
      .sort((x, y) => y.v - x.v).slice(0, 6);

  const agentsByTokens = tokenBars(d.per_agent, (g) => g.prompt_tokens + g.completion_tokens, (g) => g.agent_name ?? "(no agent)");
  const tokensByUser = tokenBars(d.per_user, (u) => u.prompt_tokens + u.completion_tokens, (u) => u.email ?? "—");
  const messagesByUser = top(d.per_user, (u) => u.count, (u) => u.email ?? "—");

  return (
    <div className="analytics anim-on fade-in">
      <div className="stat-cards">
        <div className="stat-card">
          <span className="serif stat-card-v">{d.total_users}</span>
          <span className="stat-card-l">Total users</span>
          <span className="stat-card-d up mono">+{d.new_users_30} this month</span>
        </div>
        <div className="stat-card">
          <span className="serif stat-card-v">{fmtTokens(tokensMonth)}</span>
          <span className="stat-card-l">Tokens · 30 days</span>
          <span className="stat-card-d mono">{messagesMonth.toLocaleString()} messages</span>
        </div>
        <div className="stat-card with-donut"><Donut pct={pct(d.active_7)} label="Active" sub="last 7 days" /></div>
        <div className="stat-card with-donut"><Donut pct={pct(d.active_30)} label="Active" sub="last 30 days" /></div>
      </div>

      <div className="chart-card wide">
        <div className="chart-head"><h4>Token usage · last 30 days</h4><span className="ed-hint mono">daily</span></div>
        <AreaChart series={d.series.map((p) => p.tokens)} labels={d.series.map((p) => p.day)} formatValue={fmtTokens} />
      </div>

      <div className="chart-row">
        <div className="chart-card">
          <div className="chart-head"><h4>Most-used agents</h4><span className="ed-hint mono">tokens</span></div>
          <Bars data={agentsByTokens} accentTop />
        </div>
        <div className="chart-card">
          <div className="chart-head"><h4>Tokens by user</h4><span className="ed-hint mono">tokens</span></div>
          <Bars data={tokensByUser} />
        </div>
      </div>
      <div className="chart-card">
        <div className="chart-head"><h4>Messages sent by user</h4><span className="ed-hint mono">last 30 days</span></div>
        <Bars data={messagesByUser} />
      </div>

      <details className="text-sm">
        <summary className="cursor-pointer text-slate hover:text-slate-lightest">Detailed breakdown (per model / user / agent)</summary>
        <div className="mt-3 space-y-5">
          <table className="w-full border-collapse">
            <thead><tr><th className={TH}>Model</th><th className={TH}>Answers</th><th className={TH}>Prompt</th><th className={TH}>Completion</th></tr></thead>
            <tbody>{d.per_model.map((m, i) => <tr key={i}><td className={TD}>{m.model ?? "—"}</td><td className={TD}>{m.count}</td><td className={TD}>{m.prompt_tokens.toLocaleString()}</td><td className={TD}>{m.completion_tokens.toLocaleString()}</td></tr>)}</tbody>
          </table>
          <table className="w-full border-collapse">
            <thead><tr><th className={TH}>User</th><th className={TH}>Answers</th><th className={TH}>Total tokens</th></tr></thead>
            <tbody>{d.per_user.map((u, i) => <tr key={i}><td className={TD}>{u.email ?? u.user_id ?? "—"}</td><td className={TD}>{u.count}</td><td className={TD}>{(u.prompt_tokens + u.completion_tokens).toLocaleString()}</td></tr>)}</tbody>
          </table>
          <table className="w-full border-collapse">
            <thead><tr><th className={TH}>Agent</th><th className={TH}>Answers</th><th className={TH}>Total tokens</th></tr></thead>
            <tbody>{d.per_agent.map((g, i) => <tr key={i}><td className={TD}>{g.agent_name ?? (g.agent_id ? g.agent_id.slice(0, 8) : "(no agent)")}</td><td className={TD}>{g.count}</td><td className={TD}>{(g.prompt_tokens + g.completion_tokens).toLocaleString()}</td></tr>)}</tbody>
          </table>
        </div>
      </details>
    </div>
  );
}

// ── Verification (groundedness dashboard, BACKLOG A1) ───────────────────────────
// Surfaces the otherwise-invisible verification moat for the client-admin:
// per-interaction trust scores, source traceability, and answer-quality-over-time,
// segmented by mode (live chat = Mode A, draft/document = Mode B).
function pctScore(f: number | null | undefined): number {
  return f == null ? 0 : Math.round(f * 100);
}

/** A faithfulness percentage as a coloured chip (green ≥85, amber ≥60, red below). */
function TrustChip({ score }: { score: number | null }) {
  if (score == null) return <Badge tone="slate">n/a</Badge>;
  const p = Math.round(score * 100);
  const tone = p >= 85 ? "green" : p >= 60 ? "gold" : "red";
  return <Badge tone={tone}>{p}%</Badge>;
}

function VerificationView() {
  const nav = useNavigate();
  const q = useGroundednessAnalytics();
  if (q.isLoading) return <p className="text-sm text-slate">Loading verification metrics…</p>;
  const d = q.data;
  if (!d) return <p className="text-sm text-urgency-red">Could not load verification metrics. {(q.error as Error | undefined)?.message}</p>;

  const liveClaims = d.live_verdicts.supported + d.live_verdicts.contradicted + d.live_verdicts.not_mentioned;
  const supportedPct = liveClaims > 0 ? Math.round((d.live_verdicts.supported / liveClaims) * 100) : 0;
  const citedPct = pctScore(d.live_cited_fraction);

  // Trust-over-time: carry the last known daily mean across no-activity days so the
  // line reads as a trend, not a series of false zeros.
  let carry = 0;
  const trustSeries = d.live_series.map((p) => { if (p.avg_score != null) carry = Math.round(p.avg_score * 100); return carry; });

  const verdictBars = (v: GroundednessAnalytics["live_verdicts"]) => [
    { name: "Supported", v: v.supported },
    { name: "Not mentioned", v: v.not_mentioned },
    { name: "Contradicted", v: v.contradicted },
  ];
  const agentBars = d.per_agent
    .filter((a) => a.avg_score != null)
    .map((a) => ({ name: a.agent_name ?? "(no agent)", v: pctScore(a.avg_score), label: `${pctScore(a.avg_score)}%` }))
    .slice(0, 6);

  const report = (runId: string) => {
    void downloadVerificationReport(runId, "pdf").catch((e) => toast(`Report failed: ${(e as Error).message}`));
  };

  return (
    <div className="analytics anim-on fade-in">
      <H1>Verification</H1>

      {/* ── Mode A — live chat ── */}
      <div className="stat-cards">
        <div className="stat-card">
          <span className="serif stat-card-v">{pctScore(d.live_avg_score)}%</span>
          <span className="stat-card-l">Avg trust score</span>
          <span className="stat-card-d mono">live RAG answers</span>
        </div>
        <div className="stat-card">
          <span className="serif stat-card-v">{d.live_runs.toLocaleString()}</span>
          <span className="stat-card-l">Verified answers</span>
          <span className="stat-card-d mono">{d.live_verdicts.contradicted.toLocaleString()} contradicted spans</span>
        </div>
        <div className="stat-card with-donut"><Donut pct={citedPct} label="Sourced" sub="carry a citation" /></div>
        <div className="stat-card with-donut"><Donut pct={supportedPct} label="Supported" sub="of all claims" /></div>
      </div>

      <div className="chart-card wide">
        <div className="chart-head"><h4>Answer trust · last 30 days</h4><span className="ed-hint mono">daily mean</span></div>
        {trustSeries.some((v) => v > 0) ? <AreaChart series={trustSeries} labels={d.live_series.map((p) => p.day)} formatValue={(v) => `${v}%`} /> : <p className="text-sm text-slate/70">No live verifications in the window.</p>}
      </div>

      <div className="chart-row">
        <div className="chart-card">
          <div className="chart-head"><h4>Verdict mix</h4><span className="ed-hint mono">live claims</span></div>
          {liveClaims > 0 ? <Bars data={verdictBars(d.live_verdicts)} /> : <p className="text-sm text-slate/70">No claims yet.</p>}
        </div>
        <div className="chart-card">
          <div className="chart-head"><h4>Grounding by agent</h4><span className="ed-hint mono">avg trust</span></div>
          {agentBars.length ? <Bars data={agentBars} accentTop /> : <p className="text-sm text-slate/70">No agent runs yet.</p>}
        </div>
      </div>

      <div className="chart-card">
        <div className="chart-head"><h4>Lowest-grounded interactions</h4><span className="ed-hint mono">click to open the chat</span></div>
        {d.lowest_interactions.length ? (
          <table className="w-full border-collapse text-sm">
            <thead><tr><th className={TH}>Trust</th><th className={TH}>Flagged</th><th className={TH}>Interaction</th><th className={TH}>When</th></tr></thead>
            <tbody>
              {d.lowest_interactions.map((it) => (
                <tr key={it.run_id} className="cursor-pointer hover:bg-navy-light" onClick={() => nav(`/c/${it.chat_id}`)}>
                  <td className={TD}><TrustChip score={it.score} /></td>
                  <td className={TD}>{it.flagged}</td>
                  <td className={TD}>{it.snippet || "—"}</td>
                  <td className={TD + " mono whitespace-nowrap"}>{new Date(it.created_at).toLocaleDateString()}</td>
                </tr>
              ))}
            </tbody>
          </table>
        ) : <p className="text-sm text-slate/70">No verified interactions yet.</p>}
      </div>

      {/* ── Mode B — draft / document ── */}
      <div className="mt-8 mb-3 flex items-center gap-3">
        <h2 className="text-lg text-slate-lightest">Draft &amp; document verification</h2>
        {d.draft_by_status.map((s) => <Badge key={s.status} tone={s.status === "error" ? "red" : s.status === "succeeded" ? "green" : "slate"}>{s.status} · {s.count}</Badge>)}
      </div>

      <div className="stat-cards">
        <div className="stat-card">
          <span className="serif stat-card-v">{pctScore(d.draft_avg_score)}%</span>
          <span className="stat-card-l">Avg trust score</span>
          <span className="stat-card-d mono">verified drafts</span>
        </div>
        <div className="stat-card">
          <span className="serif stat-card-v">{d.draft_runs.toLocaleString()}</span>
          <span className="stat-card-l">Verification runs</span>
          <span className="stat-card-d mono">{d.draft_verdicts.contradicted.toLocaleString()} contradicted</span>
        </div>
        <div className="stat-card with-donut">
          <Donut
            pct={(() => { const t = d.draft_verdicts.supported + d.draft_verdicts.contradicted + d.draft_verdicts.not_mentioned; return t > 0 ? Math.round((d.draft_verdicts.supported / t) * 100) : 0; })()}
            label="Supported" sub="of all claims"
          />
        </div>
      </div>

      <div className="chart-row">
        <div className="chart-card">
          <div className="chart-head"><h4>Draft trust · last 30 days</h4><span className="ed-hint mono">daily mean</span></div>
          {(() => { let c = 0; const s = d.draft_series.map((p) => { if (p.avg_score != null) c = Math.round(p.avg_score * 100); return c; });
            return s.some((v) => v > 0) ? <AreaChart series={s} labels={d.draft_series.map((p) => p.day)} formatValue={(v) => `${v}%`} /> : <p className="text-sm text-slate/70">No draft verifications in the window.</p>; })()}
        </div>
        <div className="chart-card">
          <div className="chart-head"><h4>Verdict mix</h4><span className="ed-hint mono">draft claims</span></div>
          {(d.draft_verdicts.supported + d.draft_verdicts.contradicted + d.draft_verdicts.not_mentioned) > 0
            ? <Bars data={verdictBars(d.draft_verdicts)} />
            : <p className="text-sm text-slate/70">No claims yet.</p>}
        </div>
      </div>

      <div className="chart-card">
        <div className="chart-head"><h4>Recent verification runs</h4><span className="ed-hint mono">drafts &amp; documents</span></div>
        {d.recent_runs.length ? (
          <table className="w-full border-collapse text-sm">
            <thead><tr><th className={TH}>Target</th><th className={TH}>Status</th><th className={TH}>Trust</th><th className={TH}>S / C / N</th><th className={TH}>When</th><th className={TH}></th></tr></thead>
            <tbody>
              {d.recent_runs.map((r) => (
                <tr key={r.run_id}>
                  <td className={TD}>{r.target_type}</td>
                  <td className={TD}><Badge tone={r.status === "error" ? "red" : r.status === "succeeded" ? "green" : "slate"}>{r.status}</Badge></td>
                  <td className={TD}><TrustChip score={r.score} /></td>
                  <td className={TD + " mono"}>{r.supported} / {r.contradicted} / {r.not_mentioned}</td>
                  <td className={TD + " mono whitespace-nowrap"}>{new Date(r.created_at).toLocaleDateString()}</td>
                  <td className={TD}>{r.status === "succeeded" && <button className={BTN2} onClick={() => report(r.run_id)}>Report</button>}</td>
                </tr>
              ))}
            </tbody>
          </table>
        ) : <p className="text-sm text-slate/70">No draft verifications yet.</p>}
      </div>
    </div>
  );
}


// ── Integrations ─────────────────────────────────────────────────────────────
function IntegrationsSection() {
  const conns = useAdminIntegrations();
  return (
    <div>
      <H1>Integrations / connectors</H1>
      <p className="mb-1 text-xs text-slate/70">External connectors ship dormant (zero-egress). Enabling permits outbound calls for that connector only.</p>
      <p className="mb-4 text-xs text-slate/60">Activation is a sensitive operation reserved for the ephemeral <strong>super-admin</strong> (an active break-glass session), not the client-admin — perform it out-of-band via the break-glass CLI. This view is read-only.</p>
      {conns.isLoading ? <p className="text-sm text-slate">Loading…</p> : (
        <table className="w-full border-collapse text-sm">
          <thead><tr><th className={TH}>Connector</th><th className={TH}>Category</th><th className={TH}>Egress</th><th className={TH}>State</th></tr></thead>
          <tbody>
            {conns.data?.map((c) => (
              <tr key={c.kind}>
                <td className={TD}>{c.display_name} <span className="text-xs text-slate/60">({c.kind})</span></td>
                <td className={TD}>{c.category}</td>
                <td className={TD}>{c.requires_egress ? <Badge tone="red">egress</Badge> : <Badge>local</Badge>}</td>
                <td className={TD}>{c.enabled ? <Badge tone="gold">enabled</Badge> : <Badge>dormant</Badge>}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}

// ── MCP servers (FEATURE B1) ────────────────────────────────────────────────
function McpServersSection() {
  const qc = useQueryClient();
  const servers = useAdminMcpServers();
  const { busy, run } = useBusy();
  const [slug, setSlug] = useState("");
  const [name, setName] = useState("");
  const [transport, setTransport] = useState<"stdio" | "http">("stdio");
  const [command, setCommand] = useState("");
  const [url, setUrl] = useState("");
  const [requiresEgress, setRequiresEgress] = useState(false);
  const [authType, setAuthType] = useState<McpAuthType>("none");
  const [authHeaderName, setAuthHeaderName] = useState("");
  const [authValue, setAuthValue] = useState("");

  const refresh = () => qc.invalidateQueries({ queryKey: ["admin-mcp-servers"] });
  const register = () =>
    run("Register", async () => {
      await registerMcpServer({
        slug: slug.trim(),
        name: name.trim() || slug.trim(),
        transport,
        command: transport === "stdio" ? command.trim().split(/\s+/).filter(Boolean) : undefined,
        url: transport === "http" ? url.trim() : undefined,
        requires_egress: transport === "http" ? requiresEgress : undefined,
        auth_type: transport === "http" ? authType : undefined,
        auth_header_name: transport === "http" && (authType === "api_key" || authType === "header") ? authHeaderName.trim() : undefined,
        auth_value: transport === "http" && authType !== "none" ? authValue : undefined,
      });
      setSlug(""); setName(""); setCommand(""); setUrl("");
      setRequiresEgress(false); setAuthType("none"); setAuthHeaderName(""); setAuthValue("");
      refresh();
    });
  const approve = (s: McpServer) =>
    run("Approve", async () => { await approveMcpServer(s.id); refresh(); }, "Server approved");
  const remove = async (s: McpServer) => {
    if (!(await confirmDialog({ title: `Delete MCP server '${s.slug}'?`, body: "Its tools are removed from agents and the connection is dropped.", danger: true, confirmLabel: "Delete" }))) return;
    run("Delete", async () => { await deleteMcpServer(s.id); refresh(); });
  };
  const statusTone = (s: string) => (s === "active" ? "green" : s === "quarantined" ? "red" : s === "unreachable" ? "red" : "slate");

  return (
    <div>
      <H1>MCP servers</H1>
      <p className="mb-1 text-xs text-slate/70">Plug client-internal MCP servers (filesystem, DB, internal APIs) into the agent loop. Admin-registered, allow-listed, sandboxed, audited.</p>
      <p className="mb-4 text-xs text-slate/60">A private HTTP endpoint must resolve to a private address (zero-egress). A <em>remote</em> server (tick “requires egress”) may reach a public HTTPS host (GitHub, Cloudflare, Context7) and authenticate with a bearer token / API key / custom header — the secret is stored encrypted and injected on every request; cloud-metadata and link-local hosts are always refused. Tools flow only once a super-admin enables MCP globally (Integrations → mcp). Side-effecting tools require per-call human approval; reconnect with a changed tool definition auto-quarantines (rug-pull defence). Grant access per principal under Sharing (resource type “MCP server”), then assign the server to an agent on its editor.</p>

      <div className="admin-card mb-4">
        <div className="admin-card-head"><h4>Register a server</h4></div>
        <div className="flex flex-wrap items-end gap-2">
          <input className={INPUT} placeholder="slug (no '__')" value={slug} onChange={(e) => setSlug(e.target.value)} />
          <input className={INPUT} placeholder="display name" value={name} onChange={(e) => setName(e.target.value)} />
          <Dropdown
            value={transport}
            onChange={setTransport}
            ariaLabel="Transport"
            options={[
              { value: "stdio", label: "stdio (spawn)" },
              { value: "http", label: "streamable-HTTP" },
            ]}
          />
          {transport === "stdio" ? (
            <input className={INPUT + " min-w-[20rem]"} placeholder="command e.g. npx -y @scope/server" value={command} onChange={(e) => setCommand(e.target.value)} />
          ) : (
            <input className={INPUT + " min-w-[20rem]"} placeholder={requiresEgress ? "https://mcp.context7.com/mcp (remote)" : "http://10.0.0.5:8931/mcp (private)"} value={url} onChange={(e) => setUrl(e.target.value)} />
          )}
          <button className={BTN} disabled={!!busy || !slug.trim()} onClick={register}>Register</button>
        </div>
        {transport === "http" && (
          <div className="mt-2 flex flex-wrap items-end gap-2">
            <label className="flex items-center gap-1 text-xs text-slate/80">
              <input type="checkbox" checked={requiresEgress} onChange={(e) => setRequiresEgress(e.target.checked)} />
              requires egress (remote/public https)
            </label>
            <Dropdown
              value={authType}
              onChange={setAuthType}
              ariaLabel="Auth type"
              options={[
                { value: "none", label: "no auth" },
                { value: "bearer", label: "bearer token" },
                { value: "api_key", label: "API key (header)" },
                { value: "header", label: "custom header" },
              ]}
            />
            {(authType === "api_key" || authType === "header") && (
              <input className={INPUT} placeholder="header name e.g. CONTEXT7_API_KEY" value={authHeaderName} onChange={(e) => setAuthHeaderName(e.target.value)} />
            )}
            {authType !== "none" && (
              <input className={INPUT + " min-w-[16rem]"} type="password" autoComplete="off" placeholder={authType === "bearer" ? "token (sent as 'Bearer …')" : "secret value"} value={authValue} onChange={(e) => setAuthValue(e.target.value)} />
            )}
          </div>
        )}
      </div>

      {servers.isLoading ? <p className="text-sm text-slate">Loading…</p> : (
        <table className="w-full border-collapse text-sm">
          <thead><tr><th className={TH}>Slug</th><th className={TH}>Transport</th><th className={TH}>Status</th><th className={TH}>Tools</th><th className={TH}>Live</th><th className={TH}></th></tr></thead>
          <tbody>
            {(servers.data ?? []).map((s) => (
              <tr key={s.id}>
                <td className={TD}>{s.slug} <span className="text-xs text-slate/60">{s.name}</span></td>
                <td className={TD}>
                  {s.transport}{s.url ? <span className="text-xs text-slate/60"> · {s.url}</span> : null}
                  {s.requires_egress ? <span className="ml-1"><Badge tone="gold">egress</Badge></span> : null}
                  {s.auth_type && s.auth_type !== "none" ? <span className="ml-1"><Badge>{s.auth_type === "bearer" ? "bearer" : s.auth_header_name || "auth"}</Badge></span> : null}
                </td>
                <td className={TD}><Badge tone={statusTone(s.status)}>{s.status}</Badge></td>
                <td className={TD}>{s.tool_count}</td>
                <td className={TD}>{s.connected ? <Badge tone="green">connected</Badge> : <Badge>—</Badge>}</td>
                <td className={TD}>
                  <button className={BTN2} disabled={!!busy} onClick={() => approve(s)}>{s.status === "active" ? "Re-pin" : s.status === "quarantined" ? "Re-approve" : "Approve"}</button>
                  <button className={BTN_DANGER + " ml-2"} disabled={!!busy} onClick={() => remove(s)}>Delete</button>
                </td>
              </tr>
            ))}
            {(!servers.data || servers.data.length === 0) && <tr><td className={TD} colSpan={6}>No MCP servers registered.</td></tr>}
          </tbody>
        </table>
      )}
    </div>
  );
}


// ── Config ────────────────────────────────────────────────────────────────────
// Human-friendly names + explanations for the technical runtime-config keys.
// Known runtime settings = the keys the backend actually reads via `runtime::get`,
// with their built-in defaults. Shown even when unset, so an admin can tune them
// before they're ever overridden. (Connector flags live in the Integrations tab;
// test.*/integration.* are filtered server-side.)
const KNOWN_SETTINGS: { key: string; label: string; desc: string; valueType: string; default: string }[] = [
  { key: "features.messaging", label: "Enable team chats & direct messages", desc: "Team/project group chats and 1:1 direct messages. Off hides the Teams and Direct messages nav and refuses the messaging endpoints. On by default.", valueType: "bool", default: "true" },
  { key: "features.workflows", label: "Enable workflows", desc: "Event-driven workflows engine — react to document, membership and directory events with agent runs or chat posts. Enabling starts dispatch from now on (existing backlog is not replayed). Off by default.", valueType: "bool", default: "false" },
  { key: "features.voice", label: "Enable voice (dictation + read-aloud)", desc: "Speech-to-text dictation into the composer and read-aloud of answers. Needs a Speech-to-text and/or Text-to-speech provider configured under Providers. Off by default.", valueType: "bool", default: "false" },
  { key: "features.voice_live", label: "Enable live voice (real-time call)", desc: "Real-time streaming voice conversation (streaming STT → LLM → streaming TTS) with barge-in. Needs voice on plus the streaming engine URLs configured. Absent streaming engines degrade to per-utterance batch. Off by default.", valueType: "bool", default: "false" },
  { key: "features.groundedness", label: "Enable groundedness verification", desc: "Post-answer faithfulness check against retrieved sources (and Verify-draft). Needs a Verifier provider configured under Providers. Off by default.", valueType: "bool", default: "false" },
  { key: "auth.allow_registration", label: "Allow new registrations", desc: "Let people self-register beyond the first account. Off by default — the first registrant becomes the admin, then registration is closed until you turn this on. Keep off for a solo or private deployment on a public IP.", valueType: "bool", default: "false" },
  { key: "automation.max_per_user", label: "Max automations per user", desc: "The most scheduled automations a single user may own.", valueType: "int", default: "50" },
  { key: "automation.min_interval_secs", label: "Minimum automation interval (seconds)", desc: "Shortest gap allowed between one automation's runs.", valueType: "int", default: "300" },
  { key: "audit.retention_months", label: "Audit retention (months)", desc: "How long audit-log partitions are kept before the retention job drops the oldest.", valueType: "int", default: "24" },
  // Web search (the connector on/off flag itself lives in the Integrations tab).
  { key: "web_search.allowlist", label: "Web search: domain allowlist", desc: "Comma-separated domain suffixes. Non-empty restricts fetching to these domains and their subdomains.", valueType: "string", default: "(off)" },
  { key: "web_search.blocklist", label: "Web search: domain blocklist", desc: "Comma-separated domain suffixes that are never searched or fetched. Wins over the allowlist.", valueType: "string", default: "(off)" },
  { key: "web_search.allowlist_only", label: "Web search: allowlist-only mode", desc: "Fail-closed: when true, ONLY allowlisted domains are reachable — true with an empty allowlist blocks all web fetching.", valueType: "bool", default: "false" },
  { key: "web_search.robots_policy", label: "Web search: robots.txt policy", desc: "user_triggered (default — single user-requested fetches proceed) or respect (honour robots.txt per host).", valueType: "string", default: "user_triggered" },
];


function ConfigSection() {
  const qc = useQueryClient();
  const { busy, run } = useBusy();
  const cfg = useAdminConfig();
  const [edits, setEdits] = useState<Record<string, string>>({});
  const refresh = () => qc.invalidateQueries({ queryKey: ["admin-config"] });

  const dbByKey = new Map((cfg.data ?? []).map((c) => [c.key, c]));
  const knownKeys = new Set(KNOWN_SETTINGS.map((s) => s.key));
  // Every known setting (DB value or default) + any extra DB rows not in the registry.
  const rows = [
    ...KNOWN_SETTINGS.map((s) => {
      const db = dbByKey.get(s.key);
      return { key: s.key, label: s.label, desc: s.desc, value_type: db?.value_type ?? s.valueType, scope: db?.scope ?? "global", current: db?.value ?? s.default, isSet: !!db };
    }),
    // Live-voice engine keys (voice.stt_*/tts_*/turn_detector_url, incl. the encrypted
    // API keys) are owned by the dedicated "Live voice" section — never surface them in
    // the generic editor (the *_api_key_enc values are ciphertext and must stay masked).
    ...(cfg.data ?? []).filter((c) => !knownKeys.has(c.key) && c.key !== "providers.user_byok_enabled"
      && !c.key.startsWith("voice.stt_") && !c.key.startsWith("voice.tts_") && c.key !== "voice.turn_detector_url"
    ).map((c) => ({ key: c.key, label: c.key, desc: "", value_type: c.value_type, scope: c.scope, current: c.value, isSet: true })),
  ];

  return (
    <div>
      <H1>Runtime config</H1>
      <p className="mb-4 text-xs text-slate/70">Live, audited tuning knobs the platform reads at request time. Each shows its current value, or the built-in <span className="text-slate">default</span> if never set. The grey monospace text is the raw key; edit and Save.</p>
      {cfg.isLoading ? <p className="text-sm text-slate">Loading…</p> : (
        <table className="w-full border-collapse text-sm">
          <thead><tr><th className={TH}>Setting</th><th className={TH}>Value</th><th className={TH}>Type</th><th className={TH}>Scope</th><th className={TH}></th></tr></thead>
          <tbody>
            {rows.map((r) => {
              const val = edits[r.key] ?? r.current;
              const dirty = r.key in edits && edits[r.key] !== r.current;
              return (
                <tr key={r.key}>
                  <td className={TD} style={{ maxWidth: 380 }}>
                    <div className="text-slate-lightest">{r.label}{!r.isSet && <span className="ml-2 rounded bg-navy-lighter px-1.5 py-0.5 text-[10px] uppercase tracking-wide text-slate/70">default</span>}</div>
                    <div className="font-mono text-[10px] text-slate/50">{r.key}</div>
                    {r.desc && <div className="mt-1 text-xs text-slate/70">{r.desc}</div>}
                  </td>
                  <td className={TD}>
                    {r.value_type === "bool" ? (
                      <Dropdown
                        value={val === "true" ? "true" : "false"}
                        fullWidth
                        ariaLabel={r.key}
                        onChange={(v) => setEdits((p) => ({ ...p, [r.key]: v }))}
                        options={[{ value: "true", label: "true" }, { value: "false", label: "false" }]}
                      />
                    ) : (
                      <input className={INPUT + " w-full"} value={val} onChange={(e) => setEdits((p) => ({ ...p, [r.key]: e.target.value }))} />
                    )}
                  </td>
                  <td className={TD}>{r.value_type}</td>
                  <td className={TD}>{r.scope}</td>
                  <td className={TD}><button className={BTN} disabled={!!busy || !dirty} onClick={() => run("Save", () => setConfig(r.key, { value: val, value_type: r.value_type, scope: r.scope }).then(() => { setEdits((p) => { const n = { ...p }; delete n[r.key]; return n; }); refresh(); }), "Setting saved.")}>Save</button></td>
                </tr>
              );
            })}
          </tbody>
        </table>
      )}
    </div>
  );
}

// ── Providers: deployment-scope LLM/embed/rerank/ocr/stt/tts/verify ────────────
const PROVIDER_ROLES: [string, string][] = [
  ["llm", "LLM (chat)"],
  ["embed", "Embeddings"],
  ["rerank", "Reranker"],
  ["ocr", "OCR"],
  ["stt", "Speech-to-text"],
  ["tts", "Text-to-speech"],
  ["verify", "Verifier"],
];

interface ProviderDraft { base_url: string; model: string; api_key: string; enabled: boolean; reasoning_mode: string }

// Operator override for the reasoning control (llm role). `auto` = detect from the
// provider/model; the rest force a specific control mode.
const REASONING_MODES: { value: string; label: string }[] = [
  { value: "auto", label: "Auto-detect" },
  { value: "none", label: "None (hidden)" },
  { value: "toggle", label: "Toggle (on/off)" },
  { value: "levels", label: "Levels" },
  { value: "budget", label: "Budget" },
  { value: "always_on", label: "Always on" },
];

const BYOK_KEY = "providers.user_byok_enabled";

// Inline result of a provider "Test connection" probe: ✓ latency / ✗ reason.
function ProviderTestStatus({ s }: { s: ProviderTestResult | "loading" | undefined }) {
  if (!s) return null;
  if (s === "loading") return <span className="text-xs text-slate">testing…</span>;
  if (s.ok) return <span className="text-xs" style={{ color: "#34d399" }}>✓ {Math.round(s.latency_ms)} ms{s.detail ? ` · ${s.detail}` : ""}</span>;
  return <span className="text-xs" style={{ color: "#f87171" }}>✗ {s.error ?? "failed"}</span>;
}

// One editable named LLM provider (create or edit form).
interface LlmDraft { label: string; base_url: string; model: string; api_key: string; enabled: boolean; reasoning_mode: string }
const blankLlm = (): LlmDraft => ({ label: "", base_url: "", model: "", api_key: "", enabled: true, reasoning_mode: "auto" });

function LlmProviderEditor({ draft, apiKeySet, onField, onSave, onCancel, onTest, test, saving }: {
  draft: LlmDraft; apiKeySet: boolean;
  onField: (k: keyof LlmDraft, v: string | boolean) => void;
  onSave: () => void; onCancel: () => void; onTest: () => void;
  test: ProviderTestResult | "loading" | undefined; saving: boolean;
}) {
  return (
    <div className="mb-2 rounded-lg border border-navy-lighter bg-navy-light/40 px-4 py-3">
      <div className="grid gap-2 sm:grid-cols-2">
        <label className="text-xs text-slate/70">Display name
          <input className={INPUT + " mt-1 w-full"} placeholder="e.g. Claude, GPT, Local vLLM" value={draft.label} onChange={(e) => onField("label", e.target.value)} />
        </label>
        <label className="text-xs text-slate/70">Model
          <input className={INPUT + " mt-1 w-full"} placeholder="(ML default)" value={draft.model} onChange={(e) => onField("model", e.target.value)} />
        </label>
        <label className="text-xs text-slate/70">Base URL
          <input className={INPUT + " mt-1 w-full"} placeholder="(ML default)" value={draft.base_url} onChange={(e) => onField("base_url", e.target.value)} />
        </label>
        <label className="text-xs text-slate/70">API key
          <input type="password" className={INPUT + " mt-1 w-full"} placeholder={apiKeySet ? "•••• set (blank = keep)" : "API key"} value={draft.api_key} onChange={(e) => onField("api_key", e.target.value)} />
        </label>
        <label className="text-xs text-slate/70">Reasoning
          <Dropdown value={draft.reasoning_mode} onChange={(v) => onField("reasoning_mode", v)} ariaLabel="Reasoning mode" fullWidth options={REASONING_MODES.map((m) => ({ value: m.value, label: m.label }))} />
        </label>
        <label className="mt-5 flex items-center gap-2 text-xs text-slate/70">
          <input type="checkbox" checked={draft.enabled} onChange={(e) => onField("enabled", e.target.checked)} /> Enabled
        </label>
      </div>
      <div className="mt-3 flex items-center gap-2">
        <button type="button" className={BTN} disabled={saving || !draft.label.trim()} onClick={onSave}>Save</button>
        <button type="button" className={BTN} onClick={onCancel}>Cancel</button>
        <button type="button" className={BTN} onClick={onTest}>Test</button>
        <ProviderTestStatus s={test} />
      </div>
    </div>
  );
}

// The LLM role as a LIST of named providers (multi-LLM). Members pick one per
// conversation in the composer; the starred row is the default fallback.
function LlmProvidersCard() {
  const qc = useQueryClient();
  const { busy, run } = useBusy();
  const q = useProviders();
  const rows = (q.data ?? []).filter((p) => p.role === "llm");
  const refresh = () => {
    qc.invalidateQueries({ queryKey: ["admin-providers"] });
    qc.invalidateQueries({ queryKey: ["whoami"] });
    qc.invalidateQueries({ queryKey: ["my-llm-providers"] });
  };
  const [edits, setEdits] = useState<Record<string, LlmDraft>>({});
  const [adding, setAdding] = useState<LlmDraft | null>(null);
  const [tests, setTests] = useState<Record<string, ProviderTestResult | "loading">>({});
  const editField = (id: string, k: keyof LlmDraft, v: string | boolean) =>
    setEdits((p) => ({ ...p, [id]: { ...p[id], [k]: v } }));
  const startEdit = (p: ProviderConfig) =>
    setEdits((e) => ({ ...e, [p.id]: { label: p.label ?? "", base_url: p.base_url ?? "", model: p.model ?? "", api_key: "", enabled: p.enabled, reasoning_mode: p.reasoning_mode ?? "auto" } }));
  const cancelEdit = (id: string) => setEdits((e) => { const n = { ...e }; delete n[id]; return n; });
  const toBody = (d: LlmDraft) => ({ label: d.label.trim(), base_url: d.base_url || undefined, model: d.model || undefined, api_key: d.api_key || undefined, enabled: d.enabled, reasoning_mode: d.reasoning_mode });
  const saveEdit = (id: string, d: LlmDraft) => run("Save", () => updateAdminLlm(id, toBody(d)).then(() => { cancelEdit(id); refresh(); }), "Provider saved.");
  const saveNew = (d: LlmDraft) => run("Create", () => createAdminLlm(toBody(d)).then(() => { setAdding(null); refresh(); }), "Provider added.");
  const del = async (p: ProviderConfig) => {
    if (await confirmDialog({ title: `Delete "${p.label ?? p.model ?? "provider"}"?`, body: "Chats using it fall back to the default.", danger: true, confirmLabel: "Delete" }))
      run("Delete", () => deleteAdminLlm(p.id).then(refresh), "Provider deleted.");
  };
  const makeDefault = (id: string) => run("Default", () => setAdminLlmDefault(id).then(refresh), "Default set.");
  const testRow = (id: string, d: LlmDraft, savedId?: string) => {
    setTests((t) => ({ ...t, [id]: "loading" }));
    testAdminLlm({ id: savedId, base_url: d.base_url || undefined, model: d.model || undefined, api_key: d.api_key || undefined, enabled: d.enabled })
      .then((r) => setTests((t) => ({ ...t, [id]: r })))
      .catch((e) => setTests((t) => ({ ...t, [id]: { ok: false, latency_ms: 0, error: e instanceof Error ? e.message : "failed" } })));
  };

  return (
    <div className="mb-6">
      <div className="mb-2 flex items-center justify-between">
        <h3 className="text-sm font-semibold text-slate-lightest">LLM providers (chat)</h3>
        {!adding && <button type="button" className={BTN} disabled={!!busy} onClick={() => setAdding(blankLlm())}>＋ Add LLM provider</button>}
      </div>
      <p className="mb-2 text-xs text-slate/70">Several named chat models. Members pick one per conversation in the composer; the <span className="text-slate">★ default</span> is used when a chat has no pick.</p>
      {adding && (
        <LlmProviderEditor draft={adding} apiKeySet={false}
          onField={(k, v) => setAdding((a) => (a ? { ...a, [k]: v } : a))}
          onSave={() => saveNew(adding)} onCancel={() => setAdding(null)}
          onTest={() => testRow("new", adding)} test={tests["new"]} saving={!!busy} />
      )}
      {q.isLoading ? <p className="text-sm text-slate">Loading…</p> : rows.length === 0 && !adding ? (
        <p className="text-xs text-slate/60">No LLM providers yet. Add one to enable chat.</p>
      ) : (
        <div className="space-y-1">
          {rows.map((p) => edits[p.id] ? (
            <LlmProviderEditor key={p.id} draft={edits[p.id]} apiKeySet={p.api_key_set}
              onField={(k, v) => editField(p.id, k, v)}
              onSave={() => saveEdit(p.id, edits[p.id])} onCancel={() => cancelEdit(p.id)}
              onTest={() => testRow(p.id, edits[p.id], p.id)} test={tests[p.id]} saving={!!busy} />
          ) : (
            <div key={p.id} className="flex items-center gap-3 rounded-lg border border-navy-lighter bg-navy-light/40 px-4 py-2.5 text-sm">
              <button type="button" title={p.is_default ? "Default provider" : "Make default"} disabled={!!busy || p.is_default} onClick={() => makeDefault(p.id)} className="text-base leading-none" style={{ color: p.is_default ? "#d1799a" : "#7b8494", cursor: p.is_default ? "default" : "pointer" }}>{p.is_default ? "★" : "☆"}</button>
              <div className="min-w-0 flex-1">
                <div className="truncate text-slate-lightest">{p.label ?? "(unnamed)"}{!p.enabled && <span className="ml-2 text-xs text-slate/50">(disabled)</span>}</div>
                <div className="truncate font-mono text-[10px] text-slate/50">{p.model ?? "(ML default)"}{p.base_url ? ` · ${p.base_url}` : ""}{p.api_key_set ? " · key set" : ""}</div>
              </div>
              <ProviderTestStatus s={tests[p.id]} />
              <button type="button" className={BTN} onClick={() => testRow(p.id, { label: p.label ?? "", base_url: p.base_url ?? "", model: p.model ?? "", api_key: "", enabled: p.enabled, reasoning_mode: p.reasoning_mode ?? "auto" }, p.id)}>Test</button>
              <button type="button" className={BTN} disabled={!!busy} onClick={() => startEdit(p)}>Edit</button>
              <button type="button" className={BTN_DANGER} disabled={!!busy} onClick={() => del(p)}>Delete</button>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

function ProvidersSection() {
  const qc = useQueryClient();
  const { busy, run } = useBusy();
  const q = useProviders();
  const cfg = useAdminConfig();
  const [edits, setEdits] = useState<Record<string, ProviderDraft>>({});
  const refresh = () => qc.invalidateQueries({ queryKey: ["admin-providers"] });
  const byRole = new Map((q.data ?? []).map((p) => [p.role, p]));
  // BYOK is on by default for public Core; an absent config row means the boot default (on).
  const byok = (cfg.data?.find((c) => c.key === BYOK_KEY)?.value ?? "true") === "true";
  const setByok = (on: boolean) =>
    run("Save", () => setConfig(BYOK_KEY, { value: on ? "true" : "false", value_type: "bool", scope: "global" })
      .then(() => qc.invalidateQueries({ queryKey: ["admin-config"] })), "Setting saved.");

  // An empty draft seeded from the saved row (the editing baseline before any keystroke).
  const blankDraft = (role: string): ProviderDraft => {
    const db = byRole.get(role);
    return { base_url: db?.base_url ?? "", model: db?.model ?? "", api_key: "", enabled: db?.enabled ?? true, reasoning_mode: db?.reasoning_mode ?? "auto" };
  };
  const draft = (role: string): ProviderDraft => edits[role] ?? blankDraft(role);
  // Derive the row's base from the updater's `prev` (NOT the outer-closure `draft`),
  // and spread `...p` so editing/saving one row never disturbs another's draft.
  const setField = (role: string, k: keyof ProviderDraft, v: string | boolean) =>
    setEdits((p) => ({ ...p, [role]: { ...(p[role] ?? blankDraft(role)), [k]: v } }));

  // Embedding-index provenance: drives the embed re-index warn modal + progress.
  const embIndex = useEmbeddingIndex();
  // `starting` bridges the ~5s gap between enqueue and the scheduler flipping the
  // status to `reindexing`, so the button can't be clicked twice and the bar shows.
  const [starting, setStarting] = useState(false);
  const startReindex = () => {
    setStarting(true);
    run("Re-index", () => reindexEmbeddings().then(() => qc.invalidateQueries({ queryKey: ["embedding-index"] })), "Re-index started.")
      .finally(() => window.setTimeout(() => setStarting(false), 12000));
  };
  // Once the job is actually running (or done), drop the local bridge flag.
  const embStatus = embIndex.data?.status;
  useEffect(() => {
    if (embStatus === "reindexing" || embStatus === "active") setStarting(false);
  }, [embStatus]);
  const reindexing = embStatus === "reindexing" || starting;
  // After saving the embed provider, if the backend says the embedding space changed,
  // offer the blue-green re-index (search keeps using the current model until done).
  const maybeOfferReindex = async (role: string, res: { reindex_required?: boolean; indexed_documents?: number }) => {
    if (role !== "embed" || !res.reindex_required) return;
    const n = res.indexed_documents ?? 0;
    const ok = await confirmDialog({
      title: "Re-index embeddings?",
      body: `This changes the embedding space. Your ${n} indexed document${n === 1 ? "" : "s"} must be re-indexed. Search keeps using the current model until re-indexing completes. Re-index may incur embedding-API cost.`,
      confirmLabel: "Re-index now",
    });
    if (ok) startReindex();
  };

  // Transient per-row "Saved ✓" flash (in addition to the toast), so the
  // confirmation is visible right at the row.
  const [saved, setSaved] = useState<Record<string, boolean>>({});
  const flashSaved = (role: string) => {
    setSaved((s) => ({ ...s, [role]: true }));
    window.setTimeout(() => setSaved((s) => { const n = { ...s }; delete n[role]; return n; }), 2500);
  };

  const [tests, setTests] = useState<Record<string, ProviderTestResult | "loading">>({});
  const runTest = (role: string, d: ProviderDraft) => {
    setTests((t) => ({ ...t, [role]: "loading" }));
    testProvider(role, { base_url: d.base_url || undefined, model: d.model || undefined, api_key: d.api_key || undefined, enabled: d.enabled })
      .then((r) => setTests((t) => ({ ...t, [role]: r })))
      .catch((e) => setTests((t) => ({ ...t, [role]: { ok: false, latency_ms: 0, error: e instanceof Error ? e.message : "failed" } })));
  };

  return (
    <div>
      <H1>Providers</H1>
      <p className="mb-4 text-xs text-slate/70">
        Point each role at a local engine or an external API (Claude / GPT / Gemini …), runtime, no restart.
        Leave a row blank to use the ML service&apos;s built-in default. API keys are <span className="text-slate">write-only</span> —
        stored encrypted, shown only as <span className="text-slate">•••• set</span>; leave the key blank to keep the current one.
      </p>
      <label className="mb-4 flex items-center justify-between rounded-lg border border-navy-lighter bg-navy-light/40 px-4 py-3 text-sm">
        <span>
          <span className="text-slate-lightest">Allow members to set their own API keys</span>
          <span className="mt-1 block text-xs text-slate/70">When on, members can store personal provider keys under their profile (BYOK). Off ⇒ everyone uses the deployment keys above.</span>
        </span>
        <input type="checkbox" checked={byok} disabled={!!busy || cfg.isLoading} onChange={(e) => setByok(e.target.checked)} />
      </label>
      {/* Embedding index status — search uses the ACTIVE model until a re-index swaps it. */}
      {embIndex.data?.seeded && (
        <div className="mb-4 rounded-lg border border-navy-lighter bg-navy-light/40 px-4 py-3 text-sm">
          <div className="text-slate-lightest">
            Embedding index: <span className="font-mono">{embIndex.data.embed_model}</span>
            <span className="text-slate/60"> · {embIndex.data.dim}-dim</span>
            {embIndex.data.status === "active" && !reindexing && <Badge tone="green" className="ml-2.5">active</Badge>}
            {reindexing && <Badge tone="gold" className="ml-2.5">re-indexing</Badge>}
            {embIndex.data.status === "failed" && <Badge tone="red" className="ml-2.5">failed</Badge>}
          </div>
          {/* In-flight (or just-triggered) → live progress bar; search stays on the old model. */}
          {reindexing && (() => {
            const done = embIndex.data.reindex_done ?? 0;
            const total = embIndex.data.reindex_total ?? 0;
            const pct = total > 0 ? Math.min(100, Math.round((done / total) * 100)) : null;
            return (
              <div className="mt-2">
                <div className="h-2 w-full overflow-hidden rounded-full bg-navy-lighter">
                  <div
                    className={"h-full rounded-full bg-gold transition-all duration-500" + (pct === null ? " animate-pulse w-1/3" : "")}
                    style={pct === null ? undefined : { width: `${pct}%` }}
                  />
                </div>
                <div className="mt-1 text-xs text-slate/70">
                  {pct === null ? "Starting re-index…" : `Re-embedding ${done.toLocaleString()} / ${total.toLocaleString()} (${pct}%)`}
                  {" · search still uses the current model."}
                </div>
              </div>
            );
          })()}
          {embIndex.data.status === "failed" && !reindexing && (
            <div className="mt-1 flex items-center gap-2 text-xs">
              <span className="text-urgency-red">Re-index failed{embIndex.data.error ? `: ${embIndex.data.error}` : ""}. Old index is intact.</span>
              <button type="button" className={BTN} disabled={!!busy} onClick={startReindex}>Retry</button>
            </div>
          )}
          {!reindexing && embIndex.data.desired_model && embIndex.data.status !== "failed" && (
            <div className="mt-1 flex items-center gap-2 text-xs">
              <span className="text-slate/70">Pending change → <span className="font-mono">{embIndex.data.desired_model}</span> ({embIndex.data.desired_dim}-dim). Search keeps using the current model until re-indexed.</span>
              <button type="button" className={BTN} disabled={!!busy} onClick={startReindex}>Re-index now</button>
            </div>
          )}
        </div>
      )}
      {/* LLM is a list of named providers (multi-LLM); the other roles stay single-row. */}
      <LlmProvidersCard />
      {q.isLoading ? <p className="text-sm text-slate">Loading…</p> : (
        <table className="w-full border-collapse text-sm">
          <thead><tr><th className={TH}>Role</th><th className={TH}>Base URL</th><th className={TH}>Model</th><th className={TH}>API key</th><th className={TH}>Reasoning</th><th className={TH}>Enabled</th><th className={TH}>Test</th><th className={TH}></th></tr></thead>
          <tbody>
            {PROVIDER_ROLES.filter(([role]) => role !== "llm").map(([role, label]) => {
              const db = byRole.get(role);
              const d = draft(role);
              const dirty = role in edits;
              return (
                <tr key={role}>
                  <td className={TD}><div className="text-slate-lightest">{label}</div><div className="font-mono text-[10px] text-slate/50">{role}</div>{role === "embed" && <div className="text-[10px] text-slate/50">deployment-wide; not per-user</div>}</td>
                  <td className={TD}><input className={INPUT + " w-full"} placeholder="(ML default)" value={d.base_url} onChange={(e) => setField(role, "base_url", e.target.value)} /></td>
                  <td className={TD}><input className={INPUT + " w-full"} placeholder="(ML default)" value={d.model} onChange={(e) => setField(role, "model", e.target.value)} /></td>
                  <td className={TD}><input type="password" className={INPUT + " w-full"} placeholder={db?.api_key_set ? "•••• set (blank = keep)" : "API key"} value={d.api_key} onChange={(e) => setField(role, "api_key", e.target.value)} /></td>
                  <td className={TD}>{role === "llm" ? (
                    <Dropdown
                      value={d.reasoning_mode}
                      onChange={(v) => setField(role, "reasoning_mode", v)}
                      ariaLabel="Reasoning mode"
                      fullWidth
                      options={REASONING_MODES.map((m) => ({ value: m.value, label: m.label }))}
                    />
                  ) : <span className="text-slate/40">—</span>}</td>
                  <td className={TD}><input type="checkbox" checked={d.enabled} onChange={(e) => setField(role, "enabled", e.target.checked)} /></td>
                  <td className={TD}><div className="flex items-center gap-2"><button type="button" className={BTN} onClick={() => runTest(role, d)}>Test</button><ProviderTestStatus s={tests[role]} /></div></td>
                  <td className={TD}><div className="flex items-center gap-2"><button type="button" className={BTN} disabled={!!busy || !dirty} onClick={() => run("Save", () => setProvider(role, { base_url: d.base_url || undefined, model: d.model || undefined, api_key: d.api_key || undefined, enabled: d.enabled, reasoning_mode: d.reasoning_mode }).then(async (res) => { setEdits((p) => { const n = { ...p }; delete n[role]; return n; }); refresh(); qc.invalidateQueries({ queryKey: ["whoami"] }); flashSaved(role); await maybeOfferReindex(role, res); }), "Provider saved.")}>Save</button>{saved[role] && <span className="text-xs text-green-400">Saved ✓</span>}</div></td>
                </tr>
              );
            })}
          </tbody>
        </table>
      )}
    </div>
  );
}

// ── Announcements: banners + login welcome message ─────────────────────────────
const SEVERITIES: Severity[] = ["info", "success", "warning", "error"];
const sevTone = (s: Severity): "slate" | "gold" | "red" | "green" =>
  s === "error" ? "red" : s === "warning" ? "gold" : s === "success" ? "green" : "slate";

function AnnouncementsSection() {
  const qc = useQueryClient();
  const { busy, run } = useBusy();
  const list = useAdminAnnouncements();
  const welcome = useAdminWelcome();
  const refresh = () => {
    qc.invalidateQueries({ queryKey: ["admin-announcements"] });
    qc.invalidateQueries({ queryKey: ["admin-welcome"] });
    qc.invalidateQueries({ queryKey: ["notices"] });
  };

  // Shared banner form — used for both adding and editing (editId set = edit mode).
  const [editId, setEditId] = useState<string | null>(null);
  const [content, setContent] = useState("");
  const [severity, setSeverity] = useState<Severity>("info");
  const [dismissible, setDismissible] = useState(true);
  const resetForm = () => { setEditId(null); setContent(""); setSeverity("info"); setDismissible(true); };
  const startEdit = (a: Announcement) => { setEditId(a.id); setContent(a.content); setSeverity(a.severity); setDismissible(a.dismissible); };
  const submit = () =>
    run(
      editId ? "Save banner" : "Add banner",
      () =>
        (editId
          ? updateAnnouncement(editId, { content, severity, dismissible })
          : createAnnouncement({ content, severity, dismissible })
        ).then(() => { resetForm(); refresh(); }),
      editId ? "Banner saved." : "Banner added.",
    );

  // Welcome form, seeded once the query resolves.
  const [w, setW] = useState<WelcomeMessage | null>(null);
  const wv = w ?? welcome.data ?? null;
  const setWf = <K extends keyof WelcomeMessage>(k: K, v: WelcomeMessage[K]) =>
    setW({ ...(wv as WelcomeMessage), [k]: v });

  return (
    <div className="space-y-8">
      <div>
        <H1>Announcements</H1>
        <p className="mb-4 text-xs text-slate/70">
          Banners show to every user in a top-right corner stack, in every section, until dismissed.
          Markdown is supported. Changes appear live for all signed-in users.
        </p>

        <div className="mb-4 space-y-3 rounded-lg border border-navy-lighter bg-navy-light/40 p-4">
          <div>
            <label className={LABEL}>{editId ? "Edit banner (markdown)" : "New banner (markdown)"}</label>
            <textarea className={INPUT + " w-full"} rows={2} value={content}
              placeholder="e.g. **Scheduled maintenance** tonight 22:00–23:00 UTC."
              onChange={(e) => setContent(e.target.value)} />
          </div>
          <div className="flex flex-wrap items-end gap-3">
            <div>
              <label className={LABEL}>Severity</label>
              <Dropdown
                value={severity}
                onChange={(v) => setSeverity(v as Severity)}
                ariaLabel="Severity"
                options={SEVERITIES.map((s) => ({ value: s, label: s }))}
              />
            </div>
            <label className="flex items-center gap-2 text-sm text-slate-lightest">
              <input type="checkbox" checked={dismissible} onChange={(e) => setDismissible(e.target.checked)} />
              Dismissible
            </label>
            <button className={BTN} disabled={!!busy || !content.trim()} onClick={submit}>
              {editId ? "Save changes" : "Add banner"}
            </button>
            {editId && <button className={BTN2} disabled={!!busy} onClick={resetForm}>Cancel</button>}
          </div>
        </div>

        {list.isLoading ? <p className="text-sm text-slate">Loading…</p> : (
          <table className="w-full border-collapse text-sm">
            <thead><tr>
              <th className={TH}>Content</th><th className={TH}>Severity</th>
              <th className={TH}>Dismissible</th><th className={TH}>Status</th><th className={TH}></th>
            </tr></thead>
            <tbody>
              {(list.data ?? []).map((a) => (
                <tr key={a.id}>
                  <td className={TD} style={{ maxWidth: 420 }}><div className="whitespace-pre-wrap break-words">{a.content}</div></td>
                  <td className={TD}><Badge tone={sevTone(a.severity)}>{a.severity}</Badge></td>
                  <td className={TD}>{a.dismissible ? "yes" : "no"}</td>
                  <td className={TD}>{a.active ? <Badge tone="green">active</Badge> : <Badge tone="slate">hidden</Badge>}</td>
                  <td className={TD}>
                    <div className="flex flex-wrap gap-2">
                      <button className={BTN2} disabled={!!busy} onClick={() => startEdit(a)}>Edit</button>
                      <button className={BTN2} disabled={!!busy} onClick={() => run("Toggle", () => updateAnnouncement(a.id, { active: !a.active }).then(refresh), a.active ? "Banner hidden." : "Banner shown.")}>{a.active ? "Hide" : "Show"}</button>
                      <button className={BTN_DANGER} disabled={!!busy} onClick={async () => { if (await confirmDialog({ title: "Delete this banner?", danger: true })) run("Delete", () => deleteAnnouncement(a.id).then(() => { if (editId === a.id) resetForm(); refresh(); }), "Banner deleted."); }}>Delete</button>
                    </div>
                  </td>
                </tr>
              ))}
              {(list.data ?? []).length === 0 && <tr><td className={TD} colSpan={5}>No banners.</td></tr>}
            </tbody>
          </table>
        )}
      </div>

      <div>
        <h3 className="mb-1 font-serif text-lg text-slate-lightest">Welcome message</h3>
        <p className="mb-3 text-sm text-slate">Shown once per new login session as a modal. Markdown is supported. Requires a title and body when enabled.</p>
        {welcome.isLoading || !wv ? <p className="text-sm text-slate">Loading…</p> : (
          <div className="max-w-2xl space-y-3">
            <label className="flex items-center gap-2 text-sm text-slate-lightest">
              <input type="checkbox" checked={wv.enabled} onChange={(e) => setWf("enabled", e.target.checked)} />
              Enabled
            </label>
            <div>
              <label className={LABEL}>Title</label>
              <input className={INPUT + " w-full"} value={wv.title} onChange={(e) => setWf("title", e.target.value)} />
            </div>
            <div>
              <label className={LABEL}>Body (markdown)</label>
              <textarea className={INPUT + " w-full"} rows={5} value={wv.body} onChange={(e) => setWf("body", e.target.value)} />
            </div>
            <button className={BTN} disabled={!!busy} onClick={() => run("Save welcome", () => setWelcome(wv).then(() => { setW(null); refresh(); }), "Welcome message saved.")}>Save welcome</button>
          </div>
        )}
      </div>
    </div>
  );
}


// ── System ────────────────────────────────────────────────────────────────────
function SystemSection() {
  const ready = useReadiness();
  const who = useWhoami();
  const anomalies = useAnomalies();
  const dot = (ok: boolean) => (ok ? <Badge tone="green">up</Badge> : <Badge tone="red">down</Badge>);
  const flagged = anomalies.data ?? [];
  return (
    <div>
      <H1>System status</H1>
      <div className="mb-6 grid max-w-md gap-3">
        <Row label="Postgres">{ready.data?.checks ? dot(ready.data.checks.postgres) : "—"}</Row>
        <Row label="Redis">{ready.data?.checks ? dot(ready.data.checks.redis) : "—"}</Row>
        <Row label="Readiness">{ready.data?.status === "ready" ? <Badge tone="green">ready</Badge> : <Badge tone="red">{ready.data?.status ?? "—"}</Badge>}</Row>
        <Row label="Code interpreter">{who.data?.capabilities.code_interpreter ? <Badge tone="gold">enabled</Badge> : <Badge>off</Badge>}</Row>
        <Row label="Voice">{who.data?.capabilities.voice ? <Badge tone="gold">enabled</Badge> : <Badge>off</Badge>}</Row>
        <Row label="Your role">{<Badge tone="gold">{who.data?.role}</Badge>}{who.data?.break_glass && <span className="ml-2 text-xs text-urgency-red">break-glass</span>}</Row>
      </div>

      <div className="mb-2 flex items-center gap-2 text-xs uppercase tracking-[0.14em] text-slate">
        Security alerts
        {flagged.length > 0 && <Badge tone="red">{flagged.length}</Badge>}
      </div>
      {flagged.length === 0 ? (
        <p className="text-sm text-slate/70">No flagged events.</p>
      ) : (
        <table className="w-full border-collapse text-sm">
          <thead><tr><th className={TH}>When</th><th className={TH}>Action</th><th className={TH}>Role</th><th className={TH}>Resource</th></tr></thead>
          <tbody>
            {flagged.map((e) => (
              <tr key={e.seq}>
                <td className={TD}>{new Date(e.occurred_at).toLocaleString()}</td>
                <td className={TD}>{e.action_type}</td>
                <td className={TD}>{e.actor_role}</td>
                <td className={TD}>{e.resource_type ?? "—"}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex items-center justify-between rounded-lg border border-navy-lighter bg-navy-light/40 px-4 py-3 text-sm">
      <span className="text-slate">{label}</span>
      <span className="text-slate-lightest">{children}</span>
    </div>
  );
}

// ── Admin-section registrations ─────────────────────────────────────────────
// Core registers the host sections through the extension registry, in the order the
// tab strip shows them. The Enterprise edition registers its own sections (audit,
// holds, moderation, branding) through the same registry, without editing a Core
// screen.
const UsersSectionForSelf = () => {
  const who = useWhoami();
  return <UsersSection selfId={who.data?.user_id} />;
};
const WorkflowsSection = () => <Workflows showOwner />;

// Live-voice engine config. One form: pick the STT engine
// (Off / local WebSocket / OpenAI Realtime) and TTS engine, set URLs/models/keys.
// Keys are write-only (masked); saving applies at runtime (next call re-resolves).
const STT_KIND_OPTS: { value: string; label: string }[] = [
  { value: "none", label: "Off (batch fallback)" },
  { value: "websocket", label: "Local (WebSocket)" },
  { value: "openai_realtime", label: "OpenAI Realtime" },
];
function VoiceLiveSection() {
  const qc = useQueryClient();
  const { busy, run } = useBusy();
  const q = useVoiceLive();
  const [edits, setEdits] = useState<Record<string, string | boolean>>({});
  if (q.isLoading || !q.data) return <p className="text-sm text-slate">Loading…</p>;
  const d = q.data;
  const sv = (k: string, dflt: string): string => (edits[k] as string) ?? dflt;
  const bv = (k: string, dflt: boolean): boolean => (edits[k] as boolean) ?? dflt;
  const setF = (k: string, v: string | boolean) => setEdits((p) => ({ ...p, [k]: v }));
  const kind = sv("stt_stream_kind", d.stt_stream_kind);
  const ttsOn = bv("tts_stream", d.tts_stream);
  const save = () =>
    run(
      "Save",
      () => {
        const body: VoiceLiveBody = {
          stt_stream_kind: kind,
          stt_stream_url: sv("stt_stream_url", d.stt_stream_url),
          stt_model: sv("stt_model", d.stt_model),
          dictation_model: sv("dictation_model", d.dictation_model),
          stt_language: sv("stt_language", d.stt_language),
          stt_sample_rate: Number(sv("stt_sample_rate", String(d.stt_sample_rate))) || 16000,
          tts_stream: ttsOn,
          tts_stream_url: sv("tts_stream_url", d.tts_stream_url),
          tts_model: sv("tts_model", d.tts_model),
          tts_voice: sv("tts_voice", d.tts_voice),
          turn_detector_url: sv("turn_detector_url", d.turn_detector_url),
          stt_api_key: (edits.stt_api_key as string) || undefined,
          tts_api_key: (edits.tts_api_key as string) || undefined,
        };
        return setVoiceLive(body).then(() => {
          setEdits({});
          qc.invalidateQueries({ queryKey: ["admin-voice-live"] });
        });
      },
      "Live voice saved.",
    );
  return (
    <div>
      <H1>Live voice</H1>
      <p className="mb-4 text-xs text-slate/70">
        Choose the streaming STT/TTS engines for live voice — local in-perimeter engines or a cloud API.
        Applies at runtime (no restart). API keys are <span className="text-slate">write-only</span> — stored encrypted,
        shown only as <span className="text-slate">•••• set</span>; leave blank to keep the current one.
      </p>

      <h3 className="mb-2 font-serif text-lg text-slate-lightest">Speech-to-text</h3>
      <div className="mb-6 max-w-2xl space-y-3">
        <label className="block text-sm text-slate-lightest">Engine
          <div className="mt-1">
            <Dropdown
              value={kind}
              onChange={(v) => setF("stt_stream_kind", v)}
              ariaLabel="Streaming STT engine"
              fullWidth
              options={STT_KIND_OPTS.map((o) => ({ value: o.value, label: o.label }))}
            />
          </div>
        </label>
        {kind === "websocket" && (
          <label className="block text-sm text-slate-lightest">Engine URL (ws://)
            <input className={INPUT + " mt-1 w-full"} placeholder="ws://localhost:6006" value={sv("stt_stream_url", d.stt_stream_url)} onChange={(e) => setF("stt_stream_url", e.target.value)} />
          </label>
        )}
        {kind === "openai_realtime" && (
          <>
            <label className="block text-sm text-slate-lightest">Model
              <input className={INPUT + " mt-1 w-full"} placeholder="gpt-4o-mini-transcribe" value={sv("stt_model", d.stt_model)} onChange={(e) => setF("stt_model", e.target.value)} />
              <span className="mt-1 block text-[11px] text-slate/50">OpenAI Realtime transcription model: gpt-4o-mini-transcribe / gpt-4o-transcribe / whisper-1. Speech-to-speech models (gpt-realtime-2) are NOT valid here. Endpoint wss://api.openai.com/v1/realtime.</span>
            </label>
            <label className="block text-sm text-slate-lightest">Dictation model
              <input className={INPUT + " mt-1 w-full"} placeholder="gpt-realtime-whisper" value={sv("dictation_model", d.dictation_model)} onChange={(e) => setF("dictation_model", e.target.value)} />
              <span className="mt-1 block text-[11px] text-slate/50">Composer-mic dictation uses this model under server VAD (live text-while-speaking). gpt-realtime-whisper streams partials as you talk. Shares the engine URL/key above.</span>
            </label>
            <label className="block text-sm text-slate-lightest">Language
              <input className={INPUT + " mt-1 w-full"} placeholder="en" value={sv("stt_language", d.stt_language)} onChange={(e) => setF("stt_language", e.target.value)} />
            </label>
            <label className="block text-sm text-slate-lightest">API key
              <input type="password" className={INPUT + " mt-1 w-full"} placeholder={d.stt_api_key_set ? "•••• set (blank = keep)" : "API key"} value={(edits.stt_api_key as string) ?? ""} onChange={(e) => setF("stt_api_key", e.target.value)} />
            </label>
          </>
        )}
        {kind !== "none" && (
          <label className="block text-sm text-slate-lightest">Capture sample rate (Hz)
            <input className={INPUT + " mt-1 w-full"} value={sv("stt_sample_rate", String(d.stt_sample_rate))} onChange={(e) => setF("stt_sample_rate", e.target.value)} />
          </label>
        )}
      </div>

      <h3 className="mb-2 font-serif text-lg text-slate-lightest">Text-to-speech</h3>
      <div className="mb-6 max-w-2xl space-y-3">
        <label className="flex items-center gap-2 text-sm text-slate-lightest">
          <input type="checkbox" checked={ttsOn} onChange={(e) => setF("tts_stream", e.target.checked)} /> Stream TTS (else per-clause batch)
        </label>
        {ttsOn && (
          <>
            <label className="block text-sm text-slate-lightest">Engine URL
              <input className={INPUT + " mt-1 w-full"} placeholder="http://localhost:8880  or  https://api.openai.com/v1" value={sv("tts_stream_url", d.tts_stream_url)} onChange={(e) => setF("tts_stream_url", e.target.value)} />
            </label>
            <label className="block text-sm text-slate-lightest">Model
              <input className={INPUT + " mt-1 w-full"} placeholder="kokoro  or  gpt-4o-mini-tts" value={sv("tts_model", d.tts_model)} onChange={(e) => setF("tts_model", e.target.value)} />
            </label>
            <label className="block text-sm text-slate-lightest">Voice
              <input className={INPUT + " mt-1 w-full"} placeholder="alloy (OpenAI)  or  af_sky (kokoro)" value={sv("tts_voice", d.tts_voice)} onChange={(e) => setF("tts_voice", e.target.value)} />
              <span className="mt-1 block text-[11px] text-slate/50">OpenAI needs a valid voice (alloy, nova, shimmer…); blank defaults to alloy on OpenAI.</span>
            </label>
            <label className="block text-sm text-slate-lightest">API key (cloud only)
              <input type="password" className={INPUT + " mt-1 w-full"} placeholder={d.tts_api_key_set ? "•••• set (blank = keep)" : "API key"} value={(edits.tts_api_key as string) ?? ""} onChange={(e) => setF("tts_api_key", e.target.value)} />
            </label>
          </>
        )}
      </div>

      <h3 className="mb-2 font-serif text-lg text-slate-lightest">Turn detection</h3>
      <div className="mb-6 max-w-2xl space-y-3">
        <label className="block text-sm text-slate-lightest">Turn-detector sidecar URL (optional)
          <input className={INPUT + " mt-1 w-full"} placeholder="http://localhost:8400" value={sv("turn_detector_url", d.turn_detector_url)} onChange={(e) => setF("turn_detector_url", e.target.value)} />
        </label>
      </div>

      <button type="button" className={BTN} disabled={!!busy || Object.keys(edits).length === 0} onClick={save}>Save</button>
    </div>
  );
}

// ── Tools ───────────────────────────────────────────────────────────────────
function ToolsSection() {
  const qc = useQueryClient();
  const cat = useToolCatalog();
  const { busy, run } = useBusy();
  const [tab, setTab] = useState<"native" | "custom" | "mcp">("native");
  const [editing, setEditing] = useState<string | null>(null);
  const [draft, setDraft] = useState("");

  const refresh = () => qc.invalidateQueries({ queryKey: ["tools", "catalog"] });
  const toggle = (t: NativeToolEntry) =>
    run("Toggle", async () => {
      // Preserve any existing description override when flipping the switch.
      await putNativeToolOverride(t.name, {
        enabled: !t.enabled,
        description_override: t.has_override ? t.description : null,
      });
      refresh();
    });
  const saveDesc = (t: NativeToolEntry) =>
    run("Save", async () => {
      await putNativeToolOverride(t.name, {
        enabled: t.enabled,
        description_override: draft.trim() ? draft.trim() : null,
      });
      setEditing(null);
      refresh();
    }, "Description saved");
  const reset = (t: NativeToolEntry) =>
    run("Reset", async () => { await resetNativeTool(t.name); setEditing(null); refresh(); }, "Reset to default");
  const startEdit = (t: NativeToolEntry) => { setEditing(t.name); setDraft(t.description); };

  return (
    <div>
      <H1>Tools</H1>
      <p className="mb-1 text-xs text-slate/70">The tool catalogue advertised to agents. Switch a native tool off to drop it from every agent's toolset, or edit the description the model reads — real behaviour customisation without a code change. Register custom HTTP/script tools under Custom; MCP tools are managed in their own tab.</p>
      <div className="my-4 flex gap-2">
        <button className={tab === "native" ? BTN : BTN2} onClick={() => setTab("native")}>Native</button>
        <button className={tab === "custom" ? BTN : BTN2} onClick={() => setTab("custom")}>Custom</button>
        <button className={tab === "mcp" ? BTN : BTN2} onClick={() => setTab("mcp")}>MCP</button>
      </div>

      {cat.isLoading && <p className="text-sm text-slate">Loading…</p>}

      {tab === "custom" && !cat.isLoading && <CustomToolsPanel tools={cat.data?.custom ?? []} onChange={refresh} />}

      {tab === "native" && !cat.isLoading && (
        <table className="w-full border-collapse text-sm">
          <thead><tr><th className={TH}>Tool</th><th className={TH}>Badges</th><th className={TH}>State</th><th className={TH}></th></tr></thead>
          <tbody>
            {(cat.data?.native ?? []).map((t) => (
              <Fragment key={t.name}>
                <tr>
                  <td className={TD}>{t.label} <span className="text-xs text-slate/60">({t.name})</span></td>
                  <td className={TD}>
                    <Badge tone={t.effect === "approval" ? "gold" : "slate"}>{t.effect}</Badge>
                    {t.egress && <span className="ml-1"><Badge tone="red">egress</Badge></span>}
                    {t.capability && <span className="ml-1"><Badge>host cap</Badge></span>}
                    {t.default && <span className="ml-1"><Badge>always on</Badge></span>}
                    {t.has_override && <span className="ml-1"><Badge tone="gold">overridden</Badge></span>}
                  </td>
                  <td className={TD}>{t.enabled ? <Badge tone="green">enabled</Badge> : <Badge tone="red">off</Badge>}</td>
                  <td className={TD}>
                    <button className={BTN2} disabled={!!busy || !!t.default} onClick={() => toggle(t)}>{t.enabled ? "Disable" : "Enable"}</button>
                    <button className={BTN2 + " ml-2"} disabled={!!busy} onClick={() => (editing === t.name ? setEditing(null) : startEdit(t))}>{editing === t.name ? "Close" : "Edit description"}</button>
                    {t.has_override && <button className={BTN_DANGER + " ml-2"} disabled={!!busy} onClick={() => reset(t)}>Reset</button>}
                  </td>
                </tr>
                {editing === t.name && (
                  <tr>
                    <td className={TD} colSpan={4}>
                      <label className={LABEL}>Description the LLM sees</label>
                      <textarea className={INPUT + " w-full"} rows={4} value={draft} onChange={(e) => setDraft(e.target.value)} />
                      <p className="mt-1 text-xs text-slate/60">Code default: {t.default_description}</p>
                      <div className="mt-2">
                        <button className={BTN} disabled={!!busy} onClick={() => saveDesc(t)}>Save</button>
                        <button className={BTN2 + " ml-2"} disabled={!!busy} onClick={() => setEditing(null)}>Cancel</button>
                      </div>
                    </td>
                  </tr>
                )}
              </Fragment>
            ))}
          </tbody>
        </table>
      )}

      {tab === "mcp" && !cat.isLoading && (
        <div>
          <p className="mb-2 text-xs text-slate/70">Active MCP servers (read-only). Register, approve and remove them in the <strong>MCP Servers</strong> tab.</p>
          {(cat.data?.mcp ?? []).length === 0 ? <p className="text-sm text-slate">No active MCP servers.</p> : (
            <table className="w-full border-collapse text-sm">
              <thead><tr><th className={TH}>Server</th><th className={TH}>Slug</th><th className={TH}>Tools</th><th className={TH}>Egress</th></tr></thead>
              <tbody>
                {(cat.data?.mcp ?? []).map((m) => (
                  <tr key={m.slug}>
                    <td className={TD}>{m.name || m.slug}</td>
                    <td className={TD}>{m.slug}</td>
                    <td className={TD}>{m.tool_count}</td>
                    <td className={TD}>{m.requires_egress ? <Badge tone="gold">egress</Badge> : <Badge>local</Badge>}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
      )}
    </div>
  );
}

const BLANK_CUSTOM: CustomToolInput = {
  name: "",
  display_name: "",
  description: "",
  kind: "http",
  params_schema: { type: "object", properties: {} },
  config: { method: "GET", url: "", headers: {}, response: { mode: "raw" } },
  requires_egress: true,
  side_effecting: true,
  timeout_secs: 30,
};

function CustomToolsPanel({ tools, onChange }: { tools: CustomToolEntry[]; onChange: () => void }) {
  const { busy, run } = useBusy();
  const [editId, setEditId] = useState<string | null>(null); // null = not editing; "" = new
  const [name, setName] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [description, setDescription] = useState("");
  const [kind, setKind] = useState<"http" | "script">("http");
  const [requiresEgress, setRequiresEgress] = useState(true);
  const [sideEffecting, setSideEffecting] = useState(true);
  const [timeout, setTimeoutSecs] = useState("30");
  const [schemaText, setSchemaText] = useState(JSON.stringify(BLANK_CUSTOM.params_schema, null, 2));
  const [configText, setConfigText] = useState(JSON.stringify(BLANK_CUSTOM.config, null, 2));
  const [sourceText, setSourceText] = useState("import json\nargs = json.load(open('params.json'))\nprint('hello', args)\n");
  const [authValue, setAuthValue] = useState("");
  const [testArgs, setTestArgs] = useState<Record<string, string>>({});
  const [testResult, setTestResult] = useState<string | null>(null);

  const startNew = () => {
    setEditId("");
    setName(""); setDisplayName(""); setDescription(""); setKind("http");
    setRequiresEgress(true); setSideEffecting(true); setTimeoutSecs("30");
    setSchemaText(JSON.stringify(BLANK_CUSTOM.params_schema, null, 2));
    setConfigText(JSON.stringify(BLANK_CUSTOM.config, null, 2));
    setSourceText("import json\nargs = json.load(open('params.json'))\nprint('hello', args)\n");
    setAuthValue("");
  };
  const startEdit = (t: CustomToolEntry) => {
    setEditId(t.id);
    setName(t.name); setDisplayName(t.display_name); setDescription(t.description); setKind(t.kind);
    setRequiresEgress(t.requires_egress); setSideEffecting(t.side_effecting);
    setTimeoutSecs(t.timeout_secs != null ? String(t.timeout_secs) : "");
    setSchemaText(JSON.stringify(t.params_schema, null, 2));
    setConfigText(JSON.stringify(t.config, null, 2));
    setSourceText((t.config as { source?: string } | null)?.source ?? "");
    setAuthValue(""); // never prefilled; blank = keep existing secret
  };
  const close = () => { setEditId(null); setTestResult(null); setTestArgs({}); };

  const save = () =>
    run("Save", async () => {
      let params_schema: unknown;
      try { params_schema = JSON.parse(schemaText); } catch { throw new Error("params schema is not valid JSON"); }
      let config: unknown;
      if (kind === "script") {
        config = { source: sourceText };
      } else {
        try { config = JSON.parse(configText); } catch { throw new Error("config is not valid JSON"); }
      }
      const body: CustomToolInput = {
        name: name.trim(),
        display_name: displayName.trim() || name.trim(),
        description: description.trim(),
        kind,
        params_schema,
        config,
        requires_egress: kind === "script" ? false : requiresEgress,
        side_effecting: sideEffecting,
        timeout_secs: timeout.trim() ? Number(timeout) : null,
        ...(authValue && kind === "http" ? { auth_value: authValue } : {}),
      };
      if (editId) await updateCustomTool(editId, body);
      else await createCustomTool(body);
      close();
      onChange();
    }, editId ? "Saved (re-approve to enable)" : "Created");

  const enable = (t: CustomToolEntry) => run("Enable", async () => { await enableCustomTool(t.id); onChange(); }, "Approved & enabled");
  const disable = (t: CustomToolEntry) => run("Disable", async () => { await disableCustomTool(t.id); onChange(); });
  const remove = async (t: CustomToolEntry) => {
    if (!(await confirmDialog({ title: `Delete custom tool '${t.name}'?`, body: "It is removed from every agent and deleted.", danger: true, confirmLabel: "Delete" }))) return;
    run("Delete", async () => { await deleteCustomTool(t.id); if (editId === t.id) close(); onChange(); });
  };
  const testRun = (t: CustomToolEntry) =>
    run("Test", async () => {
      const r = await testRunCustomTool(t.id, testArgs);
      setTestResult(r.result);
    });

  // The parameter names to offer in the Test-run form (from the tool's schema).
  const paramNames = (t: CustomToolEntry): string[] => {
    const props = (t.params_schema as { properties?: Record<string, unknown> } | null)?.properties;
    return props ? Object.keys(props) : [];
  };

  return (
    <div>
      <p className="mb-1 text-xs text-slate/70">Custom tools an agent can call. An <strong>HTTP</strong> tool is a declarative call — define a URL template with <code>{"{{param}}"}</code> placeholders. A <strong>script</strong> tool runs Python in the zero-network sandbox (parameters arrive as a <code>params.json</code> file; its stdout is returned). Editing bumps the version and requires re-approval (a running agent never silently calls a changed tool).</p>
      <p className="mb-3 text-xs text-slate/60">HTTP calls pass the zero-egress gate + the same SSRF checks as MCP (enable Integrations → custom_tool). Script tools need the code-interpreter capability (a Linux host).</p>

      <button className={BTN} disabled={!!busy} onClick={startNew}>New custom tool</button>

      {tools.length === 0 ? <p className="mt-3 text-sm text-slate">No custom tools yet.</p> : (
        <table className="mt-3 w-full border-collapse text-sm">
          <thead><tr><th className={TH}>Name</th><th className={TH}>Kind</th><th className={TH}>State</th><th className={TH}>Version</th><th className={TH}></th></tr></thead>
          <tbody>
            {tools.map((t) => (
              <Fragment key={t.id}>
                <tr>
                  <td className={TD}>{t.display_name || t.name} <span className="text-xs text-slate/60">({t.name})</span></td>
                  <td className={TD}>{t.kind}{t.requires_egress && <span className="ml-1"><Badge tone="gold">egress</Badge></span>}{t.side_effecting && <span className="ml-1"><Badge>approval</Badge></span>}</td>
                  <td className={TD}>{t.enabled && t.approved ? <Badge tone="green">live</Badge> : t.approved ? <Badge>approved, off</Badge> : <Badge tone="red">needs approval</Badge>}</td>
                  <td className={TD}>v{t.version}{t.approved_version != null && t.approved_version !== t.version ? <span className="text-xs text-slate/60"> (approved v{t.approved_version})</span> : null}</td>
                  <td className={TD}>
                    <button className={BTN2} disabled={!!busy} onClick={() => (editId === t.id ? close() : startEdit(t))}>{editId === t.id ? "Close" : "Edit"}</button>
                    {t.enabled ? (
                      <button className={BTN2 + " ml-2"} disabled={!!busy} onClick={() => disable(t)}>Disable</button>
                    ) : (
                      <button className={BTN2 + " ml-2"} disabled={!!busy} onClick={() => enable(t)}>Approve &amp; enable</button>
                    )}
                    <button className={BTN_DANGER + " ml-2"} disabled={!!busy} onClick={() => remove(t)}>Delete</button>
                  </td>
                </tr>
                {editId === t.id && (
                  <tr><td className={TD} colSpan={5}>
                    <TestRunBox names={paramNames(t)} args={testArgs} setArgs={setTestArgs} onRun={() => testRun(t)} result={testResult} busy={!!busy} />
                  </td></tr>
                )}
              </Fragment>
            ))}
          </tbody>
        </table>
      )}

      {editId !== null && (
        <div className="admin-card mt-4">
          <div className="admin-card-head"><h4>{editId ? "Edit custom tool" : "New custom tool"}</h4></div>
          <div className="flex flex-wrap items-end gap-2">
            <div><label className={LABEL}>Name (tool id)</label><input className={INPUT} placeholder="fx_rate" value={name} onChange={(e) => setName(e.target.value)} disabled={!!editId} /></div>
            <div><label className={LABEL}>Display name</label><input className={INPUT} value={displayName} onChange={(e) => setDisplayName(e.target.value)} /></div>
            <div><label className={LABEL}>Kind</label>
              <Dropdown
                value={kind}
                onChange={setKind}
                ariaLabel="Tool kind"
                disabled={!!editId}
                options={[
                  { value: "http", label: "http" },
                  { value: "script", label: "script (python)" },
                ]}
              />
            </div>
            <div><label className={LABEL}>Timeout (s)</label><input className={INPUT + " w-24"} value={timeout} onChange={(e) => setTimeoutSecs(e.target.value)} /></div>
          </div>
          <div className="mt-2"><label className={LABEL}>Description (the model reads this)</label><textarea className={INPUT + " w-full"} rows={2} value={description} onChange={(e) => setDescription(e.target.value)} /></div>
          <div className="mt-2 flex flex-wrap gap-4">
            {kind === "http" && <label className="flex items-center gap-1 text-xs text-slate/80"><input type="checkbox" checked={requiresEgress} onChange={(e) => setRequiresEgress(e.target.checked)} /> requires egress (public host)</label>}
            <label className="flex items-center gap-1 text-xs text-slate/80"><input type="checkbox" checked={sideEffecting} onChange={(e) => setSideEffecting(e.target.checked)} /> side-effecting (needs approval per call){kind === "script" ? " — scripts always require approval" : ""}</label>
          </div>
          <div className="mt-2"><label className={LABEL}>Parameters (JSON Schema)</label><textarea className={INPUT + " w-full font-mono"} rows={5} value={schemaText} onChange={(e) => setSchemaText(e.target.value)} /></div>
          {kind === "script" ? (
            <div className="mt-2"><label className={LABEL}>Python source (reads <code>params.json</code>, prints the result)</label><textarea className={INPUT + " w-full font-mono"} rows={10} value={sourceText} onChange={(e) => setSourceText(e.target.value)} /></div>
          ) : (
            <>
              <div className="mt-2"><label className={LABEL}>Request config (JSON)</label><textarea className={INPUT + " w-full font-mono"} rows={7} value={configText} onChange={(e) => setConfigText(e.target.value)} /></div>
              <div className="mt-2"><label className={LABEL}>Auth secret {editId ? "(blank = keep current)" : "(optional)"}</label><input className={INPUT + " min-w-[16rem]"} type="password" autoComplete="off" value={authValue} onChange={(e) => setAuthValue(e.target.value)} placeholder="token / api key" /></div>
            </>
          )}
          <div className="mt-3">
            <button className={BTN} disabled={!!busy || !name.trim()} onClick={save}>{editId ? "Save new version" : "Create"}</button>
            <button className={BTN2 + " ml-2"} disabled={!!busy} onClick={close}>Cancel</button>
          </div>
        </div>
      )}
    </div>
  );
}

function TestRunBox(
  { names, args, setArgs, onRun, result, busy }:
  { names: string[]; args: Record<string, string>; setArgs: (a: Record<string, string>) => void; onRun: () => void; result: string | null; busy: boolean },
) {
  return (
    <div>
      <p className="mb-1 text-xs text-slate/70">Test run (uses the same egress/SSRF gates; no approval needed):</p>
      <div className="flex flex-wrap items-end gap-2">
        {names.length === 0 ? <span className="text-xs text-slate/60">no parameters</span> : names.map((n) => (
          <div key={n}><label className={LABEL}>{n}</label><input className={INPUT} value={args[n] ?? ""} onChange={(e) => setArgs({ ...args, [n]: e.target.value })} /></div>
        ))}
        <button className={BTN2} disabled={busy} onClick={onRun}>Run</button>
      </div>
      {result != null && <pre className="mt-2 max-h-64 overflow-auto whitespace-pre-wrap rounded bg-black/20 p-2 text-xs">{result}</pre>}
    </div>
  );
}

registerAdminSection({ key: "users", label: "Users", component: UsersSectionForSelf, permission: "users.view" });
registerAdminSection({ key: "sharing", label: "Sharing", component: SharingSection, permission: "grants.manage" });
registerAdminSection({ key: "feedback", label: "Feedback", component: FeedbackSection, permission: "feedback.view" });
registerAdminSection({ key: "groups", label: "Groups", component: GroupsSection, permission: "groups.manage" });
registerAdminSection({ key: "workflows", label: "Workflows", component: WorkflowsSection, capability: "workflows", fullBleed: true });
registerAdminSection({ key: "integrations", label: "Integrations", component: IntegrationsSection, permission: "integrations.manage" });
registerAdminSection({ key: "mcp-servers", label: "MCP Servers", component: McpServersSection, capability: "mcp", permission: "mcp.manage" });
registerAdminSection({ key: "tools", label: "Tools", component: ToolsSection, permission: "tools.manage" });
registerAdminSection({ key: "config", label: "Config", component: ConfigSection, permission: "config.manage" });
registerAdminSection({ key: "providers", label: "Providers", component: ProvidersSection, permission: "providers.manage" });
registerAdminSection({ key: "voice-live", label: "Live voice", component: VoiceLiveSection, capability: "voice_live", permission: "voice.manage" });
registerAdminSection({ key: "announcements", label: "Announcements", component: AnnouncementsSection, permission: "announcements.manage" });
registerAdminSection({ key: "system", label: "System", component: SystemSection });
