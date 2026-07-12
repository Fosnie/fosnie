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

import { lazy, useEffect } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { createBrowserRouter, Navigate, RouterProvider, useParams } from "react-router-dom";
import { useWhoami } from "@/api/client";
import { useAuth } from "@/auth/AuthProvider";
import { MfaEnrolFlow } from "@/components/MfaEnrol";
import { Icon } from "@/components/icons";
import { SuperAdmin } from "@/screens/SuperAdmin";
import { AppearanceProvider } from "@/app/AppearanceContext";
import { useWorkmode } from "@/app/WorkmodeContext";
import { Login } from "@/screens/Login";
import { Shell } from "@/app/Shell";
import { Chat } from "@/screens/Chat"; // eager — the initial route
import { wsStore } from "@/ws/store";
import { getRoutes } from "@/ext/registry";
// Edition overlay (side-effect): the Enterprise build points `EDITION_ENTRY` at the
// private edition's entry module (registers its sections/route/nav/message-actions
// before the router builds); the Core build aliases `@edition` to an empty stub and
// references no Enterprise file. See vite.config.ts.
import "@edition";

// Lazy-loaded screens — each becomes its own chunk (docx-preview rides with
// DocumentViewer), loaded on first navigation behind the Shell's Suspense.
const lazyScreen = <T extends Record<string, unknown>>(load: () => Promise<T>, name: keyof T) =>
  lazy(() => load().then((m) => ({ default: m[name] as React.ComponentType })));
const ProjectWorkspace = lazyScreen(() => import("@/screens/ProjectWorkspace"), "ProjectWorkspace");
const TabularReview = lazyScreen(() => import("@/screens/TabularReview"), "TabularReview");
const DocumentViewer = lazyScreen(() => import("@/screens/DocumentViewer"), "DocumentViewer");
const Agents = lazyScreen(() => import("@/screens/Agents"), "Agents");
const Admin = lazyScreen(() => import("@/screens/Admin"), "Admin");
const Power = lazyScreen(() => import("@/screens/Power"), "Power");
const Automations = lazyScreen(() => import("@/screens/Automations"), "Automations");
const Prompts = lazyScreen(() => import("@/screens/Prompts"), "Prompts");
const Memory = lazyScreen(() => import("@/screens/Memory"), "Memory");
const Teams = lazyScreen(() => import("@/screens/Teams"), "Teams");
const DirectMessages = lazyScreen(() => import("@/screens/DirectMessages"), "DirectMessages");
const Libraries = lazyScreen(() => import("@/screens/Libraries"), "Libraries");
const LegalShell = lazyScreen(() => import("@/screens/LegalShell"), "LegalShell");
const Profile = lazyScreen(() => import("@/screens/Profile"), "Profile");
const Studio = lazyScreen(() => import("@/screens/Studio"), "Studio");
const DeepResearch = lazyScreen(() => import("@/screens/DeepResearch"), "DeepResearch");

// The chat surface (New chat / `/` / `/c/:id`) is mode-aware: General → Chat,
// Legal → LegalShell (its Assistant/Tabular/Viewer/Documents tab strip),
// Research → the DR home (no chat open) or the normal Chat screen as the run
// view (it already renders posted reports, citations, artefacts). A PROJECT
// always opens the project window (ProjectWorkspace) in both modes — the legal
// project just carries the extra Tabular Review tab (see ProjectWorkspace).
function HomeRoute() {
  const { mode } = useWorkmode();
  const { chatId } = useParams();
  if (mode === "legal") return <LegalShell />;
  if (mode === "research") return chatId ? <Chat /> : <DeepResearch />;
  return <Chat />;
}

// Gate the Teams/DM routes on the `messaging` presence capability (default on).
// When off, the backend endpoints 403 and the sidebar hides the nav; this stops a
// direct URL from rendering a dead screen. Loading state renders the child (the
// API call settles it).
function RequireMessaging({ children }: { children: React.ReactNode }) {
  const who = useWhoami();
  if (who.data && who.data.capabilities.messaging === false) return <Navigate to="/" replace />;
  return <>{children}</>;
}

