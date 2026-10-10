// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Fixed inference roles are guarded, never quality-ranked or silently replaced.
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use super::*;
use crate::{AppState, Result, auth::{AuthContext, permissions}};

#[derive(Debug,Clone,Copy,PartialEq,Eq,PartialOrd,Ord,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum FixedRole { Embed, EmbedDesired, Rerank, Ocr, Stt, Tts, Verify, VerifyClaims }
impl FixedRole {
    /// Physical provider role; stages have independent attestations/grants.
    pub fn name(self) -> &'static str { match self { Self::Embed|Self::EmbedDesired=>"embed",Self::Rerank=>"rerank",Self::Ocr=>"ocr",Self::Stt=>"stt",Self::Tts=>"tts",Self::Verify|Self::VerifyClaims=>"verify" } }
    pub fn slot(self) -> &'static str { match self { Self::EmbedDesired=>"embed_desired",Self::VerifyClaims=>"verify_claims",_=>self.name() } }
    pub fn purpose(self) -> CallPurpose { match self { Self::Embed|Self::EmbedDesired=>CallPurpose::Embed,Self::Rerank=>CallPurpose::Rerank,Self::Ocr=>CallPurpose::Ocr,Self::Stt=>CallPurpose::Stt,Self::Tts=>CallPurpose::Tts,Self::Verify|Self::VerifyClaims=>CallPurpose::Verify } }
    fn parse(value: &str) -> Result<Self> { match value {
        "embed"=>Ok(Self::Embed),"rerank"=>Ok(Self::Rerank),"ocr"=>Ok(Self::Ocr),"stt"=>Ok(Self::Stt),"tts"=>Ok(Self::Tts),"verify"=>Ok(Self::Verify),
        _=>Err(RoutingError::Invalid("unknown fixed inference role").into()),
    } }
}

#[derive(Debug,Clone,PartialEq,Eq,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixedReference {
    pub provider_id: Uuid, pub provider_revision: i64, pub profile_revision: i64,
    pub role: FixedRole, pub model: String,
    pub active_index_revision: Option<i64>,
    #[serde(default)] pub desired_index_revision: Option<i64>,
}
#[derive(Debug,Clone,Copy,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum OcrPdfMode { Document, PageImages }

#[derive(Debug,Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixedProfile {
    pub provider_id: Uuid, pub provider_revision: i64, pub revision: i64,
    pub role: FixedRole, pub model: String,
    pub active_index_revision: Option<i64>,
    #[serde(default)] pub desired_index_revision: Option<i64>,
    #[serde(default)] pub locality: Locality,
    #[serde(default)] pub locality_trusted: bool,
    #[serde(default,skip_serializing_if="Option::is_none")] pub ocr_pdf_mode:Option<OcrPdfMode>,
}
impl FixedProfile {
    pub fn reference(&self) -> FixedReference {
        FixedReference { provider_id:self.provider_id,provider_revision:self.provider_revision,profile_revision:self.revision,
            role:self.role,model:self.model.clone(),active_index_revision:self.active_index_revision,desired_index_revision:self.desired_index_revision }
    }
}

#[derive(Clone,Serialize)]
pub struct FixedTarget {
    pub schema_version: u32,
    #[serde(flatten)] pub reference: FixedReference,
    pub base_url: String, pub api_key: String,
    pub embedding_dimension: Option<i32>, pub embedding_collection: Option<String>,
    #[serde(skip_serializing_if="Option::is_none")] pub ocr_pdf_mode:Option<OcrPdfMode>,
}
impl std::fmt::Debug for FixedTarget {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FixedTarget").field("provider_id",&self.reference.provider_id).field("role",&self.reference.role).finish_non_exhaustive()
    }
}

