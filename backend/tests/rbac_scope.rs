//! Messaging "circle" scope (RBAC tightening). Skips when DATABASE_URL is unset.
//! A non-admin may only DM / group-chat people they co-belong to a project or group
//! with; `rbac::shares_circle` is that wall.

use std::sync::Arc;

use uuid::Uuid;

use fosnie_backend::auth::rbac;
use fosnie_backend::config::BootConfig;
use fosnie_backend::state::AppState;
use fosnie_backend::{cache, db};

async fn pg() -> Option<sqlx::PgPool> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let pool = db::connect(&db_url, 5).await.ok()?;
    let _ = AppState::new(pool.clone(), cache::create_pool(&redis_url).ok()?, Arc::new(BootConfig::default()));
    Some(pool)
}

async fn mk_user(pool: &sqlx::PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, display_name, email, role) VALUES ($1, $2, $3, 'user')")
        .bind(id)
        .bind(format!("U {}", &id.to_string()[..8]))
        .bind(format!("{id}@circle.test"))
        .execute(pool)
        .await
        .unwrap();
    id
}

#[tokio::test]
async fn circle_is_shared_project_or_group() {
    let Some(pool) = pg().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };

    let a = mk_user(&pool).await; // project owner
    let b = mk_user(&pool).await; // granted on a's project
    let c = mk_user(&pool).await; // unrelated
    let g_user = mk_user(&pool).await; // shares only a group with a

    // a owns a project; b gets a read grant on it.
    let proj = Uuid::now_v7();
    sqlx::query("INSERT INTO projects (id, name, owner_user_id, sector) VALUES ($1, 'Circle Matter', $2, 'legal')")
        .bind(proj).bind(a).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO access_grants (id, resource_type, resource_id, principal_type, principal_id, permission, created_by) \
                 VALUES ($1, 'project', $2, 'user', $3, 'read', $4)")
        .bind(Uuid::now_v7()).bind(proj).bind(b).bind(a).execute(&pool).await.unwrap();

    // a + g_user share a group (no shared project).
    let grp = Uuid::now_v7();
    sqlx::query("INSERT INTO groups (id, name, created_by) VALUES ($1, 'Circle Group', $2)")
        .bind(grp).bind(a).execute(&pool).await.unwrap();
    for u in [a, g_user] {
        sqlx::query("INSERT INTO group_members (group_id, user_id) VALUES ($1, $2)")
            .bind(grp).bind(u).execute(&pool).await.unwrap();
    }

    assert!(rbac::shares_circle(&pool, a, b).await.unwrap(), "shared project ⇒ circle");
    assert!(rbac::shares_circle(&pool, b, a).await.unwrap(), "symmetric");
    assert!(rbac::shares_circle(&pool, a, g_user).await.unwrap(), "shared group ⇒ circle");
    assert!(!rbac::shares_circle(&pool, a, c).await.unwrap(), "no shared project/group ⇒ NOT in circle");
    assert!(!rbac::shares_circle(&pool, b, c).await.unwrap(), "b and c share nothing");
    assert!(!rbac::shares_circle(&pool, b, g_user).await.unwrap(), "b (project only) and g_user (group only) don't overlap");

    // Cleanup (children → parents).
    sqlx::query("DELETE FROM group_members WHERE group_id = $1").bind(grp).execute(&pool).await.unwrap();
    sqlx::query("DELETE FROM groups WHERE id = $1").bind(grp).execute(&pool).await.unwrap();
    sqlx::query("DELETE FROM access_grants WHERE resource_id = $1").bind(proj).execute(&pool).await.unwrap();
    sqlx::query("DELETE FROM projects WHERE id = $1").bind(proj).execute(&pool).await.unwrap();
    sqlx::query("DELETE FROM users WHERE id = ANY($1)").bind(vec![a, b, c, g_user]).execute(&pool).await.unwrap();
}

#[tokio::test]
async fn led_member_ids_covers_own_groups_and_owned_projects() {
    let Some(pool) = pg().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };

    let lead = mk_user(&pool).await; // creates a group + owns a project
    let m1 = mk_user(&pool).await; // member of the lead's group
    let m2 = mk_user(&pool).await; // direct user-grant on the lead's project
    let m3 = mk_user(&pool).await; // via a group-grant on the lead's project
    let outsider = mk_user(&pool).await; // unrelated — must NOT appear

    // Group the lead created, with m1 in it.
    let g1 = Uuid::now_v7();
    sqlx::query("INSERT INTO groups (id, name, created_by) VALUES ($1, 'Led Team', $2)")
        .bind(g1).bind(lead).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO group_members (group_id, user_id) VALUES ($1, $2)")
        .bind(g1).bind(m1).execute(&pool).await.unwrap();

    // A second group (NOT created by the lead) holding m3, used as a project grant principal.
    let g2 = Uuid::now_v7();
    sqlx::query("INSERT INTO groups (id, name, created_by) VALUES ($1, 'Other Group', $2)")
        .bind(g2).bind(outsider).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO group_members (group_id, user_id) VALUES ($1, $2)")
        .bind(g2).bind(m3).execute(&pool).await.unwrap();

    // Project owned by the lead; m2 via a direct user grant, g2 (⇒ m3) via a group grant.
    let proj = Uuid::now_v7();
    sqlx::query("INSERT INTO projects (id, name, owner_user_id, sector) VALUES ($1, 'Led Matter', $2, 'legal')")
        .bind(proj).bind(lead).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO access_grants (id, resource_type, resource_id, principal_type, principal_id, permission, created_by) \
                 VALUES ($1, 'project', $2, 'user', $3, 'read', $4)")
        .bind(Uuid::now_v7()).bind(proj).bind(m2).bind(lead).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO access_grants (id, resource_type, resource_id, principal_type, principal_id, permission, created_by) \
                 VALUES ($1, 'project', $2, 'group', $3, 'read', $4)")
        .bind(Uuid::now_v7()).bind(proj).bind(g2).bind(lead).execute(&pool).await.unwrap();

    let ids = rbac::led_member_ids(&pool, lead).await.unwrap();
    let has = |u: Uuid| ids.contains(&u);
    assert!(has(lead), "lead includes self");
    assert!(has(m1), "member of a group the lead created");
    assert!(has(m2), "direct user-grant on the lead's project");
    assert!(has(m3), "member of a group granted on the lead's project");
    assert!(!has(outsider), "unrelated user is NOT a led member");

    // A plain member leads nothing → only themselves.
    let m1_led = rbac::led_member_ids(&pool, m1).await.unwrap();
    assert_eq!(m1_led, vec![m1], "a non-lead's led set is just self");

    // Cleanup (children → parents).
    sqlx::query("DELETE FROM access_grants WHERE resource_id = $1").bind(proj).execute(&pool).await.unwrap();
    sqlx::query("DELETE FROM projects WHERE id = $1").bind(proj).execute(&pool).await.unwrap();
    sqlx::query("DELETE FROM group_members WHERE group_id = ANY($1)").bind(vec![g1, g2]).execute(&pool).await.unwrap();
    sqlx::query("DELETE FROM groups WHERE id = ANY($1)").bind(vec![g1, g2]).execute(&pool).await.unwrap();
    sqlx::query("DELETE FROM users WHERE id = ANY($1)").bind(vec![lead, m1, m2, m3, outsider]).execute(&pool).await.unwrap();
}
