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

use crate::error::{AppError, Result};
use crate::mcp::client::{connect, McpConn, Transport};
use crate::mcp::ToolCatalogEntry;

#[derive(Clone, Default)]
pub struct McpManager {
    conns: Arc<RwLock<HashMap<String, Arc<dyn McpConn>>>>,
}

impl McpManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a real connection and cache it; returns its discovered catalog.
    pub async fn connect(&self, slug: &str, transport: Transport) -> Result<Vec<ToolCatalogEntry>> {
        let conn = connect(transport).await?;
        let tools = conn.list_tools().await?;
        self.conns.write().await.insert(slug.to_string(), conn);
        Ok(tools)
    }

    /// Inject a connection directly (tests / the `FakeConn` demo path).
    pub async fn insert_conn(&self, slug: &str, conn: Arc<dyn McpConn>) {
        self.conns.write().await.insert(slug.to_string(), conn);
    }

    pub async fn disconnect(&self, slug: &str) {
        self.conns.write().await.remove(slug);
    }

    pub async fn is_connected(&self, slug: &str) -> bool {
        self.conns.read().await.contains_key(slug)
    }

    pub async fn list_tools(&self, slug: &str) -> Result<Vec<ToolCatalogEntry>> {
        self.get(slug).await?.list_tools().await
    }

    pub async fn call_tool(&self, slug: &str, tool: &str, args: Value) -> Result<String> {
        self.get(slug).await?.call_tool(tool, args).await
    }

    pub async fn ping(&self, slug: &str) -> bool {
        match self.get(slug).await {
            Ok(c) => c.ping().await,
            Err(_) => false,
        }
    }

    async fn get(&self, slug: &str) -> Result<Arc<dyn McpConn>> {
        self.conns
            .read()
            .await
            .get(slug)
            .cloned()
            .ok_or_else(|| AppError::Validation(format!("MCP server '{slug}' is not connected")))
    }
}
