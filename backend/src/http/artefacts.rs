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

//! Generated-artefact REST. Artefacts are produced by
//! the `generate_artefact` tool inside a chat, stored chat-scoped on disk +
//! Postgres metadata, and downloaded here. No preview, no edit (a revision is a
//! follow-up prompt). Cascade-deleted with the chat; orphans swept by the
//! `ArtefactCleanup` background job.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::keycloak::AuthUser;
use crate::auth::rbac::{Permission, ResourceType};
use crate::auth::AuthContext;
use crate::error::{AppError, Result};
use crate::state::AppState;

/// Read access to a chat: its owner, an admin, or a project-read grant.
async fn require_chat_read(state: &AppState, ctx: &AuthContext, chat_id: Uuid) -> Result<()> {
    let row = sqlx::query!(
        "SELECT owner_user_id, project_id FROM chats WHERE id = $1",
        chat_id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::Validation("chat not found".into()))?;
    if ctx.user_id == Some(row.owner_user_id) || ctx.is_admin() {
        return Ok(());
    }
    if let Some(pid) = row.project_id {
        return state.rbac.require(&state.pg, ctx, ResourceType::Project, pid, Permission::Read).await;
    }
    Err(AppError::Forbidden("not permitted to access this chat".into()))
}

#[derive(Serialize)]
pub struct ArtefactOut {
    pub id: Uuid,
    pub kind: String,
    pub title: String,
    pub mime: String,
    /// The assistant message that produced it (null until the turn persists);
    /// the UI renders the artefact inline under that message.
    pub message_id: Option<Uuid>,
    /// The source chat's mode ("general"|"legal"|"research"), included on the list
    /// so the UI can offer "Create page" only on Deep Research reports. Omitted on
    /// the transient convert/create-page returns (the UI refetches the list).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_mode: Option<String>,
}

pub async fn list_artefacts(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(chat_id): Path<Uuid>,
) -> Result<Json<Vec<ArtefactOut>>> {
    require_chat_read(&state, &ctx, chat_id).await?;
    let rows = sqlx::query!(
        r#"SELECT g.id, g.kind::text AS "kind!", g.title, g.mime, g.message_id, c.mode AS "chat_mode!"
           FROM generated_artefacts g JOIN chats c ON c.id = g.chat_id
           WHERE g.chat_id = $1 ORDER BY g.created_at DESC"#,
        chat_id
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| ArtefactOut {
                id: r.id,
                kind: r.kind,
                title: r.title,
                mime: r.mime,
                message_id: r.message_id,
                chat_mode: Some(r.chat_mode),
            })
            .collect(),
    ))
}

#[derive(serde::Deserialize)]
pub struct ConvertIn {
    /// Target kind: "docx" | "pdf".
    pub to: String,
}

