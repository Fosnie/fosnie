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

//! Local email/password auth endpoints (open-core). Mounted only as
//! plain `/api/auth/*` routes — they need no Keycloak middleware. The session is
//! delivered as an `httpOnly` cookie; `register`/`login`/`logout`/`config` are
//! public, `password` is behind the `AuthUser` extractor (which, in local mode,
//! resolves the cookie via [`crate::auth::local::LocalAuthProvider`]).

use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::auth::keycloak::AuthUser;
use crate::auth::local::{self, SESSION_COOKIE};
use crate::auth::{mfa, PlatformRole};
use crate::config::AuthMode;
use crate::error::AppError;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct RegisterBody {
    email: String,
    password: String,
    #[serde(default)]
    display_name: Option<String>,
}

#[derive(Deserialize)]
pub struct LoginBody {
    email: String,
    password: String,
}

#[derive(Deserialize)]
pub struct PasswordBody {
    current_password: String,
    new_password: String,
}

/// Per-IP cap on failed logins before lock-out (mirrors break-glass `FAIL_MAX`).
const LOGIN_FAIL_MAX: i64 = 10;
const LOGIN_FAIL_WINDOW_SECS: u64 = 60;

/// True when the deployment's public URL is https — only then mark the cookie
/// `Secure` (so dev over http://localhost still receives it).
fn cookie_secure(state: &AppState) -> bool {
    state.boot.server.public_url.to_ascii_lowercase().starts_with("https://")
}

/// Build the `Set-Cookie` value carrying a fresh session token.
fn set_cookie(state: &AppState, token: &str) -> String {
    let ttl = state.boot.auth.session_ttl_secs;
    let mut c = format!("{SESSION_COOKIE}={token}; HttpOnly; SameSite=Lax; Path=/; Max-Age={ttl}");
    if cookie_secure(state) {
        c.push_str("; Secure");
    }
    c
}

/// Build the `Set-Cookie` value that clears the session cookie.
fn clear_cookie(state: &AppState) -> String {
    let mut c = format!("{SESSION_COOKIE}=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0");
    if cookie_secure(state) {
        c.push_str("; Secure");
    }
    c
}

/// Attach a `Set-Cookie` header to a JSON body response.
fn json_with_cookie(body: serde_json::Value, cookie: String) -> Response {
    let mut resp = Json(body).into_response();
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        resp.headers_mut().append(header::SET_COOKIE, v);
    }
    resp
}

/// Best-effort source IP for the login rate-limit, read from the proxy forwarding
/// headers (the platform sits behind a reverse proxy in any real deployment),
/// else `"unknown"` (a single shared bucket — still throttles a brute-forcer).
fn source_ip(headers: &HeaderMap) -> String {
    for h in ["x-forwarded-for", "x-real-ip"] {
        if let Some(first) = headers.get(h).and_then(|v| v.to_str().ok()).and_then(|v| v.split(',').next()) {
            let t = first.trim();
            if !t.is_empty() {
                return t.to_string();
            }
        }
    }
    "unknown".into()
}

fn valid_email(email: &str) -> bool {
    let e = email.trim();
    e.len() >= 3 && e.contains('@') && !e.contains(' ')
}

