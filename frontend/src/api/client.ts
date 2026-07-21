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

import { QueryClient, useQuery } from "@tanstack/react-query";
import { freshToken } from "@/auth/keycloak";
import { authMode } from "@/auth/config";
import type { Citation } from "@/ws/protocol";

export const queryClient = new QueryClient({
  // Freshness defaults: every screen open / tab-switch remount refetches
  // (refetchOnMount 'always'), and a refocus/reconnect refetches once data is
  // briefly stale — so navigation always shows current data without a reload.
  // Per-query overrides (pollers, staleTime:0 audit/feedback) still win.
  defaultOptions: {
    queries: {
      staleTime: 5_000,
      retry: 1,
      refetchOnMount: "always",
      refetchOnWindowFocus: true,
      refetchOnReconnect: true,
    },
  },
});

/** Authenticated fetch against the backend (same-origin; Vite proxies in dev).
 *  Keycloak mode attaches a Bearer token; local mode relies on the session cookie
 *  (`credentials: "include"`) and sends no token. */
export async function apiFetch<T = unknown>(path: string, init: RequestInit = {}): Promise<T> {
  const headers = new Headers(init.headers);
  if (authMode() === "keycloak") {
    headers.set("Authorization", `Bearer ${await freshToken()}`);
  }
  if (init.body && !headers.has("Content-Type")) headers.set("Content-Type", "application/json");
  const res = await fetch(path, { ...init, headers, credentials: "include" });
  if (!res.ok) {
    const body = await res.text().catch(() => "");
    throw new Error(`${res.status} ${res.statusText}: ${body.slice(0, 200)}`);
  }
  if (res.status === 204) return undefined as T;
  const ct = res.headers.get("content-type") ?? "";
  return (ct.includes("application/json") ? await res.json() : await res.text()) as T;
}

// ── Typed shapes the spine uses ──────────────────────────────────────────────

/** Capability-aware reasoning descriptor for the effective llm model.
 * `mode`: none → hide the control; toggle → on/off; levels/budget → segments; always_on → no Off. */
export interface ReasoningCapability {
  mode: "none" | "toggle" | "levels" | "budget" | "always_on";
  levels: string[];
  can_disable: boolean;
  supports_trace: boolean;
}

export interface WhoAmI {
  user_id: string;
  email: string | null;
  display_name: string | null;
  role: string;
  break_glass: boolean;
  /** True for a restricted enrol-only local session: `auth.require_mfa`
   *  is on and the caller has not yet enrolled a factor. The SPA forces the enrol
   *  wizard and hides everything else until this clears. */
  mfa_enroll_only: boolean;
  // (ReasoningCapability defined below WhoAmI.)
  /** True iff the caller has ≥1 moderator assignment (gates the Moderation tab). */
  is_moderator: boolean;
  /** Epoch (s) of the caller's last avatar change; null = no avatar. */
  avatar_updated_at: number | null;
  /** First-run onboarding: false ⇒ no LLM provider is configured yet (empty
   *  deployment) → the SPA shows the setup checklist on the empty chat. */
  llm_configured: boolean;
  /** Fine-grained admin permissions the caller holds (custom RBAC). Empty in Core
   *  → gate admin sections by `is_admin`. A scoped-only holding is `perm:scoped`. */
  permissions: string[];
  capabilities: { code_interpreter: boolean; voice: boolean; voice_live: boolean; dictation_streaming: boolean; workflows: boolean; groundedness: boolean; groundedness_repair: boolean; mcp: boolean; messaging: boolean; public_api: boolean; white_label: boolean; compliance_audit: boolean; moderation: boolean; message_review: boolean; data_owner_approval: boolean; federated_sso: boolean; custom_rbac: boolean; enterprise_connectors: boolean; reasoning: ReasoningCapability };
  /** Live-voice client dials (present only when `capabilities.voice_live`). */
  voice_live_opts: { ptt_default: boolean; aec_required: boolean; silence_threshold_ms: number } | null;
}

export interface AgentSummary {
  id: string;
  name: string;
  description: string | null;
  tools: string[];
  sector: string | null;
  /** Workmodes this agent appears in (general/legal/research). */
  modes: string[];
  /** May the caller edit/delete this agent? (owner or admin; shared/seeded = false) */
  can_manage: boolean;
}

export function useWhoami() {
  return useQuery({ queryKey: ["whoami"], queryFn: () => apiFetch<WhoAmI>("/api/whoami") });
}

export function useAgents() {
  return useQuery({ queryKey: ["agents"], queryFn: () => apiFetch<AgentSummary[]>("/api/agents") });
}

export function createAgent(
  name: string,
  systemPrompt: string,
  tools: string[] = [],
  description?: string,
  params?: AgentParams,
  projectKnowledgeIds?: string[],
  sector?: string | null,
  modes: string[] = [],
): Promise<{ id: string }> {
  return apiFetch<{ id: string }>("/api/agents", {
    method: "POST",
    body: JSON.stringify({ name, system_prompt: systemPrompt, tools, description, params, project_knowledge_ids: projectKnowledgeIds, sector: sector || undefined, modes }),
  });
}

// ── Agent runs (action-taking agents): approve/reject a paused gated action ───
export interface AgentRunEvent { action: string; occurred_epoch: number | null; payload: Record<string, unknown> | null }
export interface AgentRunDetail {
  id: string;
  status: string;
  step_count: number;
  agent_id: string | null;
  chat_id: string | null;
  pending_tool: string | null;
  created_epoch: number;
  finished_epoch: number | null;
  events: AgentRunEvent[];
}
export function approveAgentRun(runId: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/agent-runs/${runId}/approve`, { method: "POST" });
}
export function rejectAgentRun(runId: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/agent-runs/${runId}/reject`, { method: "POST" });
}
/** Stop a RUNNING agent-run (e.g. a Deep Research run) — drops the kill-token;
 * the run aborts and discards any in-flight result. */
export function cancelAgentRun(runId: string): Promise<{ ok: boolean; status: string }> {
  return apiFetch(`/api/agent-runs/${runId}/cancel`, { method: "POST" });
}
export function useAgentRun(runId: string | undefined) {
  return useQuery({
    queryKey: ["agent-run", runId],
    queryFn: () => apiFetch<AgentRunDetail>(`/api/agent-runs/${runId}`),
    enabled: !!runId,
  });
}
export interface AgentRunSummary {
  id: string;
  status: string;
  step_count: number;
  agent_id: string | null;
  pending_tool: string | null;
  created_epoch: number;
  finished_epoch: number | null;
}
export function useAgentRuns(chatId: string | undefined) {
  return useQuery({
    queryKey: ["agent-runs", chatId],
    queryFn: () => apiFetch<AgentRunSummary[]>(`/api/agent-runs?chat_id=${chatId}`),
    enabled: !!chatId,
    refetchInterval: (q) =>
      (q.state.data as AgentRunSummary[] | undefined)?.some((r) => r.status === "running" || r.status === "awaiting_approval") ? 3000 : false,
  });
}
export interface PendingApproval {
  run_id: string;
  tool: string | null;
  summary: string;
  context: string;
  created_epoch: number;
}
export function usePendingApprovals() {
  return useQuery({
    queryKey: ["pending-approvals"],
    queryFn: () => apiFetch<PendingApproval[]>("/api/agent-runs/pending"),
    refetchInterval: 15000,
  });
}

// Matter-owner approval of group-membership adds (the confidentiality gate). Routed
// to project owners whose matters the membership would expose.
export interface PendingMemberRequest {
  id: string;
  group_id: string;
  group_name: string;
  target_user_id: string;
  target_name: string;
  requester_name: string;
  projects: { id: string; name: string }[];
  created_epoch: number;
}
export function usePendingMemberRequests(enabled = true) {
  return useQuery({
    queryKey: ["pending-member-requests"],
    queryFn: () => apiFetch<PendingMemberRequest[]>("/api/group-member-requests/pending"),
    refetchInterval: 15000,
    enabled,
  });
}
export function decideMemberRequest(id: string, approve: boolean): Promise<{ ok: boolean; status: string }> {
  return apiFetch(`/api/group-member-requests/${id}/${approve ? "approve" : "reject"}`, { method: "POST" });
}

/** Sampling/turn params the chat reads (all optional → engine/config defaults). */
export interface AgentParams {
  temperature?: number;
  top_p?: number;
  max_tokens?: number;
  frequency_penalty?: number;
  presence_penalty?: number;
  tool_concurrency?: number;
  /** Agent run-control: hard cap on tool-loop steps. */
  max_steps?: number;
  /** Web-search cap: the deepest effort class this Agent may request. */
  web_depth_max?: string;
  /** Web-search cap: max pages fetched per search (tightens, never widens). */
  web_max_fetches?: number;
}

export interface AgentDetail {
  id: string;
  name: string;
  description: string | null;
  system_prompt: string;
  params: AgentParams; // free-form JSON; we read the four keys above
  tools: string[];
  skills: { id: string; name: string }[];
  project_knowledge_ids: string[];
  sector: string | null;
  /** Workmodes this agent appears in (general/legal/research). */
  modes: string[];
  can_manage: boolean;
}

export interface SkillSummary {
  id: string;
  name: string;
  description: string;
  scope: string;
  /** May the caller edit/delete this skill? (owner or admin; seeded/default = false) */
  can_manage: boolean;
  /** Built-in default skill (applied to every agent). */
  is_default: boolean;
  /** Active? A disabled skill never enters the model's slot [2] / read_skill. */
  enabled: boolean;
}

/** One native tool in the catalogue (backend `tools::catalog` + `tool_overrides`).
 *  `dormant` = ships off; `capability` = host-gated (usable only when the whoami
 *  capability is true). Replaces the old hardcoded `AGENT_TOOL_CATALOG` — the
 *  backend catalogue endpoint is now the single source of truth for labels/hints. */
export interface NativeToolEntry {
  name: string;
  label: string;
  hint: string;
  /** State effect — mirrors the backend classifier. "run" = mutates state /
   *  runs code, so it makes the turn agentic (opens an agent run); it does NOT
   *  pause for approval. */
  effect: "read" | "proposal" | "run";
  /** Crosses the zero-egress perimeter. */
  egress: boolean;
  /** Host capability required to run this tool, else null. */
  capability: string | null;
  /** Ships off (needs an admin to enable a connector). */
  dormant: boolean;
  /** Always-on baseline (backend `tools::DEFAULT_TOOLS`); shown locked-on. */
  default: boolean;
  /** Effective on/off — an admin may switch a tool off per deployment. */
  enabled: boolean;
  /** Effective description advertised to the LLM (override or code default). */
  description: string;
  /** The code-default description, for preview and reset. */
  default_description: string;
  has_override: boolean;
}

/** Read-only slice of an active MCP server in the tool catalogue (CRUD lives in
 *  the MCP Servers admin tab). */
export interface McpCatalogEntry {
  name: string;
  slug: string;
  tool_count: number;
  requires_egress: boolean;
  status: string;
}

/** A deployment-defined custom tool. `config` shape
 *  (http): {method, url, headers{}, body?, auth{type,header_name?}, response{mode,pointer?}}. */
export interface CustomToolEntry {
  id: string;
  name: string;
  display_name: string;
  description: string;
  kind: "http" | "script";
  params_schema: unknown;
  config: unknown;
  requires_egress: boolean;
  side_effecting: boolean;
  enabled: boolean;
  version: number;
  approved_version: number | null;
  timeout_secs: number | null;
  has_secret: boolean;
  /** version === approved_version: live and dispatchable. */
  approved: boolean;
}

export interface ToolCatalog {
  native: NativeToolEntry[];
  mcp: McpCatalogEntry[];
  custom: CustomToolEntry[];
}

/** Create/update payload for a custom tool. `auth_value` is write-only: omit to
 *  leave the stored secret unchanged, "" to clear it. */
export interface CustomToolInput {
  name: string;
  display_name: string;
  description: string;
  kind: "http" | "script";
  params_schema: unknown;
  config: unknown;
  auth_value?: string;
  requires_egress: boolean;
  side_effecting: boolean;
  timeout_secs: number | null;
}

/** The tool catalogue — native tools with badges + effective state, active MCP
 *  servers, and custom tools. Fetched by the agent editor and the admin Tools tab. */
export function useToolCatalog() {
  return useQuery({
    queryKey: ["tools", "catalog"],
    queryFn: () => apiFetch<ToolCatalog>("/api/tools/catalog"),
  });
}

/** Set a native tool's override (on/off + description). A null/empty description
 *  clears the override text and reverts to the code default. Needs `tools.manage`. */
export function putNativeToolOverride(
  name: string,
  body: { enabled: boolean; description_override: string | null },
) {
  return apiFetch<NativeToolEntry>(`/api/admin/tools/native/${encodeURIComponent(name)}`, {
    method: "PUT",
    body: JSON.stringify(body),
  });
}

/** Reset a native tool to its code default (drops any override). Needs `tools.manage`. */
export function resetNativeTool(name: string) {
  return apiFetch<NativeToolEntry>(`/api/admin/tools/native/${encodeURIComponent(name)}`, {
    method: "DELETE",
  });
}

// ── Custom tools — all need `tools.manage` ──────────────────────────────────
export function createCustomTool(body: CustomToolInput) {
  return apiFetch<CustomToolEntry>("/api/admin/tools/custom", {
    method: "POST",
    body: JSON.stringify(body),
  });
}
export function updateCustomTool(id: string, body: CustomToolInput) {
  return apiFetch<CustomToolEntry>(`/api/admin/tools/custom/${id}`, {
    method: "PUT",
    body: JSON.stringify(body),
  });
}
export function enableCustomTool(id: string) {
  return apiFetch<CustomToolEntry>(`/api/admin/tools/custom/${id}/enable`, { method: "POST" });
}
export function disableCustomTool(id: string) {
  return apiFetch<CustomToolEntry>(`/api/admin/tools/custom/${id}/disable`, { method: "POST" });
}
export function deleteCustomTool(id: string) {
  return apiFetch<{ deleted: boolean }>(`/api/admin/tools/custom/${id}`, { method: "DELETE" });
}
export function testRunCustomTool(id: string, args: unknown) {
  return apiFetch<{ result: string }>(`/api/admin/tools/custom/${id}/test-run`, {
    method: "POST",
    body: JSON.stringify({ args }),
  });
}

export function useAgent(agentId: string | undefined) {
  return useQuery({
    queryKey: ["agent", agentId],
    queryFn: () => apiFetch<AgentDetail>(`/api/agents/${agentId}`),
    enabled: !!agentId,
  });
}

export function updateAgent(
  id: string,
  body: { name?: string; description?: string | null; system_prompt?: string; params?: AgentParams; tools?: string[]; project_knowledge_ids?: string[]; sector?: string | null; modes?: string[] },
): Promise<{ ok: boolean }> {
  return apiFetch<{ ok: boolean }>(`/api/agents/${id}`, { method: "PATCH", body: JSON.stringify(body) });
}

// Agent version history (Tier-2 #7) — snapshots on create/update, restore a prior one.
export interface AgentVersion {
  version_number: number;
  source: string; // "created" | "updated" | "rollback"
  created_at: string;
  created_by: string | null;
}
export function useAgentVersions(agentId: string | undefined) {
  return useQuery({
    queryKey: ["agent-versions", agentId],
    queryFn: () => apiFetch<AgentVersion[]>(`/api/agents/${agentId}/versions`),
    enabled: !!agentId,
  });
}
export function rollbackAgentVersion(agentId: string, versionNumber: number): Promise<{ ok: boolean; version: number }> {
  return apiFetch(`/api/agents/${agentId}/versions/${versionNumber}/rollback`, { method: "POST" });
}

export function deleteAgent(id: string): Promise<{ ok: boolean }> {
  return apiFetch<{ ok: boolean }>(`/api/agents/${id}`, { method: "DELETE" });
}

