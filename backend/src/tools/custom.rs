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

//! Custom (deployment-defined) tools — the http kind.
//! A custom tool is registered, versioned and approved by an admin; an Agent
//! selects it by name (via the existing `agent_tools` list) and calls it in the
//! agentic loop. Dispatch is a hardened, declarative HTTP call: `{{param}}`
//! substitution from the model's arguments, the single zero-egress choke-point
//! (`guard_egress(CustomTool)`), the same dual-mode SSRF guard as MCP, an auth
//! secret from the keyring, and a size-capped response (raw or JSON-Pointer
//! extraction). All failures come back to the model as structured text — a bad
//! tool call must let the model recover, never abort the turn. The script kind is
//! a deferred follow-up (Firecracker); until then it is honestly refused.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::audit::{self, AuditEvent, AuditOutcome};
use crate::error::{AppError, Result};
use crate::integrations::{self, ConnectorKind};
use crate::state::AppState;

/// Response bodies are capped so a runaway endpoint cannot exhaust memory or the
/// model's context.
const RESPONSE_CAP: usize = 256 * 1024;
const DEFAULT_TIMEOUT_SECS: u64 = 30;

#[derive(Debug, Clone)]
pub struct CustomToolRow {
    pub id: Uuid,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub kind: String,
    pub params_schema: Value,
    pub config: Value,
    pub auth_value_enc: Option<String>,
    pub requires_egress: bool,
    pub side_effecting: bool,
    pub version: i32,
    pub timeout_secs: Option<i32>,
}

impl CustomToolRow {
    /// The OpenAI function definition advertised to the model.
    fn def(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.params_schema,
            }
        })
    }
}

/// Load the enabled + approved custom tools an Agent has selected, returning both
/// their OpenAI defs (advertised order = name) and a name→row map for dispatch. A
/// disabled, unapproved or edited-but-unapproved tool (`approved_version != version`)
/// is neither advertised nor dispatchable. Empty `agent_tools` ⇒ no query, empty
/// result (keeps the zero-custom per-turn defs byte-identical).
pub async fn load_enabled_custom(
    pg: &sqlx::PgPool,
    agent_tools: &[String],
) -> (Vec<Value>, HashMap<String, CustomToolRow>) {
    if agent_tools.is_empty() {
        return (Vec::new(), HashMap::new());
    }
    let rows = sqlx::query!(
        r#"SELECT id, name, display_name, description, kind, params_schema, config,
                  auth_value_enc, requires_egress, side_effecting, version, timeout_secs
             FROM custom_tools
            WHERE enabled AND approved_version = version AND name = ANY($1)
            ORDER BY name"#,
        agent_tools
    )
    .fetch_all(pg)
    .await
    .unwrap_or_default();

    let mut defs = Vec::with_capacity(rows.len());
    let mut map = HashMap::with_capacity(rows.len());
    for r in rows {
        let row = CustomToolRow {
            id: r.id,
            name: r.name,
            display_name: r.display_name,
            description: r.description,
            kind: r.kind,
            params_schema: r.params_schema,
            config: r.config,
            auth_value_enc: r.auth_value_enc,
            requires_egress: r.requires_egress,
            side_effecting: r.side_effecting,
            version: r.version,
            timeout_secs: r.timeout_secs,
        };
        defs.push(row.def());
        map.insert(row.name.clone(), row);
    }
    (defs, map)
}

/// Load one custom tool by id regardless of enabled/approved state — for the admin
/// Test-run path (an admin tests a tool BEFORE approving it).
pub async fn load_by_id(pg: &sqlx::PgPool, id: Uuid) -> Option<CustomToolRow> {
    let r = sqlx::query!(
        r#"SELECT id, name, display_name, description, kind, params_schema, config,
                  auth_value_enc, requires_egress, side_effecting, version, timeout_secs
             FROM custom_tools WHERE id = $1"#,
        id
    )
    .fetch_optional(pg)
    .await
    .ok()
    .flatten()?;
    Some(CustomToolRow {
        id: r.id,
        name: r.name,
        display_name: r.display_name,
        description: r.description,
        kind: r.kind,
        params_schema: r.params_schema,
        config: r.config,
        auth_value_enc: r.auth_value_enc,
        requires_egress: r.requires_egress,
        side_effecting: r.side_effecting,
        version: r.version,
        timeout_secs: r.timeout_secs,
    })
}

