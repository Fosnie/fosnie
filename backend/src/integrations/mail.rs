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

//! Mail connector framework.
//!
//! The sibling of [`super::dms`] for mailboxes (Outlook via Graph, Gmail via the
//! Gmail API). **Framework only in a Core build:** the Core registry resolves the
//! named mail kinds to a dormant [`NotBuilt`] adapter that fails honestly; a private
//! `fosnie-enterprise` crate injects the real adapters via
//! [`crate::state::AppStateBuilder::with_connectors`].
//!
//! Activation is the same dual-mode gate as every other connector
//! ([`super::guard_egress`]) — a mail connector reaches the network only once an
//! admin has enabled its kind.

use async_trait::async_trait;
use uuid::Uuid;

use super::dms::{Cursor, Page};
use super::ConnectorKind;
use crate::auth::AuthContext;
use crate::error::{AppError, Result};
use crate::state::AppState;

/// A mail folder / label as seen through a mail connector.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MailFolder {
    pub id: String,
    pub name: String,
}

/// Lightweight message metadata for listing / delta.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MailMeta {
    pub id: String,
    pub subject: Option<String>,
    pub from: Option<String>,
    pub received_at: Option<String>,
    /// A source-native change token (Graph `changeKey`, Gmail internal id) used for
    /// dedup/version detection where the API exposes one.
    pub version: Option<String>,
}

/// One attachment's bytes + naming, fetched with the full message.
#[derive(Debug, Clone)]
pub struct MailAttachment {
    pub filename: String,
    pub mime: Option<String>,
    pub bytes: Vec<u8>,
}

/// A fully-fetched message: headers, body (HTML preferred, plain-text fallback),
/// attachments, and the raw MIME (kept for provenance, not indexed).
#[derive(Debug, Clone)]
pub struct MailFull {
    pub id: String,
    pub subject: Option<String>,
    pub from: Option<String>,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub date: Option<String>,
    pub body_html: Option<String>,
    pub body_text: Option<String>,
    pub attachments: Vec<MailAttachment>,
    pub raw_mime: Option<Vec<u8>>,
    pub version: Option<String>,
}

/// The mail connector-adapter contract. Signatures mirror [`super::dms::DmsConnector`];
/// exact paging/delta semantics are the adapter's concern. Every method carries the
/// `connection_id` of the specific stored connection to act under — a user may hold
/// several connections of one kind, and a sync mapping names a particular one.
#[async_trait]
pub trait MailConnector: Send + Sync {
    /// Establish an authenticated session for `connection_id`.
    async fn authenticate(&self, state: &AppState, ctx: &AuthContext, connection_id: Uuid) -> Result<()>;

    /// The connection's folders (Outlook mail folders) / labels (Gmail).
    async fn folders(&self, state: &AppState, ctx: &AuthContext, connection_id: Uuid) -> Result<Vec<MailFolder>>;

    /// A page of message metadata in a folder; `cursor` continues a previous page.
    async fn list(
        &self,
        state: &AppState,
        ctx: &AuthContext,
        connection_id: Uuid,
        folder: &str,
        cursor: Option<Cursor>,
    ) -> Result<Page<MailMeta>>;

    /// Fetch one message in full (headers, body, attachments, raw MIME).
    async fn fetch(&self, state: &AppState, ctx: &AuthContext, connection_id: Uuid, id: &str) -> Result<MailFull>;

    /// Changed messages since `cursor` (Graph delta / Gmail history). The returned
    /// cursor advances the sync mapping. A `None` incoming cursor ⇒ initial sync.
    async fn changes(
        &self,
        state: &AppState,
        ctx: &AuthContext,
        connection_id: Uuid,
        folder: &str,
        cursor: Option<Cursor>,
    ) -> Result<Page<MailMeta>>;
}

/// A mail connector named but not built (Core build) — every method fails clearly.
pub struct NotBuilt {
    pub kind: ConnectorKind,
}

impl NotBuilt {
    fn err<T>(&self) -> Result<T> {
        Err(AppError::Unavailable(format!(
            "{} connector not built in this edition",
            self.kind.display_name()
        )))
    }
}

#[async_trait]
impl MailConnector for NotBuilt {
    async fn authenticate(&self, _state: &AppState, _ctx: &AuthContext, _connection_id: Uuid) -> Result<()> {
        self.err()
    }
    async fn folders(&self, _state: &AppState, _ctx: &AuthContext, _connection_id: Uuid) -> Result<Vec<MailFolder>> {
        self.err()
    }
    async fn list(
        &self,
        _state: &AppState,
        _ctx: &AuthContext,
        _connection_id: Uuid,
        _folder: &str,
        _cursor: Option<Cursor>,
    ) -> Result<Page<MailMeta>> {
        self.err()
    }
    async fn fetch(&self, _state: &AppState, _ctx: &AuthContext, _connection_id: Uuid, _id: &str) -> Result<MailFull> {
        self.err()
    }
    async fn changes(
        &self,
        _state: &AppState,
        _ctx: &AuthContext,
        _connection_id: Uuid,
        _folder: &str,
        _cursor: Option<Cursor>,
    ) -> Result<Page<MailMeta>> {
        self.err()
    }
}

/// Guarded mail listing: the dormancy gate (audited) runs **before** the connector
/// is resolved, so it cannot reach the network while dormant. The single entry point
/// an HTTP handler / job should call.
pub async fn mail_list(
    state: &AppState,
    ctx: &AuthContext,
    kind: ConnectorKind,
    connection_id: Uuid,
    folder: &str,
    cursor: Option<Cursor>,
) -> Result<Page<MailMeta>> {
    super::guard_egress(state, ctx, kind).await?;
    let connector = state
        .connectors
        .resolve_mail(kind)
        .ok_or_else(|| AppError::Validation(format!("{} is not a mail connector", kind.as_str())))?;
    connector.list(state, ctx, connection_id, folder, cursor).await
}
