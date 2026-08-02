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

//! Writing down what a caller wanted, and who may read it afterwards.
//!
//! The tools are driven the way a turn drives them, through the authorisation seam and
//! not around it: the witness `dispatch` demands cannot be made from outside the registry,
//! so the only way in from here is the same gate a real call meets. That is the point
//! rather than an inconvenience.
//!
//! Two properties carry the slice. A record can only be written by somebody who is
//! actually on a telephone, and a record's words belong to the account whose line took
//! them: whoever may wire that line may see that it took a message and not what it says.
//!
//! Needs a reachable Postgres and Redis; skips when `DATABASE_URL` is unset.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Query, State};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use fosnie_backend::auth::keycloak::AuthUser;
use fosnie_backend::auth::{AuthContext, PlatformRole};
use fosnie_backend::config::BootConfig;
use fosnie_backend::http::enquiries::{self, EnquiryQuery};
use fosnie_backend::state::{AppState, AppStateBuilder};
use fosnie_backend::tools::phone::{self, CallToolCtx};
use fosnie_backend::tools::{self, AuthorisedTools, NativeDecision};
use fosnie_backend::{cache, db};

/// Somebody who may wire a line without administering the platform.
///
/// This edition answers every permission question with "are you an administrator?", so
/// the delegated holder the redaction rule is written against cannot be made here at all
/// without saying so. It is made through the seam that exists for exactly this, which is
/// also how the edition that ships delegated administration provides one: the rule being
/// tested is the projection's, and it must hold whoever is asking.
struct DelegatedTelephony;

#[async_trait::async_trait]
impl fosnie_backend::ext::RbacPolicy for DelegatedTelephony {
    async fn can(
        &self,
        _pool: &PgPool,
        ctx: &AuthContext,
        _resource_type: fosnie_backend::auth::rbac::ResourceType,
        _resource_id: Uuid,
        _permission: fosnie_backend::auth::rbac::Permission,
    ) -> fosnie_backend::Result<bool> {
        Ok(ctx.is_admin())
    }

    async fn may_grant(
        &self,
        _pool: &PgPool,
        granter: &AuthContext,
        _resource_type: fosnie_backend::auth::rbac::ResourceType,
        _resource_id: Uuid,
    ) -> fosnie_backend::Result<bool> {
        Ok(granter.is_admin())
    }

    /// Everyone holds the one permission this file is about, and nothing else changes.
    async fn has_permission(
        &self,
        _pool: &PgPool,
        ctx: &AuthContext,
        permission: &str,
    ) -> fosnie_backend::Result<bool> {
        Ok(ctx.is_admin() || permission == fosnie_backend::auth::permissions::TELEPHONY_MANAGE)
    }
}

/// Everything one run of this file owns, so it can be taken away again.
struct Cast {
    state: AppState,
    pg: PgPool,
    owner: Uuid,
    agent: Uuid,
    line: Uuid,
    call: Uuid,
    chat: Uuid,
}

fn ctx_for(user: Uuid, role: PlatformRole) -> AuthContext {
    AuthContext {
        user_id: Some(user),
        email: None,
        display_name: None,
        role,
        break_glass: false,
        mfa_enroll_only: false,
    }
}

async fn cast() -> Option<Cast> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:16379".into());
    let pg = db::connect(&db_url, 5).await.unwrap_or_else(|e| {
        panic!("DATABASE_URL is set but unreachable, so nothing here was tested: {e}")
    });
    let redis = cache::create_pool(&redis_url).expect("redis pool");
    let mut boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    // The family is gated on the deployment answering a telephone at all.
    boot.features.telephony = true;
    boot.message_encryption_key = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=".into();
    let state = AppStateBuilder::new(pg.clone(), redis, Arc::new(boot))
        .with_rbac(Arc::new(DelegatedTelephony))
        .build();

    let owner = mk_user(&pg, "Line owner").await;
    let agent = mk_agent(&pg, owner).await;
    let line = mk_line(&pg, owner, agent).await;
    let call = mk_call(&pg, owner, agent, Some(line), "+447700900123").await;
    let chat = mk_chat(&pg, owner, agent).await;
    Some(Cast { state, pg, owner, agent, line, call, chat })
}

async fn clear_up(c: &Cast) {
    // In the order the erasure routine uses, because it is the order the references
    // permit: what points at a thing goes before the thing.
    for sql in [
        "DELETE FROM enquiries WHERE owner_user_id = $1",
        "DELETE FROM calls WHERE owner_user_id = $1",
        "DELETE FROM phone_numbers WHERE owner_user_id = $1",
        "DELETE FROM group_chat_members WHERE user_id = $1",
        "DELETE FROM group_chats WHERE created_by = $1",
        "DELETE FROM chats WHERE owner_user_id = $1",
        "DELETE FROM agents WHERE created_by = $1",
        "DELETE FROM users WHERE id = $1",
    ] {
        let _ = sqlx::query(sql).bind(c.owner).execute(&c.pg).await;
    }
}

async fn mk_user(pg: &PgPool, name: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, display_name, email, role) VALUES ($1, $2, $3, 'user')")
        .bind(id)
        .bind(name)
        .bind(format!("{id}@example.test"))
        .execute(pg)
        .await
        .unwrap();
    id
}

async fn mk_agent(pg: &PgPool, owner: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO agents (id, name, description, system_prompt, created_by, modes) \
         VALUES ($1, 'Reception', '', 'Answer the telephone.', $2, ARRAY['general'])",
    )
    .bind(id)
    .bind(owner)
    .execute(pg)
    .await
    .unwrap();
    id
}

async fn mk_line(pg: &PgPool, owner: Uuid, agent: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO phone_numbers (id, e164, provider, owner_user_id, agent_id, enabled) \
         VALUES ($1, $2, 'twilio', $3, $4, true)",
    )
    .bind(id)
    .bind(format!("+44131555{:05}", Uuid::now_v7().as_u128() % 100_000))
    .bind(owner)
    .bind(agent)
    .execute(pg)
    .await
    .unwrap();
    id
}

