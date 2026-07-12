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

//! Ephemeral super-admin — break-glass, OUTSIDE Keycloak (one of the two admin
//! levels).
//!
//! Zero standing privilege: a grant is minted just-in-time (host/CLI only), lives
//! only in Redis with a capped TTL (auto-revoke on expiry), and every issue /
//! revoke / use / DENIED attempt is written to the hash-chain audit with the
//! source IP. The grant is a 256-bit CSPRNG token (a real secret, not a UUID).
//! Independent of Keycloak by design — it can repair a broken Keycloak.
//!
//! Use: present `X-Break-Glass: <token>` — the [`SuperAdmin`] extractor rate-limits
//! by source IP, validates the token, audits, and yields a super-admin context.

use std::net::SocketAddr;

use aes_gcm::aead::{rand_core::RngCore, OsRng};
use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::request::Parts;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use deadpool_redis::redis;
use serde_json::json;

use crate::audit::{self, AuditEvent};
use crate::auth::{AuthContext, PlatformRole};
use crate::error::AppError;
use crate::state::AppState;

const HEADER: &str = "x-break-glass";
/// Per-source-IP cap on *failed* break-glass attempts before lock-out. Only wrong
/// tokens count, so a legitimate operator (whose calls succeed) is never throttled,
/// while a brute-forcer is stopped and made visible after a few tries.
const FAIL_MAX: i64 = 10;
const FAIL_WINDOW_SECS: u64 = 60;
/// Sanity bounds on the presented token (the real token is ~43 chars).
const TOKEN_MIN: usize = 16;
const TOKEN_MAX: usize = 128;

fn key(token: &str) -> String {
    format!("pai:breakglass:{token}")
}

/// Short, non-secret fingerprint — correlates a grant across audit rows without
/// ever recording the secret itself.
fn fingerprint(token: &str) -> String {
    token.chars().take(8).collect()
}

/// A fresh 256-bit CSPRNG grant token (base64url, no padding ≈ 43 chars). A full
/// random secret — not a UUID — since it is the sole credential for super-admin.
fn new_token() -> String {
    let mut b = [0u8; 32];
    OsRng.fill_bytes(&mut b);
    URL_SAFE_NO_PAD.encode(b)
}

/// Mint a break-glass grant. `ttl_secs` is capped at `breakglass_max_ttl_secs`
/// (ephemeral by design — a long TTL would recreate standing privilege). Returns
/// the token the caller must present. Audited `breakglass.issued` (fingerprint only).
pub async fn issue(
    state: &AppState,
    ttl_secs: u64,
    label: &str,
    reason: &str,
) -> Result<String, AppError> {
    let max = state.boot.breakglass_max_ttl_secs;
    if ttl_secs == 0 || ttl_secs > max {
        return Err(AppError::Validation(format!(
            "ttl must be between 1 and {max} seconds (break-glass is ephemeral by design)"
        )));
    }
    let token = new_token();
    let value = json!({
        "label": label,
        "reason": reason,
        "issued_at": time::OffsetDateTime::now_utc().unix_timestamp(),
    })
    .to_string();

    let mut conn = state.redis.get().await?;
    redis::cmd("SET")
        .arg(key(&token))
        .arg(value)
        .arg("EX")
        .arg(ttl_secs)
        .query_async::<()>(&mut conn)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("redis SET failed: {e}")))?;

    let mut event = AuditEvent::action("breakglass.issued", PlatformRole::SuperAdmin.as_str());
    event.resource_type = Some("breakglass_grant".into());
    event.payload =
        Some(json!({ "grant_fp": fingerprint(&token), "label": label, "reason": reason, "ttl_secs": ttl_secs }));
    event.risk_anomaly_flag = true; // break-glass use is always notable
    audit::append(&state.pg, &event).await?;

    Ok(token)
}

/// Is the grant currently active (present and not expired)? A token outside the
/// sane length bounds is rejected without touching Redis.
pub async fn validate(state: &AppState, token: &str) -> Result<bool, AppError> {
    if !(TOKEN_MIN..=TOKEN_MAX).contains(&token.len()) {
        return Ok(false);
    }
    let mut conn = state.redis.get().await?;
    let exists: i64 = redis::cmd("EXISTS")
        .arg(key(token))
        .query_async(&mut conn)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("redis EXISTS failed: {e}")))?;
    Ok(exists > 0)
}

/// A currently-active grant, for the CLI `list` and the read-only grants route.
/// `grant_id` carries the token (the panel already holds it; not re-exposed).
#[derive(Debug, serde::Serialize)]
pub struct ActiveGrant {
    pub grant_id: String,
    /// Seconds left before auto-expiry.
    pub ttl_secs: i64,
    pub label: Option<String>,
    pub reason: Option<String>,
}