/// Convert a stored MARKDOWN artefact to DOCX/PDF on demand (the Deep Research
/// "Save as DOCX / PDF" buttons — generic for every md artefact). Dedupe: the
/// same conversion returns the existing artefact rather than minting another.
pub async fn convert_artefact(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(artefact_id): Path<Uuid>,
    Json(body): Json<ConvertIn>,
) -> Result<Json<ArtefactOut>> {
    let to = body.to.as_str();
    if !matches!(to, "docx" | "pdf") {
        return Err(AppError::Validation(format!("cannot convert to '{to}' — docx or pdf only")));
    }
    let a = sqlx::query!(
        "SELECT chat_id, turn_id, message_id, title, kind::text AS \"kind!\", disk_path FROM generated_artefacts WHERE id = $1",
        artefact_id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::Validation("artefact not found".into()))?;
    require_chat_read(&state, &ctx, a.chat_id).await?;
    if a.kind != "md" {
        return Err(AppError::Validation("only markdown artefacts can be converted".into()));
    }

    // Dedupe — an identical earlier conversion is returned as-is.
    if let Some(existing) = sqlx::query!(
        "SELECT id, kind::text AS \"kind!\", title, mime, message_id FROM generated_artefacts \
         WHERE chat_id = $1 AND title = $2 AND kind = ($3::text)::artefact_kind \
           AND message_id IS NOT DISTINCT FROM $4 \
         ORDER BY created_at DESC LIMIT 1",
        a.chat_id, a.title, to, a.message_id,
    )
    .fetch_optional(&state.pg)
    .await?
    {
        return Ok(Json(ArtefactOut {
            id: existing.id,
            kind: existing.kind,
            title: existing.title,
            mime: existing.mime,
            message_id: existing.message_id,
            chat_mode: None,
        }));
    }

    let src_abs = crate::storage::resolve_file(&state.boot.storage.artefacts_dir, &a.disk_path);
    let safe = crate::upload::ensure_within_storage(&state.boot.storage.artefacts_dir, &src_abs.to_string_lossy())?;
    let md = tokio::fs::read_to_string(&safe)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("read artefact: {e}")))?;

    let new_id = Uuid::now_v7();
    // Store the RELATIVE suffix; resolve for the ML call only.
    let rel = format!("{}/{new_id}.{to}", a.chat_id);
    let out_path = crate::storage::resolve_file(&state.boot.storage.artefacts_dir, &rel)
        .to_string_lossy()
        .to_string();
    let (_path, mime) = crate::ml::generate_artefact(
        &state.http, &state.boot.ml.base_url, to, &a.title, &md, &out_path,
    )
    .await?;

    sqlx::query!(
        "INSERT INTO generated_artefacts (id, chat_id, turn_id, message_id, kind, title, disk_path, mime, created_by) \
         VALUES ($1, $2, $3, $4, ($5::text)::artefact_kind, $6, $7, $8, $9)",
        new_id, a.chat_id, a.turn_id, a.message_id, to, a.title, rel, mime, ctx.user_id,
    )
    .execute(&state.pg)
    .await?;

    let mut ev = AuditEvent::action("artefact.converted", ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("artefact".into());
    ev.resource_id = Some(new_id);
    ev.payload = Some(serde_json::json!({ "from": artefact_id, "to": to, "chat_id": a.chat_id }));
    let _ = audit::append(&state.pg, &ev).await;

    Ok(Json(ArtefactOut {
        id: new_id,
        kind: to.to_string(),
        title: a.title,
        mime,
        message_id: a.message_id,
        chat_mode: None,
    }))
}

