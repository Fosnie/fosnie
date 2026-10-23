// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { catalogueApi, activeVariants } from "./routingCatalogue";
import { requireRoutingSupport, type RoutingIntentV2 } from "./routingVNext";
import { currentModel, useRoutingModels } from "./documentJobs";

export type WorkflowObjective = "economy" | "balanced" | "quality";
export type WorkflowSelection = { mode: "default" } | { mode: "manual"; provider_id: string }
  | { mode: "auto"; objective?: WorkflowObjective; policy_id?: string | null };
interface Capabilities {
  schema_version: number; enabled: boolean;selection_versions?:number[];profile_versions?:number[]; selection_modes: string[];
  custom_agents: boolean; group_delivery: boolean; max_recipients: number;
  max_libraries: number; event_placeholders: string[];
}
interface Policy {
  required_profile_version?:number;id: string; label: string; scope: string; revision: number; is_default: boolean;
  policy: { id: string; revision: number; strategy?: "rules" | "classifier" | "hybrid";
    rollout?: "off" | "shadow" | "active"; classifier_provider_id?: string | null };
}
// Unknown API configurations remain intact until the user explicitly replaces
// them. In particular, null is not the same as an absent model_selection.
export function workflowSelection(value: unknown): WorkflowSelection | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const s = value as Record<string, unknown>;
  if (s.mode === "default" && Object.keys(s).every(k => k === "mode")) return s as WorkflowSelection;
  if (s.mode === "manual" && typeof s.provider_id === "string" && Object.keys(s).every(k => ["mode", "provider_id"].includes(k))) return s as WorkflowSelection;
  if (s.mode === "auto" && (s.objective === undefined || ["economy", "balanced", "quality"].includes(String(s.objective)))
    && (s.policy_id == null || typeof s.policy_id === "string") && Object.keys(s).every(k => ["mode", "objective", "policy_id"].includes(k))) return s as WorkflowSelection;
  return null;
}
export function workflowSelectionLabel(value: unknown): string {
  const s = workflowSelection((value as RoutingIntentV2|undefined)?.schema_version===2?(value as RoutingIntentV2).selection:value);
  return !s ? "Saved configuration" : s.mode === "auto" ? `Auto · ${s.objective ?? "balanced"}` : s.mode === "manual" ? "Manual" : "Default";
}
export function useWorkflowRouting(value: unknown, enabled: boolean, ownerId?: string) {
  const intent=(value as RoutingIntentV2|undefined)?.schema_version===2?value as RoutingIntentV2:undefined;
  const selection = workflowSelection(intent?intent.selection:value);
  const viewer = useQuery({ queryKey: ["whoami"], enabled: enabled && !!ownerId, retry: false,
    queryFn: () => apiFetch<{ user_id: string }>("/api/whoami") });
  const ownerCatalog = !ownerId || (!viewer.isError && viewer.data?.user_id === ownerId);
  const capabilities = useQuery({ queryKey: ["workflow-routing-capabilities"], enabled, retry: false,
    queryFn: () => apiFetch<Capabilities>("/api/routing/workflow-jobs") });
  const catalogue=useQuery({queryKey:["routing-vnext-catalogue"],enabled:enabled && capabilities.data?.selection_versions?.includes(2),retry:false,queryFn:catalogueApi.catalogue});
  const models = useRoutingModels(enabled && (selection?.mode === "manual" || selection?.mode === "auto"));
  const policies = useQuery({ queryKey: ["workflow-routing-policies"], enabled: enabled && selection?.mode === "auto", retry: false,
    queryFn: () => apiFetch<Policy[]>("/api/routing/policies") });
  const availableModels = models.isError ? [] : (models.data ?? []).filter(m => currentModel(m) && (ownerCatalog || m.scope === "deployment"));
  const availablePolicies = policies.isError ? [] : (policies.data ?? []).filter(p => p.id === p.policy.id && p.revision === p.policy.revision && (ownerCatalog || p.scope === "deployment"));
  const policy = selection?.mode === "auto" ? selection.policy_id
    ? availablePolicies.find(p => p.id === selection.policy_id)
    : ownerCatalog ? availablePolicies.find(p => p.is_default && p.scope === "user") ?? availablePolicies.find(p => p.is_default && p.scope === "deployment") : undefined : undefined;
  const supported = !capabilities.isError && capabilities.data?.schema_version === 1;
  let problem: string | undefined;
  if (value !== undefined) {
    if (!selection) problem = "This saved selection is not supported by this editor. It will be preserved until you replace it.";
    else if (!supported) problem = capabilities.isPending ? "Loading workflow routing settings…" : "Workflow routing settings are unavailable. Retry before changing the selection.";
    else if (!capabilities.data?.selection_modes.includes(selection.mode)) problem = "This selection mode is not supported by the server.";
    else if (selection.mode === "manual" && !availableModels.some(m => m.id === selection.provider_id)) problem = models.isPending
      ? "Loading models…" : "Choose an available model with a current routing profile.";
    else if (selection.mode === "auto" && !policy && (ownerCatalog || selection.policy_id)) problem = policies.isPending
      ? "Loading routing policies…" : "Choose an available routing policy. No matching default policy was found.";
  }
  if(intent) {try{requireRoutingSupport(intent,capabilities.data?.selection_versions && capabilities.data.profile_versions?{selection_versions:capabilities.data.selection_versions,profile_versions:capabilities.data.profile_versions}:undefined);
    if(intent.variant && !activeVariants(catalogue.data).some(e=>e.variant.id===intent.variant!.id && e.variant.revision===intent.variant!.expected_revision))problem="Selected variant is stale or unavailable.";
    }catch(e){problem=(e as Error).message;}}
  const defaultProfileVersion:1|2=(availablePolicies.find(p=>p.is_default && p.scope==="user") ?? availablePolicies.find(p=>p.is_default && p.scope==="deployment"))?.required_profile_version===2?2:1;
  return { defaultProfileVersion,intent,catalogue,availableVariants:activeVariants(catalogue.data).filter(e=>ownerCatalog || e.variant.scope.kind==="deployment").map(e=>e.variant),selection, capabilities, models, policies, availableModels, availablePolicies, policy, supported, problem, ownerCatalog, viewer };
}
