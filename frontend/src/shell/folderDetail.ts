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

// The shape of a folder action's approval detail, and how a client decides it is
// looking at one. Kept apart from the card that renders it so the decision — the
// part that has to keep working when it meets an instance older than this field —
// can be tested on its own, in plain Node.

export interface FolderDetail {
  kind: "diff" | "command" | "delete";
  path?: string;
  full_path?: string;
  workspace?: string;
  workspace_id?: string;
  new_content?: string;
  command?: string;
  cwd?: string;
  prefix?: string | null;
}

/**
 * Fold the raw approval detail into the shape a folder card renders, or null.
 *
 * Null is the answer for anything that is not a folder action: an MCP or
 * custom-tool approval, and — the case that matters most — an approval from an
 * instance built before this field existed, which sends no detail at all. In
 * every one of those the caller falls back to the sentence it has always shown,
 * so a newer client pointed at an older instance still asks the question.
 */
export function asFolderDetail(detail: unknown): FolderDetail | null {
  if (!detail || typeof detail !== "object") return null;
  const kind = (detail as Record<string, unknown>).kind;
  if (kind === "diff" || kind === "command" || kind === "delete") {
    return detail as FolderDetail;
  }
  return null;
}
