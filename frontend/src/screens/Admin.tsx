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
  CALL_OUTCOMES,
  useTelephonyEnquiries,
  setEnquiryHandled,
  useConflictNames,
  addConflictNames,
  removeConflictName,
  useDiary,
  setDiary,
  addDiaryClosure,
  removeDiaryClosure,
  useAppointments,
  cancelAppointment,
  type DiaryBody,
  type DiaryOpening,
  type Appointment,
  useGroupChats,
  type Enquiry,
  usePhoneNumbers,
  createPhoneNumber,
  updatePhoneNumber,
  deletePhoneNumber,
  useTelephonyCalls,
  useTelephonyCompliance,
  deleteCallTranscript,
  runTelephonyCheck,
  type TelephonyCheck,
  callRecordingUrl,
  deleteCallRecording,
  useNotifyTargets,
  createNotifyTarget,
  updateNotifyTarget,
  deleteNotifyTarget,
  testNotifyTarget,
  NOTIFY_EVENTS,
  type NotifyTarget,
  type PhoneLine,
  type CallRecord,
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
  patchMcpServer,
  deleteMcpServer,
  discoverMcpOauth,
  putMcpOauthClient,
  deleteMcpOauthClient,
  connectMcpServer,
  type McpOauthDiscovery,
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
  useUserApiKeys,
  adminRevokeApiKey,
  useUserDevices,
  adminRevokeDevice,
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
  useUsers,
  useWhoami,
} from "@/api/client";
import { AreaChart, Bars, Donut } from "@/components/charts";
import { Icon } from "@/components/icons";
import { Dropdown } from "@/components/Dropdown";
import { Workflows } from "@/screens/Workflows";
import { useBusy } from "@/components/useBusy";
import { BTN, BTN2, BTN_DANGER, Badge, H1, INPUT, LABEL, TD, TH, TableScroll } from "@/components/adminUi";
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
// A user's platform API keys, on demand. Read-and-revoke only: the secret is
// unrecoverable for an administrator exactly as it is for its owner, so the
// useful admin actions are seeing that keys exist and withdrawing one.
function UserApiKeys({ userId }: { userId: string }) {
  const keys = useUserApiKeys(userId);
  const { busy, run } = useBusy();
  const live = (keys.data ?? []).filter((k) => !k.revoked_at);

  if (keys.isLoading) return <span className="text-xs text-slate/60">Loading…</span>;
  if (live.length === 0) return <span className="text-xs text-slate/60">No active keys.</span>;
  return (
    <div className="flex flex-col gap-1">
      {live.map((k) => (
        <div key={k.id} className="flex items-center gap-2 text-xs">
          <span>{k.name}</span>
          <span className="font-mono text-slate/60">{k.display_prefix}…</span>
          <span className="text-slate/60">
            last used {k.last_used_at ? new Date(k.last_used_at).toLocaleDateString() : "never"}
          </span>
          <button
            className={BTN_DANGER}
            disabled={!!busy}
            onClick={async () => {
              if (
                await confirmDialog({
                  danger: true,
                  title: "Revoke this key?",
                  body: `Anything using "${k.name}" stops working immediately. This cannot be undone.`,
                  confirmLabel: "Revoke key",
                })
              )
                run("Revoke key", () => adminRevokeApiKey(userId, k.id).then(() => keys.refetch()), "Key revoked.");
            }}
          >
            Revoke
          </button>
        </div>
      ))}
    </div>
  );
}

// A user's paired devices, on demand. Read-and-sign-out only, mirroring the key
// view: an administrator sees which machines are connected and can withdraw one.
function UserDevices({ userId }: { userId: string }) {
  const devices = useUserDevices(userId);
  const { busy, run } = useBusy();
  const live = (devices.data ?? []).filter((d) => !d.revoked_at);

  if (devices.isLoading) return <span className="text-xs text-slate/60">Loading…</span>;
  if (live.length === 0) return <span className="text-xs text-slate/60">No connected devices.</span>;
  return (
    <div className="flex flex-col gap-1">
      {live.map((d) => (
        <div key={d.id} className="flex items-center gap-2 text-xs">
          <span>{d.name}</span>
          <span className="text-slate/60">{d.platform}</span>
          <span className="text-slate/60">
            last seen {d.last_seen_at ? new Date(d.last_seen_at).toLocaleDateString() : "never"}
          </span>
          <button
            className={BTN_DANGER}
            disabled={!!busy}
            onClick={async () => {
              if (
                await confirmDialog({
                  danger: true,
                  title: "Sign this device out?",
                  body: `"${d.name}" is signed out immediately and must be paired again to reconnect.`,
                  confirmLabel: "Sign out device",
                })
              )
                run("Sign out device", () => adminRevokeDevice(userId, d.id).then(() => devices.refetch()), "Device signed out.");
            }}
          >
            Sign out
          </button>
        </div>
      ))}
    </div>
  );
}

function UsersSection({ selfId }: { selfId?: string }) {
  const qc = useQueryClient();
  const users = useAdminUsers();
  const { busy, run } = useBusy();
  const who = useWhoami();
  const publicApi = !!who.data?.capabilities?.public_api;
  const [openKeys, setOpenKeys] = useState<string | null>(null);
  const [openDevices, setOpenDevices] = useState<string | null>(null);
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
        <TableScroll>
          <table className="w-full border-collapse text-sm">
            <thead>
              <tr>
                <th className={TH}>Email</th>
                <th className={TH}>Name</th>
                <th className={TH}>Role</th>
                <th className={TH}>Status</th>
                <th className={TH}>MFA</th>
                {publicApi && <th className={TH}>API keys</th>}
                <th className={TH}>Devices</th>
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
                  {publicApi && (
                    <td className={TD}>
                      <button
                        className={BTN2}
                        onClick={() => setOpenKeys((cur) => (cur === u.id ? null : u.id))}
                      >
                        {openKeys === u.id ? "Hide" : "View"}
                      </button>
                    </td>
                  )}
                  <td className={TD}>
                    <button
                      className={BTN2}
                      onClick={() => setOpenDevices((cur) => (cur === u.id ? null : u.id))}
                    >
                      {openDevices === u.id ? "Hide" : "View"}
                    </button>
                  </td>
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
        </TableScroll>
      )}
      {openKeys && (
        <div className="mt-3 rounded border border-slate/20 p-3">
          <div className="mb-2 text-xs text-slate/70">
            API keys for {users.data?.find((u) => u.id === openKeys)?.email}
          </div>
          <UserApiKeys userId={openKeys} />
        </div>
      )}
      {openDevices && (
        <div className="mt-3 rounded border border-slate/20 p-3">
          <div className="mb-2 text-xs text-slate/70">
            Devices for {users.data?.find((u) => u.id === openDevices)?.email}
          </div>
          <UserDevices userId={openDevices} />
        </div>
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
            <TableScroll className="mb-4">
              <table className="w-full border-collapse text-sm">
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
            </TableScroll>
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
          <TableScroll>
            <table className="w-full border-collapse">
              <thead><tr><th className={TH}>Model</th><th className={TH}>Answers</th><th className={TH}>Prompt</th><th className={TH}>Completion</th></tr></thead>
              <tbody>{d.per_model.map((m, i) => <tr key={i}><td className={TD}>{m.model ?? "—"}</td><td className={TD}>{m.count}</td><td className={TD}>{m.prompt_tokens.toLocaleString()}</td><td className={TD}>{m.completion_tokens.toLocaleString()}</td></tr>)}</tbody>
            </table>
          </TableScroll>
          <TableScroll>
            <table className="w-full border-collapse">
              <thead><tr><th className={TH}>User</th><th className={TH}>Answers</th><th className={TH}>Total tokens</th></tr></thead>
              <tbody>{d.per_user.map((u, i) => <tr key={i}><td className={TD}>{u.email ?? u.user_id ?? "—"}</td><td className={TD}>{u.count}</td><td className={TD}>{(u.prompt_tokens + u.completion_tokens).toLocaleString()}</td></tr>)}</tbody>
            </table>
          </TableScroll>
          <TableScroll>
            <table className="w-full border-collapse">
              <thead><tr><th className={TH}>Agent</th><th className={TH}>Answers</th><th className={TH}>Total tokens</th></tr></thead>
              <tbody>{d.per_agent.map((g, i) => <tr key={i}><td className={TD}>{g.agent_name ?? (g.agent_id ? g.agent_id.slice(0, 8) : "(no agent)")}</td><td className={TD}>{g.count}</td><td className={TD}>{(g.prompt_tokens + g.completion_tokens).toLocaleString()}</td></tr>)}</tbody>
            </table>
          </TableScroll>
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
          <TableScroll>
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
          </TableScroll>
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
          <TableScroll>
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
          </TableScroll>
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
        <TableScroll>
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
        </TableScroll>
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
  const [oauthFor, setOauthFor] = useState<McpServer | null>(null);

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
  // Whether this server may be reached while somebody is on the telephone. Allowing it is
  // a standing decision, so it is confirmed once here rather than asked per call, which is
  // the thing that cannot happen mid-conversation.
  const onCall = async (s: McpServer) => {
    const allow = s.call_policy !== "allow";
    if (allow) {
      const ok = await confirmDialog({
        title: `Let '${s.slug}' be used during a telephone call?`,
        body: "On a call there is nobody to approve a tool that changes something, so this server's tools would run for an anonymous caller without anyone seeing them first. Every such call is recorded. Leave it refused and the agent tells the caller it cannot do that on the telephone and offers to take a message.",
        confirmLabel: "Allow on calls",
      });
      if (!ok) return;
    }
    run(
      "Save",
      async () => { await patchMcpServer(s.id, { call_policy: allow ? "allow" : "refuse" }); refresh(); },
      allow ? "Allowed during calls." : "Refused during calls.",
    );
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
                { value: "oauth", label: "OAuth 2.1 (per-user)" },
              ]}
            />
            {(authType === "api_key" || authType === "header") && (
              <input className={INPUT} placeholder="header name e.g. CONTEXT7_API_KEY" value={authHeaderName} onChange={(e) => setAuthHeaderName(e.target.value)} />
            )}
            {authType !== "none" && authType !== "oauth" && (
              <input className={INPUT + " min-w-[16rem]"} type="password" autoComplete="off" placeholder={authType === "bearer" ? "token (sent as 'Bearer …')" : "secret value"} value={authValue} onChange={(e) => setAuthValue(e.target.value)} />
            )}
            {authType === "oauth" && (
              <span className="text-xs text-slate/70">Register by URL only — configure the issuer + connect below once the row exists.</span>
            )}
          </div>
        )}
      </div>

      {servers.isLoading ? <p className="text-sm text-slate">Loading…</p> : (
        <TableScroll>
          <table className="w-full border-collapse text-sm">
            <thead><tr><th className={TH}>Slug</th><th className={TH}>Transport</th><th className={TH}>Status</th><th className={TH}>Tools</th><th className={TH}>Live</th><th className={TH}>On calls</th><th className={TH}></th></tr></thead>
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
                    <button
                      className={BTN2}
                      disabled={!!busy}
                      title="On a telephone call there is nobody to approve a tool that changes something. A server that is refused makes the agent say it cannot do that on the telephone and offer to take a message."
                      onClick={() => onCall(s)}
                    >
                      {s.call_policy === "allow" ? "Allowed" : "Refused"}
                    </button>
                  </td>
                  <td className={TD}>
                    {s.auth_type === "oauth" && (
                      <button className={BTN2 + " mr-2"} disabled={!!busy} onClick={() => setOauthFor(s)}>OAuth</button>
                    )}
                    <button className={BTN2} disabled={!!busy} onClick={() => approve(s)}>{s.status === "active" ? "Re-pin" : s.status === "quarantined" ? "Re-approve" : "Approve"}</button>
                    <button className={BTN_DANGER + " ml-2"} disabled={!!busy} onClick={() => remove(s)}>Delete</button>
                  </td>
                </tr>
              ))}
              {(!servers.data || servers.data.length === 0) && <tr><td className={TD} colSpan={7}>No MCP servers registered.</td></tr>}
            </tbody>
          </table>
        </TableScroll>
      )}

      {oauthFor && (
        <McpOAuthPanel
          key={oauthFor.id}
          server={oauthFor}
          onClose={() => setOauthFor(null)}
          onChanged={refresh}
        />
      )}
    </div>
  );
}