async fn mk_call(pg: &PgPool, owner: Uuid, agent: Uuid, line: Option<Uuid>, from: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO calls (id, phone_number_id, provider, provider_call_id, to_e164, from_e164, \
                            owner_user_id, agent_id) \
         VALUES ($1, $2, 'twilio', $3, '+441315550000', $4, $5, $6)",
    )
    .bind(id)
    .bind(line)
    .bind(format!("CA{}", id.simple()))
    .bind(from)
    .bind(owner)
    .bind(agent)
    .execute(pg)
    .await
    .unwrap();
    id
}

async fn mk_chat(pg: &PgPool, owner: Uuid, agent: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO chats (id, owner_user_id, agent_id, title, origin) \
         VALUES ($1, $2, $3, 'A call', 'phone')",
    )
    .bind(id)
    .bind(owner)
    .bind(agent)
    .execute(pg)
    .await
    .unwrap();
    id
}

/// Run one tool call the way a turn runs it: offer the tool, meet the gate, dispatch.
///
/// Returns whatever the model would have been shown, refusals included, because a refusal
/// is a string the model reads rather than an error the turn fails on.
async fn call_tool(
    c: &Cast,
    ctx: &AuthContext,
    name: &str,
    call_ctx: Option<&CallToolCtx>,
    args: serde_json::Value,
) -> String {
    // The offered set, filtered exactly as a turn filters it: a telephone tool is offered
    // only when there is a call, and membership of this set is what the gate consults.
    let offered: Vec<String> = phone::ALL
        .iter()
        .filter(|t| tools::host_enabled(t, &c.state.boot.features))
        .filter(|t| !phone::is_phone_tool(t) || call_ctx.is_some())
        // And putting a caller through needs somewhere to put them.
        .filter(|t| {
            **t != phone::TRANSFER_CALL
                || call_ctx.map(|c| c.transfer_e164.is_some()).unwrap_or(false)
        })
        // And checking a caller needs something to check against.
        .filter(|t| {
            **t != phone::SCREEN_CONFLICT
                || call_ctx.map(|c| c.screening_required).unwrap_or(false)
        })
        .map(|t| t.to_string())
        .collect();
    let authorised = AuthorisedTools::build(&offered, &offered, false, &HashMap::new());
    let overrides = HashMap::new();
    let decision =
        tools::authorize_native_call(&c.state, ctx, c.chat, &authorised, &overrides, name, None)
            .await;
    let witness = match decision {
        NativeDecision::Allowed(w) => w,
        NativeDecision::Recoverable(msg) => return msg,
        NativeDecision::Denied(e) => return format!("error: {e}"),
    };
    let (tx, _rx) = tokio::sync::mpsc::channel(64);
    tools::dispatch(
        &c.state,
        ctx,
        c.chat,
        Uuid::now_v7(),
        &tx,
        None,
        None,
        None,
        call_ctx,
        &[],
        &HashMap::new(),
        &witness,
        &args,
    )
    .await
    .unwrap_or_else(|e| format!("error: {e}"))
}

