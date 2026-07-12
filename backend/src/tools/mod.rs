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

//! Closed registry of predefined tools. No admin-uploaded
//! code. Each tool has an OpenAI function-schema (the slot-[2] contribution,
//! passed via the `tools` param) and an executor. Dispatch is RBAC-aware and
//! routes to Rust (CRUD/system) or Python (document layer); future tools
//! (`code_interpreter`, `generate_artefact`, `edit_document`, DMS) plug in here.

pub mod custom;

use std::collections::HashMap;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::auth::AuthContext;
use crate::config::FeaturesConfig;
use crate::error::{AppError, Result};
use crate::integrations::{self, ConnectorKind};
use crate::state::AppState;
use crate::ws::protocol::ServerFrame;

/// All tool names the platform knows.
pub const ALL: &[&str] = &[
    "current_time",
    "list_documents",
    "read_document",
    "remember_fact",
    "list_workspace_documents",
    "edit_document",
    "read_table_cells",
    "generate_artefact",
    "read_skill",
    "web_search",
    "code_interpreter",
    "track_steps",
];

/// Baseline tools every Agent gets without enabling them (injected in `load_agent`).
///
/// Only `generate_artefact` — and it is never advertised to the LLM (filtered out in
/// the chat turn); it is the silent capability marker that lets the post-hoc drafter
/// fallback save a turn's answer as a downloadable document when the user asked for
/// one. The LLM-callable helpers (`current_time`, `track_steps`, `list_documents`)
/// are NO LONGER forced on every agent: a small model would call them on a plain
/// question and never answer. Agents that want them now opt in explicitly, so an
/// agent with no tools selected advertises nothing the model can call.
///
/// NOTE: `read_document` is deliberately NOT a default. On a large document it falls
/// into an exhaustive map-reduce over the WHOLE text (hundreds of LLM calls — minutes,
/// and it saturates the engine, knocking out concurrent retrieval). Agents should rely
/// on RAG retrieval (only the relevant chunks) instead, and opt into `read_document`
/// explicitly only where whole-document reading of small files is genuinely wanted.
pub const DEFAULT_TOOLS: &[&str] = &["generate_artefact"];

/// Where a tool executes / how long it may run. Per-type
/// timeouts are mandatory: a RAG / external / code call must error out, never
/// hang the turn forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolType {
    /// Cheap in-process CRUD / system call.
    System,
    /// Reads document text via the Python layer.
    DocumentRead,
    /// The agentic-RAG retrieve call (Python).
    Rag,
    /// Artefact generation (Python / soffice).
    Artefact,
    /// Memory write.
    Memory,
    /// External DMS connector (only when enabled).
    Dms,
    /// External web connector (only when enabled).
    Web,
    /// Code interpreter (Firecracker).
    Code,
}

impl ToolType {
    /// Stable key used for per-type timeout-override config lookups.
    pub fn as_str(self) -> &'static str {
        match self {
            ToolType::System => "system",
            ToolType::DocumentRead => "document_read",
            ToolType::Rag => "rag",
            ToolType::Artefact => "artefact",
            ToolType::Memory => "memory",
            ToolType::Dms => "dms",
            ToolType::Web => "web",
            ToolType::Code => "code",
        }
    }
}

/// The host capability a tool requires, if any. A capability-gated tool is only
/// advertised/dispatchable when its `features` flag is on — this is how the same
/// binary refuses to assume Firecracker on a host that cannot run it.
pub fn capability(name: &str) -> Option<&'static str> {
    match name {
        "code_interpreter" => Some("code_interpreter"),
        _ => None,
    }
}

/// Is this tool runnable on this host? True unless it needs a capability whose
/// feature flag is off.
pub fn host_enabled(name: &str, features: &FeaturesConfig) -> bool {
    match capability(name) {
        Some("code_interpreter") => features.code_interpreter,
        Some(_) => false,
        None => true,
    }
}

/// The execution type of a tool by name. Every entry in [`ALL`] maps to a type
/// (no silent default — a unit test enforces it).
pub fn tool_type(name: &str) -> ToolType {
    match name {
        "current_time" | "list_documents" | "list_workspace_documents" | "read_skill"
        | "track_steps" => ToolType::System,
        "read_document" | "edit_document" | "read_table_cells" => ToolType::DocumentRead,
        "remember_fact" => ToolType::Memory,
        "generate_artefact" => ToolType::Artefact,
        "web_search" => ToolType::Web,
        "code_interpreter" => ToolType::Code,
        // Defensive default for tools not in ALL; ALL is fully covered by the test.
        _ => ToolType::System,
    }
}

/// What a tool does to STATE — orthogonal to [`egress`] (which crosses the
/// perimeter). A tool can be both (a future `send_email` is write + egress).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolEffect {
    /// Reads only; no state mutation. Safe to auto-run when not egress.
    ReadOnly,
    /// Proposes a reversible change with its OWN downstream human gate
    /// (tracked-change accept/reject) or writes the caller's own moderatable
    /// data — auto-runs, the human resolves it afterwards.
    Proposal,
    /// Mutates state or runs code; the agentic run pauses for explicit approval
    /// before this dispatches.
    RequiresApproval,
}

/// The state-effect of a tool (agents §8.4). Every entry in [`ALL`] maps to one
/// (a unit test enforces coverage).
pub fn effect(name: &str) -> ToolEffect {
    match name {
        "current_time" | "list_documents" | "read_document" | "read_table_cells"
        | "list_workspace_documents" | "read_skill" | "track_steps" | "web_search" => {
            ToolEffect::ReadOnly
        }
        "edit_document" | "remember_fact" => ToolEffect::Proposal,
        "generate_artefact" | "code_interpreter" => ToolEffect::RequiresApproval,
        _ => ToolEffect::RequiresApproval, // unknown ⇒ safest: gate it
    }
}

/// Does the tool cross the zero-egress perimeter (the lethal-trifecta third leg)?
/// `web_search` and any future DMS / send-email connector do; everything internal
/// does not. An egress tool is ALWAYS gated regardless of its [`effect`].
pub fn egress(name: &str) -> bool {
    matches!(name, "web_search")
}

/// Should this tool run automatically inside an agent loop, or pause for human
/// approval first? Auto-run iff it neither mutates state (RequiresApproval) nor
/// crosses the perimeter. Proposal + ReadOnly internal tools auto-run.
pub fn gated(name: &str) -> bool {
    matches!(effect(name), ToolEffect::RequiresApproval) || egress(name)
}

