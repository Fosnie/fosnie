// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
import { apiFetch } from "./client";
import type { RoutingModel } from "./documentJobs";

export const tasks = ["unknown","current_fact","general","editing","extraction","coding","reasoning","research"] as const;
export const tiers = ["economy","balanced","advanced"] as const;
export const purposes = ["primary","tool_loop","final","router_classify","utility_title","utility_summary","retrieval_decompose","retrieval_grade","web_plan","web_grade","web_synthesis","research_plan","research_write","document","voice","moderation","verify","embed","rerank","ocr","stt","tts"] as const;
export const utilityPurposes = purposes.filter(p => !["primary","tool_loop","final","router_classify","verify","embed","rerank","ocr","stt","tts"].includes(p));
export type Tier = typeof tiers[number];
export type Task = typeof tasks[number];
export type Purpose = typeof purposes[number];
export type Evidence = "unknown" | "declared" | "verified";
export type Locality = "unknown" | "local" | "cloud";
export interface EvaluationEvidence {id:string;source:"live"|"synthetic";dataset_sha256:string;families_sha256:string;
  profile_sha256:string;evaluator_version:string;purpose:Purpose;workload:Requirements;families:number;passed:number;reviewed:boolean}
export interface Profile {
  provider_id:string; provider_revision:number; revision:number; model:string;
  adapter:"openai_chat"|"openai_responses"|"anthropic"|"gemini";
  locality:Locality; locality_trusted:boolean; auto_enabled:boolean; auto_effort?:string|null;
  capabilities:{tools:Evidence;vision:Evidence;structured_output:Evidence;context_tokens:number|null;max_output_tokens:number|null;efforts:string[];structured_output_efforts?:string[]|null};
  tier:Tier;task_tiers:Partial<Record<Task,Tier>>;task_exclusions?:Task[];quality_status:Evidence;evaluation_samples:number;evaluation_version:string|null;
  pricing:{input_microusd_per_million:number|null;output_microusd_per_million:number|null;source:string|null;checked_at:string|null};
  latency_ms:number|null;
  quality_evidence?:EvaluationEvidence[];
}
export interface ModelEntry extends Omit<RoutingModel,"profile"> {scope:"deployment"|"user";profile:Profile|null}
export interface FixedProfile {provider_id:string;provider_revision:number;revision:number;role:string;model:string;
  active_index_revision:number|null;desired_index_revision:number|null;locality:Locality;locality_trusted:boolean;ocr_pdf_mode?:"document"|"page_images"|null}
export interface FixedEntry {id:string;role:string;scope:"deployment"|"user";enabled:boolean;model:string|null;provider_revision:number;
  active_index_revision:number|null;desired_index_revision:number|null;profile_revision:number|null;profile:FixedProfile|null}
export interface Constraints {allowed_providers:string[]|null;local_only:boolean;max_calls:number|null;max_output_tokens:number|null;
  max_total_tokens:number|null;max_duration_ms:number|null;max_cost_microusd:number|null}
export interface Conditions {tasks:Task[];purposes:Purpose[];languages:string[];
  min_input_tokens:number|null;max_input_tokens:number|null;tools:boolean|null;vision:boolean|null;uncertain:boolean|null;
  fresh_information?:boolean|null;structured_output?:boolean|null;all?:Conditions[];any?:Conditions[]}
export interface Rule {id:string;priority:number;conditions:Conditions;
  action:{kind:"select";provider_id:string;effort:string|null}|{kind:"require";minimum_tier:Tier}|{kind:"restrict_pool";allowed_providers:string[]}|{kind:"classify"}}
export interface ClassifierConfig {native_schema:boolean;effort?:string|null;min_confidence?:number;max_input_tokens:number;max_output_tokens:number;local_timeout_ms:number;cloud_timeout_ms:number;answer_reserve_ms?:number|null}
export interface Policy {id:string;revision:number;rollout:"off"|"shadow"|"active";strategy:"rules"|"classifier"|"hybrid";
  objective:"economy"|"balanced"|"quality";constraints:Constraints;rules:Rule[];
  default_provider_id:string|null;classifier_provider_id:string|null;utility_assignments:Partial<Record<Purpose,string>>;technical_fallback:boolean;
  quality_escalation?:boolean;learned_ranking?:boolean;classifier?:ClassifierConfig}
export interface PolicyEntry {id:string;label:string;scope:"deployment"|"user";revision:number;is_default:boolean;policy:Policy}
export interface Management {schema_version:number;deployment_manage:boolean;user_byok_enabled:boolean;personal_policies:boolean}
export interface ApiAlias {schema_version:number;name:string;enabled:boolean;revision:number;objective:"economy"|"balanced"|"quality";
  status:"disabled"|"conflict"|"routing_disabled"|"ready";agent_field:string}
