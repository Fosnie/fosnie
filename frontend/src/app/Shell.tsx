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

import { Suspense, useEffect, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Outlet, useLocation } from "react-router-dom";
import { MotionConfig } from "motion/react";
import { THEME_VARS, useBranding, useTheme } from "@/api/client";
import { ProjectProvider } from "@/app/ProjectContext";
import { WorkmodeProvider } from "@/app/WorkmodeContext";
import { useAppearance } from "@/app/AppearanceContext";
import { Sidebar } from "@/components/Sidebar";
import { Icon } from "@/components/icons";
import { Spinner } from "@/components/ui";
import { CommandPalette } from "@/components/CommandPalette";
import { DialogHost, ToastHost, toast } from "@/components/dialogs";
import { AnnouncementBanners } from "@/components/AnnouncementBanners";
import { WelcomeGate } from "@/components/WelcomeGate";
import { wsStore } from "@/ws/store";

// App shell: full-height sidebar (272px) + main canvas. No global header — the
// brand lives in the sidebar top, the user chip + sign-out in its footer, and
// connection status in each surface's own top bar (per the design).
export function Shell() {
  const qc = useQueryClient();
  const look = useAppearance();
  const branding = useBranding();
  const theme = useTheme();
  const hasFavicon = branding.data?.some((b) => b.kind === "favicon");

  // Mobile off-canvas sidebar (≤640px; the drawer chrome is display:none on
  // desktop). Auto-close on navigation so tapping a nav/project item dismisses it.
  const [navOpen, setNavOpen] = useState(false);
  const { pathname } = useLocation();
  useEffect(() => setNavOpen(false), [pathname]);

  // Drawer gestures: an edge-swipe right (starting at the left screen edge) opens
  // it; a left-swipe on the open drawer closes it. The backdrop tap and the in-
  // drawer close button also close. Re-bound on `navOpen` so the closure reads
  // the current state. Vertical-dominant moves are left to the scroller.
  useEffect(() => {
    let x0 = 0, y0 = 0, track = false;
    const start = (e: TouchEvent) => {
      const t = e.touches[0];
      x0 = t.clientX; y0 = t.clientY;
      track = navOpen || x0 <= 28;
    };
    const move = (e: TouchEvent) => {
      if (!track) return;
      const t = e.touches[0];
      const dx = t.clientX - x0, dy = t.clientY - y0;
      if (Math.abs(dy) > Math.abs(dx)) { track = false; return; }
      if (!navOpen && dx > 55) { setNavOpen(true); track = false; }
      else if (navOpen && dx < -55) { setNavOpen(false); track = false; }
    };
    const end = () => { track = false; };
    document.addEventListener("touchstart", start, { passive: true });
    document.addEventListener("touchmove", move, { passive: true });
    document.addEventListener("touchend", end, { passive: true });
    return () => {
      document.removeEventListener("touchstart", start);
      document.removeEventListener("touchmove", move);
      document.removeEventListener("touchend", end);
    };
  }, [navOpen]);

  // Apply branding colours/fonts as :root CSS-variable overrides. Each maps onto
  // an existing design token; an absent value leaves the
  // static token in place. Values are sanitised server-side before they reach here.
  useEffect(() => {
    const t = theme.data;
    if (!t) return;
    const root = document.documentElement;
    const applied: string[] = [];
    for (const { key, cssVar } of THEME_VARS) {
      const v = t[key];
      if (v) {
        root.style.setProperty(cssVar, v);
        applied.push(cssVar);
      }
    }
    return () => applied.forEach((v) => root.style.removeProperty(v));
  }, [theme.data]);

  // Live ingest status: the backend pushes `ingest.status` to the uploader as a
  // document advances (extracting → indexing → ready/error). Refresh the doc
  // lists immediately rather than waiting for the 2s poll fallback.
  useEffect(() => {
    return wsStore.onFrame((f) => {
      if (f.type === "ingest.status") {
        qc.invalidateQueries({ queryKey: ["project-docs"] });
        qc.invalidateQueries({ queryKey: ["kb"] });
      } else if (f.type === "group.message") {
        // Refresh unread badges live as messages arrive (#12).
        qc.invalidateQueries({ queryKey: ["group-chats"] });
      } else if (f.type === "invalidate") {
        // Server-pushed read-cache hint after a write (group/membership/grant):
        // refresh the named queries so open views update without a reload.
        for (const k of (f as { keys: string[][] }).keys) qc.invalidateQueries({ queryKey: k });
      } else if (f.type === "automation.reminder") {
        // Lookahead reminder for a soon-to-run automation (Tier-2 #16).
        const mins = Math.max(1, Math.round(Number(f.in_seconds) / 60));
        toast(`Automation "${f.name}" runs in ~${mins} min`, { variant: "info", duration: 8000 });
      }
    });
  }, [qc]);

  // Swap the favicon when a branding asset is present.
  useEffect(() => {
    if (!hasFavicon) return;
    let link = document.querySelector<HTMLLinkElement>("link[rel='icon']");
    if (!link) {
      link = document.createElement("link");
      link.rel = "icon";
      document.head.appendChild(link);
    }
    const prev = link.href;
    link.href = "/api/branding/favicon";
    return () => { if (link) link.href = prev; };
  }, [hasFavicon]);

  return (
    <MotionConfig reducedMotion={look.motion === "reduced" ? "always" : "user"}>
    <ProjectProvider>
      <WorkmodeProvider>
        <div className={"app app-shell" + (navOpen ? " nav-open" : "")}>
          {/* Mobile-only chrome (display:none ≥641px): the sidebar opens by an
              edge-swipe from the left; a slim grabber hints the gesture and taps
              open as a fallback. Inside, an explicit close button + a backdrop
              tap (and a left-swipe) dismiss it. */}
          <button
            type="button"
            className="nav-edge"
            aria-label="Open navigation"
            onClick={() => setNavOpen(true)}
          />
          <Sidebar />
          {navOpen && (
            <button
              type="button"
              className="drawer-close"
              aria-label="Close navigation"
              onClick={() => setNavOpen(false)}
            >
              <Icon.Close size={18} />
            </button>
          )}
          {navOpen && <div className="nav-backdrop" onClick={() => setNavOpen(false)} />}
          {/* Canvas owns no scroll — each surface scrolls its own region
              (.thread / .main-scroll / .legal-thread / .tab-scroll). */}
          <main className="canvas">
            <Suspense fallback={<Spinner />}>
              <Outlet />
            </Suspense>
          </main>
          <DialogHost />
          <ToastHost />
          <AnnouncementBanners />
          <WelcomeGate />
          <CommandPalette />
        </div>
      </WorkmodeProvider>
    </ProjectProvider>
    </MotionConfig>
  );
}
