import { defineConfig } from "@playwright/test";
export default defineConfig({
  testDir: "./tests/routing-stack", workers: 1, retries: 0, timeout: 30_000,
  use: { baseURL: "http://127.0.0.1:5189", headless: true,
    channel: process.env.PW_CHANNEL || (process.platform === "win32" ? "msedge" : undefined) },
  webServer: { command: "node node_modules/vite/bin/vite.js --config vite.routing-stack.config.ts --host 127.0.0.1 --port 5189 --strictPort",
    url: "http://127.0.0.1:5189", reuseExistingServer: false },
});