/// Admin Test-run: execute the tool with hand-entered args under the same egress +
/// SSRF gates as a real call, but WITHOUT the enabled/approved gate (the admin is
/// validating it pre-approval). Returns the result/error text.
pub async fn test_run(
    state: &AppState,
    ctx: &crate::auth::AuthContext,
    row: &CustomToolRow,
    args: &Value,
) -> Result<String> {
    let started = Instant::now();
    let res = match row.kind.as_str() {
        "http" => match integrations::guard_egress(state, ctx, ConnectorKind::CustomTool).await {
            Ok(()) => perform_http(row, args).await,
            Err(e) => Err(e),
        },
        "script" => run_script(state, row, args).await,
        other => Err(AppError::Validation(format!("unknown custom tool kind: {other}"))),
    };
    let ms = started.elapsed().as_millis() as i128;
    let (text, outcome) = match res {
        Ok(s) => (s, AuditOutcome::Success),
        Err(e) => (format!("error: {e}"), AuditOutcome::Failure),
    };
    audit_custom_call(state, ctx.user_id, ctx.role.as_str(), Uuid::nil(), row, args, &text, ms, outcome)
        .await;
    Ok(text)
}

/// Run a script custom tool in the Firecracker sandbox (zero-network). Parameters
/// ride as a working-dir `params.json` file (the sandbox has no stdin channel);
/// the script's stdout is returned to the model. `code_interpreter::execute`
/// returns `Err(Unavailable)` when the sandbox is off / the host is non-Linux,
/// which the caller folds into `error: …` text.
async fn run_script(state: &AppState, row: &CustomToolRow, args: &Value) -> Result<String> {
    let source = row
        .config
        .get("source")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| AppError::Validation("script tool has no source".into()))?;
    let params = serde_json::to_vec(args).unwrap_or_else(|_| b"{}".to_vec());
    let req = crate::code_interpreter::ExecRequest {
        language: "python".into(),
        code: source.to_string(),
        inputs: vec![crate::code_interpreter::InputFile { name: "params.json".into(), bytes: params }],
    };
    let r = crate::code_interpreter::execute(state, req).await?;
    let out = if r.exit_code == 0 {
        truncate(&r.stdout, RESPONSE_CAP)
    } else {
        truncate(
            &format!("exit {}\nstdout:\n{}\nstderr:\n{}", r.exit_code, r.stdout, r.stderr),
            RESPONSE_CAP,
        )
    };
    Ok(out)
}

/// Defence in depth: is this exact version still enabled + approved right now? The
/// per-turn snapshot could be stale if an admin edited the tool mid-turn (mirrors
/// MCP re-querying `status='active'` inside dispatch).
async fn still_live(pg: &sqlx::PgPool, id: Uuid, version: i32) -> bool {
    sqlx::query_scalar!(
        r#"SELECT EXISTS(
             SELECT 1 FROM custom_tools
              WHERE id = $1 AND version = $2 AND enabled AND approved_version = version
           ) AS "e!""#,
        id,
        version
    )
    .fetch_one(pg)
    .await
    .unwrap_or(false)
}

/// Live dispatch (inside the agentic loop). Returns the tool result as text; every
/// error is folded into `error: …` text (never propagated) so the model recovers.
pub async fn dispatch_custom(
    state: &AppState,
    ctx: &crate::auth::AuthContext,
    chat_id: Uuid,
    row: &CustomToolRow,
    args: &Value,
) -> Result<String> {
    if !still_live(&state.pg, row.id, row.version).await {
        return Ok("error: this tool was changed or disabled and needs an administrator to re-approve it".into());
    }
    let started = Instant::now();
    let res = match row.kind.as_str() {
        // http crosses the perimeter → the single zero-egress choke-point, then the request.
        "http" => match integrations::guard_egress(state, ctx, ConnectorKind::CustomTool).await {
            Ok(()) => perform_http(row, args).await,
            Err(e) => Err(e),
        },
        // script runs in the zero-egress Firecracker VM → no egress gate.
        "script" => run_script(state, row, args).await,
        other => Err(AppError::Validation(format!("unknown custom tool kind: {other}"))),
    };
    let ms = started.elapsed().as_millis() as i128;
    let (text, outcome) = match res {
        Ok(s) => (s, AuditOutcome::Success),
        Err(e) => (format!("error: {e}"), AuditOutcome::Failure),
    };
    audit_custom_call(state, ctx.user_id, ctx.role.as_str(), chat_id, row, args, &text, ms, outcome).await;
    Ok(text)
}