#[derive(sqlx::FromRow)]
struct Metadata {
    id: Uuid, scope: String, scope_id: Option<Uuid>, enabled: bool,
    model: Option<String>, base_url: Option<String>, routing_revision:i64,
    profile_revision:Option<i64>, profile:Option<serde_json::Value>,
}
#[derive(sqlx::FromRow)]
struct ActiveIndex { embed_model:String,embed_base_url:Option<String>,routing_revision:i64,dim:i32,collection_name:String }
async fn active_index(state: &AppState) -> Result<Option<ActiveIndex>> {
    Ok(sqlx::query_as("SELECT embed_model,embed_base_url,routing_revision,dim,collection_name FROM embedding_index WHERE id=1").fetch_optional(&state.pg).await?)
}

async fn desired_index(state:&AppState) -> Result<Option<ActiveIndex>> {
    let row:Option<(String,Option<String>,i64,i32,Option<String>)>=sqlx::query_as("SELECT desired_model,desired_base_url,desired_routing_revision,desired_dim,desired_collection_name FROM embedding_index WHERE id=1 AND desired_model IS NOT NULL AND desired_dim>0")
        .fetch_optional(&state.pg).await?;
    Ok(row.map(|(embed_model,embed_base_url,routing_revision,dim,collection)| {
        let collection_name=collection.unwrap_or_else(||desired_collection(dim,&embed_model));
        ActiveIndex { embed_model,embed_base_url,routing_revision,dim,collection_name }
    }))
}
/// Matches ML reindex.collection_name; the name is pinned before a job starts.
pub fn desired_collection(dim:i32,model:&str) -> String {
    let mut slug=String::new();
    for c in model.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() { slug.push(c); }
        else if !slug.is_empty() && !slug.ends_with('_') { slug.push('_'); }
    }
    let slug=slug.trim_end_matches('_');
    format!("pai_kb__{dim}__{}",if slug.is_empty() { "model" } else { slug })
}

