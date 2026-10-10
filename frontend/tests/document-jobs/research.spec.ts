import { test, expect, type Page } from "@playwright/test";
const url = "/tests/document-jobs/research.html";
const customTemplate = "41e7d953-7c41-4c20-a9ed-921561672450";
async function fixture(page: Page) {
  const state = {
    enabled: true, webEnabled: true, customTemplates: true, clarification: false, quality: false, capsDenied: false, denied: false, launchFails: false,
    launches: [] as any[], answers: [] as any[], answerFails: false, legacy: [] as string[], cancellations: 0,
    history: false, idempotent: false, historyDenied: false, receiptStatus: "succeeded", receiptDenied: false,
    job: { job_id: "research-one", chat_id: "chat-one", status: "queued", message_id: null as string | null,
      selection: { mode: "default" }, decision_id: "decision-one", reason_codes: [] as string[],
      usage: { calls: 0, unknown_cost_calls: 0, exhausted: false }, accounting: "conservative_snapshot",
      deadline: new Date(Date.now() + 600000).toISOString(), clarification: null as any,
      options: { verify: false, deepen: false }, verification: null as any },
  };
  await page.route("**/api/**", async route => {
    const req = route.request(); const path = new URL(req.url()).pathname;
    if (!path.startsWith("/api/")) return route.continue();
    const json = (body: unknown, status = 200) => route.fulfill({ status, json: body });
    if (path === "/api/auth/config") return json({ mode: "local", local_enabled: true });
    if (path === "/api/routing/research-jobs") return state.capsDenied ? json({}, 503) : json({ schema_version: 1, enabled: state.enabled,
      selection_modes: ["default", "manual", "auto"], sources: ["files", "web", "hybrid"], web_enabled: state.webEnabled,
      custom_templates: state.customTemplates, maximum_template_sections: 8, clarification: state.clarification,
      history: state.history, idempotent_launch: state.idempotent,
      maximum_web_pages: 4, templates: ["exploration", "formal", "literature", "freeform"], maximum_documents: 4,
      verification: state.quality, ...(state.quality ? { deepening: "existing_sources_v1" } : {}) });
    if (path === "/api/research/templates") return json({ builtin: [{ id: "exploration", label: "Exploration", description: "Explore the sources", structure: [], outline_mode: "free" }],
      custom: [{ id: customTemplate, label: "My custom template", description: "Custom", structure: [], outline_mode: "free" }] });
    if (path === "/api/research-jobs/scope") return json([{ id: "library-one", name: "Contracts", kind: "project", doc_count: 1 },
      { id: "large-library", name: "Large archive", kind: "library", doc_count: 5 }]);
    if (path === "/api/routing/catalog") return json([
      { id: "local-one", label: "Local Qwen", model: "qwen", enabled: true, provider_revision: 1, profile_revision: 1,
        profile: { provider_id: "local-one", provider_revision: 1, revision: 1, model: "qwen" } },
      { id: "stale", label: "Outdated model", model: "old", enabled: true, provider_revision: 2, profile_revision: 1,
        profile: { provider_id: "stale", provider_revision: 1, revision: 1, model: "old" } },
    ]);
    if (path.startsWith("/api/research-launches/")) return state.receiptDenied ? json({}, 403) : json({
      request_id: path.split("/").at(-1), status: state.receiptStatus,
      ...(state.receiptStatus === "succeeded" ? { job_id: "research-one", chat_id: "chat-one", status_url: "/api/research-jobs/research-one" } : {}) });
    if (path === "/api/research-jobs" && req.method() === "GET") {
      if (state.historyDenied) return json({}, 403);
      if (new URL(req.url()).searchParams.has("before")) return json({ items: [
        { job_id: "older-one", chat_id: "chat-one", title: "Older report", status: "succeeded", accessible: true, created_at: new Date().toISOString() }
      ], next_cursor: null });
      return json({ items: [{ job_id: "research-one", status: state.job.status, accessible: false, created_at: new Date().toISOString() }], next_cursor: "research-one" });
    }
    if (path === "/api/research-jobs" && req.method() === "POST") {
      state.launches.push(req.postDataJSON());
      if (state.launchFails) return route.abort();
      state.job.selection = req.postDataJSON().selection;
      state.job.options = { verify: !!req.postDataJSON().request.verify, deepen: !!req.postDataJSON().request.deepen };
      return json({ job_id: "research-one", chat_id: "chat-one", status_url: "/api/research-jobs/research-one" }, 202);
    }
    if (path === "/api/research-jobs/research-one/cancel") {
      state.cancellations++; state.job.status = "cancelled"; return json({ job_id: "research-one", status: "cancelled" });
    }
    if (path === "/api/research-jobs/research-one/answers") {
      state.answers.push(req.postDataJSON());
      if (state.answerFails) return route.abort();
      state.job.status = "queued"; state.job.clarification = null; return json({ job_id: "research-one", accepted: true });
    }
    if (path === "/api/research-jobs/research-one") return state.denied ? json({}, 403) : json(state.job);
    if (path === "/api/chats") return json([{ id: "chat-one", title: "Research question", created_at: new Date().toISOString(),
      research_params: { routed_job_id: "research-one", source: "files", question: "Compare clauses", template: "exploration", kb_ids: ["library-one"], refinements: [] } }]);
    if (path === "/api/chats/chat-one/messages") return json(state.job.status === "succeeded" ? [{ id: "message-one", role: "assistant", content: "Completed research report" }] : []);
    if (path === "/api/chats/chat-one/artefacts") return json(state.job.status === "succeeded" ? [{ id: "report-one" }] : []);
    if (path === "/api/research/prepare") {
      state.legacy.push(path); return json({ questions: [], scope_summary: "Legacy research", scope: [{ kb_id: "library-one", name: "Contracts", doc_count: 1, kind: "library" }] });
    }
    if (path === "/api/research/start") { state.legacy.push(path); return json({ chat_id: "legacy-one", run_id: "agent-one" }); }
    return json({}, 404);
  });
  return state;
}
async function files(page: Page) {
  await page.goto(url);
  await page.getByRole("button", { name: "Research my files", exact: true }).click();
  await page.getByRole("textbox").fill("Compare clauses");
}
test("Research launch receipt survives lost response and reload with launch gate off", async ({ page }) => {
  const s = await fixture(page); s.idempotent = true; s.history = true; s.launchFails = true;
  await files(page); await page.getByRole("checkbox", { name: "Contracts" }).check();
  await page.getByRole("button", { name: "Start file research", exact: true }).click();
  await expect(page.getByRole("button", { name: "Open existing research" })).toBeVisible();
  expect(s.launches).toHaveLength(1);
  expect(s.launches[0].request_id).toMatch(/^[0-9a-f-]{36}$/);
  await expect(page).toHaveURL(new RegExp(`research_request=${s.launches[0].request_id}`));
  await expect(page.getByRole("button", { name: "Start file research", exact: true })).toBeDisabled();
  s.enabled = false; await page.reload();
  await expect(page.getByRole("button", { name: "Open existing research" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Review scope" })).toHaveCount(0);
  s.receiptDenied = true; await page.getByRole("button", { name: "Check saved launch" }).click();
  await expect(page.getByText(/Could not look up this launch/)).toBeVisible();
  await expect(page.getByRole("button", { name: "Open existing research" })).toHaveCount(0);
  s.receiptDenied = false; await page.getByRole("button", { name: "Check saved launch" }).click();
  await page.getByRole("button", { name: "Open existing research" }).click();
  await expect(page).toHaveURL(/#\/c\/chat-one/);
  expect(s.launches).toHaveLength(1); expect(s.legacy).toEqual([]);
});

test("Research planning receipt blocks restart until explicit separate request", async ({ page }) => {
  const s = await fixture(page); s.idempotent = true; s.launchFails = true; s.receiptStatus = "planning";
  await files(page); await page.getByRole("checkbox", { name: "Contracts" }).check();
  await page.getByRole("button", { name: "Start file research", exact: true }).click();
  await expect(page.getByText(/Model planning is still pending/)).toBeVisible();
  await expect(page.getByRole("button", { name: "Prepare a separate request" })).toHaveCount(0);
  const firstKey = s.launches[0].request_id;
  s.receiptStatus = "interrupted"; await page.getByRole("button", { name: "Check saved launch" }).click();
  await expect(page.getByText(/Some planning work may already have been charged/)).toBeVisible();
  await page.getByRole("button", { name: "Prepare a separate request" }).click();
  expect(s.launches).toHaveLength(1);
  s.launchFails = false;
  await page.getByRole("button", { name: "Start file research", exact: true }).click();
  await expect(page).toHaveURL(/#\/c\/chat-one/);
  expect(s.launches).toHaveLength(2); expect(s.launches[1].request_id).not.toBe(firstKey);
  expect(s.legacy).toEqual([]);
});

test("Research history pages and cancels redacted jobs with launch disabled", async ({ page }) => {
  const s = await fixture(page); s.history = true; s.enabled = false; await page.goto(url);
  const history = page.getByRole("region", { name: "Research job history" });
  await expect(history.getByText(/Research details unavailable/)).toBeVisible();
  await expect(history.getByRole("button", { name: "Open research" })).toHaveCount(0);
  await history.getByRole("button", { name: "Cancel job" }).click();
  await expect(history.getByText(/Research details unavailable · cancelled/)).toBeVisible();
  expect(s.cancellations).toBe(1);
  await history.getByRole("button", { name: "Older jobs" }).click();
  await expect(history.getByText(/Older report/)).toBeVisible();
  await expect(history.getByRole("button", { name: "Open research" })).toBeVisible();
  await history.getByRole("button", { name: "Newer jobs" }).click();
  await expect(history.getByText(/Research details unavailable/)).toBeVisible();
  s.historyDenied = true; await history.getByRole("button", { name: "Refresh research history" }).click();
  await expect(history.getByText(/Could not load research history/)).toBeVisible();
  await expect(history.getByText(/Research details unavailable/)).toHaveCount(0);
  expect(s.launches).toEqual([]); expect(s.legacy).toEqual([]);
});

test("Research Manual launch bypasses triage, reload restores status and cancellation", async ({ page }) => {
  const s = await fixture(page); await files(page);
  await expect(page.getByRole("button", { name: "Start file research", exact: true })).toBeDisabled();
  await page.getByRole("checkbox", { name: "Contracts" }).check();
  await page.getByRole("button", { name: "Research model selection", exact: true }).click();
  await page.getByRole("option", { name: "Manual — choose a model" }).click();
  await expect(page.getByRole("button", { name: "Start file research", exact: true })).toBeDisabled();
  await page.getByRole("button", { name: "Research model", exact: true }).click();
  await expect(page.getByRole("option", { name: "Outdated model" })).toHaveCount(0);
  await page.getByRole("option", { name: "Local Qwen · qwen" }).click();
  await page.getByRole("button", { name: "Start file research", exact: true }).dblclick();
  await expect(page).toHaveURL(/#\/c\/chat-one/);
  expect(s.launches).toEqual([{ request: { question: "Compare clauses", source: "files", kb_ids: ["library-one"], template: "exploration", refinements: [] }, selection: { mode: "manual", provider_id: "local-one" } }]);
  expect(s.legacy).toEqual([]);
  await page.reload(); await expect(page.getByText("Research: queued", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Cancel research", exact: true }).click();
  await expect(page.getByText("Research: cancelled", { exact: true })).toBeVisible(); expect(s.cancellations).toBe(1);
});
test("Research quality controls persist intent and show flagged evidence after completion", async ({ page }) => {
  const s = await fixture(page); s.quality = true; await files(page);
  await page.getByRole("checkbox", { name: "Contracts" }).check();
  await page.getByRole("checkbox", { name: "Check report against source evidence" }).check();
  await page.getByRole("checkbox", { name: "Re-examine sources for each section" }).check();
  await expect(page.getByText(/It does not search for more sources/)).toBeVisible();
  await page.getByRole("button", { name: "Start file research", exact: true }).click();
  await expect(page.getByText("Source grounding check required before publication.")).toBeVisible();
  expect(s.launches[0].request).toMatchObject({ verify: true, deepen: true });
  s.job.status = "succeeded"; s.job.message_id = "message-one";
  s.job.verification = { method: "source_grounding_v1", model: "verifier", score: 0.7, total: 5, flagged: 2 };
  await expect(page.getByText(/Source grounding checked: 2 flagged passages/)).toBeVisible();
  await page.reload();
  await expect(page.getByText(/Source grounding checked: 2 flagged passages/)).toBeVisible();
  await expect(page.getByText("Completed research report", { exact: true })).toBeVisible();
  expect(s.launches).toHaveLength(1); expect(s.legacy).toEqual([]);
});

test("Research Auto explains shadow and refreshes completed report without reload", async ({ page }) => {
  const s = await fixture(page); await files(page);
  await page.getByRole("checkbox", { name: "Contracts" }).check();
  await page.getByRole("button", { name: "Research model selection", exact: true }).click();
  await page.getByRole("option", { name: "Auto — routing policy" }).click();
  await expect(page.getByText(/Question and document contents are not assessed/)).toBeVisible();
  await page.getByRole("button", { name: "Start file research", exact: true }).click();
  await expect(page.getByText("Research: queued", { exact: true })).toBeVisible();
  expect(s.launches[0].selection).toEqual({ mode: "auto", objective: "balanced" });
  s.job.status = "succeeded"; s.job.message_id = "message-one"; s.job.reason_codes = ["shadow_default_planned"];
  await expect(page.getByText("Completed research report", { exact: true })).toBeVisible();
  await expect(page.getByText("Saved reports: 1", { exact: true })).toBeVisible();
  await expect(page.getByText(/Shadow mode:/)).toBeVisible();
  await expect(page.getByRole("button", { name: "Cancel research" })).toHaveCount(0);
});
test("Research uncertainty sends one Default request, never falls back, rejects custom templates", async ({ page }) => {
  const s = await fixture(page); s.launchFails = true; s.customTemplates = false; await files(page);
  await page.getByRole("checkbox", { name: "Contracts" }).check();
  await page.getByRole("checkbox", { name: "Large archive" }).check();
  await expect(page.getByRole("button", { name: "Start file research", exact: true })).toBeDisabled();
  await page.getByRole("checkbox", { name: "Large archive" }).uncheck();
  await page.getByRole("button", { name: "Report template", exact: true }).click();
  await page.getByRole("option", { name: "My custom template" }).click();
  await expect(page.getByRole("button", { name: "Start file research", exact: true })).toBeDisabled();
  await expect(page.getByText(/Choose a supported built-in/)).toBeVisible();
  await page.getByRole("button", { name: "Report template", exact: true }).click();
  await page.getByRole("option", { name: "Exploration", exact: true }).click();
  await page.getByRole("button", { name: "Start file research", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText("No automatic retry was sent");
  expect(s.launches).toHaveLength(1); expect(s.launches[0].selection).toEqual({ mode: "default" }); expect(s.legacy).toEqual([]);
});
test("Research settings error blocks legacy; explicit gate off keeps legacy files and web", async ({ page }) => {
  const s = await fixture(page); s.capsDenied = true; await files(page);
  await expect(page.getByRole("button", { name: "Start file research", exact: true })).toBeDisabled();
  await expect(page.getByRole("button", { name: "Review scope" })).toHaveCount(0);
  s.capsDenied = false; s.enabled = false;
  await page.getByRole("button", { name: "Retry routing settings" }).click();
  await page.getByRole("button", { name: "Review scope" }).click();
  await expect(page.getByText("Legacy research", { exact: true })).toBeVisible();
  expect(s.legacy).toEqual(["/api/research/prepare"]);
  await page.getByRole("button", { name: "Research the web", exact: true }).click();
  await expect(page.getByText(/Per-job model selection is disabled/)).toBeVisible();
  await expect(page.getByRole("button", { name: "Research model selection", exact: true })).toHaveCount(0);
  expect(s.launches).toHaveLength(0);
});
test("Research gate revocation does not switch to legacy; access loss retains cancellation", async ({ page }) => {
  const s = await fixture(page); await files(page);
  await page.getByRole("checkbox", { name: "Contracts" }).check();
  await expect(page.getByRole("button", { name: "Start file research", exact: true })).toBeEnabled();
  s.enabled = false;
  // Trigger the normal refetch-on-focus instead of waiting for a 15-second poll.
  await page.evaluate(() => window.dispatchEvent(new Event("visibilitychange")));
  await expect(page.getByRole("button", { name: "Start file research", exact: true })).toBeDisabled({ timeout: 20000 });
  await expect(page.getByRole("button", { name: "Review scope" })).toHaveCount(0);
  s.denied = true; await page.goto(`${url}#/c/chat-one`);
  await expect(page.getByText(/Could not access this research job/)).toBeVisible();
  await page.getByRole("button", { name: "Cancel research", exact: true }).click();
  await expect(page.getByText("Research cancellation confirmed.", { exact: true })).toBeVisible();
  expect(s.cancellations).toBe(1); expect(s.launches).toHaveLength(0); expect(s.legacy).toEqual([]);
});

test("Web and hybrid use routed selection and enforce the independent egress gate", async ({ page }) => {
  const s = await fixture(page); s.webEnabled = false; await page.goto(url);
  await page.getByRole("textbox").fill("Compare public evidence");
  await expect(page.getByRole("button", { name: "Start web research", exact: true })).toBeDisabled();
  await expect(page.getByText(/requires an enabled web-search connector/)).toBeVisible();
  s.webEnabled = true; await page.reload();
  await page.getByRole("textbox").fill("Compare public evidence");
  await expect(page.getByRole("checkbox")).toHaveCount(0);
  await page.getByRole("button", { name: "Start web research", exact: true }).click();
  await expect(page).toHaveURL(/#\/c\/chat-one/);
  expect(s.launches[0].request).toEqual({ question: "Compare public evidence", source: "web", template: "exploration", kb_ids: [], refinements: [] });
  await page.goto(url);
  await page.getByRole("button", { name: "Files + web", exact: true }).click();
  await page.getByRole("textbox").fill("Compare with public evidence");
  await expect(page.getByRole("button", { name: "Start hybrid research", exact: true })).toBeDisabled();
  await page.getByRole("checkbox", { name: "Contracts" }).check();
  await page.getByRole("button", { name: "Start hybrid research", exact: true }).click();
  await expect(page).toHaveURL(/#\/c\/chat-one/);
  expect(s.launches[1].request.source).toBe("hybrid");
  expect(s.launches[1].request.kb_ids).toEqual(["library-one"]); expect(s.legacy).toEqual([]);
});

test("Custom research template is launched by identity with explicit limits", async ({ page }) => {
  const s = await fixture(page); await files(page);
  await page.getByRole("checkbox", { name: "Contracts" }).check();
  await page.getByRole("button", { name: "Report template", exact: true }).click();
  await page.getByRole("option", { name: "My custom template" }).click();
  await expect(page.getByText(/Editing a template during its run stops that job/)).toBeVisible();
  await page.getByRole("button", { name: "Start file research", exact: true }).click();
  await expect(page).toHaveURL(/#\/c\/chat-one/);
  expect(s.launches).toHaveLength(1); expect(s.launches[0].request.template).toBe(customTemplate);
  expect(s.launches[0].request.template_spec).toBeUndefined(); expect(s.legacy).toEqual([]);
});


test("Clarification survives reload and answers resume the same job once", async ({ page }) => {
  const s = await fixture(page); s.clarification = true; await files(page);
  await page.getByRole("checkbox", { name: "Contracts" }).check();
  await page.getByRole("checkbox", { name: "Ask clarifying questions before research" }).check();
  await page.getByRole("button", { name: "Start file research", exact: true }).click();
  await expect(page).toHaveURL(/#\/c\/chat-one/);
  expect(s.launches[0].clarify).toBe(true);
  s.job.status = "awaiting_input"; s.job.usage.calls = 1;
  s.job.clarification = { id: "clarify-one", questions: [{ prompt: "Which timeframe?", options: ["This year", "Last year"] },
    { prompt: "Who is the audience?", options: [] }] };
  await page.reload();
  await expect(page.getByRole("form", { name: "Research clarification" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Continue research", exact: true })).toBeDisabled();
  await page.getByRole("button", { name: "Last year", exact: true }).click();
  await page.getByRole("textbox", { name: "Who is the audience?" }).fill("Engineering team");
  await page.getByRole("button", { name: "Continue research", exact: true }).click();
  await expect(page.getByText("Research: queued", { exact: true })).toBeVisible();
  expect(s.answers).toEqual([{ clarification_id: "clarify-one", answers: ["Last year", "Engineering team"] }]);
  expect(s.launches).toHaveLength(1); expect(s.legacy).toEqual([]);
  await expect(page.getByRole("form", { name: "Research clarification" })).toHaveCount(0);
});

test("Uncertain clarification submission keeps the checkpoint and expiry blocks continuation", async ({ page }) => {
  const s = await fixture(page); s.answerFails = true;
  s.job.status = "awaiting_input";
  s.job.clarification = { id: "clarify-one", questions: [{ prompt: "Which timeframe?", options: ["Last year"] }] };
  await page.goto(`${url}#/c/chat-one`);
  await page.getByRole("textbox", { name: "Which timeframe?" }).fill("é".repeat(129));
  await expect(page.getByRole("button", { name: "Continue research", exact: true })).toBeDisabled();
  await page.getByRole("button", { name: "Last year", exact: true }).click();
  await page.getByRole("button", { name: "Continue research", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText("Sending the same answers again is safe");
  expect(s.answers).toHaveLength(1); expect(s.launches).toHaveLength(0);
  s.job.deadline = new Date(Date.now() - 1000).toISOString(); await page.reload();
  await expect(page.getByRole("alert")).toContainText("deadline has passed");
  await expect(page.getByRole("button", { name: "Continue research", exact: true })).toBeDisabled();
  await page.getByRole("button", { name: "Cancel research", exact: true }).click();
  await expect(page.getByText("Research: cancelled", { exact: true })).toBeVisible();
});
