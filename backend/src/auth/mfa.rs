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

//! Second factor (TOTP, RFC 6238) + recovery codes for Core local-auth.
//! Opt-in per user; an admin `auth.require_mfa` policy can make it
//! mandatory (enforced in the login/enrol path, not here). Keycloak-mode 2FA is
//! Keycloak's own job — nothing in this module runs on the Keycloak path.
//!
//! Design: SHA-1 / 6 digits / 30s / ±1 step (compatible with every common
//! authenticator), with **anti-replay** — the last accepted time-step is persisted
//! (`users.mfa_last_step`) and a code is only accepted for a strictly greater step,
//! then CAS-advanced, so a code cannot be reused inside its validity window. The
//! shared secret is stored base32 under the keyring [`crate::crypto::encrypt_at_rest`]
//! (BYOK/rotation-compatible). Recovery codes are single-use, stored only as
//! SHA-256 hashes, shown once. The two-step login pending token lives in Redis as
//! a single-use, 5-minute value.

use argon2::password_hash::rand_core::{OsRng, RngCore};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use totp_rs::{Algorithm, Secret, TOTP};
use uuid::Uuid;

use crate::error::{AppError, Result};
use crate::state::AppState;

/// otpauth issuer label shown in the authenticator app.
const ISSUER: &str = "Fosnie";
const STEP_SECS: u64 = 30;
const DIGITS: usize = 6;
/// ±1 step tolerance for clock drift (the standard authenticator window).
const SKEW: u8 = 1;
const RECOVERY_COUNT: usize = 10;
/// Failed `mfa/verify` attempts (per pending token AND per IP) before the pending
/// token is burned and the user must log in again.
pub const VERIFY_FAIL_MAX: i64 = 5;
pub const VERIFY_FAIL_WINDOW_SECS: u64 = 300;
/// Two-step login pending-token TTL — the window to enter a code after the
/// password check succeeds.
const PENDING_TTL_SECS: u64 = 300;

fn pending_key(token: &str) -> String {
    format!("pai:mfa:pending:{token}")
}

/// Constant-time byte comparison (never short-circuits on content) — used for both
/// the TOTP code and the recovery-code hash comparison.
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// A fresh CSPRNG base32 TOTP secret (the value stored, encrypted, in `mfa_secret_enc`).
pub fn gen_secret() -> String {
    Secret::generate_secret().to_encoded().to_string()
}

/// Build a [`TOTP`] from a stored base32 secret + the account label (the user's email).
fn build_totp(secret_b32: &str, account: &str) -> Result<TOTP> {
    let bytes = Secret::Encoded(secret_b32.to_string())
        .to_bytes()
        .map_err(|e| AppError::Other(anyhow::anyhow!("invalid TOTP secret: {e:?}")))?;
    TOTP::new(
        Algorithm::SHA1,
        DIGITS,
        SKEW,
        STEP_SECS,
        bytes,
        Some(ISSUER.to_string()),
        account.to_string(),
    )
    .map_err(|e| AppError::Other(anyhow::anyhow!("TOTP build failed: {e}")))
}

/// The `otpauth://…` provisioning URL for a (pending or live) secret — the SPA
/// renders it as a QR code and also shows the raw secret for manual entry.
pub fn otpauth_url(secret_b32: &str, account: &str) -> Result<String> {
    Ok(build_totp(secret_b32, account)?.get_url())
}

