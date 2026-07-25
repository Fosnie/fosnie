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

//! The count of approvals waiting on this client, shown on the taskbar.
//!
//! An agent that has stopped to ask permission is easy to miss when the window is
//! not in front of you. The window already raises a notification; this keeps a
//! running tally on the taskbar and the tray so a waiting decision is visible at a
//! glance and stays visible until it is dealt with.
//!
//! The tally is the set of runs currently awaiting a decision, and it is kept
//! here, from the socket the core already reads: an `agent.approval` opens one and
//! an `agent.approval.resolved` — sent to every one of the user's clients when the
//! gate is decided anywhere — closes it. It is scoped to this run of the client
//! and reset whenever the socket reconnects, so a decision taken while the client
//! was away does not leave a number stuck on the taskbar.
//!
//! On Windows the taskbar has no numeric badge, so a small dot is drawn over the
//! icon instead; elsewhere the platform's own badge count is used.

use std::collections::HashSet;
use std::sync::Mutex;

use fosnie_protocol::ServerFrame;
use tauri::tray::TrayIcon;
use tauri::{AppHandle, Manager};
use uuid::Uuid;

/// Every approval and its resolution carry this in their `type`, so a stream of
/// tokens is rejected without parsing. The resolution's type extends it
/// (`agent.approval.resolved`), so the one needle catches both.
const MARKER: &str = "\"type\":\"agent.approval";

/// The runs awaiting a decision on this client, and the tray whose tooltip mirrors
/// the count. Registered once at startup.
#[derive(Default)]
pub struct Badge {
    open: Mutex<HashSet<Uuid>>,
    tray: Mutex<Option<TrayIcon>>,
}

/// Keep the tray so its tooltip can be updated as the count changes.
pub fn set_tray(app: &AppHandle, tray: TrayIcon) {
    if let Some(b) = app.try_state::<Badge>() {
        *b.tray.lock().unwrap() = Some(tray);
    }
}

/// Forget every waiting approval and clear the taskbar. Called when the socket
/// (re)connects: the set is only ever a picture of the current session.
pub fn reset(app: &AppHandle) {
    let Some(b) = app.try_state::<Badge>() else { return };
    b.open.lock().unwrap().clear();
    apply(app, &b, 0);
}

/// Update the count from one socket frame, if it is an approval opening or
/// closing. Anything else is ignored cheaply.
pub fn consider(app: &AppHandle, raw: &str) {
    if !raw.contains(MARKER) {
        return;
    }
    let Some(b) = app.try_state::<Badge>() else { return };
    let Ok(frame) = serde_json::from_str::<ServerFrame>(raw) else { return };
    let n = {
        let mut open = b.open.lock().unwrap();
        match frame {
            ServerFrame::AgentApproval { run_id, .. } => {
                open.insert(run_id);
            }
            // A resolution for a run this client never saw open (it was decided on
            // another device before this one connected) is a no-op, not an error.
            ServerFrame::AgentApprovalResolved { run_id, .. } => {
                open.remove(&run_id);
            }
            _ => return,
        }
        open.len()
    };
    apply(app, &b, n);
}

/// Put the count on the tray tooltip and the taskbar.
fn apply(app: &AppHandle, badge: &Badge, n: usize) {
    if let Some(tray) = badge.tray.lock().unwrap().as_ref() {
        let tip = if n == 0 {
            "Fosnie".to_string()
        } else {
            format!("{n} approval{} waiting", if n == 1 { "" } else { "s" })
        };
        let _ = tray.set_tooltip(Some(tip));
    }

    let Some(win) = app.get_webview_window("main") else { return };
    #[cfg(target_os = "windows")]
    {
        // Windows has no numeric taskbar badge; a dot over the icon is the
        // convention. The pixels live for the length of the call.
        if n == 0 {
            let _ = win.set_overlay_icon(None);
        } else {
            let rgba = dot_rgba();
            let _ = win.set_overlay_icon(Some(tauri::image::Image::new(&rgba, DOT_SIZE, DOT_SIZE)));
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = win.set_badge_count(if n == 0 { None } else { Some(n as i64) });
    }
}

#[cfg(target_os = "windows")]
const DOT_SIZE: u32 = 32;

/// A filled dot, red, on transparency: the overlay drawn on the taskbar icon when
/// an approval is waiting. Built in code so there is no asset to ship or find.
#[cfg(target_os = "windows")]
fn dot_rgba() -> Vec<u8> {
    let s = DOT_SIZE as i32;
    let mut px = vec![0u8; (s * s * 4) as usize];
    let centre = (s as f32 - 1.0) / 2.0;
    let radius = s as f32 * 0.44;
    for y in 0..s {
        for x in 0..s {
            let dx = x as f32 - centre;
            let dy = y as f32 - centre;
            if dx * dx + dy * dy <= radius * radius {
                let i = ((y * s + x) * 4) as usize;
                px[i] = 214; // r
                px[i + 1] = 69; // g
                px[i + 2] = 69; // b
                px[i + 3] = 255; // a
            }
        }
    }
    px
}

#[cfg(test)]
mod tests {
    use super::*;

    // The set logic, exercised without a running app: the numbers a sequence of
    // approvals and resolutions should leave on the taskbar.
    fn count(open: &Mutex<HashSet<Uuid>>) -> usize {
        open.lock().unwrap().len()
    }

    fn id(n: u8) -> Uuid {
        Uuid::from_bytes([n; 16])
    }

    #[test]
    fn opens_and_closes_track_the_waiting_set() {
        let open: Mutex<HashSet<Uuid>> = Mutex::new(HashSet::new());
        open.lock().unwrap().insert(id(1));
        assert_eq!(count(&open), 1);
        open.lock().unwrap().insert(id(2));
        assert_eq!(count(&open), 2);
        open.lock().unwrap().remove(&id(1));
        assert_eq!(count(&open), 1);
        // Removing the same run again is idempotent, not a negative count.
        open.lock().unwrap().remove(&id(1));
        assert_eq!(count(&open), 1);
        // A resolution for a run never seen open changes nothing.
        open.lock().unwrap().remove(&id(9));
        assert_eq!(count(&open), 1);
        // A reconnect wipes the session's picture.
        open.lock().unwrap().clear();
        assert_eq!(count(&open), 0);
    }
}
