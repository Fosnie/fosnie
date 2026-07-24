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

//! Who this process says it is to Windows.
//!
//! Windows addresses a running program by an application identity, and uses it
//! for two things that are otherwise inexplicable: which taskbar button the
//! windows group under, and whose name and icon a notification carries. A
//! program that never declares one is guessed at — which is how a notification
//! from this client used to arrive wearing Windows PowerShell's name.
//!
//! Declaring it is two steps, and both are needed. The process announces the
//! identity before it opens a window, and the identity is described once in the
//! current user's registry so the notification centre has a name and an icon to
//! show for it. The registry half is written under `HKEY_CURRENT_USER`, which
//! needs no elevation and belongs to the person running the client, so it is
//! safe to reassert on every start; that also means a build run straight out of
//! `target/` is attributed properly rather than borrowing another program's
//! identity.
//!
//! Everything here is Windows-only. macOS and Linux take an application's
//! identity from its bundle and its desktop entry, and have nothing to declare.

/// The identity this client announces to Windows.
///
/// A development build uses a distinct one so that running from `target/` never
/// overwrites the description an installed client registered for itself, and so
/// that a toast on a developer's machine is honestly labelled.
pub const APP_ID: &str =
    if cfg!(debug_assertions) { "dev.fosnie.desktop.dev" } else { "dev.fosnie.desktop" };

/// The name shown on notifications from this client.
#[cfg(windows)]
const DISPLAY_NAME: &str = if cfg!(debug_assertions) { "Fosnie (development)" } else { "Fosnie" };

/// Announce the identity to Windows.
///
/// Called before any window exists: the taskbar reads it when the first window
/// is created, and a later call would not regroup what is already there.
#[cfg(windows)]
pub fn announce() {
    use windows::core::HSTRING;
    use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;

    let id = HSTRING::from(APP_ID);
    // Failure costs the grouping and the toast's name, not the client.
    if let Err(e) = unsafe { SetCurrentProcessExplicitAppUserModelID(&id) } {
        tracing::debug!(error = %e, "the application identity was not accepted");
    }
}

#[cfg(not(windows))]
pub fn announce() {}

/// Describe the identity so notifications from it carry this client's name and
/// icon.
///
/// The icon has to be a file the notification centre can open, so the icon
/// built into this binary is written next to the client's own data and pointed
/// at. It is rewritten whenever it differs, which keeps a client that has been
/// updated from showing the icon it shipped with a year ago.
#[cfg(windows)]
pub fn describe(app: &tauri::AppHandle) {
    use tauri::Manager;

    const ICON: &[u8] = include_bytes!("../icons/icon.ico");

    let icon = match app.path().app_data_dir() {
        Ok(dir) => {
            let path = dir.join("icon.ico");
            let current = std::fs::read(&path).ok();
            if current.as_deref() != Some(ICON) {
                if let Err(e) = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, ICON)) {
                    tracing::debug!(error = %e, "the notification icon could not be written");
                    return;
                }
            }
            path
        }
        Err(e) => {
            tracing::debug!(error = %e, "there is nowhere to keep the notification icon");
            return;
        }
    };

    if let Err(e) = write_registration(&icon) {
        tracing::debug!(error = %e, "the application identity was not described");
    }
}

#[cfg(not(windows))]
pub fn describe(_app: &tauri::AppHandle) {}

#[cfg(windows)]
fn write_registration(icon: &std::path::Path) -> std::io::Result<()> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
    use winreg::RegKey;

    let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
        .create_subkey_with_flags(format!("Software\\Classes\\AppUserModelId\\{APP_ID}"), KEY_SET_VALUE)?;
    key.set_value("DisplayName", &DISPLAY_NAME)?;
    key.set_value("IconUri", &icon.display().to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_development_build_does_not_answer_to_the_installed_identity() {
        // The two must never coincide: a development run reasserting the
        // description would repoint an installed client's notifications at a
        // binary in somebody's `target/` directory.
        assert_eq!(cfg!(debug_assertions), APP_ID.ends_with(".dev"));
    }
}