export interface Requirements {task:Task;minimum_tier:Tier;fresh_information:boolean;tools:boolean;vision:boolean;structured_output:boolean;
  input_tokens:number;output_tokens:number;language:string|null;effort:string|null;uncertain:boolean}
export interface DryReport {dry_run:true;inference_calls:0;classifier_required:boolean;rollout:Policy["rollout"];
  execution_target:{model:string;effort:string|null}|null;
  decision:{proposed:{model:string;effort:string|null}|null;actual:null;policy_revision:number;reason_codes:string[];estimated_cost_microusd:number|null};
  requirements:Requirements;constraints:Constraints}
export interface JudgeReport {ok:boolean;error?:string;judge_model?:string;requirements?:Requirements;quality_evaluated:false;answer_calls:0;
  usage:{calls:number;tokens:number;cost_microusd:number;unknown_cost_calls:number;exhausted:boolean}}
export interface ProbeReport {compatible:boolean;quality_evaluated:false;inference_calls:number;usage?:{prompt_tokens?:number;completion_tokens?:number}|null}
export const emptyConstraints = ():Constraints => ({allowed_providers:null,local_only:false,max_calls:null,max_output_tokens:null,max_total_tokens:null,max_duration_ms:null,max_cost_microusd:null});
export const emptyRequirements = ():Requirements => ({task:"unknown",minimum_tier:"economy",fresh_information:false,tools:false,vision:false,structured_output:false,input_tokens:256,output_tokens:1024,language:null,effort:null,uncertain:false});
export const defaultClassifier = ():ClassifierConfig => ({native_schema:false,effort:null,min_confidence:0.75,max_input_tokens:4096,max_output_tokens:512,local_timeout_ms:5000,cloud_timeout_ms:3000});
export const emptyConditions = ():Conditions => ({tasks:[],purposes:[],languages:[],min_input_tokens:null,max_input_tokens:null,tools:null,vision:null,uncertain:null,fresh_information:null,structured_output:null,all:[],any:[]});
export const newPolicy = (scope:PolicyEntry["scope"]):PolicyEntry => {
  const id=crypto.randomUUID();return {id,scope,label:"",revision:0,is_default:false,policy:{id,revision:1,rollout:"off",strategy:"rules",objective:"balanced",
    constraints:emptyConstraints(),rules:[],default_provider_id:null,classifier_provider_id:null,utility_assignments:{},technical_fallback:false,quality_escalation:false,learned_ranking:false,classifier:defaultClassifier()}};
};
export const newRule = ():Rule => ({id:crypto.randomUUID(),priority:0,conditions:emptyConditions(),action:{kind:"require",minimum_tier:"balanced"}});
export function current(entry:ModelEntry):boolean {
  const p=entry.profile;return !!p && p.provider_id===entry.id && p.provider_revision===entry.provider_revision && p.revision===entry.profile_revision && p.model===entry.model;
}
export function profileDraft(entry:ModelEntry,manager:boolean):Profile {
  const p:Profile=entry.profile ? structuredClone(entry.profile) : {provider_id:entry.id,provider_revision:entry.provider_revision,revision:1,model:entry.model ?? "",adapter:"openai_chat",
    locality:"unknown",locality_trusted:false,auto_enabled:false,auto_effort:null,capabilities:{tools:"unknown",vision:"unknown",structured_output:"unknown",context_tokens:null,max_output_tokens:null,efforts:[]},
    tier:"balanced",task_tiers:{},task_exclusions:[],quality_status:"unknown",evaluation_samples:0,evaluation_version:null,
    pricing:{input_microusd_per_million:null,output_microusd_per_million:null,source:null,checked_at:null},latency_ms:null};
  if(!current(entry)) {
    p.auto_enabled=false;p.auto_effort=null;p.locality_trusted=false;p.quality_evidence=[];
    for(const field of ["tools","vision","structured_output"] as const) if(p.capabilities[field]==="verified")p.capabilities[field]="declared";
    if(p.quality_status==="verified") {p.quality_status="declared";p.evaluation_samples=0;p.evaluation_version=null;}
  }
  p.provider_id=entry.id;p.provider_revision=entry.provider_revision;p.model=entry.model ?? "";
  if(!manager){p.locality_trusted=false;p.quality_evidence=[];}
  return p;
}
// Import only into a draft. The server validates the full schema, fingerprint,
// permissions and compare-and-swap revision before accepting evidence.
export function importEvidence(raw:string):EvaluationEvidence[] {
  if(new TextEncoder().encode(raw).length>131072)throw new Error("Evidence file exceeds 128 KiB.");
  const rows:unknown=JSON.parse(raw);
  if(!Array.isArray(rows)||!rows.length||rows.length>64)throw new Error("Import 1–64 evaluation records.");
  for(const row of rows)if(!row || typeof row!=="object" || !["live","synthetic"].includes(row.source)
    || typeof row.reviewed!=="boolean" || !Number.isSafeInteger(row.families) || row.families<1
    || !Number.isSafeInteger(row.passed) || row.passed<0 || row.passed>row.families
    || typeof row.id!=="string" || typeof row.profile_sha256!=="string" || !purposes.includes(row.purpose)
    || !row.workload || !tasks.includes(row.workload.task))throw new Error("Invalid evaluation record.");
  if(new Set(rows.map(row=>row.id)).size!==rows.length)throw new Error("Duplicate evaluation IDs.");
  return rows as EvaluationEvidence[];
}
// Parse decimal USD exactly. Empty means unknown; zero is a known price/cap.
export function usdToMicro(value:string):number|null {
  if(!value.trim())return null;
  if(!/^\d+(\.\d{1,6})?$/.test(value.trim()))throw new Error("Use non-negative USD with at most 6 decimal places.");
  const [whole,fraction=""]=value.trim().split(".");const result=BigInt(whole)*1000000n+BigInt(fraction.padEnd(6,"0"));
  if(result>BigInt(Number.MAX_SAFE_INTEGER))throw new Error("USD amount is too large.");
  return Number(result);
}
export const microToUsd = (value:number|null|undefined):string => value==null ? "" : (value/1000000).toFixed(6).replace(/\.?0+$/,"") || "0";
export const routingSettingsApi = {
  management:()=>apiFetch<Management>("/api/routing/management"),
  models:()=>apiFetch<ModelEntry[]>("/api/routing/catalog"),
  fixed:()=>apiFetch<FixedEntry[]>("/api/routing/fixed-catalog"),
  policies:()=>apiFetch<PolicyEntry[]>("/api/routing/policies"),
  alias:()=>apiFetch<ApiAlias>("/api/routing/public-api-alias"),
  saveAlias:(saved:ApiAlias,draft:Pick<ApiAlias,"name"|"enabled"|"objective">)=>apiFetch<ApiAlias>("/api/routing/public-api-alias",{method:"PUT",body:JSON.stringify({...draft,expected_revision:saved.revision})}),
  saveProfile:(entry:ModelEntry,profile:Profile)=>apiFetch<Profile>(`/api/routing/profiles/${entry.id}`,{method:"PUT",body:JSON.stringify({expected_revision:entry.profile_revision ?? 0,profile})}),
  deleteProfile:(entry:ModelEntry)=>apiFetch(`/api/routing/profiles/${entry.id}?expected_revision=${entry.profile_revision}`,{method:"DELETE"}),
  savePolicy:(entry:PolicyEntry)=>apiFetch<Policy>(`/api/routing/policies/${entry.id}`,{method:"PUT",body:JSON.stringify({label:entry.label,scope:entry.scope,expected_revision:entry.revision,is_default:entry.is_default,policy:entry.policy})}),
  deletePolicy:(entry:PolicyEntry)=>apiFetch(`/api/routing/policies/${entry.id}?expected_revision=${entry.revision}`,{method:"DELETE"}),
  saveFixed:(entry:FixedEntry,profile:FixedProfile)=>apiFetch<FixedProfile>(`/api/routing/fixed-profiles/${entry.id}`,{method:"PUT",body:JSON.stringify({expected_revision:entry.profile_revision ?? 0,profile})}),
  deleteFixed:(entry:FixedEntry)=>apiFetch(`/api/routing/fixed-profiles/${entry.id}?expected_revision=${entry.profile_revision}&role=${entry.role}`,{method:"DELETE"}),
  preview:(policy:PolicyEntry,requirements:Requirements,purpose:Purpose)=>apiFetch<DryReport>("/api/routing/preview",{method:"POST",body:JSON.stringify({policy_id:policy.id,expected_policy_revision:policy.revision,objective:policy.policy.objective,purpose,requirements})}),
  judge:(policy:PolicyEntry,conversation:string,cap:number)=>apiFetch<JudgeReport>("/api/routing/classifier-probe",{method:"POST",body:JSON.stringify({policy_id:policy.id,expected_policy_revision:policy.revision,conversation,max_cost_microusd:cap})}),
  probe:(entry:ModelEntry,kind:string,cap:number)=>apiFetch<ProbeReport>(`/api/routing/profiles/${entry.id}/probe`,{method:"POST",body:JSON.stringify({kind,expected_profile_revision:entry.profile_revision,max_cost_microusd:cap})}),
};