const router = createBrowserRouter([
  {
    element: <Shell />,
    children: [
      { path: "/", element: <HomeRoute /> },
      { path: "/c/:chatId", element: <HomeRoute /> },
      { path: "/p/:projectId", element: <ProjectWorkspace /> },
      { path: "/p/:projectId/t/:reviewId", element: <TabularReview /> },
      { path: "/p/:projectId/d/:documentId", element: <DocumentViewer /> },
      { path: "/admin", element: <Admin /> },
      { path: "/admin/:section", element: <Admin /> },
      { path: "/power", element: <Power /> },
      { path: "/power/:tab", element: <Power /> },
      // Extension routes — Core registers none; the Enterprise-bound `/moderation`
      // route is registered via `@/ext/registrations`, so the Core build ships
      // without it.
      ...getRoutes().map((r) => ({ path: r.path, element: r.element })),
      {
        path: "/studio",
        element: <Studio />,
        children: [
          { index: true, element: <Navigate to="/studio/agents" replace /> },
          { path: "agents", element: <Agents /> },
          { path: "agents/:agentId", element: <Agents /> },
          { path: "libraries", element: <Libraries /> },
          { path: "libraries/:kbId", element: <Libraries /> },
          { path: "automations", element: <Automations /> },
          { path: "automations/:automationId", element: <Automations /> },
          { path: "prompts", element: <Prompts /> },
          { path: "prompts/:promptId", element: <Prompts /> },
          { path: "memory", element: <Memory /> },
        ],
      },
      { path: "/teams", element: <RequireMessaging><Teams /></RequireMessaging> },
      { path: "/teams/:chatId", element: <RequireMessaging><Teams /></RequireMessaging> },
      { path: "/dm", element: <RequireMessaging><DirectMessages /></RequireMessaging> },
      { path: "/dm/:chatId", element: <RequireMessaging><DirectMessages /></RequireMessaging> },
      { path: "/profile", element: <Profile /> },
    ],
  },
  { path: "*", element: <Navigate to="/" replace /> },
]);

// Mandatory-MFA gate: when the deployment requires a second factor
// and the caller has not enrolled, their session is enrol-only — nothing but this
// wizard is reachable (the backend enforces the same). Full-screen; no nav away.
function MfaEnrolGate() {
  const { logout } = useAuth();
  const qc = useQueryClient();
  return (
    <div className="signin-wrap">
      <div className="signin-card anim-on fade-up" style={{ maxWidth: 520 }}>
        <div className="eyebrow">Fosnie</div>
        <h1 className="serif signin-title">Set up two-step verification</h1>
        <p className="signin-sub" style={{ marginBottom: 18 }}>
          Your organisation requires a second factor before you can continue.
        </p>
        <MfaEnrolFlow onDone={() => qc.invalidateQueries({ queryKey: ["whoami"] })} />
        <button
          type="button"
          className="btn-link"
          style={{ marginTop: 18, background: "none", border: "none", cursor: "pointer", color: "var(--color-gold)" }}
          onClick={logout}
        >
          <Icon.Logout size={13} /> Sign out
        </button>
      </div>
    </div>
  );
}

export function App() {
  const { ready, authenticated } = useAuth();
  const who = useWhoami();
  // Break-glass super-admin panel: reachable WITHOUT a Keycloak login (works even
  // when Keycloak is down — the whole point of break-glass), so it renders ahead
  // of the auth gate.
  const isSuper = typeof window !== "undefined" && window.location.pathname === "/superadmin";

  useEffect(() => {
    if (isSuper || !authenticated) return;
    wsStore.start();
    return () => wsStore.stop();
  }, [authenticated, isSuper]);

  if (isSuper) {
    return (
      <AppearanceProvider>
        <SuperAdmin />
      </AppearanceProvider>
    );
  }
  if (!ready) {
    return <div className="flex h-full items-center justify-center text-slate">Loading…</div>;
  }
  if (!authenticated) return <Login />;
  // Enrol-only session: force the MFA wizard before anything else renders.
  if (who.data?.mfa_enroll_only) {
    return (
      <AppearanceProvider>
        <MfaEnrolGate />
      </AppearanceProvider>
    );
  }
  return (
    <AppearanceProvider>
      <RouterProvider router={router} />
    </AppearanceProvider>
  );
}
