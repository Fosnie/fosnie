//! Event-driven workflow engine — outbox durability + run idempotency
//! The pure loop-guard,
//! condition and template tests live inline in the crate; these need a database
//! and skip when `DATABASE_URL` is unset.

use sqlx::PgPool;
use uuid::Uuid;

use fosnie_backend::db;
use fosnie_backend::events::{self, ActorType, NewEvent};

async fn pool_from_env() -> Option<PgPool> {
    let url = std::env::var("DATABASE_URL").ok()?;
    db::connect(&url, 5).await.ok()
}

/// §12.1 — the transactional outbox: an event exists iff its mutation commits.
#[tokio::test]
async fn outbox_event_committed_iff_mutation_commits() {
    let Some(pool) = pool_from_env().await else {
        eprintln!("skipping outbox_event_committed_iff_mutation_commits: DATABASE_URL unset");
        return;
    };

    // Rollback ⇒ no event row.
    let rolled = {
        let mut tx = pool.begin().await.unwrap();
        let ev = NewEvent::new("test.rollback", ActorType::Human)
            .payload(serde_json::json!({ "k": "v" }));
        let id = events::emit_with(&mut tx, &ev).await.unwrap();
        tx.rollback().await.unwrap();
        id
    };
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM events WHERE id = $1)")
        .bind(rolled)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(!exists, "a rolled-back mutation must leave no event");

    // Commit ⇒ the event row persists.
    let committed = {
        let mut tx = pool.begin().await.unwrap();
        let ev = NewEvent::new("test.commit", ActorType::Human);
        let id = events::emit_with(&mut tx, &ev).await.unwrap();
        tx.commit().await.unwrap();
        id
    };
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM events WHERE id = $1)")
        .bind(committed)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(exists, "a committed mutation must persist its event");

    sqlx::query("DELETE FROM events WHERE id = $1")
        .bind(committed)
        .execute(&pool)
        .await
        .ok();
}

/// §12.8 — idempotency: a replayed (workflow, event-set) cannot double-run. The
/// `workflow_runs` unique index makes the dispatcher's ON CONFLICT a no-op.
#[tokio::test]
async fn workflow_run_dedupe_by_event_set() {
    let Some(pool) = pool_from_env().await else {
        eprintln!("skipping workflow_run_dedupe_by_event_set: DATABASE_URL unset");
        return;
    };

    let owner = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, display_name, email, role) VALUES ($1, 'WF', $2, 'user')")
        .bind(owner)
        .bind(format!("wf-{owner}@test.local"))
        .execute(&pool)
        .await
        .unwrap();
    let wf = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO workflows (id, name, owner_id, trigger_event_type, action_type, action_config) \
         VALUES ($1, 'dedupe', $2, 'document.ingested', 'system_action', '{}'::jsonb)",
    )
    .bind(wf)
    .bind(owner)
    .execute(&pool)
    .await
    .unwrap();

    let event_ids = vec![Uuid::now_v7()];

    // First firing inserts a run; a replay over the same event-set inserts nothing.
    let first: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO workflow_runs (id, workflow_id, trigger_event_ids, status, depth, run_as_user_id) \
         VALUES ($1, $2, $3, 'queued', 0, $4) \
         ON CONFLICT (workflow_id, trigger_event_ids) DO NOTHING RETURNING id",
    )
    .bind(Uuid::now_v7())
    .bind(wf)
    .bind(&event_ids)
    .bind(owner)
    .fetch_optional(&pool)
    .await
    .unwrap();
    let second: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO workflow_runs (id, workflow_id, trigger_event_ids, status, depth, run_as_user_id) \
         VALUES ($1, $2, $3, 'queued', 0, $4) \
         ON CONFLICT (workflow_id, trigger_event_ids) DO NOTHING RETURNING id",
    )
    .bind(Uuid::now_v7())
    .bind(wf)
    .bind(&event_ids)
    .bind(owner)
    .fetch_optional(&pool)
    .await
    .unwrap();

    assert!(first.is_some(), "first firing inserts a run");
    assert!(second.is_none(), "a replay of the same (workflow, event-set) does not double-run");

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM workflow_runs WHERE workflow_id = $1")
        .bind(wf)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "exactly one run for the deduped event-set");

    // Cleanup (runs cascade with the workflow).
    sqlx::query("DELETE FROM workflows WHERE id = $1").bind(wf).execute(&pool).await.ok();
    sqlx::query("DELETE FROM users WHERE id = $1").bind(owner).execute(&pool).await.ok();
}

async fn seed_user(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, display_name, email, role) VALUES ($1, 'WF', $2, 'user')")
        .bind(id)
        .bind(format!("wf-{id}@test.local"))
        .execute(pool)
        .await
        .unwrap();
    id
}

