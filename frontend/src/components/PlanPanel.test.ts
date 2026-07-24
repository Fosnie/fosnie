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

import { planLine } from "./PlanPanel";

const S = (title: string, status: string) => ({ title, status });

describe("planLine", () => {
  it("points at the running step, at its human position", () => {
    const line = planLine([S("a", "done"), S("b", "done"), S("c", "running"), S("d", "pending")]);
    expect(line.done).toBe(2);
    expect(line.total).toBe(4);
    expect(line.position).toBe(3);
    expect(line.current?.title).toBe("c");
  });

  it("falls back to the first unfinished step when none is marked running", () => {
    const line = planLine([S("a", "done"), S("b", "pending"), S("c", "pending")]);
    expect(line.current?.title).toBe("b");
    expect(line.position).toBe(2);
  });

  it("clamps the position when every step is done but the turn has not ended", () => {
    const line = planLine([S("a", "done"), S("b", "done")]);
    expect(line.position).toBe(2);
    expect(line.current?.title).toBe("b");
  });
});
