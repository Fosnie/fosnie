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

// Key-surface screenshots for the UI-refresh visual-regression guard. Routes that
// need a signed-in session only render meaningfully when PW_STORAGE_STATE is set
// (see playwright.config.ts); they are skipped otherwise so the suite still runs
// against a fresh, unauthenticated app for the login surface.
import { test, expect } from "@playwright/test";

const AUTHED = !!process.env.PW_STORAGE_STATE;

// One entry per baseline. `auth: false` routes work without a session.
const ROUTES: { name: string; path: string; auth?: boolean }[] = [
  { name: "login", path: "/", auth: false },
  { name: "chat-home", path: "/" },
  { name: "studio-agents", path: "/studio/agents" },
  { name: "admin", path: "/admin" },
  { name: "profile-appearance", path: "/profile" },
  // Deep Research home renders at `/` in research mode; capture after switching
  // there in the app, or add a seeded run id here once fixtures exist.
];

for (const r of ROUTES) {
  test(`screenshot: ${r.name}`, async ({ page }) => {
    test.skip(r.auth !== false && !AUTHED, "needs PW_STORAGE_STATE (signed-in session)");
    await page.goto(r.path);
    await page.waitForLoadState("networkidle");
    await expect(page).toHaveScreenshot(`${r.name}.png`, { fullPage: true });
  });
}
