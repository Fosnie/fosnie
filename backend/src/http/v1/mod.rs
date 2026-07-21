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

//! The OpenAI-compatible surface: `GET /v1/models` and
//! `POST /v1/chat/completions`, authenticated by a platform API key.
//!
//! Why a compatibility facade at all: the ecosystem of clients that speak this
//! shape is enormous, and pointing one of them at a self-hosted instance turns
//! the platform's governance (authentication, key lifecycle, provider
//! resolution including a user's own keys, rate limiting, audit and metering)
//! into a wrapper around the models that instance already runs.
//!
//! Two modes, distinguished by the `model` field:
//!
//! * **A model label** — a stateless passthrough to that provider. No
//!   conversation is stored; the messages travel verbatim.
//! * **`agent/<uuid>`** — the full chat pipeline for that agent (retrieval,
//!   tools, its skills), so a caller can talk to their own library from any
//!   compatible client. Persistent, see the agent module.
//!
//! Routing note: this surface is mounted **outside** the session-protected
//! router. A key is the credential in every deployment mode, including one
//! where interactive login goes through an identity provider, so it must not
//! sit behind that provider's request layer.

pub mod agent;
pub mod chat;
pub mod error;
pub mod models;
pub mod passthrough;
pub mod sse;

use axum::Router;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::state::AppState;
use error::ApiError;

/// Largest request body accepted. Generous for a conversation of messages,
/// small enough that the surface cannot be used as an upload channel.
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

/// The window the rate limiter counts over, and therefore the `Retry-After` a
/// throttled caller is told to wait.
const RATE_WINDOW_SECS: u64 = 60;

pub fn router(state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/v1/models", axum::routing::get(models::list_models))
        .route("/v1/chat/completions", axum::routing::post(chat::completions))
        // The availability check runs as a layer, ahead of routing and therefore
        // ahead of key authentication: an instance with the surface switched off
        // must not reveal whether a presented key is valid.
        .layer(axum::middleware::from_fn_with_state(state.clone(), enabled_guard))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
}

async fn enabled_guard(State(state): State<AppState>, req: Request, next: Next) -> Response {
    match require_enabled_globally(&state).await {
        Ok(()) => next.run(req).await,
        Err(e) => e.into_response(),
    }
}

/// Cross-origin access for this surface, off unless an administrator turns it on
/// (`api.cors_allow_all`).
///
/// Native clients and server-side SDKs do not need CORS at all, so the default
/// is to send no cross-origin headers and let the browser refuse — the same
/// posture as the rest of the platform. The knob exists for browser-based
/// tooling pointed at a self-hosted instance.
///
/// Hand-rolled rather than delegated to a CORS layer because the decision is a
/// runtime setting and a layer is fixed when the router is built. The
/// permissive form is deliberately credential-free: a key travels in the
/// `Authorization` header, never in an ambient cookie, so `*` cannot be used to
/// ride a logged-in browser session.
pub async fn cors(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let allow = cors_allow_all(&state).await;
    if !allow {
        return next.run(req).await;
    }
    // Preflight is answered here: the request never reaches a route (it carries
    // no key), so there is nothing to authenticate.
    if req.method() == Method::OPTIONS {
        let mut res = Response::new(axum::body::Body::empty());
        *res.status_mut() = StatusCode::NO_CONTENT;
        put_cors_headers(&mut res);
        return res;
    }
    let mut res = next.run(req).await;
    put_cors_headers(&mut res);
    res
}

fn put_cors_headers(res: &mut Response) {
    let h = res.headers_mut();
    h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    h.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, POST, OPTIONS"),
    );
    h.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("authorization, content-type, x-fosnie-chat-id"),
    );
    h.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("x-fosnie-chat-id"),
    );
    h.insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("600"));
}

async fn cors_allow_all(state: &AppState) -> bool {
    crate::config::runtime::get(&state.pg, "api.cors_allow_all")
        .await
        .ok()
        .flatten()
        .map(|e| e.value == "true")
        .unwrap_or(false)
}

/// Is the programmatic surface available to this caller at all?
///
/// Checked twice per request, deliberately: once globally before
/// authentication (so an instance with the surface switched off does not even
/// reveal whether a key is valid), then once for the resolved user (so a group
/// restriction applies). Both answer 404 rather than 403 — the endpoint is
/// absent, not forbidden.
pub(crate) async fn require_enabled_globally(state: &AppState) -> Result<(), ApiError> {
    // `None` for the user id evaluates the deployment ceiling only.
    if state.features.enabled_for_user(state, None, "public_api").await {
        Ok(())
    } else {
        Err(ApiError::not_found("not found"))
    }
}

pub(crate) async fn require_enabled_for(
    state: &AppState,
    ctx: &crate::auth::AuthContext,
) -> Result<(), ApiError> {
    if state.features.enabled_for(state, ctx, "public_api").await {
        Ok(())
    } else {
        Err(ApiError::not_found("not found"))
    }
}

/// Per-key request throttle.
///
/// A coarse abuse guard, not a quota: the underlying limiter fails **open** if
/// Redis is unreachable, on the grounds that availability beats strictness for
/// a guard of this kind. Anything that must be enforced (permissions, ACLs)
/// lives in the request path proper, not here.
pub(crate) async fn rate_limit(state: &AppState, key_id: uuid::Uuid) -> Result<(), ApiError> {
    let max = crate::config::runtime::get(&state.pg, "api.rate_per_min")
        .await
        .ok()
        .flatten()
        .and_then(|e| e.value.parse::<i64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(60);
    if crate::cache::rate_limit_ok(
        &state.redis,
        &format!("apikey:{key_id}"),
        max,
        RATE_WINDOW_SECS,
    )
    .await
    {
        Ok(())
    } else {
        Err(ApiError::rate_limited(RATE_WINDOW_SECS))
    }
}