/// `POST /api/auth/register` — create a local user and start a session.
pub async fn register(
    State(state): State<AppState>,
    Json(body): Json<RegisterBody>,
) -> Result<Response, AppError> {
    if state.boot.auth.mode != AuthMode::Local {
        return Err(AppError::Forbidden("local registration is disabled".into()));
    }
    // The first account bootstraps the instance (becomes admin below); after that,
    // self-registration is closed unless an admin re-opens it via the runtime
    // `auth.allow_registration` toggle (default off). Stops a public deployment from
    // being open to any visitor/bot that finds the IP.
    let first = local::first_user_is_admin(&state.pg).await?;
    if !first && !allow_registration(&state).await {
        return Err(AppError::Forbidden("new registrations are disabled".into()));
    }
    let email = body.email.trim().to_lowercase();
    if !valid_email(&email) {
        return Err(AppError::Validation("a valid email is required".into()));
    }
    if body.password.len() < state.boot.auth.password_min_len {
        return Err(AppError::Validation(format!(
            "password must be at least {} characters",
            state.boot.auth.password_min_len
        )));
    }
    if local::email_taken(&state.pg, &email).await? {
        return Err(AppError::Conflict("an account with this email already exists".into()));
    }
    // Seat gate (extension seam): block a new account past the deployment's licensed
    // seat limit. Core default is unlimited; an Enterprise licence policy enforces the
    // signed `seats` claim. The bootstrap admin (count 0) always passes.
    state.seats.allow_new_user(&state.pg).await?;
    // First registrant becomes the admin; everyone after is a plain user.
    let role = if first { PlatformRole::ClientAdmin } else { PlatformRole::User };
    let display_name = body
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| email.split('@').next().unwrap_or("user"))
        .to_string();

    let hash = local::hash_password(&body.password)?;
    let user_id = local::register_user(&state.pg, &email, &display_name, &hash, role).await?;
    // A fresh account has no factor yet; if the deployment mandates MFA, the
    // auto-login session is enrolment-only until they complete the wizard (D6/D7).
    let enroll_only = require_mfa(&state).await;
    let token =
        local::issue_session(&state, user_id, state.boot.auth.session_ttl_secs, enroll_only).await?;

    Ok(json_with_cookie(
        json!({ "user_id": user_id, "email": email, "role": role.as_str(), "mfa_enroll_only": enroll_only }),
        set_cookie(&state, &token),
    ))
}

/// Whether the deployment mandates a second factor for everyone —
/// a runtime `config_settings` knob, default off. Fail-soft to off.
async fn require_mfa(state: &AppState) -> bool {
    crate::config::runtime::get(&state.pg, "auth.require_mfa")
        .await
        .ok()
        .flatten()
        .map(|e| e.value == "true")
        .unwrap_or(false)
}

/// Whether self-registration is open beyond the first (bootstrap) account — a
/// runtime `config_settings` knob, **default off**. The first user always registers
/// (bootstraps the admin); everyone after needs an admin to flip this on. Fail-soft
/// to off (closed) so a config read error never opens registration.
async fn allow_registration(state: &AppState) -> bool {
    crate::config::runtime::get(&state.pg, "auth.allow_registration")
        .await
        .ok()
        .flatten()
        .map(|e| e.value == "true")
        .unwrap_or(false)
}

/// `POST /api/auth/login` — verify the password and start a session.
pub async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<LoginBody>,
) -> Result<Response, AppError> {
    if state.boot.auth.mode != AuthMode::Local {
        return Err(AppError::Forbidden("local login is disabled".into()));
    }
    let email = body.email.trim().to_lowercase();
    let ip = source_ip(&headers);

    let cred = local::credential_by_email(&state.pg, &email).await?;
    let ok = match &cred {
        Some(c) if !c.deactivated => c
            .password_hash
            .as_deref()
            .is_some_and(|h| local::verify_password(h, &body.password)),
        _ => false,
    };

    if !ok {
        // Only failed attempts count toward the lock-out, so a legitimate user is
        // never throttled while a brute-forcer is stopped and made visible.
        if !crate::cache::rate_limit_ok(
            &state.redis,
            &format!("login-fail:{ip}"),
            LOGIN_FAIL_MAX,
            LOGIN_FAIL_WINDOW_SECS,
        )
        .await
        {
            return Err(AppError::TooManyRequests(
                "too many failed logins from this address — slow down".into(),
            ));
        }
        audit_login(&state, "auth.login_failed", None, &email).await;
        return Err(AppError::Unauthorized("invalid email or password".into()));
    }

    let user_id = cred.expect("ok implies a credential row").id;
    // load_context re-checks deactivation and yields the canonical role/identity.
    let ctx = crate::auth::load_context(&state.pg, user_id).await?;

    // Second factor: if the user has enrolled MFA, the password
    // alone does NOT mint a session — return a single-use pending token and demand
    // a code at `mfa/verify`. The pending token proves nothing beyond "this user is
    // mid-login".
    if mfa::is_enabled(&state.pg, user_id).await? {
        let pending = mfa::issue_pending(&state, user_id).await?;
        return Ok((
            StatusCode::OK,
            Json(json!({ "mfa_required": true, "pending": pending })),
        )
            .into_response());
    }

    // No enrolled factor. If the deployment mandates MFA, mint a restricted
    // enrolment-only session that can only reach the setup wizard (D6); otherwise a
    // normal session.
    let enroll_only = require_mfa(&state).await;
    let token =
        local::issue_session(&state, user_id, state.boot.auth.session_ttl_secs, enroll_only).await?;
    audit_login_mfa(&state, "auth.login", Some(user_id), &email, Some(false)).await;

    Ok(json_with_cookie(
        json!({ "user_id": user_id, "email": ctx.email, "role": ctx.role.as_str(), "mfa_enroll_only": enroll_only }),
        set_cookie(&state, &token),
    ))
}

