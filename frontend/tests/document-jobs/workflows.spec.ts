import { test, expect, type Page } from "@playwright/test";
const url = "/tests/document-jobs/workflows.html";
const local = "10000000-0000-0000-0000-000000000001";
const cloud = "10000000-0000-0000-0000-000000000002";
const rules = "20000000-0000-0000-0000-000000000001";
const hybrid = "20000000-0000-0000-0000-000000000002";
const off = "20000000-0000-0000-0000-000000000003";
const personal = "20000000-0000-0000-0000-000000000004";
function policy(id: string, label: string, strategy: string, rollout: string, scope = "deployment", is_default = false) {
  return { id, label, scope, revision: 1, is_default, policy: { id, revision: 1, strategy, rollout, classifier_provider_id: local } };
}
async function fixture(page: Page) {
  const state = {
    capsError: false, catalogError: false, policyError: false, enabled: true, failSave: false, detailReads: 0,
    writes: [] as { path: string; body: any }[], existing: true,
    workflow: { id: "workflow-one", name: "Document summary", description: null, owner_id: "owner", enabled: false,
      project_id: null, agent_id: null, trigger_event_type: "document.ingested", trigger_scope: {}, trigger_on_system_events: false,
      condition: null as any, coalesce_window_secs: 0, max_runs_per_window: 60, action_type: "agent_run", version: 1,
      action_config: { prompt: "Summarise {{filename}}" } as any },
    policies: [policy(rules, "Office rules", "rules", "active", "deployment", true), policy(hybrid, "Trial judge", "hybrid", "shadow"),
      policy(off, "Paused judge", "classifier", "off"), policy(personal, "My judge", "classifier", "active", "user", true)],
    models: [
      { id: local, label: "Local Qwen", model: "qwen", enabled: true, scope: "deployment", provider_revision: 1, profile_revision: 1,
        profile: { provider_id: local, provider_revision: 1, revision: 1, model: "qwen" } },
      { id: cloud, label: "Cloud model", model: "cloud", enabled: true, scope: "user", provider_revision: 1, profile_revision: 1,
        profile: { provider_id: cloud, provider_revision: 1, revision: 1, model: "cloud" } },
      { id: "stale", label: "Stale model", model: "old", enabled: true, scope: "deployment", provider_revision: 2, profile_revision: 1,
        profile: { provider_id: "stale", provider_revision: 1, revision: 1, model: "old" } },
    ],
  };
  await page.route("**/api/**", async route => {
    const req = route.request(); const path = new URL(req.url()).pathname;
    if (!path.startsWith("/api/")) return route.continue();
    const json = (body: unknown, status = 200) => route.fulfill({ status, json: body });
    if (req.method() !== "GET") {
      state.writes.push({ path, body: req.postDataJSON() });
      if (state.failSave) return json({ error: "Save refused" }, 409);
      if (path === "/api/workflows" || path === "/api/workflows/workflow-one") {
        Object.assign(state.workflow, req.postDataJSON()); state.existing = true;
        return json(path === "/api/workflows" ? { id: "workflow-one" } : { ok: true });
      }
      return json({}, 403);
    }
    if (path === "/api/auth/config") return json({ mode: "local", local_enabled: true });
    if (path === "/api/whoami") return json({ user_id: "owner", role: "admin" });
    if (path === "/api/workflows") return json(state.existing ? [state.workflow] : []);
    if (path === "/api/workflows/workflow-one") { state.detailReads++; return json(state.workflow); }
    if (path === "/api/workflows/triggers") return json([{ name: "document.ingested", description: "Document ingested", emitted: true }]);
    if (path === "/api/workflows/workflow-one/runs") return json([{ id: "run-one", status: "succeeded", event_count: 1,
      created_at: "2026-09-29T10:00:00Z", outcome: { routing: true, selection: { mode: "auto", objective: "economy" }, output_chat_id: "output-one" } }]);
    if (path === "/api/routing/workflow-jobs") return state.capsError ? json({}, 503) : json({ schema_version: 1, enabled: state.enabled,
      selection_modes: ["default", "manual", "auto"], custom_agents: true, group_delivery: true, max_recipients: 128, max_libraries: 32,
      event_placeholders: ["filename", "mime", "kb_id", "doc_id"] });
    if (path === "/api/routing/catalog") return state.catalogError ? json({}, 503) : json(state.models);
    if (path === "/api/routing/policies") return state.policyError ? json({}, 503) : json(state.policies);
    if (path === "/api/agents") return json([{ id: "agent-one", name: "Reader agent", modes: ["general"], tools: ["web_search"] }]);
    if (path === "/api/projects") return json([{ id: "project-one", name: "Matter" }]);
    if (path === "/api/kb") return json([{ id: "library-one", name: "Contracts" }]);
    if (path === "/api/group-chats") return json([{ id: "team-one", name: "Review team", kind: "group" }]);
    return json({}, 404);
  });
  return state;
}
async function edit(page: Page) { await page.goto(url); await page.getByText("Document summary", { exact: true }).click(); }
async function choose(page: Page, control: string, option: string) {
  await page.getByRole("button", { name: control, exact: true }).click();
  await page.getByRole("option", { name: option, exact: true }).click();
}
async function save(page: Page) {
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByRole("button", { name: "Save", exact: true })).toBeEnabled();
}
test("Legacy stays absent; explicit Default is saved without enabling deployment routing", async ({ page }) => {
  const s = await fixture(page); await edit(page);
  await expect(page.getByRole("button", { name: "Workflow model selection" })).toContainText("Legacy");
  await save(page); expect(s.writes[0].body.action_config).not.toHaveProperty("model_selection");
  await choose(page, "Workflow model selection", "Default — configured default model"); await save(page);
  expect(s.writes[1].body.action_config.model_selection).toEqual({ mode: "default" });
  expect(s.writes.every(w => w.path === "/api/workflows/workflow-one")).toBe(true);
});
test("Create Manual with sources and delivery; no stale models and no duplicate save", async ({ page }) => {
  const s = await fixture(page); s.existing = false; await page.goto(url);
  await page.getByRole("button", { name: "New workflow", exact: true }).click();
  await page.getByPlaceholder("e.g. Summarise new matter documents").fill("New routed workflow");
  await page.getByRole("button", { name: "Agent run", exact: true }).click();
  await page.getByPlaceholder(/What should the agent do/).fill("Summarise {{filename}}");
  await choose(page, "Scope project", "Matter"); await choose(page, "Agent", "Reader agent");
  await page.getByRole("button", { name: "Contracts", exact: true }).click();
  await choose(page, "Deliver to team chat", "Review team");
  await choose(page, "Workflow model selection", "Manual — choose a model");
  await expect(page.getByRole("button", { name: "Save", exact: true })).toBeDisabled();
  await page.getByRole("button", { name: "Workflow model", exact: true }).click();
  await expect(page.getByRole("option", { name: /Stale model/ })).toHaveCount(0);
  await page.getByRole("option", { name: "Local Qwen · qwen", exact: true }).click();
  await page.getByRole("button", { name: "Save", exact: true }).dblclick();
  await expect(page.getByRole("button", { name: "Save", exact: true })).toBeEnabled();
  expect(s.writes).toHaveLength(1); expect(s.writes[0].body).toMatchObject({ action_type: "agent_run", agent_id: "agent-one", project_id: "project-one",
    action_config: { prompt: "Summarise {{filename}}", kb_ids: ["library-one"], deliver_group_chat_id: "team-one", model_selection: { mode: "manual", provider_id: local } } });
  expect(s.writes[0].body).not.toHaveProperty("enabled");
});
test("Auto resolves personal default and explains Rules, Hybrid, Shadow and Off", async ({ page }) => {
  const s = await fixture(page); await edit(page);
  await choose(page, "Workflow model selection", "Auto — routing policy");
  await expect(page.getByText("Policy: My judge", { exact: true })).toBeVisible();
  await expect(page.getByText(/Assigned judge: Local Qwen/)).toBeVisible();
  await choose(page, "Workflow routing policy", "Office rules · Deployment");
  await expect(page.getByText(/no model judge is called/)).toBeVisible();
  await choose(page, "Workflow routing policy", "Trial judge · Deployment");
  await expect(page.getByText(/Hybrid: rules run first/)).toBeVisible();
  await expect(page.getByText(/Shadow: routing evaluates a proposal/)).toBeVisible();
  await choose(page, "Workflow routing objective", "Quality — prefer stronger capability"); await save(page);
  expect(s.writes[0].body.action_config.model_selection).toEqual({ mode: "auto", objective: "quality", policy_id: hybrid });
  await choose(page, "Workflow routing policy", "Paused judge · Deployment");
  await expect(page.getByText(/Off: the default model answers/)).toBeVisible();
});
test("Saved intent and custom fields survive refetch; history describes its own run", async ({ page }) => {
  const s = await fixture(page); s.workflow.action_config = { prompt: "API prompt", model_selection: { mode: "auto", objective: "economy", policy_id: hybrid }, future_config: { retain: true } };
  s.workflow.condition = { not: { field: "event_type", op: "eq", value: "none" } }; await edit(page);
  await expect(page.getByRole("button", { name: "Workflow routing objective" })).toContainText("Economy");
  await choose(page, "Workflow routing objective", "Quality — prefer stronger capability");
  const reads = s.detailReads; await page.getByRole("button", { name: "Refresh saved workflow" }).click();
  await expect.poll(() => s.detailReads).toBeGreaterThan(reads);
  await expect(page.getByRole("button", { name: "Workflow routing objective" })).toContainText("Quality"); await save(page);
  expect(s.writes[0].body.action_config.future_config).toEqual({ retain: true }); expect(s.writes[0].body.condition).toEqual(s.workflow.condition);
  expect(s.writes[0].body.action_config.model_selection).toEqual({ mode: "auto", objective: "quality", policy_id: hybrid });
  await expect(page.getByText("Requested: Auto · economy", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Open output chat" }).click(); await expect(page).toHaveURL(/#\/c\/output-one/);
});
test("Gate off allows configuration but never enables routing; errors never coerce a saved choice", async ({ page }) => {
  const s = await fixture(page); s.enabled = false; s.workflow.action_config.model_selection = { mode: "auto", objective: "quality", policy_id: hybrid }; s.capsError = true; await edit(page);
  await expect(page.getByText(/Workflow routing settings are unavailable/)).toBeVisible(); await save(page);
  expect(s.writes[0].body.action_config.model_selection).toEqual({ mode: "auto", objective: "quality", policy_id: hybrid });
  s.capsError = false; await page.getByRole("button", { name: "Retry workflow routing settings" }).click();
  await expect(page.getByText(/Workflow model routing is disabled/)).toBeVisible();
  await choose(page, "Workflow model selection", "Default — configured default model"); await save(page);
  expect(s.writes[1].body.action_config.model_selection).toEqual({ mode: "default" });
  expect(s.writes.every(w => w.path === "/api/workflows/workflow-one")).toBe(true);
});
test("Unavailable catalogs block changed selections; save failure keeps Manual with no fallback", async ({ page }) => {
  const s = await fixture(page); s.catalogError = true; await edit(page);
  await choose(page, "Workflow model selection", "Manual — choose a model");
  await expect(page.getByRole("button", { name: "Save", exact: true })).toBeDisabled();
  s.catalogError = false; await page.getByRole("button", { name: "Retry workflow routing settings" }).click();
  await choose(page, "Workflow model", "Local Qwen · qwen"); s.failSave = true; await save(page);
  expect(s.writes).toHaveLength(1); await expect(page.getByRole("button", { name: "Workflow model selection" })).toContainText("Manual");
  expect(s.writes[0].body.action_config.model_selection).toEqual({ mode: "manual", provider_id: local });
});
test("Unknown saved selection is preserved, then explicitly removable as Legacy", async ({ page }) => {
  const s = await fixture(page); const selection = { mode: "auto", objective: "future", extra: 123 }; s.workflow.action_config.model_selection = selection; await edit(page);
  await expect(page.getByRole("button", { name: "Workflow model selection" })).toContainText("Saved configuration"); await save(page);
  expect(s.writes[0].body.action_config.model_selection).toEqual(selection);
  await choose(page, "Workflow model selection", "Legacy — existing workflow behavior"); await save(page);
  expect(s.writes[1].body.action_config).not.toHaveProperty("model_selection");
});
test("Another owner's workflow never previews the editor's personal default or offers their BYOK", async ({ page }) => {
  const s = await fixture(page); s.workflow.owner_id = "another-owner"; s.workflow.action_config.model_selection = { mode: "auto" }; await edit(page);
  await expect(page.getByText(/owner's personal catalog is unavailable here/)).toBeVisible();
  await expect(page.getByText("Policy: My judge", { exact: true })).toHaveCount(0);
  await page.getByRole("button", { name: "Workflow routing policy" }).click();
  await expect(page.getByRole("option", { name: "My judge · Personal" })).toHaveCount(0); await page.keyboard.press("Escape");
  await choose(page, "Workflow model selection", "Manual — choose a model");
  await page.getByRole("button", { name: "Workflow model", exact: true }).click();
  await expect(page.getByRole("option", { name: "Cloud model · cloud" })).toHaveCount(0);
  await page.getByRole("option", { name: "Local Qwen · qwen" }).click(); await save(page);
  expect(s.writes[0].body.action_config.model_selection).toEqual({ mode: "manual", provider_id: local });
});
test("Missing policies block a changed Auto selection and recover without losing its objective", async ({ page }) => {
  const s = await fixture(page); s.policyError = true; await edit(page);
  await choose(page, "Workflow model selection", "Auto — routing policy");
  await choose(page, "Workflow routing objective", "Economy — prefer lower cost");
  await expect(page.getByRole("button", { name: "Save", exact: true })).toBeDisabled();
  s.policyError = false; await page.getByRole("button", { name: "Retry workflow routing settings" }).click(); await save(page);
  expect(s.writes[0].body.action_config.model_selection).toEqual({ mode: "auto", objective: "economy" });
});
