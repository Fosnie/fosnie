// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
import type { WorkflowSelection } from "./workflowRouting";
import type { RoutingOptions } from "./chatRouting";

// A server must explicitly advertise implemented v2
// support before a composer/editor can send this intent.
export const routingVNextVersion = 2 as const;
export const maxActiveVariants = 256;
export interface VariantSelection { id: string; expected_revision: number }
export interface RoutingIntentV2 {
  schema_version: 2;
  selection: WorkflowSelection;
  required_profile_version: 1 | 2;
  variant?: VariantSelection;
  options?: RoutingOptions;
}
export interface RoutingContractSupport {
  selection_versions: number[];
  profile_versions: number[];
}
const nonNilUuid = (v: string) => /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(v)
  && v !== "00000000-0000-0000-0000-000000000000";
export function validateRoutingIntent(intent: RoutingIntentV2): void {
  if (intent.schema_version !== 2 || ![1, 2].includes(intent.required_profile_version))
    throw new Error("Unsupported routing contract version.");
  if (intent.variant && (intent.selection.mode !== "manual" || !nonNilUuid(intent.variant.id)
    || !Number.isSafeInteger(intent.variant.expected_revision) || intent.variant.expected_revision < 1))
    throw new Error("A variant requires a manual model and its current revision.");
  if (intent.required_profile_version !== 1 && intent.selection.mode !== "auto")
    throw new Error("Profile version applies to Auto selection.");
}
export function requireRoutingSupport(intent: RoutingIntentV2, support?: RoutingContractSupport): void {
  validateRoutingIntent(intent);
  if (!support?.selection_versions.includes(intent.schema_version)
    || !support.profile_versions.includes(intent.required_profile_version))
    throw new Error("This server does not support the selected routing settings.");
}
export function legacyRoutingIntent(intent: RoutingIntentV2): { selection: WorkflowSelection; options?: RoutingOptions } {
  validateRoutingIntent(intent);
  if (intent.variant || intent.required_profile_version !== 1)
    throw new Error("Keep this draft: these routing settings cannot be sent to an older server.");
  return { selection: structuredClone(intent.selection), ...(intent.options ? { options: structuredClone(intent.options) } : {}) };
}
