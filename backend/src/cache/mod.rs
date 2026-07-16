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

//! Redis connection pool + helpers.
//!
//! Named `cache` rather than `redis` to avoid shadowing the `redis` crate
//! (re-exported via `deadpool_redis`). Redis is Rust-only here:
//! pub/sub, socket state, and — once auth lands — the OIDC session cache.

use deadpool_redis::{Config, Pool, Runtime};

use crate::error::{AppError, Result};

/// Build the Redis pool from a URL. Lazy — no connection is made until first use.
pub fn create_pool(url: &str) -> Result<Pool, deadpool_redis::CreatePoolError> {
    let cfg = Config::from_url(url);
    cfg.create_pool(Some(Runtime::Tokio1))
}

/// Fixed-window rate limit. Returns `true` when the action is allowed (still
/// under `max` within the current `window_secs`), `false` when it should be
/// rejected. **Fails open** on any Redis error — this is a coarse abuse guard,
/// not a security control, so availability beats strictness.
pub async fn rate_limit_ok(pool: &Pool, key: &str, max: i64, window_secs: u64) -> bool {
    use deadpool_redis::redis;
    let Ok(mut conn) = pool.get().await else {
        return true; // fail open
    };
    let rk = format!("pai:rl:{key}");
    let count: i64 = match redis::cmd("INCR").arg(&rk).query_async(&mut conn).await {
        Ok(n) => n,
        Err(_) => return true,
    };
    if count == 1 {
        // First hit in this window — start the expiry clock.
        let _ = redis::cmd("EXPIRE")
            .arg(&rk)
            .arg(window_secs)
            .query_async::<i64>(&mut conn)
            .await;
    }
    count <= max
}

/// As [`rate_limit_ok`], but bubbles a `TooManyRequests` (429) error — for REST
/// handlers that return `Result`. Same fail-open coarse-guard semantics.
pub async fn rate_limit_guard(pool: &Pool, key: &str, max: i64, window_secs: u64) -> Result<()> {
    if rate_limit_ok(pool, key, max, window_secs).await {
        Ok(())
    } else {
        Err(AppError::TooManyRequests("rate limited — please slow down".into()))
    }
}

/// Store `value` under `key` with a TTL (seconds). A general single-use / short-
/// lived KV set — used e.g. for pending OAuth-connect flows (the PKCE verifier +
/// flow context, keyed by a random state id). Errors bubble (unlike the fail-open
/// rate limiter): a lost write must fail the flow, not silently proceed.
pub async fn kv_set_ex(pool: &Pool, key: &str, value: &str, ttl_secs: u64) -> Result<()> {
    use deadpool_redis::redis;
    let mut conn = pool.get().await.map_err(|e| AppError::Other(anyhow::anyhow!("redis pool: {e}")))?;
    redis::cmd("SET")
        .arg(key)
        .arg(value)
        .arg("EX")
        .arg(ttl_secs)
        .query_async::<()>(&mut conn)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("redis SET failed: {e}")))
}

/// Read `key` without deleting it (a peek). `None` when absent/expired. Used when a
/// value must survive several reads before a terminal consume — e.g. the MFA
/// two-step pending token, which is retried on a wrong code and only burned on
/// success or after the failure cap.
pub async fn kv_get(pool: &Pool, key: &str) -> Result<Option<String>> {
    use deadpool_redis::redis;
    let mut conn = pool.get().await.map_err(|e| AppError::Other(anyhow::anyhow!("redis pool: {e}")))?;
    let v: Option<String> = redis::cmd("GET")
        .arg(key)
        .query_async(&mut conn)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("redis GET failed: {e}")))?;
    Ok(v)
}

/// Atomically read **and delete** `key` (single-use consume). Returns `None` if the
/// key is absent/expired. Uses `GETDEL` so a replay cannot reuse the value.
pub async fn kv_get_del(pool: &Pool, key: &str) -> Result<Option<String>> {
    use deadpool_redis::redis;
    let mut conn = pool.get().await.map_err(|e| AppError::Other(anyhow::anyhow!("redis pool: {e}")))?;
    let v: Option<String> = redis::cmd("GETDEL")
        .arg(key)
        .query_async(&mut conn)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("redis GETDEL failed: {e}")))?;
    Ok(v)
}

/// Readiness probe: does Redis reply to PING?
pub async fn ping(pool: &Pool) -> bool {
    let Ok(mut conn) = pool.get().await else {
        return false;
    };
    match deadpool_redis::redis::cmd("PING")
        .query_async::<String>(&mut conn)
        .await
    {
        Ok(reply) => reply.eq_ignore_ascii_case("pong"),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixed-window guard allows `max` hits then rejects — the behaviour the
    /// per-user abuse limits rely on. Runs against the dev Redis; skips when Redis
    /// is unreachable (the limiter fails open, so asserting rejection would be moot).
    #[tokio::test]
    async fn rate_limit_allows_max_then_rejects() {
        let url = std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
        let Ok(pool) = create_pool(&url) else { return };
        if !ping(&pool).await {
            return; // no Redis → skip (fail-open would defeat the assertion)
        }
        // A key unique to this run so a re-run within the window doesn't collide.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let key = format!("test-rl-{nanos}");
        for _ in 0..3 {
            assert!(rate_limit_ok(&pool, &key, 3, 60).await, "first {} should pass", 3);
        }
        assert!(!rate_limit_ok(&pool, &key, 3, 60).await, "over the cap → rejected");
        assert!(rate_limit_guard(&pool, &key, 3, 60).await.is_err(), "guard surfaces 429");
    }
}
