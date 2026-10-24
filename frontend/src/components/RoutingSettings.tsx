// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
import { cloneElement, useId, useState, type FormEvent, type ReactElement, type ReactNode } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "@/api/client";
import { routingSettingsApi as api, current, profileDraft, newPolicy, newRule, emptyRequirements, defaultClassifier, usdToMicro, microToUsd,
  tasks, tiers, purposes, utilityPurposes, type ModelEntry, type Profile, type PolicyEntry, type Policy, type Rule, type FixedEntry,
  type FixedProfile, type Management, type Requirements, type Purpose, type Evidence, type DryReport, type JudgeReport, type ProbeReport, type ApiAlias } from "@/api/routingSettings";
import "./RoutingSettings.css";
import {ConditionsEditor} from "./RoutingConditionsEditor";
import { RoutingCatalogueSettings } from "./RoutingCatalogueSettings";
import { Dropdown } from "./Dropdown";

const human = (s:string) => s.replaceAll("_"," ");
const list = (s:string) => [...new Set(s.split(",").map(v=>v.trim()).filter(Boolean))];
const errorText = (e:unknown) => (e as {status?:number})?.status===409
  ? "Configuration changed or another default already exists. Your draft is preserved. Reload the latest version before retrying; edit the existing default to replace it."
  : e instanceof Error ? e.message : "Routing settings are unavailable.";
function useOperation() {
  const [busy,setBusy]=useState(false);const [error,setError]=useState("");
  const run=async(work:()=>Promise<unknown>)=>{setBusy(true);setError("");try{await work();return true;}catch(e){setError(errorText(e));return false;}finally{setBusy(false);}};
  return {busy,error,run};
}
function Field({label,children,hint}:{label:string;children:ReactNode;hint?:string}) {
  const id=useId();
  return <div className="routing-field"><label htmlFor={id}>{label}</label>
    {cloneElement(children as ReactElement<{id:string;"aria-describedby"?:string}>,{id,"aria-describedby":hint ? `${id}-hint` : undefined})}
    {hint && <small id={`${id}-hint`}>{hint}</small>}</div>;
}
function Choice<T extends string>({label,value,options,onChange,disabled=false}:{label:string;value:T;options:readonly T[];onChange:(v:T)=>void;disabled?:boolean}) {
  return <Field label={label}><select value={value} onChange={e=>onChange(e.target.value as T)} disabled={disabled}>{options.map(v=><option key={v} value={v}>{human(v)}</option>)}</select></Field>;
}
function NumberField({label,value,onChange,max=Number.MAX_SAFE_INTEGER,min=0,placeholder="Unknown / unlimited"}:{label:string;value:number|null;onChange:(v:number|null)=>void;max?:number;min?:number;placeholder?:string}) {
  return <Field label={label}><input type="number" min={min} max={max} step="1" value={value ?? ""} placeholder={placeholder} onChange={e=>onChange(e.target.value==="" ? null : Number(e.target.value))}/></Field>;
}
function Toggle({label,value,onChange}:{label:string;value:boolean;onChange:(v:boolean)=>void}) {
  return <label className="routing-check"><input type="checkbox" checked={value} onChange={e=>onChange(e.target.checked)}/><span>{label}</span></label>;
}
function ListField({label,values,onChange,hint,maxLength}:{label:string;values:string[];onChange:(v:string[])=>void;hint?:string;maxLength?:number}) {
  const [raw,setRaw]=useState(values.join(", "));
  return <Field label={label} hint={hint}><input maxLength={maxLength} value={raw} onChange={e=>{setRaw(e.target.value);onChange(list(e.target.value));}}/></Field>;
}
function RefChoice({label,value,models,onChange,optional=true}:{label:string;value:string|null;models:ModelEntry[];onChange:(v:string|null)=>void;optional?:boolean}) {
  return <Field label={label}><select value={value ?? ""} required={!optional} onChange={e=>onChange(e.target.value || null)}>
    <option value="">{optional ? "Not assigned" : "Choose a model"}</option>
    {value && !models.some(m=>m.id===value) && <option value={value}>Unavailable saved assignment</option>}
    {models.map(m=><option key={m.id} value={m.id}>{m.label || m.model || "Unnamed model"}{!m.enabled ? " (disabled)" : !current(m) ? " (profile needed)" : ""}</option>)}
  </select></Field>;
}
function EffortChoice({label,value,efforts,onChange,hint}:{label:string;value:string|null|undefined;efforts:string[];onChange:(v:string|null)=>void;hint:string}) {
  return <Field label={label} hint={hint}><select value={value ?? ""} onChange={e=>onChange(e.target.value || null)}>
    <option value="">Provider default</option>
    {value && !efforts.includes(value) && <option value={value}>Unsupported saved value: {value}</option>}
    {efforts.map(e=><option key={e} value={e}>{e}</option>)}
  </select></Field>;
}
function DeleteButton({label,onDelete}:{label:string;onDelete:()=>void}) {
  const [armed,setArmed]=useState(false);return armed ? <span className="routing-actions"><span>Remove {label}?</span><button type="button" className="btn btn-line sm" onClick={onDelete}>Confirm removal</button><button type="button" className="btn btn-line sm" onClick={()=>setArmed(false)}>Keep</button></span>
    : <button type="button" className="btn btn-line sm" onClick={()=>setArmed(true)}>Remove {label}</button>;
}
function EditorFooter({busy,error,onClose,onReload,children}:{busy:boolean;error:string;onClose:()=>void;onReload:()=>void;children?:ReactNode}) {
  return <><div className="routing-actions"><button className="btn btn-gold sm" disabled={busy}>Save</button><button type="button" className="btn btn-line sm" disabled={busy} onClick={onClose}>Close</button>{children}</div>
    {error && <div role="alert" className="form-err">{error}<button type="button" className="btn btn-line sm" disabled={busy} onClick={onReload}>Reload latest (discard draft)</button></div>}</>;
}

