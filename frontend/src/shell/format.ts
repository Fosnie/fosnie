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

// Tidying up what people type when they connect something to an instance.
// Shared by the desktop client's pairing screen and the development connect
// form, so the two treat the same input the same way.

/** Pairing codes are read off one screen and typed into another, so people group
 *  and lower-case them. Fold that back before sending. */
export function normaliseCode(raw: string): string {
  return raw.replace(/[\s-]/g, "").toUpperCase();
}

/** Add a scheme when someone typed a bare host, and drop trailing slashes. */
export function normaliseBase(raw: string): string {
  const t = raw.trim().replace(/\/+$/, "");
  return /^https?:\/\//i.test(t) ? t : `https://${t}`;
}