export function useSkills() {
  return useQuery({ queryKey: ["skills"], queryFn: () => apiFetch<SkillSummary[]>("/api/skills") });
}

export interface SkillDetail {
  id: string;
  name: string;
  description: string;
  body: string;
  scope: string;
  can_manage: boolean;
  is_default: boolean;
  enabled: boolean;
}

export function useSkill(id: string | undefined) {
  return useQuery({
    queryKey: ["skill", id],
    queryFn: () => apiFetch<SkillDetail>(`/api/skills/${id}`),
    enabled: !!id,
  });
}

export function createSkill(body: { name: string; description: string; body: string; scope?: string }): Promise<{ id: string }> {
  return apiFetch<{ id: string }>("/api/skills", { method: "POST", body: JSON.stringify(body) });
}

export function updateSkill(id: string, body: { name?: string; description?: string; body?: string }): Promise<{ ok: boolean }> {
  return apiFetch<{ ok: boolean }>(`/api/skills/${id}`, { method: "PATCH", body: JSON.stringify(body) });
}

export function deleteSkill(id: string): Promise<{ ok: boolean }> {
  return apiFetch<{ ok: boolean }>(`/api/skills/${id}`, { method: "DELETE" });
}

/** Enable/disable a skill (admin for defaults, owner for own). A disabled skill is
 *  kept but never reaches the model. */
export function setSkillEnabled(id: string, enabled: boolean): Promise<{ ok: boolean; enabled: boolean }> {
  return apiFetch(`/api/skills/${id}/enabled`, { method: "POST", body: JSON.stringify({ enabled }) });
}

export function testSkill(id: string, input: string): Promise<{ output: string }> {
  return apiFetch<{ output: string }>(`/api/skills/${id}/test`, { method: "POST", body: JSON.stringify({ input }) });
}

/** Thumbs feedback on an assistant message. */
export function submitFeedback(messageId: string, rating: "up" | "down", comment?: string): Promise<unknown> {
  const body: Record<string, unknown> = { rating };
  if (comment && comment.trim()) body.comment = comment.trim();
  return apiFetch(`/api/messages/${messageId}/feedback`, { method: "POST", body: JSON.stringify(body) });
}

// ── Citation source ──────────────────────────────────────────────────────────

export interface KnowledgeSource {
  filename: string;
  mime: string | null;
  text: string;
}

/** Extracted text of a Project-Knowledge source doc (chat citations point here). */
export function useKnowledgeDocSource(docId: string | null | undefined) {
  return useQuery({
    queryKey: ["knowledge-source", docId],
    queryFn: () => apiFetch<KnowledgeSource>(`/api/knowledge-docs/${docId}/source`),
    enabled: !!docId,
  });
}

export function attachSkill(agentId: string, skillId: string): Promise<unknown> {
  return apiFetch(`/api/agents/${agentId}/skills/${skillId}`, { method: "POST" });
}

export function detachSkill(agentId: string, skillId: string): Promise<unknown> {
  return apiFetch(`/api/agents/${agentId}/skills/${skillId}`, { method: "DELETE" });
}

// ── Admin console ────────────────────────────────────────────────────────────
// All gated server-side on is_admin() (client_admin | super_admin). No backend
// changes — these hit existing endpoints.

export const RESOURCE_TYPES = [
  "project", "agent", "document", "tabular_review", "project_knowledge",
  "skill", "prompt", "chat", "automation",
] as const;
export const PERMISSIONS = ["read", "write", "share", "delete"] as const;

// Sharing UI: only the resource types with a clean global name-list (so the admin
// picks by name, never a UUID); permissions exposed for granting (no `delete`).
export const GRANT_RESOURCE_TYPES: { value: string; label: string }[] = [
  { value: "project", label: "Project" },
  { value: "agent", label: "Agent" },
  { value: "skill", label: "Skill" },
  { value: "prompt", label: "Prompt" },
  { value: "automation", label: "Automation" },
  { value: "mcp_server", label: "MCP server" },
];
export const GRANT_PERMISSIONS: { value: string; label: string }[] = [
  { value: "read", label: "Read" },
  { value: "write", label: "Write" },
  { value: "share", label: "Share" },
];

/** Human-friendly token count for charts: 850, 12.4k, 1.20M. */
export function fmtTokens(n: number): string {
  if (n < 1000) return String(Math.round(n));
  if (n < 1e6) return (n / 1e3).toFixed(1).replace(/\.0$/, "") + "k";
  return (n / 1e6).toFixed(2).replace(/\.00$/, "") + "M";
}

// Admin feedback triage
export interface AdminFeedbackItem {
  id: string;
  rating: "up" | "down";
  comment: string | null;
  user_email: string | null;
  agent_name: string | null;
  model: string | null;
  message_excerpt: string;
  created_at: string;
}
export function useAdminFeedback(rating?: "up" | "down") {
  return useQuery({
    queryKey: ["admin-feedback", rating ?? "all"],
    queryFn: () => apiFetch<AdminFeedbackItem[]>(`/api/admin/feedback${rating ? `?rating=${rating}` : ""}`),
    // Triage view: each filter switch (and tab focus) should reflect new feedback
    // submitted elsewhere — don't serve a stale cached page.
    staleTime: 0,
    refetchOnMount: "always",
    refetchOnWindowFocus: true,
  });
}

// Users
export interface AdminUser {
  id: string;
  email: string;
  display_name: string;
  role: string;
  deactivated: boolean;
  /** "local" or "scim" — a directory/IdP-managed user is shown read-only. */
  managed_by: string;
  /** Whether the user has a confirmed second factor. */
  mfa_enabled: boolean;
}
export function useAdminUsers() {
  return useQuery({ queryKey: ["admin-users"], queryFn: () => apiFetch<AdminUser[]>("/api/admin/users") });
}
export function deactivateUser(id: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/admin/users/${id}/deactivate`, { method: "POST" });
}
export function reactivateUser(id: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/admin/users/${id}/reactivate`, { method: "POST" });
}
/** Admin clears a user's second factor (device lost, no recovery codes left). */
export function resetUserMfa(id: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/admin/users/${id}/mfa/reset`, { method: "POST" });
}

// ── Second factor (TOTP). Local-auth only; 409 under Keycloak. ──
export interface MfaStatus { enabled: boolean; recovery_remaining: number }
export interface MfaSetup { otpauth_url: string; secret: string }
export function mfaStatus(): Promise<MfaStatus> {
  return apiFetch<MfaStatus>("/api/auth/mfa/status");
}
/** Begin enrolment: returns a pending secret + otpauth URL (MFA not yet enabled). */
export function mfaSetup(): Promise<MfaSetup> {
  return apiFetch<MfaSetup>("/api/auth/mfa/setup", { method: "POST" });
}
/** Confirm a code → enables MFA, returns the one-time recovery codes. */
export function mfaConfirm(code: string): Promise<{ recovery_codes: string[] }> {
  return apiFetch("/api/auth/mfa/confirm", { method: "POST", body: JSON.stringify({ code }) });
}
/** Disable MFA — password AND a valid factor (TOTP code or recovery code) required. */
export function mfaDisable(password: string, code: string): Promise<{ ok: boolean }> {
  return apiFetch("/api/auth/mfa/disable", { method: "POST", body: JSON.stringify({ password, code }) });
}
/** Replace the recovery-code set (old ones invalidated), returned once. */
export function mfaRegenerate(password: string, code: string): Promise<{ recovery_codes: string[] }> {
  return apiFetch("/api/auth/mfa/recovery/regenerate", { method: "POST", body: JSON.stringify({ password, code }) });
}

// Usage analytics
export interface ModelRollup { model: string | null; prompt_tokens: number; completion_tokens: number; count: number }
export interface UserRollup { user_id: string | null; email: string | null; prompt_tokens: number; completion_tokens: number; count: number }
export interface AgentRollup { agent_id: string | null; agent_name: string | null; prompt_tokens: number; completion_tokens: number; count: number }
export interface DayPoint { day: string; tokens: number; messages: number }
export interface Analytics {
  per_model: ModelRollup[];
  per_user: UserRollup[];
  per_agent: AgentRollup[];
  total_prompt_tokens: number;
  total_completion_tokens: number;
  total_answers: number;
  series: DayPoint[];
  total_users: number;
  new_users_30: number;
  active_7: number;
  active_30: number;
}
export function useAnalytics() {
  return useQuery({ queryKey: ["admin-analytics"], queryFn: () => apiFetch<Analytics>("/api/admin/analytics") });
}

// Groundedness / verification dashboard (BACKLOG A1) — read-only aggregation over
// verification_runs, segmented by mode (live chat / draft+document).
export interface VerdictMix { supported: number; contradicted: number; not_mentioned: number }
export interface GroundednessDay { day: string; avg_score: number | null; runs: number }
export interface AgentGrounding { agent_id: string | null; agent_name: string | null; avg_score: number | null; runs: number }
export interface LiveInteraction {
  run_id: string; message_id: string; chat_id: string; snippet: string;
  score: number | null; flagged: number; created_at: string;
}
export interface DraftRun {
  run_id: string; target_type: string; status: string; score: number | null;
  supported: number; contradicted: number; not_mentioned: number; created_at: string;
}
export interface StatusCount { status: string; count: number }
export interface GroundednessAnalytics {
  live_runs: number;
  live_avg_score: number | null;
  live_verdicts: VerdictMix;
  live_cited_fraction: number | null;
  live_series: GroundednessDay[];
  per_agent: AgentGrounding[];
  lowest_interactions: LiveInteraction[];
  draft_runs: number;
  draft_avg_score: number | null;
  draft_verdicts: VerdictMix;
  draft_by_status: StatusCount[];
  draft_series: GroundednessDay[];
  recent_runs: DraftRun[];
}
export function useGroundednessAnalytics() {
  return useQuery({ queryKey: ["admin-groundedness"], queryFn: () => apiFetch<GroundednessAnalytics>("/api/admin/groundedness") });
}

// Power-user "lead" console — usage scoped to the teams the caller leads (groups
// they created + projects they own), and a full directory for building teams.
export interface PowerAnalytics {
  team_size: number;
  per_user: UserRollup[];
  per_agent: AgentRollup[];
  total_prompt_tokens: number;
  total_completion_tokens: number;
  total_answers: number;
}
export function usePowerAnalytics() {
  return useQuery({ queryKey: ["power-analytics"], queryFn: () => apiFetch<PowerAnalytics>("/api/power/analytics") });
}
export function usePowerDirectory() {
  return useQuery({ queryKey: ["power-directory"], queryFn: () => apiFetch<UserEntry[]>("/api/power/directory") });
}

// Access grants (sharing)
export interface Grant { id: string; principal_type: string; principal_id: string; permission: string }
export interface CreateGrantBody {
  resource_type: string;
  resource_id: string;
  principal_type: "user" | "group";
  principal_id: string;
  permission: string;
}
export function useGrants(resourceType: string | undefined, resourceId: string | undefined) {
  return useQuery({
    queryKey: ["admin-grants", resourceType, resourceId],
    queryFn: () =>
      apiFetch<Grant[]>(`/api/admin/grants?resource_type=${resourceType}&resource_id=${resourceId}`),
    enabled: !!resourceType && !!resourceId,
  });
}
export function createGrant(body: CreateGrantBody): Promise<{ id: string }> {
  return apiFetch("/api/admin/grants", { method: "POST", body: JSON.stringify(body) });
}
export function revokeGrant(id: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/admin/grants/${id}`, { method: "DELETE" });
}

// Groups
export interface GroupSummary { id: string; name: string }
export interface GroupDetail { id: string; name: string; members: string[] }
export function useGroups() {
  return useQuery({ queryKey: ["groups"], queryFn: () => apiFetch<GroupSummary[]>("/api/groups") });
}
export function useGroup(id: string | undefined) {
  return useQuery({
    queryKey: ["group", id],
    queryFn: () => apiFetch<GroupDetail>(`/api/groups/${id}`),
    enabled: !!id,
  });
}
export function createGroup(name: string, memberIds: string[] = []): Promise<{ id: string }> {
  return apiFetch("/api/groups", { method: "POST", body: JSON.stringify({ name, member_user_ids: memberIds }) });
}
export function deleteGroup(id: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/groups/${id}`, { method: "DELETE" });
}
export function addGroupMember(id: string, userId: string): Promise<{ ok: boolean; pending?: boolean; request_id?: string }> {
  return apiFetch(`/api/groups/${id}/members`, { method: "POST", body: JSON.stringify({ user_id: userId }) });
}
export function removeGroupMember(id: string, userId: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/groups/${id}/members/${userId}`, { method: "DELETE" });
}

// Per-group feature flags (Tier-2 #8) — restrict-only: a row with enabled=false
// disables a host feature for the group's members.
export interface GroupFeatureFlag { feature: string; enabled: boolean }
export function useGroupFlags(groupId: string | undefined) {
  return useQuery({
    queryKey: ["group-flags", groupId],
    queryFn: () => apiFetch<GroupFeatureFlag[]>(`/api/groups/${groupId}/feature-flags`),
    enabled: !!groupId,
  });
}
export function setGroupFlag(groupId: string, feature: string, enabled: boolean): Promise<{ ok: boolean }> {
  return apiFetch(`/api/groups/${groupId}/feature-flags/${feature}`, { method: "PUT", body: JSON.stringify({ enabled }) });
}
export function clearGroupFlag(groupId: string, feature: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/groups/${groupId}/feature-flags/${feature}`, { method: "DELETE" });
}

// Audit (hash-chain export)
export interface AuditEventOut {
  seq: number;
  id: string;
  actor_user_id: string | null;
  actor_name: string | null;
  actor_role: string;
  action_type: string;
  resource_type: string | null;
  resource_id: string | null;
  occurred_at: string;
  outcome: string;
  payload: unknown;
  prev_hash: string;
  hash: string;
}
export interface AuditExport {
  events: AuditEventOut[];
  verification: { ok: boolean; checked: number; first_bad_seq: number | null; reason: string | null };
  ed25519_public_key: string | null;
}
export function useAuditExport() {
  return useQuery({
    queryKey: ["admin-audit"],
    queryFn: () => apiFetch<AuditExport>("/api/admin/audit/export"),
    staleTime: 0,
  });
}
export interface AuditFilters {
  action?: string;
  actor_role?: string;
  resource_type?: string;
  limit?: number;
}
/** Live, server-filtered audit events (newest first). Admin only. */
export function useAuditEvents(f: AuditFilters) {
  return useQuery({
    queryKey: ["admin-audit-query", f.action ?? "", f.actor_role ?? "", f.resource_type ?? "", f.limit ?? 100],
    queryFn: () => {
      const qs = new URLSearchParams();
      if (f.action) qs.set("action", f.action);
      if (f.actor_role) qs.set("actor_role", f.actor_role);
      if (f.resource_type) qs.set("resource_type", f.resource_type);
      qs.set("limit", String(f.limit ?? 100));
      return apiFetch<AuditEventOut[]>(`/api/admin/audit?${qs}`);
    },
    staleTime: 0,
  });
}

/** Recent flagged (risk-anomaly) audit events for the admin alerts strip. */
export function useAnomalies() {
  return useQuery({
    queryKey: ["admin-anomalies"],
    queryFn: () => apiFetch<AuditEventOut[]>("/api/admin/anomalies"),
    refetchInterval: 30_000,
  });
}

/** Plain fetch for pagination (Load older). */
export function fetchAuditEvents(f: AuditFilters & { before_seq?: number }): Promise<AuditEventOut[]> {
  const qs = new URLSearchParams();
  if (f.action) qs.set("action", f.action);
  if (f.actor_role) qs.set("actor_role", f.actor_role);
  if (f.resource_type) qs.set("resource_type", f.resource_type);
  if (f.before_seq != null) qs.set("before_seq", String(f.before_seq));
  qs.set("limit", String(f.limit ?? 100));
  return apiFetch<AuditEventOut[]>(`/api/admin/audit?${qs}`);
}

// ── Identity (Enterprise federated SSO + SCIM) ──────────────────────────────────
export interface SsoIdp {
  alias: string;
  display_name?: string | null;
  provider_id: string;
  enabled: boolean;
  cert_expiry_days?: number | null;
}
export interface CreateSsoIdp {
  alias: string;
  display_name?: string;
  kind: "saml" | "oidc";
  metadata_url?: string;
  oidc_issuer?: string;
  oidc_client_id?: string;
  oidc_client_secret?: string;
  validate_logout_signature?: boolean;
}
export function useSsoIdps() {
  return useQuery({ queryKey: ["sso-idps"], queryFn: () => apiFetch<SsoIdp[]>("/api/admin/sso/idp") });
}
export function createSsoIdp(body: CreateSsoIdp): Promise<{ alias: string; ok: boolean }> {
  return apiFetch("/api/admin/sso/idp", { method: "POST", body: JSON.stringify(body) });
}
export function deleteSsoIdp(alias: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/admin/sso/idp/${encodeURIComponent(alias)}`, { method: "DELETE" });
}
export function ssoSpMetadata(): Promise<{ descriptor_url: string; descriptor_xml?: string | null }> {
  return apiFetch("/api/admin/sso/sp-metadata");
}