function ProfileEditor({entry,manager,onDone,onClose,onReload}:{entry:ModelEntry;manager:boolean;onDone:()=>Promise<void>;onClose:()=>void;onReload:()=>void}) {
  const [draft,setDraft]=useState(()=>profileDraft(entry,manager));const op=useOperation();
  const [inputPrice,setInputPrice]=useState(microToUsd(draft.pricing.input_microusd_per_million));
  const [outputPrice,setOutputPrice]=useState(microToUsd(draft.pricing.output_microusd_per_million));
  const patch=(p:Partial<Profile>)=>setDraft(d=>({...d,...p}));
  const caps=(p:Partial<Profile["capabilities"]>)=>setDraft(d=>({...d,quality_evidence:[],quality_status:d.quality_status==="verified" ? "declared" : d.quality_status,evaluation_samples:d.quality_status==="verified" ? 0 : d.evaluation_samples,evaluation_version:d.quality_status==="verified" ? null : d.evaluation_version,auto_effort:p.efforts && d.auto_effort && !p.efforts.includes(d.auto_effort) ? null : d.auto_effort,capabilities:{...d.capabilities,...p,...(p.efforts && d.capabilities.structured_output_efforts != null ? {structured_output_efforts:d.capabilities.structured_output_efforts.filter(v=>p.efforts!.includes(v))} : {})}}));
  const quality=(p:Partial<Profile>)=>setDraft(d=>({...d,...p,quality_evidence:[],quality_status:d.quality_status==="verified" ? "declared" : d.quality_status,
    evaluation_samples:d.quality_status==="verified" ? 0 : d.evaluation_samples,evaluation_version:d.quality_status==="verified" ? null : d.evaluation_version}));
  const submit=(e:FormEvent)=>{e.preventDefault();void op.run(async()=>{await api.saveProfile(entry,{...draft,pricing:{...draft.pricing,input_microusd_per_million:usdToMicro(inputPrice),output_microusd_per_million:usdToMicro(outputPrice)}});await onDone();});};
  const evidenceOptions=(v:Evidence):Evidence[]=>v==="verified" ? ["unknown","declared","verified"] : ["unknown","declared"];
  return <form className="routing-editor" onSubmit={submit} aria-label="Model profile editor"><h3>{entry.label || entry.model} · routing profile</h3>
    <p className="ed-hint">Provider revision {entry.provider_revision}; profile revision {entry.profile_revision ?? "none"}. Model: {entry.model || "configure the provider model first"}.</p>
    {!current(entry) && <p className="routing-notice">{entry.profile ? "This profile is stale. Review adapter, capabilities, prices and evidence for the current provider before saving." : "No profile yet. Unknown capabilities and prices remain unknown until you declare or measure them."}</p>}
    {entry.profile?.locality_trusted && !manager && <p className="routing-notice">Saving your profile removes its administrator locality attestation. An administrator must attest it again for local-only routing.</p>}
    <fieldset disabled={op.busy}><div className="routing-grid">
      <Choice label="Adapter" value={draft.adapter} options={["openai_chat","openai_responses","anthropic","gemini"]} onChange={v=>quality({adapter:v,auto_effort:null})}/>
      <Choice label="Locality" value={draft.locality} options={["unknown","local","cloud"]} onChange={v=>patch({locality:v,locality_trusted:false})}/>
      <Choice label="Model tier" value={draft.tier} options={tiers} onChange={v=>quality({tier:v})}/>
      <Choice label="Quality evidence" value={draft.quality_status} options={evidenceOptions(draft.quality_status)} onChange={v=>patch({quality_status:v})}/>
      {(["tools","vision","structured_output"] as const).map(k=><Choice key={k} label={`${human(k)} evidence`} value={draft.capabilities[k]} options={evidenceOptions(draft.capabilities[k])} onChange={v=>caps({[k]:v})}/>)}
      <NumberField label="Context tokens" value={draft.capabilities.context_tokens} max={4294967295} onChange={v=>caps({context_tokens:v})}/>
      <NumberField label="Maximum output tokens" value={draft.capabilities.max_output_tokens} max={4294967295} onChange={v=>caps({max_output_tokens:v})}/>
      <ListField label="Supported efforts" hint="Comma separated provider API values; unknown = empty." maxLength={399} values={draft.capabilities.efforts} onChange={v=>caps({efforts:v})}/>
      <EffortChoice label="Auto effort preset" value={draft.auto_effort} efforts={draft.capabilities.efforts} onChange={v=>quality({auto_effort:v})} hint="Applies to Auto and explicit Auto helper assignments. Request or rule effort wins. Manual uses the provider default unless effort is requested."/>
      <NumberField label="Observed latency (ms)" value={draft.latency_ms} onChange={v=>patch({latency_ms:v})}/>
      <Field label="Input price (USD / million tokens)"><input inputMode="decimal" value={inputPrice} placeholder="Unknown" onChange={e=>setInputPrice(e.target.value)}/></Field>
      <Field label="Output price (USD / million tokens)"><input inputMode="decimal" value={outputPrice} placeholder="Unknown" onChange={e=>setOutputPrice(e.target.value)}/></Field>
      <Field label="Price source"><input maxLength={2048} value={draft.pricing.source ?? ""} onChange={e=>patch({pricing:{...draft.pricing,source:e.target.value || null}})}/></Field>
      <Field label="Price checked at" hint="ISO date or timestamp; blank = unknown."><input maxLength={64} value={draft.pricing.checked_at ?? ""} onChange={e=>patch({pricing:{...draft.pricing,checked_at:e.target.value || null}})}/></Field>
    </div><div className="routing-checks"><Toggle label="Eligible for Auto" value={draft.auto_enabled} onChange={v=>patch({auto_enabled:v})}/>
      {manager && <Toggle label="Administrator attests locality" value={draft.locality_trusted} onChange={v=>patch({locality_trusted:v})}/>}</div>
    <p className="ed-hint">Declared tiers are operator estimates, not measured model quality. A connectivity or JSON probe does not verify quality. Local-only also checks every helper and fixed service; web tools retain their own permissions.</p>
    <details><summary>JSON output effort compatibility</summary>
      <Toggle label="Restrict JSON output to selected efforts" value={draft.capabilities.structured_output_efforts != null} onChange={on=>caps({structured_output_efforts:on ? [] : null})}/>
      <p className="ed-hint">Use this when JSON output works only at particular API efforts. With the restriction enabled, select the supported efforts below; none selected blocks JSON output. An unspecified provider default is also blocked. Auto, rules, helpers and Manual keep the requested effort. This setting does not verify model quality.</p>
      {draft.capabilities.structured_output_efforts != null && <div className="routing-checks">{draft.capabilities.efforts.map(effort=><Toggle key={effort} label={effort} value={draft.capabilities.structured_output_efforts!.includes(effort)} onChange={on=>caps({structured_output_efforts:on ? [...draft.capabilities.structured_output_efforts!,effort] : draft.capabilities.structured_output_efforts!.filter(v=>v!==effort)})}/>)}</div>}
    </details>
    <details><summary>Tasks excluded from Auto</summary>
      <p className="ed-hint">Exclude tasks this model configuration should not handle automatically. Rules and fallback cannot override exclusions. Manual selection and fixed assignments remain explicit choices. Unknown excludes unclassified tasks. These are operator restrictions, not verified quality scores.</p>
      <div className="routing-checks">{tasks.map(task=><Toggle key={task} label={human(task)} value={(draft.task_exclusions ?? []).includes(task)} onChange={on=>quality({task_exclusions:on ? [...(draft.task_exclusions ?? []),task] : (draft.task_exclusions ?? []).filter(t=>t!==task)})}/>)}</div>
    </details>
    <details><summary>Task tiers and evaluation provenance</summary><div className="routing-grid">{tasks.filter(t=>t!=="unknown").map(t=><Choice key={t} label={`${human(t)} tier`} value={draft.task_tiers[t] ?? "inherit"} options={["inherit",...tiers]} onChange={v=>{const next={...draft.task_tiers};if(v==="inherit")delete next[t];else next[t]=v;quality({task_tiers:next});}}/>)}
      <NumberField label="Evaluation samples" value={draft.evaluation_samples} max={4294967295} onChange={v=>patch({evaluation_samples:v ?? 0,quality_status:draft.quality_status==="verified" ? "declared" : draft.quality_status})}/>
      <Field label="Evaluation version / source" hint="Existing verified evidence is retained only while unchanged; evaluation date is not stored by this schema."><input maxLength={128} value={draft.evaluation_version ?? ""} onChange={e=>patch({evaluation_version:e.target.value || null,quality_status:draft.quality_status==="verified" ? "declared" : draft.quality_status})}/></Field>
    </div></details>
    <EditorFooter {...op} onClose={onClose} onReload={onReload}>{entry.profile_revision!=null && <DeleteButton label="profile" onDelete={()=>{void op.run(async()=>{await api.deleteProfile(entry);await onDone();});}}/>}</EditorFooter></fieldset>
  </form>;
}