/// Constrained delegation (agents §8.3): the agent run acts under the invoking
/// user, never exceeding them. Assert the tool's required permission ⊆ the user's
/// BEFORE dispatch. Per-resource scoping still happens inside `dispatch`; this is
/// the uniform pre-gate for the write-class tools.
pub async fn tool_permitted(
    state: &AppState,
    ctx: &AuthContext,
    name: &str,
    project_id: Option<Uuid>,
) -> Result<()> {
    use crate::auth::rbac::{Permission};
    match name {
        // Proposing tracked-change edits to a workspace document needs project write.
        "edit_document" => {
            let pid = project_id
                .ok_or_else(|| AppError::Forbidden("edit_document requires a project".into()))?;
            state.rbac.require_project(&state.pg, ctx, pid, Permission::Write).await
        }
        _ => Ok(()),
    }
}

/// Per-tool-type timeout (defaults by type, configurable).
/// A per-deployment override (keyed by tool-type, e.g. widened for a slower
/// llama.cpp profile) wins over the code default.
pub fn timeout_for(name: &str, overrides: &HashMap<String, u64>) -> Duration {
    let ty = tool_type(name);
    if let Some(&secs) = overrides.get(ty.as_str()) {
        return Duration::from_secs(secs);
    }
    let secs = match ty {
        ToolType::System | ToolType::Memory => 10,
        ToolType::Dms => 30,
        ToolType::DocumentRead => 60,
        // Web is generous by design: the ML pipeline paces SERP/fetch calls
        // politely (never risk an engine IP ban), so the budget is pacing-sized, not snappy.
        ToolType::Web => 120,
        ToolType::Rag | ToolType::Artefact | ToolType::Code => 120,
    };
    Duration::from_secs(secs)
}

/// A per-deployment override for a native tool, loaded
/// from `tool_overrides`. Absence of a row means defaults: enabled, code
/// description. `enabled=false` is a kill-switch; `description_override` replaces
/// the schema description the LLM sees.
#[derive(Debug, Clone)]
pub struct Override {
    pub enabled: bool,
    pub description_override: Option<String>,
}

/// Load the native-tool overrides keyed by tool name. An empty map (the default,
/// no rows) leaves every tool byte-identical to its code default — the
/// prefix-cache relies on this.
pub async fn load_overrides(pg: &sqlx::PgPool) -> Result<HashMap<String, Override>> {
    let rows = sqlx::query!("SELECT tool_name, enabled, description_override FROM tool_overrides")
        .fetch_all(pg)
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                r.tool_name,
                Override { enabled: r.enabled, description_override: r.description_override },
            )
        })
        .collect())
}

/// OpenAI function-call definitions for the enabled tool names (unknown names
/// ignored). Applies `tool_overrides`: a tool switched off is dropped; a tool
/// with a description override has its schema `description` replaced. With an
/// empty `overrides` map the output is byte-identical to the code default.
pub fn defs(enabled: &[String], overrides: &HashMap<String, Override>) -> Vec<Value> {
    enabled
        .iter()
        .filter(|n| overrides.get(n.as_str()).map(|o| o.enabled).unwrap_or(true))
        .filter_map(|n| {
            let mut v = def(n.as_str())?;
            if let Some(desc) =
                overrides.get(n.as_str()).and_then(|o| o.description_override.as_deref())
            {
                v["function"]["description"] = json!(desc);
            }
            Some(v)
        })
        .collect()
}

/// UI-facing metadata for one native tool. The `label`
/// and `hint` used to be hardcoded in the frontend `AGENT_TOOL_CATALOG`; this is
/// now the single source of truth. `effect`/`egress`/`capability`/`default` are
/// derived from the code classifiers so they can never drift from behaviour.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CatalogEntry {
    pub name: &'static str,
    pub label: &'static str,
    pub hint: &'static str,
    /// "read" | "proposal" | "approval" — mirrors [`effect`].
    pub effect: &'static str,
    pub egress: bool,
    pub capability: Option<&'static str>,
    /// Ships off (needs an admin to enable a connector) — presentation only.
    pub dormant: bool,
    /// Always-on baseline ([`DEFAULT_TOOLS`]); shown locked-on in the editor.
    pub default: bool,
}

fn effect_str(name: &str) -> &'static str {
    match effect(name) {
        ToolEffect::ReadOnly => "read",
        ToolEffect::Proposal => "proposal",
        ToolEffect::RequiresApproval => "approval",
    }
}

/// The native tool catalogue in the frontend display order, with derived badges.
/// The label/hint/dormant strings mirror the old `AGENT_TOOL_CATALOG`; every
/// other field is computed from the same classifiers `dispatch` uses.
pub fn catalog() -> Vec<CatalogEntry> {
    // (name, label, hint, dormant) — order matches the agent editor's tool list.
    const META: &[(&str, &str, &str, bool)] = &[
        ("read_document", "Read document", "read a document's text", false),
        ("edit_document", "Edit document", "propose tracked changes on a workspace doc", false),
        ("list_documents", "List documents", "list a project's documents", false),
        (
            "list_workspace_documents",
            "List workspace docs",
            "list editable workspace documents",
            false,
        ),
        ("read_table_cells", "Read table cells", "read a tabular review's results", false),
        ("generate_artefact", "Generate artefact", "create a new document", false),
        ("read_skill", "Read skill", "load an attached Skill's instructions", false),
        ("remember_fact", "Remember fact", "write to memory (explicit only)", false),
        ("current_time", "Current time", "read the current time", false),
        ("track_steps", "Track steps", "show a live checklist for multi-step tasks", false),
        ("web_search", "Web search", "external — ships dormant", true),
        ("code_interpreter", "Code interpreter", "run code (host capability)", false),
    ];
    META.iter()
        .map(|(name, label, hint, dormant)| CatalogEntry {
            name,
            label,
            hint,
            effect: effect_str(name),
            egress: egress(name),
            capability: capability(name),
            dormant: *dormant,
            default: DEFAULT_TOOLS.contains(name),
        })
        .collect()
}

/// The code-default schema description a native tool advertises, before any
/// `tool_overrides` row is applied. Used by the catalogue endpoint so the admin
/// UI can show (and reset to) the original text.
pub fn default_description(name: &str) -> Option<String> {
    def(name).and_then(|v| v["function"]["description"].as_str().map(str::to_string))
}

