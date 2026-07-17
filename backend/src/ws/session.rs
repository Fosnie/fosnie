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

//! Redis-backed WebSocket session state: socket metadata,
//! the per-user socket set, presence (TTL + heartbeat), and the resume token
//! used to rebind a reconnecting socket within a short window without a fresh
//! interactive login.

use deadpool_redis::redis;
use uuid::Uuid;

use crate::error::AppError;

/// Resume window — within this, a reconnect with `?resume=<token>` rebinds.
const RESUME_TTL_SECS: u64 = 300; // 5 min (redis-keyspace: 2–5 min)
const PRESENCE_TTL_SECS: u64 = 60;
/// WS connect ticket — minted over the authenticated HTTP path so the browser
/// never puts the JWT in the socket URL. Short-lived + single-use.
const TICKET_TTL_SECS: u64 = 30;

fn sock_key(socket_id: Uuid) -> String {
    format!("pai:sock:{socket_id}")
}
fn user_sockets_key(user_id: Uuid) -> String {
    format!("pai:user_sockets:{user_id}")
}
fn presence_key(user_id: Uuid) -> String {
    format!("pai:presence:{user_id}")
}
fn resume_key(token: &str) -> String {
    format!("pai:resume:{token}")
}
fn ticket_key(token: &str) -> String {
    format!("pai:ws_ticket:{token}")
}
fn replay_key(user_id: Uuid) -> String {
    format!("pai:replay:{user_id}")
}

/// Recent non-token frames retained per user, so a reconnecting socket within
/// the resume window can catch up on events it missed (redis-keyspace: replay).
const REPLAY_CAP: isize = 100;

/// Append a frame (already-serialised JSON) to the user's replay buffer.
pub async fn buffer_frame(
    pool: &deadpool_redis::Pool,
    user_id: Uuid,
    json: &str,
) -> Result<(), AppError> {
    let mut c = conn(pool).await?;
    redis::pipe()
        .cmd("LPUSH")
        .arg(replay_key(user_id))
        .arg(json)
        .ignore()
        .cmd("LTRIM")
        .arg(replay_key(user_id))
        .arg(0)
        .arg(REPLAY_CAP - 1)
        .ignore()
        .cmd("EXPIRE")
        .arg(replay_key(user_id))
        .arg(RESUME_TTL_SECS)
        .ignore()
        .query_async::<()>(&mut c)
        .await
        .map_err(redis_err)
}

/// The user's buffered frames, oldest-first, for replay on a resume reconnect.
pub async fn replay_frames(
    pool: &deadpool_redis::Pool,
    user_id: Uuid,
) -> Result<Vec<String>, AppError> {
    let mut c = conn(pool).await?;
    let mut v: Vec<String> = redis::cmd("LRANGE")
        .arg(replay_key(user_id))
        .arg(0)
        .arg(-1)
        .query_async(&mut c)
        .await
        .map_err(redis_err)?;
    v.reverse(); // LPUSH stores newest-first; replay in chronological order
    Ok(v)
}

async fn conn(pool: &deadpool_redis::Pool) -> Result<deadpool_redis::Connection, AppError> {
    pool.get().await.map_err(AppError::from)
}

/// Register a socket: metadata, add to the user's socket set, set presence.
pub async fn register_socket(
    pool: &deadpool_redis::Pool,
    socket_id: Uuid,
    user_id: Uuid,
) -> Result<(), AppError> {
    let mut c = conn(pool).await?;
    redis::pipe()
        .cmd("HSET")
        .arg(sock_key(socket_id))
        .arg("user_id")
        .arg(user_id.to_string())
        .ignore()
        .cmd("EXPIRE")
        .arg(sock_key(socket_id))
        .arg(RESUME_TTL_SECS)
        .ignore()
        .cmd("SADD")
        .arg(user_sockets_key(user_id))
        .arg(socket_id.to_string())
        .ignore()
        .cmd("SET")
        .arg(presence_key(user_id))
        .arg("online")
        .arg("EX")
        .arg(PRESENCE_TTL_SECS)
        .ignore()
        .query_async::<()>(&mut c)
        .await
        .map_err(redis_err)
}