// One-click MCP connections (OAuth 2.1): discover + approve an issuer, register a client
// (auto via DCR or a pasted client_id), then connect the admin's catalogue-source (and,
// optionally, a service connection for unattended runs).
function McpOAuthPanel({ server, onClose, onChanged }: { server: McpServer; onClose: () => void; onChanged: () => void }) {
  const { busy, run } = useBusy();
  const [disc, setDisc] = useState<McpOauthDiscovery | null>(null);
  const [allowedOrigin, setAllowedOrigin] = useState("");
  const [clientId, setClientId] = useState("");
  const [clientSecret, setClientSecret] = useState("");

  const check = () =>
    run("Check", async () => {
      setDisc(await discoverMcpOauth(server.id, allowedOrigin.trim() || undefined));
    });
  const registerDcr = () =>
    run("Register", async () => {
      await putMcpOauthClient(server.id, { use_dcr: true, allowed_issuer_origin: allowedOrigin.trim() || undefined });
      onChanged();
    }, "Client registered — now connect");
  const saveManual = () =>
    run("Save", async () => {
      await putMcpOauthClient(server.id, {
        client_id: clientId.trim(),
        client_secret: clientSecret.trim() || undefined,
        allowed_issuer_origin: allowedOrigin.trim() || undefined,
      });
      onChanged();
    }, "Client saved — now connect");
  const connect = (service: boolean) =>
    run("Connect", async () => {
      const { authorize_url } = await connectMcpServer(server.id, service);
      window.location.href = authorize_url;
    });
  const removeClient = () =>
    run("Remove", async () => {
      await deleteMcpOauthClient(server.id);
      setDisc(null);
      onChanged();
    });

  return (
    <div className="admin-card mb-4">
      <div className="admin-card-head">
        <h4>OAuth setup — {server.slug}</h4>
        <button className={BTN2} onClick={onClose}>Close</button>
      </div>
      <p className="mb-2 text-xs text-slate/70">
        Register the server by URL, check it, approve the issuer, then connect. Each user connects once and the
        server’s tools run under their own identity.
      </p>
      <div className="flex flex-wrap items-end gap-2">
        <input className={INPUT + " min-w-[18rem]"} placeholder="allowed issuer origin (only if cross-origin)" value={allowedOrigin} onChange={(e) => setAllowedOrigin(e.target.value)} />
        <button className={BTN} disabled={!!busy} onClick={check}>Check server</button>
      </div>

      {disc && (
        <div className="mt-3 text-sm">
          <p>Issuer: <code className="break-all">{disc.issuer}</code></p>
          <p className="text-xs text-slate/70">
            DCR: {disc.dcr_available ? "available" : "not supported"} · PKCE S256: {disc.s256_ok ? "yes" : "no"}
          </p>
          <p className="mt-1 text-xs">Redirect URI to register at the provider: <code className="break-all">{disc.callback_url}</code></p>
          {disc.warnings.map((w, i) => (
            <p key={i} className="text-xs text-gold">⚠ {w}</p>
          ))}

          <div className="mt-3">
            {disc.dcr_available ? (
              <button className={BTN} disabled={!!busy || !disc.s256_ok} onClick={registerDcr}>Register automatically</button>
            ) : (
              <div className="flex flex-wrap items-end gap-2">
                <input className={INPUT} placeholder="client_id" value={clientId} onChange={(e) => setClientId(e.target.value)} />
                <input className={INPUT} type="password" autoComplete="off" placeholder="client secret (blank to keep)" value={clientSecret} onChange={(e) => setClientSecret(e.target.value)} />
                <button className={BTN} disabled={!!busy || !disc.s256_ok || !clientId.trim()} onClick={saveManual}>Save client</button>
              </div>
            )}
          </div>

          <div className="mt-3 flex flex-wrap items-center gap-2">
            <button className={BTN2} disabled={!!busy} onClick={() => connect(false)}>Connect (my catalogue source)</button>
            <button className={BTN2} disabled={!!busy} onClick={() => connect(true)}>Use as service connection</button>
            <button className={BTN_DANGER} disabled={!!busy} onClick={removeClient}>Remove client</button>
          </div>
          <p className="mt-1 text-xs text-slate/60">After connecting, set the catalogue source and approve the server (Approve button in the table).</p>
        </div>
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
  { key: "desktop.always_prompt_commands", label: "Always ask before running commands", desc: "Confirm every command the desktop agent runs, even on a computer whose boundary would otherwise let a contained, offline command run without asking. On restores a prompt for every command. Off by default.", valueType: "bool", default: "false" },
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
        <TableScroll>
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
        </TableScroll>
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
        <TableScroll>
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
        </TableScroll>
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
          <TableScroll>
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
          </TableScroll>
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
        <TableScroll>
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
        </TableScroll>
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

// ── Telephone lines ─────────────────────────────────────────────────────────
// A line binds a public number to one agent and one account. Whoever rings it gets
// a session running as that account, so the two pickers here are the whole security
// decision and the table shows how wide each line is (its agent's tool count).
// Deliberately absent: the carrier's own credential, address and call ceiling, which
// stay with the operator rather than becoming a second place to rotate a secret.
const OUTCOME_LABELS: Record<string, string> = {
  in_progress: "In progress",
  completed: "Completed",
  carrier_ended: "Ended by network",
  dropped: "Dropped",
  no_media: "No audio",
  line_full: "Lines busy",
  transferred: "Put through",
  notice_failed: "Could not tell the caller",
};
const CALL_PAGE = 50;

/// What checking a caller concluded. Only "clear" lets a call be put through: the other
/// two mean the same thing, which is what makes the check fail closed.
const CHECK_LABELS: Record<string, string> = {
  clear: "Clear",
  possible: "Needs a person",
  unknown: "Could not check",
};

const fmtEpoch = (e: number) => new Date(e * 1000).toLocaleString();
const fmtDuration = (s: number | null) => {
  if (s === null) return "—";
  if (s < 60) return `${s}s`;
  return `${Math.floor(s / 60)}m ${String(s % 60).padStart(2, "0")}s`;
};

type LineDraft = { e164: string; agent_id: string; owner_user_id: string; provider: string; label: string; greeting: string; notice: string; transcript_days: string; log_days: string; record_calls: boolean; recording_days: string; enabled: boolean; deliver_group_chat_id: string; transfer_e164: string };
const EMPTY_LINE: LineDraft = { e164: "", agent_id: "", owner_user_id: "", provider: "twilio", label: "", greeting: "", notice: "", transcript_days: "0", log_days: "0", record_calls: false, recording_days: "30", enabled: false, deliver_group_chat_id: "", transfer_e164: "" };

/// What can answer a line, in the words an operator chooses between.
const ANSWERED_BY = [
  { value: "twilio", label: "A telephone carrier" },
  { value: "audiosocket", label: "This practice's own telephone system" },
];

/// The standard notice, so the editor can show what a line will say before it has said it.
///
/// A copy of the wording the server speaks, kept here for the preview alone: what is
/// actually said is composed on the server from the line's own row, and every line list
/// carries those words back in `opening`.
const STANDARD_NOTICE =
  "You are speaking to an automated assistant. What you say is written down so that your enquiry " +
  "can be dealt with, and a member of staff may read it. If you would rather speak to a person, " +
  "please say so. How can I help you today?";

/// The sentence a line that records adds, second, where somebody would say it.
const RECORDED_SENTENCE = "This call is recorded.";

/// What a caller will hear, joined the way the server joins it.
const spokenOpening = (greeting: string, notice: string, recorded: boolean): string => {
  const one = (t: string) => t.split(/\s+/).filter(Boolean).join(" ");
  const hello = one(greeting);
  let said = one(notice) || one(STANDARD_NOTICE);
  if (recorded) {
    const at = said.indexOf(". ");
    said = at >= 0
      ? `${said.slice(0, at + 2)}${RECORDED_SENTENCE} ${said.slice(at + 2)}`
      : `${RECORDED_SENTENCE} ${said}`;
  }
  if (!hello) return said;
  const ended = /[.!?,;:]$/.test(hello) ? hello : `${hello}.`;
  return `${ended} ${said}`;
};

function TelephonySection() {
  const nav = useNavigate();
  const qc = useQueryClient();
  const { busy, run } = useBusy();
  const lines = usePhoneNumbers();
  const agents = useAgents();
  const users = useUsers();
  const teams = useGroupChats();
  const [editing, setEditing] = useState<string | "new" | null>(null);
  const [draft, setDraft] = useState<LineDraft>(EMPTY_LINE);

  const [fLine, setFLine] = useState("");
  const [fOutcome, setFOutcome] = useState("");
  const [cursor, setCursor] = useState<string | undefined>(undefined);
  const [rows, setRows] = useState<CallRecord[]>([]);
  const [exhausted, setExhausted] = useState(false);
  const calls = useTelephonyCalls({
    numberId: fLine || undefined,
    outcome: fOutcome || undefined,
    before: cursor,
    limit: CALL_PAGE,
  });

  // Pages accumulate: the log is read by keyset, so each answer is the slice after the
  // last call already shown. Merged by id rather than concatenated, so a repeated
  // render of the same page cannot show a call twice.
  useEffect(() => {
    const page = calls.data;
    if (!page) return;
    setRows((prev) => {
      const base = cursor ? prev : [];
      const seen = new Set(base.map((r) => r.id));
      return [...base, ...page.filter((r) => !seen.has(r.id))];
    });
    setExhausted(page.length < CALL_PAGE);
  }, [calls.data, cursor]);

  const refilter = (fn: () => void) => {
    fn();
    setCursor(undefined);
    setRows([]);
    setExhausted(false);
  };
  const refreshCalls = () => {
    refilter(() => {});
    qc.invalidateQueries({ queryKey: ["admin-telephony-calls"] });
  };
  const refreshLines = () => qc.invalidateQueries({ queryKey: ["admin-telephony-numbers"] });

  const startNew = () => { setDraft(EMPTY_LINE); setEditing("new"); };
  const startEdit = (l: PhoneLine) => {
    setDraft({
      e164: l.e164,
      agent_id: l.agent_id,
      owner_user_id: l.owner_user_id,
      provider: l.provider,
      label: l.label ?? "",
      greeting: l.greeting ?? "",
      notice: l.notice ?? "",
      transcript_days: String(l.transcript_days),
      log_days: String(l.log_days),
      record_calls: l.record_calls,
      recording_days: String(l.recording_days || 30),
      enabled: l.enabled,
      deliver_group_chat_id: l.deliver_group_chat_id ?? "",
      transfer_e164: l.transfer_e164 ?? "",
    });
    setEditing(l.id);
  };
  const setD = (k: keyof LineDraft, v: string | boolean) => setDraft((p) => ({ ...p, [k]: v }));

  const save = () =>
    run(
      "Save",
      async () => {
        const body = {
          e164: draft.e164.trim(),
          agent_id: draft.agent_id,
          owner_user_id: draft.owner_user_id,
          provider: draft.provider,
          label: draft.label.trim() || null,
          greeting: draft.greeting.trim() || null,
          notice: draft.notice.trim() || null,
          transcript_days: Number(draft.transcript_days) || 0,
          log_days: Number(draft.log_days) || 0,
          record_calls: draft.record_calls,
          recording_days: Number(draft.recording_days) || 0,
          enabled: draft.enabled,
          deliver_group_chat_id: draft.deliver_group_chat_id || null,
          transfer_e164: draft.transfer_e164.trim() || null,
        };
        if (editing === "new") await createPhoneNumber(body);
        else if (editing) await updatePhoneNumber(editing, body);
        setEditing(null);
        refreshLines();
      },
      editing === "new" ? "Line registered." : "Line saved.",
    );

  const toggle = (l: PhoneLine) =>
    run(
      l.enabled ? "Switch off" : "Switch on",
      () => updatePhoneNumber(l.id, { enabled: !l.enabled }).then(refreshLines),
      l.enabled ? "Line switched off." : "Line answering.",
    );

  const remove = async (l: PhoneLine) => {
    const ok = await confirmDialog({
      title: `Release ${l.e164}?`,
      body: "The line stops answering and cannot be recovered. The calls it took stay in the log. To stop it answering reversibly, switch it off instead.",
      danger: true,
      confirmLabel: "Release",
    });
    if (!ok) return;
    run("Release", () => deletePhoneNumber(l.id).then(() => { refreshLines(); refreshCalls(); }), "Line released.");
  };

  // Throwing away what was said on one call, for the moment somebody asks for their
  // information to be removed and will not wait for the nightly sweep. The record of the
  // call survives: that it happened, from what number and for how long is the practice's
  // own record and is not what was asked about.
  const dropTranscript = async (c: CallRecord) => {
    const ok = await confirmDialog({
      title: "Delete what was said on this call?",
      body: "The conversation is deleted and cannot be recovered. The call stays in the log, marked as tidied away. Anything the caller asked to be passed on, and any appointment they made, is kept.",
      danger: true,
      confirmLabel: "Delete",
    });
    if (!ok) return;
    run("Delete", () => deleteCallTranscript(c.id).then(refreshCalls), "Conversation deleted.");
  };

  const agentOpts = (agents.data ?? []).map((a) => ({
    value: a.id,
    label: `${a.name} · ${a.tools.length} ${a.tools.length === 1 ? "tool" : "tools"}`,
  }));
  const userOpts = (users.data ?? []).map((u) => ({ value: u.id, label: `${u.display_name} (${u.email})` }));
  const teamOpts = [
    { value: "", label: "Do not announce" },
    ...(teams.data ?? [])
      .filter((t) => t.kind !== "dm")
      .map((t) => ({ value: t.id, label: t.name ?? "Untitled chat" })),
  ];
  const lineOpts = [
    { value: "", label: "Every line" },
    ...(lines.data ?? []).map((l) => ({ value: l.id, label: l.label ? `${l.e164} — ${l.label}` : l.e164 })),
  ];
  // The accounts a list could belong to: the ones that actually own a line, rather than
  // every account on the deployment. A conflict list is a practice's own holding, so the
  // only accounts worth offering are the ones answering a telephone.
  const listOwners = Array.from(
    new Map((lines.data ?? []).map((l) => [l.owner_user_id, l.owner_name])).entries(),
  ).map(([value, label]) => ({ value, label }));
  const outcomeOpts = [
    { value: "", label: "Any outcome" },
    ...CALL_OUTCOMES.map((o) => ({ value: o, label: OUTCOME_LABELS[o] ?? o })),
  ];
  const complete = draft.e164.trim() !== "" && draft.agent_id !== "" && draft.owner_user_id !== "";

  return (
    <div>
      <H1>Telephone</H1>
      <p className="mb-1 text-xs text-slate/70">
        Each line binds a public telephone number to one agent and one account. Anybody who rings it
        speaks to that agent, in a session that runs as that account and can reach what that account can
        reach, so choose both carefully: the agent's tool count below is the width of the line. A new line
        starts switched off. Every caller hears the line's greeting and its notice before anything they say
        is acted on, and a line that cannot say them does not take the call.
      </p>
      <p className="mb-4 text-xs text-slate/70">
        The account list is the one every signed-in person can see, so if somebody is missing from it, ask
        a platform administrator to make the binding. The carrier account, public address and limit on
        simultaneous calls are set by whoever operates this deployment and are not shown here.
      </p>
      <details className="mb-4 text-xs text-slate/70">
        <summary className="cursor-pointer">Answering from your own telephone system</summary>
        <p className="mt-2">
          A line answered by your own system never sends the caller's voice anywhere but here. Your
          telephone system asks this deployment what to do with the call, is given a one-off identifier
          good for thirty seconds, and opens a connection with it. Whoever operates this deployment sets
          the address to listen on and the shared secret; the two requests below carry that secret and
          are only accepted from your own network.
        </p>
        <pre className="mt-2 overflow-x-auto rounded-lg border border-navy-lighter bg-navy/40 p-3 text-[11px]">
{`exten => _X.,1,Set(CURLOPT(httpheader)=x-fosnie-telephony-key: YOUR-SECRET)
 same => n,Set(ID=\${CURL(https://your-deployment/api/telephony/audiosocket/answer?from=\${CALLERID(num)}&to=\${EXTEN})})
 same => n,GotoIf($["\${ID}" = ""]?hangup)
 same => n,Answer()
 same => n,Dial(AudioSocket/your-deployment:9092/\${ID})
 same => n,Set(TO=\${CURL(https://your-deployment/api/telephony/audiosocket/continue?call=\${ID})})
 same => n,GotoIf($["\${TO}" = ""]?hangup)
 same => n,Dial(PJSIP/\${TO}@your-trunk)
 same => n(hangup),Hangup()`}
        </pre>
        <p className="mt-2">
          The last three lines are what lets the agent put a caller through to a person: once this side
          of the call ends, your system asks whether anybody is to be rung, and dials them if so.
        </p>
      </details>

      <ReadinessBlock />

      <div className="mb-3 flex items-center gap-2">
        <button className={BTN} disabled={!!busy || editing === "new"} onClick={startNew}>Add a line</button>
      </div>

      {editing !== null && (
        <div className="mb-6 max-w-2xl space-y-3 rounded-xl border border-navy-lighter p-4">
          <h3 className="font-serif text-lg text-slate-lightest">{editing === "new" ? "New line" : "Edit line"}</h3>
          <label className="block text-sm text-slate-lightest">Number
            <input className={INPUT + " mt-1 w-full"} placeholder="+441315550000" value={draft.e164} onChange={(e) => setD("e164", e.target.value)} />
            <span className="mt-1 block text-[11px] text-slate/50">Full international form, including the country code.</span>
          </label>
          <label className="block text-sm text-slate-lightest">Agent
            <div className="mt-1">
              <Dropdown value={draft.agent_id} onChange={(v) => setD("agent_id", v)} ariaLabel="Agent answering this line" fullWidth options={agentOpts} />
            </div>
          </label>
          <label className="block text-sm text-slate-lightest">Account
            <div className="mt-1">
              <Dropdown value={draft.owner_user_id} onChange={(v) => setD("owner_user_id", v)} ariaLabel="Account the calls run as" fullWidth options={userOpts} />
            </div>
          </label>
          <label className="block text-sm text-slate-lightest">Answered by
            <div className="mt-1">
              <Dropdown value={draft.provider} onChange={(v) => setD("provider", v)} ariaLabel="What answers this line" fullWidth options={ANSWERED_BY} />
            </div>
            <span className="mt-1 block text-[11px] text-slate/50">
              A carrier means the call is carried by a telephone company and the audio passes through
              them. Your own telephone system means the audio comes straight here over your network and
              reaches nobody else. Both need setting up by whoever operates this deployment before a
              line will answer.
            </span>
          </label>
          <label className="block text-sm text-slate-lightest">Label (optional)
            <input className={INPUT + " mt-1 w-full"} placeholder="Reception" value={draft.label} onChange={(e) => setD("label", e.target.value)} />
          </label>
          <label className="block text-sm text-slate-lightest">Greeting (optional)
            <input className={INPUT + " mt-1 w-full"} placeholder="Good morning, Smith and Company" value={draft.greeting} onChange={(e) => setD("greeting", e.target.value)} />
            <span className="mt-1 block text-[11px] text-slate/50">Spoken first, before the notice. Leave off the question: the notice ends by asking how it can help.</span>
          </label>
          <label className="block text-sm text-slate-lightest">Notice (optional)
            <textarea className={INPUT + " mt-1 w-full"} rows={3} placeholder="Leave empty for the standard notice" value={draft.notice} onChange={(e) => setD("notice", e.target.value)} />
            <span className="mt-1 block text-[11px] text-slate/50">
              Every caller is told this before anything they say is acted on, and a line that cannot say
              it does not take the call. Leave it empty for the standard wording, which tells the caller
              they are speaking to an automated assistant, that what they say is written down and may be
              read by a member of staff, and that they can ask for a person. Nothing here records audio.
            </span>
          </label>
          <div className="rounded-lg border border-navy-lighter bg-navy/40 p-3">
            <span className="block text-[11px] uppercase tracking-wide text-slate/50">What the caller hears</span>
            <p className="mt-1 text-sm text-slate-lightest">{spokenOpening(draft.greeting, draft.notice, draft.record_calls)}</p>
          </div>
          <div className="flex gap-3">
            <label className="block text-sm text-slate-lightest">Keep conversations for
              <div className="mt-1 flex items-center gap-2">
                <input className={INPUT + " w-20"} inputMode="numeric" value={draft.transcript_days} onChange={(e) => setD("transcript_days", e.target.value.replace(/[^0-9]/g, ""))} />
                <span className="text-xs text-slate/70">days</span>
              </div>
            </label>
            <label className="block text-sm text-slate-lightest">Keep the record of a call for
              <div className="mt-1 flex items-center gap-2">
                <input className={INPUT + " w-20"} inputMode="numeric" value={draft.log_days} onChange={(e) => setD("log_days", e.target.value.replace(/[^0-9]/g, ""))} />
                <span className="text-xs text-slate/70">days</span>
              </div>
            </label>
          </div>
          <p className="text-[11px] text-slate/50">
            Nought keeps it indefinitely, which is what a new line does. Once a period is set, what was
            said is deleted after it, and the record of the call, who rang and how long it lasted, goes
            after its own. Messages callers left and appointments they made are the practice's own records
            and are never deleted by either of these.
          </p>

          <div className="rounded-lg border border-navy-lighter p-3">
            <label className="flex items-center gap-2 text-sm text-slate-lightest">
              <input type="checkbox" checked={draft.record_calls} onChange={(e) => setD("record_calls", e.target.checked)} /> Keep a recording of the call
            </label>
            <p className="mt-1 text-[11px] text-slate/50">
              Both sides of the conversation, kept as a sound file you can play back from the call log.
              <strong className="text-slate-lightest"> Every caller is told the call is recorded</strong>, in
              the sentence shown in the preview above: switching this on changes what your line says, which
              is the whole difference between a recording and a covert one. About a megabyte a minute.
            </p>
            {draft.record_calls && (
              <label className="mt-2 block text-sm text-slate-lightest">Keep recordings for
                <div className="mt-1 flex items-center gap-2">
                  <input className={INPUT + " w-20"} inputMode="numeric" value={draft.recording_days} onChange={(e) => setD("recording_days", e.target.value.replace(/[^0-9]/g, ""))} />
                  <span className="text-xs text-slate/70">days</span>
                </div>
                <span className="mt-1 block text-[11px] text-slate/50">
                  Required, and there is no keep for ever here: a voice recording is the most sensitive
                  thing this line produces. Deleting what was said on a call deletes its recording too.
                </span>
              </label>
            )}
          </div>
          <label className="block text-sm text-slate-lightest">Put callers through to (optional)
            <input className={INPUT + " mt-1 w-full"} placeholder="+441315557788" value={draft.transfer_e164} onChange={(e) => setD("transfer_e164", e.target.value)} />
            <span className="mt-1 block text-[11px] text-slate/50">A number the agent can hand the call to when the caller needs a person. Leave this empty and it cannot offer to: the agent is never given the ability rather than being given it and refused. The agent never chooses the number.</span>
          </label>
          <label className="block text-sm text-slate-lightest">Announce messages in (optional)
            <div className="mt-1">
              <Dropdown value={draft.deliver_group_chat_id} onChange={(v) => setD("deliver_group_chat_id", v)} ariaLabel="Team chat to announce messages in" fullWidth options={teamOpts} />
            </div>
            <span className="mt-1 block text-[11px] text-slate/50">A team chat the line's own account belongs to. Members are told who rang and what about, never what was said. Messages are recorded either way.</span>
          </label>
          <label className="flex items-center gap-2 text-sm text-slate-lightest">
            <input type="checkbox" checked={draft.enabled} onChange={(e) => setD("enabled", e.target.checked)} /> Answering
          </label>
          <div className="flex gap-2">
            <button className={BTN} disabled={!!busy || !complete} onClick={save}>Save</button>
            <button className={BTN2} disabled={!!busy} onClick={() => setEditing(null)}>Cancel</button>
          </div>
        </div>
      )}

      {lines.isLoading && <p className="text-sm text-slate">Loading…</p>}
      {!lines.isLoading && (lines.data ?? []).length === 0 && (
        <p className="mb-6 text-sm text-slate">No lines yet. Add one and it will answer once you switch it on.</p>
      )}
      {/* The gap below belongs to the scrolling box rather than to the table: a
          bottom margin inside it has the scrollbar between it and the heading
          that follows, and the two run together. */}
      {(lines.data ?? []).length > 0 && (
        <TableScroll className="mb-10">
          <table className="w-full border-collapse text-sm">
            <thead>
              <tr>
                <th className={TH}>Number</th>
                <th className={TH}>Answered by</th>
                <th className={TH}>Label</th>
                <th className={TH}>Agent</th>
                <th className={TH}>Account</th>
                <th className={TH}>Said to callers</th>
                <th className={TH}>Kept for</th>
                <th className={TH}>Puts through to</th>
                <th className={TH}>Screening</th>
                <th className={TH}>Diary</th>
                <th className={TH}>State</th>
                <th className={TH}>Last call</th>
                <th className={TH}>Actions</th>
              </tr>
            </thead>
            <tbody>
              {(lines.data ?? []).map((l) => (
                <tr key={l.id}>
                  <td className={TD + " mono"}>{l.e164}</td>
                  <td className={TD}>
                    {l.provider === "audiosocket"
                      ? <Badge tone="green">own system</Badge>
                      : <Badge tone="slate">carrier</Badge>}
                  </td>
                  <td className={TD}>{l.label ?? "—"}</td>
                  <td className={TD}>
                    {l.agent_name}
                    <span className="mt-0.5 block text-[11px] text-slate/60">{l.agent_tool_count} {l.agent_tool_count === 1 ? "tool" : "tools"}</span>
                  </td>
                  <td className={TD}>{l.owner_name}</td>
                  <td className={TD + " max-w-xs"}>
                    <span className="block truncate" title={l.opening}>{l.opening}</span>
                    {!l.notice && <span className="mt-0.5 block text-[11px] text-slate/60">standard notice</span>}
                  </td>
                  <td className={TD}>
                    <span className="block text-[11px] text-slate/70">
                      words: {l.transcript_days > 0 ? `${l.transcript_days} days` : "indefinitely"}
                    </span>
                    <span className="block text-[11px] text-slate/70">
                      record: {l.log_days > 0 ? `${l.log_days} days` : "indefinitely"}
                    </span>
                    {l.record_calls && (
                      <span className="mt-0.5 block"><Badge tone="red">recorded · {l.recording_days} days</Badge></span>
                    )}
                  </td>
                  <td className={TD + " mono"}>{l.transfer_e164 ?? <span className="text-slate/60">nobody</span>}</td>
                  <td className={TD}>
                    {l.screening_names > 0
                      ? <Badge tone="gold">{l.screening_names} names</Badge>
                      : <span className="text-slate/60">off</span>}
                  </td>
                  <td className={TD}>
                    {l.diary_slot_minutes
                      ? <Badge tone="green">{l.diary_slot_minutes} min</Badge>
                      : <span className="text-slate/60">off</span>}
                  </td>
                  <td className={TD}><Badge tone={l.enabled ? "green" : "slate"}>{l.enabled ? "Answering" : "Off"}</Badge></td>
                  <td className={TD}>{l.last_call_epoch ? fmtEpoch(l.last_call_epoch) : "never"}</td>
                  <td className={TD}>
                    <div className="flex gap-2">
                      <button className={BTN2} disabled={!!busy} onClick={() => startEdit(l)}>Edit</button>
                      <button className={BTN2} disabled={!!busy} onClick={() => toggle(l)}>{l.enabled ? "Switch off" : "Switch on"}</button>
                      <button className={BTN_DANGER} disabled={!!busy} onClick={() => remove(l)}>Release</button>
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </TableScroll>
      )}

      <h3 className="mb-2 font-serif text-lg text-slate-lightest">Call log</h3>
      <p className="mb-3 text-xs text-slate/70">
        Every call a line answered. Calls that were refused, because the number is unknown or the line is
        switched off, are not calls and are recorded in the audit trail with the reason. The transcript
        link opens the conversation, which is readable by whoever may read that account's conversations.
      </p>
      <div className="mb-3 flex flex-wrap gap-2">
        <Dropdown value={fLine} onChange={(v) => refilter(() => setFLine(v))} ariaLabel="Filter by line" options={lineOpts} />
        <Dropdown value={fOutcome} onChange={(v) => refilter(() => setFOutcome(v))} ariaLabel="Filter by outcome" options={outcomeOpts} />
      </div>
      {calls.isLoading && rows.length === 0 && <p className="text-sm text-slate">Loading…</p>}
      {!calls.isLoading && rows.length === 0 && <p className="text-sm text-slate">No calls yet.</p>}
      {rows.length > 0 && (
        <TableScroll>
          <table className="w-full border-collapse text-sm">
            <thead>
              <tr>
                <th className={TH}>Started</th>
                <th className={TH}>Caller</th>
                <th className={TH}>Line</th>
                <th className={TH}>Agent</th>
                <th className={TH}>Account</th>
                <th className={TH}>Length</th>
                <th className={TH}>Outcome</th>
                <th className={TH}>Checked</th>
                <th className={TH}>Told</th>
                <th className={TH}>Recording</th>
                <th className={TH}>Transcript</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((c) => (
                <tr key={c.id}>
                  <td className={TD}>{fmtEpoch(c.started_epoch)}</td>
                  <td className={TD + " mono"}>{c.from_e164 || "withheld"}</td>
                  <td className={TD + " mono"}>{c.to_e164}</td>
                  <td className={TD}>{c.agent_name ?? "—"}</td>
                  <td className={TD}>{c.owner_name}</td>
                  <td className={TD}>{fmtDuration(c.seconds)}</td>
                  <td className={TD}>
                    <Badge tone={c.outcome === "completed" ? "green" : c.outcome === "in_progress" ? "gold" : "slate"}>
                      {OUTCOME_LABELS[c.outcome] ?? c.outcome}
                    </Badge>
                  </td>
                  <td className={TD}>
                    {c.conflict_check
                      ? <Badge tone={c.conflict_check === "clear" ? "green" : "red"}>{CHECK_LABELS[c.conflict_check] ?? c.conflict_check}</Badge>
                      : <span className="text-slate/60">not checked</span>}
                  </td>
                  <td className={TD}>
                    {c.notice_epoch
                      ? <Badge tone="green">told</Badge>
                      : <Badge tone="red">not told</Badge>}
                  </td>
                  <td className={TD}><RecordingCell call={c} onChange={refreshCalls} /></td>
                  <td className={TD}>
                    {c.chat_id ? (
                      <div className="flex gap-2">
                        <button className="text-gold hover:underline" onClick={() => nav(`/c/${c.chat_id}`)}>Open</button>
                        <button className="text-slate/70 hover:underline" disabled={!!busy} onClick={() => dropTranscript(c)}>Delete</button>
                      </div>
                    ) : c.transcript_deleted_epoch ? (
                      <span className="text-slate/60" title={fmtEpoch(c.transcript_deleted_epoch)}>deleted</span>
                    ) : (
                      <span className="text-slate/60">no transcript</span>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </TableScroll>
      )}
      {rows.length > 0 && !exhausted && (
        <button
          className={BTN2 + " mt-3"}
          disabled={calls.isFetching}
          onClick={() => setCursor(rows[rows.length - 1].id)}
        >
          {calls.isFetching ? "Loading…" : "Load more"}
        </button>
      )}

      <EnquiriesBlock lineOpts={lineOpts} />
      <ConflictListBlock owners={listOwners} />
      <DiaryBlock owners={listOwners} />
      <NotificationsBlock owners={listOwners} />
      <ComplianceBlock owners={listOwners} />
    </div>
  );
}

// Messages the lines took. Kept in its own component because the rule it renders is not
// the table's: a row's words belong to the account whose line took them, and the server
// sends null rather than text to anybody else. Null is shown as withheld and never as
// blank, or a message somebody may not read would look like a message nobody wrote.
function EnquiriesBlock({ lineOpts }: { lineOpts: { value: string; label: string }[] }) {
  const qc = useQueryClient();
  const nav = useNavigate();
  const { busy, run } = useBusy();
  const [fLine, setFLine] = useState("");
  const [openOnly, setOpenOnly] = useState(true);
  const q = useTelephonyEnquiries({ numberId: fLine || undefined, open: openOnly || undefined });

  const mark = (e: Enquiry, handled: boolean) =>
    run(
      handled ? "Mark dealt with" : "Reopen",
      () =>
        setEnquiryHandled(e.id, handled).then(() => {
          qc.invalidateQueries({ queryKey: ["admin-telephony-enquiries"] });
          qc.invalidateQueries({ queryKey: ["enquiries"] });
        }),
      handled ? "Marked dealt with." : "Reopened.",
    );

  const rows = q.data ?? [];
  return (
    <div className="mt-8">
      <h3 className="mb-2 font-serif text-lg text-slate-lightest">Messages taken</h3>
      <p className="mb-3 text-xs text-slate/70">
        What callers wanted, written down by the agent answering. What a caller said belongs to the
        account whose line took it: unless that account is yours, or you administer this platform, you
        see that a message was taken and not what it says. Only the account it was taken for can mark
        it dealt with.
      </p>
      <div className="mb-3 flex flex-wrap items-center gap-2">
        <Dropdown value={fLine} onChange={setFLine} ariaLabel="Filter messages by line" options={lineOpts} />
        <label className="flex items-center gap-2 text-sm text-slate-lightest">
          <input type="checkbox" checked={openOnly} onChange={(e) => setOpenOnly(e.target.checked)} /> Not yet dealt with
        </label>
      </div>
      {q.isLoading && <p className="text-sm text-slate">Loading…</p>}
      {!q.isLoading && rows.length === 0 && (
        <p className="text-sm text-slate">{openOnly ? "Nothing waiting." : "No messages yet."}</p>
      )}
      {rows.length > 0 && (
        <TableScroll>
          <table className="w-full border-collapse text-sm">
            <thead>
              <tr>
                <th className={TH}>Taken</th>
                <th className={TH}>Line</th>
                <th className={TH}>Kind</th>
                <th className={TH}>Caller</th>
                <th className={TH}>What about</th>
                <th className={TH}>State</th>
                <th className={TH}>Actions</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((e) => (
                <tr key={e.id}>
                  <td className={TD}>{fmtEpoch(e.created_epoch)}</td>
                  <td className={TD + " mono"}>{e.to_e164}</td>
                  <td className={TD}>
                    <Badge tone={e.urgency === "urgent" ? "red" : "slate"}>
                      {e.kind === "message" ? "Message" : e.kind === "handover" ? "Put through" : "Enquiry"}
                      {e.urgency === "urgent" ? " · urgent" : ""}
                    </Badge>
                  </td>
                  <td className={TD}>
                    {e.caller_name ?? (e.caller_e164 === null ? <span className="text-slate/60">withheld</span> : null)}
                    {e.caller_e164 && <span className="mt-0.5 block text-[11px] text-slate/60 mono">{e.caller_e164}</span>}
                    {e.for_whom && <span className="mt-0.5 block text-[11px] text-slate/60">for {e.for_whom}</span>}
                    {e.contact && <span className="mt-0.5 block text-[11px] text-slate/60">{e.contact}</span>}
                  </td>
                  <td className={TD}>
                    {e.subject === null ? (
                      <span className="text-slate/60">withheld</span>
                    ) : (
                      <>
                        {e.subject}
                        {e.body && <span className="mt-0.5 block text-[11px] text-slate/70">{e.body}</span>}
                        {e.details && Object.keys(e.details).length > 0 && (
                          <span className="mt-0.5 block text-[11px] text-slate/60">
                            {Object.entries(e.details).map(([k, v]) => `${k}: ${v}`).join(" · ")}
                          </span>
                        )}
                      </>
                    )}
                  </td>
                  <td className={TD}>
                    <Badge tone={e.handled ? "green" : "gold"}>{e.handled ? "Dealt with" : "Waiting"}</Badge>
                  </td>
                  <td className={TD}>
                    <div className="flex gap-2">
                      <button className={BTN2} disabled={!!busy} onClick={() => mark(e, !e.handled)}>
                        {e.handled ? "Reopen" : "Mark dealt with"}
                      </button>
                      {e.chat_id && (
                        <button className={BTN2} onClick={() => nav(`/c/${e.chat_id}`)}>Open call</button>
                      )}
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </TableScroll>
      )}
    </div>
  );
}


// The list a line checks callers against. Its own component because what it renders is a
// practice's confidential holding rather than line wiring: the server refuses it to
// anybody but the account it belongs to and a platform administrator, so this block shows
// nothing at all rather than an empty list when the reader is neither.
function ConflictListBlock({ owners }: { owners: { value: string; label: string }[] }) {
  const qc = useQueryClient();
  const { busy, run } = useBusy();
  const [owner, setOwner] = useState("");
  const [paste, setPaste] = useState("");
  const [note, setNote] = useState("");
  const chosen = owner || owners[0]?.value || "";
  const names = useConflictNames(chosen || undefined);

  const refresh = () => {
    qc.invalidateQueries({ queryKey: ["conflict-names"] });
    qc.invalidateQueries({ queryKey: ["admin-telephony-numbers"] });
  };

  const add = () =>
    run(
      "Add",
      async () => {
        const res = await addConflictNames({
          owner_user_id: chosen,
          names: paste,
          note: note.trim() || null,
        });
        setPaste("");
        setNote("");
        refresh();
        toast(
          res.added === 0
            ? "Every one of those was already on the list."
            : `Added ${res.added}${res.already_there ? `, ${res.already_there} already there` : ""}.`,
        );
      },
    );

  const remove = (id: string, name: string) =>
    run("Remove", async () => {
      if (!(await confirmDialog({ title: `Take ${name} off the list?`, confirmLabel: "Remove" }))) return;
      await removeConflictName(id);
      refresh();
    });

  if (owners.length === 0) return null;
  const rows = names.data ?? [];
  return (
    <div className="mt-8">
      <h3 className="mb-2 font-serif text-lg text-slate-lightest">Conflict list</h3>
      <p className="mb-1 text-xs text-slate/70">
        Names the line checks a caller against before offering them anything or putting them through
        to anybody. A caller who matches has a message taken instead, and is told only that somebody
        will be in touch: never that a check was made, and never what it found.
      </p>
      <p className="mb-3 text-xs text-slate/70">
        While an account keeps a list, no call on its lines is put through until it has been checked
        and found clear, so a caller who will not give a full name is not put through either. This
        list is the account's own: whoever may register telephone numbers can see that it exists and
        how many names are on it, and cannot read it.
      </p>
      <div className="mb-3 flex flex-wrap items-end gap-2">
        {owners.length > 1 && (
          <Dropdown value={chosen} onChange={setOwner} ariaLabel="Whose list" options={owners} />
        )}
        {owners.length === 1 && <span className="text-sm text-slate">{owners[0].label}</span>}
        <Badge tone={rows.length > 0 ? "gold" : "slate"}>{rows.length} names</Badge>
      </div>
      <div className="mb-4 max-w-2xl space-y-2">
        <label className="block text-sm text-slate-lightest">Add names, one per line
          <textarea
            className={INPUT + " mt-1 h-28 w-full font-mono text-xs"}
            placeholder={"Marchetti Quarry Holdings\nJane Alice Fraser"}
            value={paste}
            onChange={(e) => setPaste(e.target.value)}
          />
          <span className="mt-1 block text-[11px] text-slate/50">
            Spelling, punctuation, titles, company endings and word order are all ignored when
            checking, so paste them as they come out of your own system. A name already on the list
            is skipped.
          </span>
        </label>
        <label className="block text-sm text-slate-lightest">Note (optional)
          <input className={INPUT + " mt-1 w-full"} placeholder="Which matter" value={note} onChange={(e) => setNote(e.target.value)} />
        </label>
        <button className={BTN} disabled={!!busy || !chosen || paste.trim() === ""} onClick={add}>Add to list</button>
      </div>
      {names.isLoading && <p className="text-sm text-slate">Loading…</p>}
      {!names.isLoading && rows.length === 0 && (
        <p className="text-sm text-slate">No names yet, so callers on these lines are not checked.</p>
      )}
      {rows.length > 0 && (
        <TableScroll>
          <table className="w-full border-collapse text-sm">
            <thead>
              <tr>
                <th className={TH}>Name</th>
                <th className={TH}>Note</th>
                <th className={TH}>Added</th>
                <th className={TH}>Actions</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((n) => (
                <tr key={n.id}>
                  <td className={TD}>{n.name}</td>
                  <td className={TD}>{n.note ?? "—"}</td>
                  <td className={TD}>{fmtEpoch(n.created_epoch)}</td>
                  <td className={TD}>
                    <button className={BTN2} disabled={!!busy} onClick={() => remove(n.id, n.name)}>Remove</button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </TableScroll>
      )}
    </div>
  );
}


// The practice's diary. Its own component because the times in it are shown in the
// PRACTICE'S zone rather than the reader's: an administrator in another country reading
// "9 o'clock" and meaning something else is the fault this whole area exists to avoid, and
// nothing else in this application shows a time in a zone that is not the browser's.
const WEEKDAYS = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];

/** Minutes from midnight as "09:30", for an input. */
const asClock = (m: number) => `${String(Math.floor(m / 60)).padStart(2, "0")}:${String(m % 60).padStart(2, "0")}`;
/** And back, or null when it is not a time. */
function asMinutes(v: string): number | null {
  const m = /^(\d{1,2}):(\d{2})$/.exec(v.trim());
  if (!m) return null;
  const [h, min] = [Number(m[1]), Number(m[2])];
  if (h > 24 || min > 59) return null;
  return h * 60 + min;
}
/** An instant, said in the practice's own zone. */
function inZone(iso: string, timeZone: string | null): string {
  const d = new Date(iso);
  if (isNaN(d.getTime())) return iso;
  try {
    return d.toLocaleString("en-GB", {
      timeZone: timeZone ?? undefined,
      weekday: "short",
      day: "numeric",
      month: "short",
      hour: "2-digit",
      minute: "2-digit",
    });
  } catch {
    // An unknown zone name should be impossible (the server validates it), and showing
    // the reader's own time silently would be the one wrong answer here.
    return `${d.toLocaleString("en-GB")} (zone unknown)`;
  }
}

/** The zones this browser knows, for the picker. */
function zoneOptions(current: string): { value: string; label: string }[] {
  const supported = (Intl as unknown as { supportedValuesOf?: (k: string) => string[] })
    .supportedValuesOf;
  const names = supported ? supported("timeZone") : [current || "UTC", "UTC"];
  const set = Array.from(new Set([current, ...names].filter(Boolean)));
  return set.map((z) => ({ value: z, label: z }));
}

function DiaryBlock({ owners }: { owners: { value: string; label: string }[] }) {
  const qc = useQueryClient();
  const { busy, run } = useBusy();
  const [owner, setOwner] = useState("");
  const chosen = owner || owners[0]?.value || "";
  const diary = useDiary(chosen || undefined);
  const appts = useAppointments(chosen || undefined);
  const [draft, setDraft] = useState<DiaryBody | null>(null);
  const [closure, setClosure] = useState("");
  const [closureNote, setClosureNote] = useState("");

  // The saved diary is the starting point; edits live in the draft until saved.
  const saved = diary.data ?? null;
  const d: DiaryBody = draft ?? {
    timezone: saved?.timezone ?? Intl.DateTimeFormat().resolvedOptions().timeZone ?? "UTC",
    slot_minutes: saved?.slot_minutes ?? 30,
    lead_minutes: saved?.lead_minutes ?? 120,
    horizon_days: saved?.horizon_days ?? 30,
    enabled: saved?.enabled ?? false,
    hours: saved?.hours ?? [],
  };
  const edit = (patch: Partial<DiaryBody>) => setDraft({ ...d, ...patch });

  const refresh = () => {
    qc.invalidateQueries({ queryKey: ["diary"] });
    qc.invalidateQueries({ queryKey: ["diary-appointments"] });
    qc.invalidateQueries({ queryKey: ["admin-telephony-numbers"] });
  };

  const save = () =>
    run("Save", async () => {
      await setDiary({ ...d, owner_user_id: chosen });
      setDraft(null);
      refresh();
    }, "Diary saved.");

  const addHours = (weekday: number) =>
    edit({ hours: [...d.hours, { weekday, opens_minute: 9 * 60, closes_minute: 17 * 60 }] });
  const dropHours = (i: number) => edit({ hours: d.hours.filter((_, n) => n !== i) });
  const setHours = (i: number, patch: Partial<DiaryOpening>) =>
    edit({ hours: d.hours.map((h, n) => (n === i ? { ...h, ...patch } : h)) });

  const addClosure = () =>
    run("Add", async () => {
      await addDiaryClosure({ owner_user_id: chosen, closed_on: closure, note: closureNote || null });
      setClosure("");
      setClosureNote("");
      refresh();
    });
  const dropClosure = (date: string) =>
    run("Remove", async () => {
      await removeDiaryClosure(date, chosen);
      refresh();
    });
  const cancel = async (a: Appointment) => {
    const ok = await confirmDialog({
      title: `Cancel ${a.caller_name}'s appointment?`,
      body: "The time becomes free again and can be offered to somebody else. Nobody is told: ring them if they need to know.",
      danger: true,
      confirmLabel: "Cancel it",
    });
    if (!ok) return;
    run("Cancel", () => cancelAppointment(a.id).then(refresh), "Cancelled.");
  };

  if (owners.length === 0) return null;
  const rows = appts.data ?? [];
  const dirty = draft !== null;
  return (
    <div className="mt-8">
      <h3 className="mb-2 font-serif text-lg text-slate-lightest">Diary</h3>
      <p className="mb-1 text-xs text-slate/70">
        When this account is open, and therefore what times a caller can be offered and take. The
        agent is never told the opening hours and never works a time out for itself: it asks what is
        free and reads back what it is given, so nothing outside these hours can be booked.
      </p>
      <p className="mb-3 text-xs text-slate/70">
        Every time here is shown in the diary's own zone, not yours. An appointment is always the
        length set below, which is what makes it impossible for two callers to take one slot.
      </p>

      <div className="mb-3 flex flex-wrap items-center gap-2">
        {owners.length > 1 && (
          <Dropdown value={chosen} onChange={(v) => { setOwner(v); setDraft(null); }} ariaLabel="Whose diary" options={owners} />
        )}
        {owners.length === 1 && <span className="text-sm text-slate">{owners[0].label}</span>}
        <Badge tone={saved?.enabled ? "green" : "slate"}>{saved?.enabled ? "Taking bookings" : "Off"}</Badge>
      </div>

      {diary.isLoading && <p className="text-sm text-slate">Loading…</p>}
      {!diary.isLoading && (
        <div className="mb-6 max-w-2xl space-y-3 rounded-xl border border-navy-lighter p-4">
          <label className="block text-sm text-slate-lightest">Time zone
            <div className="mt-1">
              <Dropdown value={d.timezone} onChange={(v) => edit({ timezone: v })} ariaLabel="The diary's time zone" fullWidth options={zoneOptions(d.timezone)} />
            </div>
            <span className="mt-1 block text-[11px] text-slate/50">The opening hours below are in this zone, so they stay right when the clocks change.</span>
          </label>
          <div className="flex flex-wrap gap-3">
            <label className="block text-sm text-slate-lightest">Appointment length (minutes)
              <input className={INPUT + " mt-1 w-32"} value={String(d.slot_minutes)} onChange={(e) => edit({ slot_minutes: Number(e.target.value) || 30 })} />
            </label>
            <label className="block text-sm text-slate-lightest">Soonest (minutes from now)
              <input className={INPUT + " mt-1 w-32"} value={String(d.lead_minutes)} onChange={(e) => edit({ lead_minutes: Number(e.target.value) || 0 })} />
            </label>
            <label className="block text-sm text-slate-lightest">How far ahead (days)
              <input className={INPUT + " mt-1 w-32"} value={String(d.horizon_days)} onChange={(e) => edit({ horizon_days: Number(e.target.value) || 1 })} />
            </label>
          </div>

          <div>
            <span className={LABEL}>Opening hours</span>
            {WEEKDAYS.map((name, weekday) => (
              <div key={weekday} className="mt-1 flex flex-wrap items-center gap-2">
                <span className="w-24 text-sm text-slate">{name}</span>
                {d.hours.map((h, i) =>
                  h.weekday !== weekday ? null : (
                    <span key={i} className="flex items-center gap-1">
                      <input
                        className={INPUT + " w-20"}
                        value={asClock(h.opens_minute)}
                        onChange={(e) => { const m = asMinutes(e.target.value); if (m !== null) setHours(i, { opens_minute: m }); }}
                      />
                      <span className="text-xs text-slate/60">to</span>
                      <input
                        className={INPUT + " w-20"}
                        value={asClock(h.closes_minute)}
                        onChange={(e) => { const m = asMinutes(e.target.value); if (m !== null) setHours(i, { closes_minute: m }); }}
                      />
                      <button className={BTN2} onClick={() => dropHours(i)}>Remove</button>
                    </span>
                  ),
                )}
                <button className={BTN2} onClick={() => addHours(weekday)}>Add</button>
              </div>
            ))}
            <span className="mt-1 block text-[11px] text-slate/50">Two periods on a day is how a lunch break is written. A day with none is a day the account is shut.</span>
          </div>

          <label className="flex items-center gap-2 text-sm text-slate-lightest">
            <input type="checkbox" checked={d.enabled} onChange={(e) => edit({ enabled: e.target.checked })} /> Take bookings by telephone
          </label>
          <div className="flex gap-2">
            <button className={BTN} disabled={!!busy || !dirty} onClick={save}>Save diary</button>
            {dirty && <button className={BTN2} disabled={!!busy} onClick={() => setDraft(null)}>Discard</button>}
          </div>
        </div>
      )}

      <h4 className="mb-2 text-sm text-slate-lightest">Days shut</h4>
      <div className="mb-2 flex flex-wrap items-end gap-2">
        <label className="block text-sm text-slate-lightest">Date
          <input className={INPUT + " mt-1 w-40"} placeholder="2026-12-25" value={closure} onChange={(e) => setClosure(e.target.value)} />
        </label>
        <label className="block text-sm text-slate-lightest">Note
          <input className={INPUT + " mt-1 w-56"} placeholder="Christmas Day" value={closureNote} onChange={(e) => setClosureNote(e.target.value)} />
        </label>
        <button className={BTN2} disabled={!!busy || closure.trim() === ""} onClick={addClosure}>Add</button>
      </div>
      {(saved?.closures ?? []).length === 0 && <p className="mb-4 text-sm text-slate">No days shut, so only the opening hours above apply.</p>}
      {(saved?.closures ?? []).length > 0 && (
        <ul className="mb-4 flex flex-wrap gap-2">
          {(saved?.closures ?? []).map((c) => (
            <li key={c.closed_on} className="flex items-center gap-2 rounded-lg border border-navy-lighter px-2 py-1 text-xs text-slate-lightest">
              <span className="mono">{c.closed_on}</span>
              {c.note && <span className="text-slate/60">{c.note}</span>}
              <button className="text-urgency-red" disabled={!!busy} onClick={() => dropClosure(c.closed_on)}>×</button>
            </li>
          ))}
        </ul>
      )}

      <h4 className="mb-2 text-sm text-slate-lightest">Coming in</h4>
      {appts.isLoading && <p className="text-sm text-slate">Loading…</p>}
      {!appts.isLoading && rows.length === 0 && <p className="text-sm text-slate">Nothing booked.</p>}
      {rows.length > 0 && (
        <TableScroll>
          <table className="w-full border-collapse text-sm">
            <thead>
              <tr>
                <th className={TH}>When</th>
                <th className={TH}>Who</th>
                <th className={TH}>What about</th>
                <th className={TH}>Reference</th>
                <th className={TH}>Actions</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((a) => (
                <tr key={a.id}>
                  <td className={TD}>{inZone(a.starts_at, a.timezone)}</td>
                  <td className={TD}>
                    {a.caller_name}
                    {a.caller_e164 && <span className="mt-0.5 block text-[11px] text-slate/60 mono">{a.caller_e164}</span>}
                    {a.contact && <span className="mt-0.5 block text-[11px] text-slate/60">{a.contact}</span>}
                  </td>
                  <td className={TD}>{a.subject}</td>
                  <td className={TD + " mono"}>{a.reference}</td>
                  <td className={TD}>
                    <button className={BTN_DANGER} disabled={!!busy} onClick={() => cancel(a)}>Cancel</button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </TableScroll>
      )}
    </div>
  );
}

// Listening to a call, where its line kept the sound.
//
// Fetched rather than linked: the address needs the session behind it, and every listen is
// recorded in the audit trail, so the player is opened deliberately rather than by a page
// that happens to render.
function RecordingCell({ call, onChange }: { call: CallRecord; onChange: () => void }) {
  const { busy, run } = useBusy();
  const [url, setUrl] = useState<string | null>(null);
  // The object URL is this component's to release, and it holds a copy of somebody's voice.
  useEffect(() => () => { if (url) URL.revokeObjectURL(url); }, [url]);

  if (call.recording_seconds == null) {
    return call.recording_failed
      ? <span className="text-[11px] text-urgency-red" title="The line was set to record and the recording did not survive.">recording failed</span>
      : <span className="text-slate/60">—</span>;
  }
  const mins = Math.floor(call.recording_seconds / 60);
  const secs = call.recording_seconds % 60;
  const size = call.recording_bytes ? `${(call.recording_bytes / (1024 * 1024)).toFixed(1)} MB` : "";

  const listen = () => run("Listen", async () => { setUrl(await callRecordingUrl(call.id)); });
  const remove = async () => {
    const ok = await confirmDialog({
      title: "Delete this recording?",
      body: "The sound of the call is deleted and cannot be recovered. What was said stays in the conversation, and the call stays in the log.",
      danger: true,
      confirmLabel: "Delete",
    });
    if (!ok) return;
    run("Delete", () => deleteCallRecording(call.id).then(() => { setUrl(null); onChange(); }), "Recording deleted.");
  };

  return (
    <div>
      {url ? (
        <audio controls src={url} className="h-8 w-44" />
      ) : (
        <button className="text-gold hover:underline" disabled={!!busy} onClick={listen}>Listen</button>
      )}
      <span className="mt-0.5 block text-[11px] text-slate/60">
        {mins}:{String(secs).padStart(2, "0")}{size ? ` · ${size}` : ""}
        {" · "}
        <button className="hover:underline" disabled={!!busy} onClick={remove}>delete</button>
      </span>
    </div>
  );
}

// Whether a call will actually work, asked before somebody rings the number.
//
// The same findings the deployment's own settings screen shows. Here because the person
// who registers a number is usually not the person who configured the carrier, and being
// told what is wrong is what stops a line being blamed for a deployment's fault.
function ReadinessBlock() {
  const { busy, run } = useBusy();
  const [checks, setChecks] = useState<TelephonyCheck[] | null>(null);
  const look = () =>
    run("Check", async () => {
      setChecks(await runTelephonyCheck());
    });
  const wrong = (checks ?? []).filter((c) => !c.ok);

  return (
    <div className="mb-5 rounded-xl border border-navy-lighter p-4">
      <div className="flex items-center justify-between gap-3">
        <div>
          <h3 className="font-serif text-lg text-slate-lightest">Will a call work?</h3>
          <p className="mt-1 text-xs text-slate/70">
            Asks the questions a call asks, in the order it asks them, including a real test request to
            the speech engine: a deployment that cannot speak answers a call and ends it, and the person
            who finds out is otherwise a caller.
          </p>
        </div>
        <button className={BTN2} disabled={!!busy} onClick={look}>{busy ? "Checking…" : "Check"}</button>
      </div>
      {checks && (
        <div className="mt-3">
          <p className="mb-2 text-xs text-slate/70">
            {wrong.length === 0
              ? "Everything a call needs is in place."
              : `${wrong.length} of ${checks.length} need attention. The deployment settings behind these are set by whoever operates this instance.`}
          </p>
          <ul className="space-y-2">
            {checks.map((c) => (
              <li key={c.id} className="text-sm">
                <span className={c.ok ? "text-emerald-400" : "text-urgency-red"}>{c.ok ? "✓" : "✗"}</span>{" "}
                <span className="text-slate-lightest">{c.title}</span>
                <span className="mt-0.5 block text-[11px] text-slate/60">{c.detail}</span>
                {c.fix && <span className="mt-0.5 block text-[11px] text-urgency-red">{c.fix}</span>}
              </li>
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}

// Where an account is told, outside this deployment, about what its lines took.
//
// The address is never shown back: it is a credential, and anybody holding it can post
// into that channel. What a reader sees is the host and the events, which is enough to
// know what is arranged without being enough to use it.
const EVENT_LABELS: Record<string, string> = {
  message_taken: "A message is taken",
  appointment_booked: "An appointment is booked",
  appointment_moved: "An appointment is moved",
  appointment_cancelled: "An appointment is cancelled",
};

type TargetDraft = { label: string; kind: string; url: string; events: string[] };
const EMPTY_TARGET: TargetDraft = { label: "", kind: "slack", url: "", events: ["message_taken"] };

function NotificationsBlock({ owners }: { owners: { value: string; label: string }[] }) {
  const qc = useQueryClient();
  const { busy, run } = useBusy();
  const [owner, setOwner] = useState("");
  const chosen = owner || owners[0]?.value || "";
  const q = useNotifyTargets(chosen || undefined, !!chosen);
  const [adding, setAdding] = useState(false);
  const [draft, setDraft] = useState<TargetDraft>(EMPTY_TARGET);
  const setD = (k: keyof TargetDraft, v: string | string[]) => setDraft((p) => ({ ...p, [k]: v }));
  const refresh = () => qc.invalidateQueries({ queryKey: ["notify-targets"] });

  const toggleEvent = (e: string) =>
    setD("events", draft.events.includes(e) ? draft.events.filter((x) => x !== e) : [...draft.events, e]);

  const save = () =>
    run(
      "Add",
      async () => {
        await createNotifyTarget({
          owner_user_id: chosen,
          label: draft.label.trim(),
          kind: draft.kind,
          url: draft.url.trim(),
          events: draft.events,
        });
        setDraft(EMPTY_TARGET);
        setAdding(false);
        refresh();
      },
      "Target added.",
    );

  const toggle = (t: NotifyTarget) =>
    run(
      t.enabled ? "Switch off" : "Switch on",
      () => updateNotifyTarget(t.id, { enabled: !t.enabled }).then(refresh),
      t.enabled ? "Switched off." : "Switched on.",
    );

  const probe = (t: NotifyTarget) =>
    run("Test", () => testNotifyTarget(t.id), "A test line was sent.");

  const remove = async (t: NotifyTarget) => {
    const ok = await confirmDialog({
      title: `Stop telling ${t.label}?`,
      body: "Nothing more is sent there. What was already taken is unaffected.",
      danger: true,
      confirmLabel: "Remove",
    });
    if (!ok) return;
    run("Remove", () => deleteNotifyTarget(t.id).then(refresh), "Target removed.");
  };

  const rows = q.data ?? [];
  return (
    <div className="mt-8">
      <h3 className="mb-2 font-serif text-lg text-slate-lightest">Telling somebody outside</h3>
      <p className="mb-3 text-xs text-slate/70">
        A message taken at four in the afternoon is no use if it is seen tomorrow. Post a line into a
        chat channel instead: who rang and what it is about, and a way back into here for the rest.
        What a caller actually said never leaves. Outward notifications have to be switched on for
        this deployment before anything is sent.
      </p>
      {owners.length > 1 && (
        <div className="mb-3">
          <Dropdown value={chosen} onChange={setOwner} ariaLabel="Whose lines" options={owners} />
        </div>
      )}
      <div className="mb-3">
        <button className={BTN} disabled={!!busy || adding || !chosen} onClick={() => setAdding(true)}>
          Add somewhere to tell
        </button>
      </div>

      {adding && (
        <div className="mb-4 max-w-xl space-y-3 rounded-xl border border-navy-lighter p-4">
          <label className="block text-sm text-slate-lightest">Name
            <input className={INPUT + " mt-1 w-full"} placeholder="Reception channel" value={draft.label} onChange={(e) => setD("label", e.target.value)} />
          </label>
          <label className="block text-sm text-slate-lightest">Kind
            <div className="mt-1">
              <Dropdown
                value={draft.kind}
                onChange={(v) => setD("kind", v)}
                ariaLabel="What kind of service"
                fullWidth
                options={[
                  { value: "slack", label: "Slack" },
                  { value: "teams", label: "Teams" },
                  { value: "webhook", label: "Anything that accepts a posted message" },
                ]}
              />
            </div>
          </label>
          <label className="block text-sm text-slate-lightest">Address
            <input className={INPUT + " mt-1 w-full"} placeholder="https://hooks.slack.com/services/…" value={draft.url} onChange={(e) => setD("url", e.target.value)} />
            <span className="mt-1 block text-[11px] text-slate/50">
              An incoming webhook address. It is stored encrypted and never shown again, because
              anybody who has it can post into that channel. A plain http address is only accepted on
              this deployment's own network.
            </span>
          </label>
          <div className="text-sm text-slate-lightest">Tell it when
            <div className="mt-1 space-y-1">
              {NOTIFY_EVENTS.map((e) => (
                <label key={e} className="flex items-center gap-2 text-sm text-slate-lightest">
                  <input type="checkbox" checked={draft.events.includes(e)} onChange={() => toggleEvent(e)} />
                  {EVENT_LABELS[e] ?? e}
                </label>
              ))}
            </div>
          </div>
          <div className="flex gap-2">
            <button className={BTN} disabled={!!busy || !draft.label.trim() || !draft.url.trim()} onClick={save}>Add</button>
            <button className={BTN2} disabled={!!busy} onClick={() => { setAdding(false); setDraft(EMPTY_TARGET); }}>Cancel</button>
          </div>
        </div>
      )}

      {q.isLoading && <p className="text-sm text-slate">Loading…</p>}
      {!q.isLoading && rows.length === 0 && (
        <p className="text-sm text-slate">Nobody outside is told anything yet.</p>
      )}
      {rows.length > 0 && (
        <TableScroll>
          <table className="w-full border-collapse text-sm">
            <thead>
              <tr>
                <th className={TH}>Name</th>
                <th className={TH}>Kind</th>
                <th className={TH}>Where</th>
                <th className={TH}>Told when</th>
                <th className={TH}>State</th>
                <th className={TH}>Actions</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((t) => (
                <tr key={t.id}>
                  <td className={TD}>{t.label}</td>
                  <td className={TD}>{t.kind}</td>
                  <td className={TD + " mono"}>{t.host}</td>
                  <td className={TD}>
                    {t.events.length === 0
                      ? <span className="text-slate/60">nothing</span>
                      : t.events.map((e) => EVENT_LABELS[e] ?? e).join(", ")}
                  </td>
                  <td className={TD}><Badge tone={t.enabled ? "green" : "slate"}>{t.enabled ? "On" : "Off"}</Badge></td>
                  <td className={TD}>
                    <div className="flex gap-2">
                      <button className={BTN2} disabled={!!busy} onClick={() => probe(t)}>Test</button>
                      <button className={BTN2} disabled={!!busy} onClick={() => toggle(t)}>{t.enabled ? "Switch off" : "Switch on"}</button>
                      <button className={BTN_DANGER} disabled={!!busy} onClick={() => remove(t)}>Remove</button>
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </TableScroll>
      )}
    </div>
  );
}

// What the lines do with what they are told, in one place.
//
// Assembled from the settings themselves every time it is read, so it cannot drift from
// what the deployment actually does, which is the failing of every assessment written down
// once in a document. Gated on owning the lines rather than on being able to wire one, like
// the screening list and the diary.
function ComplianceBlock({ owners }: { owners: { value: string; label: string }[] }) {
  const [owner, setOwner] = useState("");
  const chosen = owner || owners[0]?.value || "";
  const q = useTelephonyCompliance(chosen || undefined, !!chosen);
  const record = q.data ?? null;

  // The whole record as plain text, so it can go into an assessment without being retyped.
  const asText = (): string => {
    if (!record) return "";
    const at = new Date(record.as_at_epoch * 1000).toISOString();
    const lines = record.lines
      .map(
        (l) =>
          [
            `${l.e164}${l.label ? ` (${l.label})` : ""}${l.enabled ? "" : " [not answering]"}`,
            `  Said to every caller: ${l.spoken_to_callers}`,
            `  Callers put through to: ${l.transfers_to ?? "nobody"}`,
            `  Conversations kept: ${l.transcript_days > 0 ? `${l.transcript_days} days` : "indefinitely"}`,
            `  Sound of the call kept: ${l.records_calls ? `${l.recording_days} days` : "not recorded"}`,
            `  Call records kept: ${l.log_days > 0 ? `${l.log_days} days` : "indefinitely"}`,
            `  Calls taken: ${l.calls}, of which told what they were speaking to: ${l.calls_with_notice}`,
          ].join("\n"),
      )
      .join("\n");
    const held = record.holdings
      .map((h) => `${h.held} (${h.rows}): ${h.contents}\n  Kept: ${h.kept}`)
      .join("\n");
    return [
      `Telephone processing record, as at ${at}`,
      "",
      "Lines",
      lines || "  None registered.",
      "",
      "What is held",
      held,
      "",
      record.no_audio_is_kept
        ? "No audio is kept at any point: speech is recognised as it arrives and discarded."
        : "The sound of calls is kept on the lines marked as recording, and callers on those lines are told so before they say anything.",
      "What leaves this deployment:",
      ...record.leaves_the_deployment.map((l) => `  ${l}`),
    ].join("\n");
  };

  return (
    <div className="mt-8">
      <h3 className="mb-2 font-serif text-lg text-slate-lightest">What these lines do with what they are told</h3>
      <p className="mb-3 text-xs text-slate/70">
        Read from the settings as they stand, so it is true of this deployment today rather than of the
        day somebody wrote it down. Useful as the starting point for an assessment of how the line
        handles personal information. Like the screening list and the diary, it belongs to the account
        whose lines these are.
      </p>
      {owners.length > 1 && (
        <div className="mb-3">
          <Dropdown value={chosen} onChange={setOwner} ariaLabel="Whose lines" options={owners} />
        </div>
      )}
      {!chosen && <p className="text-sm text-slate">No lines yet.</p>}
      {q.isLoading && <p className="text-sm text-slate">Loading…</p>}
      {q.isError && <p className="text-sm text-slate">That account's record is not yours to read.</p>}
      {record && (
        <>
          <TableScroll>
            <table className="mb-4 w-full border-collapse text-sm">
              <thead>
                <tr>
                  <th className={TH}>Line</th>
                  <th className={TH}>Said to every caller</th>
                  <th className={TH}>A person can be reached</th>
                  <th className={TH}>Conversations kept</th>
                  <th className={TH}>Call records kept</th>
                  <th className={TH}>Sound kept</th>
                  <th className={TH}>Calls told</th>
                </tr>
              </thead>
              <tbody>
                {record.lines.map((l) => (
                  <tr key={l.id}>
                    <td className={TD + " mono"}>
                      {l.e164}
                      {!l.enabled && <span className="mt-0.5 block text-[11px] text-slate/60">not answering</span>}
                    </td>
                    <td className={TD}>
                      {l.spoken_to_callers}
                      {l.notice_is_standard && <span className="mt-0.5 block text-[11px] text-slate/60">standard notice</span>}
                    </td>
                    <td className={TD + " mono"}>{l.transfers_to ?? <span className="font-sans text-slate/60">no</span>}</td>
                    <td className={TD}>{l.transcript_days > 0 ? `${l.transcript_days} days` : "indefinitely"}</td>
                    <td className={TD}>{l.log_days > 0 ? `${l.log_days} days` : "indefinitely"}</td>
                    <td className={TD}>{l.records_calls ? `${l.recording_days} days` : "not recorded"}</td>
                    <td className={TD}>
                      {l.calls_with_notice === l.calls
                        ? <Badge tone="green">{l.calls} of {l.calls}</Badge>
                        : <Badge tone="red">{l.calls_with_notice} of {l.calls}</Badge>}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </TableScroll>

          <TableScroll>
            <table className="mb-4 w-full border-collapse text-sm">
              <thead>
                <tr>
                  <th className={TH}>Held</th>
                  <th className={TH}>What is in it</th>
                  <th className={TH}>How long</th>
                  <th className={TH}>How many</th>
                </tr>
              </thead>
              <tbody>
                {record.holdings.map((h) => (
                  <tr key={h.held}>
                    <td className={TD}>{h.held}</td>
                    <td className={TD}>{h.contents}</td>
                    <td className={TD}>{h.kept}</td>
                    <td className={TD}>{h.rows}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </TableScroll>

          <ul className="mb-3 list-disc pl-5 text-xs text-slate/70">
            <li>
              {record.no_audio_is_kept
                ? "No audio is kept at any point: speech is recognised as it arrives and discarded."
                : "The sound of calls is kept on the lines marked as recording, and callers on those lines are told so before they say anything. No audio at all is kept on the others."}
            </li>
            {record.leaves_the_deployment.map((l) => (
              <li key={l}>{l}</li>
            ))}
            <li>
              Callers are {record.screening_names > 0 ? `checked against ${record.screening_names} names on this account's list` : "not checked against any list"};
              appointments {record.diary_enabled ? "can be arranged by telephone" : "cannot be arranged by telephone"}.
            </li>
          </ul>
          <button
            className={BTN2}
            onClick={() => {
              void navigator.clipboard
                .writeText(asText())
                .then(() => toast("Record copied."))
                .catch(() => toast("Could not copy the record."));
            }}
          >
            Copy as text
          </button>
        </>
      )}
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
        <TableScroll>
          <table className="w-full border-collapse text-sm">
            <thead><tr><th className={TH}>Tool</th><th className={TH}>Badges</th><th className={TH}>State</th><th className={TH}></th></tr></thead>
            <tbody>
              {(cat.data?.native ?? []).map((t) => (
                <Fragment key={t.name}>
                  <tr>
                    <td className={TD}>{t.label} <span className="text-xs text-slate/60">({t.name})</span></td>
                    <td className={TD}>
                      <Badge tone={t.effect === "run" ? "gold" : "slate"}>{t.effect}</Badge>
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
        </TableScroll>
      )}

      {tab === "mcp" && !cat.isLoading && (
        <div>
          <p className="mb-2 text-xs text-slate/70">Active MCP servers (read-only). Register, approve and remove them in the <strong>MCP Servers</strong> tab.</p>
          {(cat.data?.mcp ?? []).length === 0 ? <p className="text-sm text-slate">No active MCP servers.</p> : (
            <TableScroll>
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
            </TableScroll>
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
  allow_on_call: false,
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
  const [allowOnCall, setAllowOnCall] = useState(false);
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
    setAllowOnCall(t.allow_on_call);
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
        allow_on_call: allowOnCall,
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
        <TableScroll className="mt-3">
          <table className="w-full border-collapse text-sm">
            <thead><tr><th className={TH}>Name</th><th className={TH}>Kind</th><th className={TH}>State</th><th className={TH}>Version</th><th className={TH}></th></tr></thead>
            <tbody>
              {tools.map((t) => (
                <Fragment key={t.id}>
                  <tr>
                    <td className={TD}>{t.display_name || t.name} <span className="text-xs text-slate/60">({t.name})</span></td>
                    <td className={TD}>{t.kind}{t.requires_egress && <span className="ml-1"><Badge tone="gold">egress</Badge></span>}{t.side_effecting && <span className="ml-1"><Badge>approval</Badge></span>}{t.allow_on_call && <span className="ml-1"><Badge tone="green">on calls</Badge></span>}</td>
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
        </TableScroll>
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
            <label className="flex items-center gap-1 text-xs text-slate/80"><input type="checkbox" checked={allowOnCall} onChange={(e) => setAllowOnCall(e.target.checked)} /> may be used during a telephone call</label>
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
registerAdminSection({ key: "telephony", label: "Telephone", component: TelephonySection, capability: "telephony", permission: "telephony.manage" });
registerAdminSection({ key: "announcements", label: "Announcements", component: AnnouncementsSection, permission: "announcements.manage" });
registerAdminSection({ key: "system", label: "System", component: SystemSection });