export interface ScimToken {
  id: string;
  label: string;
  prefix: string;
  expires_at?: string | null;
  last_used_at?: string | null;
  created_at: string;
  revoked_at?: string | null;
}
export function useScimTokens() {
  return useQuery({ queryKey: ["scim-tokens"], queryFn: () => apiFetch<ScimToken[]>("/api/admin/scim/tokens") });
}
export function createScimToken(body: { label: string; expires_in_days?: number }): Promise<{ id: string; label: string; prefix: string; token: string; expires_at?: string | null }> {
  return apiFetch("/api/admin/scim/tokens", { method: "POST", body: JSON.stringify(body) });
}
export function revokeScimToken(id: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/admin/scim/tokens/${id}`, { method: "DELETE" });
}

export interface IdentitySettings {
  role_source: string;
  role_mapping: Record<string, string>;
  jit_group_sync: string;
  delete_behaviour: string;
  scim_ip_allowlist: string;
  scim_base_url: string;
}
export function useIdentitySettings() {
  return useQuery({ queryKey: ["identity-settings"], queryFn: () => apiFetch<IdentitySettings>("/api/admin/identity/settings") });
}
export function saveIdentitySettings(body: Partial<IdentitySettings>): Promise<{ ok: boolean }> {
  return apiFetch("/api/admin/identity/settings", { method: "PUT", body: JSON.stringify(body) });
}

// --- Offline licence status + token administration (Enterprise) --------------
export interface LicenseStatus {
  state: "valid" | "grace" | "expired" | "unlicensed";
  licensee: string | null;
  tier: string | null;
  seats_used: number;
  seats_limit: number;
  expires_at: number | null; // Unix seconds
  grace_days: number | null;
  source: string; // env | file | db | none
  features: string[];
  strict: boolean;
}
export function useLicense() {
  return useQuery({ queryKey: ["license"], queryFn: () => apiFetch<LicenseStatus>("/api/admin/license") });
}
export function setLicenseToken(token: string): Promise<{ stored: boolean; preview_state: string; note: string }> {
  return apiFetch("/api/admin/license", { method: "PUT", body: JSON.stringify({ token }) });
}

// --- Custom RBAC: roles, assignments, ABAC policies (Enterprise) -------------
export interface PermissionDef { name: string; description: string; area: string; scope: string }
export interface CustomRole { id: string; name: string; description: string; permissions: string[]; system: boolean; assignment_count: number }
export interface RoleAssignment {
  id: string; role_id: string; role_name: string;
  principal_type: string; principal_id: string; principal_label?: string | null;
  scope_type?: string | null; scope_ids?: string[] | null;
}
export interface AbacPolicy { id: string; name: string; description: string; policy_text: string; enabled: boolean; last_validated_at?: string | null }

export function usePermissionCatalogue() {
  return useQuery({ queryKey: ["rbac-catalogue"], queryFn: () => apiFetch<PermissionDef[]>("/api/admin/rbac/catalogue") });
}
export function useCustomRoles() {
  return useQuery({ queryKey: ["rbac-roles"], queryFn: () => apiFetch<CustomRole[]>("/api/admin/rbac/roles") });
}
export function createRole(body: { name: string; description?: string; permissions: string[] }): Promise<{ id: string }> {
  return apiFetch("/api/admin/rbac/roles", { method: "POST", body: JSON.stringify(body) });
}
export function updateRole(id: string, body: { description?: string; permissions: string[] }): Promise<{ ok: boolean }> {
  return apiFetch(`/api/admin/rbac/roles/${id}`, { method: "PUT", body: JSON.stringify(body) });
}
export function deleteRole(id: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/admin/rbac/roles/${id}`, { method: "DELETE" });
}
export function useRoleAssignments() {
  return useQuery({ queryKey: ["rbac-assignments"], queryFn: () => apiFetch<RoleAssignment[]>("/api/admin/rbac/assignments") });
}
export function createAssignment(body: { role_id: string; principal_type: string; principal_id: string; scope_type?: string | null; scope_ids?: string[] | null }): Promise<{ id: string }> {
  return apiFetch("/api/admin/rbac/assignments", { method: "POST", body: JSON.stringify(body) });
}
export function deleteAssignment(id: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/admin/rbac/assignments/${id}`, { method: "DELETE" });
}
export function useAbacPolicies() {
  return useQuery({ queryKey: ["abac-policies"], queryFn: () => apiFetch<AbacPolicy[]>("/api/admin/abac/policies") });
}
export function createPolicy(body: { name: string; description?: string; policy_text: string }): Promise<{ id: string }> {
  return apiFetch("/api/admin/abac/policies", { method: "POST", body: JSON.stringify(body) });
}
export function updatePolicy(id: string, body: { description?: string; policy_text: string }): Promise<{ ok: boolean }> {
  return apiFetch(`/api/admin/abac/policies/${id}`, { method: "PUT", body: JSON.stringify(body) });
}
export function setPolicyEnabled(id: string, enabled: boolean): Promise<{ ok: boolean }> {
  return apiFetch(`/api/admin/abac/policies/${id}/enabled`, { method: "PUT", body: JSON.stringify({ enabled }) });
}
export function deletePolicy(id: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/admin/abac/policies/${id}`, { method: "DELETE" });
}
export interface SimResult { decision: string; reasons: string[]; errors: string[] }
export function simulatePolicy(body: { principal: string; permission: string; resource_type: string; resource_id: string; policy_text?: string }): Promise<SimResult> {
  return apiFetch("/api/admin/abac/simulate", { method: "POST", body: JSON.stringify(body) });
}
export interface AccessCheck { decision: string; layers: Record<string, boolean>; cedar: SimResult }
export function accessCheck(q: { user: string; permission: string; resource_type: string; resource_id: string }): Promise<AccessCheck> {
  const p = new URLSearchParams(q).toString();
  return apiFetch(`/api/admin/abac/access-check?${p}`);
}

export async function downloadAuditExport(): Promise<void> {
  const token = await freshToken();
  const res = await fetch("/api/admin/audit/export", { headers: { Authorization: `Bearer ${token}` } });
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
  const url = URL.createObjectURL(await res.blob());
  const a = document.createElement("a");
  a.href = url;
  a.download = "audit-evidence.json";
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}

// --- A2 evidence pack / checkpoints / erasure --------------------------------
export interface EvidenceFilters {
  from?: string;
  to?: string;
  action?: string;
  actor_user_id?: string;
  resource_id?: string;
  interaction_id?: string;
  include_pii?: boolean;
  format: "json" | "pdf";
}
/** Download a filtered, signed evidence pack (JSON bundle or PDF report). */
export async function downloadEvidencePack(f: EvidenceFilters): Promise<void> {
  const token = await freshToken();
  const qs = new URLSearchParams();
  (["from", "to", "action", "actor_user_id", "resource_id", "interaction_id"] as const).forEach((k) => {
    const val = f[k];
    if (val) qs.set(k, val);
  });
  if (f.include_pii) qs.set("include_pii", "true");
  qs.set("format", f.format);
  const res = await fetch(`/api/admin/audit/evidence?${qs}`, { headers: { Authorization: `Bearer ${token}` } });
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
  const url = URL.createObjectURL(await res.blob());
  const a = document.createElement("a");
  a.href = url;
  a.download = f.format === "pdf" ? "evidence-pack.pdf" : "evidence-pack.json";
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}

/** GDPR right-to-erasure: crypto-shred a subject's key. The chain still verifies. */
export function eraseSubject(subjectId: string): Promise<{ subject_id: string; erased: boolean }> {
  return apiFetch("/api/admin/audit/erase", { method: "POST", body: JSON.stringify({ subject_id: subjectId }) });
}

export interface AuditCheckpoint {
  id: string;
  seq_no: number;
  head_hash: string;
  created_at: string;
  signature: string | null;
  public_key: string | null;
  cosigned: boolean;
}
export function useLatestCheckpoint() {
  return useQuery({
    queryKey: ["admin-audit-checkpoint"],
    queryFn: () => apiFetch<AuditCheckpoint | null>("/api/admin/audit/checkpoints/latest"),
    staleTime: 0,
  });
}
export function mintCheckpoint(): Promise<AuditCheckpoint | null> {
  return apiFetch("/api/admin/audit/checkpoints", { method: "POST" });
}

// Integrations / connectors
export interface Connector {
  kind: string;
  display_name: string;
  category: string;
  requires_egress: boolean;
  enabled: boolean;
}
export function useAdminIntegrations() {
  return useQuery({ queryKey: ["admin-integrations"], queryFn: () => apiFetch<Connector[]>("/api/admin/integrations") });
}
export function setIntegrationEnabled(kind: string, enabled: boolean): Promise<unknown> {
  return apiFetch(`/api/admin/integrations/${kind}`, { method: "PUT", body: JSON.stringify({ enabled }) });
}

// MCP servers (FEATURE B1) — admin registry
export type McpAuthType = "none" | "bearer" | "api_key" | "header" | "oauth";
export interface McpToolBrief {
  name: string;
  description: string;
}
export interface McpServer {
  id: string;
  slug: string;
  name: string;
  transport: string; // stdio | http
  url: string | null;
  status: string; // pending | active | quarantined | unreachable
  enabled: boolean;
  connected: boolean;
  tool_count: number;
  tools: McpToolBrief[];
  last_health_at: string | null;
  created_at: string;
  auth_type: McpAuthType;
  auth_header_name: string | null;
  has_secret: boolean;
  requires_egress: boolean;
}
export function useAdminMcpServers() {
  return useQuery({ queryKey: ["admin-mcp-servers"], queryFn: () => apiFetch<McpServer[]>("/api/admin/mcp-servers") });
}
export function registerMcpServer(body: {
  slug: string;
  name: string;
  transport: "stdio" | "http";
  command?: string[];
  url?: string;
  auth_type?: McpAuthType;
  auth_header_name?: string;
  auth_value?: string;
  requires_egress?: boolean;
}): Promise<{ id: string; status: string }> {
  return apiFetch("/api/admin/mcp-servers", { method: "POST", body: JSON.stringify(body) });
}
export function approveMcpServer(id: string): Promise<{ status: string; tools: number }> {
  return apiFetch(`/api/admin/mcp-servers/${id}/approve`, { method: "POST" });
}
export function deleteMcpServer(id: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/admin/mcp-servers/${id}`, { method: "DELETE" });
}
export function patchMcpServer(
  id: string,
  body: {
    name?: string;
    url?: string;
    auth_type?: McpAuthType;
    auth_header_name?: string;
    auth_value?: string;
    requires_egress?: boolean;
  },
): Promise<{ ok: boolean; reapprove: boolean }> {
  return apiFetch(`/api/admin/mcp-servers/${id}`, { method: "PATCH", body: JSON.stringify(body) });
}

// ── MCP one-click connections (OAuth 2.1) ──────────────────────────────────────
export interface McpOauthDiscovery {
  issuer: string;
  dcr_available: boolean;
  scopes_supported: string[];
  s256_ok: boolean;
  callback_url: string;
  warnings: string[];
}
export function discoverMcpOauth(
  id: string,
  allowed_issuer_origin?: string,
): Promise<McpOauthDiscovery> {
  return apiFetch(`/api/admin/mcp-servers/${id}/oauth/discover`, {
    method: "POST",
    body: JSON.stringify({ allowed_issuer_origin }),
  });
}
export function putMcpOauthClient(
  id: string,
  body: {
    allowed_issuer_origin?: string;
    use_dcr?: boolean;
    client_id?: string;
    client_secret?: string;
    scopes?: string[];
  },
): Promise<{ issuer: string; registration_source: string; has_secret: boolean; scopes: string[] }> {
  return apiFetch(`/api/admin/mcp-servers/${id}/oauth/client`, {
    method: "PUT",
    body: JSON.stringify(body),
  });
}
export function deleteMcpOauthClient(id: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/admin/mcp-servers/${id}/oauth/client`, { method: "DELETE" });
}
export function setMcpCatalogSource(id: string, connection_id: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/admin/mcp-servers/${id}/oauth/catalog-source`, {
    method: "PUT",
    body: JSON.stringify({ connection_id }),
  });
}

export interface MyMcpConnection {
  server_id: string;
  slug: string;
  name: string;
  status: "connected" | "disconnected" | "reauth_required";
  subject_label: string | null;
  scopes: string[];
}
export function useMyMcpConnections(enabled: boolean) {
  return useQuery({
    queryKey: ["my-mcp-connections"],
    queryFn: () => apiFetch<MyMcpConnection[]>("/api/me/mcp-connections"),
    enabled,
  });
}
export function connectMcpServer(serverId: string, service?: boolean): Promise<{ authorize_url: string }> {
  return apiFetch(`/api/me/mcp-connections/${serverId}/connect`, {
    method: "POST",
    body: JSON.stringify({ service: !!service }),
  });
}
export function disconnectMcpServer(serverId: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/me/mcp-connections/${serverId}`, { method: "DELETE" });
}