/// Turn a Deep Research report (a Markdown artefact in a `research`-mode chat) into
/// a self-contained HTML page — the "Create page" button. **Deterministic skill
/// injection**: the `report-to-page` skill body is
/// the system prompt (we never rely on the model matching a description); the LLM
/// writes the HTML; the ML html path inlines the vendored libraries + injects the
/// CSP + validates. Dedupe: the same page is returned rather than minting another.
pub async fn create_page(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(artefact_id): Path<Uuid>,
) -> Result<Json<ArtefactOut>> {
    let a = sqlx::query!(
        "SELECT g.chat_id, g.turn_id, g.message_id, g.title, g.kind::text AS \"kind!\", g.disk_path, c.mode \
         FROM generated_artefacts g JOIN chats c ON c.id = g.chat_id WHERE g.id = $1",
        artefact_id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::Validation("artefact not found".into()))?;
    require_chat_read(&state, &ctx, a.chat_id).await?;
    if a.kind != "md" {
        return Err(AppError::Validation("only markdown reports can become a page".into()));
    }
    if a.mode != "research" {
        return Err(AppError::Validation(
            "Create page is available on Deep Research reports".into(),
        ));
    }

    // Dedupe — an identical earlier page is returned as-is.
    if let Some(existing) = sqlx::query!(
        "SELECT id, kind::text AS \"kind!\", title, mime, message_id FROM generated_artefacts \
         WHERE chat_id = $1 AND title = $2 AND kind = 'html'::artefact_kind \
           AND message_id IS NOT DISTINCT FROM $3 \
         ORDER BY created_at DESC LIMIT 1",
        a.chat_id, a.title, a.message_id,
    )
    .fetch_optional(&state.pg)
    .await?
    {
        return Ok(Json(ArtefactOut {
            id: existing.id,
            kind: existing.kind,
            title: existing.title,
            mime: existing.mime,
            message_id: existing.message_id,
            chat_mode: None,
        }));
    }

    let src_abs = crate::storage::resolve_file(&state.boot.storage.artefacts_dir, &a.disk_path);
    let safe = crate::upload::ensure_within_storage(&state.boot.storage.artefacts_dir, &src_abs.to_string_lossy())?;
    let md = tokio::fs::read_to_string(&safe)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("read artefact: {e}")))?;

    // Deterministic injection: the report-to-page skill body is the system prompt.
    let skill_rel: String =
        sqlx::query_scalar!("SELECT disk_path FROM skills WHERE slug = 'report-to-page'")
            .fetch_optional(&state.pg)
            .await?
            .ok_or_else(|| AppError::Unavailable("the report-to-page skill is not installed".into()))?;
    let skill_dir = crate::storage::resolve_file(&state.boot.storage.skills_dir, &skill_rel);
    let system = crate::http::skills::read_skill_body(&skill_dir.to_string_lossy()).await?;
    let messages = vec![
        serde_json::json!({ "role": "system", "content": system }),
        serde_json::json!({ "role": "user", "content": md }),
    ];
    // A full page needs a generous ceiling (well above the chat default).
    let sampling = crate::ml::Sampling {
        temperature: Some(0.0),
        max_tokens: Some(8192),
        ..Default::default()
    };
    let step =
        crate::ml::chat_step(&state.http, &state.boot.ml.base_url, &messages, None, &sampling, crate::ml::provider_overrides(&state, ctx.user_id).await).await?;
    let html = step.content;
    if html.trim().is_empty() {
        return Err(AppError::Other(anyhow::anyhow!("report-to-page produced no HTML")));
    }

    let new_id = Uuid::now_v7();
    // Store the RELATIVE suffix; resolve for the ML call only.
    let rel = format!("{}/{new_id}.html", a.chat_id);
    let out_path = crate::storage::resolve_file(&state.boot.storage.artefacts_dir, &rel)
        .to_string_lossy()
        .to_string();
    let (_path, mime) = crate::ml::generate_artefact(
        &state.http, &state.boot.ml.base_url, "html", &a.title, &html, &out_path,
    )
    .await?;

    sqlx::query!(
        "INSERT INTO generated_artefacts (id, chat_id, turn_id, message_id, kind, title, disk_path, mime, created_by) \
         VALUES ($1, $2, $3, $4, 'html'::artefact_kind, $5, $6, $7, $8)",
        new_id, a.chat_id, a.turn_id, a.message_id, a.title, rel, mime, ctx.user_id,
    )
    .execute(&state.pg)
    .await?;

    let mut ev = AuditEvent::action("artefact.page_created", ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("artefact".into());
    ev.resource_id = Some(new_id);
    ev.payload = Some(serde_json::json!({ "from": artefact_id, "chat_id": a.chat_id }));
    let _ = audit::append(&state.pg, &ev).await;

    Ok(Json(ArtefactOut {
        id: new_id,
        kind: "html".to_string(),
        title: a.title,
        mime,
        message_id: a.message_id,
        chat_mode: None,
    }))
}

pub async fn download_artefact(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(artefact_id): Path<Uuid>,
) -> Result<Response> {
    let a = sqlx::query!(
        "SELECT chat_id, title, kind::text AS \"kind!\", disk_path, mime FROM generated_artefacts WHERE id = $1",
        artefact_id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::Validation("artefact not found".into()))?;
    require_chat_read(&state, &ctx, a.chat_id).await?;

    let src_abs = crate::storage::resolve_file(&state.boot.storage.artefacts_dir, &a.disk_path);
    let safe = crate::upload::ensure_within_storage(&state.boot.storage.artefacts_dir, &src_abs.to_string_lossy())?;
    let bytes = tokio::fs::read(&safe)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("read artefact: {e}")))?;

    let mut ev = AuditEvent::action("artefact.downloaded", ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("artefact".into());
    ev.resource_id = Some(artefact_id);
    let _ = audit::append(&state.pg, &ev).await;

    let filename = format!("{}.{}", a.title.replace(['"', '/', '\\'], "_"), a.kind);
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, a.mime),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{filename}\"")),
        ],
        Body::from(bytes),
    )
        .into_response())
}
