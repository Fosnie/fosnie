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

//! Background-task scheduler.
//!
//! Runs on a **second tokio runtime** hosted on a dedicated OS thread so the
//! hot API/WebSocket path is never starved by background work. Drives the
//! durable Postgres `tasks` queue: event-triggered work is enqueued via
//! [`enqueue`]; periodic work (e.g. audit retention) is registered with
//! tokio-cron-scheduler. Claiming uses `FOR UPDATE SKIP LOCKED` so multiple
//! workers (now, or a separate process later) never double-process a task.
//!
//! Skeleton: handlers are no-ops that succeed; the retry/backoff/dead-letter
//! machinery is in place for when real handlers (ingestion, automations,
//! retention) arrive.

use std::time::Duration;

use sqlx::PgPool;
use time::OffsetDateTime;
use tokio::sync::watch;
use tokio_cron_scheduler::{Job, JobScheduler};
use uuid::Uuid;

use crate::config::SchedulerConfig;
use crate::state::AppState;

pub mod registry;
pub use registry::{CoreJobs, JobRegistry, TaskHandler};

/// Kind of durable task. Maps to the `task_type` Postgres enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "task_type", rename_all = "snake_case")]
pub enum TaskType {
    Ingest,
    AutomationRun,
    AuditRetention,
    ArtefactCleanup,
    TabularGenerate,
    Export,
    AgentResume,
    WorkflowRun,
    VerifyDraft,
    RepairRun,
    WebSearchDeep,
    DeepResearch,
    McpHealth,
    ReindexEmbeddings,
    ApiChatCleanup,
}

impl TaskType {
    /// The stable `task_type` string (matches the Postgres enum labels). Used for
    /// dispatch + metric labels now that the worker reads `task_type` as text.
    pub fn as_key(self) -> &'static str {
        match self {
            TaskType::Ingest => "ingest",
            TaskType::AutomationRun => "automation_run",
            TaskType::AuditRetention => "audit_retention",
            TaskType::ArtefactCleanup => "artefact_cleanup",
            TaskType::TabularGenerate => "tabular_generate",
            TaskType::Export => "export",
            TaskType::AgentResume => "agent_resume",
            TaskType::WorkflowRun => "workflow_run",
            TaskType::VerifyDraft => "verify_draft",
            TaskType::RepairRun => "repair_run",
            TaskType::WebSearchDeep => "web_search_deep",
            TaskType::DeepResearch => "deep_research",
            TaskType::McpHealth => "mcp_health",
            TaskType::ReindexEmbeddings => "reindex_embeddings",
            TaskType::ApiChatCleanup => "api_chat_cleanup",
        }
    }

    /// Parse a `task_type` string into a Core variant. `None` for a kind not known
    /// to Core (e.g. an Enterprise `audit_checkpoint`) — the worker then routes it
    /// through the [`registry::JobRegistry`] handlers.
    pub fn from_key(s: &str) -> Option<Self> {
        match s {
            "ingest" => Some(TaskType::Ingest),
            "automation_run" => Some(TaskType::AutomationRun),
            "audit_retention" => Some(TaskType::AuditRetention),
            "artefact_cleanup" => Some(TaskType::ArtefactCleanup),
            "tabular_generate" => Some(TaskType::TabularGenerate),
            "export" => Some(TaskType::Export),
            "agent_resume" => Some(TaskType::AgentResume),
            "workflow_run" => Some(TaskType::WorkflowRun),
            "verify_draft" => Some(TaskType::VerifyDraft),
            "repair_run" => Some(TaskType::RepairRun),
            "web_search_deep" => Some(TaskType::WebSearchDeep),
            "deep_research" => Some(TaskType::DeepResearch),
            "mcp_health" => Some(TaskType::McpHealth),
            "reindex_embeddings" => Some(TaskType::ReindexEmbeddings),
            "api_chat_cleanup" => Some(TaskType::ApiChatCleanup),
            _ => None,
        }
    }
}

/// Spawn the background runtime on its own thread. Returns its join handle;
/// signal shutdown by sending `true` on the watch channel.
pub fn spawn(
    state: AppState,
    cfg: SchedulerConfig,
    shutdown: watch::Receiver<bool>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("pai-bg-runtime".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(cfg.worker_threads.max(1))
                .thread_name("pai-bg")
                .enable_all()
                .build()
                .expect("build background runtime");
            rt.block_on(async move {
                if let Err(e) = run(state, cfg, shutdown).await {
                    tracing::error!(error = %e, "scheduler terminated with error");
                }
            });
        })
        .expect("spawn background runtime thread")
}

/// Enqueue a durable task. Safe to call from the hot path.
pub async fn enqueue(
    pool: &PgPool,
    task_type: TaskType,
    payload: serde_json::Value,
) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO tasks (id, task_type, payload) VALUES ($1, $2, $3)",
        id,
        task_type as TaskType,
        payload,
    )
    .execute(pool)
    .await?;
    Ok(id)
}

/// Enqueue a durable task by its `task_type` *string* — for kinds not in the Core
/// [`TaskType`] enum (e.g. an Enterprise-registered `audit_checkpoint`). The DB
/// `task_type` enum still defines the label; the text is cast to it.
pub async fn enqueue_key(
    pool: &PgPool,
    task_key: &str,
    payload: serde_json::Value,
) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO tasks (id, task_type, payload) VALUES ($1, ($2::text)::task_type, $3)",
        id,
        task_key,
        payload,
    )
    .execute(pool)
    .await?;
    Ok(id)
}

