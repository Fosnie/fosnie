// Copyright 2026 Private AI Ltd (SC881079)
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Runtime provider selection (open-core seam) — the Core
//! [`ext::ProviderRegistry`]. Reads `provider_configs` (deployment- and, later,
//! user-scoped) and hands the backend a [`ResolvedProvider`] per role, which is
//! injected into the ML request's override channel. An empty table ⇒ every
//! `resolve` returns `None` ⇒ the ML service keeps its `.env` defaults.

use uuid::Uuid;

use crate::error::Result;
use crate::ext::{self, ResolvedProvider};

/// The seven inference roles a provider can be configured for. Used to validate
/// admin input and to drive the per-request override map.
pub const ROLES: [&str; 7] = ["llm", "embed", "rerank", "ocr", "stt", "tts", "verify"];

/// Roles configured deployment-wide only — never per-user. `embed` shares one
/// vector index; `rerank`/`verify` are platform-quality
/// knobs, not personal preference, so a stray per-user override is a footgun.
/// Per-user writes are rejected and `resolve` ignores any legacy user-scope row.
pub const DEPLOYMENT_WIDE: [&str; 3] = ["embed", "rerank", "verify"];

/// Core default registry: `provider_configs` with precedence user → deployment →
/// `None`. Holds the at-rest key (the deployment `message_encryption_key`) to
/// decrypt stored API keys; without a key, ciphertext keys resolve to `None`.
pub struct DbProviderRegistry {
    key: Option<[u8; 32]>,
}

impl DbProviderRegistry {
    pub fn new(key: Option<[u8; 32]>) -> Self {
        Self { key }
    }
}

#[async_trait::async_trait]
impl ext::ProviderRegistry for DbProviderRegistry {
    async fn resolve(
        &self,
        pool: &sqlx::PgPool,
        role: &str,
        user_id: Option<Uuid>,
    ) -> Result<Option<ResolvedProvider>> {
        if role == "llm" {
            return resolve_db_llm(pool, self.key, user_id, None, None).await;
        }
        // Pick the user row over the deployment row when both exist. A NULL
        // `user_id` never matches the user branch, so deployment wins.
        // Deployment-wide roles ignore any (legacy) user-scope row entirely.
        let user_id = if DEPLOYMENT_WIDE.contains(&role) { None } else { user_id };
        let row = sqlx::query!(
            r#"SELECT base_url, model, api_key_encrypted, enabled, reasoning_mode
               FROM provider_configs
               WHERE role = $1
                 AND ( (scope = 'user' AND scope_id = $2)
                    OR (scope = 'deployment' AND scope_id IS NULL) )
               ORDER BY (scope = 'user') DESC
               LIMIT 1"#,
            role,
            user_id,
        )
        .fetch_optional(pool)
        .await?;

        let Some(row) = row else { return Ok(None) };

        let api_key = decrypt_api_key(row.api_key_encrypted, self.key)?;

        Ok(Some(ResolvedProvider {
            base_url: row.base_url,
            model: row.model,
            api_key,
            enabled: row.enabled,
            reasoning_mode: row.reasoning_mode,
        }))
    }

    async fn resolve_llm_id(&self, pool: &sqlx::PgPool, user_id: Option<Uuid>, provider_id: Uuid) -> Result<Option<ResolvedProvider>> {
        fetch_visible_llm(pool, self.key, user_id, provider_id).await
    }

    async fn resolve_active_embed(&self,pool:&sqlx::PgPool,provider_id:Uuid,index_revision:i64) -> Result<Option<ResolvedProvider>> {
        let row:Option<(String,Option<String>,Option<String>)>=sqlx::query_as("SELECT e.embed_model,e.embed_base_url,e.embed_api_key_encrypted FROM embedding_index e JOIN provider_configs p ON p.id=$1 AND p.role='embed' AND p.scope='deployment' AND p.scope_id IS NULL AND p.enabled WHERE e.id=1 AND e.routing_revision=$2")
            .bind(provider_id).bind(index_revision).fetch_optional(pool).await?;
        let Some((model,base_url,encrypted))=row else { return Ok(None); };
        Ok(Some(ResolvedProvider { base_url,model:Some(model),api_key:decrypt_api_key(encrypted,self.key)?,enabled:true,reasoning_mode:None }))
    }