/// List active break-glass grants by scanning the Redis keyspace. Best-effort: a
/// key that expires between SCAN and the read is simply skipped.
pub async fn list_active(state: &AppState) -> Result<Vec<ActiveGrant>, AppError> {
    let mut conn = state.redis.get().await?;
    let mut cursor: u64 = 0;
    let mut out = Vec::new();
    loop {
        let (next, keys): (u64, Vec<String>) = redis::cmd("SCAN")
            .arg(cursor)
            .arg("MATCH")
            .arg("pai:breakglass:*")
            .arg("COUNT")
            .arg(200)
            .query_async(&mut conn)
            .await
            .map_err(|e| AppError::Other(anyhow::anyhow!("redis SCAN failed: {e}")))?;
        for k in keys {
            let ttl: i64 = redis::cmd("TTL").arg(&k).query_async(&mut conn).await.unwrap_or(-2);
            if ttl < 0 {
                continue;
            }
            let raw: Option<String> = redis::cmd("GET")
                .arg(&k)
                .query_async::<Option<String>>(&mut conn)
                .await
                .ok()
                .flatten();
            let (label, reason) = raw
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .map(|v| {
                    (
                        v.get("label").and_then(|x| x.as_str()).map(String::from),
                        v.get("reason").and_then(|x| x.as_str()).map(String::from),
                    )
                })
                .unwrap_or((None, None));
            if let Some(token) = k.strip_prefix("pai:breakglass:") {
                out.push(ActiveGrant { grant_id: token.to_string(), ttl_secs: ttl, label, reason });
            }
        }
        if next == 0 {
            break;
        }
        cursor = next;
    }
    Ok(out)
}

/// Audit a CLI listing of active grants — a notable read of break-glass state.
pub async fn audit_listed(state: &AppState, count: usize) {
    let mut ev = AuditEvent::action("breakglass.listed", PlatformRole::SuperAdmin.as_str());
    ev.resource_type = Some("breakglass_grant".into());
    ev.risk_anomaly_flag = true;
    ev.payload = Some(json!({ "count": count, "via": "cli" }));
    let _ = audit::append(&state.pg, &ev).await;
}

/// Revoke a grant before its TTL. Audited `breakglass.revoked`.
pub async fn revoke(state: &AppState, token: &str) -> Result<(), AppError> {
    let mut conn = state.redis.get().await?;
    redis::cmd("DEL")
        .arg(key(token))
        .query_async::<i64>(&mut conn)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("redis DEL failed: {e}")))?;

    let mut event = AuditEvent::action("breakglass.revoked", PlatformRole::SuperAdmin.as_str());
    event.resource_type = Some("breakglass_grant".into());
    event.payload = Some(json!({ "grant_fp": fingerprint(token) }));
    audit::append(&state.pg, &event).await?;
    Ok(())
}

// --- CLI (shared by the `fosnie-backend` and `fosnie-enterprise` binaries) -----------

/// Ephemeral super-admin (break-glass) grant administration subcommands. Talks
/// directly to Redis/Postgres, so it works even when the HTTP server or Keycloak is
/// down (one of break-glass's purposes is repairing a broken platform).
#[derive(clap::Subcommand)]
pub enum BreakglassCmd {
    /// Mint a grant; prints the grant id to present via `X-Break-Glass`.
    Issue {
        /// Lifetime in seconds (default: config `breakglass_default_ttl_secs`, 1800).
        #[arg(long)]
        ttl: Option<u64>,
        /// Short label for the audit trail (who/what this session is for).
        #[arg(long, default_value = "cli")]
        label: String,
        /// Reason for the audit trail.
        #[arg(long, default_value = "manual break-glass session")]
        reason: String,
    },
    /// Revoke an active grant by id (before its TTL).
    Revoke { grant_id: String },
    /// List currently-active grants and their remaining TTL.
    List,
}