/// `POST /api/auth/logout` — revoke the current session and clear the cookie.
pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, AppError> {
    if let Some(token) = read_cookie(&headers) {
        let user_id = local::lookup_session(&state, &token)
            .await
            .ok()
            .flatten()
            .map(|(uid, _)| uid);
        local::revoke_session(&state, &token).await?;
        audit_login(&state, "auth.logout", user_id, "").await;
    }
    Ok(json_with_cookie(json!({ "ok": true }), clear_cookie(&state)))
}

/// Read a runtime-config string value, fail-soft to `None` (missing key / DB blip).
async fn runtime_str(state: &AppState, key: &str) -> Option<String> {
    crate::config::runtime::get(&state.pg, key)
        .await
        .ok()
        .flatten()
        .map(|e| e.value)
        .filter(|v| !v.is_empty())
}

/// `GET /api/auth/config` — PUBLIC. Tells the SPA which login UI to render.
pub async fn config(State(state): State<AppState>) -> impl IntoResponse {
    let mode = match state.boot.auth.mode {
        AuthMode::Local => "local",
        AuthMode::Keycloak => "keycloak",
    };
    let keycloak_url = if state.boot.auth.mode == AuthMode::Keycloak {
        Some(state.boot.keycloak.url.clone())
    } else {
        None
    };
    // Optional SSO button branding (Enterprise federated SSO): a runtime-config
    // label/logo for the customer IdP shown on the SSO button. Absent ⇒ the SPA
    // shows the generic "Sign in with SSO" label. Fail-soft.
    let sso_label = runtime_str(&state, "identity.sso_label").await;
    let sso_logo_url = runtime_str(&state, "identity.sso_logo_url").await;
    // Whether a second factor is mandatory — lets the SPA warn a
    // fresh user before they even register. Not secret. Local mode only.
    let mfa_mandatory = state.boot.auth.mode == AuthMode::Local && require_mfa(&state).await;
    // Whether the SPA should offer a "create an account" path: local mode AND either
    // the instance is empty (the first registrant bootstraps the admin) or an admin
    // has opened self-registration. Off ⇒ the login screen shows only sign-in. Mirrors
    // the register handler's gate so the UI never dangles a button that would 403.
    let registration_open = state.boot.auth.mode == AuthMode::Local
        && (local::first_user_is_admin(&state.pg).await.unwrap_or(false)
            || allow_registration(&state).await);
    Json(json!({
        "mode": mode,
        "local_enabled": state.boot.auth.mode == AuthMode::Local,
        "keycloak_url": keycloak_url,
        "sso_label": sso_label,
        "sso_logo_url": sso_logo_url,
        "require_mfa": mfa_mandatory,
        "registration_open": registration_open,
        // Authoritative minimum local-password length, so the SPA's hints match
        // what the backend actually enforces (register + change-password).
        "password_min_len": state.boot.auth.password_min_len,
    }))
}

/// `POST /api/auth/password` — change the caller's local password.
pub async fn change_password(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<PasswordBody>,
) -> Result<Response, AppError> {
    let Some(user_id) = ctx.user_id else {
        return Err(AppError::Forbidden("no local account".into()));
    };
    if body.new_password.len() < state.boot.auth.password_min_len {
        return Err(AppError::Validation(format!(
            "password must be at least {} characters",
            state.boot.auth.password_min_len
        )));
    }
    let cred = local::credential_by_email(&state.pg, ctx.email.as_deref().unwrap_or_default())
        .await?
        .filter(|c| c.id == user_id);
    let current_ok = cred
        .as_ref()
        .and_then(|c| c.password_hash.as_deref())
        .is_some_and(|h| local::verify_password(h, &body.current_password));
    if !current_ok {
        return Err(AppError::Unauthorized("current password is incorrect".into()));
    }
    let hash = local::hash_password(&body.new_password)?;
    local::set_password_hash(&state.pg, user_id, &hash).await?;

    let mut ev = crate::audit::AuditEvent::action("user.password_changed", ctx.role.as_str());
    ev.actor_user_id = Some(user_id);
    ev.resource_type = Some("user".into());
    ev.resource_id = Some(user_id);
    let _ = crate::audit::append(&state.pg, &ev).await;

    Ok((StatusCode::OK, Json(json!({ "ok": true }))).into_response())
}

