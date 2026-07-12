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

// Top-level React error boundary. Catches render/lifecycle crashes, reports
// them to the intra-perimeter telemetry sink, and shows a calm fallback with a
// Reload action instead of a blank white screen.

import { Component, type ErrorInfo, type ReactNode } from "react";
import { reportClientError } from "@/telemetry/report";

interface Props {
  children: ReactNode;
}
interface State {
  crashed: boolean;
}

export class ErrorBoundary extends Component<Props, State> {
  state: State = { crashed: false };

  static getDerivedStateFromError(): State {
    return { crashed: true };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    reportClientError({
      kind: "react",
      message: error.message || String(error),
      stack: `${error.stack ?? ""}\n--- componentStack ---${info.componentStack ?? ""}`,
    });
  }

  render(): ReactNode {
    if (!this.state.crashed) return this.props.children;
    return (
      <div className="flex h-full min-h-screen flex-col items-center justify-center gap-4 p-8 text-center text-slate">
        <div className="text-lg font-medium text-slate-lightest">Something went wrong</div>
        <p className="max-w-md text-sm text-slate">
          The page hit an unexpected error and could not continue. The problem has been reported to
          your administrator.
        </p>
        <button type="button" className="btn btn-gold" onClick={() => window.location.reload()}>
          Reload
        </button>
      </div>
    );
  }
}
