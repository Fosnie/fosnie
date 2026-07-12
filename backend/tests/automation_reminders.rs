//! Calendar reminders (Tier-2 #16): the scheduler pushes a lookahead reminder to
//! an automation's owner once, shortly before it is due. Drives
//! `scheduler::scan_reminders` against a fake hub socket. Skips when DATABASE_URL
//! is unset.

use std::sync::Arc;

use fosnie_backend::config::BootConfig;
use fosnie_backend::state::AppState;
use fosnie_backend::ws::protocol::ServerFrame;
use fosnie_backend::{cache, db, scheduler};
use tokio::sync::mpsc;
use uuid::Uuid;

async fn state() -> Option<(AppState, sqlx::PgPool)> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let pg = db::connect(&db_url, 5).await.ok()?;
    let redis = cache::create_pool(&redis_url).ok()?;
    let boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    Some((AppState::new(pg.clone(), redis, Arc::new(boot)), pg))
}

#[tokio::test]
async fn reminds_once_for_soon_due_automation() {
    let Some((st, pg)) = state().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let owner: Uuid = sqlx::query_scalar("SELECT id FROM users LIMIT 1").fetch_one(&pg).await.unwrap();

    // Due in ~5 minutes — inside the 600s default lookahead.
    let soon = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO automations (id, owner_user_id, name, schedule, prompt, status, next_run_at) \
         VALUES ($1, $2, 'Daily brief', '0 0 9 * * *', 'summarise', 'active', now() + interval '5 minutes')",
    )
    .bind(soon).bind(owner).execute(&pg).await.unwrap();

    // One far in the future — must NOT be reminded.
    let later = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO automations (id, owner_user_id, name, schedule, prompt, status, next_run_at) \
         VALUES ($1, $2, 'Weekly', '0 0 9 * * Mon', 'x', 'active', now() + interval '2 hours')",
    )
    .bind(later).bind(owner).execute(&pg).await.unwrap();

    // Register a fake socket for the owner to capture pushed frames.
    let (tx, mut rx) = mpsc::channel::<ServerFrame>(16);
    st.hub.register(Uuid::now_v7(), owner, tx);

    let sent = scheduler::scan_reminders(&st).await.unwrap();
    assert!(sent >= 1, "the soon-due automation is reminded");

    // The captured frame is for the soon-due automation, within the window.
    let mut saw = false;
    while let Ok(f) = rx.try_recv() {
        if let ServerFrame::AutomationReminder { automation_id, in_seconds, .. } = f {
            if automation_id == soon {
                assert!(in_seconds > 0 && in_seconds <= 600, "due within the lookahead");
                assert!(automation_id != later, "the far-future one is not reminded");
                saw = true;
            }
        }
    }
    assert!(saw, "a reminder frame for the soon-due automation arrived");

    // Second scan: already reminded for this occurrence → no duplicate.
    let again = scheduler::scan_reminders(&st).await.unwrap();
    let mut dup = false;
    while let Ok(f) = rx.try_recv() {
        if let ServerFrame::AutomationReminder { automation_id, .. } = f {
            if automation_id == soon { dup = true; }
        }
    }
    assert!(!dup, "no duplicate reminder for the same occurrence (sent={again})");

    let _ = sqlx::query("DELETE FROM automations WHERE id = ANY($1)").bind(vec![soon, later]).execute(&pg).await;
}
