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

// What this installation is, shown only when the application is running in the
// desktop client.
//
// Two versions, not one. The client is installed on a machine and updates on its
// own schedule; the instance is upgraded by whoever runs it. They are routinely
// several releases apart, and knowing both is the first useful fact about almost
// any report.
//
// Signing out lives here too: it is the counterpart of pairing, and it ends this
// machine's access from this machine, without needing the web.

import { useEffect, useState } from "react";
import { confirmDialog, toast } from "@/components/dialogs";
import { isShell } from "@/shell/detect";
import { shellInfo, unpair, type ShellInfo } from "@/shell/bridge";
import { useServerVersion } from "@/ws/store";

export function ShellAbout() {
  const [info, setInfo] = useState<ShellInfo | null>(null);
  const [busy, setBusy] = useState(false);
  const serverVersion = useServerVersion();

  useEffect(() => {
    if (!isShell()) return;
    let alive = true;
    shellInfo()
      .then((i) => alive && setInfo(i))
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, []);

  if (!isShell()) return null;

  const signOut = async () => {
    const ok = await confirmDialog({
      danger: true,
      title: "Sign this computer out?",
      body: "This computer will be signed out of the instance and will need a new pairing code to connect again.",
      confirmLabel: "Sign out",
    });
    if (!ok) return;
    setBusy(true);
    try {
      await unpair();
      window.location.reload();
    } catch (e) {
      toast(`Failed: ${e instanceof Error ? e.message : String(e)}`, { variant: "error" });
      setBusy(false);
    }
  };

  return (
    <section className="prof-section">
      <h3>This computer</h3>
      <p className="muted" style={{ marginTop: 6 }}>
        Desktop app {info?.app_version ?? "…"}
        {info?.platform ? ` on ${info.platform}` : ""}, connected to an instance running{" "}
        {serverVersion ?? "an unknown version"}.
      </p>
      <button className="btn btn-danger" disabled={busy} onClick={() => void signOut()}>
        Sign this computer out
      </button>
    </section>
  );
}