function RuleEditor({rule,models,strategy,onChange,onRemove}:{rule:Rule;models:ModelEntry[];strategy:Policy["strategy"];onChange:(r:Rule)=>void;onRemove:()=>void}) {
  const poolModels=new Map(models.filter(m=>m.enabled).map(m=>[m.id,m.label || m.model || m.id]));
  if(rule.action.kind==="restrict_pool")rule.action.allowed_providers.forEach(id=>{if(!poolModels.has(id))poolModels.set(id,"Unavailable saved provider");});
  return <div className="routing-rule"><div className="routing-actions"><strong>Rule</strong><code>{rule.id.slice(0,8)}</code><button type="button" className="btn btn-line sm" onClick={onRemove}>Remove rule</button></div>
    <div className="routing-grid"><Field label="Priority" hint="Higher first; UUID breaks ties."><input type="number" step="1" min={-2147483648} max={2147483647} value={rule.priority} onChange={e=>onChange({...rule,priority:Number(e.target.value)})}/></Field>
      <Choice label="Rule action" value={rule.action.kind} options={strategy==="rules" ? ["select","require","restrict_pool"] : ["select","require","restrict_pool","classify"]} onChange={kind=>onChange({...rule,action:kind==="select" ? {kind,provider_id:"",effort:null} : kind==="require" ? {kind,minimum_tier:"balanced"} : kind==="restrict_pool" ? {kind,allowed_providers:[]} : {kind}})}/>
      {rule.action.kind==="select" && <><RefChoice label="Rule model" models={models} value={rule.action.provider_id} optional={false} onChange={v=>onChange({...rule,action:{kind:"select",provider_id:v ?? "",effort:rule.action.kind==="select" ? rule.action.effort : null}})}/>
        <Field label="Rule effort"><input maxLength={24} value={rule.action.effort ?? ""} onChange={e=>onChange({...rule,action:{...rule.action as Extract<Rule["action"],{kind:"select"}>,effort:e.target.value || null}})}/></Field></>}
      {rule.action.kind==="require" && <Choice label="Required tier" value={rule.action.minimum_tier} options={tiers} onChange={v=>onChange({...rule,action:{kind:"require",minimum_tier:v}})}/>}
    </div>
    {rule.action.kind==="restrict_pool" && <><div className="routing-checks">{[...poolModels].map(([id,label])=><Toggle key={id} label={label} value={rule.action.kind==="restrict_pool" && rule.action.allowed_providers.includes(id)} onChange={on=>{if(rule.action.kind==="restrict_pool")onChange({...rule,action:{kind:"restrict_pool",allowed_providers:on ? [...rule.action.allowed_providers,id] : rule.action.allowed_providers.filter(saved=>saved!==id)}});}}/>)}</div>
      <p className="ed-hint">This narrows the main Auto model and its technical fallback. Judge and explicit helpers use their own assignments within the run limits. An empty rule pool denies all main models.</p></>}
    <ConditionsEditor value={rule.conditions} onChange={conditions=>onChange({...rule,conditions})}/>
    <p className="ed-hint">Select/classify ends matching; require raises the tier; restrict pool intersects earlier pools. Rules apply to Auto selection.</p>
  </div>;
}

