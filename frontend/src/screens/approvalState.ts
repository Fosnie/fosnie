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

// The state of the one approval a chat is waiting on, and how a resolution frame
// moves it. Kept apart from the screen so the transition can be tested on its own:
// a decision taken anywhere settles the card here, and a decision about a
// different run leaves it alone.

export type ApprovalState = "pending" | "approved" | "closed";

export interface PendingApproval {
  runId: string;
  tool: string;
  summary: string;
  detail?: Record<string, unknown> | null;
  state: ApprovalState;
}

/** Apply an `agent.approval.resolved` frame: a matching run becomes approved or
 *  closed; anything else (a stale frame, a different run) is unchanged. */
export function applyResolved(
  prev: PendingApproval | null,
  runId: string,
  approved: boolean,
): PendingApproval | null {
  if (!prev || prev.runId !== runId) return prev;
  return { ...prev, state: approved ? "approved" : "closed" };
}