fn def(name: &str) -> Option<Value> {
    let v = match name {
        "current_time" => json!({
            "type": "function",
            "function": {
                "name": "current_time",
                "description": "Return the current UTC time as an ISO-8601 string.",
                "parameters": { "type": "object", "properties": {} }
            }
        }),
        "list_documents" => json!({
            "type": "function",
            "function": {
                "name": "list_documents",
                "description": "List the ready documents in the current project's knowledge base (id, filename, and the document's own date when known, else when it was ingested). To scope by time (e.g. a monthly report), pass last_n_days — the date window is computed server-side from the current date, so you must NOT compute dates yourself.",
                "parameters": { "type": "object", "properties": {
                    "last_n_days": { "type": "integer", "description": "Optional: only documents ingested within the last N days." }
                } }
            }
        }),
        "read_document" => json!({
            "type": "function",
            "function": {
                "name": "read_document",
                "description": "Read the extracted text of a document in the current project by its id. For a large document, pass `query` describing what you need — the whole document is then read exhaustively and focused on that, instead of being truncated.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "doc_id": { "type": "string", "description": "The document id (UUID)." },
                        "query": { "type": "string", "description": "Optional: what you are looking for in the document (enables an exhaustive focused read of large documents)." }
                    },
                    "required": ["doc_id"]
                }
            }
        }),
        "remember_fact" => json!({
            "type": "function",
            "function": {
                "name": "remember_fact",
                "description": "Persist a fact to memory. Call this ONLY when the user explicitly asks to remember something (e.g. \"remember that …\", \"don't forget …\"). Never infer or auto-save facts.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "content": { "type": "string", "description": "The fact to remember, in a self-contained sentence." },
                        "scope": { "type": "string", "enum": ["user", "project"], "description": "user = about the person (default); project = shared with this project's members." }
                    },
                    "required": ["content"]
                }
            }
        }),
        "track_steps" => json!({
            "type": "function",
            "function": {
                "name": "track_steps",
                "description": "Show the user a live checklist of the steps for a multi-step task. Call once to outline the plan, then call AGAIN with the FULL updated list whenever a step's status changes (always pass every step, not just the changed one).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "steps": {
                            "type": "array",
                            "description": "The full ordered checklist.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "title": { "type": "string", "description": "Short step description." },
                                    "status": { "type": "string", "enum": ["pending", "running", "done", "skipped"], "description": "Current status of this step." }
                                },
                                "required": ["title", "status"]
                            }
                        }
                    },
                    "required": ["steps"]
                }
            }
        }),
        "list_workspace_documents" => json!({
            "type": "function",
            "function": {
                "name": "list_workspace_documents",
                "description": "List the editable workspace documents in the current project (id + filename). Use to find a document's id before editing it.",
                "parameters": { "type": "object", "properties": {} }
            }
        }),
        "edit_document" => json!({
            "type": "function",
            "function": {
                "name": "edit_document",
                "description": "Propose tracked-change edits to a DOCX workspace document. Each edit replaces 'find' with 'replace' as a tracked change the user can accept or reject. Use empty 'replace' to delete, empty 'find' (with context_before) to insert.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "doc_id": { "type": "string", "description": "The workspace document id (UUID)." },
                        "edits": {
                            "type": "array",
                            "description": "The edits to apply as tracked changes.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "find": { "type": "string", "description": "Exact text to replace (empty for a pure insertion)." },
                                    "replace": { "type": "string", "description": "Replacement text (empty to delete)." },
                                    "context_before": { "type": "string", "description": "Optional text immediately preceding the match, to disambiguate." },
                                    "context_after": { "type": "string", "description": "Optional text immediately following the match." }
                                },
                                "required": ["find", "replace"]
                            }
                        }
                    },
                    "required": ["doc_id", "edits"]
                }
            }
        }),
        "read_table_cells" => json!({
            "type": "function",
            "function": {
                "name": "read_table_cells",
                "description": "Read the already-extracted cells of the tabular review this chat is scoped to (document, column, value, reasoning). Use to answer questions about the review without re-reading the source documents.",
                "parameters": { "type": "object", "properties": {} }
            }
        }),
        "generate_artefact" => json!({
            "type": "function",
            "function": {
                "name": "generate_artefact",
                "description": "Generate a downloadable artefact (DOCX, PDF, Markdown, a self-contained HTML page, or an XLSX spreadsheet) from the given content for the user to download. Use when the user asks for a document/file to take away, a dashboard/infographic/web page, or a spreadsheet. For html, `content` is a complete self-contained HTML page (read the dashboard Skill); reference charts via the `<!-- pai:echarts -->` marker — never link an external script or stylesheet. For xlsx, `content` is a JSON workbook spec (read the xlsx-tables Skill).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "enum": ["docx", "pdf", "md", "html", "xlsx"], "description": "The file type to produce." },
                        "title": { "type": "string", "description": "The artefact title (used as the heading / page title)." },
                        "content": { "type": "string", "description": "For docx/pdf/md: the body text (Markdown). For html: a self-contained HTML page. For xlsx: a JSON workbook spec." }
                    },
                    "required": ["kind", "content"]
                }
            }
        }),
        "read_skill" => json!({
            "type": "function",
            "function": {
                "name": "read_skill",
                "description": "Load a Skill's instructions on demand. Call with just `skill_id` to read its SKILL.md plus a list of any extra files it bundles; call again with `subpath` (e.g. \"references/policy.md\") to load one of those files only when you need it.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "skill_id": { "type": "string", "description": "The Skill id (UUID)." },
                        "subpath": { "type": "string", "description": "Optional: a bundled file to load, under references/, templates/ or assets/ (as listed in the SKILL.md read)." }
                    },
                    "required": ["skill_id"]
                }
            }
        }),
        "web_search" => json!({
            "type": "function",
            "function": {
                "name": "web_search",
                "description": "Search the public web and read the top results. Returns a digest with numbered sources [n] you can cite. Only available when an administrator has enabled the web-search connector; otherwise it is dormant (the platform is zero-egress by default).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "The search query." },
                        "recency": { "type": "string", "enum": ["any", "year", "month", "week", "day"], "description": "Restrict results by age — use month/week/day for 'latest/current' questions." },
                        "depth": { "type": "string", "enum": ["quick", "standard", "deep"], "description": "Effort budget: quick for simple facts, deep for thorough research." }
                    },
                    "required": ["query"]
                }
            }
        }),
        "code_interpreter" => json!({
            "type": "function",
            "function": {
                "name": "code_interpreter",
                "description": "Run Python in a sandboxed, network-isolated (zero-egress) environment to compute, analyse data, or produce files (charts, CSVs). Files attached to the current message are present in the working directory by their filename — read them directly (e.g. pandas.read_excel('name.xlsx') / read_csv); prefer this over the inline text for precise analysis of large tables. Returns stdout/stderr/exit code; any files the code writes become downloadable artefacts. Available only on hosts where the capability is enabled.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "code": { "type": "string", "description": "The Python source to execute." },
                        "language": { "type": "string", "enum": ["python"], "description": "Language (only python is supported)." }
                    },
                    "required": ["code"]
                }
            }
        }),
        _ => return None,
    };
    Some(v)
}

