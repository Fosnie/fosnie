import { defineConfig } from '@playwright/test';
export default defineConfig({ testDir: './tests/local-routing-demo', testMatch: 'recovery.spec.ts',
  workers: 1, retries: 0, timeout: 120000,
  outputDir: process.env.FOSNIE_DEMO_TEST_OUTPUT,
  use: { baseURL: 'http://127.0.0.1:5190', headless: true, channel: 'msedge' } });
