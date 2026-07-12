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

//! Provenance of the active embedding index. The single
//! `embedding_index` row records the model/dim/collection that the LIVE vectors
//! were actually built with, so retrieval + ingest embed with a model consistent
//! with the index — independent of the "desired" `provider_configs` embed row,
//! which only names the migration target. A model change stages `desired_*` + a
//! warn-gate; the blue-green re-index job rebuilds into a new collection and only
//! then promotes desired → active (atomic alias swap on the Qdrant side).

use uuid::Uuid;

use crate::error::Result;

/// The default shared KB collection name (also the Qdrant alias after the first
/// re-index). Kept here so the provenance seed and the ML side agree.
pub const DEFAULT_COLLECTION: &str = "pai_kb";

/// The resolved active-index embed config (key decrypted) that retrieval + ingest
/// must use so queries match the index.
#[derive(Debug, Clone)]
pub struct ActiveEmbed {
    pub model: String,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub dim: i32,
    pub collection_name: String,
    /// True while a re-index is in flight (the orchestrator dual-writes new ingests).
    pub reindexing: bool,
}

/// The full provenance row (status surface for the admin UI; keys never exposed).
#[derive(Debug, Clone)]
pub struct Provenance {
    pub embed_model: String,
    pub embed_base_url: Option<String>,
    pub dim: i32,
    pub collection_name: String,
    pub status: String,
    pub reindex_done: i32,
    pub reindex_total: i32,
    pub error: Option<String>,
    pub desired_model: Option<String>,
    pub desired_base_url: Option<String>,
    pub desired_dim: Option<i32>,
    pub api_key_set: bool,
    pub desired_api_key_set: bool,
}

fn decrypt_key(key: Option<[u8; 32]>, ct: Option<String>) -> Option<String> {
    match (ct, key) {
        (Some(ct), Some(_k)) => crate::crypto::decrypt_at_rest(&ct).ok(),
        _ => None,
    }
}

/// Read the provenance row for the admin UI (no secrets).
pub async fn get(pg: &sqlx::PgPool) -> Result<Option<Provenance>> {
    let row = sqlx::query!(
        r#"SELECT embed_model, embed_base_url, dim, collection_name, status,
                  reindex_done, reindex_total, error,
                  desired_model, desired_base_url, desired_dim,
                  (embed_api_key_encrypted IS NOT NULL) AS "api_key_set!",
                  (desired_api_key_encrypted IS NOT NULL) AS "desired_api_key_set!"
           FROM embedding_index WHERE id = 1"#
    )
    .fetch_optional(pg)
    .await?;
    Ok(row.map(|r| Provenance {
        embed_model: r.embed_model,
        embed_base_url: r.embed_base_url,
        dim: r.dim,
        collection_name: r.collection_name,
        status: r.status,
        reindex_done: r.reindex_done,
        reindex_total: r.reindex_total,
        error: r.error,
        desired_model: r.desired_model,
        desired_base_url: r.desired_base_url,
        desired_dim: r.desired_dim,
        api_key_set: r.api_key_set,
        desired_api_key_set: r.desired_api_key_set,
    }))
}

/// The active-index embed config (decrypted). `None` until the index is seeded —
/// callers then fall back to the `provider_configs` embed override (legacy path),
/// so an un-seeded deployment behaves exactly as before.
pub async fn active(pg: &sqlx::PgPool, message_key: Option<[u8; 32]>) -> Result<Option<ActiveEmbed>> {
    let row = sqlx::query!(
        r#"SELECT embed_model, embed_base_url, embed_api_key_encrypted, dim,
                  collection_name, status
           FROM embedding_index WHERE id = 1"#
    )
    .fetch_optional(pg)
    .await?;
    Ok(row.map(|r| ActiveEmbed {
        model: r.embed_model,
        base_url: r.embed_base_url,
        api_key: decrypt_key(message_key, r.embed_api_key_encrypted),
        dim: r.dim,
        collection_name: r.collection_name,
        reindexing: r.status == "reindexing",
    }))
}

/// The staged desired-target embed config (decrypted), or `None` if nothing is
/// pending. Drives the re-index job.
pub async fn desired(pg: &sqlx::PgPool, message_key: Option<[u8; 32]>) -> Result<Option<ActiveEmbed>> {
    let row = sqlx::query!(
        r#"SELECT desired_model, desired_base_url, desired_api_key_encrypted,
                  desired_dim, desired_collection_name
           FROM embedding_index WHERE id = 1 AND desired_model IS NOT NULL AND desired_dim IS NOT NULL"#
    )
    .fetch_optional(pg)
    .await?;
    Ok(row.map(|r| ActiveEmbed {
        model: r.desired_model.unwrap_or_default(),
        base_url: r.desired_base_url,
        api_key: decrypt_key(message_key, r.desired_api_key_encrypted),
        dim: r.desired_dim.unwrap_or(0),
        collection_name: r.desired_collection_name.unwrap_or_else(|| DEFAULT_COLLECTION.to_string()),
        reindexing: false,
    }))
}

