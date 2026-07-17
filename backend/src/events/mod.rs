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

//! Internal domain-event bus — the transactional outbox.
//!
//! A domain mutation writes an `events` row in its OWN transaction via
//! [`emit_with`], so an event exists **iff** its mutation committed (no event for
//! a rolled-back change; no lost event for a committed one). The dispatcher
//! ([`crate::workflows::dispatch_once`]) relays undispatched events to matching
//! workflows. Provenance (`actor_type` + `trigger_chain` + `depth`) drives the
//! loop guards: a workflow triggers only on human-originated events by
//! default, and a cascade is capped by `depth`.
//!
//! The bus is **internal only** — these are platform domain events, never
//! external ingress.

use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

/// Origin of an event. Maps to the `event_actor_type` Postgres enum. The loop
/// guard keys off this: a workflow ignores non-`Human` events unless it
/// explicitly opts in via `trigger_on_system_events`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "event_actor_type", rename_all = "lowercase")]
pub enum ActorType {
    Human,
    Agent,
    Workflow,
    System,
}

// v1 event catalogue subset we emit. Stable strings — workflows match on these.
pub const DOCUMENT_INGESTED: &str = "document.ingested";
pub const DOCUMENT_DELETED: &str = "document.deleted";
pub const PROJECT_MEMBER_ADDED: &str = "project.member_added";
/// A user archived their own account (self-service). Human actor.
pub const ACCOUNT_ARCHIVED: &str = "account.archived";
/// The child event a `post_message` system-action emits — `actor_type=workflow`,
/// so by default it triggers nothing. Makes the loop guard observable end-to-end.
pub const WORKFLOW_MESSAGE_POSTED: &str = "workflow.message_posted";

// New-subsystem catalogue. Emitted by Core mutations and — via the
// public `emit_with` outbox API — by the Enterprise crate in its own paths. Names
// are stable: they surface in the trigger UI and workflows match on them.
/// A document arrived from an external source (connector import). Distinct from
/// [`DOCUMENT_INGESTED`] (readiness in a KB): `imported` is the fact of arrival
/// into the workspace; a KB-destined import still yields `document.ingested` once
/// the scheduler finishes ingesting. Human on manual import, System on sync.
pub const DOCUMENT_IMPORTED: &str = "document.imported";
/// A directory user was provisioned (SCIM create/adopt, or local manual create).
/// Named `directory.*` to avoid clashing with the `user.provisioned` **audit**
/// action (a different bus). System actor for SCIM.
pub const DIRECTORY_USER_PROVISIONED: &str = "directory.user_provisioned";
/// A directory user was deactivated (SCIM deactivate/delete, or local deactivate).
pub const DIRECTORY_USER_DEACTIVATED: &str = "directory.user_deactivated";
/// A user was added to a group (Core admin action = Human; SCIM/JIT sync = System).
pub const GROUP_MEMBER_ADDED: &str = "group.member_added";
/// A user was removed from a group.
pub const GROUP_MEMBER_REMOVED: &str = "group.member_removed";
/// A user was added to a group chat. Human actor.
pub const CHAT_MEMBER_ADDED: &str = "chat.member_added";

/// An event to append to the outbox. Build via [`NewEvent::new`], then chain the
/// setters for the fields you need.
#[derive(Debug, Clone)]
pub struct NewEvent {
    pub event_type: String,
    pub actor_type: ActorType,
    pub actor_user_id: Option<Uuid>,
    pub resource_type: Option<String>,
    pub resource_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub payload: Value,
    pub causation_id: Option<Uuid>,
    pub trigger_chain: Vec<Uuid>,
    pub depth: i32,
}

impl NewEvent {
    pub fn new(event_type: impl Into<String>, actor_type: ActorType) -> Self {
        Self {
            event_type: event_type.into(),
            actor_type,
            actor_user_id: None,
            resource_type: None,
            resource_id: None,
            project_id: None,
            payload: Value::Null,
            causation_id: None,
            trigger_chain: Vec::new(),
            depth: 0,
        }
    }
    pub fn actor(mut self, user_id: Option<Uuid>) -> Self {
        self.actor_user_id = user_id;
        self
    }
    pub fn resource(mut self, rtype: impl Into<String>, id: Uuid) -> Self {
        self.resource_type = Some(rtype.into());
        self.resource_id = Some(id);
        self
    }
    pub fn project(mut self, id: Option<Uuid>) -> Self {
        self.project_id = id;
        self
    }
    pub fn payload(mut self, p: Value) -> Self {
        self.payload = p;
        self
    }
}

