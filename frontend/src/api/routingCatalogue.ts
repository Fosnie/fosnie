// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
import { apiFetch } from "./client";
import { newPolicy, defaultClassifier, emptyConditions, type Policy, type Conditions } from "./routingSettings";
export type Scope={kind:"deployment"}|{kind:"user";owner_id:string};
export type Effort={kind:"provider_default"}|{kind:"explicit";value:string};
export interface Generation {max_output_tokens?:number|null;max_reasoning_tokens?:number|null;temperature_milli?:number|null;top_p_milli?:number|null;seed?:number|null}
export interface Variant {id:string;revision:number;scope:Scope;provider_id:string;model_config_revision:number;label:string;behaviour_sha256:string;
  behaviour:{adapter:string;model_id:string;effort:Effort;generation:Generation}}
export interface VariantEntry {variant:Variant;configuration_current:boolean;binding_current:boolean;calibration_state:string;behaviour_state?:string}
export interface Deployment {id:string;label:string|null;scope:Scope;revision:number;provider_revision:number;model_config_revision:number;enabled:boolean;model:string|null;
  connection:{id:string;auth_revision:number}|null;reasoning_mode:string|null;is_default:boolean;adapter:string|null;binding_current:boolean;
  capabilities:{efforts:string[]}|null;metadata:Record<string,unknown>;tariffs:Record<string,unknown>}
export interface Catalogue {schema_version:number;catalogue_revision:number;selection_versions:number[];profile_versions:number[];max_active_variants:number;deployments:Deployment[];variants:VariantEntry[]}
export interface Connection {id:string;scope:"deployment"|"user";scope_id:string|null;label:string|null;revision:number;auth_revision:number;adapter:string|null;base_url:string|null;enabled:boolean;api_key_set:boolean;quota_group_id:string|null;metadata:{locality:string;locality_trusted:boolean}}
export interface VariantRef {id:string;revision:number}
export interface TaskGroup {id:string;label:string;priority:number;conditions:Conditions;difficulty:{levels:string[];min_confidence_bp:number;include_unknown:boolean}|null;
 allowed_variants:string[]|null;excluded_variants:string[];assignment:{kind:"pin";variant:VariantRef}|{kind:"prefer";variant_id:string}|null}
export interface ProfileConfig {language_scope?:"en"|"ru"|null;workload_scope?:"source_extract_qa_v1"|null;recovery?:{technical:boolean;source_escalation:boolean}|null;schema_version:2;membership:{kind:"selected";variant_ids:string[]}|{kind:"all_available"};max_active_variants:number;
 quality:{assurance:"declared"|"calibrated";max_relative_loss_bp:number;minimum_absolute_quality_bp:number|null};
 reference:{kind:"configured";variant:VariantRef;allow_uncalibrated:boolean}|{kind:"automatic";default_variant:VariantRef|null;allow_uncalibrated_default:boolean};
 fallback_variant_ids:string[];groups:TaskGroup[];selector_sha256:string|null;verifier_sha256:string|null}
export interface RoutingProfile {schema_version:2;id:string;label:string;scope:"deployment"|"user";revision:number;is_default:boolean;policy:Policy;config:ProfileConfig;calibration_state?:string}
export interface ProfileExport {schema_version:2;label:string;policy:Policy;config:ProfileConfig}
export const catalogueReady=(c?:Catalogue)=>c?.schema_version===2 && Array.isArray(c.selection_versions) && c.selection_versions.includes(2) && Array.isArray(c.profile_versions) && c.profile_versions.includes(2) && Array.isArray(c.variants) && Array.isArray(c.deployments);
export const activeVariants=(c?:Catalogue)=>catalogueReady(c)?c!.variants.filter(v=>v?.configuration_current===true && v.binding_current===true):[];
export const variantRef=(v:Variant):VariantRef=>({id:v.id,revision:v.revision});
export function newProfile(scope:"deployment"|"user",kind:"selected"|"all_available",reference?:Variant):RoutingProfile {
 const base=newPolicy(scope);return {schema_version:2,...base,policy:{...base.policy,rollout:"active",strategy:"hybrid",classifier_provider_id:reference?.provider_id ?? null,classifier:{...defaultClassifier(),cloud_timeout_ms:10000},default_provider_id:reference?.provider_id ?? null},
  config:{schema_version:2,membership:kind==="selected"?{kind,variant_ids:reference?[reference.id]:[]}:{kind},max_active_variants:256,
   quality:{assurance:"declared",max_relative_loss_bp:200,minimum_absolute_quality_bp:null},
   reference:reference?{kind:"configured",variant:variantRef(reference),allow_uncalibrated:true}:{kind:"automatic",default_variant:null,allow_uncalibrated_default:false},
   fallback_variant_ids:[],groups:[],selector_sha256:null,verifier_sha256:null}};
}
export const newGroup=():TaskGroup=>({id:crypto.randomUUID(),label:"Task group",priority:0,conditions:emptyConditions(),difficulty:null,allowed_variants:null,excluded_variants:[],assignment:null});
const put=(path:string,body:unknown)=>apiFetch(path,{method:"PUT",body:JSON.stringify(body)});
export const catalogueApi={
 catalogue:()=>apiFetch<Catalogue>("/api/routing/v2/catalogue"),connections:()=>apiFetch<Connection[]>("/api/routing/v2/connections"),profiles:()=>apiFetch<RoutingProfile[]>("/api/routing/v2/profiles"),
 saveConnection:(id:string,body:unknown)=>put(`/api/routing/v2/connections/${id}`,body),saveDeployment:(id:string,body:unknown)=>put(`/api/routing/v2/deployments/${id}`,body),
 saveVariant:(id:string,body:unknown)=>put(`/api/routing/v2/variants/${id}`,body),
 saveProfile:(p:RoutingProfile)=>put(`/api/routing/v2/profiles/${p.id}`,{schema_version:2,label:p.label,scope:p.scope,expected_revision:p.revision,is_default:p.is_default,policy:p.policy,config:p.config}),
 remove:(kind:string,id:string,revision:number)=>apiFetch(`/api/routing/v2/${kind}/${id}?expected_revision=${revision}`,{method:"DELETE"}),
 discover:(c:Connection)=>apiFetch<{models:{model_id:string}[];partial:boolean;generation_calls:number;capabilities_verified:boolean}>(`/api/routing/v2/connections/${c.id}/discovery`,{method:"POST",body:JSON.stringify({schema_version:2,expected_revision:c.revision})}),
 exportProfile:(id:string)=>apiFetch<ProfileExport>(`/api/routing/v2/profiles/${id}/export`),
 importProfile:(id:string,body:unknown)=>apiFetch(`/api/routing/v2/profiles/${id}/import`,{method:"POST",body:JSON.stringify(body)}),
 metadata:(d:Deployment,metadata:unknown)=>put(`/api/routing/v2/deployments/${d.id}/metadata`,{schema_version:2,expected_revision:d.revision,metadata}),
 tariffs:(d:Deployment,tariffs:unknown)=>put(`/api/routing/v2/deployments/${d.id}/tariffs`,{schema_version:2,expected_revision:d.revision,tariffs}),
};