    async fn resolve_desired_embed(&self,pool:&sqlx::PgPool,provider_id:Uuid,index_revision:i64) -> Result<Option<ResolvedProvider>> {
        let row:Option<(String,Option<String>,Option<String>)>=sqlx::query_as("SELECT e.desired_model,e.desired_base_url,e.desired_api_key_encrypted FROM embedding_index e JOIN provider_configs p ON p.id=$1 AND p.role='embed' AND p.scope='deployment' AND p.scope_id IS NULL AND p.enabled WHERE e.id=1 AND e.desired_routing_revision=$2 AND e.desired_model IS NOT NULL AND e.desired_dim>0")
            .bind(provider_id).bind(index_revision).fetch_optional(pool).await?;
        let Some((model,base_url,encrypted))=row else { return Ok(None); };
        Ok(Some(ResolvedProvider { base_url,model:Some(model),api_key:decrypt_api_key(encrypted,self.key)?,enabled:true,reasoning_mode:None }))
    }

    async fn resolve_fixed_id(&self,pool:&sqlx::PgPool,role:&str,user_id:Option<Uuid>,provider_id:Uuid) -> Result<Option<ResolvedProvider>> {
        if role=="llm" || !ROLES.contains(&role) { return Err(crate::AppError::Validation("invalid fixed inference role".into())); }
        let user=if DEPLOYMENT_WIDE.contains(&role) { None } else { user_id };
        let row:Option<(Option<String>,Option<String>,Option<String>)>=sqlx::query_as("SELECT base_url,model,api_key_encrypted FROM provider_configs WHERE id=$1 AND role=$2 AND enabled AND ((scope='deployment' AND scope_id IS NULL) OR (scope='user' AND scope_id=$3))")
            .bind(provider_id).bind(role).bind(user).fetch_optional(pool).await?;
        let Some((base_url,model,encrypted))=row else { return Ok(None); };
        Ok(Some(ResolvedProvider { base_url,model,api_key:decrypt_api_key(encrypted,self.key)?,enabled:true,reasoning_mode:None }))
    }
}

// --- Multiple named LLM providers (phase 1: llm role only) -------------------
//
// The `llm` role can hold several named `provider_configs` rows per scope (mig
// 0091). A chat remembers which one it uses (`chats.llm_provider_id`); a per-turn
// composer pick can override for that turn. These free functions do their own
// query + decrypt (parallel to [`DbProviderRegistry::resolve`], which stays the
// single-row seam for the other six roles). They gate decryption on `key`
// (`AppState.message_key`) exactly as the registry does. Org "allowed-providers"
// policy (Enterprise) is a later phase — these read `provider_configs` directly.

/// Decrypt a stored provider api_key ciphertext, gated on the deployment key being
/// present. Mirrors [`DbProviderRegistry::resolve`]'s decrypt arm.
fn decrypt_api_key(ct: Option<String>, key: Option<[u8; 32]>) -> Result<Option<String>> {
    match (ct, key) {
        (Some(ct), Some(_)) => crate::crypto::decrypt_at_rest(&ct).map(Some)
            .map_err(|_| crate::AppError::Unavailable("provider credentials could not be decrypted".into())),
        (Some(_), None) => Err(crate::AppError::Unavailable("provider credentials require an encryption key".into())),
        (None, _) => Ok(None),
    }
}

/// Fetch a single VISIBLE `llm` provider row by id → [`ResolvedProvider`]. Visible
/// = an ENABLED `llm` row that is either deployment-scoped or owned by `user_id`.
/// `None` = not found / not owned / disabled.
async fn fetch_visible_llm(
    pool: &sqlx::PgPool,
    key: Option<[u8; 32]>,
    user_id: Option<Uuid>,
    id: Uuid,
) -> Result<Option<ResolvedProvider>> {
    let row = sqlx::query!(
        r#"SELECT base_url, model, api_key_encrypted, enabled, reasoning_mode
           FROM provider_configs
           WHERE id = $1 AND role = 'llm' AND enabled
             AND ( scope = 'deployment'
                OR (scope = 'user' AND scope_id = $2) )"#,
        id,
        user_id,
    )
    .fetch_optional(pool)
    .await?;
    row.map(|r| Ok(ResolvedProvider {
        base_url: r.base_url,
        model: r.model,
        api_key: decrypt_api_key(r.api_key_encrypted, key)?,
        enabled: r.enabled,
        reasoning_mode: r.reasoning_mode,
    })).transpose()
}

