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

//! Client-side telemetry sink — the SPA reports its own errors here so a
//! browser-side crash is visible to the operator (logs + metrics) instead of
//! being silent. **Intra-perimeter only**: this endpoint logs and meters; it
//! never forwards anything outward (zero-egress). An optional consent-based
//! external error-reporter connector is a deferred follow-up.
//!
//! The route is **public** (no `AuthUser`): a client error frequently happens
//! exactly when auth is unavailable — before login, during a token-refresh
//! failure, on the Login screen, or when Keycloak is down — which is when an
//! authed endpoint would miss it. Abuse is bounded by a small body limit (set
//! on the route) + a per-IP fixed-window rate limit, and every string field is
//! truncated before it is logged.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::Deserialize;

use crate::error::Result;
use crate::state::AppState;

/// One client-side error report. All fields optional/defaulted — a malformed or
/// partial body still meters as an error rather than 400-ing the browser.
#[derive(Debug, Deserialize)]
pub struct ClientErrorReport {
    /// Bounded to a fixed whitelist by the handler (keeps the metric label
    /// cardinality fixed): `error | unhandledrejection | react | chunk`.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub stack: Option<String>,
    /// The SPA route (`location.pathname`) — a log field, never a metric label.
    #[serde(default)]
    pub route: Option<String>,
    #[serde(default)]
    pub user_agent: Option<String>,
    /// Build/release stamp (`__APP_RELEASE__`).
    #[serde(default)]
    pub release: Option<String>,
    /// Client-side timestamp (epoch ms) — accepted for forward-compatibility.
    #[serde(default)]
    pub ts: Option<i64>,
}

/// Truncate to at most `max` bytes on a UTF-8 char boundary.
fn clamp(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Coarse client key for the rate limiter: first hop of `X-Forwarded-For`
/// (the SPA is served same-origin behind a reverse proxy), else a constant.
fn client_key(headers: &HeaderMap) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown")
        .to_string()
}

/// `POST /api/telemetry` — accept a client error report, meter + log it, 204.
pub async fn report(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ClientErrorReport>,
) -> Result<StatusCode> {
    // Operator mute switch — drop ingestion entirely without metering/logging.
    if !state.boot.observability.client_telemetry {
        return Ok(StatusCode::NO_CONTENT);
    }

    // Abuse bound — 60 reports / minute / IP (fail-open coarse guard).
    let ip = client_key(&headers);
    crate::cache::rate_limit_guard(&state.redis, &format!("telemetry:{ip}"), 60, 60).await?;

    // Fixed whitelist → bounded `kind` label cardinality.
    let kind: &'static str = match body.kind.as_str() {
        "error" => "error",
        "unhandledrejection" => "unhandledrejection",
        "react" => "react",
        "chunk" => "chunk",
        _ => "other",
    };
    metrics::counter!("client_errors_total", "kind" => kind).increment(1);

    // Structured, truncated, intra-perimeter. These are the operator's own
    // deployment's client JS errors — logged, never sent outward.
    let route = clamp(body.route.as_deref().unwrap_or("-"), 256);
    let release = clamp(body.release.as_deref().unwrap_or("-"), 256);
    let ua = clamp(body.user_agent.as_deref().unwrap_or("-"), 256);
    let message = clamp(&body.message, 1024);
    let stack = clamp(body.stack.as_deref().unwrap_or(""), 8192);
    tracing::warn!(
        kind = kind,
        route = %route,
        release = %release,
        ua = %ua,
        message = %message,
        stack = %stack,
        "client error reported"
    );

    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_respects_byte_budget_and_char_boundary() {
        assert_eq!(clamp("hello", 10), "hello");
        assert_eq!(clamp("hello", 3), "hel");
        // "é" is 2 bytes — clamping to 1 byte must not split it.
        assert_eq!(clamp("é", 1), "");
        assert_eq!(clamp("aé", 2), "a");
    }

    #[test]
    fn client_key_takes_first_forwarded_hop() {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "203.0.113.5, 10.0.0.1".parse().unwrap());
        assert_eq!(client_key(&h), "203.0.113.5");
        assert_eq!(client_key(&HeaderMap::new()), "unknown");
    }
}
