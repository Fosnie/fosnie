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

//! Keeping the client current, without taking the decision away from the person
//! using it.
//!
//! Downloading is quiet: it happens as soon as a release is found, in the
//! background, and costs the user nothing but bandwidth. Installing is not:
//! replacing the application someone is working in restarts it, so the release
//! is fetched and verified first and only then is the question asked. Declining
//! leaves the running version exactly as it was and the offer returns with the
//! next day's check.
//!
//! The download is verified against the signing key compiled into this build
//! before it is offered at all. An update that does not verify is not an update.

use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

/// A release has been fetched, verified, and is waiting for a yes.
pub const EVENT_UPDATE_READY: &str = "shell:update-ready";

/// How often the client looks for a new release of itself, after the check it
/// makes at startup.
const INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// The downloaded release, held until it is installed or the client exits.
///
/// One slot: there is only ever a newest version worth installing, and a second
/// find replaces the first rather than queueing behind it.
#[derive(Default)]
pub struct PendingUpdate(Mutex<Option<(Update, Vec<u8>)>>);

/// What the window is told when an update is ready.
#[derive(Clone, Serialize)]
pub struct UpdateReady {
    pub version: String,
    /// The release notes from the manifest, when it carries any.
    pub notes: Option<String>,
}

/// Check now, and once a day from here on.
pub fn spawn_checks(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            check(&app).await;
            tokio::time::sleep(INTERVAL).await;
        }
    });
}

/// Look for a release; if there is one, fetch it and offer it.
///
/// Every failure here is a non-event: no network, no manifest, a download that
/// breaks off. The client goes on working and asks again tomorrow. The one thing
/// that must not happen is announcing an update that then cannot be installed,
/// so the slot is only filled once the bytes are in hand and verified.
async fn check(app: &AppHandle) {
    let updater = match app.updater() {
        Ok(updater) => updater,
        Err(e) => {
            tracing::debug!(error = %e, "no update channel is configured");
            return;
        }
    };
    let update = match updater.check().await {
        Ok(Some(update)) => update,
        Ok(None) => return,
        Err(e) => {
            tracing::debug!(error = %e, "could not check for updates");
            return;
        }
    };

    let version = update.version.clone();
    let notes = update.body.clone();
    tracing::info!(%version, "downloading an update");

    let bytes = match update.download(|_, _| {}, || {}).await {
        Ok(bytes) => bytes,
        Err(e) => {
            // Includes a failed signature check, which is the interesting case:
            // something was served that this build will not run, and the right
            // response is to carry on with the version already installed.
            tracing::warn!(error = %e, %version, "the update was not usable");
            clear(app);
            return;
        }
    };

    if let Some(state) = app.try_state::<PendingUpdate>() {
        if let Ok(mut slot) = state.0.lock() {
            *slot = Some((update, bytes));
        }
    }
    tracing::info!(%version, "an update is ready to install");
    let _ = app.emit(EVENT_UPDATE_READY, UpdateReady { version, notes });
}

/// The release waiting to be installed, if there is one.
///
/// The window asks this at startup because the announcement above can happen
/// before there is anything listening: the client checks as soon as it starts,
/// and a fast answer beats the window's first render. An event that arrives
/// early is simply lost, so the state it announced has to be readable too.
pub fn pending(app: &AppHandle) -> Option<UpdateReady> {
    let state = app.try_state::<PendingUpdate>()?;
    let slot = state.0.lock().ok()?;
    let (update, _) = slot.as_ref()?;
    Some(UpdateReady { version: update.version.clone(), notes: update.body.clone() })
}

fn clear(app: &AppHandle) {
    if let Some(state) = app.try_state::<PendingUpdate>() {
        if let Ok(mut slot) = state.0.lock() {
            *slot = None;
        }
    }
}

/// Install the release that is waiting, and come back on the new version.
///
/// Called only after the person has said yes. On Windows the installer takes the
/// process over, so `install` may never return; where it does return, the client
/// restarts itself so nobody is left looking at the version they just replaced.
pub fn install(app: &AppHandle) -> Result<(), String> {
    let pending = app
        .try_state::<PendingUpdate>()
        .and_then(|state| state.0.lock().ok().and_then(|mut slot| slot.take()));

    let Some((update, bytes)) = pending else {
        return Err("there is no update waiting to be installed".into());
    };

    update.install(bytes).map_err(|e| {
        tracing::warn!(error = %e, "could not install the update");
        format!("The update could not be installed: {e}")
    })?;
    app.restart();
}
