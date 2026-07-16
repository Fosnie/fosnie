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

//! Connection manager (FEATURE B1): one live `McpConn` per approved server, keyed
//! by slug, on `AppState`. Mirrors the `VoiceSessions`/`Approvals` registry pattern
//! (`Arc<RwLock<HashMap>>`). Connections are opened on admin approval and reconciled
//! by the periodic `mcp_health` sweep (see `mcp::health_sweep`).

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::error::{AppError, Result};
use crate::mcp::client::{connect, McpConn, Transport};
use crate::mcp::ToolCatalogEntry;

/// The cache key for a live connection. A server's slug plus, for an OAuth server, the id
/// of the connection whose token authenticates it — different users (and the service
/// connection) hold distinct live connections to the same server. `None` covers every
/// non-OAuth auth type (`none|bearer|api_key|header`), one shared connection per slug, so
/// their behaviour is unchanged.
pub type ConnKey = (String, Option<Uuid>);

fn key(slug: &str, connection_id: Option<Uuid>) -> ConnKey {
    (slug.to_string(), connection_id)
}

#[derive(Clone, Default)]
pub struct McpManager {
    conns: Arc<RwLock<HashMap<ConnKey, Arc<dyn McpConn>>>>,
}

impl McpManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a real connection and cache it; returns its discovered catalog.
    pub async fn connect(
        &self,
        slug: &str,
        connection_id: Option<Uuid>,
        transport: Transport,
    ) -> Result<Vec<ToolCatalogEntry>> {
        let conn = connect(transport).await?;
        let tools = conn.list_tools().await?;
        self.conns.write().await.insert(key(slug, connection_id), conn);
        Ok(tools)
    }

    /// Inject a pre-built connection (the OAuth path, tests, the `FakeConn` demo path).
    pub async fn insert_conn(&self, slug: &str, connection_id: Option<Uuid>, conn: Arc<dyn McpConn>) {
        self.conns.write().await.insert(key(slug, connection_id), conn);
    }

    pub async fn disconnect(&self, slug: &str, connection_id: Option<Uuid>) {
        self.conns.write().await.remove(&key(slug, connection_id));
    }

    /// Drop every cached connection for a slug, whatever the connection id. Used when a
    /// server is quarantined, deleted, or reconfigured and all of its live connections
    /// (across users) must be torn down at once.
    pub async fn disconnect_all(&self, slug: &str) {
        self.conns.write().await.retain(|(s, _), _| s != slug);
    }

    pub async fn is_connected(&self, slug: &str, connection_id: Option<Uuid>) -> bool {
        self.conns.read().await.contains_key(&key(slug, connection_id))
    }

    pub async fn list_tools(&self, slug: &str, connection_id: Option<Uuid>) -> Result<Vec<ToolCatalogEntry>> {
        self.get(slug, connection_id).await?.list_tools().await
    }

    pub async fn call_tool(
        &self,
        slug: &str,
        connection_id: Option<Uuid>,
        tool: &str,
        args: Value,
    ) -> Result<String> {
        self.get(slug, connection_id).await?.call_tool(tool, args).await
    }

    pub async fn ping(&self, slug: &str, connection_id: Option<Uuid>) -> bool {
        match self.get(slug, connection_id).await {
            Ok(c) => c.ping().await,
            Err(_) => false,
        }
    }

    async fn get(&self, slug: &str, connection_id: Option<Uuid>) -> Result<Arc<dyn McpConn>> {
        self.conns
            .read()
            .await
            .get(&key(slug, connection_id))
            .cloned()
            .ok_or_else(|| AppError::Validation(format!("MCP server '{slug}' is not connected")))
    }
}