/// Mint a resume token bound to the user; TTL = the resume window.
pub async fn issue_resume(
    pool: &deadpool_redis::Pool,
    user_id: Uuid,
) -> Result<String, AppError> {
    let token = Uuid::now_v7().to_string();
    let mut c = conn(pool).await?;
    redis::cmd("SET")
        .arg(resume_key(&token))
        .arg(user_id.to_string())
        .arg("EX")
        .arg(RESUME_TTL_SECS)
        .query_async::<()>(&mut c)
        .await
        .map_err(redis_err)?;
    Ok(token)
}

/// Resolve a resume token to its user id, if still within the window.
pub async fn lookup_resume(
    pool: &deadpool_redis::Pool,
    token: &str,
) -> Result<Option<Uuid>, AppError> {
    let mut c = conn(pool).await?;
    let val: Option<String> = redis::cmd("GET")
        .arg(resume_key(token))
        .query_async(&mut c)
        .await
        .map_err(redis_err)?;
    Ok(val.and_then(|s| Uuid::parse_str(&s).ok()))
}

/// Mint a single-use WS connect ticket bound to the user (TTL = `TICKET_TTL_SECS`).
/// Issued by `POST /api/ws-ticket` over the authenticated Bearer path, so the
/// access token stays in the `Authorization` header and never reaches a URL.
pub async fn issue_ticket(
    pool: &deadpool_redis::Pool,
    user_id: Uuid,
) -> Result<String, AppError> {
    let token = Uuid::now_v7().to_string();
    let mut c = conn(pool).await?;
    redis::cmd("SET")
        .arg(ticket_key(&token))
        .arg(user_id.to_string())
        .arg("EX")
        .arg(TICKET_TTL_SECS)
        .query_async::<()>(&mut c)
        .await
        .map_err(redis_err)?;
    Ok(token)
}

/// Redeem a WS ticket → its user id, deleting it atomically so it cannot be
/// replayed (single-use). `None` if unknown/expired/already used.
pub async fn redeem_ticket(
    pool: &deadpool_redis::Pool,
    token: &str,
) -> Result<Option<Uuid>, AppError> {
    let mut c = conn(pool).await?;
    let val: Option<String> = redis::cmd("GETDEL")
        .arg(ticket_key(token))
        .query_async(&mut c)
        .await
        .map_err(redis_err)?;
    Ok(val.and_then(|s| Uuid::parse_str(&s).ok()))
}

/// Refresh presence TTL on heartbeat.
pub async fn refresh_presence(
    pool: &deadpool_redis::Pool,
    user_id: Uuid,
) -> Result<(), AppError> {
    let mut c = conn(pool).await?;
    redis::cmd("SET")
        .arg(presence_key(user_id))
        .arg("online")
        .arg("EX")
        .arg(PRESENCE_TTL_SECS)
        .query_async::<()>(&mut c)
        .await
        .map_err(redis_err)
}

/// Remove a socket; if it was the user's last, drop presence (presence-leave).
pub async fn deregister_socket(
    pool: &deadpool_redis::Pool,
    socket_id: Uuid,
    user_id: Uuid,
) -> Result<bool, AppError> {
    let mut c = conn(pool).await?;
    redis::cmd("DEL")
        .arg(sock_key(socket_id))
        .query_async::<i64>(&mut c)
        .await
        .map_err(redis_err)?;
    redis::cmd("SREM")
        .arg(user_sockets_key(user_id))
        .arg(socket_id.to_string())
        .query_async::<i64>(&mut c)
        .await
        .map_err(redis_err)?;
    let remaining: i64 = redis::cmd("SCARD")
        .arg(user_sockets_key(user_id))
        .query_async(&mut c)
        .await
        .map_err(redis_err)?;
    if remaining == 0 {
        redis::cmd("DEL")
            .arg(presence_key(user_id))
            .query_async::<i64>(&mut c)
            .await
            .map_err(redis_err)?;
        return Ok(true); // last socket left
    }
    Ok(false)
}

fn redis_err(e: redis::RedisError) -> AppError {
    AppError::Other(anyhow::anyhow!("redis: {e}"))
}
