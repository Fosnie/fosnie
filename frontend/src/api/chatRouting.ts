// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { currentModel, useRoutingModels, type RoutingModel } from "./documentJobs";
import { workflowSelection, type WorkflowSelection } from "./workflowRouting";
import { catalogueApi, activeVariants } from "./routingCatalogue";
import { requireRoutingSupport, type VariantSelection } from "./routingVNext";
import { isShell } from "@/shell/detect";
export type RoutingOptions = { local_only: boolean; effort: string | null };
export type RoutingChoice = { selection: WorkflowSelection; options: RoutingOptions; variant?:VariantSelection; required_profile_version?:1|2; save_binding?:boolean;inherit?:boolean;override_options?:boolean };
export interface RoutingMessage {
  schema_version: number; selection_mode: string; actual: { model: string; effort: string | null } | null;
  actual_unavailable: boolean; reason_codes: string[]; status: string; local_only?: boolean;
  economics?:{assurance:string;scope:string;quality_difference_lower_bp:number;absolute_quality_lower_bp:number;
    forecast:{expected_microusd:number|null;expected_compute_ms:number|null;expected_latency_ms:number|null};
    reference_forecast:{expected_microusd:number|null};evaluated_candidates:number;excluded_unknown_cost:number}|null;
  failure?:unknown; attempts?:{failure:string;model?:string|null}[];
}
export interface ChatRoutingCapabilities { schema_version: number; enabled: boolean; frame: string; selection_modes: string[]; selection_versions?:number[]; profile_versions?:number[];v2_frame?:string; desktop?:boolean; desktop_credential?:string }
interface Saved { schema_version: number; selection: unknown; options: RoutingOptions; adopted: boolean; supported: boolean }
interface Policy { required_profile_version?:number;id: string; label: string; scope: string; revision: number; is_default: boolean;
  policy: { id: string; revision: number; strategy: string; rollout: string; default_provider_id: string | null; classifier_provider_id: string | null } }