// Legal holds
export interface Hold { id: string; resource_type: string; resource_id: string; reason: string | null }
export function useHolds() {
  return useQuery({ queryKey: ["admin-holds"], queryFn: () => apiFetch<Hold[]>("/api/admin/holds") });
}
export function setHold(body: { resource_type: string; resource_id: string; reason?: string }): Promise<{ id: string }> {
  return apiFetch("/api/admin/holds", { method: "POST", body: JSON.stringify(body) });
}
export function clearHold(id: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/admin/holds/${id}`, { method: "DELETE" });
}

// Runtime config
export interface ConfigEntry { key: string; value: string; value_type: string; scope: string }
export function useAdminConfig() {
  return useQuery({ queryKey: ["admin-config"], queryFn: () => apiFetch<ConfigEntry[]>("/api/admin/config") });
}
export function setConfig(key: string, body: { value: string; value_type?: string; scope?: string }): Promise<{ ok: boolean }> {
  return apiFetch(`/api/admin/config/${key}`, { method: "PUT", body: JSON.stringify(body) });
}

// Deployment-scope provider config (LLM/embed/rerank/ocr/stt/tts/verify). The
// API key is write-only: never returned, only `api_key_set` indicates one is stored.
export interface ProviderConfig {
  /** Row id — addressable for the multi-row `llm` CRUD; present for every row. */
  id: string;
  role: string;
  /** Display name (multi-LLM). null/ignored for the single-row roles. */
  label: string | null;
  base_url: string | null;
  model: string | null;
  enabled: boolean;
  api_key_set: boolean;
  /** Reasoning-control override (auto|none|toggle|levels|budget|always_on); null = auto. */
  reasoning_mode: string | null;
  /** The deployment-default llm row (multi-LLM). Always false for single roles. */
  is_default: boolean;
}
export function useProviders() {
  return useQuery({ queryKey: ["admin-providers"], queryFn: () => apiFetch<ProviderConfig[]>("/api/admin/providers") });
}
export function setProvider(
  role: string,
  body: { base_url?: string; model?: string; api_key?: string; enabled: boolean; reasoning_mode?: string | null },
): Promise<{ ok: boolean; reindex_required?: boolean; indexed_documents?: number }> {
  return apiFetch(`/api/admin/providers/${role}`, { method: "PUT", body: JSON.stringify(body) });
}

// Embedding-index provenance + blue-green re-index. Changing the
// embed model stages a `desired` target; an explicit re-index rebuilds the vectors.
export interface EmbeddingIndex {
  seeded: boolean;
  embed_model?: string;
  dim?: number;
  collection_name?: string;
  status?: string; // active | reindexing | failed
  reindex_done?: number;
  reindex_total?: number;
  error?: string | null;
  desired_model?: string | null;
  desired_dim?: number | null;
}
export function useEmbeddingIndex() {
  return useQuery({
    queryKey: ["embedding-index"],
    queryFn: () => apiFetch<EmbeddingIndex>("/api/admin/embedding-index"),
    // Poll from the moment a change is pending (`desired_model` staged) through the
    // whole migration, not just once `reindexing` is observed — the durable job
    // flips status a few seconds AFTER the trigger enqueues, so polling on a pending
    // change is what lets the UI catch the active → reindexing → active transitions.
    refetchInterval: (q) =>
      q.state.data?.status === "reindexing" || q.state.data?.desired_model ? 2000 : false,
  });
}
export function reindexEmbeddings(): Promise<{ ok: boolean; task_id?: string }> {
  return apiFetch("/api/admin/embedding-index/reindex", { method: "POST" });
}

// Live-voice engine config. STT/TTS engines + models selected
// at runtime; API keys are write-only (masked as `*_api_key_set`).
export interface VoiceLive {
  stt_stream_kind: string; // none | websocket | openai_realtime
  stt_stream_url: string;
  stt_model: string;
  dictation_model: string;
  stt_language: string;
  stt_sample_rate: number;
  tts_stream: boolean;
  tts_stream_url: string;
  tts_model: string;
  tts_voice: string;
  turn_detector_url: string;
  stt_api_key_set: boolean;
  tts_api_key_set: boolean;
}
export function useVoiceLive() {
  return useQuery({ queryKey: ["admin-voice-live"], queryFn: () => apiFetch<VoiceLive>("/api/admin/voice-live") });
}
export type VoiceLiveBody = Omit<VoiceLive, "stt_api_key_set" | "tts_api_key_set"> & { stt_api_key?: string; tts_api_key?: string };
export function setVoiceLive(body: VoiceLiveBody): Promise<{ ok: boolean }> {
  return apiFetch("/api/admin/voice-live", { method: "PUT", body: JSON.stringify(body) });
}

// Provider "Test connection". Probes the role with the given
// (possibly unsaved) config; the server resolves/decrypts the key and never echoes it.
export interface ProviderTestResult { ok: boolean; latency_ms: number; error?: string; detail?: string; model?: string }
type ProviderTestBody = { base_url?: string; model?: string; api_key?: string; enabled: boolean };
export function testProvider(role: string, body: ProviderTestBody): Promise<ProviderTestResult> {
  return apiFetch(`/api/admin/providers/${role}/test`, { method: "POST", body: JSON.stringify(body) });
}
export function testMyProvider(role: string, body: ProviderTestBody): Promise<ProviderTestResult> {
  return apiFetch(`/api/me/providers/${role}/test`, { method: "POST", body: JSON.stringify(body) });
}

// Per-user BYOK. `source` is which scope the resolver will use for
// the role: your own key, the deployment provider, or the ML built-in default.
export interface MyProvider {
  role: string;
  base_url: string | null;
  model: string | null;
  enabled: boolean;
  api_key_set: boolean;
  source: "user" | "deployment" | "default";
}
export interface MyProviders {
  user_byok_enabled: boolean;
  providers: MyProvider[];
}
export function useMyProviders() {
  return useQuery({ queryKey: ["my-providers"], queryFn: () => apiFetch<MyProviders>("/api/me/providers") });
}
export function setMyProvider(
  role: string,
  body: { base_url?: string; model?: string; api_key?: string; enabled: boolean },
): Promise<{ ok: boolean }> {
  return apiFetch(`/api/me/providers/${role}`, { method: "PUT", body: JSON.stringify(body) });
}
export function clearMyProvider(role: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/me/providers/${role}`, { method: "DELETE" });
}

// ── Multiple named LLM providers (mig 0091) ──────────────────────────────────
// The `llm` role holds several named rows per scope; a chat remembers which one it
// uses. Admin manages deployment rows; a user manages their own (BYOK). The
// composer picks per chat from `useMyLlmProviders`.

/** Create/update body for a named llm provider. `api_key` is write-only. */
export interface UpsertLlmBody {
  label: string;
  base_url?: string;
  model?: string;
  api_key?: string;
  enabled: boolean;
  reasoning_mode?: string | null;
}
export function createAdminLlm(body: UpsertLlmBody): Promise<{ ok: boolean; id: string; is_default: boolean }> {
  return apiFetch("/api/admin/providers/llm", { method: "POST", body: JSON.stringify(body) });
}
export function updateAdminLlm(id: string, body: UpsertLlmBody): Promise<{ ok: boolean }> {
  return apiFetch(`/api/admin/providers/llm/${id}`, { method: "PUT", body: JSON.stringify(body) });
}
export function deleteAdminLlm(id: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/admin/providers/llm/${id}`, { method: "DELETE" });
}
export function setAdminLlmDefault(id: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/admin/providers/llm/${id}/default`, { method: "PUT" });
}
export function testAdminLlm(body: { id?: string; base_url?: string; model?: string; api_key?: string; enabled: boolean }): Promise<ProviderTestResult> {
  return apiFetch("/api/admin/providers/llm/test", { method: "POST", body: JSON.stringify(body) });
}
export function createMyLlm(body: UpsertLlmBody): Promise<{ ok: boolean; id: string }> {
  return apiFetch("/api/me/providers/llm", { method: "POST", body: JSON.stringify(body) });
}
export function updateMyLlm(id: string, body: UpsertLlmBody): Promise<{ ok: boolean }> {
  return apiFetch(`/api/me/providers/llm/${id}`, { method: "PUT", body: JSON.stringify(body) });
}
export function deleteMyLlm(id: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/me/providers/llm/${id}`, { method: "DELETE" });
}
export function testMyLlm(body: { id?: string; base_url?: string; model?: string; api_key?: string; enabled: boolean }): Promise<ProviderTestResult> {
  return apiFetch("/api/me/providers/llm/test", { method: "POST", body: JSON.stringify(body) });
}

/** One selectable llm provider for the composer, with its reasoning capability so
 *  the Tune control re-derives per pick. `is_active` = this chat's provider (or the
 *  default when the chat has no pick). */
export interface LlmProviderOption {
  id: string;
  label: string | null;
  model: string | null;
  base_url: string | null;
  api_key_set: boolean;
  source: "user" | "deployment";
  enabled: boolean;
  is_default: boolean;
  is_active: boolean;
  reasoning: ReasoningCapability;
}
export interface LlmProviderList {
  providers: LlmProviderOption[];
  active_id: string | null;
}
export function useMyLlmProviders(chatId: string | null) {
  return useQuery({
    queryKey: ["my-llm-providers", chatId ?? "draft"],
    queryFn: () => apiFetch<LlmProviderList>(`/api/me/llm-providers${chatId ? `?chat_id=${chatId}` : ""}`),
  });
}
export function setChatLlmProvider(chatId: string, providerId: string | null): Promise<{ ok: boolean }> {
  return apiFetch(`/api/me/chats/${chatId}/llm-provider`, {
    method: "PUT",
    body: JSON.stringify({ provider_id: providerId }),
  });
}

// Self-serve account deletion (soft-archive). Server anonymises + deactivates the
// row, emits `account.archived`; the caller must then sign out.
export function deleteAccount(): Promise<{ ok: boolean }> {
  return apiFetch("/api/me/account", { method: "DELETE" });
}

// ── Enterprise connectors (Profile → Connections) ────────────────────────────
// A user's own OAuth connection to a DMS/mailbox source. Enterprise-only surface
// (gated by capabilities.enterprise_connectors); the endpoints 404 in a Core build.
export interface ConnectorConnection {
  id: string;
  kind: string;
  user_id: string | null;
  display_name: string;
  status: "active" | "reauth_required" | "revoked";
  scopes: string[];
  expires_at: number | null;
  last_used_at: number | null;
}
export function useConnectorConnections(enabled: boolean) {
  return useQuery({
    queryKey: ["connector-connections"],
    queryFn: () => apiFetch<{ connections: ConnectorConnection[] }>("/api/connectors/connections"),
    enabled,
  });
}
/** Begin an OAuth connect flow; returns the provider authorize URL to navigate to. */
export function connectConnector(kind: string): Promise<{ authorize_url: string }> {
  return apiFetch(`/api/connectors/${kind}/connect`, { method: "GET" });
}
export function disconnectConnector(id: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/connectors/connections/${id}`, { method: "DELETE" });
}

// Admin connector overview (app-configs per kind + connections). integrations.manage.
export interface ConnectorKindAdmin {
  kind: string;
  display_name: string;
  category: string;
  enabled: boolean;
  configured: boolean;
  config: Record<string, unknown>;
  has_secret: boolean;
}
export interface ConnectorAdminOverview {
  kinds: ConnectorKindAdmin[];
  connections: ConnectorConnection[];
  callback_url: string;
}
export function useAdminConnectors() {
  return useQuery({ queryKey: ["admin-connectors"], queryFn: () => apiFetch<ConnectorAdminOverview>("/api/admin/connectors") });
}
export function saveConnectorConfig(kind: string, body: { config: Record<string, unknown>; client_secret?: string }): Promise<{ ok: boolean }> {
  return apiFetch(`/api/admin/connectors/${kind}/config`, { method: "PUT", body: JSON.stringify(body) });
}

// Browse + import + mappings (Enterprise, capability-gated).
export interface ConnectorFolder { id: string; name: string }
export interface ConnectorMessage { id: string; subject: string | null; from: string | null; received_at: string | null; version: string | null }
export function connectorFolders(kind: string, connectionId: string): Promise<{ folders: ConnectorFolder[] }> {
  return apiFetch(`/api/connectors/${kind}/folders?connection_id=${connectionId}`);
}
export function connectorMessages(kind: string, connectionId: string, folder: string, cursor?: string): Promise<{ messages: ConnectorMessage[]; next: string | null }> {
  const q = new URLSearchParams({ connection_id: connectionId, folder, ...(cursor ? { cursor } : {}) });
  return apiFetch(`/api/connectors/${kind}/messages?${q.toString()}`);
}
export type ImportDestination = "workspace" | "kb" | "both";
export function importConnectorItems(body: { connection_id: string; kind: string; project_id: string; remote_ids: string[]; destination?: ImportDestination; target_kb_id?: string | null }): Promise<{ created: number; updated: number; skipped: number; kb_id: string | null }> {
  return apiFetch("/api/connectors/import", { method: "POST", body: JSON.stringify(body) });
}
export interface ConnectorMapping {
  id: string;
  connection_id: string;
  kind: string;
  remote_container_id: string;
  remote_container_name: string | null;
  sync_enabled: boolean;
  last_sync_at: number | null;
  last_error: string | null;
  acl_mode: "off" | "warn" | "enforce";
  destination: ImportDestination;
  target_kb_id: string | null;
}
export function useConnectorMappings(projectId: string, enabled: boolean) {
  return useQuery({
    queryKey: ["connector-mappings", projectId],
    queryFn: () => apiFetch<{ mappings: ConnectorMapping[] }>(`/api/connectors/mappings?project_id=${projectId}`),
    enabled,
  });
}
export function createConnectorMapping(body: { connection_id: string; kind: string; project_id: string; container_id: string; container_name?: string; destination?: ImportDestination; target_kb_id?: string | null }): Promise<{ id: string }> {
  return apiFetch("/api/connectors/mappings", { method: "POST", body: JSON.stringify(body) });
}
export function deleteConnectorMapping(id: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/connectors/mappings/${id}`, { method: "DELETE" });
}
export function syncConnectorMapping(id: string): Promise<{ imported: number }> {
  return apiFetch(`/api/connectors/mappings/${id}/sync`, { method: "POST" });
}

// ── Source-ACL inheritance (enterprise_connectors + integrations.manage) ──
export interface AclPrincipal {
  id: string;
  kind: string;
  principal_key: string;
  principal_display: string | null;
  status: "unmatched" | "auto" | "manual" | "ignored";
  matched_via: string | null;
  mapped_principal_type: "user" | "group" | null;
  mapped_principal_id: string | null;
  document_count: number;
}
export function useAclPrincipals(status: "unmatched" | "all", enabled: boolean) {
  return useQuery({
    queryKey: ["acl-principals", status],
    queryFn: () => apiFetch<{ principals: AclPrincipal[] }>(`/api/admin/connectors/acl/principals?status=${status}`),
    enabled,
  });
}
export function mapAclPrincipal(id: string, body: { principal_type: "user" | "group"; principal_id: string }): Promise<{ ok: boolean; documents_recomputed: number }> {
  return apiFetch(`/api/admin/connectors/acl/principals/${id}/map`, { method: "POST", body: JSON.stringify(body) });
}
export function ignoreAclPrincipal(id: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/admin/connectors/acl/principals/${id}/ignore`, { method: "POST" });
}
export function unmapAclPrincipal(id: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/admin/connectors/acl/principals/${id}/unmap`, { method: "POST" });
}
export function rematchAclPrincipals(): Promise<{ considered: number; matched: number }> {
  return apiFetch(`/api/admin/connectors/acl/rematch`, { method: "POST" });
}
export interface AclImpact {
  documents_restricted: number;
  directly_entitled_users: number;
  documents_with_unmapped_principals: number;
  note: string;
}
export function aclImpact(mappingId: string): Promise<AclImpact> {
  return apiFetch(`/api/connectors/mappings/${mappingId}/acl-impact`);
}
export function setAclMode(mappingId: string, mode: "off" | "warn" | "enforce"): Promise<{ ok: boolean; mode: string }> {
  return apiFetch(`/api/connectors/mappings/${mappingId}/acl-mode`, { method: "PUT", body: JSON.stringify({ mode }) });
}
export interface ConnectorItem {
  document_id: string;
  kind: string;
  remote_id: string;
  remote_version: string | null;
  remote_deleted: boolean;
  unsupported_format: boolean;
  acl_status: "ok" | "unmapped_principals" | "no_snapshot";
  acl_mode: "off" | "warn" | "enforce";
}
export function useConnectorItems(projectId: string, enabled: boolean) {
  return useQuery({
    queryKey: ["connector-items", projectId],
    queryFn: () => apiFetch<{ items: ConnectorItem[] }>(`/api/connectors/items?project_id=${projectId}`),
    enabled,
  });
}
/** File a DMS-imported document back to the source as a new version (HITL). */
export function writeBackConnectorDoc(documentId: string): Promise<{ new_version: string }> {
  return apiFetch("/api/connectors/writeback", { method: "POST", body: JSON.stringify({ document_id: documentId }) });
}

// ── Admin notices: announcement banners + login welcome message ──────────────
export type Severity = "info" | "success" | "warning" | "error";
export interface Announcement {
  id: string;
  content: string;
  severity: Severity;
  dismissible: boolean;
  active: boolean;
  sort_order: number;
}
export interface WelcomeMessage { enabled: boolean; title: string; body: string }
export interface Notices { banners: Announcement[]; welcome: WelcomeMessage | null }

/** Banners + welcome for the current user; fetched once on Shell mount and
 *  refreshed by the server's `["notices"]` invalidate broadcast on any change. */
export function useNotices() {
  return useQuery({ queryKey: ["notices"], queryFn: () => apiFetch<Notices>("/api/notices") });
}
export function useAdminAnnouncements() {
  return useQuery({ queryKey: ["admin-announcements"], queryFn: () => apiFetch<Announcement[]>("/api/admin/announcements") });
}
export function createAnnouncement(body: {
  content: string; severity?: Severity; dismissible?: boolean; active?: boolean; sort_order?: number;
}): Promise<Announcement> {
  return apiFetch("/api/admin/announcements", { method: "POST", body: JSON.stringify(body) });
}
export function updateAnnouncement(id: string, body: {
  content?: string; severity?: Severity; dismissible?: boolean; active?: boolean; sort_order?: number;
}): Promise<Announcement> {
  return apiFetch(`/api/admin/announcements/${id}`, { method: "PUT", body: JSON.stringify(body) });
}
export function deleteAnnouncement(id: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/admin/announcements/${id}`, { method: "DELETE" });
}
export function useAdminWelcome() {
  return useQuery({ queryKey: ["admin-welcome"], queryFn: () => apiFetch<WelcomeMessage>("/api/admin/welcome") });
}
export function setWelcome(body: WelcomeMessage): Promise<{ ok: boolean }> {
  return apiFetch("/api/admin/welcome", { method: "PUT", body: JSON.stringify(body) });
}

