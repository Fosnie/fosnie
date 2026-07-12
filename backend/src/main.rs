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

//! `fosnie-backend` entrypoint — the **Core-only** binary.
//!
//! Boots the hot API/WebSocket tokio runtime (this `#[tokio::main]`), brings up
//! a second tokio runtime on its own thread for background work, connects the
//! datastores, applies migrations, and serves until a shutdown signal drains
//! both runtimes. Core defaults are active for every extension seam (no Enterprise
//! routes/jobs/CLI). The combined edition is the separate `fosnie-enterprise` binary.

use std::sync::Arc;

use anyhow::Context;
use clap::{Parser, Subcommand};

use fosnie_backend::auth::breakglass::BreakglassCmd;
use fosnie_backend::config::{AuthMode, BootConfig};
use fosnie_backend::provider_cli::ProvidersCmd;
use fosnie_backend::state::AppStateBuilder;
use fosnie_backend::{auth, cache, db, provider_cli, telemetry};

/// `fosnie-backend` — runs the HTTP/WebSocket server by default, or an operational
/// subcommand (e.g. `breakglass`) that talks straight to Redis/Postgres.
#[derive(Parser)]
#[command(name = "fosnie-backend", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Ephemeral super-admin (break-glass) grant administration. Talks directly
    /// to Redis/Postgres, so it works even when the HTTP server or Keycloak is
    /// down (one of break-glass's purposes is repairing a broken platform).
    Breakglass {
        #[command(subcommand)]
        action: BreakglassCmd,
    },
    /// Provider verify-harness: probe a set of provider configs (provider × role
    /// matrix) through the ML service. Reuses the same path as the "Test" button.
    Providers {
        #[command(subcommand)]
        action: ProvidersCmd,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Install the process-wide rustls crypto provider before any TLS is used. With
    // both `aws-lc-rs` and `ring` in the dependency tree, rustls 0.23 cannot pick a
    // default itself, so `tokio-tungstenite`'s `wss://` connect (OpenAI Realtime STT)
    // would panic mid-handshake. reqwest/sqlx configure their own connectors; this
    // covers the one path that relies on the process default. Idempotent (Err if set).
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let cli = Cli::parse();

    let boot = BootConfig::load().context("loading boot config")?;
    boot.validate().map_err(|e| anyhow::anyhow!(e))?;

    // CLI subcommands run a lean path (no server, no scheduler) and exit.
    match cli.command {
        Some(Command::Breakglass { action }) => return auth::breakglass::run_cli(boot, action).await,
        Some(Command::Providers { action }) => return provider_cli::run_cli(boot, action).await,
        None => {}
    }

    telemetry::init(&boot.log_level, boot.observability.log_format == "json");
    fosnie_backend::metrics::init();
    fosnie_backend::metrics::spawn_process_collector();
    // BYOK seam: the Core binary uses the env/file KeyProvider (DEK + audit seed from
    // config) — byte-identical to the pre-BYOK boot. The `fosnie-enterprise` binary
    // injects an HSM-backed provider here instead.
    use fosnie_backend::ext::KeyProvider as _;
    let key_provider =
        fosnie_backend::ext::EnvFileKeyProvider::new(&boot.message_encryption_key, &boot.audit_signing_key);
    fosnie_backend::ext::install_key_provider(&key_provider)
        .context("installing key provider")?;
    tracing::info!(provider = key_provider.kind(), "key provider loaded");
    if boot.audit_signing_key.trim().is_empty() {
        tracing::warn!(
            "audit_signing_key is empty — the audit log is hash-chained but UNSIGNED; \
             set PAI__AUDIT_SIGNING_KEY in production for non-repudiation"
        );
    }
    // Non-fatal deployment-posture advisories (e.g. non-loopback bind, open ML service).
    for warning in boot.hardening_warnings() {
        tracing::warn!("security posture: {warning}");
    }

    tracing::info!("connecting to Postgres");
    let pg = db::connect(&boot.database_url, boot.db.max_connections)
        .await
        .context("connecting to Postgres")?;

    tracing::info!("running migrations");
    db::run_migrations(&pg).await.context("running migrations")?;

    // Audit the key provider at boot (kind + active key-id — never the key material).
    {
        let key_id = key_provider
            .active_dek()
            .ok()
            .flatten()
            .map(|d| d.id)
            .unwrap_or_else(|| "none".into());
        let mut ev = fosnie_backend::audit::AuditEvent::action("key.provider_loaded", "system");
        ev.payload = Some(serde_json::json!({ "kind": key_provider.kind(), "key_id": key_id }));
        let _ = fosnie_backend::audit::append(&pg, &ev).await;
    }

    let redis = cache::create_pool(&boot.redis_url).context("building Redis pool")?;

    let boot = Arc::new(boot);
    // Select the auth provider by mode (the seam slot is set at build time and
    // cannot be swapped afterwards). Local = Core email/password; Keycloak = the
    // default `KeycloakAuthProvider`. Other seams take their Core defaults.
    let mut builder = AppStateBuilder::new(pg, redis, boot.clone());
    if boot.auth.mode == AuthMode::Local {
        builder = builder.with_auth(Arc::new(auth::local::LocalAuthProvider));
    }
    let state = builder.build();

    fosnie_backend::server::run(state, boot, None, None).await
}
