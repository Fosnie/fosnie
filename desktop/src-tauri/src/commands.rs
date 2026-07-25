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
//! The list is short on purpose and each entry does one named thing: pair,
//! unpair, learn where it is pointed, put a frame on the socket, open a link in
//! the user's own browser, and — since the client works in folders — choose a
//! folder, connect one, see and undo what was done in it, stop a running
//! command, and show a file in the file manager.
//!
//! What is deliberately absent is as much the point as what is here. There is no
//! "read this file", no "run this command" and no general request proxy: the
//! window cannot ask for work in a folder at all. Those requests arrive on the
//! socket, from the instance, for a conversation bound to a folder somebody
//! connected at this keyboard, and every one that changes anything has been
//! agreed to first.

use serde::Serialize;
use tauri::{AppHandle, State};

use crate::folders::{self, Folder, Tier};
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

// ── Folders on this machine ─────────────────────────────────────────────────

/// Ask the person which folder, using the operating system's own picker.
///
/// Only a path comes back. Nothing in the folder is opened, listed or read here:
/// choosing a folder is not agreeing to anything, and the agreement is the next
/// call.
#[tauri::command]
pub async fn choose_folder(app: AppHandle) -> CmdResult<Option<String>> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().pick_folder(move |picked| {
        let _ = tx.send(picked);
    });
    let picked = rx.await.map_err(|_| "the folder picker was closed".to_string())?;
    Ok(picked.and_then(|p| p.into_path().ok()).map(|p| folders::display(&p)))
}

/// Connect a folder at a level of trust the person has just agreed to: tell the
/// instance, and record it here as what this machine may work in.
#[tauri::command]
pub async fn connect_folder(
    app: AppHandle,
    shell: State<'_, Shell>,
    path: String,
    tier: String,
) -> CmdResult<Folder> {
    let tier = Tier::parse(&tier).ok_or("that is not a level of trust")?;
    let pairing = shell.pairing().await.ok_or("this computer is not paired with an instance")?;
    // Canonicalised here, on the machine that has the folder, so what is recorded
    // and what is checked later are the same thing.
    let canonical = std::fs::canonicalize(&path).map_err(|_| format!("no such folder: {path}"))?;
    let display = folders::display(&canonical);

    let (workspace_id, _stored_path, _tier) = instance::connect_folder(
        &shell.http,
        &pairing.base_url,
        &pairing.token,
        &display,
        tier.as_str(),
    )
    .await
    .map_err(message)?;

    let folder =
        Folder { workspace_id, path: display, tier, base_url: pairing.base_url.clone() };
    folders::remember(&app, folder.clone()).map_err(message)?;
    Ok(folder)
}

/// The folders this machine holds for the instance it is paired with, after
/// checking with the instance which of them still stand.
#[tauri::command]
pub async fn list_folders(app: AppHandle, shell: State<'_, Shell>) -> CmdResult<Vec<Folder>> {
    let Some(pairing) = shell.pairing().await else { return Ok(Vec::new()) };
    if let Some(live) =
        instance::live_workspaces(&shell.http, &pairing.base_url, &pairing.token).await
    {
        let _ = folders::keep_only(&app, &pairing.base_url, &live);
    }
    Ok(folders::list(&app, &pairing.base_url))
}

/// Stop working in a folder on this machine. Withdrawing the grant itself is
/// done on the instance, from wherever the owner happens to be.
#[tauri::command]
pub fn forget_folder(app: AppHandle, workspace_id: String) -> CmdResult<()> {
    folders::forget(&app, &workspace_id).map_err(message)
}

/// What was changed in one turn, so it can be listed and undone.
#[tauri::command]
pub fn turn_changes(app: AppHandle, turn_id: String) -> CmdResult<Vec<crate::backup::Change>> {
    Ok(crate::backup::for_turn(&app, &turn_id))
}

/// Put one file back the way it was. Refuses, unless `force`, when the file has
/// been edited since the agent changed it — a restore then would discard that.
#[tauri::command]
pub fn restore_change(app: AppHandle, id: String, force: bool) -> CmdResult<String> {
    crate::backup::restore_one(&app, &id, force).map_err(message)
}

/// Put back everything one turn changed. Returns how many were restored and how
/// many were left alone because they had been edited since (skipped unless `force`).
#[tauri::command]
pub fn restore_turn(app: AppHandle, turn_id: String, force: bool) -> CmdResult<(usize, usize)> {
    crate::backup::restore_turn(&app, &turn_id, force).map_err(message)
}

/// Stop the command running for a turn. The window knows a command by the turn
/// that asked for it — it never sees the call id — so this is how the stop button
/// beside the output reaches the process. False when nothing is running for it.
#[tauri::command]
pub fn cancel_local_call(shell: State<'_, Shell>, turn_id: String) -> CmdResult<bool> {
    let pids = shell.executor.pids_for_turn(&turn_id);
    let stopped = !pids.is_empty();
    for pid in pids {
        crate::executor::stop(pid);
    }
    Ok(stopped)
}

/// The difference a proposed write would make, computed here because this is
/// where the file is. The instance has never seen its contents and cannot show
/// the change; without this the person would be agreeing to a description of a
/// change rather than to the change.
#[tauri::command]
pub fn preview_change(
    app: AppHandle,
    shell: State<'_, Shell>,
    workspace_id: String,
    path: String,
    new_content: String,
) -> CmdResult<crate::diff::Preview> {
    let base_url = shell.paired_base_url().ok_or("this computer is not paired")?;
    let folder = folders::resolve(&app, &base_url, &workspace_id)
        .ok_or("this computer has no record of that folder")?;
    let target = folders::within(std::path::Path::new(&folder.path), &path, false)
        .map_err(message)?;
    let before = std::fs::read_to_string(&target).ok();
    Ok(crate::diff::preview(before.as_deref(), &new_content))
}

/// Show a file in the operating system's file manager.
///
/// Scoped to a connected folder for the same reason opening a link is scoped to
/// the paired instance: this exists so somebody can find what the agent just
/// wrote, not so that anything rendered in the window can point the system
/// wherever it likes.
#[tauri::command]
pub fn reveal_path(
    app: AppHandle,
    shell: State<'_, Shell>,
    workspace_id: String,
    path: String,
) -> CmdResult<()> {
    let base_url = shell.paired_base_url().ok_or("this computer is not paired")?;
    let folder = folders::resolve(&app, &base_url, &workspace_id)
        .ok_or("this computer has no record of that folder")?;
    let target =
        folders::within(std::path::Path::new(&folder.path), &path, true).map_err(message)?;
    tauri_plugin_opener::reveal_item_in_dir(target).map_err(message)
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