// Branding assets (public; logo/favicon)
export interface BrandingAsset { kind: string; mime: string }
export function useBranding() {
  return useQuery({
    queryKey: ["branding"],
    queryFn: () => apiFetch<BrandingAsset[]>("/api/config/branding"),
    staleTime: 5 * 60_000,
    retry: 0,
  });
}

// Branding theme (public; colours/fonts applied as :root CSS variables at boot).
export interface BrandingTheme {
  primary: string | null;
  accent: string | null;
  bg: string | null;
  fg: string | null;
  font_sans: string | null;
  font_serif: string | null;
}
// The customisable theme keys → the CSS custom property each overrides.
export const THEME_VARS: { key: keyof BrandingTheme; cssVar: string; label: string; kind: "colour" | "font" }[] = [
  { key: "primary", cssVar: "--color-gold", label: "Primary / accent colour", kind: "colour" },
  { key: "accent", cssVar: "--color-gold-light", label: "Secondary accent colour", kind: "colour" },
  { key: "bg", cssVar: "--color-navy-deep", label: "Background colour", kind: "colour" },
  { key: "fg", cssVar: "--color-off-white", label: "Foreground / text colour", kind: "colour" },
  { key: "font_sans", cssVar: "--font-sans", label: "Sans-serif font family", kind: "font" },
  { key: "font_serif", cssVar: "--font-serif", label: "Serif font family", kind: "font" },
];
export function useTheme() {
  return useQuery({
    queryKey: ["branding-theme"],
    queryFn: () => apiFetch<BrandingTheme>("/api/branding/theme"),
    staleTime: 5 * 60_000,
    retry: 0,
  });
}
/** Upload a branding asset (logo|favicon) — raw bytes + ?mime (admin). */
export async function uploadBranding(kind: string, file: File): Promise<void> {
  const token = await freshToken();
  const mime = file.type || "image/png";
  const res = await fetch(`/api/admin/branding/${kind}?mime=${encodeURIComponent(mime)}`, {
    method: "POST",
    headers: { Authorization: `Bearer ${token}`, "Content-Type": mime },
    body: file,
  });
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
}

// Project-Knowledge directory (for binding to an agent). Power-user+.
export interface ProjectKnowledgeEntry {
  id: string;
  project_id: string;
  project_name: string;
  status: string;
}
export function useProjectKnowledge() {
  return useQuery({ queryKey: ["project-knowledge"], queryFn: () => apiFetch<ProjectKnowledgeEntry[]>("/api/project-knowledge"), retry: 0 });
}

// System readiness
export interface Readiness { status: string; checks: { postgres: boolean; redis: boolean } }
export function useReadiness() {
  return useQuery({
    queryKey: ["readiness"],
    queryFn: () => apiFetch<Readiness>("/health/ready"),
    refetchInterval: 15_000,
  });
}

// ── Voice (dictation + read-aloud) ───────────────────────────────────────────
// Gated server-side on features.voice (400 if off, 503 if the engine is down).
// Binary bodies → raw fetch + Bearer (not apiFetch).

/** STT: raw audio bytes → transcript. */
export async function transcribeAudio(blob: Blob): Promise<{ text: string }> {
  const token = await freshToken();
  const mime = blob.type || "application/octet-stream";
  const res = await fetch(`/api/voice/transcribe?mime=${encodeURIComponent(mime)}`, {
    method: "POST",
    headers: { Authorization: `Bearer ${token}`, "Content-Type": mime },
    body: blob,
  });
  if (!res.ok) {
    const body = await res.text().catch(() => "");
    throw new Error(`${res.status} ${res.statusText}: ${body.slice(0, 200)}`);
  }
  return res.json();
}

/** TTS: text → audio blob (mime from the engine). */
export async function speakText(text: string, voice?: string): Promise<Blob> {
  const token = await freshToken();
  const res = await fetch("/api/voice/speech", {
    method: "POST",
    headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
    body: JSON.stringify({ text, voice }),
  });
  if (!res.ok) {
    const body = await res.text().catch(() => "");
    throw new Error(`${res.status} ${res.statusText}: ${body.slice(0, 200)}`);
  }
  return res.blob();
}

// ── Automations (scheduled prompts) ──────────────────────────────────────────

export type AutomationStatus = "active" | "paused";
export type RunStatus = "running" | "succeeded" | "failed";

export interface Automation {
  id: string;
  name: string;
  schedule: string; // cron (6-field, seconds-leading)
  prompt: string;
  agent_id: string | null;
  status: AutomationStatus;
  next_run_at: string | null;
  last_run_at: string | null;
  project_id: string | null;
  kb_ids: string[];
  deliver_group_chat_id: string | null;
}
export interface AutomationRun {
  id: string;
  status: RunStatus;
  output_chat_id: string | null;
  error: string | null;
  started_at: string | null;
  completed_at: string | null;
}
export interface CalendarEntry {
  automation_id: string;
  name: string;
  at: string;
}
export interface CreateAutomationBody {
  name: string;
  schedule: string;
  prompt: string;
  agent_id?: string | null;
  project_id?: string | null;
  kb_ids?: string[];
  deliver_group_chat_id?: string | null;
}
export interface UpdateAutomationBody {
  name?: string;
  schedule?: string;
  prompt?: string;
  status?: AutomationStatus;
  project_id?: string | null;
  kb_ids?: string[];
  deliver_group_chat_id?: string | null;
}

export function useAutomations() {
  return useQuery({ queryKey: ["automations"], queryFn: () => apiFetch<Automation[]>("/api/automations") });
}
export function useAutomation(id: string | undefined) {
  return useQuery({
    queryKey: ["automation", id],
    queryFn: () => apiFetch<Automation>(`/api/automations/${id}`),
    enabled: !!id,
  });
}
export function createAutomation(body: CreateAutomationBody): Promise<{ id: string }> {
  return apiFetch("/api/automations", { method: "POST", body: JSON.stringify(body) });
}
export function updateAutomation(id: string, body: UpdateAutomationBody): Promise<{ ok: boolean }> {
  return apiFetch(`/api/automations/${id}`, { method: "PATCH", body: JSON.stringify(body) });
}
export function deleteAutomation(id: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/automations/${id}`, { method: "DELETE" });
}
export function runAutomation(id: string): Promise<unknown> {
  return apiFetch(`/api/automations/${id}/run`, { method: "POST" });
}
export function useAutomationRuns(id: string | undefined) {
  return useQuery({
    queryKey: ["automation-runs", id],
    queryFn: () => apiFetch<AutomationRun[]>(`/api/automations/${id}/runs`),
    enabled: !!id,
    // Poll while the editor is open so a just-enqueued run appears (the scheduler
    // creates the `running` row a few seconds after "Run now") and flips to its
    // final status live; faster cadence while a run is actually in flight.
    refetchInterval: (q) =>
      (q.state.data as AutomationRun[] | undefined)?.some((r) => r.status === "running") ? 3000 : 5000,
  });
}

// ── Workflows (event-driven; power-user only) ────────────────────────────────

export type WorkflowActionType = "agent_run" | "system_action";
export type WorkflowRunStatus = "queued" | "running" | "succeeded" | "failed" | "skipped";

export interface Workflow {
  id: string;
  name: string;
  description: string | null;
  owner_id: string;
  owner_name: string | null;
  project_id: string | null;
  enabled: boolean;
  trigger_event_type: string;
  trigger_scope: Record<string, unknown>;
  trigger_on_system_events: boolean;
  condition: Record<string, unknown> | null;
  coalesce_window_secs: number;
  action_type: WorkflowActionType;
  agent_id: string | null;
  action_config: Record<string, unknown>;
  max_runs_per_window: number;
  version: number;
}
export interface WorkflowRun {
  id: string;
  status: WorkflowRunStatus;
  depth: number;
  event_count: number;
  outcome: Record<string, unknown> | null;
  error: string | null;
  started_at: string | null;
  finished_at: string | null;
  created_at: string | null;
}
export interface CreateWorkflowBody {
  name: string;
  description?: string | null;
  project_id?: string | null;
  trigger_event_type: string;
  trigger_scope?: Record<string, unknown>;
  trigger_on_system_events?: boolean;
  condition?: Record<string, unknown> | null;
  coalesce_window_secs?: number;
  action_type: WorkflowActionType;
  agent_id?: string | null;
  action_config?: Record<string, unknown>;
  max_runs_per_window?: number;
}
export interface UpdateWorkflowBody {
  name?: string;
  description?: string | null;
  enabled?: boolean;
  trigger_on_system_events?: boolean;
  condition?: Record<string, unknown> | null;
  action_config?: Record<string, unknown>;
  coalesce_window_secs?: number;
  max_runs_per_window?: number;
}

export interface WorkflowTrigger {
  name: string;
  description: string;
  emitted: boolean;
}
export function useWorkflows() {
  return useQuery({ queryKey: ["workflows"], queryFn: () => apiFetch<Workflow[]>("/api/workflows") });
}
/// The trigger catalogue — the single source for the create-form dropdown.
export function useWorkflowTriggers() {
  return useQuery({ queryKey: ["workflow-triggers"], queryFn: () => apiFetch<WorkflowTrigger[]>("/api/workflows/triggers") });
}
export function useWorkflow(id: string | undefined) {
  return useQuery({
    queryKey: ["workflow", id],
    queryFn: () => apiFetch<Workflow>(`/api/workflows/${id}`),
    enabled: !!id,
  });
}
export function createWorkflow(body: CreateWorkflowBody): Promise<{ id: string }> {
  return apiFetch("/api/workflows", { method: "POST", body: JSON.stringify(body) });
}
export function updateWorkflow(id: string, body: UpdateWorkflowBody): Promise<{ ok: boolean }> {
  return apiFetch(`/api/workflows/${id}`, { method: "PATCH", body: JSON.stringify(body) });
}
export function deleteWorkflow(id: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/workflows/${id}`, { method: "DELETE" });
}
export function useWorkflowRuns(id: string | undefined) {
  return useQuery({
    queryKey: ["workflow-runs", id],
    queryFn: () => apiFetch<WorkflowRun[]>(`/api/workflows/${id}/runs`),
    enabled: !!id,
    // Poll while a run is in flight so status flips live; calm cadence otherwise.
    refetchInterval: (q) =>
      (q.state.data as WorkflowRun[] | undefined)?.some((r) => r.status === "running" || r.status === "queued")
        ? 3000
        : 5000,
  });
}

// ── Prompts (reusable templates) ─────────────────────────────────────────────

export type PromptScope = "personal" | "project" | "global";
export interface PromptSummary {
  id: string;
  name: string;
  scope: PromptScope;
  project_id: string | null;
  agent_id: string | null;
}
export type PromptFieldType = "short" | "long" | "date" | "select";
/** Friendly metadata for a template `{{key}}` slot — built visually so authors
 *  never see the braces; drives the typed fill inputs + labels. */
export interface PromptVariable {
  key: string;
  label: string;
  type: PromptFieldType;
  help?: string;
  options?: string[];
}
export interface PromptDetail {
  id: string;
  name: string;
  content: string;
  placeholders: string[];
  agent_id: string | null;
  variables: PromptVariable[];
}
export interface CreatePromptBody {
  name: string;
  content: string;
  scope?: PromptScope;
  project_id?: string | null;
  agent_id?: string | null;
  variables?: PromptVariable[];
}

export function usePrompts() {
  return useQuery({ queryKey: ["prompts"], queryFn: () => apiFetch<PromptSummary[]>("/api/prompts") });
}
export function usePrompt(id: string | undefined) {
  return useQuery({
    queryKey: ["prompt", id],
    queryFn: () => apiFetch<PromptDetail>(`/api/prompts/${id}`),
    enabled: !!id,
  });
}
export function createPrompt(body: CreatePromptBody): Promise<{ id: string }> {
  return apiFetch("/api/prompts", { method: "POST", body: JSON.stringify(body) });
}
/** Imperative prompt fetch (for the composer "/" picker deciding fill vs insert). */
export function getPrompt(id: string): Promise<PromptDetail> {
  return apiFetch(`/api/prompts/${id}`);
}
export function renderPrompt(id: string, values: Record<string, string>): Promise<{ content: string }> {
  return apiFetch(`/api/prompts/${id}/render`, { method: "POST", body: JSON.stringify({ values }) });
}

// ── Memory (explicit facts) ──────────────────────────────────────────────────

export interface MemoryFact {
  id: string;
  scope: "user" | "project";
  content: string;
  pinned: boolean;
  user_edited: boolean;
}
export interface CreateFactBody {
  content: string;
  scope: "user" | "project";
  project_id?: string | null;
}

export function useMemoryFacts(projectId?: string | null) {
  const suffix = projectId ? `?project_id=${projectId}` : "";
  return useQuery({
    queryKey: ["memory", projectId ?? null],
    queryFn: () => apiFetch<MemoryFact[]>(`/api/memory${suffix}`),
  });
}
export function createFact(body: CreateFactBody): Promise<{ id: string }> {
  return apiFetch("/api/memory", { method: "POST", body: JSON.stringify(body) });
}
export function updateFact(id: string, body: { content?: string; pinned?: boolean }): Promise<{ ok: boolean }> {
  return apiFetch(`/api/memory/${id}`, { method: "PATCH", body: JSON.stringify(body) });
}
export function deleteFact(id: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/memory/${id}`, { method: "DELETE" });
}

// ── Generated artefacts (chat-scoped) ────────────────────────────────────────

export interface Artefact {
  id: string;
  kind: "docx" | "pdf" | "md" | "file" | "html" | "xlsx" | "pptx";
  title: string;
  mime: string;
  /** Assistant message that produced it (null until the turn persists). */
  message_id: string | null;
  /** Source chat mode ("general"|"legal"|"research") — present on the list; lets the
   *  UI offer "Create page" only on Deep Research reports. */
  chat_mode?: string;
}

export function useChatArtefacts(chatId: string | undefined) {
  return useQuery({
    queryKey: ["artefacts", chatId],
    queryFn: () => apiFetch<Artefact[]>(`/api/chats/${chatId}/artefacts`),
    enabled: !!chatId,
  });
}

// ── Team / group chats ───────────────────────────────────────────────────────

export type GroupChatKind = "dm" | "group" | "project";
export interface GroupChatSummary {
  id: string;
  kind: GroupChatKind;
  name: string | null;
  project_id: string | null;
  unread_count: number;
}
export interface GroupMember {
  user_id: string;
  role: string;
}
export interface GroupChatDetail extends GroupChatSummary {
  members: GroupMember[];
}
export interface GroupMessage {
  id: string;
  seq: number;
  sender_user_id: string | null;
  message_type: "user" | "system";
  content: string;
  created_at: string;
  mentions?: unknown;
  /** A shared chat reference: `{ chat_id }` → the UI offers an "open chat" link. */
  shared_resources?: { chat_id?: string } | null;
  /** Uploaded image/file attachments (rendered inline / as download chips). */
  attachments?: MessageAttachment[] | null;
  /** Emoji reactions aggregated for this message. */
  reactions?: ReactionAgg[];
}
export interface MessageAttachment {
  id: string;
  filename: string;
  mime: string;
}
export interface ReactionAgg {
  emoji: string;
  count: number;
  mine: boolean;
}

/** Toggle the caller's emoji reaction on a message. Returns whether it was added. */
export function toggleReaction(chatId: string, messageId: string, emoji: string): Promise<{ added: boolean }> {
  return apiFetch(`/api/group-chats/${chatId}/messages/${messageId}/reactions`, {
    method: "POST",
    body: JSON.stringify({ emoji }),
  });
}

/** Share an LLM chat into a group/DM chat (posts a link the members can open). */
export function shareChat(chatId: string, groupChatId: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/chats/${chatId}/share`, { method: "POST", body: JSON.stringify({ group_chat_id: groupChatId }) });
}

