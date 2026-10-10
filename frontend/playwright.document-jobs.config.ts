import { defineConfig } from "@playwright/test";
export default defineConfig({
  testDir: "./tests/document-jobs", testMatch: "*.spec.ts", workers: 1,
  use: { baseURL: "http://127.0.0.1:5187", headless: true, channel: process.env.PW_CHANNEL || "msedge" },
  webServer: { command: "npm run dev -- --host 127.0.0.1 --port 5187 --strictPort", url: "http://127.0.0.1:5187", reuseExistingServer: false },
});
