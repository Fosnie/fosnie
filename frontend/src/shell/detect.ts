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

// Whether this application is running inside the desktop client rather than a
// browser tab.
//
// Decided once, from something the client itself puts on the window before any
// of this code runs. Everything that differs between the two — where the
// instance is, who holds the socket — branches on this single answer, so there
// is one place to look and one thing to stub in a test.

/** True when the desktop client is hosting this window. */
export function isShell(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}