#[derive(Serialize)]
pub struct FixedCatalogEntry {
    pub id:Uuid, pub role:FixedRole, pub scope:String, pub enabled:bool,
    pub model:Option<String>, pub provider_revision:i64, pub active_index_revision:Option<i64>,
    pub desired_index_revision:Option<i64>,
    pub profile_revision:Option<i64>, pub profile:Option<serde_json::Value>,
}
async fn metadata(state:&AppState,ctx:&AuthContext,slot:FixedRole) -> Result<Vec<Metadata>> {
    let user=if store::byok_enabled(state).await? { ctx.user_id } else { None };
    Ok(sqlx::query_as("SELECT p.id,p.role,p.scope,p.scope_id,p.enabled,p.model,p.base_url,p.routing_revision,f.revision AS profile_revision,f.profile FROM provider_configs p LEFT JOIN fixed_inference_profiles f ON f.provider_id=p.id AND f.slot=$2 WHERE p.role=$3 AND ((p.scope='deployment' AND p.scope_id IS NULL) OR (p.scope='user' AND p.scope_id=$1 AND p.role NOT IN ('embed','rerank','verify'))) ORDER BY (p.scope='user') DESC,p.id")
        .bind(user).bind(slot.slot()).bind(slot.name()).fetch_all(&state.pg).await?)
}
pub async fn catalog(state:&AppState,ctx:&AuthContext) -> Result<Vec<FixedCatalogEntry>> {
    let active=active_index(state).await?;
    let desired=desired_index(state).await?;
    let mut result=Vec::new();
    for role in [FixedRole::Embed,FixedRole::EmbedDesired,FixedRole::Rerank,FixedRole::Ocr,FixedRole::Stt,FixedRole::Tts,FixedRole::Verify,FixedRole::VerifyClaims] {
        for row in metadata(state,ctx,role).await? {
            let index=match role { FixedRole::Embed=>active.as_ref(),FixedRole::EmbedDesired=>desired.as_ref(),_=>None };
            let model=if role==FixedRole::VerifyClaims { row.profile.as_ref().and_then(|p|p.get("model")).and_then(|v|v.as_str()).map(str::to_owned) }
                else if role==FixedRole::EmbedDesired { index.map(|i|i.embed_model.clone()) }
                else { index.map(|i|i.embed_model.clone()).or(row.model) };
            result.push(FixedCatalogEntry { id:row.id,role,scope:row.scope,enabled:row.enabled,model,provider_revision:row.routing_revision,
                active_index_revision:index.filter(|_|role==FixedRole::Embed).map(|i|i.routing_revision),
                desired_index_revision:index.filter(|_|role==FixedRole::EmbedDesired).map(|i|i.routing_revision),profile_revision:row.profile_revision,profile:row.profile });
        }
    }
    Ok(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveFixedProfile { pub expected_revision:i64,pub profile:FixedProfile }
pub async fn save_profile(state:&AppState,ctx:&AuthContext,id:Uuid,mut body:SaveFixedProfile) -> Result<FixedProfile> {
    if body.profile.ocr_pdf_mode.is_some() && body.profile.role!=FixedRole::Ocr {
        return Err(RoutingError::Invalid("PDF input mode applies only to OCR").into());
    }
    if body.expected_revision<0 || body.profile.provider_id!=id || body.profile.model.trim().is_empty() || body.profile.model.len()>256 {
        return Err(RoutingError::Invalid("fixed profile identity or model").into());
    }
    let admin=state.rbac.has_permission(&state.pg,ctx,permissions::PROVIDERS_MANAGE).await?;
    let mut tx=state.pg.begin().await?;
    let locked:Option<(String,Option<Uuid>,String,i64,Option<String>)>=sqlx::query_as("SELECT scope,scope_id,role,routing_revision,model FROM provider_configs WHERE id=$1 FOR UPDATE")
        .bind(id).fetch_optional(&mut *tx).await?;
    let (scope,owner,provider_role,revision,model)=locked.ok_or(RoutingError::ProviderUnavailable)?;
    let role=body.profile.role;
    if role.name()!=provider_role { return Err(RoutingError::Invalid("fixed stage/provider role mismatch").into()); }
    if scope=="deployment" && owner.is_none() {
        if !admin { return Err(RoutingError::Forbidden.into()); }
    } else if scope!="user" || owner.is_none() || owner!=ctx.user_id || crate::providers::DEPLOYMENT_WIDE.contains(&role.name()) || !store::byok_enabled(state).await? {
        return Err(RoutingError::Forbidden.into());
    }
    if body.profile.locality_trusted && !admin { return Err(RoutingError::Forbidden.into()); }
    let active:Option<(String,i64)>=if role==FixedRole::Embed {
        sqlx::query_as("SELECT embed_model,routing_revision FROM embedding_index WHERE id=1 FOR SHARE").fetch_optional(&mut *tx).await?
    } else if role==FixedRole::EmbedDesired {
        Some(sqlx::query_as("SELECT desired_model,desired_routing_revision FROM embedding_index WHERE id=1 AND desired_model IS NOT NULL AND desired_dim>0 FOR SHARE").fetch_optional(&mut *tx).await?.ok_or(RoutingError::NeedsReplan)?)
    } else { None };
    // Claims use a separately attested primary model at the verifier endpoint;
    // ML requires that endpoint to acknowledge the exact model before success.
    let effective_model=if role==FixedRole::VerifyClaims { Some(&body.profile.model) } else { active.as_ref().map(|a|&a.0).or(model.as_ref()) };
    if body.profile.role!=role || body.profile.provider_revision!=revision || effective_model!=Some(&body.profile.model)
        || body.profile.active_index_revision!=active.as_ref().filter(|_|role==FixedRole::Embed).map(|a|a.1)
        || body.profile.desired_index_revision!=active.as_ref().filter(|_|role==FixedRole::EmbedDesired).map(|a|a.1) {
        return Err(RoutingError::NeedsReplan.into());
    }
    let existing:Option<i64>=sqlx::query_scalar("SELECT revision FROM fixed_inference_profiles WHERE provider_id=$1 AND slot=$2")
        .bind(id).bind(role.slot()).fetch_optional(&mut *tx).await?;
    if existing.unwrap_or(0)!=body.expected_revision { return Err(RoutingError::NeedsReplan.into()); }
    body.profile.revision=sqlx::query_scalar("SELECT nextval('routing_revision_generation')").fetch_one(&mut *tx).await?;
    sqlx::query("INSERT INTO fixed_inference_profiles(provider_id,revision,profile,updated_by,slot) VALUES ($1,$2,$3,$4,$5) ON CONFLICT(provider_id,slot) DO UPDATE SET revision=$2,profile=$3,updated_by=$4,updated_at=now()")
        .bind(id).bind(body.profile.revision).bind(serde_json::to_value(&body.profile).map_err(|_|RoutingError::Invalid("fixed profile"))?).bind(ctx.user_id).bind(role.slot()).execute(&mut *tx).await?;
    tx.commit().await?;
    store::audit(state,ctx,"routing.fixed_profile_updated",id,body.profile.revision).await;
    Ok(body.profile)
}

pub async fn resolve(
    state:&AppState, context:&RoutingContext, role:FixedRole, expected:Option<&FixedReference>, constraints:&RoutingConstraints,
) -> Result<FixedTarget> {
    if matches!(role,FixedRole::Embed|FixedRole::EmbedDesired) {crate::embedding_promotion::require_ready(&state.pg).await?;}
    let row=metadata(state,&context.actor,role).await?.into_iter().next().ok_or(RoutingError::ProviderUnavailable)?;
    if !row.enabled { return Err(RoutingError::ProviderUnavailable.into()); }
    let active=match role { FixedRole::Embed=>active_index(state).await?,FixedRole::EmbedDesired=>Some(desired_index(state).await?.ok_or(RoutingError::NeedsReplan)?),_=>None };
    let profile:FixedProfile=serde_json::from_value(row.profile.ok_or(RoutingError::NeedsReplan)?).map_err(|_|RoutingError::Invalid("stored fixed profile"))?;
    let model=if role==FixedRole::VerifyClaims { Some(profile.model.clone()) } else { active.as_ref().map(|a|a.embed_model.clone()).or(row.model.clone()) }
        .filter(|s|!s.trim().is_empty()).ok_or(RoutingError::Invalid("fixed model is required"))?;
    let base=active.as_ref().map(|a|a.embed_base_url.clone()).unwrap_or(row.base_url).filter(|s|!s.trim().is_empty()).ok_or(RoutingError::Invalid("fixed endpoint is required"))?;
    let index_revision=active.as_ref().map(|a|a.routing_revision);
    if profile.provider_id!=row.id || profile.role!=role || profile.provider_revision!=row.routing_revision
        || Some(profile.revision)!=row.profile_revision || profile.model!=model
        || profile.active_index_revision!=index_revision.filter(|_|role==FixedRole::Embed)
        || profile.desired_index_revision!=index_revision.filter(|_|role==FixedRole::EmbedDesired) {
        return Err(RoutingError::NeedsReplan.into());
    }
    let reference=profile.reference();
    if expected.is_some_and(|e|e!=&reference) { return Err(RoutingError::NeedsReplan.into()); }
    let mut purpose_context=context.clone(); purpose_context.purpose=role.purpose();
    let constraints=constraints.intersect(&store::deployment_constraints(state).await?)
        .intersect(&state.inference_policy.constraints(&state.pg,&purpose_context).await?);
    if !constraints.permits(row.id,if profile.locality_trusted { profile.locality } else { Locality::Unknown }) {
        return Err(RoutingError::Forbidden.into());
    }
    let provider=if let Some(revision)=index_revision {
        if role==FixedRole::EmbedDesired { state.providers.resolve_desired_embed(&state.pg,row.id,revision).await? }
        else { state.providers.resolve_active_embed(&state.pg,row.id,revision).await? }
    } else {
        let user=if crate::providers::DEPLOYMENT_WIDE.contains(&role.name()) { None }
            else if row.scope=="user" { row.scope_id } else { None };
        state.providers.resolve_fixed_id(&state.pg,role.name(),user,row.id).await?
    }.ok_or(RoutingError::ProviderUnavailable)?;
    let registry_model=if role==FixedRole::VerifyClaims { row.model.as_deref() } else { Some(model.as_str()) };
    if !provider.enabled || provider.model.as_deref()!=registry_model
        || provider.base_url.as_deref().map(|s|s.trim_end_matches('/'))!=Some(base.trim_end_matches('/')) {
        return Err(RoutingError::NeedsReplan.into());
    }
    let now:Option<(i64,i64)>=sqlx::query_as("SELECT p.routing_revision,f.revision FROM provider_configs p JOIN fixed_inference_profiles f ON f.provider_id=p.id AND f.slot=$2 WHERE p.id=$1")
        .bind(row.id).bind(role.slot()).fetch_optional(&state.pg).await?;
    if now!=Some((row.routing_revision,profile.revision)) { return Err(RoutingError::NeedsReplan.into()); }
    if role==FixedRole::Embed && active_index(state).await?.map(|a|a.routing_revision)!=index_revision { return Err(RoutingError::NeedsReplan.into()); }
    if role==FixedRole::EmbedDesired && desired_index(state).await?.map(|a|a.routing_revision)!=index_revision { return Err(RoutingError::NeedsReplan.into()); }
    Ok(FixedTarget { schema_version:SCHEMA_VERSION,reference,base_url:base,api_key:provider.api_key.unwrap_or_default(),
        embedding_dimension:active.as_ref().map(|a|a.dim),embedding_collection:active.map(|a|a.collection_name),ocr_pdf_mode:profile.ocr_pdf_mode })
}

pub async fn delete_profile(state:&AppState,ctx:&AuthContext,id:Uuid,expected_revision:i64) -> Result<()> {
    delete_profile_stage(state,ctx,id,expected_revision,None).await
}
pub async fn delete_profile_stage(state:&AppState,ctx:&AuthContext,id:Uuid,expected_revision:i64,stage:Option<FixedRole>) -> Result<()> {
    let admin=state.rbac.has_permission(&state.pg,ctx,permissions::PROVIDERS_MANAGE).await?;
    let mut tx=state.pg.begin().await?;
    let row:Option<(String,Option<Uuid>,String)>=sqlx::query_as("SELECT scope,scope_id,role FROM provider_configs WHERE id=$1 FOR UPDATE")
        .bind(id).fetch_optional(&mut *tx).await?;
    let (scope,owner,provider_role)=row.ok_or(RoutingError::ProviderUnavailable)?;
    let role=stage.unwrap_or(FixedRole::parse(&provider_role)?);
    if role.name()!=provider_role { return Err(RoutingError::Invalid("fixed stage/provider role mismatch").into()); }
    if scope=="deployment" && owner.is_none() {
        if !admin { return Err(RoutingError::Forbidden.into()); }
    } else if scope!="user" || owner.is_none() || owner!=ctx.user_id || crate::providers::DEPLOYMENT_WIDE.contains(&role.name()) || !store::byok_enabled(state).await? {
        return Err(RoutingError::Forbidden.into());
    }
    let deleted=sqlx::query("DELETE FROM fixed_inference_profiles WHERE provider_id=$1 AND revision=$2 AND slot=$3")
        .bind(id).bind(expected_revision).bind(role.slot()).execute(&mut *tx).await?;
    if deleted.rows_affected()!=1 { return Err(RoutingError::NeedsReplan.into()); }
    tx.commit().await?;
    store::audit(state,ctx,"routing.fixed_profile_deleted",id,expected_revision).await;
    Ok(())
}