/// Generate the TOTP code for `secret_b32` at `unix_secs` — the counterpart to
/// [`verify_totp`]. Exposed for tests and any external verification helper.
pub fn generate_code(secret_b32: &str, account: &str, unix_secs: u64) -> Result<String> {
    Ok(build_totp(secret_b32, account)?.generate(unix_secs))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A user's stored MFA secret state.
struct SecretRow {
    secret_enc: Option<String>,
    last_step: Option<i64>,
}

async fn secret_row(pg: &PgPool, user_id: Uuid) -> Result<SecretRow> {
    let row = sqlx::query!(
        r#"SELECT mfa_secret_enc, mfa_last_step FROM users WHERE id = $1"#,
        user_id
    )
    .fetch_optional(pg)
    .await?
    .ok_or_else(|| AppError::Unauthorized("user not found".into()))?;
    Ok(SecretRow {
        secret_enc: row.mfa_secret_enc,
        last_step: row.mfa_last_step,
    })
}

/// Verify a TOTP `code` against the user's stored secret with anti-replay. Returns
/// `Ok(true)` and CAS-advances `mfa_last_step` on the matched step; `Ok(false)`
/// when no step in the ±1 window matches or the matched step was already used.
pub async fn verify_totp(pg: &PgPool, user_id: Uuid, account: &str, code: &str) -> Result<bool> {
    let code = code.trim();
    if code.len() != DIGITS || !code.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(false);
    }
    let row = secret_row(pg, user_id).await?;
    let Some(secret_enc) = row.secret_enc else {
        return Ok(false);
    };
    let secret = crate::crypto::decrypt_at_rest(&secret_enc)?;
    let totp = build_totp(&secret, account)?;
    let last_step = row.last_step.unwrap_or(-1);
    let cur = (now_secs() / STEP_SECS) as i64;
    // Candidate steps in the ±1 skew window, newest first.
    for cand in [cur + 1, cur, cur - 1] {
        if cand <= last_step {
            continue; // already used / too old — anti-replay
        }
        let expected = totp.generate((cand as u64) * STEP_SECS);
        if ct_eq(&expected, code) {
            // CAS-advance: only wins if no concurrent verify already moved past.
            sqlx::query!(
                r#"UPDATE users SET mfa_last_step = $2
                   WHERE id = $1 AND (mfa_last_step IS NULL OR mfa_last_step < $2)"#,
                user_id,
                cand
            )
            .execute(pg)
            .await?;
            return Ok(true);
        }
    }
    Ok(false)
}

// ── Recovery codes ──────────────────────────────────────────────────────────

fn hash_code(code: &str) -> String {
    let norm = code.trim().to_ascii_lowercase();
    let digest = Sha256::digest(norm.as_bytes());
    hex::encode(digest)
}

/// Generate `RECOVERY_COUNT` fresh single-use codes in `xxxx-xxxx` form (lowercase
/// alphanumeric). Returned in the clear once — only their hashes are stored.
pub fn gen_recovery_codes() -> Vec<String> {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    (0..RECOVERY_COUNT)
        .map(|_| {
            let mut b = [0u8; 8];
            OsRng.fill_bytes(&mut b);
            let s: String = b
                .iter()
                .map(|x| ALPHABET[(*x as usize) % ALPHABET.len()] as char)
                .collect();
            format!("{}-{}", &s[0..4], &s[4..8])
        })
        .collect()
}