/// Durable/unattended resume of an approved custom call (no live loop to stream
/// into). Runs under the same egress choke-point as the live path (`guard_egress` for
/// http, which refuses + audits when the connector is dormant); the caller has already
/// enforced the agent grant + the enabled/approved gate. Audits the same event.
pub async fn dispatch_custom_durable(
    state: &AppState,
    ctx: &crate::auth::AuthContext,
    chat_id: Uuid,
    row: &CustomToolRow,
    args: &Value,
) {
    let started = Instant::now();
    let res = match row.kind.as_str() {
        // http crosses the perimeter → the single zero-egress choke-point, then the request.
        "http" => match integrations::guard_egress(state, ctx, ConnectorKind::CustomTool).await {
            Ok(()) => perform_http(row, args).await,
            Err(e) => Err(e),
        },
        "script" => run_script(state, row, args).await, // zero-egress VM, no gate
        other => Err(AppError::Validation(format!("unknown custom tool kind: {other}"))),
    };
    let ms = started.elapsed().as_millis() as i128;
    let (text, outcome) = match res {
        Ok(s) => (s, AuditOutcome::Success),
        Err(e) => (format!("error: {e}"), AuditOutcome::Failure),
    };
    let status = if matches!(outcome, AuditOutcome::Success) { "ok" } else { "error" };
    audit_custom_call(state, ctx.user_id, ctx.role.as_str(), chat_id, row, args, &text, ms, outcome).await;
    metrics::counter!("tool_calls_total", "tool" => row.name.clone(), "kind" => "custom", "status" => status)
        .increment(1);
}

