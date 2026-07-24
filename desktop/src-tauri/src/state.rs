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
    /// Commands still running in a connected folder, so one can be stopped from
    /// the window and none outlives the socket that asked for it.
    pub executor: crate::executor::Running,
    /// The SHA-256 of each file at the moment the agent last read it, so a write
    /// can be refused if the file changed on disk underneath it. Tracked here
    /// rather than asked of the model, which cannot carry a hash reliably. Keyed by
    /// the resolved file path.
    pub read_hashes: std::sync::Mutex<std::collections::HashMap<String, String>>,
    /// This run of the client, which is how backups are grouped.
    pub session: String,
    pairing: RwLock<Option<Pairing>>,
    /// Where the instance is, readable without waiting. The pairing behind the
    /// lock is the same answer; a folder request is resolved on a path that has
    /// no business awaiting anything, and the address is not a secret.
    base_url: std::sync::RwLock<Option<String>>,
    /// The address someone is part-way through typing on the pairing screen, so
    /// the link to its profile page can be opened before there is a pairing.
    candidate: RwLock<Option<String>>,
}

impl Shell {
    pub fn new(http: reqwest::Client, pairing: Option<Pairing>) -> Self {
        let base_url = pairing.as_ref().map(|p| p.base_url.clone());
        Self {
            http,
            bridge: Bridge::default(),
            executor: crate::executor::Running::default(),
            read_hashes: std::sync::Mutex::new(std::collections::HashMap::new()),
            session: crate::executor::session_id(),
            pairing: RwLock::new(pairing),
            base_url: std::sync::RwLock::new(base_url),
            candidate: RwLock::new(None),
        }
    }

    /// The instance this client is paired with, without waiting.
    pub fn paired_base_url(&self) -> Option<String> {
        self.base_url.read().unwrap().clone()
    }

    pub async fn pairing(&self) -> Option<Pairing> {
        self.pairing.read().await.clone()
    }

    /// Take up a pairing and bring the socket up on it.
    pub async fn adopt(&self, app: AppHandle, pairing: Pairing) -> Result<()> {
        *self.pairing.write().await = Some(pairing.clone());
        *self.base_url.write().unwrap() = Some(pairing.base_url.clone());
        *self.candidate.write().await = Some(pairing.base_url.clone());
        self.bridge.start(app, self.http.clone(), pairing).await
    }

    pub async fn clear_pairing(&self) {
        *self.pairing.write().await = None;
        *self.base_url.write().unwrap() = None;
    }

    pub async fn remember_candidate(&self, base_url: String) {
        *self.candidate.write().await = Some(base_url);
    }

    pub async fn candidate_instance(&self) -> Option<String> {
        self.candidate.read().await.clone()
    }
}