function targetsEfforts(models:ModelEntry[],id:string|null):string[] {
  const entry=models.find(m=>m.id===id);return entry && current(entry) ? entry.profile?.capabilities.efforts ?? [] : [];
}
function PolicyEditor({entry,models,fixed,onDone,onClose,onReload}:{entry:PolicyEntry;models:ModelEntry[];fixed:FixedEntry[];onDone:()=>Promise<void>;onClose:()=>void;onReload:()=>void}) {
  const [draft,setDraft]=useState(()=>structuredClone(entry));const [cap,setCap]=useState(microToUsd(entry.policy.constraints.max_cost_microusd));const op=useOperation();
  const p=draft.policy;const patch=(v:Partial<Policy>)=>setDraft(d=>({...d,policy:{...d.policy,...v}}));
  const judge={...defaultClassifier(),...p.classifier};
  const judgeEfforts=targetsEfforts(models,p.classifier_provider_id);
  const constraints=(v:Partial<Policy["constraints"]>)=>patch({constraints:{...p.constraints,...v}});
  const targets=models.filter(m=>entry.scope==="user" || m.scope==="deployment");
  const allowed=new Map<string,string>();targets.filter(m=>m.enabled).forEach(m=>allowed.set(m.id,m.label || m.model || m.id));
  fixed.filter(f=>f.enabled && (entry.scope==="user" || f.scope==="deployment")).forEach(f=>allowed.set(f.id,`${human(f.role)} · ${f.model || "model unknown"}`));
  p.constraints.allowed_providers?.forEach(id=>{if(!allowed.has(id))allowed.set(id,"Unavailable saved provider");});
  const submit=(e:FormEvent)=>{e.preventDefault();void op.run(async()=>{
    if(p.strategy!=="rules" && !p.classifier_provider_id)throw new Error("Assign a judge for classifier or hybrid routing.");
    if(p.strategy==="rules" && p.rules.some(r=>r.action.kind==="classify"))throw new Error("Remove classify actions before switching to rules.");
    if(p.rules.some(r=>r.conditions.min_input_tokens!=null && r.conditions.max_input_tokens!=null && r.conditions.min_input_tokens>r.conditions.max_input_tokens))throw new Error("A rule's minimum input cannot exceed its maximum.");
    await api.savePolicy({...draft,policy:{...p,constraints:{...p.constraints,max_cost_microusd:usdToMicro(cap)}}});await onDone();
  });};
  return <form className="routing-editor" onSubmit={submit} aria-label="Routing policy editor"><h3>{entry.revision ? "Edit policy" : "New policy"} · {entry.scope==="user" ? "personal" : "deployment"}</h3>
    <p className="ed-hint">Revision {entry.revision || "new"}. Deployment and Enterprise limits always intersect personal limits. Saving active rollout authorizes Auto for callers whose separate feature gates are enabled.</p>
    <fieldset disabled={op.busy}><div className="routing-grid">
      <Field label="Policy name"><input required maxLength={120} value={draft.label} onChange={e=>setDraft(d=>({...d,label:e.target.value}))}/></Field>
      <Choice label="Rollout" value={p.rollout} options={["off","shadow","active"]} onChange={v=>patch({rollout:v})}/>
      <Choice label="Strategy" value={p.strategy} options={["rules","classifier","hybrid"]} onChange={v=>patch({strategy:v})}/>
      <Choice label="Default objective" value={p.objective} options={["economy","balanced","quality"]} onChange={v=>patch({objective:v})}/>
      <RefChoice label="Checked default model" models={targets} value={p.default_provider_id} onChange={v=>patch({default_provider_id:v})}/>
      <RefChoice label="Judge model" models={targets} value={p.classifier_provider_id} optional={p.strategy==="rules"} onChange={v=>patch({classifier_provider_id:v,classifier:{...judge,effort:null}})}/>
    </div><div className="routing-checks"><Toggle label="Default policy in this scope" value={draft.is_default} onChange={v=>setDraft(d=>({...d,is_default:v}))}/>
      <Toggle label="Local inference only" value={p.constraints.local_only} onChange={v=>constraints({local_only:v})}/></div>
    <p className="routing-notice">Off uses the checked default without a judge. Shadow records an Auto proposal while answering with the checked default. Active applies the Auto decision. Rules uses no judge; classifier always classifies; hybrid first tries terminal rules. A judge can be local or cloud and must satisfy every hard limit.</p>
    <details><summary>Judge limits</summary><div className="routing-grid">
      <EffortChoice label="Judge effort" value={judge.effort} efforts={judgeEfforts} onChange={v=>patch({classifier:{...judge,effort:v}})} hint="Independent of executor Auto presets. Provider default leaves thinking unchanged; use only API values supported by this judge."/>
      <Field label="Judge minimum confidence" hint="Below this value Auto uses the checked default. Self-reported confidence is not a calibrated probability of correctness."><input type="number" min="0" max="1" step="0.01" required value={judge.min_confidence} onChange={e=>patch({classifier:{...judge,min_confidence:Number(e.target.value)}})}/></Field>
      <NumberField label="Judge input budget" value={judge.max_input_tokens} max={16384} onChange={v=>patch({classifier:{...judge,max_input_tokens:v ?? 4096}})}/>
      <NumberField label="Judge output limit" value={judge.max_output_tokens} max={2048} onChange={v=>patch({classifier:{...judge,max_output_tokens:v ?? 512}})}/>
      <NumberField label="Local judge timeout (ms)" value={judge.local_timeout_ms} max={30000} onChange={v=>patch({classifier:{...judge,local_timeout_ms:v ?? 5000}})}/>
      <NumberField label="Cloud judge timeout (ms)" value={judge.cloud_timeout_ms} max={30000} onChange={v=>patch({classifier:{...judge,cloud_timeout_ms:v ?? 3000}})}/>
      <NumberField label="Reserve answer time (ms)" value={judge.answer_reserve_ms ?? null} min={50} max={30000} placeholder="Not reserved" onChange={v=>patch({classifier:{...judge,answer_reserve_ms:v}})}/>
    </div><Toggle label="Require native JSON schema from judge" value={judge.native_schema} onChange={v=>patch({classifier:{...judge,native_schema:v}})}/><p className="ed-hint">Enable after checking the judge supports native structured output. A rejected schema does not trigger another judge call. If the judge cannot leave an answer call or the configured answer time, Auto uses the allowed checked default without a judge. Clearing the answer reserve preserves existing timeout behaviour. One judge call per selection. Input includes instructions and a conservative text allowance. Shortened context always becomes uncertain and uses the checked default. Run limits may tighten these values.</p></details>
    <Toggle label="Technical fallback before output" value={p.technical_fallback} onChange={v=>patch({technical_fallback:v})}/>
    <p className="ed-hint">Active Auto may switch once before output in stateless text API and eligible general browser/desktop text turns. The original limits and permissions stay in force. Tools, documents, voice, durable jobs and partially emitted answers do not replay automatically.</p>
    <details open><summary>Pool and run limits</summary>
      <Toggle label="Restrict provider pool" value={p.constraints.allowed_providers!==null} onChange={v=>constraints({allowed_providers:v ? [] : null})}/>
      {p.constraints.allowed_providers!==null && <><div className="routing-checks">{[...allowed].map(([id,label])=><Toggle key={id} label={label} value={p.constraints.allowed_providers!.includes(id)} onChange={on=>constraints({allowed_providers:on ? [...p.constraints.allowed_providers!,id] : p.constraints.allowed_providers!.filter(x=>x!==id)})}/>)}</div>
        {!p.constraints.allowed_providers.length && <p className="routing-notice">Empty restricted pool denies all providers, including the judge and fixed services.</p>}</>}
      <div className="routing-grid"><NumberField label="Maximum calls" value={p.constraints.max_calls} max={4294967295} onChange={v=>constraints({max_calls:v})}/>
        <NumberField label="Run output limit per call" value={p.constraints.max_output_tokens} max={4294967295} onChange={v=>constraints({max_output_tokens:v})}/>
        <NumberField label="Run total token limit" value={p.constraints.max_total_tokens} onChange={v=>constraints({max_total_tokens:v})}/>
        <NumberField label="Run duration limit (ms)" value={p.constraints.max_duration_ms} onChange={v=>constraints({max_duration_ms:v})}/>
        <Field label="Run cost limit (USD)" hint="Blank = no monetary cap; unknown prices cannot satisfy a hard cap."><input inputMode="decimal" value={cap} onChange={e=>setCap(e.target.value)}/></Field>
      </div><p className="ed-hint">Limits cover judge and helpers too. Include their providers and fixed services in a restricted pool. Local-only does not authorize web access.</p>
    </details>
    <details open><summary>Rules ({p.rules.length})</summary>{p.rules.map((r,i)=><RuleEditor key={r.id} rule={r} models={targets} strategy={p.strategy} onChange={next=>patch({rules:p.rules.map((old,j)=>i===j ? next : old)})} onRemove={()=>patch({rules:p.rules.filter((_,j)=>i!==j)})}/>)}
      <button type="button" className="btn btn-line sm" disabled={p.rules.length>=100} onClick={()=>patch({rules:[...p.rules,newRule()]})}>Add rule</button>
    </details>
    <details><summary>Helper model assignments</summary><p className="ed-hint">The judge has its own assignment above. Primary/tool loop/final use the selected model. Fixed services are configured separately.</p>
      <div className="routing-grid">{utilityPurposes.map(k=><RefChoice key={k} label={human(k)} models={targets} value={p.utility_assignments[k] ?? null} onChange={v=>{const next={...p.utility_assignments};if(v)next[k]=v;else delete next[k];patch({utility_assignments:next});}}/>)}</div>
    </details><EditorFooter {...op} onClose={onClose} onReload={onReload}>{entry.revision>0 && <DeleteButton label="policy" onDelete={()=>{void op.run(async()=>{await api.deletePolicy(entry);await onDone();});}}/>}</EditorFooter></fieldset>
  </form>;
}