async fn run(
    state: AppState,
    cfg: SchedulerConfig,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    // Periodic jobs + extra task handlers are assembled by the JobRegistrar seam
    // (Core default registers the host set; Enterprise overrides it).
    let mut registry = JobRegistry::default();
    state.jobs.register(&mut registry);

    let mut sched = JobScheduler::new().await?;
    for spec in &registry.periodic {
        let run = spec.run.clone();
        let st = state.clone();
        let job = Job::new_async(spec.cron.as_str(), move |_uuid, _l| {
            let run = run.clone();
            let st = st.clone();
            Box::pin(async move { run(st).await })
        })?;
        sched.add(job).await?;
    }
    sched.start().await?;

    let mut ticker = tokio::time::interval(Duration::from_secs(cfg.poll_interval_secs.max(1)));
    tracing::info!(
        poll_interval_secs = cfg.poll_interval_secs,
        batch_size = cfg.batch_size,
        "background scheduler started"
    );

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                // Enqueue any due automations (durable tasks), then process the queue.
                if let Err(e) = scan_due_automations(&state).await {
                    tracing::error!(error = %e, "automation scan failed");
                }
                // Lookahead reminders to owners for soon-to-run automations.
                if let Err(e) = scan_reminders(&state).await {
                    tracing::error!(error = %e, "automation reminder scan failed");
                }
                // Relay any undispatched domain events to matching workflows
                // (no-op while features.workflows is off / paused).
                match crate::workflows::dispatch_once(&state).await {
                    Ok(0) => {}
                    Ok(n) => tracing::debug!(dispatched = n, "workflow runs enqueued"),
                    Err(e) => tracing::error!(error = %e, "workflow dispatch failed"),
                }
                // Fire coalescing buffers whose window has closed (one batched run).
                match crate::workflows::scan_coalesced(&state).await {
                    Ok(0) => {}
                    Ok(n) => tracing::debug!(fired = n, "coalesced workflow runs enqueued"),
                    Err(e) => tracing::error!(error = %e, "workflow coalesce scan failed"),
                }
                match poll_once(&state, &registry, cfg.batch_size).await {
                    Ok(0) => tracing::trace!("scheduler idle tick"),
                    Ok(n) => tracing::debug!(processed = n, "scheduler processed tasks"),
                    Err(e) => tracing::error!(error = %e, "scheduler poll failed"),
                }
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    tracing::info!("background scheduler shutting down");
                    break;
                }
            }
        }
    }

    let _ = sched.shutdown().await;
    Ok(())
}

