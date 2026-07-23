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

// Starting up inside the desktop client.
//
// The client knows where the instance is and holds the credential for it; this
// asks for both, points the application at them, and renders the same
// application a browser would get. An unpaired installation gets the pairing
// screen first and the application immediately after, with no restart.
//
// This is one of the few places allowed to configure the request layer, for the
// same reason the sign-in flow is: it is where the credential enters, and it
// keeps no copy of its own.

import { StrictMode } from "react";
import type { Root } from "react-dom/client";
import { queryClient } from "@/api/client";
import { configureInstance, setUnauthorisedHandler } from "@/api/instance";
import { DialogHost, ToastHost, confirmDialog, toast } from "@/components/dialogs";
import { Pairing } from "@/shell/Pairing";
import {
  SHELL_EVENTS,
  installUpdate,
  instanceConfig,
  onShellEvent,
  pendingUpdate,
  type InstanceConfig,
  type UpdateReady,
} from "@/shell/bridge";

/** Point the application at the instance this client is paired with. */
function adopt(cfg: InstanceConfig) {
  configureInstance({ baseUrl: cfg.base_url, token: cfg.token });
}

/**
 * Boot the application in the desktop client.
 *
 * `renderApp` is the ordinary application render, shared with the browser path
 * so that what runs inside the client is the application itself and not a
 * variant of it.
 */
export async function bootShell(root: Root, renderApp: () => void): Promise<void> {
  const showPairing = (notice?: string) => {
    root.render(
      <StrictMode>
        {/* The dialogs and toasts the rest of the application mounts inside its
            own frame. This screen is rendered instead of that frame, not within
            it, so without these a question asked here has nowhere to appear —
            an update offer while the machine is unpaired would be silently
            swallowed. */}
        <DialogHost />
        <ToastHost />
        <Pairing
          notice={notice}
          onPaired={(cfg) => {
            adopt(cfg);
            // Re-arm: this is a new pairing, and it can be withdrawn too.
            finished = false;
            setUnauthorisedHandler(unpaired);
            renderApp();
          }}
        />
      </StrictMode>,
    );
  };

  // The instance can withdraw this machine at any time. Drop the credential,
  // clear anything read with it, and ask to be paired again — with a reason, so
  // it does not read as a fault.
  //
  // Two things can notice, and both end here. The client's own check asks the
  // instance once a minute, which covers a window sitting idle. A refused
  // request notices immediately, which covers the far more common case: the
  // person is using the application, and every screen they open is quietly
  // failing. Whichever gets there first, this runs once.
  let finished = false;
  const unpaired = () => {
    if (finished) return;
    finished = true;
    configureInstance(null);
    setUnauthorisedHandler(null);
    queryClient.clear();
    showPairing("This computer was signed out of the instance. Pair it again to carry on.");
  };
  void onShellEvent(SHELL_EVENTS.unpaired, unpaired);
  setUnauthorisedHandler(unpaired);

  // A notification was clicked. The client has already brought the window
  // forward; take the user to what it was about.
  void onShellEvent<string>(SHELL_EVENTS.openChat, (chatId) => {
    if (!chatId) return;
    window.history.pushState({}, "", `/c/${chatId}`);
    window.dispatchEvent(new PopStateEvent("popstate"));
  });

  // A new release has been fetched and checked in the background; installing it
  // restarts the application, so it is asked for rather than done. Declining is
  // free: the running version carries on and the offer returns with tomorrow's
  // check. Asked once per version, so a long session is not nagged.
  const offered = new Set<string>();
  const offerUpdate = (update: UpdateReady | null) => {
    if (!update?.version || offered.has(update.version)) return;
    offered.add(update.version);
    void (async () => {
      const ok = await confirmDialog({
        title: `Update to version ${update.version}?`,
        body: update.notes
          ? `${update.notes}\n\nFosnie will restart to finish installing.`
          : "The update has been downloaded. Fosnie will restart to finish installing.",
        confirmLabel: "Restart and update",
      });
      if (!ok) return;
      try {
        await installUpdate();
      } catch (e) {
        toast(`The update could not be installed: ${e instanceof Error ? e.message : String(e)}`, {
          variant: "error",
        });
      }
    })();
  };

  void onShellEvent<UpdateReady>(SHELL_EVENTS.updateReady, offerUpdate);
  // And ask, because the check runs the moment the client starts and routinely
  // finishes before this window exists. An event nobody was listening for is an
  // update nobody is ever offered — which is exactly how an application ends up
  // claiming to keep itself current while sitting on an old version forever.
  void pendingUpdate().then(offerUpdate).catch(() => {});

  let cfg: InstanceConfig | null = null;
  try {
    cfg = await instanceConfig();
  } catch {
    // A credential store that cannot be read is treated as an unpaired machine:
    // pairing again is something the user can actually do about it.
    cfg = null;
  }

  if (cfg) {
    adopt(cfg);
    renderApp();
  } else {
    showPairing();
  }
}
