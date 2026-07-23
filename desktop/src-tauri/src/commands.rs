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

//! Everything the web view is allowed to ask this process to do.
//!
//! The list is short on purpose, and it is the whole of it: pair, unpair, learn
//! where it is pointed, put a frame on the socket, open a link in the user's own
//! browser. There is no file access, no command execution, and no general
//! request proxy — this client cannot reach the machine it runs on, and adding
//! that reach would be a deliberate change to this file and to the capability
//! list beside it.

use serde::Serialize;
use tauri::{AppHandle, State};

use crate::instance;
use crate::state::Shell;
use crate::store::{self, Pairing};

/// A command failure as the application sees it: a sentence to show someone.
type CmdResult<T> = std::result::Result<T, String>;

fn message(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// What the application needs to address the instance: where it is and the
/// device token to present.
///
/// This is the one crossing the token makes, once per start, into the
/// application's memory. It is never written to browser storage, and the
/// application asks again after every restart.
#[derive(Serialize)]
pub struct InstanceConfig {
    pub base_url: String,
    pub token: String,
}

/// What this client is, for the About panel. The instance reports its own
/// version over the socket; the two are released separately and are both worth
/// showing when someone is describing a problem.
#[derive(Serialize)]
pub struct ShellInfo {
    pub app_version: &'static str,
    pub platform: &'static str,
}

#[tauri::command]
pub fn shell_info() -> ShellInfo {
    ShellInfo { app_version: env!("CARGO_PKG_VERSION"), platform: crate::state::PLATFORM }
}

/// The release waiting to be installed, if the client has already fetched one.
///
/// Asked at startup: the check runs the moment the client does, and can finish
/// before the window is listening for the announcement.
#[tauri::command]
pub fn pending_update(app: AppHandle) -> Option<crate::update::UpdateReady> {
    crate::update::pending(&app)
}

/// Install the release that has already been downloaded and verified, and
/// restart into it. Called only after the person has agreed to it.
#[tauri::command]
pub fn install_update(app: AppHandle) -> CmdResult<()> {
    crate::update::install(&app)
}

#[tauri::command]
pub async fn instance_config(shell: State<'_, Shell>) -> CmdResult<Option<InstanceConfig>> {
    let pairing = shell.pairing().await;
    Ok(pairing.map(|p| InstanceConfig { base_url: p.base_url, token: p.token }))
}

/// Check an address before anyone types a code into it, so a typo or an old
/// release is named as such rather than surfacing as a failed pairing.
#[tauri::command]
pub async fn validate_instance(
    shell: State<'_, Shell>,
    url: String,
) -> CmdResult<instance::InstanceInfo> {
    let info = instance::validate(&shell.http, &url).await.map_err(message)?;
    // Held so the "open my profile page" link on the second pairing step has
    // somewhere to point before a pairing exists.
    shell.remember_candidate(info.base_url.clone()).await;
    Ok(info)
}

/// Redeem a pairing code minted from a signed-in web session. On success the
/// credential goes to the operating system's store and the socket comes up.
#[tauri::command]
pub async fn pair(
    app: AppHandle,
    shell: State<'_, Shell>,
    url: String,
    code: String,
) -> CmdResult<InstanceConfig> {
    let info = instance::validate(&shell.http, &url).await.map_err(message)?;
    let (device_id, token) = instance::pair(
        &shell.http,
        &info.base_url,
        &normalise_code(&code),
        &crate::state::device_name(),
        crate::state::PLATFORM,
    )
    .await
    .map_err(message)?;

    let pairing = Pairing { base_url: info.base_url, token, device_id: Some(device_id) };
    store::save(&pairing).map_err(message)?;
    shell.adopt(app, pairing.clone()).await.map_err(message)?;
    Ok(InstanceConfig { base_url: pairing.base_url, token: pairing.token })
}

/// Sign this machine out: close the socket, ask the instance to withdraw the
/// device, and clear the credential store. The local half happens whether or not
/// the instance is reachable.
#[tauri::command]
pub async fn unpair(shell: State<'_, Shell>) -> CmdResult<()> {
    shell.bridge.stop().await;
    if let Some(p) = shell.pairing().await {
        if let Some(device_id) = &p.device_id {
            instance::revoke_self(&shell.http, &p.base_url, &p.token, device_id).await;
        }
    }
    shell.clear_pairing().await;
    store::forget().map_err(message)
}

/// Put a frame the application composed on the socket, exactly as given.
#[tauri::command]
pub async fn ws_send(shell: State<'_, Shell>, frame: String) -> CmdResult<bool> {
    Ok(shell.bridge.send(frame).await)
}

/// Open a link in the user's own browser.
///
/// Restricted to the paired instance: this exists so someone can reach their
/// profile page to mint a pairing code, not so that content rendered in the
/// window can send the operating system anywhere it likes.
#[tauri::command]
pub async fn open_external(shell: State<'_, Shell>, url: String) -> CmdResult<()> {
    let allowed = match shell.pairing().await {
        Some(p) => p.base_url,
        // Before pairing there is one thing worth opening: the instance the user
        // is in the middle of naming.
        None => shell.candidate_instance().await.ok_or("no instance has been chosen yet")?,
    };
    if !is_within(&url, &allowed) {
        return Err("that link does not belong to the connected instance".into());
    }
    tauri_plugin_opener::open_url(url, None::<&str>).map_err(message)
}

/// Whether `url` addresses `base` (same scheme, host and port). Compared on the
/// parsed origin rather than as text, so a prefix like
/// `https://ai.example.com.attacker.test` cannot pass for the instance.
fn is_within(url: &str, base: &str) -> bool {
    let (Ok(url), Ok(base)) = (url::Url::parse(url), url::Url::parse(base)) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    url.scheme() == base.scheme() && url.host() == base.host() && url.port() == base.port()
}

/// Fold a code as it was read off one screen and typed into another: people
/// group it, lower-case it, and paste stray spaces.
fn normalise_code(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_folded_the_way_the_instance_folds_them() {
        assert_eq!(normalise_code(" k7m2-qx4d "), "K7M2QX4D");
        assert_eq!(normalise_code("K7M2QX4D"), "K7M2QX4D");
    }

    #[test]
    fn only_the_paired_instance_may_be_opened() {
        let base = "https://ai.example.com";
        assert!(is_within("https://ai.example.com/profile", base));
        assert!(is_within("https://ai.example.com", base));
        // A look-alike host, a different scheme, and anything that is not a web
        // address at all.
        assert!(!is_within("https://ai.example.com.attacker.test/profile", base));
        assert!(!is_within("http://ai.example.com/profile", base));
        assert!(!is_within("https://elsewhere.test", base));
        assert!(!is_within("file:///C:/Windows/System32", base));
        assert!(!is_within("not a url", base));
    }

    #[test]
    fn a_port_is_part_of_the_instance() {
        assert!(is_within("http://localhost:8080/profile", "http://localhost:8080"));
        assert!(!is_within("http://localhost:9000/profile", "http://localhost:8080"));
    }
}