// Shared-chats governance: see and revoke the chats you've shared.
export interface ChatShare {
  chat_id: string;
  chat_title: string;
  group_chat_id: string;
  group_chat_name: string;
  group_chat_kind: string;
  shared_at: string;
}
export function useMyShares() {
  return useQuery({ queryKey: ["my-shares"], queryFn: () => apiFetch<ChatShare[]>("/api/chat-shares") });
}
export function revokeShare(chatId: string, groupChatId: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/chat-shares/${chatId}/${groupChatId}`, { method: "DELETE" });
}

/** Start (or reuse) a 1:1 DM with another user. Idempotent. */
export function startDm(userId: string): Promise<{ id: string }> {
  return apiFetch(`/api/dms/${userId}`, { method: "POST" });
}

/** Upload an image/file for a group/DM message; returns the stored attachment ref. */
export async function uploadMessageAttachment(file: File): Promise<MessageAttachment> {
  const token = await freshToken();
  const qs = new URLSearchParams({ filename: file.name, mime: file.type || "application/octet-stream" });
  const res = await fetch(`/api/message-attachments?${qs}`, {
    method: "POST",
    headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/octet-stream" },
    body: file,
  });
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
  return res.json();
}

/** Fetch a message attachment's bytes (Bearer) → object URL for inline render. */
export async function messageAttachmentUrl(id: string): Promise<string> {
  const token = await freshToken();
  const res = await fetch(`/api/message-attachments/${id}`, { headers: { Authorization: `Bearer ${token}` } });
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
  return URL.createObjectURL(await res.blob());
}

export function useGroupChats() {
  return useQuery({ queryKey: ["group-chats"], queryFn: () => apiFetch<GroupChatSummary[]>("/api/group-chats") });
}
export function useGroupChat(id: string | undefined) {
  return useQuery({
    queryKey: ["group-chat", id],
    queryFn: () => apiFetch<GroupChatDetail>(`/api/group-chats/${id}`),
    enabled: !!id,
  });
}
export function createGroupChat(body: { kind?: GroupChatKind; name?: string; project_id?: string; member_user_ids?: string[] }): Promise<{ id: string }> {
  // Teams only creates standalone group chats; project chats are auto-created per
  // Project, DMs are not created here. Default to "group".
  return apiFetch("/api/group-chats", { method: "POST", body: JSON.stringify({ kind: "group", ...body }) });
}
export function fetchGroupMessages(id: string, since = 0): Promise<GroupMessage[]> {
  return apiFetch<GroupMessage[]>(`/api/group-chats/${id}/messages?since=${since}`);
}
export function sendGroupMessageRest(
  id: string,
  content: string,
  opts?: { attachments?: MessageAttachment[]; mentions?: string[] },
): Promise<{ id: string; seq: number; created_at: string }> {
  const body: Record<string, unknown> = { content };
  if (opts?.attachments?.length) body.attachments = opts.attachments;
  if (opts?.mentions?.length) body.mentions = opts.mentions;
  return apiFetch(`/api/group-chats/${id}/messages`, { method: "POST", body: JSON.stringify(body) });
}
export function addGroupChatMember(id: string, userId: string, role?: string): Promise<unknown> {
  return apiFetch(`/api/group-chats/${id}/members`, { method: "POST", body: JSON.stringify({ user_id: userId, role }) });
}
export function removeGroupChatMember(id: string, userId: string): Promise<unknown> {
  return apiFetch(`/api/group-chats/${id}/members/${userId}`, { method: "DELETE" });
}

// Shared notes (member-gated; content-only; `version` = optimistic token)
export interface GroupNote {
  id: string;
  content: string;
  version: number;
}
export function useGroupNotes(chatId: string | undefined) {
  return useQuery({
    queryKey: ["group-notes", chatId],
    queryFn: () => apiFetch<GroupNote[]>(`/api/group-chats/${chatId}/notes`),
    enabled: !!chatId,
  });
}
export function createNote(chatId: string, content: string): Promise<GroupNote> {
  return apiFetch(`/api/group-chats/${chatId}/notes`, { method: "POST", body: JSON.stringify({ content }) });
}
export function updateNote(chatId: string, noteId: string, content: string, version: number): Promise<GroupNote> {
  return apiFetch(`/api/group-chats/${chatId}/notes/${noteId}`, { method: "PUT", body: JSON.stringify({ content, version }) });
}
export function deleteNote(chatId: string, noteId: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/group-chats/${chatId}/notes/${noteId}`, { method: "DELETE" });
}

/** Download an artefact's bytes via an anchor (needs Bearer → blob). */
export async function downloadArtefact(id: string, title: string, kind: string): Promise<void> {
  const token = await freshToken();
  const res = await fetch(`/api/artefacts/${id}/download`, { headers: { Authorization: `Bearer ${token}` } });
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
  const url = URL.createObjectURL(await res.blob());
  const a = document.createElement("a");
  a.href = url;
  a.download = kind === "file" ? title : `${title}.${kind}`;
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}

export function useCalendar(from?: string, to?: string) {
  const qs = new URLSearchParams();
  if (from) qs.set("from", from);
  if (to) qs.set("to", to);
  const suffix = qs.toString() ? `?${qs}` : "";
  return useQuery({
    queryKey: ["automation-calendar", from, to],
    queryFn: () => apiFetch<CalendarEntry[]>(`/api/automations/calendar${suffix}`),
  });
}

export interface ChatSummary {
  id: string;
  title: string;
  project_id: string | null;
  agent_id: string | null;
  created_at: string;
  /** Workspace mode: "general" | "legal" | "research". Research runs are
   * excluded from the default list and fetched via useResearchChats(). */
  mode: string;
  /** Saved Deep Research request params (research mode only) for 'Refine'. */
  research_params?: ResearchRefineParams | null;
  /** Which client started this conversation: "web" | "desktop". Programmatic
   * conversations are never listed, so "api" does not appear here. */
  origin?: string;
}

/** The persisted params of a Deep Research run, replayed by 'Refine'. */
export interface ResearchRefineParams {
  question: string;
  source: ResearchRequestBody["source"];
  template: ResearchRequestBody["template"];
  kb_ids: string[];
  /** Library names captured at run time (corpus modes), for display in the chat
   * and report. Absent on web runs and on pre-existing rows. */
  kb_names?: string[];
  refinements: string[];
}

/** An assistant turn's agent activity (track_steps plan + tools used), for the
 * inline activity timeline. null for plain turns. */
export interface MsgActivity {
  steps?: { title: string; status: string }[];
  tools?: string[];
  /** Deep Research roadmap, persisted on the finished report message so the
   * "Research steps" block survives a reload. */
  research_roadmap?: ResearchRoadmap;
  /** The retrieval Coverage summary, rendered as a completed
   * activity step (survives a reload, unlike the transient progress label). */
  coverage?: string | null;
}

/** The persisted roadmap of a Deep Research run: the ordered section headings and
 * the phase timeline (seconds-from-start per progress event). */
export interface ResearchRoadmap {
  sections: string[];
  sections_total: number;
  phases: { phase: string; detail?: string | null; at: number }[];
}
/** A live groundedness verdict on a RAG answer (Mode A): the grounded fraction,
 * claim counts, and the unsupported spans. null when not verified. */
export interface MsgGroundedness {
  score: number | null;
  total: number;
  flagged: number;
  contradicted?: number;
  not_mentioned?: number;
  model?: string;
  spans: { start: number; end: number; text: string; label: string }[];
}
/** A durable chat attachment's metadata (bytes served via /api/chat-attachments/{id}). */
export interface ChatAttachmentMeta {
  id: string;
  filename: string;
  mime: string;
  byte_size: number;
}

export interface MessageOut {
  id: string;
  role: "user" | "assistant";
  content: string;
  sequence_number: number;
  created_at: string;
  activity?: MsgActivity | null;
  groundedness?: MsgGroundedness | null;
  /** True while an assistant turn is still being written — the SPA polls until it settles. */
  streaming?: boolean;
  /** Human sign-off on this assistant turn (approved | changes_requested | rejected), if any. */
  review_decision?: string | null;
  /** Files the user attached to this message (user turns only). */
  attachments?: ChatAttachmentMeta[];
  /** Document (RAG) + web citations for this assistant turn — the "Sources" list.
   * Returned on load so sources survive a reload. */
  citations?: Citation[] | null;
}

/** URL to fetch an attachment's bytes — usable directly as an <img src> (cookie auth). */
export function chatAttachmentUrl(id: string): string {
  return `/api/chat-attachments/${id}`;
}

/** Download a chat attachment via an authorised fetch (bearer) → blob → save. */
export async function downloadChatAttachment(id: string, filename: string): Promise<void> {
  const token = await freshToken();
  const res = await fetch(`/api/chat-attachments/${id}`, { headers: { Authorization: `Bearer ${token}` } });
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
  const url = URL.createObjectURL(await res.blob());
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}

// ── Agent action audit view: the consolidated per-turn review bundle + sign-off ──
export interface ReviewState {
  decision: string;
  note?: string | null;
  reviewer_user_id?: string | null;
  reviewer_name?: string | null;
  reviewed_at: string;
}
export interface ReviewBundle {
  message_id: string;
  chat_id: string;
  turn_id?: string | null;
  role: string;
  content: string;
  created_at: string;
  completed_at?: string | null;
  interrupted_at?: string | null;
  prompt_tokens?: number | null;
  completion_tokens?: number | null;
  activity?: MsgActivity | null;
  groundedness?: MsgGroundedness | null;
  model_name?: string | null;
  content_hash?: string | null;
  citation_coverage?: number | null;
  retrieval_meta?: unknown;
  citations: { doc_id?: string | null; quote_text: string; page_number?: number | null; clause_section_ref?: string | null }[];
  web_citations: { url: string; title?: string | null; domain: string; quote_text: string }[];
  verification?: {
    run_id: string;
    status: string;
    score: number | null;
    total: number;
    supported: number;
    contradicted: number;
    not_mentioned: number;
    verdicts: { claim_text: string; verdict: string; repair_action?: string | null; had_citation: boolean }[];
  } | null;
  run?: {
    id: string;
    status: string;
    step_count: number;
    token_used: number;
    events: { action: string; occurred_epoch?: number | null; payload?: unknown }[];
  } | null;
  artefacts: { id: string; kind: string; title: string }[];
  review?: ReviewState | null;
}
export function useMessageReview(messageId: string | undefined) {
  return useQuery({
    queryKey: ["message-review", messageId],
    queryFn: () => apiFetch<ReviewBundle>(`/api/messages/${messageId}/review`),
    enabled: !!messageId,
  });
}
export function submitMessageReview(
  messageId: string,
  decision: "approved" | "changes_requested" | "rejected",
  note?: string,
): Promise<ReviewState> {
  return apiFetch(`/api/messages/${messageId}/review`, { method: "POST", body: JSON.stringify({ decision, note }) });
}

// ── Verify draft (groundedness Mode B) ──────────────────────────────────────
export type Verdict = "supported" | "contradicted" | "not_mentioned";

export type RepairAction = "regenerated" | "cut" | "kept";

export interface VerifyClaim {
  claim_text: string;
  verdict: Verdict;
  score: number | null;
  evidence: string;
  section: string;
  had_citation: boolean;
  /** The claim's verbatim span in the document; null if unlocatable. */
  source_text: string | null;
  /** Set once ground-or-cut repair has run on this claim. */
  repair_action: RepairAction | null;
}

export interface VerificationRun {
  id: string;
  target_type: string;
  target_id: string;
  mode: string;
  status: string; // queued | running | succeeded | error
  verifier_model: string;
  faithfulness_score: number | null;
  total_claims: number;
  supported: number;
  contradicted: number;
  not_mentioned: number;
  created_at: string;
  finished_at: string | null;
  claims: VerifyClaim[];
}

export function startVerifyDraft(
  target_type: "draft" | "document",
  target_id: string,
): Promise<{ run_id: string; status: string }> {
  return apiFetch("/api/verify-draft", {
    method: "POST",
    body: JSON.stringify({ target_type, target_id }),
  });
}

/** Ground-or-cut repair of a finished verification run. Enqueues a durable
 *  job that proposes tracked changes; surfaced in the accept/reject panel. */
export function startRepair(runId: string): Promise<{ status: string }> {
  return apiFetch(`/api/verification-runs/${runId}/repair`, { method: "POST" });
}

const stillRunning = (s: string | undefined) => s === "queued" || s === "running";

export function useVerificationRun(runId: string | undefined) {
  return useQuery({
    queryKey: ["verification-run", runId],
    queryFn: () => apiFetch<VerificationRun>(`/api/verification-runs/${runId}`),
    enabled: !!runId,
    refetchInterval: (q) => (stillRunning((q.state.data as VerificationRun | undefined)?.status) ? 1500 : false),
  });
}

export function useLatestVerification(target_type: "draft" | "document" | "message", target_id: string | undefined) {
  return useQuery({
    queryKey: ["verification-latest", target_type, target_id],
    queryFn: () =>
      apiFetch<VerificationRun | null>(
        `/api/verification-runs?target_type=${target_type}&target_id=${target_id}`,
      ),
    enabled: !!target_id,
    refetchInterval: (q) => (stillRunning((q.state.data as VerificationRun | null | undefined)?.status) ? 1500 : false),
  });
}

export interface ProjectSummary {
  id: string;
  name: string;
  sector: string;
  description: string | null;
}

export function useChats() {
  return useQuery({ queryKey: ["chats"], queryFn: () => apiFetch<ChatSummary[]>("/api/chats") });
}

/** Deep Research runs (mode=research chats) — listed only in the Research mode. */
export function useResearchChats(enabled = true) {
  return useQuery({
    queryKey: ["chats", "research"],
    queryFn: () => apiFetch<ChatSummary[]>("/api/chats?mode=research"),
    enabled,
  });
}

// --- Platform API keys -------------------------------

/** A key the user has minted for an external application. The secret itself is
 * returned once, by createApiKey, and is unrecoverable afterwards. */
export interface ApiKey {
  id: string;
  name: string;
  display_prefix: string;
  created_at: string;
  last_used_at: string | null;
  expires_at: string | null;
  revoked_at: string | null;
}

export interface CreatedApiKey extends ApiKey {
  /** Shown once. Never returned again. */
  token: string;
}

export function useMyApiKeys(enabled = true) {
  return useQuery({
    queryKey: ["api-keys"],
    queryFn: () => apiFetch<ApiKey[]>("/api/me/api-keys"),
    enabled,
  });
}

export async function createApiKey(body: { name: string; expires_in_days?: number | null }) {
  return apiFetch<CreatedApiKey>("/api/me/api-keys", {
    method: "POST",
    body: JSON.stringify(body),
  });
}

