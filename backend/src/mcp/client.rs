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

//! The MCP client transport boundary (FEATURE B1). `McpConn` is the trait every
//! live connection implements; `RmcpConn` is the real `rmcp` adapter (stdio via
//! `TokioChildProcess` + streamable-HTTP) and is the ONLY place that touches the
//! `rmcp` crate. `FakeConn` is a deterministic in-process implementation used by
//! tests + (optionally) demos, so the registry / dispatch / RBAC / HITL logic is
//! exercised without a live server.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::error::{AppError, Result};
use crate::mcp::ToolCatalogEntry;

/// Auth to inject on every request to a remote (streamable-HTTP) server. Values are
/// already decrypted by the caller and set as a `default_headers` entry on the reqwest
/// client we hand to rmcp (our reqwest is aligned to rmcp's, so `with_client` works).
#[derive(Debug, Clone)]
pub enum HttpAuth {
    /// Sent as `Authorization: Bearer <token>`.
    Bearer(String),
    /// An arbitrary header, e.g. `CONTEXT7_API_KEY: <key>`.
    Header { name: String, value: String },
}

impl HttpAuth {
    /// The `(header_name, header_value)` to inject on every request.
    fn wire(&self) -> (String, String) {
        match self {
            HttpAuth::Bearer(token) => ("Authorization".to_string(), format!("Bearer {token}")),
            HttpAuth::Header { name, value } => (name.clone(), value.clone()),
        }
    }
}

/// How to reach a server.
#[derive(Debug, Clone)]
pub enum Transport {
    /// Spawn a client-internal server as a child process (JSON-RPC over stdio).
    Stdio { command: Vec<String> },
    /// A server on the client's private network or a remote/public endpoint
    /// (streamable-HTTP). `auth`, when present, is injected as a default header.
    Http { url: String, auth: Option<HttpAuth> },
}

/// A live MCP connection — list its tools, call one, check liveness.
#[async_trait]
pub trait McpConn: Send + Sync {
    async fn list_tools(&self) -> Result<Vec<ToolCatalogEntry>>;
    async fn call_tool(&self, tool: &str, args: Value) -> Result<String>;
    async fn ping(&self) -> bool;
}

/// Open a real connection over `transport` (handshake + protocol negotiation).
pub async fn connect(transport: Transport) -> Result<Arc<dyn McpConn>> {
    let conn = RmcpConn::connect(transport).await?;
    Ok(Arc::new(conn))
}

// ── Real rmcp adapter ────────────────────────────────────────────────────────
use rmcp::model::CallToolRequestParams;
use rmcp::service::RunningService;
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};
use rmcp::transport::TokioChildProcess;
use rmcp::{RoleClient, ServiceExt};

struct RmcpConn {
    service: RunningService<RoleClient, ()>,
}

impl RmcpConn {
    async fn connect(transport: Transport) -> Result<Self> {
        let service = match transport {
            Transport::Stdio { command } => {
                let (bin, args) = command
                    .split_first()
                    .ok_or_else(|| AppError::Validation("empty stdio command".into()))?;
                let mut cmd = tokio::process::Command::new(bin);
                cmd.args(args);
                let tp = TokioChildProcess::new(cmd)
                    .map_err(|e| AppError::Other(anyhow::anyhow!("spawn MCP child: {e}")))?;
                ().serve(tp)
                    .await
                    .map_err(|e| AppError::Other(anyhow::anyhow!("MCP stdio handshake: {e}")))?
            }
            Transport::Http { url, auth } => {
                // Build our own reqwest client so we can inject auth as a default header
                // AND forbid redirects — MCP is request/response over a fixed endpoint, so
                // a 3xx is never legitimate and following one could reach an internal host
                // (SSRF-to-private) with the secret header attached.
                let mut headers = reqwest::header::HeaderMap::new();
                if let Some(a) = &auth {
                    let (name, value) = a.wire();
                    let hn = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                        .map_err(|e| AppError::Validation(format!("invalid auth header name: {e}")))?;
                    let mut hv = reqwest::header::HeaderValue::from_str(&value)
                        .map_err(|e| AppError::Validation(format!("invalid auth header value: {e}")))?;
                    hv.set_sensitive(true);
                    headers.insert(hn, hv);
                }
                let client = reqwest::Client::builder()
                    .default_headers(headers)
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .map_err(|e| AppError::Other(anyhow::anyhow!("build MCP http client: {e}")))?;
                let cfg = StreamableHttpClientTransportConfig::with_uri(url);
                let tp = StreamableHttpClientTransport::with_client(client, cfg);
                ().serve(tp)
                    .await
                    .map_err(|e| AppError::Other(anyhow::anyhow!("MCP http handshake: {e}")))?
            }
        };
        Ok(Self { service })
    }
}

#[async_trait]
impl McpConn for RmcpConn {
    async fn list_tools(&self) -> Result<Vec<ToolCatalogEntry>> {
        let res = self
            .service
            .peer()
            .list_tools(Default::default())
            .await
            .map_err(|e| AppError::Other(anyhow::anyhow!("list_tools: {e}")))?;
        Ok(res
            .tools
            .into_iter()
            .map(|t| {
                let schema = Value::Object((*t.input_schema).clone());
                let description = t.description.map(|c| c.to_string()).unwrap_or_default();
                ToolCatalogEntry {
                    name: t.name.to_string(),
                    description,
                    schema,
                    // Unknown ⇒ side-effecting (gated). Admins can refine per tool.
                    side_effecting: true,
                }
            })
            .collect())
    }

    async fn call_tool(&self, tool: &str, args: Value) -> Result<String> {
        let arguments = match args {
            Value::Object(m) => Some(m),
            Value::Null => None,
            other => {
                let mut m = serde_json::Map::new();
                m.insert("value".into(), other);
                Some(m)
            }
        };
        let mut param = CallToolRequestParams::new(tool.to_string());
        if let Some(m) = arguments {
            param = param.with_arguments(m);
        }
        let res = self
            .service
            .peer()
            .call_tool(param)
            .await
            .map_err(|e| AppError::Other(anyhow::anyhow!("call_tool: {e}")))?;
        let mut out = String::new();
        for c in &res.content {
            if let Some(t) = c.as_text() {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&t.text);
            }
        }
        if res.is_error.unwrap_or(false) {
            return Err(AppError::Other(anyhow::anyhow!("MCP tool reported an error: {out}")));
        }
        Ok(out)
    }

    async fn ping(&self) -> bool {
        self.service.peer().list_tools(Default::default()).await.is_ok()
    }
}

// ── Deterministic fake (tests / demos) ───────────────────────────────────────
/// An in-process fake connection: a fixed catalog and an echoing `call_tool`.
/// Lets the whole MCP pipeline (registry → namespace → dispatch → result, RBAC,
/// HITL, rug-pull) be tested without a live server or the rmcp transport.
pub struct FakeConn {
    pub catalog: Vec<ToolCatalogEntry>,
}

#[async_trait]
impl McpConn for FakeConn {
    async fn list_tools(&self) -> Result<Vec<ToolCatalogEntry>> {
        Ok(self.catalog.clone())
    }
    async fn call_tool(&self, tool: &str, args: Value) -> Result<String> {
        if self.catalog.iter().any(|t| t.name == tool) {
            Ok(format!("ok:{tool}:{args}"))
        } else {
            Err(AppError::Validation(format!("unknown tool '{tool}'")))
        }
    }
    async fn ping(&self) -> bool {
        true
    }
}