fn message(subject: &str) -> serde_json::Value {
    json!({
        "subject": subject,
        "message": "They want the survey re-sent to the new address.",
        "caller_name": "Alex Fraser",
        "contact": "07700 900123, mornings",
        "for_whom": "the practice manager",
        "urgency": "urgent",
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_message_is_written_down_against_the_call_it_was_taken_on() {
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let outcome = check_recording(&c).await;
    clear_up(&c).await;
    outcome.expect("the message was not written down as it should have been");
}

async fn check_recording(c: &Cast) -> Result<(), String> {
    let ctx = ctx_for(c.owner, PlatformRole::User);
    let call_ctx = phone::load_ctx(&c.pg, Some(c.call), Some(c.owner))
        .await
        .ok_or("the call this turn is on did not resolve")?;
    if call_ctx.from_e164 != "+447700900123" || call_ctx.phone_number_id != Some(c.line) {
        return Err("the call resolved to the wrong caller or line".into());
    }

    let said = call_tool(c, &ctx, "take_message", Some(&call_ctx), message("A survey")).await;
    if said.starts_with("error:") {
        return Err(format!("refused a message it should have taken: {said}"));
    }
    // Something the caller can quote, and the name of whoever it went to, so the agent can
    // say both aloud without inventing either.
    if !said.contains("Line owner") || said.len() < 20 {
        return Err(format!("said something unusable: {said}"));
    }

    let row = sqlx::query!(
        r#"SELECT kind, urgency, subject, body, caller_e164, caller_name, contact, for_whom,
                  call_id, chat_id, phone_number_id, owner_user_id, agent_id, details
             FROM enquiries WHERE call_id = $1"#,
        c.call
    )
    .fetch_all(&c.pg)
    .await
    .map_err(|e| format!("could not read what was written: {e}"))?;
    let [r] = &row[..] else {
        return Err(format!("expected one record, found {}", row.len()));
    };
    if r.kind != "message" || r.urgency != "urgent" {
        return Err(format!("recorded as {} / {}", r.kind, r.urgency));
    }
    if r.chat_id != Some(c.chat) || r.phone_number_id != Some(c.line) {
        return Err("the record does not point at the conversation and the line".into());
    }
    if r.owner_user_id != c.owner || r.agent_id != Some(c.agent) {
        return Err("the record was filed against the wrong account or agent".into());
    }
    // Copied from the call rather than from what the model said, so a caller cannot put
    // somebody else's number in the column that says who rang.
    if r.caller_e164 != "+447700900123" {
        return Err(format!("the caller's number came out as {:?}", r.caller_e164));
    }
    if r.subject != "A survey" || !r.body.contains("survey") {
        return Err("what the caller wanted did not survive".into());
    }
    if r.caller_name.as_deref() != Some("Alex Fraser") || r.for_whom.is_none() || r.contact.is_none()
    {
        return Err("what the caller gave did not survive".into());
    }

    // An enquiry keeps only the agreed intake keys, whatever the model offered.
    let said = call_tool(
        c,
        &ctx,
        "capture_lead",
        Some(&call_ctx),
        json!({
            "subject": "New enquiry",
            "summary": "Wants a quote for two rooms.",
            "details": { "organisation": "Acme", "note": "anything at all" },
        }),
    )
    .await;
    if said.starts_with("error:") {
        return Err(format!("refused an enquiry it should have taken: {said}"));
    }
    let lead = sqlx::query!(
        r#"SELECT kind, urgency, details FROM enquiries WHERE call_id = $1 AND kind = 'lead'"#,
        c.call
    )
    .fetch_one(&c.pg)
    .await
    .map_err(|e| format!("the enquiry was not written: {e}"))?;
    if lead.urgency != "routine" {
        return Err("an enquiry with no urgency given did not fall to routine".into());
    }
    if lead.details != json!({ "organisation": "Acme" }) {
        return Err(format!("kept details it should not have: {}", lead.details));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nothing_is_written_down_when_nobody_is_on_the_line() {
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let outcome = check_no_call(&c).await;
    clear_up(&c).await;
    outcome.expect("a record was taken with no caller to take it from");
}

async fn check_no_call(c: &Cast) -> Result<(), String> {
    let ctx = ctx_for(c.owner, PlatformRole::User);
    // The whole refusal, and it happens before dispatch: a turn with no call does not
    // offer the tool, so the gate refuses a name that was never on offer. The model is
    // told, recoverably, and nothing is written.
    let said = call_tool(c, &ctx, "take_message", None, message("Nobody rang")).await;
    if !said.starts_with("error:") {
        return Err(format!("a turn with no call was allowed to record: {said}"));
    }
    if !said.contains("not available to this agent") {
        return Err(format!("refused for the wrong reason: {said}"));
    }

    // And a call belonging to somebody else resolves to nothing at all, rather than to a
    // record filed against them.
    let stranger = mk_user(&c.pg, "Somebody else").await;
    let theirs = phone::load_ctx(&c.pg, Some(c.call), Some(stranger)).await;
    let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(stranger).execute(&c.pg).await;
    if theirs.is_some() {
        return Err("another account's call resolved as though it were this one's".into());
    }

    // A call that has ended is over: there is nobody left to be taking a message from.
    let ended = mk_call(&c.pg, c.owner, c.agent, Some(c.line), "+447700900999").await;
    sqlx::query("UPDATE calls SET ended_at = now() WHERE id = $1")
        .bind(ended)
        .execute(&c.pg)
        .await
        .unwrap();
    if phone::load_ctx(&c.pg, Some(ended), Some(c.owner)).await.is_some() {
        return Err("a call that had ended still looked like one in progress".into());
    }

    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM enquiries WHERE owner_user_id = $1")
        .bind(c.owner)
        .fetch_one(&c.pg)
        .await
        .expect("count what was written");
    if n != 0 {
        return Err(format!("{n} records were written by a turn with no call"));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_line_given_up_mid_call_still_takes_the_message() {
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let outcome = check_released_line(&c).await;
    clear_up(&c).await;
    outcome.expect("releasing a line lost a caller mid-sentence");
}

async fn check_released_line(c: &Cast) -> Result<(), String> {
    let ctx = ctx_for(c.owner, PlatformRole::User);
    // Resolved while the line is still there, as a turn resolves it once at the start.
    let call_ctx = phone::load_ctx(&c.pg, Some(c.call), Some(c.owner))
        .await
        .ok_or("the call did not resolve")?;
    // And then somebody gives the number up, which is how a line under abuse is stopped,
    // while this caller is still talking.
    sqlx::query("DELETE FROM phone_numbers WHERE id = $1")
        .bind(c.line)
        .execute(&c.pg)
        .await
        .map_err(|e| format!("could not release the line: {e}"))?;

    let said = call_tool(c, &ctx, "take_message", Some(&call_ctx), message("Still speaking")).await;
    if said.starts_with("error:") {
        return Err(format!("lost the caller's message when the line went: {said}"));
    }
    let r = sqlx::query!(
        "SELECT phone_number_id, caller_e164, subject FROM enquiries WHERE call_id = $1",
        c.call
    )
    .fetch_one(&c.pg)
    .await
    .map_err(|e| format!("nothing was written: {e}"))?;
    // The line is gone and the record stands: which number was rung is denormalised on the
    // call, and who rang is on the record itself.
    if r.phone_number_id.is_some() {
        return Err("the record still names a line that no longer exists".into());
    }
    if r.caller_e164 != "+447700900123" || r.subject != "Still speaking" {
        return Err("the record survived but lost what it was about".into());
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_caller_cannot_fill_somebody_s_morning() {
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let outcome = check_ceiling(&c).await;
    clear_up(&c).await;
    outcome.expect("one call was allowed to leave more records than the line accepts");
}

async fn check_ceiling(c: &Cast) -> Result<(), String> {
    let ctx = ctx_for(c.owner, PlatformRole::User);
    let call_ctx = phone::load_ctx(&c.pg, Some(c.call), Some(c.owner))
        .await
        .ok_or("the call did not resolve")?;
    let limit = phone::PER_CALL_LIMIT;
    for i in 0..limit {
        let said =
            call_tool(c, &ctx, "take_message", Some(&call_ctx), message(&format!("Thing {i}")))
                .await;
        if said.starts_with("error:") {
            return Err(format!("refused record {i} of {limit}: {said}"));
        }
    }
    let said = call_tool(c, &ctx, "take_message", Some(&call_ctx), message("One more")).await;
    if !said.starts_with("error:") {
        return Err("the ceiling did not hold".into());
    }
    // And it says what to do next rather than only what went wrong, because the thing
    // reading it is about to say something to a caller.
    if !said.contains("finish the call") {
        return Err(format!("the ceiling gave no way out: {said}"));
    }
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM enquiries WHERE call_id = $1")
        .bind(c.call)
        .fetch_one(&c.pg)
        .await
        .expect("count what was written");
    if n != limit {
        return Err(format!("{n} records were kept, not {limit}"));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn who_may_read_what_a_caller_said() {
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let outcome = check_redaction(&c).await;
    clear_up(&c).await;
    outcome.expect("the wrong person could read a caller's words");
}

async fn check_redaction(c: &Cast) -> Result<(), String> {
    let owner = ctx_for(c.owner, PlatformRole::User);
    let call_ctx = phone::load_ctx(&c.pg, Some(c.call), Some(c.owner))
        .await
        .ok_or("the call did not resolve")?;
    call_tool(c, &owner, "take_message", Some(&call_ctx), message("A survey")).await;

    let only_mine = |_: ()| EnquiryQuery { open: None, number_id: None, before: None, limit: None };
    // The person whose line it is: all of it.
    let mine = enquiries::list_mine(State(c.state.clone()), AuthUser(owner.clone()), Query(only_mine(())))
        .await
        .map_err(|e| format!("the owner could not read their own: {e}"))?
        .0;
    let mine: Vec<_> = mine.into_iter().filter(|e| e.call_id == Some(c.call)).collect();
    let [m] = &mine[..] else {
        return Err(format!("the owner saw {} of their own records", mine.len()));
    };
    if m.body.is_none() || m.subject.is_none() || m.caller_name.is_none() {
        return Err("the owner was refused their own message".into());
    }

    // Somebody who may wire a line, and is neither the owner nor an administrator of the
    // platform: that a message exists, and not a word of it. The same refusal as the
    // transcript nobody can read through this door either.
    let delegate = ctx_for(mk_user(&c.pg, "Delegated").await, PlatformRole::User);
    let seen = enquiries::list_all(State(c.state.clone()), AuthUser(delegate.clone()), Query(only_mine(())))
        .await
        .map_err(|e| format!("the delegate could not list at all: {e}"))?
        .0;
    let _ = sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(delegate.user_id.unwrap())
        .execute(&c.pg)
        .await;
    let seen: Vec<_> = seen.into_iter().filter(|e| e.call_id == Some(c.call)).collect();
    let [d] = &seen[..] else {
        return Err(format!("the delegate saw {} records for this call", seen.len()));
    };
    for (field, value) in [
        ("subject", &d.subject),
        ("body", &d.body),
        ("caller name", &d.caller_name),
        ("caller number", &d.caller_e164),
        ("contact", &d.contact),
        ("who it is for", &d.for_whom),
    ] {
        if value.is_some() {
            return Err(format!("the delegate was shown the {field}"));
        }
    }
    if d.details.is_some() {
        return Err("the delegate was shown the intake details".into());
    }
    // What they may see: that it happened, how urgent, on which line, dealt with or not.
    if d.urgency != "urgent" || d.kind != "message" || d.handled || d.to_e164.is_empty() {
        return Err("the delegate could not see that anything had happened at all".into());
    }

    // A platform administrator: all of it, because they can read the conversation anyway.
    let admin = ctx_for(mk_user(&c.pg, "Platform admin").await, PlatformRole::ClientAdmin);
    let all = enquiries::list_all(State(c.state.clone()), AuthUser(admin.clone()), Query(only_mine(())))
        .await
        .map_err(|e| format!("an administrator could not list: {e}"))?
        .0;
    let _ = sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(admin.user_id.unwrap())
        .execute(&c.pg)
        .await;
    let all: Vec<_> = all.into_iter().filter(|e| e.call_id == Some(c.call)).collect();
    let [a] = &all[..] else {
        return Err(format!("an administrator saw {} records", all.len()));
    };
    if a.body.is_none() {
        return Err("an administrator was refused a message".into());
    }

    // The rule, asserted directly as well, so the sentence in the comment has a test
    // about the sentence rather than about a fixture.
    if !enquiries::may_read_body(&owner, c.owner)
        || !enquiries::may_read_body(&admin, c.owner)
        || enquiries::may_read_body(&delegate, c.owner)
    {
        return Err("the rule and the query disagree about who may read a message".into());
    }

    // Marking it dealt with is the owner's, not the wirer's.
    enquiries::set_handled(
        State(c.state.clone()),
        AuthUser(delegate.clone()),
        axum::extract::Path(m.id),
        axum::Json(enquiries::Handled { handled: true }),
    )
    .await
    .err()
    .ok_or("a delegate marked somebody else's message dealt with")?;
    enquiries::set_handled(
        State(c.state.clone()),
        AuthUser(owner.clone()),
        axum::extract::Path(m.id),
        axum::Json(enquiries::Handled { handled: true }),
    )
    .await
    .map_err(|e| format!("the owner could not mark their own message dealt with: {e}"))?;
    let handled: bool =
        sqlx::query_scalar("SELECT handled_at IS NOT NULL FROM enquiries WHERE id = $1")
            .bind(m.id)
            .fetch_one(&c.pg)
            .await
            .expect("read the record back");
    if !handled {
        return Err("marking a message dealt with did not take".into());
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_line_announces_what_it_took_without_saying_what_was_said() {
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let outcome = check_announcement(&c).await;
    clear_up(&c).await;
    outcome.expect("the announcement went wrong");
}

async fn check_announcement(c: &Cast) -> Result<(), String> {
    let ctx = ctx_for(c.owner, PlatformRole::User);
    // A team chat the owner belongs to, which is the only kind a line may announce into.
    let team = Uuid::now_v7();
    sqlx::query("INSERT INTO group_chats (id, kind, name, created_by) VALUES ($1, 'group', 'Front desk', $2)")
        .bind(team)
        .bind(c.owner)
        .execute(&c.pg)
        .await
        .map_err(|e| format!("could not make a team chat: {e}"))?;
    sqlx::query("INSERT INTO group_chat_members (group_chat_id, user_id) VALUES ($1, $2)")
        .bind(team)
        .bind(c.owner)
        .execute(&c.pg)
        .await
        .map_err(|e| format!("could not put the owner in it: {e}"))?;
    sqlx::query("UPDATE phone_numbers SET deliver_group_chat_id = $2 WHERE id = $1")
        .bind(c.line)
        .bind(team)
        .execute(&c.pg)
        .await
        .map_err(|e| format!("could not point the line at it: {e}"))?;

    let call_ctx = phone::load_ctx(&c.pg, Some(c.call), Some(c.owner))
        .await
        .ok_or("the call did not resolve")?;
    if call_ctx.deliver_group_chat_id != Some(team) {
        return Err("the call did not carry where to announce".into());
    }
    call_tool(c, &ctx, "take_message", Some(&call_ctx), message("A survey")).await;

    // Announced off the caller's clock, so it lands shortly after rather than during.
    let mut posted = None;
    for _ in 0..80 {
        posted = sqlx::query_scalar::<_, String>(
            "SELECT content FROM group_chat_messages WHERE group_chat_id = $1 \
             ORDER BY sequence_number DESC LIMIT 1",
        )
        .bind(team)
        .fetch_optional(&c.pg)
        .await
        .expect("read the team chat");
        if posted.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let posted = posted.ok_or("nothing was announced")?;
    // Who rang, how urgent, and the one line naming what it is about. Never the message
    // itself: the people in a team chat are not necessarily the people entitled to read
    // what a caller dictated, and whoever is can read it where it is kept.
    if !posted.contains("+447700900123") || !posted.contains("A survey") || !posted.contains("urgent")
    {
        return Err(format!("the announcement said too little: {posted}"));
    }
    if posted.contains("survey re-sent") || posted.contains("Alex Fraser") {
        return Err(format!("the announcement said too much: {posted}"));
    }

    // And when the owner has since left that chat, nothing is said and nothing fails: the
    // record is the durable thing, and announcing it is not.
    sqlx::query("DELETE FROM group_chat_members WHERE group_chat_id = $1 AND user_id = $2")
        .bind(team)
        .bind(c.owner)
        .execute(&c.pg)
        .await
        .unwrap();
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM group_chat_messages WHERE group_chat_id = $1")
        .bind(team)
        .fetch_one(&c.pg)
        .await
        .unwrap();
    let said = call_tool(c, &ctx, "take_message", Some(&call_ctx), message("Another")).await;
    if said.starts_with("error:") {
        return Err(format!("leaving the chat lost the message too: {said}"));
    }
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM group_chat_messages WHERE group_chat_id = $1")
        .bind(team)
        .fetch_one(&c.pg)
        .await
        .unwrap();
    if after != before {
        return Err("a line went on announcing into a chat its owner had left".into());
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn erasing_the_owner_takes_what_callers_told_them_with_it() {
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let outcome = check_erasure(&c).await;
    clear_up(&c).await;
    outcome.expect("erasure and the messages a line took cannot both be true");
}

async fn check_erasure(c: &Cast) -> Result<(), String> {
    let ctx = ctx_for(c.owner, PlatformRole::User);
    let call_ctx = phone::load_ctx(&c.pg, Some(c.call), Some(c.owner))
        .await
        .ok_or("the call did not resolve")?;
    call_tool(c, &ctx, "take_message", Some(&call_ctx), message("A survey")).await;

    // The same sequence the erasure routine performs, in the same order. What is being
    // proved is that the order is possible at all: the records go before the calls they
    // were taken on, and the lines before the agents that answered them, or a delete
    // trips a foreign key and the whole erasure fails.
    let mut tx = c.pg.begin().await.unwrap();
    for sql in [
        "DELETE FROM enquiries WHERE owner_user_id = $1",
        "DELETE FROM calls WHERE owner_user_id = $1",
        "DELETE FROM phone_numbers WHERE owner_user_id = $1 \
           OR agent_id IN (SELECT id FROM agents WHERE created_by = $1)",
        "DELETE FROM chats WHERE owner_user_id = $1",
        "DELETE FROM agents WHERE created_by = $1",
    ] {
        sqlx::query(sql)
            .bind(c.owner)
            .execute(&mut *tx)
            .await
            .map_err(|e| format!("erasure failed at {sql:?}: {e}"))?;
    }
    tx.commit().await.map_err(|e| format!("erasure could not be committed: {e}"))?;

    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM enquiries WHERE owner_user_id = $1")
        .bind(c.owner)
        .fetch_one(&c.pg)
        .await
        .expect("count what is left");
    if left != 0 {
        return Err(format!("{left} records survived the erasure of the person they belonged to"));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_line_with_nowhere_to_put_callers_does_not_offer_to() {
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let outcome = check_transfer_offer(&c).await;
    clear_up(&c).await;
    outcome.expect("the wrong thing was offered to a caller");
}

async fn check_transfer_offer(c: &Cast) -> Result<(), String> {
    let ctx = ctx_for(c.owner, PlatformRole::User);
    // The line this run made has no number to put anybody through to, which is the
    // ordinary case: most lines take messages and nothing else.
    let call_ctx = phone::load_ctx(&c.pg, Some(c.call), Some(c.owner))
        .await
        .ok_or("the call did not resolve")?;
    if call_ctx.transfer_e164.is_some() {
        return Err("a line was given somewhere to transfer to that nobody set".into());
    }
    // Refused at the gate rather than at dispatch, because a turn that cannot transfer
    // never advertises it: an agent that has told somebody it is connecting them has
    // already said the wrong thing by the time anything could refuse.
    let said = call_tool(c, &ctx, "transfer_call", Some(&call_ctx), json!({
        "subject": "Wants a person",
        "summary": "Asked to speak to somebody.",
    }))
    .await;
    if !said.starts_with("error:") {
        return Err(format!("a line with nowhere to transfer offered to anyway: {said}"));
    }

    // Give the line somewhere, and it becomes possible.
    sqlx::query("UPDATE phone_numbers SET transfer_e164 = '+441315557788' WHERE id = $1")
        .bind(c.line)
        .execute(&c.pg)
        .await
        .map_err(|e| format!("could not give the line a transfer number: {e}"))?;
    let call_ctx = phone::load_ctx(&c.pg, Some(c.call), Some(c.owner))
        .await
        .ok_or("the call did not resolve")?;
    let said = call_tool(c, &ctx, "transfer_call", Some(&call_ctx), json!({
        "subject": "Wants a person",
        "summary": "Asked to speak to somebody.",
    }))
    .await;
    if said.starts_with("error:") {
        return Err(format!("a line that can transfer refused to: {said}"));
    }
    // The intent is written down before anything is closed, because what happens next
    // arrives as a fresh request that knows only which call it is about.
    let to: Option<String> =
        sqlx::query_scalar("SELECT transfer_to FROM calls WHERE id = $1")
            .bind(c.call)
            .fetch_one(&c.pg)
            .await
            .expect("read the call back");
    if to.as_deref() != Some("+441315557788") {
        return Err(format!("the call was to be put through to {to:?}"));
    }
    // And the person about to pick up gets what the caller already explained.
    let kinds: Vec<String> = sqlx::query_scalar("SELECT kind FROM enquiries WHERE call_id = $1")
        .bind(c.call)
        .fetch_all(&c.pg)
        .await
        .expect("read what was written");
    if kinds != vec!["handover".to_string()] {
        return Err(format!("a transfer wrote {kinds:?}"));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_full_morning_does_not_stop_somebody_being_put_through() {
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let outcome = check_ceiling_exemption(&c).await;
    clear_up(&c).await;
    outcome.expect("the ceiling stopped the wrong thing");
}

async fn check_ceiling_exemption(c: &Cast) -> Result<(), String> {
    let ctx = ctx_for(c.owner, PlatformRole::User);
    sqlx::query("UPDATE phone_numbers SET transfer_e164 = '+441315557788' WHERE id = $1")
        .bind(c.line)
        .execute(&c.pg)
        .await
        .unwrap();
    let call_ctx = phone::load_ctx(&c.pg, Some(c.call), Some(c.owner))
        .await
        .ok_or("the call did not resolve")?;
    for i in 0..phone::PER_CALL_LIMIT {
        call_tool(c, &ctx, "take_message", Some(&call_ctx), message(&format!("Thing {i}"))).await;
    }
    // The ceiling holds for what it is for.
    let said = call_tool(c, &ctx, "take_message", Some(&call_ctx), message("One more")).await;
    if !said.starts_with("error:") {
        return Err("the ceiling did not hold for messages".into());
    }
    // And does not hold for handing the caller to a person, which happens once and ends
    // the call: refusing it because somebody had already left five messages would be the
    // wrong way round.
    let said = call_tool(c, &ctx, "transfer_call", Some(&call_ctx), json!({
        "subject": "Wants a person after all",
        "summary": "Five messages later, they would rather speak to somebody.",
    }))
    .await;
    if said.starts_with("error:") {
        return Err(format!("a full morning stopped a caller being put through: {said}"));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn finishing_a_call_writes_nothing_down() {
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let outcome = check_end_call(&c).await;
    clear_up(&c).await;
    outcome.expect("finishing a call went wrong");
}

async fn check_end_call(c: &Cast) -> Result<(), String> {
    let ctx = ctx_for(c.owner, PlatformRole::User);
    let call_ctx = phone::load_ctx(&c.pg, Some(c.call), Some(c.owner))
        .await
        .ok_or("the call did not resolve")?;
    let said = call_tool(c, &ctx, "end_call", Some(&call_ctx), json!({})).await;
    if said.starts_with("error:") {
        return Err(format!("a call could not be finished: {said}"));
    }
    // It tells the agent to say goodbye rather than going silent, because the call ends
    // once the caller has heard it and not before.
    if !said.contains("goodbye") {
        return Err(format!("finishing a call said nothing useful: {said}"));
    }
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM enquiries WHERE call_id = $1")
        .bind(c.call)
        .fetch_one(&c.pg)
        .await
        .expect("count what was written");
    if n != 0 {
        return Err(format!("finishing a call left {n} records behind"));
    }
    // Nothing was asked to be put through, either: a call that is simply over must not
    // be handed to anybody.
    let to: Option<String> = sqlx::query_scalar("SELECT transfer_to FROM calls WHERE id = $1")
        .bind(c.call)
        .fetch_one(&c.pg)
        .await
        .expect("read the call back");
    if to.is_some() {
        return Err(format!("finishing a call put somebody through to {to:?}"));
    }
    Ok(())
}

/// A name distinctive enough that finding it in any output means it leaked.
const ON_THE_LIST: &str = "Marchetti Quarry Holdings";

async fn add_to_list(c: &Cast, name: &str) {
    sqlx::query(
        "INSERT INTO conflict_names (id, owner_user_id, name, normalised) \
         VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
    )
    .bind(Uuid::now_v7())
    .bind(c.owner)
    .bind(name)
    .bind(fosnie_backend::telephony::conflict::normalise(name))
    .execute(&c.pg)
    .await
    .expect("add a name to the list");
}

async fn verdict_on(c: &Cast) -> Option<String> {
    sqlx::query_scalar("SELECT conflict_check FROM calls WHERE id = $1")
        .bind(c.call)
        .fetch_one(&c.pg)
        .await
        .expect("read the call back")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_caller_is_checked_against_the_practices_own_list() {
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let outcome = check_screening(&c).await;
    clear_up(&c).await;
    outcome.expect("the check went wrong");
}

async fn check_screening(c: &Cast) -> Result<(), String> {
    let ctx = ctx_for(c.owner, PlatformRole::User);

    // An account with no list is not offered the check at all: being told "nothing on
    // record" by a check that looked at nothing reads as clearance.
    let call_ctx = phone::load_ctx(&c.pg, Some(c.call), Some(c.owner))
        .await
        .ok_or("the call did not resolve")?;
    if call_ctx.screening_required {
        return Err("an account with no list was told it screens callers".into());
    }
    let said = call_tool(c, &ctx, "screen_conflict", Some(&call_ctx), json!({ "name": "Jane Fraser" })).await;
    if !said.starts_with("error:") {
        return Err(format!("a check was made against nothing: {said}"));
    }

    // Now the practice keeps a list.
    add_to_list(c, ON_THE_LIST).await;
    add_to_list(c, "Jane Alice Fraser").await;
    let call_ctx = phone::load_ctx(&c.pg, Some(c.call), Some(c.owner))
        .await
        .ok_or("the call did not resolve")?;
    if !call_ctx.screening_required {
        return Err("an account with a list was not told it screens callers".into());
    }

    // Somebody on it, given without the middle name they are recorded under.
    let said = call_tool(c, &ctx, "screen_conflict", Some(&call_ctx), json!({ "name": "Jane Fraser" })).await;
    if verdict_on(c).await.as_deref() != Some("possible") {
        return Err(format!("a caller on the list was recorded as {:?}", verdict_on(c).await));
    }
    // What the agent is told is a decision and an instruction, and nothing it could repeat
    // to the caller as evidence that anything was looked at.
    if !said.contains("do not put the caller through") || !said.contains("Do not tell the caller why") {
        return Err(format!("the check gave no usable instruction: {said}"));
    }

    // Somebody not on it, with enough of a name to have checked.
    let said_clear = call_tool(c, &ctx, "screen_conflict", Some(&call_ctx), json!({ "name": "Peter Bell" })).await;
    if verdict_on(c).await.as_deref() != Some("clear") {
        return Err(format!("an unrelated caller was recorded as {:?}", verdict_on(c).await));
    }
    if said_clear.starts_with("error:") {
        return Err(format!("an unrelated caller was refused: {said_clear}"));
    }

    // A surname on its own is not a check: it identifies nobody, and treating it as clear
    // would be passing somebody on no evidence at all.
    call_tool(c, &ctx, "screen_conflict", Some(&call_ctx), json!({ "name": "Fraser" })).await;
    if verdict_on(c).await.as_deref() != Some("unknown") {
        return Err(format!("one word counted as a check: {:?}", verdict_on(c).await));
    }

    // The organisation is checked in its own right.
    call_tool(
        c,
        &ctx,
        "screen_conflict",
        Some(&call_ctx),
        json!({ "name": "Peter Bell", "organisation": "marchetti quarry holdings ltd" }),
    )
    .await;
    if verdict_on(c).await.as_deref() != Some("possible") {
        return Err(format!("a known organisation was recorded as {:?}", verdict_on(c).await));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_list_is_never_read_out_to_whoever_is_asking() {
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let outcome = check_no_leak(&c).await;
    clear_up(&c).await;
    outcome.expect("the list leaked");
}

/// The security assertion of the check.
///
/// The thing calling this tool is in the middle of a conversation with an anonymous
/// stranger, and the list is the most confidential holding a practice has. So no output of
/// the check may carry a name from it, however the caller phrases the question: not the
/// matched name, not a near miss, not a count, not a hint that a list exists at all.
async fn check_no_leak(c: &Cast) -> Result<(), String> {
    let ctx = ctx_for(c.owner, PlatformRole::User);
    add_to_list(c, ON_THE_LIST).await;
    let call_ctx = phone::load_ctx(&c.pg, Some(c.call), Some(c.owner))
        .await
        .ok_or("the call did not resolve")?;

    // Ask in the way that would extract it if anything could: with the exact name.
    let outputs = vec![
        call_tool(c, &ctx, "screen_conflict", Some(&call_ctx), json!({ "name": ON_THE_LIST })).await,
        call_tool(
            c,
            &ctx,
            "screen_conflict",
            Some(&call_ctx),
            json!({ "name": "Peter Bell", "organisation": ON_THE_LIST }),
        )
        .await,
        call_tool(c, &ctx, "screen_conflict", Some(&call_ctx), json!({ "name": "Marchetti Quarry" })).await,
        call_tool(c, &ctx, "screen_conflict", Some(&call_ctx), json!({ "name": "Somebody Else" })).await,
    ];
    for said in &outputs {
        let lower = said.to_lowercase();
        for word in ["marchetti", "quarry", "holdings"] {
            if lower.contains(word) {
                return Err(format!("the check said {word:?} back: {said}"));
            }
        }
    }

    // And what is refused to the agent is refused to the caller through it: a transfer on a
    // matched call says nothing about why.
    sqlx::query("UPDATE phone_numbers SET transfer_e164 = '+441315557788' WHERE id = $1")
        .bind(c.line)
        .execute(&c.pg)
        .await
        .unwrap();
    let call_ctx = phone::load_ctx(&c.pg, Some(c.call), Some(c.owner))
        .await
        .ok_or("the call did not resolve")?;
    call_tool(c, &ctx, "screen_conflict", Some(&call_ctx), json!({ "name": ON_THE_LIST })).await;
    let refused = call_tool(
        c,
        &ctx,
        "transfer_call",
        Some(&call_ctx),
        json!({ "subject": "Wants a person", "summary": "Asked to be put through." }),
    )
    .await;
    if !refused.starts_with("error:") {
        return Err(format!("a matched caller was put through: {refused}"));
    }
    if refused.to_lowercase().contains("marchetti") || refused.to_lowercase().contains("list") {
        return Err(format!("the refusal explained itself: {refused}"));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nobody_is_put_through_until_they_have_been_checked() {
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let outcome = check_the_gate(&c).await;
    clear_up(&c).await;
    outcome.expect("the gate did not hold");
}

async fn check_the_gate(c: &Cast) -> Result<(), String> {
    let ctx = ctx_for(c.owner, PlatformRole::User);
    sqlx::query("UPDATE phone_numbers SET transfer_e164 = '+441315557788' WHERE id = $1")
        .bind(c.line)
        .execute(&c.pg)
        .await
        .unwrap();
    let transfer = json!({ "subject": "Wants a person", "summary": "Asked to be put through." });

    // A practice keeping no list is untouched by any of this: the line transfers as it did
    // before the check existed.
    let call_ctx = phone::load_ctx(&c.pg, Some(c.call), Some(c.owner))
        .await
        .ok_or("the call did not resolve")?;
    let said = call_tool(c, &ctx, "transfer_call", Some(&call_ctx), transfer.clone()).await;
    if said.starts_with("error:") {
        return Err(format!("a line with no list refused to transfer: {said}"));
    }

    // A fresh call, and now the practice keeps a list.
    let call = mk_call(&c.pg, c.owner, c.agent, Some(c.line), "+447700900555").await;
    add_to_list(c, ON_THE_LIST).await;
    let fresh = phone::load_ctx(&c.pg, Some(call), Some(c.owner))
        .await
        .ok_or("the second call did not resolve")?;

    // Never checked. Refused, and told what to do instead: forgetting the check must fail
    // safe rather than fail open, because a model that forgets is the ordinary case.
    let said = call_tool(c, &ctx, "transfer_call", Some(&fresh), transfer.clone()).await;
    if !said.starts_with("error:") {
        return Err(format!("an unchecked caller was put through: {said}"));
    }
    if !said.contains("screen_conflict") {
        return Err(format!("the refusal did not say how to proceed: {said}"));
    }

    // Checked, and not clear. Still refused.
    call_tool(c, &ctx, "screen_conflict", Some(&fresh), json!({ "name": "Fraser" })).await;
    let said = call_tool(c, &ctx, "transfer_call", Some(&fresh), transfer.clone()).await;
    if !said.starts_with("error:") {
        return Err(format!("a caller who could not be checked was put through: {said}"));
    }

    // Checked and clear. Allowed.
    call_tool(c, &ctx, "screen_conflict", Some(&fresh), json!({ "name": "Peter Bell" })).await;
    let said = call_tool(c, &ctx, "transfer_call", Some(&fresh), transfer).await;
    if said.starts_with("error:") {
        return Err(format!("a checked, clear caller was refused: {said}"));
    }
    let to: Option<String> = sqlx::query_scalar("SELECT transfer_to FROM calls WHERE id = $1")
        .bind(call)
        .fetch_one(&c.pg)
        .await
        .expect("read the call back");
    if to.as_deref() != Some("+441315557788") {
        return Err(format!("the transfer was not recorded: {to:?}"));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn who_may_read_the_list() {
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let outcome = check_list_access(&c).await;
    clear_up(&c).await;
    outcome.expect("the wrong person could read the list");
}

async fn check_list_access(c: &Cast) -> Result<(), String> {
    use fosnie_backend::http::conflict_names::{self, AddNames, WhoseList};

    let owner = ctx_for(c.owner, PlatformRole::User);
    // Pasted, the way a real list arrives, with a duplicate and some rubbish in it.
    let added = conflict_names::add(
        State(c.state.clone()),
        AuthUser(owner.clone()),
        axum::Json(AddNames {
            owner_user_id: None,
            names: format!("{ON_THE_LIST}\nJane Fraser\n\n  \n{ON_THE_LIST}\n!!!\n"),
            note: Some("Quarry matter".into()),
        }),
    )
    .await
    .map_err(|e| format!("the owner could not add to their own list: {e}"))?
    .0;
    if added["added"] != 2 || added["already_there"] != 1 {
        return Err(format!("a pasted list was not deduplicated: {added}"));
    }

    let whose = |id: Option<Uuid>| WhoseList { owner_user_id: id };
    let mine = conflict_names::list(
        State(c.state.clone()),
        AuthUser(owner.clone()),
        axum::extract::Query(whose(None)),
    )
    .await
    .map_err(|e| format!("the owner could not read their own list: {e}"))?
    .0;
    if mine.len() != 2 || !mine.iter().any(|n| n.name == ON_THE_LIST) {
        return Err(format!("the owner's own list came back as {} names", mine.len()));
    }

    // Somebody who may wire lines, and is neither the owner nor an administrator: refused,
    // and refused as though the list did not exist. Whose account keeps one is not their
    // business either.
    let delegate = ctx_for(mk_user(&c.pg, "Delegated").await, PlatformRole::User);
    let refused = conflict_names::list(
        State(c.state.clone()),
        AuthUser(delegate.clone()),
        axum::extract::Query(whose(Some(c.owner))),
    )
    .await;
    let write_refused = conflict_names::add(
        State(c.state.clone()),
        AuthUser(delegate.clone()),
        axum::Json(AddNames {
            owner_user_id: Some(c.owner),
            names: "Somebody Injected".into(),
            note: None,
        }),
    )
    .await;
    let _ = sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(delegate.user_id.unwrap())
        .execute(&c.pg)
        .await;
    if refused.is_ok() {
        return Err("a delegated administrator read somebody's client list".into());
    }
    if write_refused.is_ok() {
        return Err("a delegated administrator wrote to somebody's client list".into());
    }

    // A platform administrator may, because they can read everything anyway.
    let admin = ctx_for(mk_user(&c.pg, "Platform admin").await, PlatformRole::ClientAdmin);
    let seen = conflict_names::list(
        State(c.state.clone()),
        AuthUser(admin.clone()),
        axum::extract::Query(whose(Some(c.owner))),
    )
    .await
    .map_err(|e| format!("an administrator could not read the list: {e}"))?
    .0;
    let _ = sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(admin.user_id.unwrap())
        .execute(&c.pg)
        .await;
    if seen.len() != 2 {
        return Err(format!("an administrator saw {} names", seen.len()));
    }

    // Taking a name off is the owner's to do.
    let id = mine[0].id;
    conflict_names::remove(State(c.state.clone()), AuthUser(owner.clone()), axum::extract::Path(id))
        .await
        .map_err(|e| format!("the owner could not remove a name: {e}"))?;
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM conflict_names WHERE owner_user_id = $1")
        .bind(c.owner)
        .fetch_one(&c.pg)
        .await
        .unwrap();
    if left != 1 {
        return Err(format!("{left} names left after removing one of two"));
    }

    // And erasing the account takes the whole list with it, because it is a list of other
    // people's names held by this one.
    sqlx::query("DELETE FROM conflict_names WHERE owner_user_id = $1")
        .bind(c.owner)
        .execute(&c.pg)
        .await
        .map_err(|e| format!("erasure could not remove the list: {e}"))?;
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM conflict_names WHERE owner_user_id = $1")
        .bind(c.owner)
        .fetch_one(&c.pg)
        .await
        .unwrap();
    if left != 0 {
        return Err(format!("{left} names survived the erasure of the account holding them"));
    }
    Ok(())
}
