// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
import { efforts, type RoutingMessage, type useChatRouting } from "@/api/chatRouting";
export function ChatModelSelection({routing,disabled}:{routing:ReturnType<typeof useChatRouting>;disabled:boolean}) {
  const {choice,policy}=routing;
  const auto=choice?.selection.mode==="auto" ? choice.selection : null;
  const manualId=choice?.selection.mode==="manual" ? choice.selection.provider_id : null;
  const options=choice?.options ?? {local_only:false,effort:null};
  const pick=(selection:NonNullable<typeof choice>["selection"])=>routing.setChoice({selection,options:{...options,effort:null},...(routing.caps.data?.selection_versions?.includes(2)?{required_profile_version:selection.mode==="auto"?routing.defaultProfileVersion:1}:{} )});
  const enabled=routing.allowed && routing.saved.data?.supported!==false && routing.supported && routing.caps.data?.enabled;
  const selected=choice?.inherit?"inherit":choice?.selection.mode==="manual" ? choice.selection.provider_id : choice?.selection.mode ?? "legacy";
  const levels=efforts(choice?.selection.mode==="auto" ? routing.available : routing.target ? [routing.target] : []);
  return <div className="composer-routing" aria-label="Chat model routing">
    <label>Model <select aria-label="Chat model" disabled={disabled || !enabled} value={selected} onChange={e=>e.target.value==="inherit"?routing.setChoice({selection:{mode:"default"},options:{...options,effort:null},required_profile_version:1,inherit:true}):pick(e.target.value==="auto"
      ? {mode:"auto",objective:"balanced"} : e.target.value==="default" ? {mode:"default"} : {mode:"manual",provider_id:e.target.value})}>
      {selected==="legacy" && <option value="legacy">Current legacy selection</option>}
      {routing.caps.data?.selection_versions?.includes(2) && <option value="inherit">Saved / project binding</option>}<option value="default">Default</option><option value="auto">Auto</option>
      {routing.available.map(m=><option key={m.id} value={m.id}>{m.label ?? m.model ?? m.id}</option>)}
      {manualId && !routing.available.some(m=>m.id===manualId) && <option value={selected}>Saved model unavailable</option>}
    </select></label>
    {choice && auto && <>
      <label>Objective <select aria-label="Routing objective" disabled={disabled || !enabled} value={auto.objective ?? "balanced"}
        onChange={e=>routing.setChoice({...choice,selection:{...auto,objective:e.target.value as "economy"|"balanced"|"quality"}})}>
        <option value="economy">Economy</option><option value="balanced">Balanced</option><option value="quality">Quality</option>
      </select></label>
      <label>Policy <select aria-label="Chat routing policy" disabled={disabled || !enabled} value={auto.policy_id ?? ""}
        onChange={e=>routing.setChoice({...choice,required_profile_version:e.target.value?(routing.availablePolicies.find(p=>p.id===e.target.value)?.required_profile_version===2?2:1):routing.defaultProfileVersion,selection:{...auto,policy_id:e.target.value || undefined}})}>
        <option value="">My default policy</option>{routing.availablePolicies.map(p=><option key={p.id} value={p.id}>{p.label}</option>)}
        {auto.policy_id && !routing.availablePolicies.some(p=>p.id===auto.policy_id) && <option value={auto.policy_id}>Saved policy unavailable</option>}
      </select></label>
      {policy && <span className="ed-hint">{policy.policy.strategy} · {policy.policy.rollout}{policy.policy.rollout==="shadow" ? " · answer uses the default model" : ""}</span>}
    </>}
    {choice?.selection.mode==="manual" && routing.caps.data?.selection_versions?.includes(2) && <label>Variant <select aria-label="Chat model variant" value={choice.variant?.id ?? ""} disabled={disabled || !enabled} onChange={e=>{const v=routing.availableVariants.find(v=>v.id===e.target.value);routing.setChoice({...choice,required_profile_version:1,variant:v?{id:v.id,expected_revision:v.revision}:undefined,options:{...options,effort:null}});}}><option value="">Model provider default</option>{routing.availableVariants.filter(v=>v.provider_id===manualId).map(v=><option key={v.id} value={v.id}>{v.label}</option>)}{choice.variant && !routing.availableVariants.some(v=>v.id===choice.variant?.id) && <option value={choice.variant.id}>Saved variant unavailable</option>}</select></label>}
    {choice && <>
      <label><input aria-label="Local inference only" type="checkbox" disabled={disabled || !enabled} checked={options.local_only}
        onChange={e=>routing.setChoice({...choice,override_options:true,options:{...options,local_only:e.target.checked}})}/> Local inference only</label>
      <label>Effort <select aria-label="Routing effort" disabled={disabled || !enabled || !!choice.variant} value={options.effort ?? ""}
        onChange={e=>routing.setChoice({...choice,override_options:true,options:{...options,effort:e.target.value || null}})}>
        <option value="">Automatic / model default</option>{levels.map(e=><option key={e} value={e}>{e}</option>)}
        {options.effort && !levels.includes(options.effort) && <option value={options.effort}>Saved effort unavailable</option>}
      </select></label>
      {(choice.required_profile_version || choice.variant) && <label><input type="checkbox" checked={choice.save_binding ?? false} onChange={e=>routing.setChoice({...choice,save_binding:e.target.checked})}/>Save as this chat's binding</label>}
      <span className="ed-hint">Applies to the next message. Local inference still permits web sources when the agent has web access.</span>
      {choice.inherit && !choice.override_options && <span className="ed-hint">Options inherit from the saved/project binding until you change them.</span>}
    </>}
    {routing.problem && <span role="status" className="ed-hint">{routing.problem}</span>}
    {(routing.caps.isError || routing.saved.isError || routing.models.isError || routing.policies.isError) && <button type="button" onClick={routing.retry} disabled={disabled}>Retry routing</button>}
  </div>;
}
export function ModelBadge({routing}:{routing?:RoutingMessage}) {
  if (!routing) return null;
  const reason=routing.reason_codes.includes("task_assignment") ? "Your routing rule"
    : routing.reason_codes.includes("task_preference") ? "Your preferred model"
    : routing.reason_codes.includes("configured_model_roles") ? "Auto · configured model roles"
    : routing.reason_codes.includes("uncertain_task_default") ? "Task unclear · default model"
    : routing.reason_codes.includes("no_matching_role_default") ? "No matching role · default model"
    : routing.reason_codes.includes("declared_explicit_fallback") ? "Your fallback model"
    : routing.reason_codes.includes("single_eligible_model") ? "Only eligible model"
    : routing.reason_codes.includes("shadow_default_planned") ? "Shadow · default answered"
    : routing.reason_codes.includes("routing_off") ? "Routing off · default answered"
    : routing.reason_codes.includes("technical_fallback") ? routing.status==="completed" ? "Alternate model answered" : "Alternate model attempted"
    : routing.reason_codes.some(c=>c.startsWith("classifier_skipped_answer_")) ? "Judge skipped · answer allowance preserved"
    : routing.reason_codes.includes("classifier_fallback") ? "Judge unavailable · checked default"
    : routing.selection_mode==="manual" ? "Manual selection" : routing.selection_mode==="default" ? "Configured default" : "Auto policy";
  return <div className="ed-hint" aria-label="Actual model">{routing.actual?.model ?? (routing.actual_unavailable ? "Model details unavailable" : "No actual model recorded")}
    {routing.actual?.effort ? ` · ${routing.actual.effort}` : ""} · {reason}{routing.status==="interrupted" ? " · interrupted" : ""}</div>;
}
