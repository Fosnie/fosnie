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

// The one decision that has to survive meeting an older instance: is this
// approval a folder action to be shown as a change, or a plain one to be shown as
// a sentence. Getting it wrong in the "older instance" direction would put an
// empty card in front of somebody instead of the question they can answer.

import { describe, expect, it } from "vitest";

import { asFolderDetail } from "@/shell/folderDetail";

describe("asFolderDetail", () => {
  it("recognises each kind of folder action", () => {
    for (const kind of ["diff", "command", "delete"] as const) {
      const d = asFolderDetail({ kind, path: "notes.md" });
      expect(d).not.toBeNull();
      expect(d!.kind).toBe(kind);
    }
  });

  it("carries the fields a card needs through unchanged", () => {
    const d = asFolderDetail({
      kind: "command",
      command: "npm test",
      cwd: "C:\\work",
      workspace_id: "ws-1",
      prefix: "npm",
    });
    expect(d).toEqual({
      kind: "command",
      command: "npm test",
      cwd: "C:\\work",
      workspace_id: "ws-1",
      prefix: "npm",
    });
  });

  it("falls back to a sentence when there is no detail (an older instance)", () => {
    // The whole point of the fallback: a client newer than the instance still
    // gets to ask the question, from the summary, rather than rendering nothing.
    expect(asFolderDetail(undefined)).toBeNull();
    expect(asFolderDetail(null)).toBeNull();
  });

  it("does not mistake another tool's approval for a folder one", () => {
    expect(asFolderDetail({ some: "mcp payload" })).toBeNull();
    expect(asFolderDetail({ kind: "something_else" })).toBeNull();
    expect(asFolderDetail("a string")).toBeNull();
    expect(asFolderDetail(42)).toBeNull();
  });
});