/// Claim a batch of due tasks, mark them running, then process each.
async fn poll_once(state: &AppState, registry: &JobRegistry, batch: i64) -> Result<u64, sqlx::Error> {
    let pool = &state.pg;
    let mut tx = pool.begin().await?;

    let claimed = sqlx::query!(
        r#"
        SELECT id,
               task_type::text AS "task_type!",
               payload,
               retry_count,
               max_retries
        FROM tasks
        WHERE status = 'queued' AND next_attempt_at <= now()
        ORDER BY priority, next_attempt_at
        LIMIT $1
        FOR UPDATE SKIP LOCKED
        "#,
        batch
    )
    .fetch_all(&mut *tx)
    .await?;

    if claimed.is_empty() {
        tx.commit().await?;
        return Ok(0);
    }

    let ids: Vec<Uuid> = claimed.iter().map(|r| r.id).collect();
    sqlx::query!(
        "UPDATE tasks SET status = 'running', started_at = now() WHERE id = ANY($1)",
        &ids
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let mut processed = 0u64;
    for row in claimed {
        let ttype = row.task_type.clone();
        // Known Core kind → the hard-coded handler; otherwise an Enterprise kind
        // registered in the JobRegistry; otherwise dead-letter (never panic).
        let outcome = match TaskType::from_key(&row.task_type) {
            Some(tt) => handle(state, tt, &row.payload).await,
            None => match registry.task_handler(&row.task_type) {
                Some(h) => h.handle(state, &row.payload).await,
                None => {
                    tracing::warn!(task_type = %row.task_type, "no handler for task_type; dead-lettering");
                    Err(format!("no handler for task_type '{}'", row.task_type))
                }
            },
        };
        match outcome {
            Ok(()) => {
                sqlx::query!(
                    "UPDATE tasks SET status = 'succeeded', finished_at = now(), last_error = NULL WHERE id = $1",
                    row.id
                )
                .execute(pool)
                .await?;
                metrics::counter!("task_runs_total", "type" => ttype.clone(), "outcome" => "succeeded").increment(1);
            }
            Err(err) => {
                let attempt = row.retry_count + 1;
                if attempt >= row.max_retries {
                    sqlx::query!(
                        "UPDATE tasks SET status = 'dead_letter', retry_count = $2, last_error = $3, finished_at = now() WHERE id = $1",
                        row.id,
                        attempt,
                        err
                    )
                    .execute(pool)
                    .await?;
                    metrics::counter!("task_runs_total", "type" => ttype.clone(), "outcome" => "dead_letter").increment(1);
                } else {
                    metrics::counter!("task_runs_total", "type" => ttype.clone(), "outcome" => "retry").increment(1);
                    let next = OffsetDateTime::now_utc()
                        + Duration::from_secs(backoff_secs(attempt));
                    sqlx::query!(
                        "UPDATE tasks SET status = 'queued', retry_count = $2, last_error = $3, next_attempt_at = $4 WHERE id = $1",
                        row.id,
                        attempt,
                        err,
                        next
                    )
                    .execute(pool)
                    .await?;
                }
            }
        }
        processed += 1;
    }

    Ok(processed)
}

/// Dispatch a task to its handler. `Ingest` runs the RAG pipeline via Python;
/// other types are still no-ops (automation runs, retention, artefact cleanup).
async fn handle(
    state: &AppState,
    task_type: TaskType,
    payload: &serde_json::Value,
) -> Result<(), String> {
    match task_type {
        TaskType::Ingest => {
            let doc_id = payload
                .get("doc_id")
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or_else(|| "ingest task: missing/invalid doc_id".to_string())?;
            match run_ingest(state, doc_id).await {
                Ok(chunks) => {
                    tracing::info!(%doc_id, chunks, "document ingested");
                    Ok(())
                }
                Err(e) => {
                    // Surface the failure on the document + knowledge base, and
                    // push an error frame to the uploader.
                    let _ = sqlx::query!(
                        "UPDATE kb_documents SET ingest_status = 'error' WHERE id = $1",
                        doc_id
                    )
                    .execute(&state.pg)
                    .await;
                    let _ = sqlx::query!(
                        "UPDATE knowledge_bases SET status = 'error' \
                         WHERE id = (SELECT kb_id FROM kb_documents WHERE id = $1)",
                        doc_id
                    )
                    .execute(&state.pg)
                    .await;
                    if let Ok(row) = sqlx::query!(
                        "SELECT created_by, kb_id FROM kb_documents WHERE id = $1",
                        doc_id
                    )
                    .fetch_one(&state.pg)
                    .await
                    {
                        emit_ingest_status(state, row.created_by, doc_id, row.kb_id, "error", Some(e.to_string()));
                    }
                    Err(e.to_string())
                }
            }
        }
        TaskType::AgentResume => {
            // Durable resume of an approved agent run (the human approved while no
            // live socket waiter was present). Execute the approved action verbatim,
            // idempotently; the kill-switch / cancellation is respected inside.
            let run_id = payload
                .get("run_id")
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or_else(|| "agent_resume: missing/invalid run_id".to_string())?;
            crate::agent::execute_approved(state, run_id).await.map_err(|e| e.to_string())
        }
        TaskType::TabularGenerate => {
            let review_id = payload
                .get("review_id")
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or_else(|| "tabular task: missing/invalid review_id".to_string())?;
            // Optional cell filter for re-run: [{document_id, column_key}, …].
            let only: Option<Vec<(Uuid, String)>> = payload.get("only").and_then(|v| v.as_array()).map(|arr| {
                arr.iter()
                    .filter_map(|c| {
                        let d = c.get("document_id")?.as_str().and_then(|s| Uuid::parse_str(s).ok())?;
                        let k = c.get("column_key")?.as_str()?.to_string();
                        Some((d, k))
                    })
                    .collect()
            });
            match run_tabular_generate(state, review_id, only).await {
                Ok(()) => Ok(()),
                Err(e) => {
                    let _ = sqlx::query!(
                        "UPDATE tabular_reviews SET status = 'error' WHERE id = $1",
                        review_id
                    )
                    .execute(&state.pg)
                    .await;
                    Err(e.to_string())
                }
            }
        }
        TaskType::ArtefactCleanup => {
            // Also prune orphan chat attachments (uploaded but never sent) older than 24h.
            match crate::http::chat_attachments::prune_orphans(state, time::Duration::hours(24)).await {
                Ok(n) if n > 0 => tracing::info!(removed = n, "orphan chat attachments pruned"),
                Ok(_) => {}
                Err(e) => tracing::error!(error = %e, "chat-attachment prune failed"),
            }
            match run_artefact_cleanup(state).await {
                Ok(n) => {
                    tracing::info!(removed = n, "orphaned artefacts cleaned");
                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            }
        }
        TaskType::ApiChatCleanup => match run_api_chat_cleanup(state).await {
            Ok(n) => {
                if n > 0 {
                    tracing::info!(removed = n, "aged API conversations pruned");
                }
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        },
        TaskType::AuditRetention => {
            match run_audit_retention(state).await {
                Ok(n) => {
                    tracing::info!(dropped = n, "audit retention swept");
                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            }
        }
        TaskType::McpHealth => {
            match crate::mcp::health_sweep(state).await {
                Ok(n) => {
                    tracing::debug!(checked = n, "mcp health sweep");
                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            }
        }
        TaskType::AutomationRun => {
            let automation_id = payload
                .get("automation_id")
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or_else(|| "automation task: missing/invalid automation_id".to_string())?;
            run_automation(state, automation_id).await.map_err(|e| e.to_string())
        }
        TaskType::Export => {
            let export_id = payload
                .get("export_id")
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or_else(|| "export task: missing/invalid export_id".to_string())?;
            crate::http::export::run_export(state, export_id).await.map_err(|e| e.to_string())
        }
        TaskType::WorkflowRun => {
            // An event-driven workflow firing. Executes its action (system_action
            // this slice) under the durable-task retry/backoff/dead-letter path.
            let run_id = payload
                .get("workflow_run_id")
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or_else(|| "workflow_run task: missing/invalid workflow_run_id".to_string())?;
            crate::workflows::run(state, run_id).await.map_err(|e| e.to_string())
        }
        TaskType::VerifyDraft => {
            // A "Verify draft" (groundedness Mode B) job: decompose the target
            // document into claims and verify each against the caller's sources.
            // Terminal — the run row carries success/error; payload has
            // {run_id, path, mime, kb_ids} resolved at request time.
            crate::groundedness::verify_draft(state, payload).await.map_err(|e| e.to_string())
        }
        TaskType::RepairRun => {
            // Ground-or-cut repair of a finished verify-draft run on a
            // document: regenerate/cut each flagged claim, re-verify the new
            // citation, and surface results as tracked-change proposals. Payload
            // is {run_id}.
            crate::groundedness::repair_run(state, payload).await.map_err(|e| e.to_string())
        }
        TaskType::WebSearchDeep => {
            // A `depth=deep` web search: run the
            // exhaustive, politely-paced loop with no tool-timeout pressure and
            // post the digest + citations back into the chat. Terminal — the
            // failure path posts an honest message and returns Ok (no retry, lest
            // a re-run double-post). Payload: {run_id?, chat_id, turn_id, user_id,
            // role, query, recency}.
            crate::web_search::run_deep(state, payload).await.map_err(|e| e.to_string())
        }
        TaskType::DeepResearch => {
            // A Deep Research run: the ML synthesis
            // pipeline streams progress and a final report, posted back into
            // the research chat with citations + an MD artefact. Terminal —
            // the failure path posts an honest message and returns Ok (no
            // retry, lest a re-run double-post). Payload: {run_id?, chat_id,
            // turn_id, user_id, role, question, source, kb_ids, refinements,
            // template?, template_spec?}.
            crate::research::run_research(state, payload).await.map_err(|e| e.to_string())
        }
        TaskType::ReindexEmbeddings => {
            // Blue-green embedding re-index: re-embed every
            // chunk from payload text into a new collection, then atomic alias swap.
            // On failure, mark the provenance `failed` (old index intact, Retry-able).
            match run_reindex(state).await {
                Ok(()) => Ok(()),
                Err(e) => {
                    let _ = crate::embedding_index::fail_reindex(&state.pg, &e).await;
                    Err(e)
                }
            }
        }
    }
}

/// Drive the blue-green re-index: stream the ML build (updating progress in the
/// provenance row), then swap the alias and promote desired → active.
async fn run_reindex(state: &AppState) -> Result<(), String> {
    use futures_util::StreamExt;

    let key = state.message_key;
    let d = crate::embedding_index::desired(&state.pg, key)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "reindex: no desired embed target staged".to_string())?;

    crate::embedding_index::begin_reindex(&state.pg, 0, None).await.map_err(|e| e.to_string())?;

    let url = format!("{}/reindex-embeddings", state.boot.ml.base_url.trim_end_matches('/'));
    let resp = state
        .http
        .post(url)
        .json(&serde_json::json!({
            "new_dim": d.dim,
            "new_model": d.model,
            "new_base_url": d.base_url,
            "new_api_key": d.api_key,
        }))
        .send()
        .await
        .map_err(|e| format!("ml /reindex-embeddings: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("ml /reindex-embeddings returned {}", resp.status()));
    }

    // Parse the NDJSON progress stream → update provenance; capture the built names.
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut new_collection: Option<String> = None;
    let mut old_collection: Option<String> = None;
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| format!("reindex stream: {e}"))?;
        buf.extend_from_slice(&bytes);
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = buf.drain(..=pos).collect();
            let line = &line[..line.len() - 1];
            if line.is_empty() {
                continue;
            }
            let Ok(ev) = serde_json::from_slice::<serde_json::Value>(line) else { continue };
            match ev.get("type").and_then(|v| v.as_str()) {
                Some("start") => {
                    let total = ev.get("total").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                    let _ = crate::embedding_index::begin_reindex(&state.pg, total, None).await;
                }
                Some("progress") => {
                    let done = ev.get("done").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                    let _ = crate::embedding_index::set_progress(&state.pg, done).await;
                }
                Some("built") => {
                    new_collection = ev.get("new_collection").and_then(|v| v.as_str()).map(String::from);
                    old_collection = ev.get("old_collection").and_then(|v| v.as_str()).map(String::from);
                }
                Some("error") => {
                    return Err(ev.get("message").and_then(|v| v.as_str()).unwrap_or("reindex error").to_string());
                }
                _ => {}
            }
        }
    }
    let new_c = new_collection.ok_or_else(|| "reindex did not complete (no built event)".to_string())?;

    // Atomic alias swap + provenance promotion (adjacent so they can't diverge).
    let swap_url = format!("{}/swap-embedding-alias", state.boot.ml.base_url.trim_end_matches('/'));
    let swap = state
        .http
        .post(swap_url)
        .json(&serde_json::json!({ "new_collection": new_c, "old_collection": old_collection }))
        .send()
        .await
        .map_err(|e| format!("ml /swap-embedding-alias: {e}"))?;
    if !swap.status().is_success() {
        return Err(format!("ml /swap-embedding-alias returned {}", swap.status()));
    }
    crate::embedding_index::finish_reindex(&state.pg, &new_c).await.map_err(|e| e.to_string())?;
    tracing::info!(collection = %new_c, "embedding re-index complete; alias swapped");
    Ok(())
}

/// Delete generated-artefact files + rows whose chat is archived or gone
/// (orphan cleanup). Hard-deleted chats already
/// cascade; this sweeps archived ones.
async fn run_artefact_cleanup(state: &AppState) -> Result<u64, crate::error::AppError> {
    let orphans = sqlx::query!(
        r#"SELECT ga.id, ga.disk_path AS "disk_path!"
           FROM generated_artefacts ga
           LEFT JOIN chats c ON c.id = ga.chat_id
           WHERE c.id IS NULL OR c.archived_at IS NOT NULL"#
    )
    .fetch_all(&state.pg)
    .await?;
    let mut removed = 0u64;
    for o in orphans {
        let abs = crate::storage::resolve_file(&state.boot.storage.artefacts_dir, &o.disk_path);
        let _ = tokio::fs::remove_file(&abs).await;
        sqlx::query!("DELETE FROM generated_artefacts WHERE id = $1", o.id)
            .execute(&state.pg)
            .await?;
        removed += 1;
    }
    Ok(removed)
}

/// Delete conversations created by external applications once they are older
/// than the configured retention.
///
/// Off by default (`api.chat_retention_days` = 0 keeps them indefinitely): the
/// conversations are the caller's own record, so discarding them is a choice an
/// operator makes rather than one made for them. When it is set, this is the
/// only sweep that touches them — they are invisible in the chat lists, so
/// nobody would ever tidy them by hand.
async fn run_api_chat_cleanup(state: &AppState) -> Result<u64, crate::error::AppError> {
    let days = crate::config::runtime::get(&state.pg, "api.chat_retention_days")
        .await
        .ok()
        .flatten()
        .and_then(|e| e.value.parse::<i64>().ok())
        .unwrap_or(0);
    if days <= 0 {
        return Ok(0);
    }
    let cutoff = time::OffsetDateTime::now_utc() - time::Duration::days(days);
    // Messages, artefacts and the rest cascade from the conversation row.
    let n = sqlx::query!(
        "DELETE FROM chats WHERE origin = 'api' AND created_at < $1",
        cutoff
    )
    .execute(&state.pg)
    .await?
    .rows_affected();
    Ok(n)
}

/// Generate every (document × column) cell for a review via the Python pool,
/// persisting + broadcasting each cell as it streams back.
async fn run_tabular_generate(
    state: &AppState,
    review_id: Uuid,
    only: Option<Vec<(Uuid, String)>>,
) -> Result<(), crate::error::AppError> {
    use crate::error::AppError;

    let review = sqlx::query!(
        "SELECT created_by, columns_config FROM tabular_reviews WHERE id = $1",
        review_id
    )
    .fetch_one(&state.pg)
    .await?;
    let owner = review.created_by;

    // Columns from the stored config (key, format, prompt, mechanism).
    let mut columns: Vec<crate::ml::ReviewColumn> = review
        .columns_config
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|c| {
                    Some(crate::ml::ReviewColumn {
                        key: c.get("key")?.as_str()?.to_string(),
                        format: c.get("format").and_then(|v| v.as_str()).unwrap_or("text").to_string(),
                        prompt: c.get("prompt").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                        mechanism: c.get("mechanism").and_then(|v| v.as_str()).unwrap_or("stuff").to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if columns.is_empty() {
        return Err(AppError::Validation("review has no columns".into()));
    }

    // Documents under review → their current version (path + version_id for
    // version-pinned citations).
    let doc_ids: Vec<Uuid> = sqlx::query_scalar!(
        "SELECT document_id FROM tabular_review_documents WHERE review_id = $1 ORDER BY position",
        review_id
    )
    .fetch_all(&state.pg)
    .await?;
    let mut documents = Vec::with_capacity(doc_ids.len());
    let mut version_of: std::collections::HashMap<Uuid, Uuid> = std::collections::HashMap::new();
    for doc_id in doc_ids {
        let cur = crate::documents::current_version(&state.pg, &state.boot.storage.workspace_dir, doc_id).await?;
        version_of.insert(doc_id, cur.version_id);
        documents.push(crate::ml::ReviewDoc {
            document_id: doc_id,
            path: cur.bytes_path,
            mime: cur.mime,
        });
    }
    if documents.is_empty() {
        return Err(AppError::Validation("review has no documents".into()));
    }

    // Re-run filter: restrict to the requested (document, column) cells.
    if let Some(only) = &only {
        let docs: std::collections::HashSet<Uuid> = only.iter().map(|(d, _)| *d).collect();
        let cols: std::collections::HashSet<&str> = only.iter().map(|(_, k)| k.as_str()).collect();
        documents.retain(|d| docs.contains(&d.document_id));
        columns.retain(|c| cols.contains(c.key.as_str()));
        if documents.is_empty() || columns.is_empty() {
            return Ok(());
        }
    }

    let mut stream =
        crate::ml::generate_review(&state.http, &state.boot.ml.base_url, &documents, &columns, crate::ml::provider_overrides(state, None).await).await?;

    while let Some(ev) = stream.recv().await {
        // Cell-level interrupt: a cancel request stops the run between cells.
        if state.cancellations.take(review_id) {
            drop(stream); // aborts the reader → cancels the Python pool
            sqlx::query!("UPDATE tabular_reviews SET status = 'cancelled' WHERE id = $1", review_id)
                .execute(&state.pg)
                .await?;
            if let Some(uid) = owner {
                state.hub.send_to_user(uid, crate::ws::protocol::ServerFrame::TabularReviewComplete { review_id });
            }
            return Ok(());
        }
        match ev {
            crate::ml::ReviewEvent::Cell {
                document_id,
                column_key,
                status,
                value,
                reasoning,
                mut citations,
                error,
            } => {
                // Version-pin the citations against the cited document's current
                // version (legal-workspace citations carry version_id).
                if let (Some(arr), Some(vid)) = (citations.as_array_mut(), version_of.get(&document_id)) {
                    for c in arr.iter_mut() {
                        if let Some(obj) = c.as_object_mut() {
                            obj.insert("version_id".into(), serde_json::json!(vid));
                        }
                    }
                }
                // The extract-failure event uses column_key "*" for the whole doc.
                if column_key == "*" {
                    sqlx::query!(
                        "UPDATE tabular_cells SET status = 'error', error = $3, updated_at = now() \
                         WHERE review_id = $1 AND document_id = $2",
                        review_id, document_id, error
                    )
                    .execute(&state.pg)
                    .await?;
                } else {
                    sqlx::query!(
                        "UPDATE tabular_cells \
                         SET status = ($4::text)::tabular_cell_status, value = $5, reasoning = $6, \
                             citations = $7, error = $8, updated_at = now() \
                         WHERE review_id = $1 AND document_id = $2 AND column_key = $3",
                        review_id,
                        document_id,
                        column_key,
                        status,
                        value,
                        reasoning,
                        citations,
                        error,
                    )
                    .execute(&state.pg)
                    .await?;
                }

                let mut event = crate::audit::AuditEvent::action("cell.generated", "system");
                event.actor_user_id = owner;
                event.resource_type = Some("tabular_review".into());
                event.resource_id = Some(review_id);
                event.payload = Some(serde_json::json!({
                    "document_id": document_id, "column_key": column_key, "status": status
                }));
                let _ = crate::audit::append(&state.pg, &event).await;

                if let Some(uid) = owner {
                    state.hub.send_to_user(
                        uid,
                        crate::ws::protocol::ServerFrame::TabularCellUpdated {
                            review_id,
                            document_id,
                            column_key,
                            status,
                        },
                    );
                }
            }
            crate::ml::ReviewEvent::Done => break,
            crate::ml::ReviewEvent::Error { message } => {
                return Err(AppError::Other(anyhow::anyhow!("review generation: {message}")));
            }
        }
    }

    sqlx::query!("UPDATE tabular_reviews SET status = 'done' WHERE id = $1", review_id)
        .execute(&state.pg)
        .await?;
    if let Some(uid) = owner {
        state
            .hub
            .send_to_user(uid, crate::ws::protocol::ServerFrame::TabularReviewComplete { review_id });
    }
    Ok(())
}

/// Best-effort live push of a document's ingest status to its uploader (the
/// `created_by`), so the Libraries / project view updates without polling.
/// Postgres remains the source of truth; a dropped frame is fine.
fn emit_ingest_status(
    state: &AppState,
    created_by: Option<Uuid>,
    doc_id: Uuid,
    kb_id: Uuid,
    status: &str,
    error: Option<String>,
) {
    if let Some(uid) = created_by {
        state.hub.send_to_user(
            uid,
            crate::ws::protocol::ServerFrame::IngestStatus {
                doc_id,
                kb_id,
                status: status.to_string(),
                error,
            },
        );
    }
}

/// Extract→chunk→embed→upsert one document via the Python ML service, updating
/// the doc + knowledge-base status as it goes (and pushing each transition to
/// the uploader over WebSocket).
async fn run_ingest(state: &AppState, doc_id: Uuid) -> Result<i64, crate::error::AppError> {
    let doc = sqlx::query!(
        r#"SELECT kd.bytes_path, kd.mime, kd.kb_id, kd.created_by, kd.original_filename,
                  kb.embedding_dimension, kb.origin_project_id, kb.parent_child
           FROM kb_documents kd
           JOIN knowledge_bases kb ON kb.id = kd.kb_id
           WHERE kd.id = $1"#,
        doc_id
    )
    .fetch_one(&state.pg)
    .await?;

    sqlx::query!("UPDATE kb_documents SET ingest_status = 'extracting' WHERE id = $1", doc_id)
        .execute(&state.pg)
        .await?;
    sqlx::query!(
        "UPDATE knowledge_bases SET status = 'indexing' WHERE id = $1",
        doc.kb_id
    )
    .execute(&state.pg)
    .await?;
    emit_ingest_status(state, doc.created_by, doc_id, doc.kb_id, "extracting", None);

    sqlx::query!("UPDATE kb_documents SET ingest_status = 'indexing' WHERE id = $1", doc_id)
        .execute(&state.pg)
        .await?;
    emit_ingest_status(state, doc.created_by, doc_id, doc.kb_id, "indexing", None);

    // Access is enforced at query time (the intersection allow-list) — chunks
    // carry only the immutable knowledge_base_id, no denormalised grants.
    // Chunking knobs come from the runtime config (super-admin panel); unset →
    // the ML service uses its own default.
    let read_int = |key: &'static str| async move {
        crate::config::runtime::get(&state.pg, key).await.ok().flatten().and_then(|e| e.value.parse::<i64>().ok())
    };
    // During a blue-green re-index, dual-write the doc into the rebuilt collection
    // with the NEW model too, so it lands in both indexes.
    let dual = match crate::embedding_index::active(&state.pg, state.message_key).await {
        Ok(Some(a)) if a.reindexing => crate::embedding_index::desired(&state.pg, state.message_key)
            .await
            .ok()
            .flatten()
            .map(|d| serde_json::json!({"dim": d.dim, "model": d.model, "base_url": d.base_url, "api_key": d.api_key})),
        _ => None,
    };
    let doc_abs = crate::storage::resolve_file(&state.boot.storage.documents_dir, &doc.bytes_path);
    let result = crate::ml::ingest(
        &state.http,
        &state.boot.ml.base_url,
        &doc_id.to_string(),
        &doc.kb_id.to_string(),
        &doc_abs.to_string_lossy(),
        doc.mime.as_deref(),
        doc.embedding_dimension,
        read_int("ingest.chunk_size").await,
        read_int("ingest.chunk_overlap").await,
        crate::config::runtime::get(&state.pg, "ingest.pdfplumber").await.ok().flatten().map(|e| e.value == "true"),
        // Resolve providers as the uploader; `provider_overrides` overlays the
        // ACTIVE embed model (provenance), so ingest is consistent with the index.
        crate::ml::provider_overrides(state, doc.created_by).await,
        dual,
        // Per-KB parent–child chunking. The column is NOT NULL,
        // so this is always a concrete choice for the KB.
        Some(doc.parent_child),
    )
    .await?;

    // The document's own effective date (best-effort), parsed from the ISO string
    // the ML pipeline returned. NULL when none was found → retrieval falls back to
    // the ingestion timestamp.
    let effective_date = result.effective_date.as_deref().and_then(parse_iso_date);
    // Mark ready and emit the `document.ingested` domain event atomically — the
    // event exists iff the ready-state commits (transactional outbox).
    // Human-originated: the uploader (`created_by`) initiated this work, so it can
    // trigger human-only workflows (e.g. "summarise every new matter document").
    let mut tx = state.pg.begin().await?;
    sqlx::query!(
        "UPDATE kb_documents SET ingest_status = 'ready', effective_date = $2 WHERE id = $1",
        doc_id,
        effective_date,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE knowledge_bases SET status = 'ready', last_ingest_at = now() WHERE id = $1",
        doc.kb_id
    )
    .execute(&mut *tx)
    .await?;
    let ev = crate::events::NewEvent::new(
        crate::events::DOCUMENT_INGESTED,
        crate::events::ActorType::Human,
    )
    .actor(doc.created_by)
    .resource("kb_document", doc_id)
    .project(doc.origin_project_id)
    .payload(serde_json::json!({
        "filename": doc.original_filename,
        "mime": doc.mime,
        "kb_id": doc.kb_id.to_string(),
        "chunks": result.chunks,
    }));
    crate::events::emit_with(&mut tx, &ev).await?;
    tx.commit().await?;
    emit_ingest_status(state, doc.created_by, doc_id, doc.kb_id, "ready", None);

    Ok(result.chunks)
}

/// Parse an ISO `YYYY-MM-DD` date into a UTC-midnight timestamp; `None` on any
/// malformed input (extraction is best-effort).
fn parse_iso_date(s: &str) -> Option<OffsetDateTime> {
    let mut it = s.splitn(3, '-');
    let y: i32 = it.next()?.trim().parse().ok()?;
    let m: u8 = it.next()?.trim().parse().ok()?;
    let d: u8 = it.next()?.trim().parse().ok()?;
    let month = time::Month::try_from(m).ok()?;
    let date = time::Date::from_calendar_date(y, month, d).ok()?;
    Some(date.midnight().assume_utc())
}

/// Drop audit partitions wholly older than the retention window.
/// A hold beats retention: if ANY legal hold is active, skip the sweep entirely
/// (conservative — never delete potentially-held evidence). Only oldest
/// (prefix) partitions are dropped, so the chain stays verifiable. Returns the
/// number of partitions dropped.
pub async fn run_audit_retention(state: &AppState) -> Result<u64, crate::error::AppError> {
    // Any active legal hold blocks the sweep (via the retention seam — `legal_holds`
    // is an enterprise concern; the Core default reads it as today).
    if state.retention.holds_active(state).await {
        tracing::info!("audit retention skipped: active legal hold(s) present");
        return Ok(0);
    }

    let months: i64 = crate::config::runtime::get(&state.pg, "audit.retention_months")
        .await
        .ok()
        .flatten()
        .and_then(|e| e.value.parse::<i64>().ok())
        .unwrap_or(24);
    // Cutoff date (ISO yyyy-mm-dd); ISO dates compare lexically.
    let cutoff = OffsetDateTime::now_utc() - time::Duration::days(months * 30);
    let cutoff_date = cutoff.date().to_string();

    // Child partitions of audit_events + their upper bound expression.
    let parts = sqlx::query!(
        r#"SELECT child.relname AS "name!",
                  pg_get_expr(child.relpartbound, child.oid) AS "bound!"
           FROM pg_inherits
           JOIN pg_class parent ON parent.oid = pg_inherits.inhparent
           JOIN pg_class child  ON child.oid  = pg_inherits.inhrelid
           WHERE parent.relname = 'audit_events'"#
    )
    .fetch_all(&state.pg)
    .await?;

    let mut dropped = 0u64;
    for p in parts {
        if p.bound.contains("DEFAULT") {
            continue;
        }
        // bound looks like: FOR VALUES FROM ('2026-04-01 …') TO ('2026-05-01 …')
        let Some(to_date) = p
            .bound
            .split("TO (")
            .nth(1)
            .and_then(|s| s.trim_start_matches(['\'', ' ']).get(0..10))
            .map(|s| s.to_string())
        else {
            continue;
        };
        if to_date > cutoff_date {
            continue; // partition's range extends to/after the cutoff — keep
        }
        // Identifier comes from pg_class (a real partition name) — audited safe.
        let sql = format!("DROP TABLE IF EXISTS \"{}\"", p.name);
        sqlx::query(sqlx::AssertSqlSafe(sql)).execute(&state.pg).await?;

        let mut ev = crate::audit::AuditEvent::action("audit.retention.dropped", "system");
        ev.resource_type = Some("audit_partition".into());
        ev.payload = Some(serde_json::json!({ "partition": p.name, "cutoff": cutoff_date }));
        let _ = crate::audit::append(&state.pg, &ev).await;
        dropped += 1;
    }

    // Per-record-class retention for the PII-bearing evidence (A2) runs via the
    // retention seam — `interaction_evidence` is an enterprise table;
    // the Core default prunes it on its own (shorter) window exactly as before.
    let _pruned = state.retention.prune_evidence(state).await;
    Ok(dropped)
}

/// Enqueue an `AutomationRun` for every active automation whose `next_run_at` is
/// due, then advance `next_run_at` to the following occurrence (or clear it when
/// the schedule has no future). Restart-safe — the schedule lives in the DB, not
/// in-memory cron jobs. Returns how many fired.
pub async fn scan_due_automations(state: &AppState) -> Result<u64, crate::error::AppError> {
    let due = sqlx::query!(
        "SELECT id, schedule FROM automations \
         WHERE status = 'active' AND next_run_at IS NOT NULL AND next_run_at <= now()"
    )
    .fetch_all(&state.pg)
    .await?;
    let now = OffsetDateTime::now_utc();
    let mut fired = 0u64;
    for a in due {
        enqueue(&state.pg, TaskType::AutomationRun, serde_json::json!({ "automation_id": a.id })).await?;
        let next = crate::automations::next_after(&a.schedule, now).ok().flatten();
        sqlx::query!("UPDATE automations SET next_run_at = $2 WHERE id = $1", a.id, next)
            .execute(&state.pg)
            .await?;
        fired += 1;
    }
    Ok(fired)
}

/// Lookahead reminders (Tier-2 #16): push a `automation.reminder` to the owner of
/// each active automation that is due within the lookahead window and has not yet
/// been reminded for this occurrence. Best-effort over the hub (Postgres remains
/// the source of truth); each occurrence is reminded at most once.
pub async fn scan_reminders(state: &AppState) -> Result<u64, crate::error::AppError> {
    use time::format_description::well_known::Rfc3339;
    let lookahead = reminder_lookahead_secs(state).await;
    let rows = sqlx::query!(
        r#"SELECT id, name, owner_user_id, next_run_at AS "next_run_at!"
           FROM automations
           WHERE status = 'active'
             AND next_run_at IS NOT NULL
             AND next_run_at > now()
             AND next_run_at <= now() + ($1::int * interval '1 second')
             AND (reminded_for IS NULL OR reminded_for <> next_run_at)"#,
        lookahead
    )
    .fetch_all(&state.pg)
    .await?;
    let now = OffsetDateTime::now_utc();
    let mut sent = 0u64;
    for r in rows {
        let in_seconds = (r.next_run_at - now).whole_seconds().max(0);
        state.hub.send_to_user(
            r.owner_user_id,
            crate::ws::protocol::ServerFrame::AutomationReminder {
                automation_id: r.id,
                name: r.name,
                due_at: r.next_run_at.format(&Rfc3339).unwrap_or_default(),
                in_seconds,
            },
        );
        sqlx::query!("UPDATE automations SET reminded_for = $2 WHERE id = $1", r.id, r.next_run_at)
            .execute(&state.pg)
            .await?;
        sent += 1;
    }
    Ok(sent)
}

/// Reminder lookahead window in seconds — a runtime-tunable
/// `automation.reminder_lookahead_secs` (default 600 = 10 min).
async fn reminder_lookahead_secs(state: &AppState) -> i32 {
    crate::config::runtime::get(&state.pg, "automation.reminder_lookahead_secs")
        .await
        .ok()
        .flatten()
        .and_then(|e| e.value.parse::<i32>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(600)
}

/// Run one automation: a headless chat against its Agent with its prompt, the
/// output persisted as a chat. Reuses `chat::run_turn` (driven through a
/// throwaway channel that captures the created chat id).
async fn run_automation(state: &AppState, automation_id: Uuid) -> Result<(), crate::error::AppError> {
    use crate::error::AppError;
    use crate::ws::protocol::ServerFrame;

    let a = sqlx::query!(
        "SELECT name, owner_user_id, agent_id, prompt, project_id, kb_ids, deliver_group_chat_id \
         FROM automations WHERE id = $1",
        automation_id
    )
    .fetch_one(&state.pg)
    .await?;
    let ctx = crate::auth::load_context(&state.pg, a.owner_user_id).await?;

    let run_id = Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO automation_runs (id, automation_id, status) VALUES ($1, $2, 'running')",
        run_id, automation_id
    )
    .execute(&state.pg)
    .await?;

    // Drive run_turn headless: a throwaway channel + a drain that captures the
    // created chat id and notes any error frame.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<ServerFrame>(64);
    let drain = tokio::spawn(async move {
        let mut chat_id: Option<Uuid> = None;
        let mut errored: Option<String> = None;
        while let Some(f) = rx.recv().await {
            match f {
                ServerFrame::ChatCreated { chat_id: c } => chat_id = Some(c),
                ServerFrame::ChatCompleted { chat_id: c, .. } => chat_id = Some(c),
                ServerFrame::ChatError { message, .. } => errored = Some(message),
                _ => {}
            }
        }
        (chat_id, errored)
    });

    let turn_id = Uuid::now_v7();
    let cancel = std::sync::Arc::new(tokio::sync::Notify::new());
    crate::chat::run_turn(
        state, &ctx, turn_id, None, a.project_id, a.agent_id, a.prompt, Vec::new(), a.kb_ids, true, None, None, None, None, &tx, cancel,
    )
    .await;
    drop(tx); // close the channel so the drain finishes
    let (chat_id, errored) = drain.await.unwrap_or((None, None));

    let now = OffsetDateTime::now_utc();
    if let Some(cid) = chat_id.filter(|_| errored.is_none()) {
        sqlx::query!(
            "UPDATE automation_runs SET status = 'succeeded', output_chat_id = $2, completed_at = $3 WHERE id = $1",
            run_id, cid, now
        )
        .execute(&state.pg)
        .await?;
        sqlx::query!("UPDATE automations SET last_run_at = $2 WHERE id = $1", automation_id, now)
            .execute(&state.pg)
            .await?;
        // Optional delivery: post a result notice into an internal group chat
        // (Teams) — in-platform, zero egress. Best-effort: a delivery failure
        // never fails the run. Gated on the owner still being a member.
        if let Some(gid) = a.deliver_group_chat_id {
            if let Err(e) = deliver_to_group(state, gid, a.owner_user_id, &a.name, cid).await {
                tracing::warn!(error = %e, automation = %automation_id, "automation delivery failed");
            }
        }
        audit_automation(state, &ctx, "automation.ran", automation_id, Some(serde_json::json!({ "output_chat_id": cid }))).await;
        Ok(())
    } else {
        let msg = errored.unwrap_or_else(|| "no chat produced".into());
        sqlx::query!(
            "UPDATE automation_runs SET status = 'failed', error = $2, completed_at = $3 WHERE id = $1",
            run_id, msg, now
        )
        .execute(&state.pg)
        .await?;
        audit_automation(state, &ctx, "automation.failed", automation_id, Some(serde_json::json!({ "error": msg }))).await;
        Err(AppError::Other(anyhow::anyhow!("automation run failed: {msg}")))
    }
}

/// Post a result notice for a finished automation into an internal group chat
/// (Teams). A one-line summary (the output's first assistant line, truncated) +
/// a `shared_resources` pointer to the output chat so the UI can offer "open".
/// Gated on `owner` still being a member of the target chat.
async fn deliver_to_group(
    state: &AppState,
    group_chat_id: Uuid,
    owner: Uuid,
    name: &str,
    output_chat_id: Uuid,
) -> Result<(), crate::error::AppError> {
    if !crate::http::messaging::is_member(state, owner, group_chat_id).await? {
        return Ok(()); // owner left the chat since configuring — skip silently
    }
    // Deliver a link, not a wall of text: record a chat_share so the group's
    // members can open the output chat, then post a one-line notice carrying the
    // chat reference in `shared_resources` (the UI renders an "open chat" link).
    sqlx::query!(
        "INSERT INTO chat_shares (chat_id, group_chat_id, shared_by) VALUES ($1, $2, $3) \
         ON CONFLICT (chat_id, group_chat_id) DO NOTHING",
        output_chat_id,
        group_chat_id,
        owner,
    )
    .execute(&state.pg)
    .await?;
    let content = format!("⚙ Automation “{name}” ran — read the output");
    crate::http::messaging::post_chat_link(state, group_chat_id, None, output_chat_id, &content, true).await?;
    Ok(())
}

async fn audit_automation(
    state: &AppState,
    ctx: &crate::auth::AuthContext,
    action: &str,
    automation_id: Uuid,
    payload: Option<serde_json::Value>,
) {
    let mut ev = crate::audit::AuditEvent::action(action, ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("automation".into());
    ev.resource_id = Some(automation_id);
    ev.payload = payload;
    let _ = crate::audit::append(&state.pg, &ev).await;
}

/// Exponential backoff in seconds, capped at five minutes.
fn backoff_secs(attempt: i32) -> u64 {
    let exp = 2u64.saturating_pow(attempt.max(0) as u32);
    exp.min(300)
}

