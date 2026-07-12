//! Extension-surface proof for the `AuditSink` seam.
//!
//! Its own test binary: the sink is a process-global `OnceLock`, so registering a
//! fake here cannot leak into other tests. A `FakeAuditSink` (counts calls, writes
//! nothing) is registered via `audit::init_sink`; both `audit::append` and
//! `audit::append_with` must route to it — proving the global sink is consumed, so
//! Enterprise can register a `TamperEvidentAuditSink`. DB-gated: the append path
//! still opens a transaction even though the fake performs no INSERT.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use fosnie_backend::audit::{self, AppendResult, AuditEvent};
use fosnie_backend::db;
use fosnie_backend::ext::AuditSink;

static CALLS: AtomicU32 = AtomicU32::new(0);

struct FakeAuditSink;

#[async_trait]
impl AuditSink for FakeAuditSink {
    async fn append_with(
        &self,
        _conn: &mut sqlx::PgConnection,
        _event: &AuditEvent,
    ) -> Result<AppendResult, sqlx::Error> {
        CALLS.fetch_add(1, Ordering::SeqCst);
        Ok(AppendResult { seq: 0, id: Uuid::now_v7(), hash: Vec::new() })
    }
}

#[tokio::test]
async fn registered_sink_receives_appends() {
    let Ok(db_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let pg = db::connect(&db_url, 2).await.expect("pg");

    audit::init_sink(Arc::new(FakeAuditSink));

    let ev = AuditEvent::action("test.sink_seam", "user");

    // Path 1: append() opens its own tx, then dispatches to the sink.
    audit::append(&pg, &ev).await.expect("append");

    // Path 2: append_with() on a caller-supplied tx, also via the sink.
    let mut tx = pg.begin().await.expect("begin");
    audit::append_with(&mut tx, &ev).await.expect("append_with");
    tx.commit().await.expect("commit");

    assert_eq!(CALLS.load(Ordering::SeqCst), 2, "both append paths route to the registered sink");
}