export const routingSupported = (c?: ChatRoutingCapabilities) => c?.schema_version === 1 && c.frame === "chat.routed.send";
export const routingSurfaceSupported = (c:ChatRoutingCapabilities|undefined,desktop:boolean) => routingSupported(c) && (!desktop || (c?.desktop===true && c.desktop_credential==="original_key_ticket_v1"));
export function routedChatFrame(choice: RoutingChoice, content: string, chat: string | null, agent: string | null, project: string | null) {
  if(choice.required_profile_version || choice.variant) return {type:"chat.routed.v2.send" as const,request:{chat_id:chat,agent_id:agent,project_id:project,content,
    save_binding:choice.save_binding ?? false,...(choice.inherit && choice.override_options?{options:choice.options}:{}),routing_intent:choice.inherit?undefined:{schema_version:2 as const,selection:choice.selection,required_profile_version:choice.required_profile_version ?? 1,
    ...(choice.variant?{variant:choice.variant}:{}),options:choice.options}}};
  return { type: "chat.routed.send" as const, request: { chat_id: chat, agent_id: agent, project_id: project,
    content, model_selection: choice.selection, options: choice.options } };
}
export function useChatRouting(chat: string | null, allowed = true) {
  const caps = useQuery({ queryKey: ["chat-routing-capabilities"], retry: false,
    queryFn: () => apiFetch<ChatRoutingCapabilities>("/api/routing/browser-chat") });
  const supported = !caps.isError && routingSupported(caps.data);
  const surfaceSupported=!caps.isError && routingSurfaceSupported(caps.data,isShell());
  const surfaceAllowed=allowed && surfaceSupported;
  const saved = useQuery({ queryKey: ["chat-routing", chat], enabled: supported && !!chat, retry: false,
    queryFn: () => apiFetch<Saved>(`/api/chats/${chat}/routing`) });
  const models = useRoutingModels(supported);
  const policies = useQuery({ queryKey: ["chat-routing-policies"], enabled: supported, retry: false,
    queryFn: () => apiFetch<Policy[]>("/api/routing/policies") });
  const catalogue=useQuery({queryKey:["routing-vnext-catalogue"],enabled:supported && caps.data?.selection_versions?.includes(2),retry:false,queryFn:catalogueApi.catalogue});
  const [local,setLocal] = useState<{ key: string; value: RoutingChoice } | null>(null);
  const key=chat ?? "draft";
  const stored=saved.data?.selection as {schema_version?:number;selection?:unknown;variant?:VariantSelection;required_profile_version?:1|2;options?:RoutingOptions}|undefined;
  const selection=workflowSelection(stored?.schema_version===2?stored.selection:stored);
  const adopted=!!saved.data?.adopted;
  const choice:RoutingChoice|null=local?.key===key ? local.value : selection && adopted
    ? { selection,...(stored?.schema_version===2?{variant:stored.variant,required_profile_version:stored.required_profile_version}:caps.data?.selection_versions?.includes(2)?{required_profile_version:1 as const}:{}), options: { local_only: saved.data?.options.local_only ?? false, effort: saved.data?.options.effort ?? null } }
    : !chat && surfaceAllowed && caps.data?.enabled ? {selection:{mode:"default"},options:{local_only:false,effort:null},...(caps.data?.selection_versions?.includes(2)?{inherit:true,required_profile_version:1 as const}:{})} : null;
  const available=models.isError ? [] : (models.data ?? []).filter(currentModel);
  const availablePolicies=(policies.data ?? []).filter(p=>p.id===p.policy.id && p.revision===p.policy.revision);
  const defaultProfileVersion:1|2=(availablePolicies.find(p=>p.is_default && p.scope==="user") ?? availablePolicies.find(p=>p.is_default && p.scope==="deployment"))?.required_profile_version===2?2:1;
  const policyId=choice?.selection.mode==="auto" ? choice.selection.policy_id : undefined;
  const manualId=choice?.selection.mode==="manual" ? choice.selection.provider_id : undefined;
  const policy=policyId
    ? availablePolicies.find(p=>p.id===policyId)
    : availablePolicies.find(p=>p.is_default && p.scope==="user") ?? availablePolicies.find(p=>p.is_default && p.scope==="deployment");
  const target=manualId ? available.find(m=>m.id===manualId) : available.find(m=>m.id===policy?.policy.default_provider_id);
  let problem:string|undefined;
  if (allowed && caps.isPending) problem="Checking model routing…";
  else if (caps.isError && (caps.error as Error & {status?:number}).status!==404) problem="Model routing discovery is unavailable. Retry before sending.";
  else if (supported && chat && saved.isPending) problem="Loading saved model selection…";
  else if (supported && chat && saved.isError) problem="Saved model selection is unavailable. Retry before sending.";
  else if (supported && chat && !selection && !saved.isPending) problem="This saved selection is unsupported. It cannot be replaced silently.";
  if(choice && (choice.required_profile_version || choice.variant)) {
    try {requireRoutingSupport({schema_version:2,selection:choice.selection,required_profile_version:choice.required_profile_version ?? 1,variant:choice.variant},
      caps.data?.selection_versions && caps.data.profile_versions?{selection_versions:caps.data.selection_versions,profile_versions:caps.data.profile_versions}:undefined);
      if(choice.variant && !activeVariants(catalogue.data).some(e=>e.variant.id===choice.variant!.id && e.variant.revision===choice.variant!.expected_revision)) problem="Selected variant is stale or unavailable. Your choice is preserved.";
    } catch(e){problem=(e as Error).message;}
  }
  if (choice) {
    if (!supported) problem="This server does not support routed chat. Auto cannot be sent as a legacy request.";
    else if (!surfaceSupported) problem="This server does not support desktop model routing. Your saved choice is preserved.";
    else if (!allowed || saved.data?.supported===false) problem="Model routing supports general remote conversations. This conversation requires a different execution path.";
    else if (!caps.data?.enabled) problem="Browser model routing is disabled. Your saved choice is preserved.";
    else if (models.isPending || policies.isPending) problem="Loading routing models and policies…";
    else if (models.isError || policies.isError) problem="Routing configuration is unavailable. Retry before sending.";
    else if (!policy) problem="No routing policy is configured. Configure Providers / Routing first.";
    else if (manualId && !available.some(m=>m.id===manualId)) problem="The selected model is unavailable or has no current routing profile.";
    else if (!available.length) problem="No model has a current routing profile. Configure Providers / Routing first.";
  }
  return { defaultProfileVersion,catalogue,availableVariants:activeVariants(catalogue.data).map(e=>e.variant),caps,saved,models,policies,supported,adopted,choice,available,availablePolicies,policy,target,problem,allowed:surfaceAllowed,
    setChoice:(value:RoutingChoice)=>setLocal({key,value}),
    retry:()=>{void catalogue.refetch();void caps.refetch();void saved.refetch();void models.refetch();void policies.refetch();} };
}
export function useRoutedRegeneration() {
  return useQuery({queryKey:["routed-regeneration-capabilities"],retry:false,
    queryFn:()=>apiFetch<{schema_version:number;enabled:boolean;preserves_original:boolean;desktop?:boolean}>("/api/routing/regenerate")});
}
export function efforts(models: RoutingModel[]): string[] {
  return [...new Set(models.flatMap(m=>(m.profile as unknown as {capabilities?:{efforts?:string[]}})?.capabilities?.efforts ?? []))].sort();
}
