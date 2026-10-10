import { test, expect, type Page } from "@playwright/test";
const url = "/tests/document-jobs/index.html";
async function fixture(page: Page) {
  const state = {
    enabled: true, denied: false, launchFails: false, reportDenied: false,
    launches: [] as unknown[], repairs: 0, cancellations: 0,
    job: { id: "job-one", document_id: "document-one", status: "queued", phase: "verify", repair: false,
      selection: { mode: "default" }, decision_id: "decision-one", reason_codes: [] as string[],
      planned_document_target: { model: "Local model", effort: null },
      usage: { calls: 0, unknown_cost_calls: 0 }, result_id: null as string | null, updated_at: "2026-09-22T12:00:00Z" },
  };
  await page.route("**/api/**", async route => {
    const request = route.request(); const path = new URL(request.url()).pathname;
    // Vite also serves source modules under /src/api/: do not mock those.
    if (!path.startsWith("/api/")) return route.continue();
    const json = (body: unknown, status = 200) => route.fulfill({ status, json: body });
    if (path === "/api/auth/config") return json({ mode: "local", local_enabled: true });
    if (path === "/api/routing/document-jobs") return json({ schema_version: 1, enabled: state.enabled,
      delayed_repair: true, selection_modes: ["default", "manual", "auto"], target_types: ["document", "draft"] });
    if (path === "/api/routing/catalog") return json([
      { id: "local-one", label: "Local Qwen", model: "qwen", enabled: true, provider_revision: 1, profile_revision: 1,
        profile: { provider_id: "local-one", provider_revision: 1, revision: 1, model: "qwen" } },
      { id: "stale", label: "Outdated model", enabled: true, provider_revision: 2, profile_revision: 1,
        profile: { provider_id: "stale", provider_revision: 1, revision: 1, model: "old" }, model: "old" },
    ]);
    if (["/api/document-verification-jobs", "/api/artefact-verification-jobs"].includes(path) && request.method() === "POST") {
      state.launches.push(request.postDataJSON());
      if (state.launchFails) return route.abort();
      state.job.selection = request.postDataJSON().selection;
      if (path.includes("artefact")) Object.assign(state.job, { document_id: null, target_type: "draft", target_id: "document-one" });
      return json({ job_id: state.job.id }, 202);
    }
    if (path.endsWith("/cancel")) { state.cancellations++; state.job.status = "cancelled"; return json({ status: "cancelled" }); }
    if (path.endsWith("/repair")) { state.repairs++; state.job.phase = "repair"; state.job.repair = true; state.job.status = "queued"; return json({ job_id: "job-one" }, 202); }
    if (path === "/api/document-verification-jobs/job-one") return state.denied ? json({ error: "forbidden" }, 403) : json(state.job);
    if (path === "/api/verification-runs/job-one") return state.reportDenied ? json({}, 403) : json({
      id: "job-one", status: "succeeded", faithfulness_score: null, total_claims: 0, supported: 0, contradicted: 0, not_mentioned: 0, claims: [],
    });
    return json({}, 404);
  });
  return state;
}
test("manual selection, one launch, persisted URL and cancellation", async ({ page }) => {
  const s = await fixture(page); await page.goto(url);
  await page.getByRole("button", { name: "Verification model selection", exact: true }).click();
  await page.getByRole("option", { name: "Manual — choose a model" }).click();
  await expect(page.getByRole("button", { name: "Verify draft", exact: true })).toBeDisabled();
  await page.getByRole("button", { name: "Verification model", exact: true }).click();
  await expect(page.getByRole("option", { name: "Outdated model" })).toHaveCount(0);
  await page.getByRole("option", { name: "Local Qwen · qwen" }).click();
  await page.getByRole("button", { name: "Verify draft", exact: true }).dblclick();
  await expect(page).toHaveURL(/verificationJob=job-one/);
  expect(s.launches).toEqual([{ document_id: "document-one", selection: { mode: "manual", provider_id: "local-one" }, repair: false }]);
  await page.reload(); await expect(page.getByText("Verification: queued", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Cancel job" }).click();
  await expect(page.getByText("Verification: cancelled", { exact: true })).toBeVisible(); expect(s.cancellations).toBe(1);
});
test("Auto shadow explanation, report and delayed repair without launch", async ({ page }) => {
  const s = await fixture(page); await page.goto(url);
  await page.getByRole("button", { name: "Verification model selection", exact: true }).click();
  await page.getByRole("option", { name: "Auto — routing policy" }).click();
  await expect(page.getByText(/Document contents are not assessed/)).toBeVisible();
  await page.getByRole("button", { name: "Verify draft", exact: true }).click();
  await expect(page).toHaveURL(/verificationJob=job-one/);
  expect(s.launches[0]).toEqual({ document_id: "document-one", selection: { mode: "auto", objective: "balanced" }, repair: false });
  s.job.status = "succeeded"; s.job.result_id = "job-one"; s.job.reason_codes = ["shadow_default_planned"];
  await page.reload();
  await expect(page.getByText(/Shadow mode:/)).toBeVisible(); await expect(page.getByText("Not scored", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Propose tracked repairs", exact: true }).click();
  await expect(page.getByText("Repair: queued", { exact: true })).toBeVisible();
  expect(s.repairs).toBe(1); expect(s.launches).toHaveLength(1);
  s.job.status = "failed"; await page.reload();
  await expect(page.getByText("Repair: failed", { exact: true })).toBeVisible();
  await expect(page.getByText("Not scored", { exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Propose tracked repairs", exact: true })).toHaveCount(0);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.screenshot({ path: "test-results/document-job-mobile.png", fullPage: true });
});
test("gate and access revocation retain cancellation, hide cached report", async ({ page }) => {
  const s = await fixture(page); s.enabled = false; s.denied = true;
  await page.goto(`${url}?verificationJob=job-one`);
  await expect(page.getByRole("button", { name: "Verify draft", exact: true })).toHaveCount(0);
  await expect(page.getByText(/Could not access this document job/)).toBeVisible();
  await page.getByRole("button", { name: "Cancel job" }).click(); await expect.poll(() => s.cancellations).toBe(1);
  expect(s.launches).toHaveLength(0);
  s.enabled = true; s.denied = false; s.reportDenied = true; s.job.status = "succeeded"; s.job.result_id = "job-one";
  await page.reload(); await expect(page.getByText(/Could not load this verification report/)).toBeVisible();
  await expect(page.getByText("Not scored", { exact: true })).toHaveCount(0);
});
test("uncertain launch is never automatically retried or sent to legacy API", async ({ page }) => {
  const s = await fixture(page); s.launchFails = true; await page.goto(url);
  await page.getByRole("button", { name: "Verify draft", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText("No automatic retry was sent");
  expect(s.launches).toHaveLength(1); await expect(page).not.toHaveURL(/verificationJob/);
});
test("artefact verification uses a draft target without document repair", async ({ page }) => {
  const s = await fixture(page); await page.goto(`${url}?target=draft`);
  await expect(page.getByRole("heading", { name: "Verify generated artefact" })).toBeVisible();
  await expect(page.getByRole("checkbox")).toHaveCount(0);
  await page.getByRole("button", { name: "Verify draft", exact: true }).click();
  await expect(page).toHaveURL(/verificationJob=job-one/);
  expect(s.launches).toEqual([{ artefact_id: "document-one", selection: { mode: "default" } }]);
  s.job.status = "succeeded"; s.job.result_id = "job-one";
  await page.reload(); await expect(page.getByText("Verification: succeeded", { exact: true })).toBeVisible();
  await expect(page.getByText("Not scored", { exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Propose tracked repairs", exact: true })).toHaveCount(0);
});
