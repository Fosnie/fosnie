import { test, expect } from "@playwright/test";
import { writeFileSync } from "node:fs";
test("Auto answers with an installed local model and persists its choice", async ({ page, request }) => {
  const info = await (await request.get("/demo/info")).json();
  await page.goto("/tests/local-routing-demo/index.html");
  await expect(page.getByLabel("Chat model", {exact:true})).toBeEnabled();
  await page.getByLabel("Chat model", {exact:true}).selectOption("auto");
  await page.getByLabel("Routing objective").selectOption("economy");
  await page.locator("textarea.comp-in").fill("Reply with only the word hello.");
  await page.locator("textarea.comp-in").press("Enter");
  await expect(page.getByLabel("Actual model")).toBeVisible({timeout:90000});
  await expect(page.getByRole("paragraph").filter({hasText:/hello/i})).toBeVisible({timeout:90000});
  const model = await page.getByLabel("Actual model").textContent();
  expect(model).toMatch(/huihui_ai\/qwen3\.5|Llama-3\.2-3B/);
  await page.reload();
  await expect(page.getByLabel("Chat model", {exact:true})).toHaveValue("auto");
  await expect(page.getByLabel("Actual model")).toHaveText(model!);
  writeFileSync(process.env.FOSNIE_DEMO_RECEIPT!, JSON.stringify({source:"live_local_smoke",selection:"auto",actual_badge:model,
    cloud_models_connected:info.cloud_models_connected,quality_evidence:false,p12_eligible:false},null,2));
  await page.screenshot({path:process.env.FOSNIE_DEMO_SCREENSHOT!,fullPage:true});
});