export async function revokeApiKey(id: string) {
  return apiFetch<void>(`/api/me/api-keys/${id}`, { method: "DELETE" });
}

export function useUserApiKeys(userId: string | null) {
  return useQuery({
    queryKey: ["admin-api-keys", userId],
    queryFn: () => apiFetch<ApiKey[]>(`/api/admin/users/${userId}/api-keys`),
    enabled: !!userId,
  });
}

export async function adminRevokeApiKey(userId: string, keyId: string) {
  return apiFetch<void>(`/api/admin/users/${userId}/api-keys/${keyId}`, { method: "DELETE" });
}

// --- Deep Research -----------------------------------

export interface ResearchRequestBody {
  question: string;
  source: "web" | "files" | "hybrid";
  /** A built-in template id or a user-defined template's UUID. */
  template: string;
  /** Narrowed corpus scope (subset of readable libraries); empty ⇒ all. */
  kb_ids?: string[];
  /** Triage-chip answers steering scope voice (non-scope clarifications). */
  refinements?: string[];
  /** Set on the re-prepare after answering/skipping the chips. */
  skip_triage?: boolean;
}

/** A readable library in the DR scope (corpus modes) — for the scope picker. */
export interface ResearchScopeEntry {
  kb_id: string;
  name: string;
  kind: "project" | "library";
  doc_count: number;
}

/** A clarifying chip option: narrows scope (kb_ids) or adds a refinement. */
export interface TriageOption {
  label: string;
  kb_ids: string[];
  refinement: string | null;
}

export interface TriageQuestion {
  id: string;
  prompt: string;
  options: TriageOption[];
}

export interface ResearchPrepareOut {
  scope_summary: string;
  estimate_minutes_lo: number;
  estimate_minutes_hi: number;
  /** Readable libraries (corpus modes); empty for web-only. */
  scope: ResearchScopeEntry[];
  doc_count: number;
  /** Clarifying chips when the question is ambiguous (corpus modes). */
  questions: TriageQuestion[];
}

/** Side-effect-free plan gate: scope summary + estimate (403 when the
 * web-search connector is dormant — rendered as the honest refusal). */
export function prepareResearch(body: ResearchRequestBody) {
  return apiFetch<ResearchPrepareOut>("/api/research/prepare", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
}

/** Start the run: creates the research chat + durable run, returns the chat. */
export function startResearch(body: ResearchRequestBody) {
  return apiFetch<{ chat_id: string; run_id: string | null }>("/api/research/start", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
}

// --- Deep Research templates -------------------------

/** One section in a report template's structure. The per-section flags are what
 * the editor toggles; the research service derives the shape it consumes. */
export interface ResearchTemplateSection {
  heading: string;
  brief: string;
  expandable: boolean;
  exec_summary: boolean;
}

/** A built-in template's picker metadata (headings only; the service owns the
 * body). */
export interface BuiltinTemplate {
  id: string;
  label: string;
  description: string;
  structure: string[];
  outline_mode: "constrained" | "free";
}

/** A user-defined template as it appears in the picker catalogue. */
export interface CustomTemplateSummary {
  id: string;
  label: string;
  description: string;
  structure: string[];
  outline_mode: "constrained" | "free";
  scope: "personal" | "global";
  can_manage: boolean;
}

export interface ResearchTemplateCatalogue {
  builtin: BuiltinTemplate[];
  custom: CustomTemplateSummary[];
}

/** Full detail of a user-defined template (the editor's load shape). */
export interface ResearchTemplateDetail {
  id: string;
  label: string;
  description: string;
  skeleton: ResearchTemplateSection[];
  writing_instructions: string;
  outline_mode: "constrained" | "free";
  scope: "personal" | "global";
  can_manage: boolean;
  archived: boolean;
}

export function useResearchTemplates() {
  return useQuery({
    queryKey: ["research", "templates"],
    queryFn: () => apiFetch<ResearchTemplateCatalogue>("/api/research/templates"),
  });
}

export function useResearchTemplate(id: string | undefined) {
  return useQuery({
    queryKey: ["research", "template", id],
    queryFn: () => apiFetch<ResearchTemplateDetail>(`/api/research/templates/${id}`),
    enabled: !!id,
  });
}

/** The editable body of a template (shared by create-from-scratch and update). */
export interface ResearchTemplateBody {
  label: string;
  description: string;
  skeleton: ResearchTemplateSection[];
  writing_instructions: string;
  outline_mode: "constrained" | "free";
}

/** Create a template. Pass `duplicate_of` (a built-in id) to fork an editable
 * copy of a built-in (personal, filled from the research service), or a full
 * body to create from scratch. `scope` defaults to personal. */
export function createResearchTemplate(
  body: ({ duplicate_of: string } | ResearchTemplateBody) & { scope?: "personal" | "global" },
): Promise<{ id: string }> {
  return apiFetch<{ id: string }>("/api/research/templates", {
    method: "POST",
    body: JSON.stringify(body),
  });
}

export function updateResearchTemplate(
  id: string,
  body: ResearchTemplateBody & { scope?: "personal" | "global" },
): Promise<{ ok: boolean }> {
  return apiFetch<{ ok: boolean }>(`/api/research/templates/${id}`, {
    method: "PATCH",
    body: JSON.stringify(body),
  });
}

/** Archive (soft-delete) a template. Existing chats keep resolving it. */
export function archiveResearchTemplate(id: string): Promise<{ ok: boolean }> {
  return apiFetch<{ ok: boolean }>(`/api/research/templates/${id}`, { method: "DELETE" });
}

/** Convert a stored markdown artefact to DOCX/PDF (dedupes server-side). */
export function convertArtefact(id: string, to: "docx" | "pdf") {
  return apiFetch<Artefact>(`/api/artefacts/${id}/convert`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ to }),
  });
}

/** Turn a Deep Research report (markdown artefact) into a self-contained HTML page —
 *  the "Create page" button. Server-side: deterministic report-to-page injection +
 *  vendored-lib inlining; dedupes. */
export function createPage(id: string) {
  return apiFetch<Artefact>(`/api/artefacts/${id}/create-page`, { method: "POST" });
}

/** Fetch an artefact's raw text (e.g. an html artefact's source for a sandboxed
 *  preview). Reuses the download endpoint and reads the body as text. */
export async function fetchArtefactText(id: string): Promise<string> {
  const token = await freshToken();
  const res = await fetch(`/api/artefacts/${id}/download`, { headers: { Authorization: `Bearer ${token}` } });
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
  return res.text();
}

export function useProjects() {
  return useQuery({ queryKey: ["projects"], queryFn: () => apiFetch<ProjectSummary[]>("/api/projects") });
}

export function useChatMessages(chatId: string | undefined) {
  return useQuery({
    queryKey: ["chat-messages", chatId],
    queryFn: () => apiFetch<MessageOut[]>(`/api/chats/${chatId}/messages`),
    enabled: !!chatId,
    // Poll while a turn is still being written so a reopened/reloaded chat resumes
    // the answer from the DB; stop once it settles.
    refetchInterval: (q) => ((q.state.data as MessageOut[] | undefined)?.some((m) => m.streaming) ? 1200 : false),
  });
}

/** Download a chat transcript (md/json/pdf) via an anchor (Bearer → blob). */
export async function exportChat(id: string, format: "md" | "json" | "pdf"): Promise<void> {
  const token = await freshToken();
  const res = await fetch(`/api/chats/${id}/export?format=${format}`, { headers: { Authorization: `Bearer ${token}` } });
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
  const url = URL.createObjectURL(await res.blob());
  const a = document.createElement("a");
  a.href = url;
  a.download = `chat.${format}`;
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}

/** Download a groundedness verification report (md/pdf/docx) via an anchor (Bearer → blob). */
export async function downloadVerificationReport(runId: string, format: "md" | "pdf" | "docx"): Promise<void> {
  const token = await freshToken();
  const res = await fetch(`/api/verification-runs/${runId}/report?format=${format}`, {
    headers: { Authorization: `Bearer ${token}` },
  });
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
  const url = URL.createObjectURL(await res.blob());
  const a = document.createElement("a");
  a.href = url;
  a.download = `groundedness-report.${format}`;
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}

export function renameChat(id: string, title: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/chats/${id}`, { method: "PATCH", body: JSON.stringify({ title }) });
}
export function deleteChat(id: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/chats/${id}`, { method: "DELETE" });
}

/** Lightweight user directory (any authenticated user) for member/grant pickers. */
export interface UserEntry {
  id: string;
  display_name: string;
  email: string;
  /** Epoch (s) of last avatar change; null = none. Used as `?v=` cache-buster. */
  avatar_updated_at: number | null;
}
export function useUsers() {
  return useQuery({ queryKey: ["users"], queryFn: () => apiFetch<UserEntry[]>("/api/users") });
}

// ── Self-service profile (own name + avatar) ─────────────────────────────────

export interface MyProfile {
  user_id: string;
  email: string | null;
  display_name: string | null;
  display_name_custom: boolean;
  role: string;
  /** Account creation time (epoch seconds). */
  created_epoch: number;
  avatar_updated_at: number | null;
  /** Keycloak account console URL (password/MFA); empty when KC isn't configured. */
  account_url: string;
}

export function useMyProfile() {
  return useQuery({ queryKey: ["myProfile"], queryFn: () => apiFetch<MyProfile>("/api/me/profile") });
}

/** Refresh everything that shows the user's name/avatar after an edit. */
async function invalidateIdentity(): Promise<void> {
  await Promise.all([
    queryClient.invalidateQueries({ queryKey: ["whoami"] }),
    queryClient.invalidateQueries({ queryKey: ["users"] }),
    queryClient.invalidateQueries({ queryKey: ["myProfile"] }),
  ]);
}

export async function updateMyName(display_name: string): Promise<void> {
  await apiFetch("/api/me/profile", { method: "PATCH", body: JSON.stringify({ display_name }) });
  await invalidateIdentity();
}

/** Local-auth password change (current → new). Only meaningful under AUTH_MODE=local;
 *  the backend rejects Keycloak-only accounts ("no local account"). */
export async function changePassword(current_password: string, new_password: string): Promise<void> {
  await apiFetch("/api/auth/password", { method: "POST", body: JSON.stringify({ current_password, new_password }) });
}

export async function uploadMyAvatar(file: File): Promise<void> {
  const token = await freshToken();
  const mime = file.type || "image/png";
  const res = await fetch(`/api/me/avatar?mime=${encodeURIComponent(mime)}`, {
    method: "POST",
    headers: { Authorization: `Bearer ${token}`, "Content-Type": mime },
    body: file,
  });
  if (!res.ok) throw new Error(`${res.status}: ${(await res.text().catch(() => "")).slice(0, 200)}`);
  await invalidateIdentity();
}

export async function removeMyAvatar(): Promise<void> {
  await apiFetch("/api/me/avatar", { method: "DELETE" });
  await invalidateIdentity();
}

export function createProject(name: string, sector: string): Promise<{ id: string }> {
  return apiFetch<{ id: string }>("/api/projects", {
    method: "POST",
    body: JSON.stringify({ name, sector }),
  });
}

/** Archive (soft-delete) a project — owner or admin only. Recoverable. */
export function deleteProject(id: string): Promise<void> {
  return apiFetch<{ ok: boolean }>(`/api/projects/${id}`, { method: "DELETE" }).then(() => undefined);
}

export type DocStatus = "uploaded" | "extracting" | "indexing" | "ready" | "error";

export interface KnowledgeDoc {
  id: string;
  filename: string;
  mime: string | null;
  status: DocStatus;
  created_at: string;
  /** How the doc entered the KB: "upload" | "connector_import". Optional — only
   *  the KB detail endpoint returns it. */
  source?: string;
}

export interface ProjectDocs {
  knowledge: { id: string; status: string } | null;
  documents: KnowledgeDoc[];
}

const TERMINAL: DocStatus[] = ["ready", "error"];

export function useProjectDocs(projectId: string | undefined) {
  return useQuery({
    queryKey: ["project-docs", projectId],
    queryFn: () => apiFetch<ProjectDocs>(`/api/projects/${projectId}/documents`),
    enabled: !!projectId,
    // Poll while any doc is still ingesting.
    refetchInterval: (q) => {
      const d = q.state.data as ProjectDocs | undefined;
      return d && d.documents.some((x) => !TERMINAL.includes(x.status)) ? 2000 : false;
    },
  });
}

export function createKnowledge(projectId: string): Promise<unknown> {
  return apiFetch(`/api/projects/${projectId}/knowledge`, { method: "POST" });
}

/** Binary upload — bypasses the JSON path of apiFetch (raw bytes body). */
export async function uploadDocument(projectId: string, file: File): Promise<void> {
  const token = await freshToken();
  const qs = new URLSearchParams({ filename: file.name, mime: file.type || "application/octet-stream" });
  const res = await fetch(`/api/projects/${projectId}/documents?${qs}`, {
    method: "POST",
    headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/octet-stream" },
    body: file,
  });
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
}

// ── Libraries (standalone Knowledge Bases) ───────────────────────────────────
// A KB is a first-class, shareable entity. Retrieval honours the *caller's*
// access (the intersection rule) — attaching a Library never exposes it to
// project members who cannot read it.

export type KbVisibility = "personal" | "project" | "team" | "shared";

export interface KbSummary {
  id: string;
  name: string;
  description: string | null;
  owner_id: string;
  visibility: KbVisibility;
  origin_project_id: string | null;
  restricted: boolean;
  embedding_model_id: string;
  embedding_dimension: number;
  status: string;
  created_at: string;
  mine: boolean;
  can_manage: boolean;
}
export interface KbDetail extends KbSummary {
  documents: KnowledgeDoc[];
}

export function useLibraries() {
  return useQuery({ queryKey: ["kb"], queryFn: () => apiFetch<KbSummary[]>("/api/kb") });
}
export function useLibrary(id: string | undefined) {
  return useQuery({
    queryKey: ["kb", id],
    queryFn: () => apiFetch<KbDetail>(`/api/kb/${id}`),
    enabled: !!id,
    refetchInterval: (q) => {
      const d = q.state.data as KbDetail | undefined;
      return d && d.documents.some((x) => !TERMINAL.includes(x.status)) ? 2000 : false;
    },
  });
}
export function createLibrary(body: { name: string; description?: string; visibility?: KbVisibility; parent_child?: boolean }): Promise<{ id: string }> {
  return apiFetch("/api/kb", { method: "POST", body: JSON.stringify(body) });
}
export function patchLibrary(
  id: string,
  body: Partial<{ name: string; description: string; visibility: KbVisibility; restricted: boolean }>,
): Promise<unknown> {
  return apiFetch(`/api/kb/${id}`, { method: "PATCH", body: JSON.stringify(body) });
}
export function deleteLibrary(id: string): Promise<unknown> {
  return apiFetch(`/api/kb/${id}`, { method: "DELETE" });
}
export async function uploadLibraryDocument(kbId: string, file: File): Promise<void> {
  const token = await freshToken();
  const qs = new URLSearchParams({ filename: file.name, mime: file.type || "application/octet-stream" });
  const res = await fetch(`/api/kb/${kbId}/documents?${qs}`, {
    method: "POST",
    headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/octet-stream" },
    body: file,
  });
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
}
export function deleteLibraryDocument(kbId: string, docId: string): Promise<unknown> {
  return apiFetch(`/api/kb/${kbId}/documents/${docId}`, { method: "DELETE" });
}

