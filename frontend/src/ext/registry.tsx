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

// Front-end extension registries (the open-core seam) — the analogue of the
// backend `ext` slots. Core screens render whatever is registered here; the
// Enterprise frontend registers its sections / message actions / routes / nav
// items without editing a Core screen. Core registers the host set.
//
// Each register* dedupes by identity key (replace) so React fast-refresh / a
// double module evaluation never duplicates an entry.

import type { ReactElement, ReactNode, ComponentType } from "react";
import type { WhoAmI } from "@/api/client";

type Caps = WhoAmI["capabilities"];
type IconCmp = (props: { size?: number }) => ReactElement;

// ── Admin sections ──────────────────────────────────────────────────────────
export interface AdminSection {
  key: string;
  label: string;
  component: ComponentType;
  /** Gate: hidden unless `who.capabilities[capability]` is true. */
  capability?: keyof Caps;
  /** Fine-grained permission gate (custom RBAC): a delegated admin sees the
   *  section only when `who.permissions` holds this (or its `:scoped` variant).
   *  A full admin sees every section; an untagged section is admin-only. */
  permission?: string;
  /** Render bare (no `.main-scroll/.panel` wrap) — e.g. Workflows. */
  fullBleed?: boolean;
}
const adminSections: AdminSection[] = [];
export function registerAdminSection(s: AdminSection): void {
  const i = adminSections.findIndex((x) => x.key === s.key);
  if (i >= 0) adminSections[i] = s;
  else adminSections.push(s);
}
export function getAdminSections(): AdminSection[] {
  return adminSections;
}

// ── Per-message actions + overlays (Chat) ───────────────────────────────────
export interface MsgActionCtx {
  msg: { id: string; reviewDecision?: string | null };
  who?: WhoAmI;
  chatId?: string;
  openOverlay: (key: string, props: Record<string, unknown>) => void;
}
export interface MessageAction {
  key: string;
  predicate: (ctx: MsgActionCtx) => boolean;
  render: (ctx: MsgActionCtx) => ReactNode;
}
const messageActions: MessageAction[] = [];
export function registerMessageAction(a: MessageAction): void {
  const i = messageActions.findIndex((x) => x.key === a.key);
  if (i >= 0) messageActions[i] = a;
  else messageActions.push(a);
}
export function getMessageActions(): MessageAction[] {
  return messageActions;
}

export interface MessageOverlay {
  key: string;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  component: ComponentType<any>;
}
const messageOverlays = new Map<string, MessageOverlay>();
export function registerMessageOverlay(o: MessageOverlay): void {
  messageOverlays.set(o.key, o);
}
export function getMessageOverlay(key: string): MessageOverlay | undefined {
  return messageOverlays.get(key);
}

// ── Routes ──────────────────────────────────────────────────────────────────
export interface RouteDef {
  path: string;
  element: ReactNode;
}
const routes: RouteDef[] = [];
export function registerRoute(r: RouteDef): void {
  const i = routes.findIndex((x) => x.path === r.path);
  if (i >= 0) routes[i] = r;
  else routes.push(r);
}
export function getRoutes(): RouteDef[] {
  return routes;
}

// ── Project documents panel (workspace docs toolbar + per-row slots) ─────────
// Lets the Enterprise edition add an "Import from…" toolbar, a per-document source
// badge, and a per-document action (e.g. Write back) to the Project workspace-docs
// panel without Core hardcoding any connector UI. Core registers nothing → the
// slots are empty and the panel is unchanged.
export interface ProjectDocRef {
  id: string;
  original_filename: string;
  mime?: string | null;
}
export interface ProjectDocsPanel {
  key: string;
  /** Rendered in the workspace-docs panel header (e.g. an Import button). */
  toolbar?: ComponentType<{ projectId: string }>;
  /** Rendered inline on each document row (e.g. a source badge). */
  rowBadge?: ComponentType<{ projectId: string; doc: ProjectDocRef }>;
  /** Rendered as a per-row action (e.g. Write back). */
  rowAction?: ComponentType<{ projectId: string; doc: ProjectDocRef }>;
}
const projectDocsPanels: ProjectDocsPanel[] = [];
export function registerProjectDocsPanel(p: ProjectDocsPanel): void {
  const i = projectDocsPanels.findIndex((x) => x.key === p.key);
  if (i >= 0) projectDocsPanels[i] = p;
  else projectDocsPanels.push(p);
}
export function getProjectDocsPanels(): ProjectDocsPanel[] {
  return projectDocsPanels;
}

// ── Sidebar nav items ───────────────────────────────────────────────────────
export interface NavItem {
  to: string;
  label: string;
  icon: IconCmp;
  predicate: (who?: WhoAmI) => boolean;
}
const navItems: NavItem[] = [];
export function registerNavItem(n: NavItem): void {
  const i = navItems.findIndex((x) => x.to === n.to);
  if (i >= 0) navItems[i] = n;
  else navItems.push(n);
}
export function getNavItems(): NavItem[] {
  return navItems;
}
