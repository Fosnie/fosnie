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

import { fmtDuration, summaryHasContent } from "./TurnSummary";

describe("summaryHasContent", () => {
  it("is false for a turn that only talked", () => {
    expect(summaryHasContent(null)).toBe(false);
    expect(summaryHasContent({ steps: [{ title: "a", status: "done" }], tools: ["retrieve"] })).toBe(false);
  });

  it("is true once a file changed or a command ran", () => {
    expect(summaryHasContent({ files: [{ path: "a.md", op: "write" }] })).toBe(true);
    expect(summaryHasContent({ commands: [{ command: "ls", duration_ms: 5, stdout_tail: "" }] })).toBe(true);
  });
});

describe("fmtDuration", () => {
  it("reads sub-second in ms", () => {
    expect(fmtDuration(820)).toBe("820ms");
  });
  it("reads seconds with one decimal below ten", () => {
    expect(fmtDuration(3400)).toBe("3.4s");
    expect(fmtDuration(42000)).toBe("42s");
  });
  it("reads minutes and seconds past a minute", () => {
    expect(fmtDuration(65000)).toBe("1m 05s");
  });
});
