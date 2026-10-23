// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "@/api/client";
import { catalogueApi,activeVariants,type RoutingProfile } from "@/api/routingCatalogue";
import type { RoutingIntentV2 } from "@/api/routingVNext";
export function ProjectRoutingBinding({projectId}:{projectId:string}) {
 const binding=useQuery({queryKey:["project-routing-v2",projectId],retry:false,queryFn:()=>apiFetch<{schema_version:number;revision:number;routing_intent:RoutingIntentV2|null}>(`/api/projects/${projectId}/routing/v2`)});
 const supported=binding.data?.schema_version===2;
 const catalogue=useQuery({queryKey:["routing-vnext-catalogue"],enabled:supported,retry:false,queryFn:catalogueApi.catalogue});
 const profiles=useQuery({queryKey:["routing-vnext-profiles"],enabled:supported,retry:false,queryFn:catalogueApi.profiles});
 const [draft,set]=useState<{value:string;revision:number}|null>(null);const [error,err]=useState("");const [busy,setBusy]=useState(false);
 if(!supported)return null;
 const intent=binding.data!.routing_intent;
 const saved=intent?.variant?`variant:${intent.variant.id}`:intent?.selection.mode==="auto" && intent.selection.policy_id?`profile:${intent.selection.policy_id}`:intent?"default":"inherit";
 const value=draft?.value ?? saved;
 const choices:RoutingProfile[]=(profiles.data ?? []).filter(p=>p.scope==="deployment");const variants=activeVariants(catalogue.data).map(e=>e.variant).filter(v=>v.scope.kind==="deployment");
 const save=async()=>{setBusy(true);err("");try {
  let routing:RoutingIntentV2|undefined;
  if(value==="default")routing={schema_version:2,required_profile_version:1,selection:{mode:"default"}};
  else if(value.startsWith("profile:"))routing={schema_version:2,required_profile_version:2,selection:{mode:"auto",policy_id:value.slice(8),objective:"balanced"}};
  else if(value.startsWith("variant:")){const v=variants.find(v=>v.id===value.slice(8));if(!v)throw new Error("Variant is no longer current");routing={schema_version:2,required_profile_version:1,selection:{mode:"manual",provider_id:v.provider_id},variant:{id:v.id,expected_revision:v.revision}};}
  await apiFetch(`/api/projects/${projectId}/routing/v2`,{method:"PUT",body:JSON.stringify({schema_version:2,expected_revision:draft?.revision ?? binding.data!.revision,...(routing?{routing_intent:routing}:{clear:true})})});await binding.refetch();set(null);
 }catch(e){err((e as Error).message);}finally{setBusy(false);}};
 return <section className="routing-editor" aria-label="Project model binding"><h3>Default model selection</h3><label className="routing-field">New project chats<select value={value} disabled={busy || catalogue.isPending || profiles.isPending} onChange={e=>set({value:e.target.value,revision:binding.data!.revision})}><option value="inherit">Use each user's configured default</option><option value="default">Explicit configured default model</option>{choices.map(p=><option value={`profile:${p.id}`} key={p.id}>{p.label} · {p.config.membership.kind==="all_available"?"Full Auto":"Custom Auto"}</option>)}{variants.map(v=><option value={`variant:${v.id}`} key={v.id}>{v.label} · Manual</option>)}{value!=="inherit" && value!=="default" && !choices.some(p=>`profile:${p.id}`===value) && !variants.some(v=>`variant:${v.id}`===value) && <option value={value}>Saved choice unavailable</option>}</select></label><p className="ed-hint">Requires project write access. Shared bindings use deployment profiles and variants. Every run applies the current user's permissions. A message selection overrides this binding; Default explicitly resets it.</p><button disabled={busy || !draft} onClick={()=>void save()}>Save project binding</button>{error && <p role="alert">{error} Draft preserved; reload before retrying a revision conflict.</p>}</section>;
}
