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

import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { QueryClientProvider } from "@tanstack/react-query";
import "@/styles/global.css";
import "@/styles/design.css";
import { AuthProvider } from "@/auth/AuthProvider";
import { queryClient } from "@/api/client";
import { App } from "@/app/App";
import { ErrorBoundary } from "@/telemetry/ErrorBoundary";
import { installGlobalErrorHandlers } from "@/telemetry/report";
import { isShell } from "@/shell/detect";

// Catch window-level errors (async, resource, uncaught) before React mounts.
installGlobalErrorHandlers();

const root = createRoot(document.getElementById("root")!);

function renderApp() {
  root.render(
    <StrictMode>
      <ErrorBoundary>
        <QueryClientProvider client={queryClient}>
          <AuthProvider>
            <App />
          </AuthProvider>
        </QueryClientProvider>
      </ErrorBoundary>
    </StrictMode>,
  );
}

// Inside the desktop client the instance is not the one serving this bundle, so
// the client is asked where to point and what credential to present before the
// application renders. In a browser this branch is not taken and the boot path
// below is exactly what it always was.
if (isShell()) {
  void import("@/shell/boot").then(({ bootShell }) => bootShell(root, renderApp));
}
// `#/connect` is a development affordance for driving a remote instance from a
// browser (see app/DevConnect). The whole branch — and the module it loads — is
// dropped from a production build, where the only way into the remote mode is a
// native shell configuring the instance before boot.
else if (import.meta.env.DEV && window.location.hash.startsWith("#/connect")) {
  void import("@/app/DevConnect").then(({ DevConnect }) => {
    root.render(
      <StrictMode>
        <DevConnect
          onReady={() => {
            history.replaceState(null, "", window.location.pathname);
            renderApp();
          }}
        />
      </StrictMode>,
    );
  });
} else {
  renderApp();
}
