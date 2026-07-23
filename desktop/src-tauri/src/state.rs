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

//! What the client holds while it is running: which instance it is paired with,
//! the HTTP client it uses for its own few calls, and the socket.

use anyhow::Result;
use tauri::AppHandle;
use tokio::sync::RwLock;

use crate::store::Pairing;
use crate::ws::Bridge;

/// The platform name this client reports when pairing. The instance accepts
/// exactly `windows`, `macos` or `linux`.
pub const PLATFORM: &str = if cfg!(target_os = "windows") {
    "windows"
} else if cfg!(target_os = "macos") {
    "macos"
} else {
    "linux"
};

/// What this machine will be called in the owner's list of devices. The host name
/// is what a person recognises; anything else and a list of five machines is five
/// identical rows.
pub fn device_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "Desktop".to_string())
}

pub struct Shell {
    pub http: reqwest::Client,
    pub bridge: Bridge,
    pairing: RwLock<Option<Pairing>>,
    /// The address someone is part-way through typing on the pairing screen, so
    /// the link to its profile page can be opened before there is a pairing.
    candidate: RwLock<Option<String>>,
}

impl Shell {
    pub fn new(http: reqwest::Client, pairing: Option<Pairing>) -> Self {
        Self {
            http,
            bridge: Bridge::default(),
            pairing: RwLock::new(pairing),
            candidate: RwLock::new(None),
        }
    }

    pub async fn pairing(&self) -> Option<Pairing> {
        self.pairing.read().await.clone()
    }

    /// Take up a pairing and bring the socket up on it.
    pub async fn adopt(&self, app: AppHandle, pairing: Pairing) -> Result<()> {
        *self.pairing.write().await = Some(pairing.clone());
        *self.candidate.write().await = Some(pairing.base_url.clone());
        self.bridge.start(app, self.http.clone(), pairing).await
    }

    pub async fn clear_pairing(&self) {
        *self.pairing.write().await = None;
    }

    pub async fn remember_candidate(&self, base_url: String) {
        *self.candidate.write().await = Some(base_url);
    }

    pub async fn candidate_instance(&self) -> Option<String> {
        self.candidate.read().await.clone()
    }
}