/// Is `id` a visible `llm` provider for `user_id`? (Cheap existence check for
/// persist/validation paths that don't need the decrypted row.)
pub async fn visible_llm(pool: &sqlx::PgPool, user_id: Option<Uuid>, id: Uuid) -> Result<bool> {
    let exists = sqlx::query_scalar!(
        r#"SELECT EXISTS(
             SELECT 1 FROM provider_configs
             WHERE id = $1 AND role = 'llm' AND enabled
               AND ( scope = 'deployment'
                  OR (scope = 'user' AND scope_id = $2) )
           ) AS "e!""#,
        id,
        user_id,
    )
    .fetch_one(pool)
    .await?;
    Ok(exists)
}

/// Resolve the effective `llm` provider for a chat turn (or the whoami probe).
/// Precedence: an explicit per-turn `requested_id` (when visible) → the chat's
/// remembered `llm_provider_id` (when still visible) → the caller's own default
/// llm row, else the deployment default → `None` (⇒ ML keeps its `.env` default).
/// Never errors on a stale pick; a deleted/disabled row simply falls through.
async fn resolve_db_llm(
    pool: &sqlx::PgPool,
    key: Option<[u8; 32]>,
    user_id: Option<Uuid>,
    chat_id: Option<Uuid>,
    requested_id: Option<Uuid>,
) -> Result<Option<ResolvedProvider>> {
    // 1. Explicit per-turn request (composer pick) wins when visible.
    if let Some(id) = requested_id {
        if let Some(p) = fetch_visible_llm(pool, key, user_id, id).await? {
            return Ok(Some(p));
        }
    }
    // 2. The chat's remembered provider, when still visible.
    if let Some(cid) = chat_id {
        let stored: Option<Uuid> = sqlx::query_scalar!(
            "SELECT llm_provider_id FROM chats WHERE id = $1",
            cid,
        )
        .fetch_optional(pool)
        .await?
        .flatten();
        if let Some(id) = stored {
            if let Some(p) = fetch_visible_llm(pool, key, user_id, id).await? {
                return Ok(Some(p));
            }
        }
    }
    // 3. The default llm row: the caller's own default wins over the deployment
    //    default (preserves per-user BYOK precedence when the user hasn't picked).
    let row = sqlx::query!(
        r#"SELECT base_url, model, api_key_encrypted, enabled, reasoning_mode
           FROM provider_configs
           WHERE role = 'llm' AND enabled AND is_default
             AND ( scope = 'deployment'
                OR (scope = 'user' AND scope_id = $1) )
           ORDER BY (scope = 'user') DESC
           LIMIT 1"#,
        user_id,
    )
    .fetch_optional(pool)
    .await?;
    if let Some(r) = row {
        return Ok(Some(ResolvedProvider {
            base_url: r.base_url,
            model: r.model,
            api_key: decrypt_api_key(r.api_key_encrypted, key)?,
            enabled: r.enabled,
            reasoning_mode: r.reasoning_mode,
        }));
    }
    // 4. Nothing configured ⇒ the ML service uses its own `.env` default.
    Ok(None)
}

/// Single entry for named/default selection. Every target is resolved through
/// the injected registry, including a remembered chat provider.
pub async fn resolve_llm(
    state: &crate::AppState,
    user_id: Option<Uuid>,
    chat_id: Option<Uuid>,
    requested_id: Option<Uuid>,
) -> Result<Option<ResolvedProvider>> {
    crate::model_routing::admission::require_legacy_chat(state, chat_id).await?;
    if let Some(id) = requested_id {
        return state.providers.resolve_llm_id(&state.pg, user_id, id).await?
            .map(Some).ok_or_else(|| crate::AppError::NotFound("model is unavailable to this caller".into()));
    }
    if let Some(chat_id) = chat_id {
        let stored = sqlx::query_scalar!("SELECT llm_provider_id FROM chats WHERE id = $1", chat_id)
            .fetch_optional(&state.pg).await?.flatten();
        if let Some(id) = stored {
            if let Some(provider) = state.providers.resolve_llm_id(&state.pg, user_id, id).await? {
                return Ok(Some(provider));
            }
            tracing::info!(%chat_id, "remembered model unavailable; resolving the legacy default");
        }
    }
    state.providers.resolve(&state.pg, "llm", user_id).await
}