// ── Second factor (TOTP) ─────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct MfaConfirmBody {
    code: String,
}

#[derive(Deserialize)]
pub struct MfaDisableBody {
    password: String,
    /// A 6-digit TOTP code or a recovery code.
    code: String,
}

#[derive(Deserialize)]
pub struct MfaVerifyBody {
    pending: String,
    /// A 6-digit TOTP code or a recovery code.
    code: String,
}

#[derive(Deserialize)]
pub struct MfaRegenBody {
    password: String,
    code: String,
}

/// 409 when not in local mode — Keycloak owns its own OTP/WebAuthn (D2).
fn mfa_local_guard(state: &AppState) -> Result<(), AppError> {
    if state.boot.auth.mode != AuthMode::Local {
        return Err(AppError::Conflict(
            "multi-factor authentication is managed by your identity provider".into(),
        ));
    }
    Ok(())
}

fn mfa_verifications(outcome: &'static str) {
    metrics::counter!("auth_mfa_verifications_total", "outcome" => outcome).increment(1);
}

/// Verify the caller's current password (required to disable/regenerate — a stolen
/// session must not be able to weaken the factor, D5).
async fn password_ok(state: &AppState, email: &str, user_id: uuid::Uuid, password: &str) -> bool {
    local::credential_by_email(&state.pg, email)
        .await
        .ok()
        .flatten()
        .filter(|c| c.id == user_id)
        .and_then(|c| c.password_hash)
        .is_some_and(|h| local::verify_password(&h, password))
}

