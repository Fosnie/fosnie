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

//! Application error type and its HTTP rendering.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

pub type Result<T, E = AppError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),

    #[error("redis pool error: {0}")]
    RedisPool(#[from] deadpool_redis::PoolError),

    #[error("configuration error: {0}")]
    Config(String),

    /// Caller-supplied input failed validation — rendered as 400.
    #[error("validation error: {0}")]
    Validation(String),

    /// No valid credential — rendered as 401.
    #[error("unauthorized: {0}")]
    Unauthorized(String),

    /// Authenticated but not permitted — rendered as 403.
    #[error("forbidden: {0}")]
    Forbidden(String),

    /// The resource does not exist, or its existence must not be revealed to this
    /// caller — rendered as 404. Used where a 403 would itself leak existence (e.g.
    /// a document hidden by an enforced source ACL).
    #[error("not found: {0}")]
    NotFound(String),

    /// Optimistic-concurrency / state conflict — rendered as 409.
    #[error("conflict: {0}")]
    Conflict(String),

    /// Caller exceeded a rate limit — rendered as 429.
    #[error("too many requests: {0}")]
    TooManyRequests(String),

    /// A dependency the request needs is not available — rendered as 503
    /// (e.g. DOCX→PDF rendition when LibreOffice is absent).
    #[error("unavailable: {0}")]
    Unavailable(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        // The 4xx/503 variants carry an operator/user-facing message we deliberately
        // authored — surface it so the client can show *why* (e.g. "under an active
        // legal hold"). The catch-all 5xx variants wrap internal/DB errors, so they
        // stay opaque (canonical reason only) — full detail goes to the logs alone.
        let (status, detail): (StatusCode, Option<&str>) = match &self {
            AppError::Validation(m) => (StatusCode::BAD_REQUEST, Some(m)),
            AppError::Unauthorized(m) => (StatusCode::UNAUTHORIZED, Some(m)),
            AppError::Forbidden(m) => (StatusCode::FORBIDDEN, Some(m)),
            AppError::NotFound(m) => (StatusCode::NOT_FOUND, Some(m)),
            AppError::Conflict(m) => (StatusCode::CONFLICT, Some(m)),
            AppError::TooManyRequests(m) => (StatusCode::TOO_MANY_REQUESTS, Some(m)),
            AppError::Unavailable(m) => (StatusCode::SERVICE_UNAVAILABLE, Some(m)),
            _ => (StatusCode::INTERNAL_SERVER_ERROR, None),
        };
        tracing::error!(error = %self, "request failed");
        let body = detail
            .map(str::to_string)
            .unwrap_or_else(|| status.canonical_reason().unwrap_or("error").to_string());
        (status, body).into_response()
    }
}
