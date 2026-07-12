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

//! Health endpoints.
//!
//! * `GET /health` — liveness; no dependencies, always 200 if the process runs.
//! * `GET /health/ready` — readiness; 200 only when Postgres *and* Redis answer,
//!   else 503 with a per-dependency breakdown.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::json;

use crate::state::AppState;

pub async fn liveness() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({ "status": "ok" })))
}

pub async fn readiness(State(state): State<AppState>) -> impl IntoResponse {
    let pg = crate::db::ping(&state.pg).await;
    let redis = crate::cache::ping(&state.redis).await;
    let ready = pg && redis;

    let code = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (
        code,
        Json(json!({
            "status": if ready { "ready" } else { "unready" },
            "checks": { "postgres": pg, "redis": redis },
        })),
    )
}