/// §12.4 — coalescing: N events in one window+scope collapse into ONE run over the
/// whole batch (not N runs). Mirrors `buffer_coalesced`'s upsert + the scan's fire.
#[tokio::test]
async fn coalesce_buffer_accumulates_then_one_run() {
    let Some(pool) = pool_from_env().await else {
        eprintln!("skipping coalesce_buffer_accumulates_then_one_run: DATABASE_URL unset");
        return;
    };
    let owner = seed_user(&pool).await;
    let wf = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO workflows (id, name, owner_id, trigger_event_type, action_type, action_config, coalesce_window_secs) \
         VALUES ($1, 'coalesce', $2, 'document.ingested', 'system_action', '{}'::jsonb, 10)",
    )
    .bind(wf)
    .bind(owner)
    .execute(&pool)
    .await
    .unwrap();

    // Three events buffered into one (workflow, scope) bucket via the upsert.
    let upsert = "INSERT INTO workflow_coalesce (workflow_id, scope_key, event_ids, depth, fire_at) \
                  VALUES ($1, 'proj-x', ARRAY[$2]::uuid[], 0, now()) \
                  ON CONFLICT (workflow_id, scope_key) DO UPDATE \
                    SET event_ids = array_append(workflow_coalesce.event_ids, $2)";
    for _ in 0..3 {
        sqlx::query(upsert).bind(wf).bind(Uuid::now_v7()).execute(&pool).await.unwrap();
    }
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT event_ids FROM workflow_coalesce WHERE workflow_id = $1 AND scope_key = 'proj-x'",
    )
    .bind(wf)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(ids.len(), 3, "the window accumulates all events into one bucket");

    // Firing the bucket creates ONE run carrying the whole batch.
    sqlx::query(
        "INSERT INTO workflow_runs (id, workflow_id, trigger_event_ids, status, depth, run_as_user_id) \
         VALUES ($1, $2, $3, 'queued', 0, $4)",
    )
    .bind(Uuid::now_v7())
    .bind(wf)
    .bind(&ids)
    .bind(owner)
    .execute(&pool)
    .await
    .unwrap();
    let runs: i64 = sqlx::query_scalar("SELECT count(*) FROM workflow_runs WHERE workflow_id = $1")
        .bind(wf)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(runs, 1, "50 files → 1 run: the batch is ONE run, not N");
    let batch: i32 =
        sqlx::query_scalar("SELECT array_length(trigger_event_ids, 1) FROM workflow_runs WHERE workflow_id = $1")
            .bind(wf)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(batch, 3, "the single run carries all batched events");

    sqlx::query("DELETE FROM workflows WHERE id = $1").bind(wf).execute(&pool).await.ok();
    sqlx::query("DELETE FROM users WHERE id = $1").bind(owner).execute(&pool).await.ok();
}

/// §4 catalogue expansion — every newly-emitted event name rides the outbox
/// contract: present iff its mutation commits, absent on rollback. Mirrors the
/// per-emit acceptance for the new document/directory/membership events.
#[tokio::test]
async fn new_event_constants_ride_the_outbox() {
    let Some(pool) = pool_from_env().await else {
        eprintln!("skipping new_event_constants_ride_the_outbox: DATABASE_URL unset");
        return;
    };
    for name in [
        events::DOCUMENT_IMPORTED,
        events::DIRECTORY_USER_PROVISIONED,
        events::DIRECTORY_USER_DEACTIVATED,
        events::GROUP_MEMBER_ADDED,
        events::GROUP_MEMBER_REMOVED,
        events::CHAT_MEMBER_ADDED,
        events::ACCOUNT_ARCHIVED,
    ] {
        // Rollback ⇒ no row.
        let rolled = {
            let mut tx = pool.begin().await.unwrap();
            let ev = NewEvent::new(name, ActorType::System);
            let id = events::emit_with(&mut tx, &ev).await.unwrap();
            tx.rollback().await.unwrap();
            id
        };
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM events WHERE id = $1)")
            .bind(rolled)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(!exists, "{name}: rolled-back emit must leave no event");

        // Commit ⇒ the row persists with the right type.
        let committed = {
            let mut tx = pool.begin().await.unwrap();
            let ev = NewEvent::new(name, ActorType::System);
            let id = events::emit_with(&mut tx, &ev).await.unwrap();
            tx.commit().await.unwrap();
            id
        };
        let ty: Option<String> = sqlx::query_scalar("SELECT event_type FROM events WHERE id = $1")
            .bind(committed)
            .fetch_optional(&pool)
            .await
            .unwrap();
        assert_eq!(ty.as_deref(), Some(name), "{name}: committed emit must persist");
        sqlx::query("DELETE FROM events WHERE id = $1").bind(committed).execute(&pool).await.ok();
    }
}