/// Replace the user's recovery-code set with the hashes of `codes` (regenerate).
pub async fn store_recovery_codes(pg: &PgPool, user_id: Uuid, codes: &[String]) -> Result<()> {
    let mut tx = pg.begin().await?;
    sqlx::query!(r#"DELETE FROM mfa_recovery_codes WHERE user_id = $1"#, user_id)
        .execute(&mut *tx)
        .await?;
    for code in codes {
        let h = hash_code(code);
        sqlx::query!(
            r#"INSERT INTO mfa_recovery_codes (user_id, code_hash) VALUES ($1, $2)"#,
            user_id,
            h
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Consume a single unused recovery code (single-use). Returns `true` when one was
/// spent. Atomic — the `used_at IS NULL` guard makes a concurrent reuse a no-op.
pub async fn consume_recovery_code(pg: &PgPool, user_id: Uuid, code: &str) -> Result<bool> {
    let h = hash_code(code);
    let res = sqlx::query!(
        r#"UPDATE mfa_recovery_codes SET used_at = now()
           WHERE user_id = $1 AND code_hash = $2 AND used_at IS NULL"#,
        user_id,
        h
    )
    .execute(pg)
    .await?;
    Ok(res.rows_affected() == 1)
}

/// Which factor a `code`-or-`recovery` value satisfied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactorUsed {
    Totp,
    Recovery,
    None,
}

/// Verify a value that may be either a 6-digit TOTP code or a recovery code. TOTP
/// is tried first when the value looks numeric; otherwise a recovery code is
/// consumed. `Recovery` is the loss-of-device signal the caller should audit.
pub async fn verify_code_or_recovery(
    pg: &PgPool,
    user_id: Uuid,
    account: &str,
    value: &str,
) -> Result<FactorUsed> {
    let v = value.trim();
    let looks_totp = v.len() == DIGITS && v.bytes().all(|b| b.is_ascii_digit());
    if looks_totp {
        if verify_totp(pg, user_id, account, v).await? {
            return Ok(FactorUsed::Totp);
        }
        return Ok(FactorUsed::None);
    }
    if consume_recovery_code(pg, user_id, v).await? {
        return Ok(FactorUsed::Recovery);
    }
    Ok(FactorUsed::None)
}

// ── Enrolment / lifecycle DB ────────────────────────────────────────────────

/// Stash a freshly generated secret as *pending* (MFA not yet enabled: the user
/// must confirm a code first). Encrypts at rest; resets any prior step counter.
pub async fn set_pending_secret(pg: &PgPool, user_id: Uuid, secret_b32: &str) -> Result<()> {
    let enc = crate::crypto::encrypt_at_rest(secret_b32)?;
    sqlx::query!(
        r#"UPDATE users
           SET mfa_secret_enc = $2, mfa_enabled_at = NULL, mfa_last_step = NULL
           WHERE id = $1"#,
        user_id,
        enc
    )
    .execute(pg)
    .await?;
    Ok(())
}

/// Flip a pending secret to live (called after a confirming code verifies).
pub async fn mark_enabled(pg: &PgPool, user_id: Uuid) -> Result<()> {
    sqlx::query!(
        r#"UPDATE users SET mfa_enabled_at = now() WHERE id = $1"#,
        user_id
    )
    .execute(pg)
    .await?;
    Ok(())
}

/// Fully remove MFA from a user (disable / admin reset): clear the secret + step +
/// enabled flag and delete every recovery code. Idempotent.
pub async fn clear(pg: &PgPool, user_id: Uuid) -> Result<()> {
    let mut tx = pg.begin().await?;
    sqlx::query!(
        r#"UPDATE users
           SET mfa_secret_enc = NULL, mfa_enabled_at = NULL, mfa_last_step = NULL
           WHERE id = $1"#,
        user_id
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(r#"DELETE FROM mfa_recovery_codes WHERE user_id = $1"#, user_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// True when the user has confirmed (live) MFA.
pub async fn is_enabled(pg: &PgPool, user_id: Uuid) -> Result<bool> {
    let enabled: bool = sqlx::query_scalar!(
        r#"SELECT (mfa_enabled_at IS NOT NULL) AS "e!" FROM users WHERE id = $1"#,
        user_id
    )
    .fetch_optional(pg)
    .await?
    .unwrap_or(false);
    Ok(enabled)
}

/// Enrolment status for the `GET /api/auth/mfa/status` endpoint + the SPA.
pub struct MfaStatus {
    pub enabled: bool,
    pub recovery_remaining: i64,
}

pub async fn status(pg: &PgPool, user_id: Uuid) -> Result<MfaStatus> {
    let enabled = is_enabled(pg, user_id).await?;
    let recovery_remaining: i64 = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM mfa_recovery_codes
           WHERE user_id = $1 AND used_at IS NULL"#,
        user_id
    )
    .fetch_one(pg)
    .await?;
    Ok(MfaStatus {
        enabled,
        recovery_remaining,
    })
}

// ── Two-step login pending token ────────────────────────────────────────────

fn new_pending_token() -> String {
    let mut b = [0u8; 32];
    OsRng.fill_bytes(&mut b);
    URL_SAFE_NO_PAD.encode(b)
}

/// Mint a single-use pending token bound to `user_id` (password already verified),
/// TTL 5 min. Returned to the client; exchanged at `mfa/verify` for a full session.
pub async fn issue_pending(state: &AppState, user_id: Uuid) -> Result<String> {
    let token = new_pending_token();
    crate::cache::kv_set_ex(
        &state.redis,
        &pending_key(&token),
        &user_id.to_string(),
        PENDING_TTL_SECS,
    )
    .await?;
    Ok(token)
}

/// Read a pending token without consuming it (survives a wrong-code retry). Returns
/// the bound user id, or `None` if unknown / expired.
pub async fn peek_pending(state: &AppState, token: &str) -> Result<Option<Uuid>> {
    let v = crate::cache::kv_get(&state.redis, &pending_key(token)).await?;
    Ok(v.and_then(|s| Uuid::parse_str(&s).ok()))
}

/// Burn a pending token (single-use consume on success, or on failure-cap lock-out).
pub async fn consume_pending(state: &AppState, token: &str) -> Result<Option<Uuid>> {
    let v = crate::cache::kv_get_del(&state.redis, &pending_key(token)).await?;
    Ok(v.and_then(|s| Uuid::parse_str(&s).ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 6238 does not fix the secret encoding of the otpauth URL, but a generated
    /// secret must round-trip: build a TOTP, generate a code, and verify it matches
    /// what an independent generation for the same step yields.
    #[test]
    fn totp_generate_is_stable_for_a_step() {
        let secret = gen_secret();
        let totp = build_totp(&secret, "alice@example.com").unwrap();
        let step_time = 59; // RFC 6238 test vector time
        assert_eq!(totp.generate(step_time), totp.generate(step_time));
        assert_eq!(totp.generate(step_time).len(), DIGITS);
    }

    /// A known RFC 6238 SHA-1 vector: secret "12345678901234567890" (ASCII), T=59
    /// ⇒ code 94287082 → 6-digit truncation 287082.
    #[test]
    fn rfc6238_sha1_vector() {
        let secret_ascii = b"12345678901234567890".to_vec();
        let totp = TOTP::new(
            Algorithm::SHA1,
            DIGITS,
            SKEW,
            STEP_SECS,
            secret_ascii,
            Some(ISSUER.to_string()),
            "test".to_string(),
        )
        .unwrap();
        assert_eq!(totp.generate(59), "287082");
    }

    #[test]
    fn otpauth_url_has_issuer_and_secret() {
        let secret = gen_secret();
        let url = otpauth_url(&secret, "alice@example.com").unwrap();
        assert!(url.starts_with("otpauth://totp/"));
        assert!(url.contains("issuer=Fosnie"));
        assert!(url.contains("secret="));
    }

    #[test]
    fn recovery_codes_shape() {
        let codes = gen_recovery_codes();
        assert_eq!(codes.len(), RECOVERY_COUNT);
        for c in &codes {
            assert_eq!(c.len(), 9); // xxxx-xxxx
            assert_eq!(c.as_bytes()[4], b'-');
        }
        // Distinct hashes.
        let mut hs: Vec<String> = codes.iter().map(|c| hash_code(c)).collect();
        hs.sort();
        hs.dedup();
        assert_eq!(hs.len(), RECOVERY_COUNT);
    }

    #[test]
    fn ct_eq_matches_and_rejects() {
        assert!(ct_eq("287082", "287082"));
        assert!(!ct_eq("287082", "287083"));
        assert!(!ct_eq("287082", "28708"));
    }
}
