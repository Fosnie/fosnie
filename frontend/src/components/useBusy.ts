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

// Shared async-action helper for editors/panels. Wraps a mutation with a single
// in-flight `busy` label, a success/error toast, and a MINIMUM visible duration so
// a fast local save doesn't make the "Saving…" button flicker — it stays readable.

import { useState } from "react";
import { toast } from "@/components/dialogs";

/** Keep an action's busy state visible at least this long (smooth, no flicker). */
const MIN_BUSY_MS = 420;

/** Pad the time since `started` up to {@link MIN_BUSY_MS} — for hand-rolled busy
 * handlers (e.g. multi-purpose busy state) that can't use {@link useBusy}. */
export async function settle(started: number): Promise<void> {
  const left = MIN_BUSY_MS - (Date.now() - started);
  if (left > 0) await new Promise((r) => setTimeout(r, left));
}

export function useBusy() {
  const [busy, setBusy] = useState<string | null>(null);
  /**
   * Run `fn` under the `label` busy state. On success, shows `success` as a
   * success toast (when given); on failure, shows a "<label> failed: …" error
   * toast. Always holds `busy` for at least {@link MIN_BUSY_MS} so the button's
   * "Saving…" state is legible rather than a flash.
   */
  const run = async (label: string, fn: () => Promise<unknown>, success?: string) => {
    setBusy(label);
    const started = Date.now();
    try {
      await fn();
      await settle(started);
      if (success) toast(success, { variant: "success" });
    } catch (e) {
      toast(`${label} failed: ${(e as Error).message}`, { variant: "error" });
    } finally {
      setBusy(null);
    }
  };
  return { busy, run };
}