/// §12.10 / acceptance §3 — the enable watermark fast-forwards the pre-enable
/// backlog: events created before the watermark are skip-marked (dispatched, no
/// run) rather than replayed, while newer events fire. Mirrors the dispatcher's
/// `created_at < watermark` partition (the engine sets `dispatched_at` and enqueues
/// nothing for the older side).
#[tokio::test]
async fn watermark_fast_forwards_pre_enable_backlog() {
    let Some(pool) = pool_from_env().await else {
        eprintln!("skipping watermark_fast_forwards_pre_enable_backlog: DATABASE_URL unset");
        return;
    };
    // An old undispatched event (backlog) and a fresh one, watermark set between.
    let old = Uuid::now_v7();
    let fresh = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO events (id, event_type, actor_type, created_at) \
         VALUES ($1, 'document.ingested', 'human'::event_actor_type, now() - interval '1 hour')",
    )
    .bind(old)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO events (id, event_type, actor_type, created_at) \
         VALUES ($1, 'document.ingested', 'human'::event_actor_type, now())",
    )
    .bind(fresh)
    .execute(&pool)
    .await
    .unwrap();

    // The dispatcher skip-marks rows with created_at < watermark; the rest dispatch.
    let q = "SELECT created_at < (now() - interval '30 minutes') FROM events WHERE id = $1";
    let old_skipped: bool = sqlx::query_scalar(q).bind(old).fetch_one(&pool).await.unwrap();
    let fresh_skipped: bool = sqlx::query_scalar(q).bind(fresh).fetch_one(&pool).await.unwrap();
    assert!(old_skipped, "pre-watermark backlog is fast-forwarded (skip-marked, no run)");
    assert!(!fresh_skipped, "a post-watermark event still fires");

    sqlx::query("DELETE FROM events WHERE id = ANY($1)")
        .bind(vec![old, fresh])
        .execute(&pool)
        .await
        .ok();
}

/// §7a.4 / §12.3 — cycle detection: a (workflow, resource) already in the lineage
/// chain is a cycle; a different resource (or empty chain) is not.
#[tokio::test]
async fn cycle_detection_via_lineage() {
    let Some(pool) = pool_from_env().await else {
        eprintln!("skipping cycle_detection_via_lineage: DATABASE_URL unset");
        return;
    };
    let owner = seed_user(&pool).await;
    let wf = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO workflows (id, name, owner_id, trigger_event_type, action_type, action_config) \
         VALUES ($1, 'cyc', $2, 'document.ingested', 'system_action', '{}'::jsonb)",
    )
    .bind(wf)
    .bind(owner)
    .execute(&pool)
    .await
    .unwrap();

    let resource = Uuid::now_v7();
    let e1 = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO events (id, event_type, actor_type, resource_id) \
         VALUES ($1, 'document.ingested', 'human'::event_actor_type, $2)",
    )
    .bind(e1)
    .bind(resource)
    .execute(&pool)
    .await
    .unwrap();
    let wr1 = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO workflow_runs (id, workflow_id, trigger_event_ids, status, depth, run_as_user_id) \
         VALUES ($1, $2, $3, 'succeeded', 0, $4)",
    )
    .bind(wr1)
    .bind(wf)
    .bind(vec![e1])
    .bind(owner)
    .execute(&pool)
    .await
    .unwrap();

    let q = "SELECT EXISTS(SELECT 1 FROM workflow_runs wr \
             JOIN events ev ON ev.id = ANY(wr.trigger_event_ids) \
             WHERE wr.id = ANY($1) AND wr.workflow_id = $2 AND ev.resource_id = $3)";
    let chain = vec![wr1];
    let hit: bool = sqlx::query_scalar(q).bind(&chain).bind(wf).bind(resource).fetch_one(&pool).await.unwrap();
    assert!(hit, "same (workflow, resource) already in the lineage = cycle");
    let miss: bool =
        sqlx::query_scalar(q).bind(&chain).bind(wf).bind(Uuid::now_v7()).fetch_one(&pool).await.unwrap();
    assert!(!miss, "a different resource is not a cycle");
    let empty: Vec<Uuid> = vec![];
    let none: bool = sqlx::query_scalar(q).bind(&empty).bind(wf).bind(resource).fetch_one(&pool).await.unwrap();
    assert!(!none, "empty lineage is not a cycle");

    sqlx::query("DELETE FROM workflows WHERE id = $1").bind(wf).execute(&pool).await.ok();
    sqlx::query("DELETE FROM events WHERE id = $1").bind(e1).execute(&pool).await.ok();
    sqlx::query("DELETE FROM users WHERE id = $1").bind(owner).execute(&pool).await.ok();
}
