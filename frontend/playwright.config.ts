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

// Visual-regression harness for the UI refresh (05-FRONTEND.md §10).
// Captures full-page screenshots of the key surfaces so each migrated phase can be
// reviewed against an intentional baseline (re-baseline with `--update-snapshots`).
//
// Auth: most routes sit behind Keycloak. Run the app, sign in once, and save the
// session with `npx playwright codegen --save-storage=auth.json <BASE_URL>` (or any
// equivalent), then point PW_STORAGE_STATE at it. Without it, only the login screen
// renders. BASE_URL defaults to the Vite preview/dev server.
import { defineConfig, devices } from "@playwright/test";

const BASE_URL = process.env.PW_BASE_URL ?? "http://localhost:5173";
const storageState = process.env.PW_STORAGE_STATE || undefined;

export default defineConfig({
  testDir: "./tests/visual",
  snapshotDir: "./tests/visual/__screenshots__",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  reporter: "list",
  use: {
    baseURL: BASE_URL,
    storageState,
    // Pin a deterministic viewport + disable animations so diffs are stable.
    viewport: { width: 1440, height: 900 },
    deviceScaleFactor: 1,
  },
  // Stability: ignore sub-pixel AA noise; fail on real visual drift.
  expect: { toHaveScreenshot: { maxDiffPixelRatio: 0.01, animations: "disabled" } },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
});