/// Run a break-glass CLI action: connect Redis + Postgres directly (no HTTP layer,
/// no Keycloak) and execute the requested grant operation. Audited via the same
/// hash-chain the server uses. Shared by both binaries.
pub async fn run_cli(boot: crate::config::BootConfig, action: BreakglassCmd) -> anyhow::Result<()> {
    use anyhow::Context;
    crate::audit::init_signing(&boot.audit_signing_key);
    let pg = crate::db::connect(&boot.database_url, 2).await.context("connecting to Postgres")?;
    let redis = crate::cache::create_pool(&boot.redis_url).context("building Redis pool")?;
    let boot = std::sync::Arc::new(boot);
    let state = AppState::new(pg, redis, boot.clone());

    match action {
        BreakglassCmd::Issue { ttl, label, reason } => {
            let ttl = ttl.unwrap_or(boot.breakglass_default_ttl_secs);
            let grant = issue(&state, ttl, &label, &reason)
                .await
                .context("issuing break-glass grant")?;
            println!("break-glass grant issued");
            println!("  token   : {grant}");
            println!("  ttl     : {ttl}s (~{} min)", ttl / 60);
            println!("  present : X-Break-Glass: {grant}");
        }
        BreakglassCmd::Revoke { grant_id } => {
            let token = grant_id.trim();
            revoke(&state, token).await.context("revoking grant")?;
            println!("revoked grant {}…", &token.chars().take(8).collect::<String>());
        }
        BreakglassCmd::List => {
            let grants = list_active(&state).await.context("listing grants")?;
            audit_listed(&state, grants.len()).await;
            if grants.is_empty() {
                println!("no active break-glass grants");
            } else {
                println!("active break-glass grants ({}):", grants.len());
                for g in grants {
                    println!(
                        "  {}  ttl {}s (~{} min)  label={}  reason={}",
                        g.grant_id,
                        g.ttl_secs,
                        g.ttl_secs / 60,
                        g.label.as_deref().unwrap_or("-"),
                        g.reason.as_deref().unwrap_or("-"),
                    );
                }
            }
        }
    }
    Ok(())
}

/// Best-effort source IP for audit + rate-limiting: the TCP peer (when the server
/// is run with `ConnectInfo`), else a forwarding header, else `"unknown"`.
pub(crate) fn source_ip(parts: &Parts) -> String {
    if let Some(ConnectInfo(addr)) = parts.extensions.get::<ConnectInfo<SocketAddr>>() {
        return addr.ip().to_string();
    }
    for h in ["x-forwarded-for", "x-real-ip"] {
        if let Some(first) = parts.headers.get(h).and_then(|v| v.to_str().ok()).and_then(|v| v.split(',').next()) {
            let t = first.trim();
            if !t.is_empty() {
                return t.to_string();
            }
        }
    }
    "unknown".into()
}

async fn audit_attempt(state: &AppState, action: &str, ip: &str, fp: Option<&str>) {
    let mut ev = AuditEvent::action(action, PlatformRole::SuperAdmin.as_str());
    ev.resource_type = Some("breakglass_grant".into());
    ev.risk_anomaly_flag = true;
    ev.payload = Some(json!({ "ip": ip, "grant_fp": fp }));
    let _ = audit::append(&state.pg, &ev).await;
}

/// Extractor for break-glass-gated routes. Validates the `X-Break-Glass` token
/// against the store, throttles + audits failed attempts by source IP, audits the
/// use, and yields a super-admin context. Works even if Keycloak is down.
pub struct SuperAdmin(pub AuthContext);

impl FromRequestParts<AppState> for SuperAdmin {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let ip = source_ip(parts);

        let raw = parts
            .headers
            .get(HEADER)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| AppError::Unauthorized("missing X-Break-Glass header".into()))?;
        let token = raw.trim();

        if !validate(state, token).await? {
            let fp = fingerprint(token);
            // Only failed attempts count toward the lock-out, so a real operator is
            // never throttled while a brute-forcer is stopped + made visible.
            if !crate::cache::rate_limit_ok(&state.redis, &format!("bg-fail:{ip}"), FAIL_MAX, FAIL_WINDOW_SECS).await {
                audit_attempt(state, "breakglass.locked_out", &ip, Some(&fp)).await;
                return Err(AppError::TooManyRequests(
                    "too many break-glass attempts from this address — locked out, slow down".into(),
                ));
            }
            audit_attempt(state, "breakglass.denied", &ip, Some(&fp)).await;
            return Err(AppError::Forbidden(
                "break-glass grant is not active (expired or revoked)".into(),
            ));
        }

        // Every successful use is notable and audited, with the source IP.
        let mut event = AuditEvent::action("breakglass.used", PlatformRole::SuperAdmin.as_str());
        event.resource_type = Some("breakglass_grant".into());
        event.risk_anomaly_flag = true;
        event.payload = Some(json!({ "ip": ip, "grant_fp": fingerprint(token) }));
        audit::append(&state.pg, &event).await?;

        Ok(SuperAdmin(AuthContext {
            user_id: None,
            email: None,
            display_name: None,
            role: PlatformRole::SuperAdmin,
            break_glass: true,
            mfa_enroll_only: false,
        }))
    }
}
