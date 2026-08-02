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

//! Which telephone calls are taken, and which are not.
//!
//! A line is a number bound to an agent and an owning account, so answering a call means
//! deciding that an anonymous member of the public may hold a session as a named person.
//! Every way that decision can go wrong is a way somebody reaches an account nobody meant
//! them to, so each is driven through the real answer endpoint with a real signature and
//! asserted on what the caller gets and on what the record says.
//!
//! The one property worth naming: a caller must not be able to tell a number that is
//! switched off from one that does not exist. The set of numbers a deployment answers is
//! its whole attack surface for somebody dialling at random, and "this one exists but is
//! off" tells them which numbers to try again later.
//!
//! Needs a reachable Postgres and Redis; skips when `DATABASE_URL` is unset.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use hmac::digest::KeyInit;
use hmac::{Hmac, Mac};
use sha1::Sha1;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use fosnie_backend::config::runtime::{self, ConfigValueType};
use fosnie_backend::config::BootConfig;
use fosnie_backend::state::AppStateBuilder;
use fosnie_backend::telephony::log::{self, CallEnd};
use fosnie_backend::{cache, db, http};

const AUTH_TOKEN: &str = "carrier-token-for-line-tests";
const ANSWER_PATH: &str = "/api/telephony/twilio/voice";

/// Deployment-wide rows this test writes, so whatever was there is put back.
const BORROWED: [(&str, ConfigValueType); 4] = [
    ("telephony.provider", ConfigValueType::String),
    ("telephony.public_base_url", ConfigValueType::String),
    ("telephony.max_concurrent_calls", ConfigValueType::Int),
    ("telephony.auth_token_enc", ConfigValueType::String),
];

struct Harness {
    base: String,
    pg: PgPool,
    owner: Uuid,
    agent: Uuid,
    was: Vec<(&'static str, ConfigValueType, Option<String>)>,
}

fn sign(url: &str, params: &[(&str, &str)]) -> String {
    let mut sorted: Vec<&(&str, &str)> = params.iter().collect();
    sorted.sort_by_key(|(k, _)| *k);
    let mut signed = url.to_string();
    for (k, v) in sorted {
        signed.push_str(k);
        signed.push_str(v);
    }
    let mut mac = Hmac::<Sha1>::new_from_slice(AUTH_TOKEN.as_bytes()).unwrap();
    mac.update(signed.as_bytes());
    B64.encode(mac.finalize().into_bytes())
}

async fn harness() -> Option<Harness> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:16379".into());
    let pg = db::connect(&db_url, 5).await.unwrap_or_else(|e| {
        panic!("DATABASE_URL is set but unreachable, so nothing here was tested: {e}")
    });
    let redis = cache::create_pool(&redis_url).expect("redis pool");
    let mut boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    boot.features.voice = true;
    boot.features.voice_live = true;
    boot.features.telephony = true;
    boot.message_encryption_key = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=".into();
    boot.server.static_dir = "___no_spa___".into();
    let state = AppStateBuilder::new(pg.clone(), redis, Arc::new(boot)).build();

    let mut was = Vec::new();
    for (key, kind) in BORROWED {
        was.push((key, kind, runtime::get(&pg, key).await.ok().flatten().map(|e| e.value)));
    }

    let owner = mk_user(&pg, false).await;
    let agent = mk_agent(&pg, owner, false).await;

    let app = http::router(state, None, None, None, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await;
    });
    let base = format!("http://127.0.0.1:{port}");

    set(&pg, "telephony.provider", "twilio", ConfigValueType::String).await;
    set(&pg, "telephony.public_base_url", &base, ConfigValueType::String).await;
    set(&pg, "telephony.max_concurrent_calls", "4", ConfigValueType::Int).await;
    let ct = fosnie_backend::crypto::encrypt_at_rest(AUTH_TOKEN).expect("encrypts");
    set(&pg, "telephony.auth_token_enc", &ct, ConfigValueType::String).await;

    Some(Harness { base, pg, owner, agent, was })
}

async fn set(pg: &PgPool, key: &str, value: &str, kind: ConfigValueType) {
    runtime::set(pg, key, value, kind, "global", None, "system").await.expect("write");
}

async fn restore(h: &Harness) {
    for (key, kind, value) in &h.was {
        match value {
            Some(v) => {
                let _ = runtime::set(&h.pg, key, v, *kind, "global", None, "system").await;
            }
            None => {
                let _ = runtime::unset(&h.pg, key, "system").await;
            }
        }
    }
    let _ = sqlx::query("DELETE FROM calls WHERE owner_user_id = $1").bind(h.owner).execute(&h.pg).await;
    let _ = sqlx::query("DELETE FROM phone_numbers WHERE owner_user_id = $1").bind(h.owner).execute(&h.pg).await;
    let _ = sqlx::query("DELETE FROM agents WHERE created_by = $1").bind(h.owner).execute(&h.pg).await;
    let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(h.owner).execute(&h.pg).await;
}