/// A persisted event as the dispatcher reads it back from the outbox.
#[derive(Debug, Clone)]
pub struct EventRow {
    pub id: Uuid,
    pub event_type: String,
    pub actor_type: ActorType,
    pub actor_user_id: Option<Uuid>,
    pub resource_type: Option<String>,
    pub resource_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub payload: Value,
    pub trigger_chain: Vec<Uuid>,
    pub depth: i32,
}

/// Append an event to the outbox **using the caller's transaction**, so it commits
/// atomically with the mutation it records. Returns the new event id.
/// There is deliberately no autocommit variant — every emit rides a mutation's tx.
pub async fn emit_with(conn: &mut PgConnection, ev: &NewEvent) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::now_v7();
    let payload = if ev.payload.is_null() {
        serde_json::json!({})
    } else {
        ev.payload.clone()
    };
    sqlx::query!(
        r#"INSERT INTO events
             (id, event_type, actor_type, actor_user_id, resource_type, resource_id,
              project_id, payload, causation_id, trigger_chain, depth)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"#,
        id,
        ev.event_type,
        ev.actor_type as ActorType,
        ev.actor_user_id,
        ev.resource_type.as_deref(),
        ev.resource_id,
        ev.project_id,
        payload,
        ev.causation_id,
        &ev.trigger_chain,
        ev.depth,
    )
    .execute(&mut *conn)
    .await?;
    Ok(id)
}

/// Provenance propagation: the event a workflow run itself causes carries
/// `actor_type=workflow`, `depth = parent.depth + 1`, and the parent chain with
/// this run appended — so the depth cap + human-only rule break any cascade. Pure.
pub fn child_event(
    event_type: impl Into<String>,
    parent: &EventRow,
    run_id: Uuid,
    owner: Option<Uuid>,
) -> NewEvent {
    let mut chain = parent.trigger_chain.clone();
    chain.push(run_id);
    NewEvent {
        event_type: event_type.into(),
        actor_type: ActorType::Workflow,
        actor_user_id: owner,
        resource_type: parent.resource_type.clone(),
        resource_id: parent.resource_id,
        project_id: parent.project_id,
        payload: Value::Null,
        causation_id: Some(parent.id),
        trigger_chain: chain,
        depth: parent.depth + 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parent() -> EventRow {
        EventRow {
            id: Uuid::now_v7(),
            event_type: DOCUMENT_INGESTED.into(),
            actor_type: ActorType::Human,
            actor_user_id: None,
            resource_type: Some("kb_document".into()),
            resource_id: Some(Uuid::now_v7()),
            project_id: Some(Uuid::now_v7()),
            payload: serde_json::json!({"filename": "x.pdf"}),
            trigger_chain: vec![],
            depth: 0,
        }
    }

    #[test]
    fn child_event_propagates_provenance() {
        let p = parent();
        let run = Uuid::now_v7();
        let owner = Some(Uuid::now_v7());
        let c = child_event(WORKFLOW_MESSAGE_POSTED, &p, run, owner);
        assert_eq!(c.actor_type, ActorType::Workflow);
        assert_eq!(c.depth, 1, "depth increments by one hop");
        assert_eq!(c.trigger_chain, vec![run], "run id appended to the chain");
        assert_eq!(c.causation_id, Some(p.id), "parent recorded as cause");
        assert_eq!(c.project_id, p.project_id, "scope inherited");
    }

    #[test]
    fn child_event_chain_grows_each_hop() {
        let mut p = parent();
        let r1 = Uuid::now_v7();
        let c1 = child_event(WORKFLOW_MESSAGE_POSTED, &p, r1, None);
        // Simulate the child being persisted and re-read as the next parent.
        p.trigger_chain = c1.trigger_chain.clone();
        p.depth = c1.depth;
        let r2 = Uuid::now_v7();
        let c2 = child_event(WORKFLOW_MESSAGE_POSTED, &p, r2, None);
        assert_eq!(c2.depth, 2);
        assert_eq!(c2.trigger_chain, vec![r1, r2]);
    }
}
