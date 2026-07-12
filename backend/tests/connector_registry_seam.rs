//! Extension-surface proof for the `ConnectorRegistry` seam.
//!
//! A `FakeConnectorRegistry` injected via `AppStateBuilder::with_connectors`
//! returns a fake DMS adapter for `IManage`; with the connector enabled in config,
//! `dms_search` must reach the fake (not the Core `NotBuilt` stub) — proving the
//! slot is consumed (Enterprise can register a real iManage). Needs Postgres
//! (:5433) + Redis (guard_egress reads config + audits); skips if DATABASE_URL unset.

use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use fosnie_backend::auth::{AuthContext, PlatformRole};
use fosnie_backend::config::BootConfig;
use fosnie_backend::error::Result;
use fosnie_backend::ext::ConnectorRegistry;
use fosnie_backend::integrations::dms::{Cursor, DmsConnector, DmsDoc, Page};
use fosnie_backend::integrations::{dms, ConnectorKind};
use fosnie_backend::state::{AppState, AppStateBuilder};
use fosnie_backend::{cache, db};

struct FakeDms;

#[async_trait]
impl DmsConnector for FakeDms {
    async fn authenticate(&self, _s: &AppState, _c: &AuthContext) -> Result<()> {
        Ok(())
    }
    async fn search(&self, _s: &AppState, _c: &AuthContext, _q: &str, _cur: Option<Cursor>) -> Result<Page<DmsDoc>> {
        Ok(Page {
            items: vec![DmsDoc { id: "d1".into(), name: "Matter A".into(), mime: None, ..Default::default() }],
            next: None,
        })
    }
    async fn fetch_document(&self, _s: &AppState, _c: &AuthContext, _id: &str) -> Result<Vec<u8>> {
        Ok(Vec::new())
    }
    async fn write_back(&self, _s: &AppState, _c: &AuthContext, _id: &str, _b: &[u8]) -> Result<()> {
        Ok(())
    }
}

struct FakeConnectorRegistry;

impl ConnectorRegistry for FakeConnectorRegistry {
    fn resolve_dms(&self, kind: ConnectorKind) -> Option<Box<dyn DmsConnector>> {
        if kind == ConnectorKind::IManage {
            Some(Box::new(FakeDms))
        } else {
            None
        }
    }
}

async fn setup(connectors: Option<Arc<dyn ConnectorRegistry>>) -> Option<(AppState, sqlx::PgPool)> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let redis_url = std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let pg = db::connect(&db_url, 5).await.ok()?;
    let redis = cache::create_pool(&redis_url).ok()?;
    let boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    let mut b = AppStateBuilder::new(pg.clone(), redis, Arc::new(boot));
    if let Some(c) = connectors {
        b = b.with_connectors(c);
    }
    Some((b.build(), pg))
}

fn ctx() -> AuthContext {
    AuthContext {
        user_id: Some(Uuid::now_v7()),
        email: None,
        display_name: None,
        role: PlatformRole::ClientAdmin,
        break_glass: false, mfa_enroll_only: false,
    }
}

async fn enable_imanage(pg: &sqlx::PgPool) {
    sqlx::query("INSERT INTO config_settings (key, value, value_type, scope) VALUES ('integration.imanage.enabled','true','bool','global') ON CONFLICT (key) DO UPDATE SET value='true'")
        .execute(pg)
        .await
        .unwrap();
}

async fn disable_imanage(pg: &sqlx::PgPool) {
    let _ = sqlx::query("DELETE FROM config_settings WHERE key = 'integration.imanage.enabled'").execute(pg).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn injected_registry_serves_dms_search() {
    let Some((state, pg)) = setup(Some(Arc::new(FakeConnectorRegistry))).await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    enable_imanage(&pg).await;

    let page = dms::dms_search(&state, &ctx(), ConnectorKind::IManage, "matter")
        .await
        .expect("fake adapter returns a page");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].id, "d1");

    disable_imanage(&pg).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn default_registry_is_not_built() {
    let Some((state, pg)) = setup(None).await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    enable_imanage(&pg).await;

    // Default registry → NotBuilt → honest Unavailable error (behaviour-identical).
    let res = dms::dms_search(&state, &ctx(), ConnectorKind::IManage, "matter").await;
    assert!(res.is_err(), "default registry yields the NotBuilt error");

    disable_imanage(&pg).await;
}