/// The declarative HTTP call: substitution → SSRF → hardened client → capped
/// response → extraction. Assumes the caller already passed the egress gate.
async fn perform_http(row: &CustomToolRow, args: &Value) -> Result<String> {
    let cfg = &row.config;
    let method = cfg.get("method").and_then(|v| v.as_str()).unwrap_or("GET").to_uppercase();
    let url_tmpl = cfg
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::Validation("custom tool has no url".into()))?;
    let url = substitute(url_tmpl, args)?;

    // Same dual-mode SSRF guard as MCP: a private tool must resolve private, a
    // remote tool only reaches a public HTTPS host; metadata/link-local always refused.
    crate::mcp::validate::validate_endpoint(&url, row.requires_egress)?;

    let mut headers = reqwest::header::HeaderMap::new();
    if let Some(obj) = cfg.get("headers").and_then(|v| v.as_object()) {
        for (k, v) in obj {
            let val = substitute(v.as_str().unwrap_or_default(), args)?;
            let hn = reqwest::header::HeaderName::from_bytes(k.as_bytes())
                .map_err(|e| AppError::Validation(format!("invalid header name '{k}': {e}")))?;
            let hv = reqwest::header::HeaderValue::from_str(&val)
                .map_err(|e| AppError::Validation(format!("invalid header value: {e}")))?;
            headers.insert(hn, hv);
        }
    }
    // Auth secret from the keyring, injected as a sensitive default header.
    if let Some(auth) = cfg.get("auth") {
        let atype = auth.get("type").and_then(|v| v.as_str()).unwrap_or("none");
        if atype != "none" {
            let secret = match &row.auth_value_enc {
                Some(enc) => crate::crypto::decrypt_at_rest(enc)?,
                None => return Err(AppError::Validation("auth configured but no secret stored".into())),
            };
            let (hname, hval) = match atype {
                "bearer" => ("authorization".to_string(), format!("Bearer {secret}")),
                "header" => (
                    auth.get("header_name")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| AppError::Validation("header auth needs a header_name".into()))?
                        .to_string(),
                    secret,
                ),
                other => return Err(AppError::Validation(format!("unknown auth type: {other}"))),
            };
            let hn = reqwest::header::HeaderName::from_bytes(hname.as_bytes())
                .map_err(|e| AppError::Validation(format!("invalid auth header name: {e}")))?;
            let mut hv = reqwest::header::HeaderValue::from_str(&hval)
                .map_err(|e| AppError::Validation(format!("invalid auth header value: {e}")))?;
            hv.set_sensitive(true);
            headers.insert(hn, hv);
        }
    }

    let timeout =
        Duration::from_secs(row.timeout_secs.map(|s| s.max(1) as u64).unwrap_or(DEFAULT_TIMEOUT_SECS));
    // Fresh, hardened client: forbid redirects (a 3xx could reach a private host
    // with the secret header attached) and disable proxies.
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .no_proxy()
        .default_headers(headers)
        .build()
        .map_err(|e| AppError::Other(anyhow::anyhow!("build http client: {e}")))?;

    let m = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|_| AppError::Validation(format!("invalid method: {method}")))?;
    let mut req = client.request(m, &url);
    if let Some(body_tmpl) = cfg.get("body").and_then(|v| v.as_str()) {
        req = req.body(substitute(body_tmpl, args)?);
    }
    let resp = req.send().await.map_err(|e| AppError::Other(anyhow::anyhow!("request failed: {e}")))?;
    let status = resp.status();
    let body = read_capped(resp).await?;

    if !status.is_success() {
        return Ok(format!("HTTP {}: {}", status.as_u16(), truncate(&body, 1000)));
    }
    let mode = cfg.get("response").and_then(|v| v.get("mode")).and_then(|v| v.as_str()).unwrap_or("raw");
    let out = if mode == "pointer" {
        let ptr = cfg
            .get("response")
            .and_then(|v| v.get("pointer"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let parsed: Value = serde_json::from_str(&body)
            .map_err(|e| AppError::Validation(format!("response is not JSON: {e}")))?;
        match parsed.pointer(ptr) {
            Some(Value::String(s)) => s.clone(),
            Some(v) => v.to_string(),
            None => return Ok(format!("error: response has no value at pointer '{ptr}'")),
        }
    } else {
        truncate(&body, RESPONSE_CAP)
    };
    Ok(out)
}

/// Read a response body, hard-capping at [`RESPONSE_CAP`] bytes (streamed so an
/// oversized body is never fully buffered).
async fn read_capped(resp: reqwest::Response) -> Result<String> {
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| AppError::Other(anyhow::anyhow!("read body: {e}")))?;
        let room = RESPONSE_CAP.saturating_sub(buf.len());
        if room == 0 {
            break;
        }
        let take = room.min(chunk.len());
        buf.extend_from_slice(&chunk[..take]);
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Replace every `{{param}}` in a template with the matching argument. An
/// unresolved placeholder is an error (never leaves `{{…}}` in a request).
pub fn substitute(tmpl: &str, args: &Value) -> Result<String> {
    let mut out = String::with_capacity(tmpl.len());
    let mut rest = tmpl;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after
            .find("}}")
            .ok_or_else(|| AppError::Validation("unterminated '{{' in template".into()))?;
        let key = after[..end].trim();
        let val = args
            .get(key)
            .ok_or_else(|| AppError::Validation(format!("missing parameter '{key}'")))?;
        let s = match val {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        out.push_str(&s);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [truncated]", &s[..end])
}

#[allow(clippy::too_many_arguments)]
async fn audit_custom_call(
    state: &AppState,
    actor: Option<Uuid>,
    role: &str,
    chat_id: Uuid,
    row: &CustomToolRow,
    args: &Value,
    result: &str,
    ms: i128,
    outcome: AuditOutcome,
) {
    // Hash args + result rather than storing raw text (A2 hygiene).
    let args_hash = hex::encode(Sha256::digest(serde_json::to_vec(args).unwrap_or_default()));
    let result_hash = hex::encode(Sha256::digest(result.as_bytes()));
    let mut ev = AuditEvent::action("tool.custom.call", role);
    ev.actor_user_id = actor;
    ev.resource_type = Some("custom_tool".into());
    ev.resource_id = Some(row.id);
    ev.outcome = outcome;
    ev.payload = Some(json!({
        "chat_id": chat_id, "tool": row.name, "version": row.version,
        "args_hash": args_hash, "result_hash": result_hash,
        "result_bytes": result.len(), "latency_ms": ms,
    }));
    let _ = audit::append(&state.pg, &ev).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitute_fills_and_rejects_missing() {
        let args = json!({ "city": "Paris", "n": 3 });
        assert_eq!(
            substitute("https://api/x?q={{city}}&k={{n}}", &args).unwrap(),
            "https://api/x?q=Paris&k=3"
        );
        // A missing parameter is an error, not a literal `{{…}}` in the request.
        assert!(substitute("https://api/{{missing}}", &args).is_err());
        // Unterminated placeholder is rejected.
        assert!(substitute("https://api/{{oops", &args).is_err());
        // No placeholders → passthrough.
        assert_eq!(substitute("https://api/plain", &args).unwrap(), "https://api/plain");
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        let s = "abcdef";
        assert_eq!(truncate(s, 100), "abcdef");
        assert!(truncate(s, 3).starts_with("abc"));
    }

    #[test]
    fn def_shape_is_openai_function() {
        let row = CustomToolRow {
            id: Uuid::now_v7(),
            name: "fx_rate".into(),
            display_name: "FX rate".into(),
            description: "Get a rate".into(),
            kind: "http".into(),
            params_schema: json!({ "type": "object", "properties": { "pair": { "type": "string" } } }),
            config: json!({}),
            auth_value_enc: None,
            requires_egress: true,
            side_effecting: false,
            version: 1,
            timeout_secs: None,
        };
        let d = row.def();
        assert_eq!(d["type"], "function");
        assert_eq!(d["function"]["name"], "fx_rate");
        assert_eq!(d["function"]["parameters"]["type"], "object");
    }
}
