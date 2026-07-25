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

import { describe, expect, it } from "vitest";

import { applyResolved, type PendingApproval } from "./approvalState";

const pending: PendingApproval = { runId: "run-1", tool: "desktop.terminal_run", summary: "Run it?", state: "pending" };

describe("applyResolved", () => {
  it("settles a matching run to approved", () => {
    expect(applyResolved(pending, "run-1", true)?.state).toBe("approved");
  });

  it("settles a matching run to closed when not approved", () => {
    // A reject, a timeout and an auto-decline all arrive the same way.
    expect(applyResolved(pending, "run-1", false)?.state).toBe("closed");
  });

  it("leaves a different run's card alone", () => {
    expect(applyResolved(pending, "run-2", true)).toBe(pending);
  });

  it("does nothing when there is no card", () => {
    expect(applyResolved(null, "run-1", true)).toBeNull();
  });
});