async fn mk_user(pg: &PgPool, deactivated: bool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, display_name, email, role, deactivated_at) \
         VALUES ($1, 'Line owner', $2, 'user', CASE WHEN $3 THEN now() ELSE NULL END)",
    )
    .bind(id)
    .bind(format!("{id}@example.test"))
    .bind(deactivated)
    .execute(pg)
    .await
    .unwrap();
    id
}

async fn mk_agent(pg: &PgPool, owner: Uuid, archived: bool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO agents (id, name, description, system_prompt, created_by, modes, archived_at) \
         VALUES ($1, 'Reception', '', 'Answer the telephone.', $2, ARRAY['general'], \
                 CASE WHEN $3 THEN now() ELSE NULL END)",
    )
    .bind(id)
    .bind(owner)
    .bind(archived)
    .execute(pg)
    .await
    .unwrap();
    id
}

/// A fresh number per line, so the abuse guards keyed on numbers never see a repeat.
fn fresh_number() -> String {
    format!("+44131555{:05}", Uuid::now_v7().as_u128() % 100_000)
}

async fn mk_line(pg: &PgPool, owner: Uuid, agent: Uuid, number: &str, enabled: bool) {
    sqlx::query(
        "INSERT INTO phone_numbers (id, e164, provider, owner_user_id, agent_id, enabled) \
         VALUES ($1, $2, 'twilio', $3, $4, $5)",
    )
    .bind(Uuid::now_v7())
    .bind(number)
    .bind(owner)
    .bind(agent)
    .bind(enabled)
    .execute(pg)
    .await
    .unwrap();
}

/// Ring a number with a properly signed request. Returns the status, the body, and the
/// number it called from, which is how the record of the outcome is found again.
async fn ring(h: &Harness, to: &str) -> (u16, String, String) {
    let call_sid = format!("CA{}", Uuid::now_v7().simple());
    let caller = format!("+44770090{:05}", Uuid::now_v7().as_u128() % 100_000);
    let params = [
        ("AccountSid", "ACtest"),
        ("CallSid", call_sid.as_str()),
        ("CallStatus", "ringing"),
        ("Direction", "inbound"),
        ("From", caller.as_str()),
        ("To", to),
    ];
    let url = format!("{}{ANSWER_PATH}", h.base);
    let resp = reqwest::Client::new()
        .post(&url)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("X-Twilio-Signature", sign(&url, &params))
        .body({
            let mut s = form_urlencoded::Serializer::new(String::new());
            for (k, v) in params {
                s.append_pair(k, v);
            }
            s.finish()
        })
        .send()
        .await
        .expect("the webhook answers");
    (resp.status().as_u16(), resp.text().await.unwrap_or_default(), caller)
}

