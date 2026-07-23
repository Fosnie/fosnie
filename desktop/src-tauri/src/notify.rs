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

//! System notifications, kept deliberately rare.
//!
//! Two things are worth interrupting someone for: an answer they are waiting on
//! has finished, and an agent has stopped to ask their permission. Everything
//! else on the socket — every token, every tool step, every progress line — is
//! the application's business and stays in the window.
//!
//! Nothing is raised while the window has focus. The user is already looking at
//! it, and a notification for something visible on screen is noise that teaches
//! people to dismiss the ones that matter.
//!
//! ## Clicking one
//!
//! On Windows a click raises the window on the chat the notification was about.
//! That is done by building the toast here rather than through the notification
//! plugin, whose desktop path discards the handle a click would arrive on. On
//! macOS and Linux the plugin is used as it is, and a notification informs
//! without being clickable: the window is raised from the tray or the dock.

use fosnie_protocol::ServerFrame;
use tauri::{AppHandle, Emitter, Manager};

use crate::store::Pairing;

/// Asks the application to open a chat, after a notification for it was clicked.
pub const EVENT_OPEN_CHAT: &str = "shell:open-chat";

/// The two frame types worth a notification, as they appear on the wire.
///
/// Checked as text before anything is parsed. A turn streams hundreds of frames
/// and all but the last of them are tokens; paying for a full deserialisation of
/// each one to discard it is work the socket does not need to do. Anything that
/// gets past this is still parsed properly below — this only rejects.
const NOTIFIABLE: [&str; 2] = ["\"type\":\"chat.completed\"", "\"type\":\"agent.approval\""];

/// Raise a notification for this frame if it deserves one and nobody is watching.
pub async fn consider(
    app: &AppHandle,
    http: &reqwest::Client,
    pairing: &Pairing,
    raw_frame: &str,
) {
    if !NOTIFIABLE.iter().any(|marker| raw_frame.contains(marker)) {
        return;
    }
    if window_is_focused(app) {
        return;
    }
    let Ok(frame) = serde_json::from_str::<ServerFrame>(raw_frame) else {
        return;
    };
    let (chat_id, body) = match frame {
        ServerFrame::ChatCompleted { chat_id, .. } => (chat_id.to_string(), "Your answer is ready."),
        ServerFrame::AgentApproval { .. } => {
            // An approval names a run, not a chat, so there is nothing to open;
            // the notification brings the window forward and the request is
            // waiting in it.
            show(app, "Approval needed", "An agent is waiting for your decision.", None);
            return;
        }
        _ => return,
    };

    let title = instance_chat_title(http, pairing, &chat_id).await;
    show(app, &title, body, Some(chat_id));
}

async fn instance_chat_title(http: &reqwest::Client, pairing: &Pairing, chat_id: &str) -> String {
    crate::instance::chat_title(http, &pairing.base_url, &pairing.token, chat_id)
        .await
        .unwrap_or_else(|| "Fosnie".to_string())
}

fn window_is_focused(app: &AppHandle) -> bool {
    app.get_webview_window("main").and_then(|w| w.is_focused().ok()).unwrap_or(false)
}

/// The chat a click should open. One slot: only the most recent notification is
/// worth acting on.
#[derive(Default)]
pub struct PendingChat(pub std::sync::Mutex<Option<String>>);

fn set_pending_chat(app: &AppHandle, chat_id: String) {
    if let Some(slot) = app.try_state::<PendingChat>() {
        if let Ok(mut held) = slot.0.lock() {
            *held = Some(chat_id);
        }
    }
}

fn take_pending_chat(app: &AppHandle) -> Option<String> {
    let slot = app.try_state::<PendingChat>()?;
    let mut held = slot.0.lock().ok()?;
    held.take()
}

/// Show one notification, remembering what a click on it should open.
fn show(app: &AppHandle, title: &str, body: &str, chat_id: Option<String>) {
    if let Some(chat_id) = chat_id {
        set_pending_chat(app, chat_id);
    }
    if let Err(e) = platform::show(app, title, body) {
        tracing::debug!(error = %e, "could not show a notification");
    }
}

/// Bring the window forward, and open the chat the last notification was about.
/// Also the single-instance and tray "Show" path.
pub fn focus_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
    if let Some(chat_id) = take_pending_chat(app) {
        let _ = app.emit(EVENT_OPEN_CHAT, chat_id);
    }
}

#[cfg(windows)]
mod platform {
    use tauri::AppHandle;
    use tauri_winrt_notification::Toast;

    /// Windows toasts are addressed to an application identity. An installed
    /// client has one, registered by its installer; a build run straight out of
    /// `target/` does not, so it borrows the shell's — the same rule the
    /// notification plugin follows, and the reason a development toast is
    /// attributed to Windows PowerShell.
    fn app_id() -> String {
        let installed = tauri::utils::platform::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.display().to_string()))
            .map(|dir| !(dir.ends_with("\\target\\debug") || dir.ends_with("\\target\\release")))
            .unwrap_or(false);
        if installed {
            "dev.fosnie.desktop".to_string()
        } else {
            Toast::POWERSHELL_APP_ID.to_string()
        }
    }

    /// Built here rather than through the notification plugin: the plugin's
    /// desktop path drops the handle that a click would be delivered on, so a
    /// notification sent through it can inform but can never lead anywhere.
    pub fn show(app: &AppHandle, title: &str, body: &str) -> Result<(), String> {
        let handle = app.clone();
        Toast::new(&app_id())
            .title(title)
            .text1(body)
            .on_activated(move |_| {
                crate::notify::focus_window(&handle);
                Ok(())
            })
            .show()
            .map_err(|e| e.to_string())
    }
}

#[cfg(not(windows))]
mod platform {
    use tauri::AppHandle;
    use tauri_plugin_notification::NotificationExt;

    /// The plugin's own path. It cannot report a click — its desktop
    /// implementation discards the handle one would arrive on — so on these
    /// platforms a notification tells the user and stops there.
    pub fn show(app: &AppHandle, title: &str, body: &str) -> Result<(), String> {
        app.notification()
            .builder()
            .title(title)
            .body(body)
            .show()
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fosnie_protocol::ServerFrame;
    use uuid::Uuid;

    #[test]
    fn a_token_stream_is_rejected_before_it_is_parsed() {
        let token = ServerFrame::ChatToken { turn_id: Uuid::nil(), delta: "hi".into() }.to_json();
        assert!(!NOTIFIABLE.iter().any(|m| token.contains(m)));
    }

    #[test]
    fn the_two_frames_worth_interrupting_for_get_through() {
        let completed = ServerFrame::ChatCompleted {
            turn_id: Uuid::nil(),
            chat_id: Uuid::nil(),
            message_id: Uuid::nil(),
            reasoning_tokens: None,
        }
        .to_json();
        let approval = ServerFrame::AgentApproval {
            run_id: Uuid::nil(),
            turn_id: Uuid::nil(),
            tool: "web_search".into(),
            summary: "".into(),
            args: serde_json::Value::Null,
        }
        .to_json();
        for frame in [completed, approval] {
            assert!(NOTIFIABLE.iter().any(|m| frame.contains(m)), "rejected {frame}");
        }
    }
}
