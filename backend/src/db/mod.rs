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

//! Postgres connection pool + small helpers.

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

/// Build the application Postgres pool.
///
/// `url` is any Postgres-compatible DSN — a local container (default) or a managed
/// host (Supabase / Neon / RDS / Cloud SQL / Azure). TLS is configured **from the
/// URL**: `?sslmode=require` (or `verify-full` plus `sslrootcert=…`) is parsed by
/// sqlx's `PgConnectOptions` (the build enables `tls-rustls`) — no extra wiring.
///
/// Connect through a **direct or session-mode** endpoint, never a transaction-mode
/// pooler: sqlx caches prepared statements, which a transaction pooler breaks
/// (prepare and execute can land on different backends), and the audit chain's
/// `pg_advisory_xact_lock` relies on a stable session.
pub async fn connect(url: &str, max_connections: u32) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(max_connections)
        .connect(url)
        .await
}

/// Readiness probe: is Postgres answering?
pub async fn ping(pool: &PgPool) -> bool {
    sqlx::query("SELECT 1").execute(pool).await.is_ok()
}

/// Apply the unified Core migration set (forward-only). The migrations are embedded
/// at **Core** compile-time (the macro path is relative to this crate), so a
/// downstream `fosnie-enterprise` binary applies the exact same set by calling this —
/// there is no separate Enterprise migration set (the Enterprise layer is code, not schema;
/// Core-only deploys create empty Enterprise tables, inert).
pub async fn run_migrations(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    sqlx::migrate!("./migrations").run(pool).await
}

/// Mint a fresh UUIDv7 (time-ordered) — the platform-wide id convention.
pub fn new_id() -> Uuid {
    Uuid::now_v7()
}