/// Per-Agent web-search budget (agents.params → `web_depth_max`,
/// `web_max_fetches`): an agent can tighten the effort class, never widen it.
#[derive(Debug, Clone, Default)]
pub struct WebBudget {
    pub depth_max: Option<String>,
    pub max_fetches: Option<i64>,
}

/// Clamp the model-requested depth to the agent's cap (quick < standard < deep).
/// Unknown values normalise to "standard"; no cap = the request stands.
pub fn clamp_depth(requested: Option<&str>, max: Option<&str>) -> String {
    fn rank(d: &str) -> u8 {
        match d {
            "quick" => 0,
            "deep" => 2,
            _ => 1, // standard + anything unknown
        }
    }
    let req = match requested {
        Some(d) if d == "quick" || d == "standard" || d == "deep" => d,
        _ => "standard",
    };
    match max {
        Some(m) if rank(req) > rank(m) => m.to_string(),
        _ => req.to_string(),
    }
}

/// Execute a tool call. `project_id` is the current chat's project (scope for
/// document tools); `web_budget` is the calling Agent's web-search cap (None
/// for agent-less contexts). Returns the tool result content (fed back to the
/// model).
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
pub async fn dispatch(
    state: &AppState,
    ctx: &AuthContext,
    project_id: Option<Uuid>,
    chat_id: Uuid,
    turn_id: Uuid,
    tx: &mpsc::Sender<ServerFrame>,
    web_budget: Option<&WebBudget>,
    ci_files: &[crate::code_interpreter::InputFile],
    custom: &HashMap<String, custom::CustomToolRow>,
    name: &str,
    args: &Value,
) -> Result<String> {
    // Defence in depth: a capability-gated tool must not run if its host feature
    // is off (the advertise filter already hides it, but never trust the model).
    if !host_enabled(name, &state.boot.features) {
        return Err(AppError::Validation(format!("capability '{name}' is disabled on this host")));
    }
    match name {
        "current_time" => Ok(time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| "unknown".into())),

        "track_steps" => {
            // Stateless: the model passes the full checklist each call; we forward
            // it to the UI as a live step list (#13). Nothing is persisted.
            let steps: Vec<crate::ws::protocol::StepOut> = args
                .get("steps")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|s| {
                            let title = s.get("title").and_then(|v| v.as_str())?.trim().to_string();
                            if title.is_empty() {
                                return None;
                            }
                            let status = match s.get("status").and_then(|v| v.as_str()) {
                                Some(v @ ("running" | "done" | "skipped" | "pending")) => v,
                                _ => "pending",
                            };
                            Some(crate::ws::protocol::StepOut { title, status: status.into() })
                        })
                        .collect()
                })
                .unwrap_or_default();
            if steps.is_empty() {
                return Err(AppError::Validation("track_steps needs a non-empty steps array".into()));
            }
            let n = steps.len();
            let _ = tx.send(ServerFrame::ChatSteps { turn_id, steps }).await;
            Ok(format!(
                "Recorded {n} step(s) for the user. Call track_steps again with the full updated list as statuses change."
            ))
        }

        "list_documents" => {
            let pid = project_id
                .ok_or_else(|| AppError::Validation("no project in this chat".into()))?;
            // Deterministic temporal filter: the model passes `last_n_days`; the
            // backend (which owns the clock) computes the absolute cutoff. Date
            // arithmetic is code, not token prediction — a "last 30 days" report
            // gets a real window, not a hallucinated one.
            let cutoff = args
                .get("last_n_days")
                .and_then(|v| v.as_i64())
                .filter(|&n| n > 0)
                .map(|n| time::OffsetDateTime::now_utc() - time::Duration::days(n));
            let rows = sqlx::query!(
                r#"SELECT kd.id, kd.original_filename,
                          COALESCE(kd.effective_date, kd.created_at) AS "doc_date!"
                   FROM kb_documents kd
                   JOIN project_kb_links pl ON pl.kb_id = kd.kb_id
                   WHERE pl.project_id = $1 AND kd.ingest_status = 'ready'
                     AND ($2::timestamptz IS NULL OR COALESCE(kd.effective_date, kd.created_at) >= $2)
                   ORDER BY COALESCE(kd.effective_date, kd.created_at) DESC"#,
                pid,
                cutoff,
            )
            .fetch_all(&state.pg)
            .await?;
            let list: Vec<Value> = rows
                .into_iter()
                .map(|r| {
                    let date = r
                        .doc_date
                        .format(&time::format_description::well_known::Rfc3339)
                        .unwrap_or_default();
                    json!({ "doc_id": r.id, "filename": r.original_filename, "date": date })
                })
                .collect();
            Ok(serde_json::to_string(&list).unwrap_or_else(|_| "[]".into()))
        }

        "read_document" => {
            let doc_id = args
                .get("doc_id")
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or_else(|| AppError::Validation("read_document needs a valid doc_id".into()))?;
            let pid = project_id
                .ok_or_else(|| AppError::Validation("no project in this chat".into()))?;
            // Scope: only documents in a KB attached to the current chat's
            // project (which the caller already has access to via the chat).
            let doc = sqlx::query!(
                r#"SELECT kd.bytes_path, kd.mime
                   FROM kb_documents kd
                   JOIN project_kb_links pl ON pl.kb_id = kd.kb_id
                   WHERE kd.id = $1 AND pl.project_id = $2"#,
                doc_id,
                pid
            )
            .fetch_optional(&state.pg)
            .await?;
            let Some(doc) = doc else {
                let _ = ctx; // RBAC is the chat-project scope here
                return Err(AppError::Forbidden("document is not in this project".into()));
            };
            let query = args.get("query").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty());
            let path = crate::storage::resolve_file(&state.boot.storage.documents_dir, &doc.bytes_path);
            crate::ml::read_document(&state.http, &state.boot.ml.base_url, &path.to_string_lossy(), doc.mime.as_deref(), query, crate::ml::provider_overrides(state, ctx.user_id).await)
                .await
        }

        "remember_fact" => {
            let content = args
                .get("content")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| AppError::Validation("remember_fact needs non-empty content".into()))?;
            let scope = args.get("scope").and_then(|v| v.as_str()).unwrap_or("user");
            let id = crate::http::memory::insert_fact(
                state, ctx, scope, content, project_id, Some(chat_id),
            )
            .await?;
            Ok(format!("Remembered (fact {id})."))
        }

        "list_workspace_documents" => {
            let pid = project_id
                .ok_or_else(|| AppError::Validation("no project in this chat".into()))?;
            let rows = sqlx::query!(
                "SELECT id, original_filename FROM documents \
                 WHERE project_id = $1 AND deleted_at IS NULL ORDER BY created_at",
                pid
            )
            .fetch_all(&state.pg)
            .await?;
            let list: Vec<Value> = rows
                .into_iter()
                .map(|r| json!({ "doc_id": r.id, "filename": r.original_filename }))
                .collect();
            Ok(serde_json::to_string(&list).unwrap_or_else(|_| "[]".into()))
        }

        "edit_document" => {
            let pid = project_id
                .ok_or_else(|| AppError::Validation("no project in this chat".into()))?;
            let doc_id = args
                .get("doc_id")
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or_else(|| AppError::Validation("edit_document needs a valid doc_id".into()))?;

            // Scope: the document must be in the chat's project.
            let doc_project = crate::documents::project_of(&state.pg, doc_id).await?;
            if doc_project != pid {
                return Err(AppError::Forbidden("document is not in this project".into()));
            }

            // Parse the edits array into ml::EditInput.
            let edits: Vec<crate::ml::EditInput> = args
                .get("edits")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .map(|e| crate::ml::EditInput {
                            find: e.get("find").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                            replace: e.get("replace").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                            context_before: e.get("context_before").and_then(|v| v.as_str()).map(String::from),
                            context_after: e.get("context_after").and_then(|v| v.as_str()).map(String::from),
                        })
                        .collect()
                })
                .unwrap_or_default();
            if edits.is_empty() {
                return Err(AppError::Validation("edit_document needs at least one edit".into()));
            }

            let cur = crate::documents::current_version(&state.pg, &state.boot.storage.workspace_dir, doc_id).await?;
            let out = std::env::temp_dir()
                .join(format!("pai_edit_{}.docx", Uuid::now_v7()))
                .to_string_lossy()
                .to_string();
            let applied = crate::ml::apply_tracked_changes(
                &state.http, &state.boot.ml.base_url, &cur.bytes_path, &out, &edits, "Assistant",
            )
            .await?;

            if applied.changes.is_empty() {
                let _ = tokio::fs::remove_file(&out).await;
                let reasons: Vec<String> = applied.errors.iter().map(|e| e.reason.clone()).collect();
                return Ok(format!("No edits applied. {}", reasons.join("; ")));
            }

            let bytes = tokio::fs::read(&out)
                .await
                .map_err(|e| AppError::Other(anyhow::anyhow!("read edited docx: {e}")))?;
            let _ = tokio::fs::remove_file(&out).await;

            // New version (assistant_edit), then one document_edits row per change.
            let (ver_id, n) =
                crate::documents::add_version(state, ctx, doc_id, "assistant_edit", &bytes, ctx.user_id).await?;
            for ch in &applied.changes {
                sqlx::query!(
                    "INSERT INTO document_edits \
                     (id, document_id, document_version_id, w_id, author, find_text, replace_text) \
                     VALUES ($1, $2, $3, $4, 'assistant', $5, $6)",
                    Uuid::now_v7(), doc_id, ver_id, ch.w_id, ch.find, ch.replace,
                )
                .execute(&state.pg)
                .await?;
            }

            let mut ev = crate::audit::AuditEvent::action("document.edit.proposed", ctx.role.as_str());
            ev.actor_user_id = ctx.user_id;
            ev.resource_type = Some("document".into());
            ev.resource_id = Some(doc_id);
            ev.payload = Some(json!({ "version_id": ver_id, "changes": applied.changes.len() }));
            let _ = crate::audit::append(&state.pg, &ev).await;

            // Live tracked-change cards for the UI (tracked-changes flow §3).
            let changes_out = applied
                .changes
                .iter()
                .map(|c| crate::ws::protocol::EditChangeOut {
                    w_id: c.w_id.clone(),
                    find: c.find.clone(),
                    replace: c.replace.clone(),
                })
                .collect();
            let _ = tx
                .send(ServerFrame::DocEdited {
                    turn_id,
                    document_id: doc_id,
                    version_id: ver_id,
                    changes: changes_out,
                })
                .await;

            let ids: Vec<&str> = applied.changes.iter().map(|c| c.w_id.as_str()).collect();
            Ok(format!(
                "Proposed {} tracked change(s) on version {n} (ids: {}). The user can accept or reject them.",
                applied.changes.len(),
                ids.join(", ")
            ))
        }

        "read_table_cells" => {
            // The chat must be scoped to a tabular review.
            let review_id: Option<Uuid> =
                sqlx::query_scalar!("SELECT tabular_review_id FROM chats WHERE id = $1", chat_id)
                    .fetch_optional(&state.pg)
                    .await?
                    .flatten();
            let review_id = review_id
                .ok_or_else(|| AppError::Validation("this chat is not scoped to a tabular review".into()))?;
            let rows = sqlx::query!(
                r#"SELECT d.original_filename AS "filename!", c.column_key, c.value, c.reasoning
                   FROM tabular_cells c JOIN documents d ON d.id = c.document_id
                   WHERE c.review_id = $1 AND c.status = 'done'
                   ORDER BY d.original_filename, c.column_key"#,
                review_id
            )
            .fetch_all(&state.pg)
            .await?;
            let cells: Vec<Value> = rows
                .into_iter()
                .map(|r| json!({
                    "document": r.filename,
                    "column": r.column_key,
                    "value": r.value,
                    "reasoning": r.reasoning,
                }))
                .collect();
            Ok(serde_json::to_string(&cells).unwrap_or_else(|_| "[]".into()))
        }

        "generate_artefact" => {
            let kind = args.get("kind").and_then(|v| v.as_str()).unwrap_or("md");
            if !matches!(kind, "docx" | "pdf" | "md" | "html" | "xlsx") {
                return Err(AppError::Validation("generate_artefact kind must be docx|pdf|md|html|xlsx".into()));
            }
            let title = args.get("title").and_then(|v| v.as_str()).unwrap_or("Artefact");
            let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
            if content.trim().is_empty() {
                return Err(AppError::Validation("generate_artefact needs content".into()));
            }

            // Chat-scoped artefact path: {artefacts_dir}/{chat_id}/{artefact_id}.{ext}.
            // Store the RELATIVE suffix; resolve to absolute only for the ML call.
            let artefact_id = Uuid::now_v7();
            let rel = format!("{chat_id}/{artefact_id}.{kind}");
            let out_path = crate::storage::resolve_file(&state.boot.storage.artefacts_dir, &rel)
                .to_string_lossy()
                .to_string();

            let (_path, mime) = crate::ml::generate_artefact(
                &state.http, &state.boot.ml.base_url, kind, title, content, &out_path,
            )
            .await?;

            sqlx::query!(
                "INSERT INTO generated_artefacts (id, chat_id, turn_id, kind, title, disk_path, mime, created_by) \
                 VALUES ($1, $2, $3, ($4::text)::artefact_kind, $5, $6, $7, $8)",
                artefact_id, chat_id, turn_id, kind, title, rel, mime, ctx.user_id,
            )
            .execute(&state.pg)
            .await?;

            let mut ev = crate::audit::AuditEvent::action("artefact.generated", ctx.role.as_str());
            ev.actor_user_id = ctx.user_id;
            ev.resource_type = Some("artefact".into());
            ev.resource_id = Some(artefact_id);
            ev.payload = Some(json!({ "chat_id": chat_id, "kind": kind, "title": title }));
            let _ = crate::audit::append(&state.pg, &ev).await;

            Ok(format!(
                "Generated {kind} artefact '{title}' (id: {artefact_id}). Download at /api/artefacts/{artefact_id}/download."
            ))
        }

        "read_skill" => {
            let skill_id = args
                .get("skill_id")
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or_else(|| AppError::Validation("read_skill needs a valid skill_id".into()))?;
            let subpath =
                args.get("subpath").and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty());
            // The skill must be attached to this chat's agent — OR be a platform
            // default skill (applied to every agent without a binding).
            let row = sqlx::query!(
                "SELECT s.disk_path FROM skills s \
                 WHERE s.id = $2 AND s.enabled AND (s.is_default OR EXISTS ( \
                     SELECT 1 FROM agent_skills a JOIN chats c ON c.agent_id = a.agent_id \
                     WHERE c.id = $1 AND a.skill_id = s.id))",
                chat_id,
                skill_id
            )
            .fetch_optional(&state.pg)
            .await?
            .ok_or_else(|| AppError::Validation("skill is not attached to this chat's agent".into()))?;
            let dir_abs = crate::storage::resolve_file(&state.boot.storage.skills_dir, &row.disk_path);
            let dir = dir_abs.as_path();
            match subpath {
                // Level 3: a bundled sub-file, on demand (path-traversal guarded).
                Some(sp) => {
                    let file = skill_subfile(dir, sp)?;
                    tokio::fs::read_to_string(&file)
                        .await
                        .map_err(|e| AppError::Validation(format!("no such skill file '{sp}': {e}")))
                }
                // Level 2: the full SKILL.md, plus a manifest of bundled files so
                // the model knows what it can load next.
                None => {
                    let body = tokio::fs::read_to_string(dir.join("SKILL.md"))
                        .await
                        .map_err(|e| AppError::Other(anyhow::anyhow!("read SKILL.md: {e}")))?;
                    let files = skill_manifest(dir).await;
                    if files.is_empty() {
                        Ok(body)
                    } else {
                        let list = files.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n");
                        Ok(format!(
                            "{body}\n\n[Bundled files — call read_skill again with `subpath` to load one]\n{list}"
                        ))
                    }
                }
            }
        }

        "web_search" => {
            // The egress gate first — dormant connectors never reach the
            // network, and the attempt is audited either way.
            match integrations::guard_egress(state, ctx, ConnectorKind::WebSearch).await {
                Err(e) => Ok(format!(
                    "Web search is not available: {e}. Answer from the provided context instead."
                )),
                Ok(()) => {
                    let query = args
                        .get("query")
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|q| !q.is_empty())
                        .ok_or_else(|| AppError::Validation("web_search requires a query".into()))?;
                    let recency = args.get("recency").and_then(|v| v.as_str());
                    // The Agent's cap clamps the requested depth BEFORE the
                    // deep-branch decision — deep under a standard cap runs inline.
                    let depth_owned = clamp_depth(
                        args.get("depth").and_then(|v| v.as_str()),
                        web_budget.and_then(|b| b.depth_max.as_deref()),
                    );
                    let depth = Some(depth_owned.as_str());
                    let agent_max_fetches = web_budget.and_then(|b| b.max_fetches);

                    // `depth=deep` runs as a durable background agent-run: the
                    // exhaustive, politely-paced loop must not be bounded by the
                    // tool timeout (web-search flow doc). Enqueue + ack now;
                    // the result is posted back into the chat when ready.
                    if depth == Some("deep") {
                        let agent_id: Option<Uuid> = sqlx::query_scalar!(
                            "SELECT agent_id FROM chats WHERE id = $1", chat_id
                        )
                        .fetch_optional(&state.pg)
                        .await
                        .ok()
                        .flatten()
                        .flatten();
                        let run_id = if state.boot.features.agents_enabled {
                            Some(
                                crate::agent::start_run(
                                    state, agent_id, ctx.user_id, ctx.role.as_str(),
                                    Some(chat_id), turn_id, project_id, None, 1800,
                                )
                                .await?,
                            )
                        } else {
                            None
                        };
                        crate::scheduler::enqueue(
                            &state.pg,
                            crate::scheduler::TaskType::WebSearchDeep,
                            json!({
                                "run_id": run_id,
                                "chat_id": chat_id,
                                "turn_id": turn_id,
                                "user_id": ctx.user_id,
                                "role": ctx.role.as_str(),
                                "query": query,
                                "recency": recency,
                                "max_fetches": agent_max_fetches,
                            }),
                        )
                        .await
                        .map_err(|e| AppError::Other(anyhow::anyhow!("enqueue deep web search: {e}")))?;

                        {
                            use crate::audit::{self, AuditEvent};
                            let mut ev = AuditEvent::action("web_search.deep_enqueued", ctx.role.as_str());
                            ev.actor_user_id = ctx.user_id;
                            ev.resource_type = Some("integration".into());
                            ev.payload = Some(json!({ "kind": "web_search", "turn_id": turn_id, "run_id": run_id }));
                            let _ = audit::append(&state.pg, &ev).await;
                        }

                        return Ok(
                            "Deep web search started in the background. Tell the user the research \
                             is running and its results will be posted to this chat when ready \
                             (typically several minutes). Do not invent results now.".into(),
                        );
                    }

                    // Inline path (quick / standard). One streaming call to the
                    // ML service; the whole agentic loop runs there, invisible
                    // to the model — progress events surface as live agent
                    // activity. `None` timeout → bounded by the 120 s Web tool
                    // timeout (dropping the stream aborts the ML run).
                    let mut overrides = crate::ml::web_overrides(&state.pg).await;
                    overrides.max_fetches = agent_max_fetches;
                    let unavailable = |e: &dyn std::fmt::Display| {
                        // Graceful, honest degradation — same style as the
                        // dormant arm; the turn carries on without the web.
                        format!(
                            "Web search is currently unavailable: {e}. Answer from the provided context instead."
                        )
                    };
                    let mut stream = match crate::ml::web_search_stream(
                        &state.http, &state.boot.ml.base_url, query, recency, depth, &overrides,
                        crate::ml::provider_overrides(state, ctx.user_id).await, None,
                    )
                    .await
                    {
                        Ok(s) => s,
                        Err(e) => return Ok(unavailable(&e)),
                    };
                    let result = loop {
                        match stream.recv().await {
                            Some(crate::ml::WebEvent::Progress { stage, detail, round, .. }) => {
                                let mut text = detail.unwrap_or(stage);
                                if let Some(r) = round {
                                    text = format!("round {r}: {text}");
                                }
                                let _ = tx
                                    .send(ServerFrame::ChatTool {
                                        turn_id,
                                        name: "web_search".into(),
                                        phase: "progress".into(),
                                        detail: Some(text),
                                    })
                                    .await;
                            }
                            Some(crate::ml::WebEvent::Done { digest, citations }) => {
                                break crate::ml::WebSearchResult { digest, citations };
                            }
                            // Synthesis tokens are emitted only on the deep background
                            // path; the inline quick/standard path's answer is written
                            // (and streamed) by the chat turn, so ignore any here.
                            Some(crate::ml::WebEvent::Token { .. }) => {}
                            Some(crate::ml::WebEvent::Error { message }) => {
                                return Ok(unavailable(&message));
                            }
                            None => {
                                return Ok(unavailable(&"the search stream ended unexpectedly"));
                            }
                        }
                    };

                    // Persist citations keyed by turn (the assistant message id is
                    // unknown here); the chat orchestrator links message_id post-
                    // stream — the generated_artefacts pattern.
                    crate::web_search::persist_web_citations(&state.pg, turn_id, None, &result.citations).await;

                    // Per-call URL logging:
                    // which sources this search surfaced, alongside the
                    // `integration.call` event the gate already wrote.
                    {
                        use crate::audit::{self, AuditEvent};
                        let urls: Vec<&str> =
                            result.citations.iter().map(|c| c.url.as_str()).collect();
                        let mut ev = AuditEvent::action("web_search.results", ctx.role.as_str());
                        ev.actor_user_id = ctx.user_id;
                        ev.resource_type = Some("integration".into());
                        ev.payload = Some(json!({
                            "kind": "web_search",
                            "turn_id": turn_id,
                            "result_count": result.citations.len(),
                            "urls": urls,
                        }));
                        let _ = audit::append(&state.pg, &ev).await;
                    }

                    Ok(result.digest)
                }
            }
        }

        "code_interpreter" => {
            // Defence-in-depth: also refuse here if a per-group flag disables it
            // (Tier-2 #8) — not only hidden from the LLM's tool list.
            if !state.features.enabled_for(state, ctx, "code_interpreter").await {
                return Ok("Code interpreter is not available for you.".into());
            }
            let code = args
                .get("code")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| AppError::Validation("code_interpreter needs non-empty code".into()))?;
            let language = args.get("language").and_then(|v| v.as_str()).unwrap_or("python");
            if language != "python" {
                return Err(AppError::Validation("only python is supported".into()));
            }
            let req = crate::code_interpreter::ExecRequest {
                language: language.into(),
                code,
                inputs: ci_files.to_vec(), // the current turn's attachment files, in the working dir
            };
            let executor = crate::code_interpreter::select_executor(state);
            crate::code_interpreter::run_and_store(state, ctx, chat_id, turn_id, &*executor, req)
                .await
        }

        // Not a native tool → a custom tool the Agent selected (never an MCP name;
        // those route via `mcp::dispatch` on separate rails). The three name spaces
        // are disjoint by construction (native ∈ ALL, MCP has `__`, custom neither).
        other => match custom.get(other) {
            Some(row) => custom::dispatch_custom(state, ctx, chat_id, row, args).await,
            None => Err(AppError::Validation(format!("unknown tool: {other}"))),
        },
    }
}

