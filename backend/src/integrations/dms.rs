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

//! DMS / CMS connector framework.
//!
//! **Framework only in v1.** No DMS-specific connector ships: an iManage or
//! NetDocuments connector is pointless without a live client to test against
//! ("connectors are written as clients appear"). What ships now is the
//! connector-adapter *interface* every future connector implements, plus dormant
//! stubs for the named v1 connectors that fail honestly until a client lands.
//!
//! Activation is the same dual-mode gate as every other connector
//! ([`super::guard_egress`]): a DMS connector reaches the network only when an
//! admin has enabled it by config. The detailed adapter signatures
//! (auth flows, exact paging/cursor semantics, import-vs-live-stream) are Pass-2.

use async_trait::async_trait;
use uuid::Uuid;

use super::ConnectorKind;
use crate::auth::AuthContext;
use crate::error::{AppError, Result};
use crate::state::AppState;

/// Opaque pagination token handed back to the connector to fetch the next page.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Cursor(pub String);

/// One page of results plus the cursor for the next page (None ⇒ last page).
#[derive(Debug, Clone, serde::Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next: Option<Cursor>,
}

/// A document as seen through a DMS connector. Bytes are fetched lazily via
/// [`DmsConnector::fetch`]; `version` drives import de-duplication and the
/// write-back conflict check.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct DmsDoc {
    pub id: String,
    pub name: String,
    pub mime: Option<String>,
    /// Source version/edit token (iManage `version`, ND `version`) — the dedup key.
    pub version: Option<String>,
    pub modified_at: Option<String>,
    pub size: Option<i64>,
    /// The container (workspace/folder) this document lives in, when known.
    pub container_id: Option<String>,
}

/// One entry in a browse listing: a container (workspace/folder) or a document.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DmsEntry {
    /// `"container"` (drill into it) or `"doc"` (importable).
    pub entry_kind: String,
    pub id: String,
    pub name: String,
    pub mime: Option<String>,
    pub container_id: Option<String>,
}

/// The connector-adapter contract. The six method families
/// are binding; exact signatures are Pass-2 and may grow (auth scopes, write
/// metadata, richer search filters).
#[async_trait]
pub trait DmsConnector: Send + Sync {
    /// Establish an authenticated session with the external DMS.
    async fn authenticate(&self, state: &AppState, ctx: &AuthContext) -> Result<()>;

    /// list / search / browse — a paged query over the DMS, `cursor` continues a
    /// previous page.
    async fn search(
        &self,
        state: &AppState,
        ctx: &AuthContext,
        query: &str,
        cursor: Option<Cursor>,
    ) -> Result<Page<DmsDoc>>;

    /// Fetch one document's bytes by its DMS id.
    async fn fetch_document(
        &self,
        state: &AppState,
        ctx: &AuthContext,
        id: &str,
    ) -> Result<Vec<u8>>;

    /// Write a document back to the DMS (new version / check-in).
    async fn write_back(
        &self,
        state: &AppState,
        ctx: &AuthContext,
        id: &str,
        bytes: &[u8],
    ) -> Result<()>;

    // ── Connection-scoped surface ──────────────────────────────────────────────
    // These carry the specific `connection_id` to act under (a user may hold several
    // connections of one kind). Default bodies return `Unavailable` so the Core
    // `NotBuilt` stub + test fakes need no change; the enterprise adapters override.

    /// Browse a container (workspace/folder). `container_id = None` ⇒ the root/top
    /// containers. Returns sub-containers and documents.
    async fn browse(
        &self,
        _state: &AppState,
        _ctx: &AuthContext,
        _connection_id: Uuid,
        _container_id: Option<&str>,
    ) -> Result<Page<DmsEntry>> {
        Err(AppError::Unavailable("browse not implemented".into()))
    }

    /// Metadata for one document (name/mime/version/size + custom attributes).
    async fn metadata(&self, _state: &AppState, _ctx: &AuthContext, _connection_id: Uuid, _id: &str) -> Result<DmsDoc> {
        Err(AppError::Unavailable("metadata not implemented".into()))
    }

    /// The effective source ACL for a document, captured losslessly (D5).
    async fn acl(&self, _state: &AppState, _ctx: &AuthContext, _connection_id: Uuid, _id: &str) -> Result<serde_json::Value> {
        Err(AppError::Unavailable("acl not implemented".into()))
    }

    /// Changed documents in a container since `cursor` (delta for continuous sync).
    async fn changes(
        &self,
        _state: &AppState,
        _ctx: &AuthContext,
        _connection_id: Uuid,
        _container_id: &str,
        _cursor: Option<Cursor>,
    ) -> Result<Page<DmsDoc>> {
        Err(AppError::Unavailable("changes not implemented".into()))
    }

