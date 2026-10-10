import { test, expect } from "@playwright/test";
test("Installed local model answers through the actual product and settings", async ({ page, request }) => {
  const info = await (await request.get("/demo/info")).json();
  expect(info.cloud_models_connected).toBe(false);
  await page.goto("/tests/local-routing-demo/index.html");
  await expect(page.getByRole("button", {name:"Local routing demo", exact:true})).toBeVisible();
  await page.getByLabel("Chat model", {exact:true}).selectOption(info.providers.qwen);
  await page.locator("textarea.comp-in").fill("Ответь одним числом: сколько будет 7 × 8?");
  await page.locator("textarea.comp-in").press("Enter");
  await expect(page.getByLabel("Actual model")).toContainText("huihui_ai/qwen3.5-abliterated", {timeout:90000});
  await expect(page.getByRole("paragraph").filter({hasText:/56/})).toBeVisible({timeout:90000});
  await page.reload();
  await expect(page.getByLabel("Actual model")).toContainText("huihui_ai/qwen3.5-abliterated");
  await page.getByRole("link", {name:"Модели и маршрутизация"}).click();
  await expect(page.getByText("Qwen 4B · local", {exact:true})).toBeVisible();
  await expect(page.getByText("Llama 3B · local", {exact:true})).toBeVisible();
  await page.screenshot({path:process.env.FOSNIE_DEMO_SCREENSHOT!,fullPage:true});
});