/// Skills bundle optional `references/` `templates/` `assets/` sub-folders
/// (Agent-Skills standard). List the files in them (one level) so the model can
/// choose what to load on demand — the third level of progressive disclosure.
async fn skill_manifest(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    for sub in ["references", "templates", "assets"] {
        let mut rd = match tokio::fs::read_dir(dir.join(sub)).await {
            Ok(rd) => rd,
            Err(_) => continue, // folder absent — fine
        };
        while let Ok(Some(entry)) = rd.next_entry().await {
            if entry.file_type().await.map(|t| t.is_file()).unwrap_or(false) {
                out.push(format!("{sub}/{}", entry.file_name().to_string_lossy()));
            }
        }
    }
    out.sort();
    out
}

/// Resolve a Skill sub-file safely. Whitelist the allowed sub-folders, then
/// canonicalise and assert the resolved path stays inside the skill directory —
/// defeating `..` traversal and symlink escape regardless of the input.
fn skill_subfile(dir: &std::path::Path, subpath: &str) -> Result<std::path::PathBuf, AppError> {
    const ALLOWED: [&str; 3] = ["references/", "templates/", "assets/"];
    if !ALLOWED.iter().any(|p| subpath.starts_with(p)) {
        return Err(AppError::Forbidden(
            "skill sub-file must be under references/, templates/ or assets/".into(),
        ));
    }
    let canon_dir = dir
        .canonicalize()
        .map_err(|e| AppError::Other(anyhow::anyhow!("skill dir: {e}")))?;
    let canon_file = dir
        .join(subpath)
        .canonicalize()
        .map_err(|e| AppError::Validation(format!("no such skill file '{subpath}': {e}")))?;
    if !canon_file.starts_with(&canon_dir) {
        return Err(AppError::Forbidden("skill sub-file escapes the skill directory".into()));
    }
    Ok(canon_file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_a_def() {
        for name in ALL {
            assert!(def(name).is_some(), "missing OpenAI def for {name}");
        }
    }

    #[test]
    fn classifier_covers_all_and_gates_correctly() {
        // Every tool maps (no silent default reached for a known tool).
        for name in ALL {
            let _ = effect(name);
            let _ = egress(name);
        }
        // Read-only internal tools auto-run.
        assert!(!gated("read_document"));
        assert!(!gated("list_documents"));
        // Proposal tools auto-run (own downstream HITL).
        assert!(!gated("edit_document"));
        assert!(!gated("remember_fact"));
        // State-changing tools are gated.
        assert!(gated("generate_artefact"));
        assert!(gated("code_interpreter"));
        // Egress is ALWAYS gated even though web_search "only reads".
        assert_eq!(effect("web_search"), ToolEffect::ReadOnly);
        assert!(egress("web_search"));
        assert!(gated("web_search"));
        // Unknown tool ⇒ gated (fail-safe).
        assert!(gated("totally_new_connector"));
    }

    #[tokio::test]
    async fn skill_subfile_guards_and_manifest_lists() {
        use std::fs;
        let base = std::env::temp_dir().join(format!("pai_skill_{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(base.join("references")).unwrap();
        fs::create_dir_all(base.join("templates")).unwrap();
        fs::write(base.join("references/policy.md"), "POLICY").unwrap();
        fs::write(base.join("templates/letter.md"), "LETTER").unwrap();
        fs::write(base.join("SKILL.md"), "the skill").unwrap();

        // Manifest lists the bundled files (not SKILL.md), sorted.
        let manifest = skill_manifest(&base).await;
        assert_eq!(manifest, vec!["references/policy.md".to_string(), "templates/letter.md".to_string()]);

        // A whitelisted, in-bounds file resolves and reads.
        let f = skill_subfile(&base, "references/policy.md").unwrap();
        assert_eq!(fs::read_to_string(&f).unwrap(), "POLICY");

        // Non-whitelisted directory → Forbidden.
        assert!(matches!(skill_subfile(&base, "secrets/x.md"), Err(AppError::Forbidden(_))));
        // Traversal escape (whitelisted prefix but climbs out) → error.
        assert!(skill_subfile(&base, "references/../../escape.md").is_err());
        // Missing file → error (not a panic).
        assert!(skill_subfile(&base, "references/missing.md").is_err());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn timeouts_differ_by_type() {
        let none = HashMap::new();
        // System is the cheap floor; Rag/Artefact/Code get the long ceiling.
        assert!(timeout_for("current_time", &none) < timeout_for("read_document", &none));
        assert!(timeout_for("read_document", &none) < timeout_for("generate_artefact", &none));
        // Web is pacing-sized, not snappy.
        assert_eq!(timeout_for("web_search", &none), Duration::from_secs(120));
        assert_eq!(timeout_for("remember_fact", &none), Duration::from_secs(10));
    }

    #[test]
    fn timeout_override_wins() {
        let mut ov = HashMap::new();
        ov.insert("document_read".to_string(), 240u64);
        assert_eq!(timeout_for("read_document", &ov), Duration::from_secs(240));
        // A type without an override keeps its code default.
        assert_eq!(timeout_for("web_search", &ov), Duration::from_secs(120));
    }

    #[test]
    fn web_search_is_a_web_tool() {
        assert_eq!(tool_type("web_search"), ToolType::Web);
    }

    #[test]
    fn clamp_depth_tightens_never_widens() {
        // No cap → the request stands (unknown/absent normalise to standard).
        assert_eq!(clamp_depth(Some("deep"), None), "deep");
        assert_eq!(clamp_depth(None, None), "standard");
        assert_eq!(clamp_depth(Some("bogus"), None), "standard");
        // Cap clamps downwards…
        assert_eq!(clamp_depth(Some("deep"), Some("standard")), "standard");
        assert_eq!(clamp_depth(Some("deep"), Some("quick")), "quick");
        assert_eq!(clamp_depth(Some("standard"), Some("quick")), "quick");
        // …but never widens.
        assert_eq!(clamp_depth(Some("quick"), Some("deep")), "quick");
        assert_eq!(clamp_depth(Some("standard"), Some("deep")), "standard");
    }

    #[test]
    fn code_interpreter_is_capability_gated() {
        let off = FeaturesConfig { code_interpreter: false, voice: false, agents_enabled: true, workflows: false, groundedness: false, voice_live: false, mcp: false, messaging: true, white_label: false, compliance_audit: false, moderation: false, message_review: false, data_owner_approval: false, federated_sso: false, custom_rbac: false, enterprise_connectors: false };
        let on = FeaturesConfig { code_interpreter: true, voice: false, agents_enabled: true, workflows: false, groundedness: false, voice_live: false, mcp: false, messaging: true, white_label: false, compliance_audit: false, moderation: false, message_review: false, data_owner_approval: false, federated_sso: false, custom_rbac: false, enterprise_connectors: false };
        assert!(!host_enabled("code_interpreter", &off));
        assert!(host_enabled("code_interpreter", &on));
        // Ordinary tools are always host-enabled.
        assert!(host_enabled("read_document", &off));
        assert_eq!(capability("read_document"), None);
    }

    // ── Native tool overrides + catalogue ───────────────────────────────────

    #[test]
    fn defs_are_byte_identical_with_empty_overrides() {
        // The acceptance pin: with no override rows the serialised defs must be
        // byte-for-byte what the code produces (prefix-cache safety, layer [2]).
        let enabled: Vec<String> = ALL.iter().map(|s| s.to_string()).collect();
        let none: HashMap<String, Override> = HashMap::new();
        let with = defs(&enabled, &none);
        let bare: Vec<Value> = enabled.iter().filter_map(|n| def(n)).collect();
        assert_eq!(
            serde_json::to_string(&with).unwrap(),
            serde_json::to_string(&bare).unwrap()
        );
    }

    #[test]
    fn override_disable_drops_the_tool_and_description_override_replaces_text() {
        let enabled: Vec<String> =
            vec!["read_document".into(), "web_search".into(), "current_time".into()];
        let mut ov = HashMap::new();
        // web_search switched off → absent from defs.
        ov.insert("web_search".to_string(), Override { enabled: false, description_override: None });
        // read_document description overridden → text replaced.
        ov.insert(
            "read_document".to_string(),
            Override { enabled: true, description_override: Some("CUSTOM READ".into()) },
        );
        let out = defs(&enabled, &ov);
        let names: Vec<&str> =
            out.iter().filter_map(|v| v["function"]["name"].as_str()).collect();
        assert_eq!(names, vec!["read_document", "current_time"], "disabled tool must be dropped");
        let read = out.iter().find(|v| v["function"]["name"] == "read_document").unwrap();
        assert_eq!(read["function"]["description"], "CUSTOM READ");
        // A tool with an enabled=true override but no description keeps the default.
        let ct = out.iter().find(|v| v["function"]["name"] == "current_time").unwrap();
        assert_eq!(ct["function"]["description"], default_description("current_time").unwrap());
    }

    #[test]
    fn catalog_covers_all_tools_with_derived_badges() {
        let cat = catalog();
        // Every native tool appears exactly once.
        let mut names: Vec<&str> = cat.iter().map(|e| e.name).collect();
        names.sort_unstable();
        let mut all: Vec<&str> = ALL.to_vec();
        all.sort_unstable();
        assert_eq!(names, all);
        // Derived fields agree with the code classifiers (no drift).
        for e in &cat {
            assert_eq!(e.effect, effect_str(e.name));
            assert_eq!(e.egress, egress(e.name));
            assert_eq!(e.capability, capability(e.name));
            assert_eq!(e.default, DEFAULT_TOOLS.contains(&e.name));
        }
        // web_search is the egress/dormant one; generate_artefact is the default.
        let ws = cat.iter().find(|e| e.name == "web_search").unwrap();
        assert!(ws.egress && ws.dormant);
        assert!(cat.iter().find(|e| e.name == "generate_artefact").unwrap().default);
    }
}