function FixedEditor({entry,manager,onDone,onClose,onReload}:{entry:FixedEntry;manager:boolean;onDone:()=>Promise<void>;onClose:()=>void;onReload:()=>void}) {
  const same=entry.profile?.provider_revision===entry.provider_revision && entry.profile.model===entry.model && entry.profile.active_index_revision===entry.active_index_revision && entry.profile.desired_index_revision===entry.desired_index_revision;
  const [draft,setDraft]=useState<FixedProfile>(()=>({provider_id:entry.id,provider_revision:entry.provider_revision,revision:entry.profile_revision ?? 1,role:entry.role,model:entry.model ?? "",
    active_index_revision:entry.active_index_revision,desired_index_revision:entry.desired_index_revision,locality:entry.profile?.locality ?? "unknown",locality_trusted:!!(same && manager && entry.profile?.locality_trusted),ocr_pdf_mode:entry.role==="ocr" ? entry.profile?.ocr_pdf_mode ?? "document" : null}));
  const op=useOperation();return <form className="routing-editor" aria-label="Fixed service profile editor" onSubmit={e=>{e.preventDefault();void op.run(async()=>{await api.saveFixed(entry,draft);await onDone();});}}>
    <h3>{human(entry.role)} · fixed service</h3><p className="ed-hint">Provider revision {entry.provider_revision}; profile revision {entry.profile_revision ?? "none"}. Active index revision {entry.active_index_revision ?? "unknown / not applicable"}; desired index revision {entry.desired_index_revision ?? "unknown / not applicable"}.</p>
    <p className="routing-notice">Embedding profiles attest the exact active or desired index. Saving locality does not change a provider, re-index documents or make a fixed service an Auto LLM candidate.</p>
    <fieldset disabled={op.busy}><div className="routing-grid"><Field label="Fixed service model"><input required value={draft.model} readOnly={entry.role!=="verify_claims"} onChange={e=>setDraft(d=>({...d,model:e.target.value}))}/></Field>
      <Choice label="Fixed service locality" value={draft.locality} options={["unknown","local","cloud"]} onChange={v=>setDraft(d=>({...d,locality:v,locality_trusted:false}))}/></div>
      {entry.role==="ocr" && <><Choice label="PDF input for OCR" value={draft.ocr_pdf_mode ?? "document"} options={["document","page_images"]}
        onChange={v=>setDraft(d=>({...d,ocr_pdf_mode:v as "document"|"page_images"}))}/><p className="ed-hint">Document sends the whole PDF to a service that accepts it. Page images sends rendered scanned pages to an image endpoint and preserves page numbers. Both use the same OCR model and routing limits.</p></>}
      {manager && <Toggle label="Administrator attests fixed service locality" value={draft.locality_trusted} onChange={v=>setDraft(d=>({...d,locality_trusted:v}))}/>} {!manager && entry.profile?.locality_trusted && <p className="ed-hint">Saving removes administrator attestation; ask your administrator to attest the updated profile.</p>}
      {!entry.model && entry.role!=="verify_claims" && <p className="form-err">Configure this provider or index before creating its routing profile.</p>}
      <EditorFooter {...op} onClose={onClose} onReload={onReload}>{entry.profile_revision!=null && <DeleteButton label="fixed profile" onDelete={()=>{void op.run(async()=>{await api.deleteFixed(entry);await onDone();});}}/>}</EditorFooter>
    </fieldset></form>;
}