/// The most recent reason a call was refused, as the record has it.
async fn refusal_for(pg: &PgPool, caller: &str) -> Option<String> {
    // Found by who was calling rather than by taking the most recent row: the record is
    // written off the request path, so it lands shortly after the answer rather than
    // during it, and more than one of these runs against the same database. Polled for
    // that reason, briefly.
    for _ in 0..40 {
        let found = sqlx::query_scalar::<_, Option<String>>(
            "SELECT payload->>'reason' FROM audit_events \
             WHERE action_type = 'telephony.refused' AND payload->>'from' = $1 \
             ORDER BY seq DESC LIMIT 1",
        )
        .bind(caller)
        .fetch_optional(pg)
        .await
        // Not swallowed. A query that cannot run would otherwise look exactly like a
        // refusal that was never recorded, which is a much more alarming result than the
        // typo that caused it.
        .expect("read the record of refused calls")
        .flatten();
        if found.is_some() {
            return found;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    None
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_is_taken_only_when_the_line_can_take_it() {
    let Some(h) = harness().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let outcome = check_resolution(&h).await;
    restore(&h).await;
    outcome.expect("a line resolved wrongly");
}

async fn check_resolution(h: &Harness) -> Result<(), String> {
    // ---- No line at all. ----
    let unknown_number = fresh_number();
    let (status, unknown_body, caller) = ring(h, &unknown_number).await;
    if status != 200 || !unknown_body.contains("<Reject/>") {
        return Err(format!("a call to an unregistered number: {status} {unknown_body}"));
    }
    let reason = refusal_for(&h.pg, &caller).await;
    if reason.as_deref() != Some("unknown_number") {
        return Err(format!("recorded as {reason:?}, not unknown_number"));
    }

    // ---- A line that exists but is switched off. ----
    let disabled_number = fresh_number();
    mk_line(&h.pg, h.owner, h.agent, &disabled_number, false).await;
    let (disabled_status, disabled_body, caller) = ring(h, &disabled_number).await;
    let reason = refusal_for(&h.pg, &caller).await;
    if reason.as_deref() != Some("line_disabled") {
        return Err(format!("recorded as {reason:?}, not line_disabled"));
    }

    // ---- And the property that matters: the caller cannot tell them apart. ----
    if (disabled_status, disabled_body.as_str()) != (status, unknown_body.as_str()) {
        return Err(format!(
            "a switched-off line is distinguishable from one that does not exist: \
             {disabled_status} {disabled_body:?} vs {status} {unknown_body:?}"
        ));
    }

    // ---- A line whose agent has been archived. ----
    let archived_agent = mk_agent(&h.pg, h.owner, true).await;
    let archived_number = fresh_number();
    mk_line(&h.pg, h.owner, archived_agent, &archived_number, true).await;
    let (s, b, caller) = ring(h, &archived_number).await;
    if s != 200 || !b.contains("<Reject/>") {
        return Err(format!("a line with an archived agent: {s} {b}"));
    }
    let reason = refusal_for(&h.pg, &caller).await;
    if reason.as_deref() != Some("agent_unavailable") {
        return Err(format!("recorded as {reason:?}, not agent_unavailable"));
    }

    // ---- A line whose owning account has been deactivated. ----
    let gone_owner = mk_user(&h.pg, true).await;
    let gone_agent = mk_agent(&h.pg, gone_owner, false).await;
    let gone_number = fresh_number();
    mk_line(&h.pg, gone_owner, gone_agent, &gone_number, true).await;
    let (s, b, caller) = ring(h, &gone_number).await;
    if s != 200 || !b.contains("<Reject/>") {
        return Err(format!("a line owned by a deactivated account: {s} {b}"));
    }
    let reason = refusal_for(&h.pg, &caller).await;
    if reason.as_deref() != Some("owner_unavailable") {
        return Err(format!("recorded as {reason:?}, not owner_unavailable"));
    }
    let _ = sqlx::query("DELETE FROM phone_numbers WHERE owner_user_id = $1").bind(gone_owner).execute(&h.pg).await;
    let _ = sqlx::query("DELETE FROM agents WHERE created_by = $1").bind(gone_owner).execute(&h.pg).await;
    let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(gone_owner).execute(&h.pg).await;

    // ---- A line that can take the call is offered a media socket. ----
    let good_number = fresh_number();
    mk_line(&h.pg, h.owner, h.agent, &good_number, true).await;
    let (s, b, _) = ring(h, &good_number).await;
    if s != 200 || !b.contains("<Connect>") {
        return Err(format!("a line that should have answered did not: {s} {b}"));
    }
    // None of the refusals above may have logged a call: a refused call was never
    // answered, so it never happened.
    let logged: i64 = sqlx::query_scalar("SELECT count(*) FROM calls WHERE owner_user_id = $1")
        .bind(h.owner)
        .fetch_one(&h.pg)
        .await
        .map_err(|e| e.to_string())?;
    if logged != 0 {
        return Err(format!("{logged} refused calls were written to the log"));
    }
    Ok(())
}

/// Just the two things the sweep test needs, so it does not have to build a server.
struct SweepFixture {
    pg: PgPool,
    owner: Uuid,
}

/// A call left open by a process that stopped is closed at the next start, and one that
/// is genuinely in progress is left alone.
/// Deliberately does not use the harness above. That one writes the deployment-wide
/// carrier settings, and two tests in one binary writing and restoring the same rows would
/// have each undo the other's while it was still running. This needs only a database.
#[tokio::test]
async fn a_call_left_open_by_a_stopped_process_is_closed_at_the_next_start() {
    let Some(db_url) = std::env::var("DATABASE_URL").ok() else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let pg = db::connect(&db_url, 5).await.expect("DATABASE_URL is set but unreachable");
    let owner = mk_user(&pg, false).await;
    let outcome = check_sweep(&pg, owner).await;
    let _ = sqlx::query("DELETE FROM calls WHERE owner_user_id = $1").bind(owner).execute(&pg).await;
    let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(owner).execute(&pg).await;
    outcome.expect("the sweep behaved wrongly");
}

async fn check_sweep(pg: &PgPool, owner: Uuid) -> Result<(), String> {
    let h = SweepFixture { pg: pg.clone(), owner };
    let stale = Uuid::now_v7();
    let fresh = Uuid::now_v7();
    // The age is a bound parameter rather than part of the statement: a query built by
    // formatting is a query nothing checks, and the tooling rightly refuses one.
    for (id, minutes_ago) in [(stale, 60_i32), (fresh, 0_i32)] {
        sqlx::query(
            "INSERT INTO calls (id, provider, provider_call_id, to_e164, owner_user_id, started_at) \
             VALUES ($1, 'twilio', $2, '+441315550000', $3, now() - make_interval(mins => $4))",
        )
        .bind(id)
        .bind(format!("CAsweep{id}"))
        .bind(h.owner)
        .bind(minutes_ago)
        .execute(&h.pg)
        .await
        .map_err(|e| e.to_string())?;
    }

    log::reconcile_open_calls(&h.pg).await;

    let outcomes = sqlx::query_as::<_, (Uuid, String, bool)>(
        "SELECT id, outcome, ended_at IS NOT NULL FROM calls WHERE id = ANY($1)",
    )
    .bind(vec![stale, fresh])
    .fetch_all(&h.pg)
    .await
    .map_err(|e| e.to_string())?;

    for (id, outcome, ended) in outcomes {
        if id == stale && (outcome != "dropped" || !ended) {
            return Err(format!("a long-open call was left as {outcome:?}"));
        }
        if id == fresh && (outcome != CallEnd::IN_PROGRESS || ended) {
            return Err(format!("a call that may still be live was closed as {outcome:?}"));
        }
    }

    // Closing is idempotent, which is what lets a socket, a carrier notice and this sweep
    // all reach for the same row.
    log::close(&h.pg, stale, CallEnd::Completed, None).await;
    let after = sqlx::query_scalar::<_, String>("SELECT outcome FROM calls WHERE id = $1")
        .bind(stale)
        .fetch_one(&h.pg)
        .await
        .map_err(|e| e.to_string())?;
    if after != "dropped" {
        return Err(format!("closing an already-closed call changed it to {after:?}"));
    }
    Ok(())
}

/// Erasing somebody who owns a telephone line and the agent that answers it must succeed,
/// and take both with it.
///
/// Without this the foreign keys make erasure fail outright, which is the sort of thing
/// discovered when somebody exercises their rights rather than in a test.
#[tokio::test]
async fn erasing_the_owner_of_a_line_takes_the_line_with_it() {
    let Some(db_url) = std::env::var("DATABASE_URL").ok() else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let pg = db::connect(&db_url, 5).await.expect("DATABASE_URL is set but unreachable");

    let owner = mk_user(&pg, false).await;
    let agent = mk_agent(&pg, owner, false).await;
    let number = fresh_number();
    mk_line(&pg, owner, agent, &number, true).await;
    sqlx::query(
        "INSERT INTO calls (id, phone_number_id, provider, provider_call_id, to_e164, \
                            owner_user_id, agent_id) \
         SELECT $1, p.id, 'twilio', $2, p.e164, $3, $4 FROM phone_numbers p WHERE p.e164 = $5",
    )
    .bind(Uuid::now_v7())
    .bind(format!("CAerase{owner}"))
    .bind(owner)
    .bind(agent)
    .bind(&number)
    .execute(&pg)
    .await
    .unwrap();

    // The same sequence the erasure routine performs, in the same order. What is being
    // tested is that the order is possible at all: the line has to go before the agent,
    // or the agent delete trips its foreign key.
    let mut tx = pg.begin().await.unwrap();
    for sql in [
        "DELETE FROM calls WHERE owner_user_id = $1",
        "DELETE FROM phone_numbers WHERE owner_user_id = $1 \
           OR agent_id IN (SELECT id FROM agents WHERE created_by = $1)",
        "DELETE FROM chats WHERE owner_user_id = $1",
        "DELETE FROM agents WHERE created_by = $1",
    ] {
        sqlx::query(sql)
            .bind(owner)
            .execute(&mut *tx)
            .await
            .unwrap_or_else(|e| panic!("erasure failed at {sql:?}: {e}"));
    }
    tx.commit().await.unwrap();

    let lines: i64 = sqlx::query_scalar("SELECT count(*) FROM phone_numbers WHERE e164 = $1")
        .bind(&number)
        .fetch_one(&pg)
        .await
        .unwrap();
    assert_eq!(lines, 0, "the line outlived its owner");

    let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(owner).execute(&pg).await;
}
