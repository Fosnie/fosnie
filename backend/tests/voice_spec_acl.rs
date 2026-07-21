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

//! A speculative search reads the speaker's knowledge bases before there is a turn
//! to authorise it, so it must reach exactly the same scope the committed turn
//! would, and nothing wider.
//!
//! The scope is resolved the same way in both cases, which is what makes access
//! control inherited rather than reimplemented. What is worth proving is the one
//! place they *look* different: speculation can happen before the chat row exists,
//! on the first thing a speaker says. That must narrow the scope to precisely what
//! the committed turn will see, not to something broader.
//!
//! Needs a reachable Postgres; skips when `DATABASE_URL` is unset.

use sqlx::PgPool;
use uuid::Uuid;

use fosnie_backend::auth::{AuthContext, PlatformRole};
use fosnie_backend::db;
use fosnie_backend::kb;

async fn pool() -> Option<PgPool> {
    let url = std::env::var("DATABASE_URL").ok()?;
    // Skipping is for "no database configured". A database that IS configured but
    // cannot be reached is an environment fault, and quietly reporting a pass for it
    // is how an untested change looks tested.
    Some(
        db::connect(&url, 5)
            .await
            .unwrap_or_else(|e| panic!("DATABASE_URL is set but unreachable, so nothing here was tested: {e}")),
    )
}

fn ctx(user_id: Uuid) -> AuthContext {
    AuthContext {
        user_id: Some(user_id),
        email: None,
        display_name: None,
        role: PlatformRole::User,
        break_glass: false,
        mfa_enroll_only: false,
    }
}

async fn mk_user(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, display_name, email, role) VALUES ($1, 'T', $2, 'user')")
        .bind(id)
        .bind(format!("{id}@example.test"))
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn mk_project(pool: &PgPool, owner: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO projects (id, name, owner_user_id) VALUES ($1, 'P', $2)")
        .bind(id)
        .bind(owner)
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn mk_chat(pool: &PgPool, owner: Uuid, project: Option<Uuid>) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO chats (id, project_id, owner_user_id, title) VALUES ($1, $2, $3, 'C')")
        .bind(id)
        .bind(project)
        .bind(owner)
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn mk_kb(pool: &PgPool, owner: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO knowledge_bases \
           (id, name, owner_id, visibility, embedding_model_id, embedding_dimension, status) \
         VALUES ($1, 'KB', $2, 'shared', 'test-model', 1024, 'ready')",
    )
    .bind(id)
    .bind(owner)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO kb_access_grants (id, kb_id, principal_type, principal_id, permission, granted_by) \
         VALUES ($1, $2, 'user', $3, 'read'::kb_permission, $3)",
    )
    .bind(Uuid::now_v7())
    .bind(id)
    .bind(owner)
    .execute(pool)
    .await
    .unwrap();
    id
}

/// Before the chat row exists there is no chat id to resolve with. Using the nil id
/// must yield exactly what the committed turn will see — not merely a subset, or the
/// speaker would get a different set of sources depending on when they spoke.
///
/// It holds because the chat-linked arm of the scope is the only one that depends on
/// the chat, and a chat that does not exist yet has nothing linked to it. Both the
/// project-linked and the personally-readable arms are unaffected.
#[tokio::test]
async fn speculating_before_the_chat_exists_sees_the_same_scope() {
    let Some(pool) = pool().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let user = mk_user(&pool).await;
    let project = mk_project(&pool, user).await;
    let kb = mk_kb(&pool, user).await;
    sqlx::query("INSERT INTO project_kb_links (project_id, kb_id) VALUES ($1, $2)")
        .bind(project)
        .bind(kb)
        .execute(&pool)
        .await
        .unwrap();

    let cx = ctx(user);
    // What speculation resolves on the very first thing said, with no chat yet.
    let speculative = kb::retrieval_allowlist(&pool, &cx, Uuid::nil(), Some(project), None).await.unwrap();
    // What the turn resolves once its chat has been created — freshly, with nothing
    // attached to it, exactly as a live voice turn creates it.
    let chat = mk_chat(&pool, user, Some(project)).await;
    let committed = kb::retrieval_allowlist(&pool, &cx, chat, Some(project), None).await.unwrap();

    assert!(speculative.contains(&kb), "the project's Library is in scope for speculation");
    assert_eq!(speculative, committed, "speculation before the chat exists must see exactly the turn's scope");

    cleanup(&pool, user, project, chat, kb).await;
}

/// The other direction, and the one that would be a leak: anything attached to the
/// chat itself must not reach speculation that has no chat to attach it from. There
/// is nothing to leak *into* here, but the assertion pins the boundary so a future
/// change to how the nil id is treated cannot widen it silently.
#[tokio::test]
async fn the_nil_chat_id_cannot_pick_up_another_chats_libraries() {
    let Some(pool) = pool().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let user = mk_user(&pool).await;
    let kb = mk_kb(&pool, user).await;
    let other_chat = mk_chat(&pool, user, None).await;
    sqlx::query("INSERT INTO chat_kb_links (chat_id, kb_id) VALUES ($1, $2)")
        .bind(other_chat)
        .bind(kb)
        .execute(&pool)
        .await
        .unwrap();

    let cx = ctx(user);
    let speculative = kb::retrieval_allowlist(&pool, &cx, Uuid::nil(), None, None).await.unwrap();
    assert!(
        speculative.is_empty(),
        "a Library attached to a DIFFERENT chat must never be in scope for speculation, \
         and a readable-but-unattached Library must not leak in either"
    );

    sqlx::query("DELETE FROM chat_kb_links WHERE chat_id = $1").bind(other_chat).execute(&pool).await.ok();
    cleanup(&pool, user, Uuid::nil(), other_chat, kb).await;
}

async fn cleanup(pool: &PgPool, user: Uuid, project: Uuid, chat: Uuid, kb: Uuid) {
    sqlx::query("DELETE FROM chat_kb_links WHERE chat_id = $1").bind(chat).execute(pool).await.ok();
    sqlx::query("DELETE FROM chats WHERE id = $1").bind(chat).execute(pool).await.ok();
    sqlx::query("DELETE FROM project_kb_links WHERE project_id = $1").bind(project).execute(pool).await.ok();
    sqlx::query("DELETE FROM projects WHERE id = $1").bind(project).execute(pool).await.ok();
    sqlx::query("DELETE FROM kb_access_grants WHERE kb_id = $1").bind(kb).execute(pool).await.ok();
    sqlx::query("DELETE FROM knowledge_bases WHERE id = $1").bind(kb).execute(pool).await.ok();
    sqlx::query("DELETE FROM users WHERE id = $1").bind(user).execute(pool).await.ok();
}
