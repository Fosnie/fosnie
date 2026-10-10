import { test, expect } from "@playwright/test";

test("Auto uses real Rust/ML/WS, persists actual metadata and survives policy Off/reenable", async ({ page, request }) => {
  const receive: unknown[] = [];
  page.on("websocket", socket => socket.on("framereceived", e => {
    try { receive.push(JSON.parse(String(e.payload))); } catch { /* Non-JSON is checked by product transport. */ }
  }));
  const send = async (content: string) => {
    const completed = page.waitForResponse(r => r.url().includes("/messages") && r.status() === 200);
    await page.locator("textarea.comp-in").fill(content); await page.locator("textarea.comp-in").press("Enter");
    await completed;
    await expect(page.getByTitle("Stop generating")).toHaveCount(0);
  };
  expect(process.env.FOSNIE_E2E_AGENT).toBeTruthy();
  await page.addInitScript(id => localStorage.setItem("pai.defaultAgentId", id!), process.env.FOSNIE_E2E_AGENT);
  await page.goto("/tests/routing-stack/chat.html");
  await expect(page.getByRole("button", { name: "Routing stack assistant", exact: true })).toBeVisible();
  await expect(page.getByLabel("Chat model", { exact: true })).toBeEnabled();
  await expect.poll(() => receive.some((f: any) => f.type === "hello")).toBe(true);
  await page.getByLabel("Chat model", { exact: true }).selectOption("auto");
  await page.getByLabel("Routing objective").selectOption("economy");
  await send("A simple synthetic routing task");
  await expect(page.getByLabel("Actual model")).toContainText("routing-e2e-local");
  await expect(page.getByRole("paragraph").filter({ hasText: /^Fosnie routing stack answer\.$/ })).toBeVisible();
  await page.reload();
  await expect(page.getByLabel("Chat model", { exact: true })).toHaveValue("auto");
  await expect(page.getByLabel("Actual model")).toContainText("routing-e2e-local");

  const policies = await (await request.get("/api/routing/policies")).json();
  const saved = policies.find((p: any) => p.is_default);
  const setPolicy = async (rollout: string, provider: string) => {
    const response = await request.put(`/api/routing/policies/${saved.id}`, { data: {
      label: saved.label, scope: saved.scope, expected_revision: saved.revision, is_default: true,
      policy: { ...saved.policy, rollout, default_provider_id: provider, constraints: { ...saved.policy.constraints, local_only: true } },
    } });
    expect(response.status()).toBe(200); saved.policy = await response.json(); saved.revision = saved.policy.revision;
  };
  const catalog = await (await request.get("/api/routing/catalog")).json();
  const local = catalog.find((p: any) => p.model === "routing-e2e-local").id;
  const cloud = catalog.find((p: any) => p.model === "routing-e2e-cloud").id;
  await setPolicy("off", cloud);
  const before = receive.filter((f: any) => f.type === "chat.error").length;
  await page.locator("textarea.comp-in").fill("Cloud default must be refused under local-only");
  await page.locator("textarea.comp-in").press("Enter");
  await expect.poll(() => receive.filter((f: any) => f.type === "chat.error").length).toBe(before + 1);
  await setPolicy("off", local);
  await send("Off uses the checked local default");
  await expect(page.getByLabel("Actual model")).toHaveCount(2);
  await setPolicy("active", local);
  await send("Re-enabled Auto remains local");
  await expect(page.getByLabel("Actual model")).toHaveCount(3);
  const gate = await request.put("/api/routing/browser-chat", { data: { enabled: false } });
  expect(gate.status()).toBe(200);
  await page.reload();
  await expect(page.getByLabel("Chat model", { exact: true })).toBeDisabled();
});
