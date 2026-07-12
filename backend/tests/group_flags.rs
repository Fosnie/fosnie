//! Per-group feature flags (Tier-2 #8): restrict-only resolution. A group flag
//! can only turn a host feature OFF for its members; the global setting is the
//! ceiling. Exercises `features::enabled_for_user` directly against Postgres.
//! Skips when DATABASE_URL is unset.

use std::sync::Arc;

use fosnie_backend::config::BootConfig;
use fosnie_backend::features;
use fosnie_backend::state::AppState;
use fosnie_backend::{cache, db};
use uuid::Uuid;

async fn state_with_voice(voice: bool, code: bool) -> Option<(AppState, sqlx::PgPool)> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let pg = db::connect(&db_url, 5).await.ok()?;
    let redis = cache::create_pool(&redis_url).ok()?;
    let mut boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    boot.features.voice = voice;
    boot.features.code_interpreter = code;
    Some((AppState::new(pg.clone(), redis, Arc::new(boot)), pg))
}

#[tokio::test]
async fn group_flag_restricts_only_within_ceiling() {
    let Some((state, pg)) = state_with_voice(true, true).await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };

    // Two distinct users: one will be in a voice-disabled group, one won't.
    let users: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM users ORDER BY id LIMIT 2")
        .fetch_all(&pg)
        .await
        .unwrap();
    if users.len() < 2 {
        eprintln!("skip: need ≥2 users");
        return;
    }
    let (member, outsider) = (users[0], users[1]);

    let gid = Uuid::now_v7();
    sqlx::query("INSERT INTO groups (id, name) VALUES ($1, 'voice-restricted')")
        .bind(gid).execute(&pg).await.unwrap();
    sqlx::query("INSERT INTO group_members (group_id, user_id) VALUES ($1, $2)")
        .bind(gid).bind(member).execute(&pg).await.unwrap();
    sqlx::query("INSERT INTO group_feature_flags (group_id, feature, enabled) VALUES ($1, 'voice', false)")
        .bind(gid).execute(&pg).await.unwrap();

    // Global voice ON: the group disables it for its member, not for the outsider.
    assert!(!features::enabled_for_user(&state, Some(member), "voice").await, "member's group disables voice");
    assert!(features::enabled_for_user(&state, Some(outsider), "voice").await, "outsider keeps voice");
    // No flag for code_interpreter → both inherit the global (on).
    assert!(features::enabled_for_user(&state, Some(member), "code_interpreter").await);

    // Restrict-only: with global voice OFF, nobody gets it (the flag can't enable).
    let (state_off, _) = state_with_voice(false, true).await.unwrap();
    assert!(!features::enabled_for_user(&state_off, Some(member), "voice").await);
    assert!(!features::enabled_for_user(&state_off, Some(outsider), "voice").await, "global off is the ceiling");

    sqlx::query("DELETE FROM groups WHERE id = $1").bind(gid).execute(&pg).await.unwrap();
}