function ProbePanel({entry}:{entry:ModelEntry}) {
  const [kind,setKind]=useState("connectivity");const [cap,setCap]=useState("");const [result,setResult]=useState<ProbeReport|null>(null);const op=useOperation();
  const known=entry.profile?.pricing.input_microusd_per_million!=null && entry.profile.pricing.output_microusd_per_million!=null;
  return <form className="routing-editor" aria-label="Compatibility probe" onSubmit={e=>{e.preventDefault();setResult(null);void op.run(async()=>{const budget=usdToMicro(cap);if(budget==null)throw new Error("Enter an explicit test cost cap.");setResult(await api.probe(entry,kind,budget));});}}>
    <h3>Explicit compatibility test · {entry.label || entry.model}</h3><p className="ed-hint">One synthetic call, at most 128 output tokens and 10 seconds. No user conversation or tool action is sent. The test does not update profile evidence or evaluate model quality.</p>
    <fieldset disabled={op.busy}><div className="routing-grid"><Choice label="Probe kind" value={kind} options={["connectivity","tool_call","json_response"]} onChange={setKind}/>
      <Field label="Probe cost cap (USD)"><input required inputMode="decimal" value={cap} onChange={e=>setCap(e.target.value)}/></Field></div>
      <button className="btn btn-line sm" disabled={!known || !current(entry) || !entry.enabled}>Run one synthetic model call</button></fieldset>
    {!known && <p className="routing-notice">Declare both input and output prices first. Unknown prices cannot satisfy this test's hard budget.</p>}
    {op.error && <p role="alert" className="form-err">{op.error}</p>}{result && <p role="status">Compatibility: {result.compatible ? "passed" : "not demonstrated"}. Calls: {result.inference_calls}. Quality was not evaluated.</p>}
  </form>;
}

function PreviewPanel({policies,models,management,surface}:{policies:PolicyEntry[];models:ModelEntry[];management:Management;surface:string}) {
  const [id,setId]=useState("");const [requirements,setRequirements]=useState(emptyRequirements);const [purpose,setPurpose]=useState<Purpose>("primary");
  const [text,setText]=useState("");const [cap,setCap]=useState("");const [dry,setDry]=useState<DryReport|null>(null);const [judge,setJudge]=useState<JudgeReport|null>(null);const op=useOperation();
  const policy=policies.find(p=>p.id===id);const judgeModel=models.find(m=>m.id===policy?.policy.classifier_provider_id);
  const writable=policy?.scope==="user" || (surface==="deployment" && management.deployment_manage);
  const priceKnown=judgeModel?.profile?.pricing.input_microusd_per_million!=null && judgeModel.profile.pricing.output_microusd_per_million!=null;
  const patch=(v:Partial<Requirements>)=>{setRequirements(r=>({...r,...v}));setDry(null);};
  return <div><p className="ed-hint">Both previews use saved configuration. Save a draft first. A model proposal is not an executed answer; actual model is always empty here.</p>
    <Field label="Preview saved policy"><Dropdown value={id} fullWidth
      options={[{value:"",label:"Choose a saved policy"},...policies.map(p=>({value:p.id,label:`${p.label} · ${p.scope} · revision ${p.revision}`}))]}
      onChange={value=>{setId(value);setDry(null);setJudge(null);}}/></Field>
    <form className="routing-editor" aria-label="Dry routing preview" onSubmit={e=>{e.preventDefault();setDry(null);void op.run(async()=>{if(!policy)throw new Error("Choose a saved policy.");setDry(await api.preview(policy,requirements,purpose));});}}>
      <h3>Dry preview · zero model calls</h3><fieldset disabled={op.busy || !policy}><div className="routing-grid">
        <Choice label="Preview task" value={requirements.task} options={tasks} onChange={v=>patch({task:v})}/><Choice label="Preview minimum tier" value={requirements.minimum_tier} options={tiers} onChange={v=>patch({minimum_tier:v})}/>
        <Choice label="Preview purpose" value={purpose} options={purposes} onChange={v=>{setPurpose(v);setDry(null);}}/>
        <NumberField label="Preview input tokens" value={requirements.input_tokens} max={4294967295} onChange={v=>patch({input_tokens:v ?? 0})}/>
        <NumberField label="Preview output tokens" value={requirements.output_tokens} max={4294967295} onChange={v=>patch({output_tokens:v ?? 0})}/>
        <Field label="Preview language"><input value={requirements.language ?? ""} onChange={e=>patch({language:e.target.value || null})}/></Field><Field label="Preview effort"><input maxLength={24} value={requirements.effort ?? ""} onChange={e=>patch({effort:e.target.value || null})}/></Field>
      </div><div className="routing-checks">{(["fresh_information","tools","vision","structured_output","uncertain"] as const).map(k=><Toggle key={k} label={`Preview ${human(k)}`} value={requirements[k]} onChange={v=>patch({[k]:v})}/>)}</div>
      <button className="btn btn-line sm">Preview without inference</button></fieldset>
      {dry && <div className="routing-report" role="status"><strong>Proposal: {dry.decision.proposed?.model ?? "none"}{dry.decision.proposed?.effort ? ` · ${dry.decision.proposed.effort}` : ""}</strong>
        <p>Inference calls: {dry.inference_calls}. Policy revision: {dry.decision.policy_revision}. Estimated answer cost: {dry.decision.estimated_cost_microusd==null ? "unknown" : `$${microToUsd(dry.decision.estimated_cost_microusd)}`}.</p>
        <p>{dry.classifier_required ? "The live path may require a judge. This dry preview uses your declared requirements and does not simulate its semantic answer." : "No judge is required for this dry selection."}</p>
        <p>Planned answer model: {dry.execution_target?.model ?? "requires live classification"}. No answer was executed.</p>
        {dry.rollout==="shadow" && <p>Shadow: the live answer uses the checked default, which may differ from this proposal.</p>}
        <p>{dry.decision.reason_codes.map(human).join(" · ")}</p></div>}
    </form>
    <form className="routing-editor" aria-label="Judge test" onSubmit={e=>{e.preventDefault();setJudge(null);void op.run(async()=>{if(!policy)throw new Error("Choose a saved policy.");const budget=usdToMicro(cap);if(budget==null)throw new Error("Enter an explicit judge cost cap.");setJudge(await api.judge(policy,text,budget));});}}>
      <h3>Explicit judge test · one model call</h3><p className="ed-hint">Sends only the test conversation to the configured judge, including cloud judges. No answer generation, fallback, retry or tools. Works while rollout is off and does not prove quality.</p>
      <fieldset disabled={op.busy || !policy || !writable}><Field label="Judge test conversation"><textarea required maxLength={12000} rows={4} value={text} onChange={e=>{setText(e.target.value);setJudge(null);}} placeholder="Use synthetic examples to inspect task requirements."/></Field>
        <Field label="Judge cost cap (USD)"><input required inputMode="decimal" value={cap} onChange={e=>setCap(e.target.value)}/></Field>
        <button className="btn btn-line sm" disabled={!judgeModel || !current(judgeModel) || !priceKnown}>Run one judge call</button></fieldset>
      {policy && !writable && <p className="ed-hint">A deployment manager can test this shared judge; create a personal policy to test your own routing configuration.</p>}
      {policy && (!judgeModel || !current(judgeModel) || !priceKnown) && <p className="routing-notice">Assign a judge with a current profile, JSON compatibility, known prices and adequate context/output limits.</p>}
      {judge && <div className="routing-report" role="status"><strong>{judge.ok ? `Judge: ${judge.judge_model}` : judge.usage.calls===0 ? "Judge was not called. Check capability, locality, price and policy limits." : "Judge test failed; no fallback was called."}</strong>
        {judge.requirements && <p>Task: {human(judge.requirements.task)}; minimum tier: {judge.requirements.minimum_tier}; uncertain: {judge.requirements.uncertain ? "yes" : "no"}. Requirements: {(["fresh_information","tools","vision","structured_output"] as const).filter(k=>judge.requirements![k]).map(human).join(", ") || "none"}.</p>}
        <p>Calls: {judge.usage.calls}; tokens: {judge.usage.tokens}; {judge.usage.unknown_cost_calls ? "cost unknown / unsettled" : `ledger cost $${microToUsd(judge.usage.cost_microusd)}`}. Answer calls: {judge.answer_calls}.</p></div>}
    </form>{op.error && <p role="alert" className="form-err">{op.error}</p>}
  </div>;
}

function Gates({refresh}:{refresh:()=>Promise<void>}) {
  const connectorIngestion=useQuery({queryKey:["connector-ingestion-routing-capabilities"],retry:false,queryFn:()=>apiFetch<{enabled:boolean}>("/api/routing/connector-ingestion")});
  const ingestion=useQuery({queryKey:["ingestion-routing-capabilities"],retry:false,queryFn:()=>apiFetch<{enabled:boolean}>("/api/routing/ingestion")});
  const browser=useQuery({queryKey:["chat-routing-capabilities"],retry:false,queryFn:()=>apiFetch<{enabled:boolean}>("/api/routing/browser-chat")});
  const regenerate=useQuery({queryKey:["routed-regeneration-capabilities"],retry:false,queryFn:()=>apiFetch<{enabled:boolean}>("/api/routing/regenerate")});const op=useOperation();
  return <details className="routing-gates"><summary>Routing rollout gates</summary><p className="ed-hint">These gates cover browser and paired desktop remote text chats. Local folders and device tools are unsupported. Policy rollout and caller gates are separate. Opening settings changes neither. Enable only after profiles, helpers, defaults and limits have been reviewed.</p>
    {([["Browser routed chat",browser,"/api/routing/browser-chat"],["Routed regeneration",regenerate,"/api/routing/regenerate"],["Uploaded document ingestion",ingestion,"/api/routing/ingestion"],["Connector document ingestion",connectorIngestion,"/api/routing/connector-ingestion"]] as const).map(([label,q,path])=><div key={path} className="routing-actions"><span>{label}: {q.isPending ? "checking" : q.isError ? "unavailable" : q.data?.enabled ? "enabled" : "disabled"}</span>
      {q.isError ? <button className="btn btn-line sm" onClick={()=>void q.refetch()}>Retry {label.toLowerCase()}</button> : <button className="btn btn-line sm" disabled={op.busy || !q.data} onClick={()=>{void op.run(async()=>{await apiFetch(path,{method:"PUT",body:JSON.stringify({enabled:!q.data?.enabled})});await refresh();});}}>{q.data?.enabled ? "Disable" : "Enable"} {label.toLowerCase()}</button>}</div>)}
    <p className="ed-hint">Routed ingestion uses the uploader's effective Auto policy, selected text helpers and pinned embedding/OCR services. It is off by default, supports human uploads up to 4 MiB, and stops interrupted jobs without automatic replay. Connector ingestion has its own opt-in gate and requires installed source-ACL support. It uses the importer or sync owner's policy and stops when source access changes. After mapping permissions, request reindexing explicitly.</p>
    {op.error && <p className="form-err" role="alert">{op.error}</p>}
  </details>;
}

function ApiAliasEditor({saved,refresh,reload}:{saved:ApiAlias;refresh:()=>Promise<void>;reload:()=>void}) {
  const [name,setName]=useState(saved.name);const [enabled,setEnabled]=useState(saved.enabled);const [objective,setObjective]=useState(saved.objective);const op=useOperation();
  return <form aria-label="API Auto alias editor" onSubmit={e=>{e.preventDefault();void op.run(async()=>{
    if(!/^[A-Za-z0-9][A-Za-z0-9._/-]{0,127}$/.test(name) || name.startsWith("agent/"))throw new Error("Choose a valid alias name outside the agent/ namespace.");
    await api.saveAlias(saved,{name,enabled,objective});await refresh();});}}>
    <p className="ed-hint">Saved status: {human(saved.status)}. Existing provider names always keep their meaning. Enabling checks every provider scope, including disabled providers. A reserved alias blocks later colliding provider names.</p>
    {saved.status==="conflict" && <p className="routing-notice">This name is already used by a provider. Choose a different alias; the existing provider continues to work.</p>}
    {saved.status==="routing_disabled" && <p className="routing-notice">Alias is configured, but API routing is disabled. It is not listed in /v1/models.</p>}
    <fieldset disabled={op.busy}><div className="routing-grid"><Field label="API Auto alias name" hint="1–128 ASCII letters, digits, dot, underscore, slash or hyphen; agent/ is reserved."><input required maxLength={128} value={name} onChange={e=>setName(e.target.value)}/></Field>
      <Field label="API alias objective"><Dropdown value={objective} fullWidth
        options={(["economy","balanced","quality"] as const).map(value=>({value,label:human(value)}))}
        onChange={setObjective}/></Field></div>
      <Toggle label="Enable API Auto alias" value={enabled} onChange={setEnabled}/><div className="routing-actions" style={{marginTop:12}}><button className="btn btn-line sm">Save API alias</button><button type="button" className="btn btn-line sm" onClick={reload}>Reload API alias (discard draft)</button></div></fieldset>
    {op.error && <p role="alert" className="form-err">{op.error}</p>}
    <p className="ed-hint">Clients use this name in model. Add fosnie_agent_id to use a Fosnie agent; raw completions have no web tools. Responses keep the requested alias in model and report the actual model separately.</p>
  </form>;
}
function ApiAliasSettings({refresh}:{refresh:()=>Promise<void>}) {
  const alias=useQuery({queryKey:["public-api-routing-alias"],retry:false,queryFn:api.alias});
  const gate=useQuery({queryKey:["public-api-routing-capabilities"],retry:false,queryFn:()=>apiFetch<{enabled:boolean}>("/api/routing/public-api")});const op=useOperation();
  const [version,setVersion]=useState(0);const reload=async()=>{const latest=await alias.refetch();if(!latest.error)setVersion(v=>v+1);};
  return <details className="routing-gates"><summary>API Auto alias</summary><p className="ed-hint">API routing, this alias and the platform's programmatic API feature are separate controls. Opening settings enables none of them.</p>
    <div className="routing-actions"><span>API routing: {gate.isError ? "unavailable" : gate.isPending ? "checking" : gate.data?.enabled ? "enabled" : "disabled"}</span>
      {gate.isError ? <button className="btn btn-line sm" onClick={()=>void gate.refetch()}>Retry API routing</button> : <button className="btn btn-line sm" disabled={op.busy || !gate.data} onClick={()=>void op.run(async()=>{await apiFetch("/api/routing/public-api",{method:"PUT",body:JSON.stringify({enabled:!gate.data?.enabled})});await refresh();})}>{gate.data?.enabled ? "Disable" : "Enable"} API routing</button>}</div>
    {op.error && <p role="alert" className="form-err">{op.error}</p>}
    {alias.isPending ? <p>Loading API alias…</p> : alias.isError || alias.data?.schema_version!==1 ? <p role="alert" className="form-err">API alias management is unavailable.<button className="btn btn-line sm" onClick={()=>void alias.refetch()}>Retry API alias</button></p>
      : <ApiAliasEditor key={`${alias.data.revision}:${version}`} saved={alias.data} refresh={refresh} reload={()=>void reload()}/>}
  </details>;
}

export function RoutingSettings({surface}:{surface:"deployment"|"personal"}) {
  const qc=useQueryClient();const management=useQuery({queryKey:["routing-management"],retry:false,queryFn:api.management});
  const known=management.data?.schema_version===1;
  const models=useQuery({queryKey:["routing-models"],enabled:known,retry:false,queryFn:api.models});
  const policies=useQuery({queryKey:["routing-policies"],enabled:known,retry:false,queryFn:api.policies});
  const fixed=useQuery({queryKey:["routing-fixed"],enabled:known,retry:false,queryFn:api.fixed});
  const [tab,setTab]=useState("models");const [selected,setSelected]=useState<ModelEntry|null>(null);const [policy,setPolicy]=useState<PolicyEntry|null>(null);const [fixedEntry,setFixed]=useState<FixedEntry|null>(null);
  const [probe,setProbe]=useState<ModelEntry|null>(null);const [notice,setNotice]=useState("");const [reloadError,setReloadError]=useState("");
  const [editorVersion,setEditorVersion]=useState(0);
  const refresh=async()=>{await Promise.all(["routing-management","routing-models","routing-policies","routing-fixed","chat-routing-policies","chat-routing-capabilities","routed-regeneration-capabilities","ingestion-routing-capabilities","connector-ingestion-routing-capabilities","public-api-routing-alias","public-api-routing-capabilities"].map(key=>qc.invalidateQueries({queryKey:[key]})));};
  const done=async()=>{await refresh();setSelected(null);setPolicy(null);setFixed(null);setProbe(null);setNotice("Routing configuration saved. New runs use the new revision.");};
  const reload=async()=>{setReloadError("");try {
    if(selected){const latest=await models.refetch();if(latest.error)throw latest.error;const entry=latest.data?.find(m=>m.id===selected.id);if(!entry)throw new Error("Provider is no longer visible. Close this draft.");setSelected(null);setSelected(entry);}
    if(policy){const latest=await policies.refetch();if(latest.error)throw latest.error;const entry=latest.data?.find(p=>p.id===policy.id);if(!entry)throw new Error("Policy is no longer visible. Close this draft.");setPolicy(entry);}
    if(fixedEntry){const latest=await fixed.refetch();if(latest.error)throw latest.error;const entry=latest.data?.find(f=>f.id===fixedEntry.id && f.role===fixedEntry.role);if(!entry)throw new Error("Fixed service is no longer visible. Close this draft.");setFixed(entry);}
    setEditorVersion(v=>v+1);
  }catch(e){setReloadError(errorText(e));}};
  const manager=!!management.data?.deployment_manage;const canEdit=(scope:string)=>scope==="deployment" ? manager && surface==="deployment" : !!management.data?.user_byok_enabled && surface==="personal";
  const chooseTab=(value:string)=>{setTab(value);setSelected(null);setPolicy(null);setFixed(null);setProbe(null);setNotice("");setReloadError("");};
  const entries=(models.data ?? []).filter(m=>surface==="personal" || m.scope==="deployment");
  const policyEntries=(policies.data ?? []).filter(p=>(p.required_profile_version ?? 1)===1).filter(p=>surface==="personal" || p.scope==="deployment");
  const fixedEntries=(fixed.data ?? []).filter(f=>surface==="personal" || f.scope==="deployment");
  return <section className="routing-settings" aria-label="Model routing settings"><h2>Model routing</h2><p className="ed-hint">Connect models in Providers, then describe their capabilities and configure how Auto chooses among them. Default and manual selections still obey hard constraints.</p>
    {management.isPending ? <p>Loading routing permissions…</p> : management.isError || !known ? <div role="alert" className="form-err">Routing management is unavailable on this server.<button className="btn btn-line sm" onClick={()=>void management.refetch()}>Retry routing management</button></div> : <>
      <RoutingCatalogueSettings surface={surface} manager={manager} byok={!!management.data?.user_byok_enabled} personalPolicies={!!management.data?.personal_policies}/>
      {surface==="personal" && <p className="ed-hint">Personal policies can use visible deployment models even when personal provider keys are disabled. Shared profiles and policies are shown for reference.</p>}
      {manager && surface==="deployment" && <Gates refresh={refresh}/>}
      {manager && surface==="deployment" && <ApiAliasSettings refresh={refresh}/>}
      <details className="routing-advanced"><summary>Model capabilities, helper services and legacy policies</summary><div className="legal-tabs-l routing-tabs">{["models","policies","fixed services","preview"].map(value=><button key={value} className={`legal-tab${tab===value ? " on" : ""}`} onClick={()=>chooseTab(value)}>{human(value)}</button>)}</div>
      {notice && <p role="status">{notice}</p>}{reloadError && <p role="alert" className="form-err">{reloadError}</p>}
      {models.isError || policies.isError || fixed.isError ? <div role="alert" className="form-err">Routing catalog could not be loaded. Drafts are preserved.<button className="btn btn-line sm" onClick={()=>void refresh()}>Retry catalogs</button></div>
        : models.isPending || policies.isPending || fixed.isPending ? <p>Loading routing catalog…</p> : <>
      {tab==="models" && <>{!entries.length && <p className="ed-hint">No providers in this scope. Add an LLM provider in Providers first.</p>}
        <div className="routing-cards">{entries.map(m=><div key={m.id} className="routing-card"><div><strong>{m.label || m.model || "Unnamed model"}</strong><p>{m.model || "Model unknown"} · {m.scope} · {!m.enabled ? "disabled" : !m.profile ? "no profile" : current(m) ? "current profile" : "stale profile"}</p>
          <small>{m.profile ? `${m.profile.tier} (${m.profile.quality_status}) · ${m.profile.locality}${m.profile.locality_trusted ? " (attested)" : " (unattested)"} · Auto ${m.profile.auto_enabled ? "eligible" : "disabled"}` : "Capabilities, locality and quality are unknown."}</small></div>
          <div className="routing-actions">{canEdit(m.scope) && <button className="btn btn-line sm" onClick={()=>{setSelected(m);setProbe(null);setNotice("");}}>Edit profile</button>}
            {canEdit(m.scope) && current(m) && m.enabled && <button className="btn btn-line sm" onClick={()=>{setProbe(m);setSelected(null);}}>Test compatibility</button>}</div></div>)}</div>
        {selected && <ProfileEditor key={`${selected.id}:${selected.profile_revision}:${selected.provider_revision}:${editorVersion}`} entry={selected} manager={manager} onDone={done} onClose={()=>setSelected(null)} onReload={()=>void reload()}/>}
        {probe && <ProbePanel key={`${probe.id}:${probe.profile_revision}`} entry={probe}/>}</>}
      {tab==="policies" && <><div className="routing-actions">{(surface==="personal" ? management.data?.personal_policies : manager) && <button className="btn btn-line sm" onClick={()=>{setPolicy(newPolicy(surface==="personal" ? "user" : "deployment"));setNotice("");}}>New {surface==="personal" ? "personal" : "deployment"} policy</button>}</div>
        {!policyEntries.length && <p className="ed-hint">No saved routing policy. Auto needs a policy and a checked default.</p>}
        <div className="routing-cards">{policyEntries.map(p=><div key={p.id} className="routing-card"><div><strong>{p.label}{p.is_default ? " · default" : ""}</strong><p>{p.scope} · {p.policy.strategy} · {p.policy.rollout} · revision {p.revision}</p></div>
          {(p.scope==="user" && surface==="personal" || p.scope==="deployment" && manager && surface==="deployment") && <button className="btn btn-line sm" onClick={()=>{setPolicy(p);setNotice("");}}>Edit policy</button>}</div>)}</div>
        {policy && <PolicyEditor key={`${policy.id}:${policy.revision}:${editorVersion}`} entry={policy} models={models.data ?? []} fixed={fixed.data ?? []} onDone={done} onClose={()=>setPolicy(null)} onReload={()=>void reload()}/>}</>}
      {tab==="fixed services" && <><p className="ed-hint">Embed, rerank, OCR, speech and verification keep their own model assignments and provenance.</p><div className="routing-cards">{fixedEntries.map(f=><div className="routing-card" key={`${f.id}:${f.role}`}><div><strong>{human(f.role)}</strong><p>{f.model || "Model unknown"} · {f.scope} · {f.enabled ? "enabled" : "disabled"}</p><small>{f.profile ? `${f.profile.locality} · ${f.profile.locality_trusted ? "attested" : "unattested"}` : "Locality unknown"}</small></div>{canEdit(f.scope) && <button className="btn btn-line sm" onClick={()=>setFixed(f)}>Edit fixed profile</button>}</div>)}</div>
        {!fixedEntries.length && <p className="ed-hint">No visible fixed services configured.</p>}{fixedEntry && <FixedEditor key={`${fixedEntry.id}:${fixedEntry.role}:${fixedEntry.profile_revision}:${fixedEntry.provider_revision}:${editorVersion}`} entry={fixedEntry} manager={manager} onDone={done} onClose={()=>setFixed(null)} onReload={()=>void reload()}/>}</>}
      {tab==="preview" && <PreviewPanel policies={policyEntries} models={models.data ?? []} management={management.data!} surface={surface}/>}</>}</details>
    </>}
  </section>;
}
