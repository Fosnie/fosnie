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

//! Server entrypoint as a library function.
//!
//! [`run`] owns the serving half of boot — runtime collectors, default-skill
//! seeding, the background scheduler, the Keycloak auth layers, the listener,
//! and graceful shutdown + drain. The binary `main` (and a future
//! `fosnie-enterprise` main) builds an [`AppState`] and hands it here, so the only
//! difference between deployments is which extension seams are wired into state.

use std::sync::Arc;

use anyhow::Context;
use tokio::net::TcpListener;
use tokio::sync::watch;

use crate::config::{AuthMode, BootConfig};
use crate::state::AppState;
use crate::{auth, http, scheduler};

/// Serve until a shutdown signal drains both runtimes. `state` is fully built
/// (pools open, migrations applied, extension seams wired); `boot` is the shared
/// boot config. Logic is identical to the former inline body of `main`.
///
/// `extra_routes` are merged into the *protected* router (gated by the auth
/// layer). `extra_public` are merged into the *public* router (ahead of any auth
/// layer) — for edition endpoints that carry their own authentication (e.g. a
/// SCIM bearer token), not the Keycloak/session gate. Core passes `None` for
/// both; only a private edition supplies them.
pub async fn run(
    state: AppState,
    boot: Arc<BootConfig>,
    extra_routes: Option<axum::Router<AppState>>,
    extra_public: Option<axum::Router<AppState>>,
) -> anyhow::Result<()> {
    // Runtime gauges (DB/Redis pool saturation + ping latency, durable-task queue
    // depth) for post-deploy observability — alongside the process-resource collector.
    crate::metrics::spawn_runtime_collector(state.pg.clone(), state.redis.clone());

    // Seed built-in default skills (idempotent, edit-preserving). Best-effort: a
    // failure must not block boot — agents simply lack the default skill until fixed.
    if let Err(e) = crate::skills_seed::ensure_default_skills(&state).await {
        tracing::warn!(error = %e, "seeding default skills failed");
    }

    // Normalise any absolute DB path values to install-relative (idempotent,
    // best-effort). Keeps the DB install-location independent after a move/redeploy.
    crate::storage::backfill_paths(&state).await;

    // Fully-local deploy (docker-compose `--profile local` sets LOCAL_STACK=1): seed
    // the Ollama/reranker provider rows so chat + RAG work with zero manual config.
    // Idempotent + non-destructive (only fills empty roles); best-effort so a
    // failure never blocks boot.
    if crate::providers_seed::enabled() {
        if let Err(e) = crate::providers_seed::seed_local_stack(&state).await {
            tracing::warn!(error = %e, "LOCAL_STACK provider seed failed");
        }
    }

    // Background runtime (second tokio runtime on its own thread).
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let bg = scheduler::spawn(state.clone(), boot.scheduler.clone(), shutdown_rx);

    // Auth wiring. The Keycloak validation layer uses lazy discovery (does not
    // fail at boot), so break-glass + Bearer validation remain available even
    // if Keycloak is momentarily down. Browser login is keycloak-js (PKCE) +
    // Bearer JWT — there is no server-side OIDC login flow to wire.
    //
    // These tower layers are the Keycloak-specific *transport* (token injection).
    // They are raised ONLY in keycloak mode. In local mode (the default, 2b) the
    // `LocalAuthProvider` reads its own session cookie in the `AuthUser`/WS
    // extractors, so no middleware layer is needed and Keycloak is not a runtime
    // dependency of the default deployment.
    let (kc_layer, ws_layer) = if boot.auth.mode == AuthMode::Keycloak {
        if boot.keycloak.is_configured() {
            let instance = std::sync::Arc::new(auth::keycloak::build_instance(&boot.keycloak)?);
            let kc_layer =
                auth::keycloak::auth_layer(instance.clone(), boot.keycloak.client_id.clone());
            let ws_layer =
                auth::keycloak::auth_layer_passthrough(instance, boot.keycloak.client_id.clone());
            (Some(kc_layer), Some(ws_layer))
        } else {
            tracing::warn!("auth.mode=keycloak but Keycloak is not configured; protected API + websocket will reject all requests (break-glass still works)");
            (None, None)
        }
    } else {
        tracing::info!("auth.mode=local — Core email/password login; Keycloak middleware not raised");
        (None, None)
    };

    let addr = format!("{}:{}", boot.server.host, boot.server.port);
    let listener = TcpListener::bind(&addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!(%addr, "fosnie-backend listening");

    // Clone into the router; we keep `state` to drain the audit writer below.
    let app = http::router(state.clone(), kc_layer, ws_layer, extra_routes, extra_public);
    let shutdown_tx_for_signal = shutdown_tx.clone();
    // `ConnectInfo` so the break-glass path can record + rate-limit by source IP.
    axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>())
        .with_graceful_shutdown(async move {
            wait_for_signal().await;
            tracing::info!("shutdown signal received");
            let _ = shutdown_tx_for_signal.send(true);
        })
        .await
        .context("serving HTTP")?;

    // Ensure the scheduler is told to stop, then drain it.
    let _ = shutdown_tx.send(true);
    if let Err(e) = bg.join() {
        tracing::error!(?e, "background runtime thread panicked");
    }

    // Drain the audit writer (re-audit R4b): take its handle, drop the last
    // AppState clone (closing the queue's senders), and wait for the writer to
    // flush the backlog — otherwise the runtime teardown cancels it mid-queue
    // and buffered audit events are lost on graceful shutdown.
    let audit_writer = state.audit_writer.lock().unwrap().take();
    drop(state);
    if let Some(handle) = audit_writer {
        if let Err(e) = handle.await {
            tracing::error!(?e, "audit writer task panicked");
        }
    }
    tracing::info!("fosnie-backend stopped");
    Ok(())
}

/// Resolve on Ctrl-C (all platforms) or SIGTERM (Unix).
async fn wait_for_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {},
            _ = term.recv() => {},
        }
    }

    #[cfg(not(unix))]
    {
        ctrl_c.await;
    }
}