    /// Fetch one document's bytes under `connection_id`.
    async fn fetch(&self, _state: &AppState, _ctx: &AuthContext, _connection_id: Uuid, _id: &str) -> Result<Vec<u8>> {
        Err(AppError::Unavailable("fetch not implemented".into()))
    }

    /// Upload a new version of a document (write-back). Returns the new remote version.
    async fn upload_version(
        &self,
        _state: &AppState,
        _ctx: &AuthContext,
        _connection_id: Uuid,
        _id: &str,
        _bytes: &[u8],
    ) -> Result<String> {
        Err(AppError::Unavailable("upload_version not implemented".into()))
    }
}

/// A connector that is named in v1 but not yet built — every method fails with a
/// clear, honest error. Activation still flips the config flag and audits; the
/// surface simply reports it has no client until one is written.
struct NotBuilt {
    kind: ConnectorKind,
}

impl NotBuilt {
    fn err<T>(&self) -> Result<T> {
        Err(AppError::Unavailable(format!(
            "{} connector not built — no client to test against (Pass-2)",
            self.kind.display_name()
        )))
    }
}

#[async_trait]
impl DmsConnector for NotBuilt {
    async fn authenticate(&self, _state: &AppState, _ctx: &AuthContext) -> Result<()> {
        self.err()
    }
    async fn search(
        &self,
        _state: &AppState,
        _ctx: &AuthContext,
        _query: &str,
        _cursor: Option<Cursor>,
    ) -> Result<Page<DmsDoc>> {
        self.err()
    }
    async fn fetch_document(
        &self,
        _state: &AppState,
        _ctx: &AuthContext,
        _id: &str,
    ) -> Result<Vec<u8>> {
        self.err()
    }
    async fn write_back(
        &self,
        _state: &AppState,
        _ctx: &AuthContext,
        _id: &str,
        _bytes: &[u8],
    ) -> Result<()> {
        self.err()
    }
}

/// The Core [`ConnectorRegistry`](crate::ext::ConnectorRegistry): the named v1 DMS
/// kinds resolve to dormant `NotBuilt` adapters, everything else to `None`. A
/// private `fosnie-enterprise` crate injects a registry returning real
/// iManage/NetDocuments adapters via [`crate::state::AppStateBuilder::with_connectors`].
pub struct DefaultConnectorRegistry;

impl crate::ext::ConnectorRegistry for DefaultConnectorRegistry {
    fn resolve_dms(&self, kind: ConnectorKind) -> Option<Box<dyn DmsConnector>> {
        match kind {
            ConnectorKind::IManage | ConnectorKind::NetDocuments => Some(Box::new(NotBuilt { kind })),
            _ => None,
        }
    }

    fn resolve_mail(
        &self,
        kind: ConnectorKind,
    ) -> Option<Box<dyn super::mail::MailConnector>> {
        match kind {
            ConnectorKind::Outlook | ConnectorKind::Gmail => {
                Some(Box::new(super::mail::NotBuilt { kind }))
            }
            _ => None,
        }
    }
}

/// Resolve a DMS connector for a kind. Thin delegator to the Core
/// [`DefaultConnectorRegistry`] — for tests/non-`AppState` callers. Stateful
/// call-sites go through the `state.connectors` slot instead.
pub fn resolve(kind: ConnectorKind) -> Option<Box<dyn DmsConnector>> {
    use crate::ext::ConnectorRegistry;
    DefaultConnectorRegistry.resolve_dms(kind)
}

/// Guarded DMS search: enforce the dormancy gate (audited) **before** touching
/// any connector, then run the resolved adapter. The single entry point a tool
/// or HTTP handler should call — it cannot reach the network while dormant.
pub async fn dms_search(
    state: &AppState,
    ctx: &AuthContext,
    kind: ConnectorKind,
    query: &str,
) -> Result<Page<DmsDoc>> {
    super::guard_egress(state, ctx, kind).await?;
    let connector = state
        .connectors
        .resolve_dms(kind)
        .ok_or_else(|| AppError::Validation(format!("{} is not a DMS connector", kind.as_str())))?;
    connector.search(state, ctx, query, None).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_only_dms_kinds() {
        assert!(resolve(ConnectorKind::IManage).is_some());
        assert!(resolve(ConnectorKind::NetDocuments).is_some());
        assert!(resolve(ConnectorKind::WebSearch).is_none());
        assert!(resolve(ConnectorKind::Mcp).is_none());
    }

    #[test]
    fn default_registry_resolves_mail_kinds_only() {
        use crate::ext::ConnectorRegistry;
        let reg = DefaultConnectorRegistry;
        assert!(reg.resolve_mail(ConnectorKind::Outlook).is_some());
        assert!(reg.resolve_mail(ConnectorKind::Gmail).is_some());
        assert!(reg.resolve_mail(ConnectorKind::IManage).is_none());
        assert!(reg.resolve_mail(ConnectorKind::WebSearch).is_none());
    }
}
