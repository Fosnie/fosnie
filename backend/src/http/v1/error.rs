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

//! The error shape the OpenAI-compatible surface must emit.
//!
//! The platform's own [`AppError`] renders as `text/plain`, which every OpenAI
//! client SDK mis-parses: they read `{"error":{"message","type","code"}}` and
//! surface the `message` field to the caller. So this surface carries its own
//! error type rather than reusing the platform one, and converts at the border.

use axum::Json;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::response::sse::Event;
use serde_json::json;

use crate::error::AppError;

/// An error in the shape OpenAI clients expect.
#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
    /// OpenAI's coarse error class (`invalid_request_error`, `authentication_error`, …).
    pub kind: &'static str,
    /// The fine-grained code clients special-case on (e.g. `model_not_found`).
    pub code: Option<&'static str>,
    /// Seconds to advertise in `Retry-After` (rate limiting only).
    pub retry_after: Option<u64>,
}

impl ApiError {
    fn new(status: StatusCode, kind: &'static str, message: impl Into<String>) -> Self {
        Self { status, message: message.into(), kind, code: None, retry_after: None }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request_error", message)
    }

    pub fn unauthorised(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "authentication_error", message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "permission_error", message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "invalid_request_error", message)
    }

    /// The 404 a client gets for an unknown `model`. `model_not_found` is the
    /// code OpenAI itself returns, and SDKs branch on it.
    pub fn model_not_found(model: &str) -> Self {
        let mut e = Self::not_found(format!("the model '{model}' does not exist"));
        e.code = Some("model_not_found");
        e
    }

    pub fn rate_limited(retry_after_secs: u64) -> Self {
        let mut e = Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_error",
            "rate limit exceeded for this API key",
        );
        e.retry_after = Some(retry_after_secs);
        e
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "api_error", message)
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "api_error", message)
    }

    /// The JSON body, shared by the response renderer and the mid-stream event.
    pub fn body(&self) -> serde_json::Value {
        json!({ "error": { "message": self.message, "type": self.kind, "code": self.code } })
    }

    /// The same payload as an SSE event, for a failure that happens after the
    /// response head has already gone out. A client that has started reading
    /// the stream cannot be given a status code any more, so the error rides
    /// the stream in the body shape the SDKs already parse.
    pub fn sse_event(&self) -> Event {
        Event::default().data(self.body().to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // 5xx detail is authored by us here (never a wrapped DB error), so it is
        // safe to surface; anything opaque is mapped to a generic message at the
        // `From<AppError>` border below.
        if self.status.is_server_error() {
            tracing::error!(status = %self.status, message = %self.message, "public API request failed");
        }
        let mut res = (self.status, Json(self.body())).into_response();
        if let Some(secs) = self.retry_after {
            if let Ok(v) = header::HeaderValue::from_str(&secs.to_string()) {
                res.headers_mut().insert(header::RETRY_AFTER, v);
            }
        }
        res
    }
}

impl From<AppError> for ApiError {
    fn from(e: AppError) -> Self {
        match e {
            AppError::Validation(m) => Self::bad_request(m),
            AppError::Unauthorized(m) => Self::unauthorised(m),
            AppError::Forbidden(m) => Self::forbidden(m),
            AppError::NotFound(m) => Self::not_found(m),
            AppError::Conflict(m) => Self::new(StatusCode::CONFLICT, "invalid_request_error", m),
            AppError::TooManyRequests(_) => Self::rate_limited(60),
            AppError::Unavailable(m) => Self::unavailable(m),
            // Database/config/internal failures keep their detail in the logs only.
            other => {
                tracing::error!(error = %other, "public API internal error");
                Self::internal("internal error")
            }
        }
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        AppError::from(e).into()
    }
}
