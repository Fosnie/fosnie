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

// A structural guard, not a behaviour test.
//
// Where the SPA sends a request, and what credential it carries, is decided in
// `api/instance.ts` and nowhere else. A handler that hard-codes a relative path
// works perfectly in a browser served by the instance and silently fails on a
// client that addresses a remote one — a class of bug that is invisible until
// someone runs the native surface. These assertions keep the seam intact.

import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath, URL } from "node:url";
import { describe, expect, it } from "vitest";

const SRC = fileURLToPath(new URL("..", import.meta.url));

/** The request layer itself, which is allowed to do both of these things. */
const SEAM = join("api", "instance.ts");

function sourceFiles(dir: string): string[] {
  return readdirSync(dir).flatMap((entry) => {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) return sourceFiles(full);
    return /\.tsx?$/.test(entry) && !/\.test\.tsx?$/.test(entry) ? [full] : [];
  });
}

const files = sourceFiles(SRC).map((path) => ({
  path: path.slice(SRC.length),
  text: readFileSync(path, "utf-8"),
}));

describe("every request goes through the request layer", () => {
  it("finds no hard-coded instance paths outside it", () => {
    // `fetch("/api…`, `fetch(`/api…`, `fetch("/health…` — an absolute path is
    // implicitly same-origin, which is the assumption being removed.
    const offenders = files
      .filter((f) => f.path !== SEAM)
      .filter((f) => /fetch\(\s*["'`]\/(api|health|ws)/.test(f.text))
      .map((f) => f.path);
    expect(offenders).toEqual([]);
  });

  it("finds no socket URL built from the serving origin outside it", () => {
    // `window.location.host` is the same same-origin assumption in another
    // shape. The identity-provider redirect URIs are exempt: they deliberately
    // point back at the page that started the flow, which is never a device.
    const offenders = files
      .filter((f) => f.path !== SEAM)
      .filter((f) => /\blocation\.host\b/.test(f.text))
      .map((f) => f.path);
    expect(offenders).toEqual([]);
  });
});

describe("the device token stays inside the request layer", () => {
  it("is never read anywhere else", () => {
    const offenders = files
      .filter((f) => f.path !== SEAM)
      .filter((f) => /\bconfigureInstance\b/.test(f.text))
      // Configuring the instance is the one legitimate touch: the boot path
      // hands over what it was given and keeps no copy.
      .filter((f) => !/^(app[\\/]DevConnect\.tsx|auth[\\/]AuthProvider\.tsx)$/.test(f.path))
      .map((f) => f.path);
    expect(offenders).toEqual([]);
  });

  it("is never persisted — a token that outlives the process can be lifted off disk", () => {
    const offenders = files
      .filter((f) => /(localStorage|sessionStorage)\s*\.\s*setItem\s*\(\s*[^)]*token/i.test(f.text))
      .map((f) => f.path);
    expect(offenders).toEqual([]);
  });
});
