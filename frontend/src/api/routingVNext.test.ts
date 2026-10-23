// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
import { describe, expect, it } from "vitest";
import { legacyRoutingIntent, requireRoutingSupport, validateRoutingIntent, type RoutingIntentV2 } from "./routingVNext";

describe("vNext routing negotiation", () => {
  const policy = "00000000-0000-0000-0000-000000000009";
  const profile: RoutingIntentV2 = { schema_version: 2, required_profile_version: 2,
    selection: { mode: "auto", objective: "balanced", policy_id: policy } };
  it("requires support for both wire and policy semantics", () => {
    expect(() => requireRoutingSupport(profile)).toThrow();
    expect(() => requireRoutingSupport(profile, { selection_versions: [1, 2], profile_versions: [1] })).toThrow();
    expect(() => requireRoutingSupport(profile, { selection_versions: [1, 2], profile_versions: [1, 2] })).not.toThrow();
    expect(() => legacyRoutingIntent(profile)).toThrow();
    expect(profile.selection.mode === "auto" && profile.selection.policy_id).toBe(policy);
  });
  it("preserves old intent, options and the caller's draft", () => {
    const old = { ...profile, required_profile_version: 1 as const, options: { local_only: true, effort: "high" } };
    const downgraded = legacyRoutingIntent(old);
    expect(downgraded).toEqual({ selection: old.selection, options: old.options });
    expect(downgraded.selection).not.toBe(old.selection);
    downgraded.options!.effort = "low"; expect(old.options.effort).toBe("high");
  });
  it("rejects variant loss, unsafe revisions and Auto variant overrides", () => {
    const manual: RoutingIntentV2 = { schema_version: 2, required_profile_version: 1,
      selection: { mode: "manual", provider_id: policy }, variant: { id: policy, expected_revision: 7 } };
    expect(() => validateRoutingIntent(manual)).not.toThrow();
    expect(() => legacyRoutingIntent(manual)).toThrow();
    expect(() => validateRoutingIntent({ ...manual, selection: profile.selection })).toThrow();
    for (const expected_revision of [0, -1, 1.5, Number.MAX_SAFE_INTEGER + 1])
      expect(() => validateRoutingIntent({ ...manual, variant: { id: policy, expected_revision } })).toThrow();
  });
});