/// `POST /api/auth/mfa/setup` — generate a pending secret + otpauth URL (MFA is NOT
/// yet enabled; the user must confirm a code). Reachable by an enrolment-only session.
pub async fn mfa_setup(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Response, AppError> {
    mfa_local_guard(&state)?;
    let user_id = ctx.user_id.ok_or_else(|| AppError::Forbidden("no local account".into()))?;
    if mfa::is_enabled(&state.pg, user_id).await? {
        return Err(AppError::Conflict("MFA is already enabled — disable it first".into()));
    }
    let email = ctx.email.clone().unwrap_or_default();
    let secret = mfa::gen_secret();
    let otpauth_url = mfa::otpauth_url(&secret, &email)?;
    mfa::set_pending_secret(&state.pg, user_id, &secret).await?;
    Ok(Json(json!({ "otpauth_url": otpauth_url, "secret": secret })).into_response())
}

/// `POST /api/auth/mfa/confirm` — verify a code against the pending secret, enable
/// MFA, issue one-time recovery codes, and revoke every other session. Promotes an
/// enrolment-only session to a full one (fresh cookie).
pub async fn mfa_confirm(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<MfaConfirmBody>,
) -> Result<Response, AppError> {
    mfa_local_guard(&state)?;
    let user_id = ctx.user_id.ok_or_else(|| AppError::Forbidden("no local account".into()))?;
    let email = ctx.email.clone().unwrap_or_default();
    if !mfa::verify_totp(&state.pg, user_id, &email, &body.code).await? {
        mfa_verifications("confirm_fail");
        return Err(AppError::Unauthorized("incorrect code — try again".into()));
    }
    mfa::mark_enabled(&state.pg, user_id).await?;
    let codes = mfa::gen_recovery_codes();
    mfa::store_recovery_codes(&state.pg, user_id, &codes).await?;
    // Revoke everything, then re-mint the current session as a full (non-enrol) one.
    local::revoke_all_for_user(&state, user_id).await?;
    let token =
        local::issue_session(&state, user_id, state.boot.auth.session_ttl_secs, false).await?;
    audit_mfa(&state, "auth.mfa_enrolled", user_id, ctx.role.as_str(), None).await;
    mfa_verifications("confirm_ok");
    Ok(json_with_cookie(
        json!({ "recovery_codes": codes }),
        set_cookie(&state, &token),
    ))
}

/// `POST /api/auth/mfa/disable` — password AND a valid factor required. Clears MFA,
/// revokes other sessions.
pub async fn mfa_disable(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<MfaDisableBody>,
) -> Result<Response, AppError> {
    mfa_local_guard(&state)?;
    let user_id = ctx.user_id.ok_or_else(|| AppError::Forbidden("no local account".into()))?;
    let email = ctx.email.clone().unwrap_or_default();
    if !password_ok(&state, &email, user_id, &body.password).await {
        return Err(AppError::Unauthorized("current password is incorrect".into()));
    }
    if mfa::verify_code_or_recovery(&state.pg, user_id, &email, &body.code).await? == mfa::FactorUsed::None {
        return Err(AppError::Unauthorized("incorrect code".into()));
    }
    mfa::clear(&state.pg, user_id).await?;
    local::revoke_all_for_user(&state, user_id).await?;
    // If the policy still mandates MFA, the replacement session is enrolment-only.
    let enroll_only = require_mfa(&state).await;
    let token =
        local::issue_session(&state, user_id, state.boot.auth.session_ttl_secs, enroll_only).await?;
    audit_mfa(&state, "auth.mfa_disabled", user_id, ctx.role.as_str(), None).await;
    Ok(json_with_cookie(
        json!({ "ok": true, "mfa_enroll_only": enroll_only }),
        set_cookie(&state, &token),
    ))
}

/// `POST /api/auth/mfa/verify` — PUBLIC. Exchange a login pending token + a code for
/// a full session (D3). Rate-limited per pending token AND per IP; the pending token
/// is burned on success or after the failure cap.
pub async fn mfa_verify(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<MfaVerifyBody>,
) -> Result<Response, AppError> {
    mfa_local_guard(&state)?;
    let ip = source_ip(&headers);
    let Some(user_id) = mfa::peek_pending(&state, &body.pending).await? else {
        return Err(AppError::Unauthorized(
            "your login attempt expired — please sign in again".into(),
        ));
    };
    let email = crate::auth::load_context(&state.pg, user_id)
        .await
        .ok()
        .and_then(|c| c.email)
        .unwrap_or_default();

    let used = mfa::verify_code_or_recovery(&state.pg, user_id, &email, &body.code).await?;
    if used == mfa::FactorUsed::None {
        // Failure: count it (per token + per IP). Over the cap ⇒ burn the pending
        // token so the whole login must restart.
        let ok_token = crate::cache::rate_limit_ok(
            &state.redis,
            &format!("mfa-fail:{}", body.pending),
            mfa::VERIFY_FAIL_MAX,
            mfa::VERIFY_FAIL_WINDOW_SECS,
        )
        .await;
        let ok_ip = crate::cache::rate_limit_ok(
            &state.redis,
            &format!("mfa-fail-ip:{ip}"),
            mfa::VERIFY_FAIL_MAX,
            mfa::VERIFY_FAIL_WINDOW_SECS,
        )
        .await;
        mfa_verifications("fail");
        if !ok_token || !ok_ip {
            let _ = mfa::consume_pending(&state, &body.pending).await;
            audit_mfa(&state, "auth.mfa_failed", user_id, "user", Some("locked_out")).await;
            return Err(AppError::TooManyRequests(
                "too many incorrect codes — please sign in again".into(),
            ));
        }
        audit_mfa(&state, "auth.mfa_failed", user_id, "user", Some("bad_code")).await;
        return Err(AppError::Unauthorized("incorrect code — try again".into()));
    }

    // Success: single-use consume + mint the full session.
    let _ = mfa::consume_pending(&state, &body.pending).await;
    let token =
        local::issue_session(&state, user_id, state.boot.auth.session_ttl_secs, false).await?;
    if used == mfa::FactorUsed::Recovery {
        // A spent recovery code is the loss-of-device signal — surface it distinctly.
        audit_mfa(&state, "auth.recovery_used", user_id, "user", None).await;
    }
    audit_login_mfa(&state, "auth.login", Some(user_id), &email, Some(true)).await;
    mfa_verifications("ok");
    Ok(json_with_cookie(
        json!({ "user_id": user_id, "email": email }),
        set_cookie(&state, &token),
    ))
}

/// `POST /api/auth/mfa/recovery/regenerate` — password + a valid factor required;
/// replaces the recovery-code set (old ones invalidated), returned once.
pub async fn mfa_regenerate(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<MfaRegenBody>,
) -> Result<Response, AppError> {
    mfa_local_guard(&state)?;
    let user_id = ctx.user_id.ok_or_else(|| AppError::Forbidden("no local account".into()))?;
    let email = ctx.email.clone().unwrap_or_default();
    if !mfa::is_enabled(&state.pg, user_id).await? {
        return Err(AppError::Conflict("MFA is not enabled".into()));
    }
    if !password_ok(&state, &email, user_id, &body.password).await {
        return Err(AppError::Unauthorized("current password is incorrect".into()));
    }
    if mfa::verify_code_or_recovery(&state.pg, user_id, &email, &body.code).await? == mfa::FactorUsed::None {
        return Err(AppError::Unauthorized("incorrect code".into()));
    }
    let codes = mfa::gen_recovery_codes();
    mfa::store_recovery_codes(&state.pg, user_id, &codes).await?;
    audit_mfa(&state, "auth.recovery_regenerated", user_id, ctx.role.as_str(), None).await;
    Ok(Json(json!({ "recovery_codes": codes })).into_response())
}

/// `GET /api/auth/mfa/status` — the caller's enrolment status.
pub async fn mfa_status(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Response, AppError> {
    mfa_local_guard(&state)?;
    let user_id = ctx.user_id.ok_or_else(|| AppError::Forbidden("no local account".into()))?;
    let s = mfa::status(&state.pg, user_id).await?;
    Ok(Json(json!({
        "enabled": s.enabled,
        "recovery_remaining": s.recovery_remaining,
    }))
    .into_response())
}

fn read_cookie(headers: &HeaderMap) -> Option<String> {
    let header = headers.get(header::COOKIE).and_then(|v| v.to_str().ok())?;
    header.split(';').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k.trim() == SESSION_COOKIE).then(|| v.trim().to_string())
    })
}