// Per-Library RBAC (share dialog). `manage` implies `read`.
export interface KbGrant {
  id: string;
  principal_type: string;
  principal_id: string;
  permission: "read" | "manage";
  name: string | null;
}
export function useKbGrants(kbId: string | undefined, enabled = true) {
  return useQuery({
    queryKey: ["kb-grants", kbId],
    queryFn: () => apiFetch<KbGrant[]>(`/api/kb/${kbId}/grants`),
    enabled: !!kbId && enabled,
  });
}
export function putKbGrant(
  kbId: string,
  body: { principal_type: "user" | "group"; principal_id: string; permission: "read" | "manage" },
): Promise<unknown> {
  return apiFetch(`/api/kb/${kbId}/grants`, { method: "PUT", body: JSON.stringify(body) });
}
export function deleteKbGrant(kbId: string, grantId: string): Promise<unknown> {
  return apiFetch(`/api/kb/${kbId}/grants/${grantId}`, { method: "DELETE" });
}
export function promoteLibrary(
  kbId: string,
  body: { visibility: "team" | "shared" | "personal"; grants?: Array<{ principal_type: "user" | "group"; principal_id: string; permission: "read" | "manage" }> },
): Promise<unknown> {
  return apiFetch(`/api/kb/${kbId}/promote`, { method: "POST", body: JSON.stringify(body) });
}

// Project / chat attach-detach.
export interface AttachedLib {
  id: string;
  name: string;
  visibility: KbVisibility;
  restricted: boolean;
  is_default: boolean;
}
export function useProjectLinks(projectId: string | undefined) {
  return useQuery({
    queryKey: ["project-kb-links", projectId],
    queryFn: () => apiFetch<AttachedLib[]>(`/api/projects/${projectId}/kb-links`),
    enabled: !!projectId,
  });
}
export function attachProjectLibrary(projectId: string, kbId: string): Promise<unknown> {
  return apiFetch(`/api/projects/${projectId}/kb-links`, { method: "POST", body: JSON.stringify({ kb_id: kbId }) });
}
export function detachProjectLibrary(projectId: string, kbId: string): Promise<unknown> {
  return apiFetch(`/api/projects/${projectId}/kb-links/${kbId}`, { method: "DELETE" });
}
export function useChatLinks(chatId: string | undefined) {
  return useQuery({
    queryKey: ["chat-kb-links", chatId],
    queryFn: () => apiFetch<AttachedLib[]>(`/api/chats/${chatId}/kb-links`),
    enabled: !!chatId,
  });
}
export function attachChatLibrary(chatId: string, kbId: string): Promise<unknown> {
  return apiFetch(`/api/chats/${chatId}/kb-links`, { method: "POST", body: JSON.stringify({ kb_id: kbId }) });
}
export function detachChatLibrary(chatId: string, kbId: string): Promise<unknown> {
  return apiFetch(`/api/chats/${chatId}/kb-links/${kbId}`, { method: "DELETE" });
}

// ── Tabular review ───────────────────────────────────────────────────────────
// Reviews run on *workspace* documents (the `documents` table — version-pinned),
// NOT the Project-Knowledge RAG docs. Cells stream in live over the WS
// (tabular.cell carries status only → refetch; tabular.complete ends the run).

/** A workspace (editable) document — the rows a review runs over. */
export interface WorkspaceDoc {
  id: string;
  original_filename: string;
  mime: string | null;
  current_version_id: string | null;
}

export type CellFormat =
  | "text" | "yes_no" | "date" | "currency" | "number"
  | "percentage" | "tag" | "bulleted_list" | "monetary_amount";

export type CellMechanism = "stuff" | "per_document_rag" | "map_section";

/** A column the user defines on the create form (mirrors backend ColumnSpec). */
export interface ColumnSpec {
  key: string;
  name: string;
  format: CellFormat;
  prompt: string;
  mechanism: CellMechanism;
}

export interface ReviewSummary {
  id: string;
  name: string;
  status: string;
}

export type CellStatus = "pending" | "running" | "done" | "error";

export interface Cell {
  document_id: string;
  column_key: string;
  status: CellStatus;
  value: unknown | null; // arbitrary JSON shaped by the column format
  reasoning: string | null;
  citations: Citation[] | null;
  error: string | null;
}

export interface ReviewDetail {
  id: string;
  name: string;
  status: string;
  columns: ColumnSpec[]; // columns_config, as stored
  documents: { id: string; filename: string }[];
  cells: Cell[];
}

export function useWorkspaceDocs(projectId: string | undefined) {
  return useQuery({
    queryKey: ["workspace-docs", projectId],
    queryFn: () => apiFetch<WorkspaceDoc[]>(`/api/projects/${projectId}/workspace/documents`),
    enabled: !!projectId,
  });
}

/** Binary upload of a workspace document (mirrors uploadDocument). */
export async function uploadWorkspaceDoc(projectId: string, file: File): Promise<void> {
  const token = await freshToken();
  const qs = new URLSearchParams({ filename: file.name, mime: file.type || "application/octet-stream" });
  const res = await fetch(`/api/projects/${projectId}/workspace/documents?${qs}`, {
    method: "POST",
    headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/octet-stream" },
    body: file,
  });
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
}

/** Upload a per-turn chat attachment → staged server-side; returns its id to pass
 *  in the next chat.send (the model reads its text for that turn only). */
export async function uploadChatAttachment(file: File): Promise<{ id: string; filename: string; chars: number }> {
  const token = await freshToken();
  const qs = new URLSearchParams({ filename: file.name, mime: file.type || "application/octet-stream" });
  // Bound the request so a stalled upload / slow server-side extraction surfaces as a
  // clear "timed out" rather than the browser's opaque "Failed to fetch". 5 min is
  // generous — large files extract slowly server-side.
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), 5 * 60 * 1000);
  let res: Response;
  try {
    res = await fetch(`/api/chat-attachments?${qs}`, {
      method: "POST",
      headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/octet-stream" },
      body: file,
      signal: ctl.signal,
    });
  } catch (e) {
    if (ctl.signal.aborted) throw new Error("upload timed out");
    throw e;
  } finally {
    clearTimeout(timer);
  }
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
  return res.json();
}

export function useReviews(projectId: string | undefined) {
  return useQuery({
    queryKey: ["reviews", projectId],
    queryFn: () => apiFetch<ReviewSummary[]>(`/api/projects/${projectId}/tabular-reviews`),
    enabled: !!projectId,
  });
}

export interface CreateReviewBody {
  project_id: string;
  name: string;
  document_ids: string[];
  columns: ColumnSpec[];
}

export function createReview(body: CreateReviewBody): Promise<{ id: string }> {
  return apiFetch<{ id: string }>("/api/tabular-reviews", {
    method: "POST",
    body: JSON.stringify(body),
  });
}

const REVIEW_RUNNING = "running";

export function useReview(reviewId: string | undefined) {
  return useQuery({
    queryKey: ["review", reviewId],
    queryFn: () => apiFetch<ReviewDetail>(`/api/tabular-reviews/${reviewId}`),
    enabled: !!reviewId,
    // A safety-net poll while running, in case a WS frame is missed; the WS
    // refetch is the primary live path.
    refetchInterval: (q) => ((q.state.data as ReviewDetail | undefined)?.status === REVIEW_RUNNING ? 3000 : false),
  });
}

export function runReview(id: string): Promise<unknown> {
  return apiFetch(`/api/tabular-reviews/${id}/run`, { method: "POST" });
}

export function cancelReview(id: string): Promise<unknown> {
  return apiFetch(`/api/tabular-reviews/${id}/cancel`, { method: "POST" });
}

export function rerunErrors(id: string): Promise<{ status: string; reran: number }> {
  return apiFetch(`/api/tabular-reviews/${id}/rerun-errors`, { method: "POST" });
}

export function rerunCell(id: string, documentId: string, columnKey: string): Promise<unknown> {
  return apiFetch(`/api/tabular-reviews/${id}/cells/${documentId}/${encodeURIComponent(columnKey)}/rerun`, {
    method: "POST",
  });
}

/** Download the xlsx export via an anchor (needs the Bearer header → blob). */
export async function exportReview(id: string, name: string): Promise<void> {
  const token = await freshToken();
  const res = await fetch(`/api/tabular-reviews/${id}/export`, {
    headers: { Authorization: `Bearer ${token}` },
  });
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
  const blob = await res.blob();
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = `${name || "review"}.xlsx`;
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}

// ── Workspace documents + tracked changes ────────────────────────────────────
// Edits are proposed by the assistant's `edit_document` tool → a new
// `assistant_edit` version (DOCX with <w:ins>/<w:del>) + pending document_edits
// rows. Accept/reject (by w_id / all / author) each create a NEW version, so
// every action refetches the document + edits and re-renders the current DOCX.

/** Thrown on HTTP 409 — a concurrent accept/reject advanced the version. */
export class ConflictError extends Error {
  constructor(message = "document changed") {
    super(message);
    this.name = "ConflictError";
  }
}

export interface DocVersion {
  id: string;
  version_number: number;
  source: string;
  byte_size: number | null;
  has_pdf: boolean;
}

export interface DocDetail {
  id: string;
  original_filename: string;
  mime: string | null;
  current_version_id: string | null;
  versions: DocVersion[];
  pending_edits: number;
}

export type EditAuthor = "assistant" | "human";
export type EditStatus = "pending" | "accepted" | "rejected";

export interface DocEdit {
  id: string;
  w_id: string;
  author: EditAuthor;
  find_text: string | null;
  replace_text: string | null;
  status: EditStatus;
}

export interface ResolvedVersion {
  version_id: string;
  version_number: number;
}

export function useDocument(documentId: string | undefined) {
  return useQuery({
    queryKey: ["document", documentId],
    queryFn: () => apiFetch<DocDetail>(`/api/documents/${documentId}`),
    enabled: !!documentId,
  });
}

export function useDocEdits(documentId: string | undefined, status: "pending" | "accepted" | "rejected" | "all" = "pending") {
  return useQuery({
    queryKey: ["doc-edits", documentId, status],
    queryFn: () => apiFetch<DocEdit[]>(`/api/documents/${documentId}/edits?status=${status}`),
    enabled: !!documentId,
  });
}

/** Raw DOCX bytes of a version (for docx-preview). Bearer fetch → Blob. */
export async function fetchVersionDocx(documentId: string, versionId: string): Promise<Blob> {
  const token = await freshToken();
  const res = await fetch(`/api/documents/${documentId}/versions/${versionId}/download`, {
    headers: { Authorization: `Bearer ${token}` },
  });
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
  return res.blob();
}

/** Plain text of a version — fallback render for non-DOCX documents. */
export async function fetchVersionText(documentId: string, versionId: string): Promise<string> {
  const { text } = await apiFetch<{ text: string }>(`/api/documents/${documentId}/versions/${versionId}/text`);
  return text;
}

/** Render (or fetch cached) the version's PDF and return it as a blob (for inline embed). */
export async function fetchVersionPdf(documentId: string, versionId: string): Promise<Blob> {
  const token = await freshToken();
  const res = await fetch(`/api/documents/${documentId}/versions/${versionId}/pdf`, {
    method: "POST",
    headers: { Authorization: `Bearer ${token}` },
  });
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
  return res.blob();
}

/** Render (or fetch cached) the version's PDF and open it in a new tab. */
export async function openVersionPdf(documentId: string, versionId: string): Promise<void> {
  const token = await freshToken();
  const res = await fetch(`/api/documents/${documentId}/versions/${versionId}/pdf`, {
    method: "POST",
    headers: { Authorization: `Bearer ${token}` },
  });
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
  const url = URL.createObjectURL(await res.blob());
  window.open(url, "_blank", "noopener");
  // The opened tab holds its own reference; revoke shortly after to free memory.
  setTimeout(() => URL.revokeObjectURL(url), 60_000);
}

async function postEdit(path: string): Promise<ResolvedVersion> {
  const token = await freshToken();
  const res = await fetch(path, { method: "POST", headers: { Authorization: `Bearer ${token}` } });
  if (res.status === 409) throw new ConflictError();
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}: ${(await res.text().catch(() => "")).slice(0, 200)}`);
  return res.json();
}

export function acceptEdit(documentId: string, wId: string): Promise<ResolvedVersion> {
  return postEdit(`/api/documents/${documentId}/edits/${encodeURIComponent(wId)}/accept`);
}

export function rejectEdit(documentId: string, wId: string): Promise<ResolvedVersion> {
  return postEdit(`/api/documents/${documentId}/edits/${encodeURIComponent(wId)}/reject`);
}

export function acceptAllEdits(documentId: string, author?: EditAuthor): Promise<ResolvedVersion> {
  const qs = author ? `?author=${author}` : "";
  return postEdit(`/api/documents/${documentId}/edits/accept-all${qs}`);
}

export function rejectAllEdits(documentId: string, author?: EditAuthor): Promise<ResolvedVersion> {
  const qs = author ? `?author=${author}` : "";
  return postEdit(`/api/documents/${documentId}/edits/reject-all${qs}`);
}

// ── Moderation (accountability, not refusal) ─────────────────────────────────
// Neutral terminology only: review priority / category / out-of-pattern — never
// risk/suspicion. Queue endpoints are moderator-only (team-scoped); settings +
// assignments are admin-only and never return a flag.

export interface ModerationFlagSummary {
  id: string;
  subject_user_id: string;
  subject_name: string;
  team_id: string | null;
  team_name: string | null;
  category: string;
  review_priority: number;
  status: string;
  created_at: string;
}

export interface ModerationFlagDetail extends ModerationFlagSummary {
  subject_email: string;
  chat_id: string;
  prompt_excerpt: string;
  out_of_hours: boolean;
  project_attached: boolean;
  topic_in_user_practice: boolean;
  related_matter_in_access: boolean;
  operational_uplift: boolean;
}

export function useModerationFlags() {
  return useQuery({
    queryKey: ["moderation", "flags"],
    queryFn: () => apiFetch<ModerationFlagSummary[]>("/api/moderation/flags"),
    // Flags are written by the async post-turn classifier seconds later — poll so a
    // new one appears without a manual reload (pull model, no push).
    refetchInterval: 8000,
  });
}

export function useModerationFlag(id: string | undefined) {
  return useQuery({
    queryKey: ["moderation", "flag", id],
    queryFn: () => apiFetch<ModerationFlagDetail>(`/api/moderation/flags/${id}`),
    enabled: !!id,
  });
}

export function reviewModerationFlag(id: string, status: "reviewed" | "dismissed"): Promise<{ ok: boolean }> {
  return apiFetch(`/api/moderation/flags/${id}/review`, { method: "POST", body: JSON.stringify({ status }) });
}

export interface ModerationSettings {
  enabled: boolean;
  staff_notice_text: string;
  lawful_basis: string;
  threshold: number;
  weight_topic_mismatch: number;
  weight_no_matter: number;
  weight_no_project: number;
  weight_out_of_hours: number;
  weight_operational: number;
  weight_llm_anomaly: number;
  working_hours_start: number;
  working_hours_end: number;
  working_hours_offset_minutes: number;
  retention_days: number;
  classifier_sample_rate: number;
}

export function useModerationSettings() {
  return useQuery({
    queryKey: ["moderation", "settings"],
    queryFn: () => apiFetch<ModerationSettings>("/api/moderation/settings"),
  });
}

export function updateModerationSettings(s: ModerationSettings): Promise<{ ok: boolean }> {
  return apiFetch("/api/moderation/settings", { method: "PATCH", body: JSON.stringify(s) });
}

export interface ModeratorAssignment {
  moderator_user_id: string;
  moderator_name: string;
  team_id: string;
  team_name: string;
  created_at: string;
}

export function useModeratorAssignments() {
  return useQuery({
    queryKey: ["moderation", "assignments"],
    queryFn: () => apiFetch<ModeratorAssignment[]>("/api/moderation/assignments"),
  });
}

export function addModeratorAssignment(moderator_user_id: string, team_id: string): Promise<{ ok: boolean }> {
  return apiFetch("/api/moderation/assignments", { method: "POST", body: JSON.stringify({ moderator_user_id, team_id }) });
}

export function removeModeratorAssignment(moderatorUserId: string, teamId: string): Promise<{ ok: boolean }> {
  return apiFetch(`/api/moderation/assignments/${moderatorUserId}/${teamId}`, { method: "DELETE" });
}