/// Seed the active index on first creation (idempotent — does nothing if a row
/// exists). Called when the first KB is built, capturing the model+dim that
/// actually produced the live `pai_kb` collection.
pub async fn seed_if_absent(
    pg: &sqlx::PgPool,
    message_key: Option<[u8; 32]>,
    model: &str,
    base_url: Option<&str>,
    api_key_plain: Option<&str>,
    dim: i32,
) -> Result<()> {
    let enc = match (api_key_plain.filter(|k| !k.is_empty()), message_key) {
        (Some(k), Some(_mk)) => Some(crate::crypto::encrypt_at_rest(k)?),
        _ => None,
    };
    sqlx::query!(
        r#"INSERT INTO embedding_index
               (id, embed_model, embed_base_url, embed_api_key_encrypted, dim, collection_name, status)
           VALUES (1, $1, $2, $3, $4, $5, 'active')
           ON CONFLICT (id) DO NOTHING"#,
        model,
        base_url,
        enc,
        dim,
        DEFAULT_COLLECTION,
    )
    .execute(pg)
    .await?;
    Ok(())
}

/// Stage the desired embed target (the admin changed the embed model). Does NOT
/// touch the active index — a re-index promotes it later.
pub async fn set_desired(
    pg: &sqlx::PgPool,
    message_key: Option<[u8; 32]>,
    model: &str,
    base_url: Option<&str>,
    api_key_plain: Option<&str>,
    dim: i32,
    by: Option<Uuid>,
) -> Result<()> {
    let enc = match (api_key_plain.filter(|k| !k.is_empty()), message_key) {
        (Some(k), Some(_mk)) => Some(crate::crypto::encrypt_at_rest(k)?),
        _ => None,
    };
    sqlx::query!(
        r#"UPDATE embedding_index
           SET desired_model = $1, desired_base_url = $2,
               desired_api_key_encrypted = COALESCE($3, desired_api_key_encrypted),
               desired_dim = $4, updated_by = $5, updated_at = now()
           WHERE id = 1"#,
        model,
        base_url,
        enc,
        dim,
        by,
    )
    .execute(pg)
    .await?;
    Ok(())
}

/// Mark the re-index started (status=reindexing, progress reset).
pub async fn begin_reindex(pg: &sqlx::PgPool, total: i32, by: Option<Uuid>) -> Result<()> {
    sqlx::query!(
        r#"UPDATE embedding_index
           SET status = 'reindexing', reindex_done = 0, reindex_total = $1,
               error = NULL, updated_by = $2, updated_at = now()
           WHERE id = 1"#,
        total,
        by,
    )
    .execute(pg)
    .await?;
    Ok(())
}

/// Update re-index progress (best-effort; drives the UI bar).
pub async fn set_progress(pg: &sqlx::PgPool, done: i32) -> Result<()> {
    sqlx::query!("UPDATE embedding_index SET reindex_done = $1, updated_at = now() WHERE id = 1", done)
        .execute(pg)
        .await?;
    Ok(())
}

/// Promote desired → active after a successful swap, recording the collection the
/// ML build actually created (from the build's `built` event), and clear desired.
pub async fn finish_reindex(pg: &sqlx::PgPool, new_collection: &str) -> Result<()> {
    sqlx::query!(
        r#"UPDATE embedding_index
           SET embed_model = COALESCE(desired_model, embed_model),
               embed_base_url = desired_base_url,
               embed_api_key_encrypted = desired_api_key_encrypted,
               dim = COALESCE(desired_dim, dim),
               collection_name = $1,
               status = 'active', error = NULL,
               reindex_done = reindex_total,
               desired_model = NULL, desired_base_url = NULL,
               desired_api_key_encrypted = NULL, desired_dim = NULL,
               desired_collection_name = NULL,
               built_at = now(), updated_at = now()
           WHERE id = 1"#,
        new_collection,
    )
    .execute(pg)
    .await?;
    Ok(())
}

/// Mark the re-index failed (alias untouched; old index stays live; Retry-able).
pub async fn fail_reindex(pg: &sqlx::PgPool, error: &str) -> Result<()> {
    sqlx::query!(
        "UPDATE embedding_index SET status = 'failed', error = $1, updated_at = now() WHERE id = 1",
        error,
    )
    .execute(pg)
    .await?;
    Ok(())
}