/// Audit a login-lifecycle fact. The password and the session token are NEVER
/// recorded — only the email and the outcome.
async fn audit_login(state: &AppState, action: &str, user_id: Option<uuid::Uuid>, email: &str) {
    let mut ev = crate::audit::AuditEvent::action(action, "user");
    ev.actor_user_id = user_id;
    ev.resource_type = Some("user".into());
    ev.resource_id = user_id;
    if !email.is_empty() {
        ev.payload = Some(json!({ "email": email }));
    }
    let _ = crate::audit::append(&state.pg, &ev).await;
}

/// As [`audit_login`] but records the second-factor outcome on the payload
/// (`mfa: true|false`) — the signal for "was this login a single or two-step sign-in".
async fn audit_login_mfa(
    state: &AppState,
    action: &str,
    user_id: Option<uuid::Uuid>,
    email: &str,
    mfa: Option<bool>,
) {
    let mut ev = crate::audit::AuditEvent::action(action, "user");
    ev.actor_user_id = user_id;
    ev.resource_type = Some("user".into());
    ev.resource_id = user_id;
    ev.payload = Some(json!({ "email": email, "mfa": mfa }));
    let _ = crate::audit::append(&state.pg, &ev).await;
}

/// Audit an MFA-lifecycle fact (`auth.mfa_enrolled/disabled/failed`,
/// `auth.recovery_used/regenerated`). Enrolment-state changes are risk-flagged, like
/// the account-state helpers in `users_admin`. Never records the code or secret.
async fn audit_mfa(
    state: &AppState,
    action: &str,
    user_id: uuid::Uuid,
    actor_role: &str,
    reason: Option<&str>,
) {
    let mut ev = crate::audit::AuditEvent::action(action, actor_role);
    ev.actor_user_id = Some(user_id);
    ev.resource_type = Some("user".into());
    ev.resource_id = Some(user_id);
    ev.risk_anomaly_flag = true;
    if let Some(r) = reason {
        ev.outcome_reason = Some(r.into());
    }
    let _ = crate::audit::append(&state.pg, &ev).await;
}
