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

//! The Fosnie desktop client.
//!
//! A window onto an instance. It renders the same application a browser does,
//! holds the socket outside the web view because web views are unreliable at
//! holding one, and keeps the device credential in the operating system's store.
//!
//! It also does one thing a browser cannot: work in a folder on this machine.
//! That reach is narrow by construction, and the shape of the narrowness is
//! worth stating, because "a desktop application that can touch your files" is
//! otherwise a sentence to be nervous about.
//!
//! - It reaches **only** folders somebody chose at this keyboard, through the
//!   system's own picker, and agreed a level of trust for. Nothing in a folder is
//!   read before that agreement.
//! - Every path is resolved against the real filesystem and checked to land
//!   inside that folder, after links are followed — the instance checks the same
//!   thing on the path as written, and this is the check that can see where it
//!   actually leads.
//! - Every write, deletion and command is put in front of the person first, and
//!   every write and deletion is copied aside so it can be put back.
//! - The **window cannot ask for any of it**. The commands it may call are listed
//!   in [`commands`] and none of them reads a file or runs a program; the work
//!   comes from the socket, for a conversation the owner bound to that folder.

pub mod backup;
pub mod commands;
pub mod diff;
pub mod executor;
pub mod folders;
pub mod instance;
pub mod notify;
pub mod state;
pub mod store;
pub mod update;
pub mod ws;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, WindowEvent};

use crate::state::Shell;

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    tauri::Builder::default()
        // A second launch is not a second client: one instance at a time, so two
        // windows cannot hold two sockets for the same account.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            notify::focus_window(app);
        }))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        // The system's folder picker. Reached only from this process's own
        // `choose_folder` command, so the window cannot open a picker of its own
        // — and there is nothing in the capability list beside this file that
        // would let it.
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(notify::PendingChat::default())
        .manage(update::PendingUpdate::default())
        .invoke_handler(tauri::generate_handler![
            commands::shell_info,
            commands::pending_update,
            commands::install_update,
            commands::instance_config,
            commands::validate_instance,
            commands::pair,
            commands::unpair,
            commands::ws_send,
            commands::open_external,
            commands::choose_folder,
            commands::connect_folder,
            commands::list_folders,
            commands::forget_folder,
            commands::turn_changes,
            commands::restore_change,
            commands::restore_turn,
            commands::cancel_local_call,
            commands::preview_change,
            commands::reveal_path,
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            let http = instance::client()?;
            // A pairing that cannot be read is treated as no pairing: the client
            // asks to be paired again rather than refusing to start.
            let pairing = store::load().unwrap_or_else(|e| {
                tracing::warn!(error = %e, "could not read the stored pairing");
                None
            });
            app.manage(Shell::new(http, pairing.clone()));

            if let Some(pairing) = pairing {
                let socket_handle = handle.clone();
                tauri::async_runtime::spawn(async move {
                    let shell = socket_handle.state::<Shell>();
                    let http = shell.http.clone();
                    if let Err(e) = shell.bridge.start(socket_handle.clone(), http, pairing).await {
                        tracing::warn!(error = %e, "could not start the socket");
                    }
                });
            }

            build_tray(app.handle())?;
            register_deep_link(app.handle());
            update::spawn_checks(handle);
            // Copies of files changed weeks ago are of no use to anybody and are
            // somebody's disk. Swept once, at startup, off the path of anything
            // a person is waiting for.
            let sweeping = app.handle().clone();
            tauri::async_runtime::spawn(async move { backup::sweep(&sweeping) });
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window puts the client in the tray rather than ending
            // it: the socket stays up, so an answer still arrives and still
            // notifies. Quit is on the tray menu.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("the desktop client starts");
}

fn build_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show Fosnie", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;

    TrayIconBuilder::new()
        .icon(app.default_window_icon().expect("the client ships an icon").clone())
        .tooltip("Fosnie")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => notify::focus_window(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let tauri::tray::TrayIconEvent::Click {
                button: tauri::tray::MouseButton::Left,
                button_state: tauri::tray::MouseButtonState::Up,
                ..
            } = event
            {
                notify::focus_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

/// The `fosnie://` scheme is registered so that a later sign-in flow has
/// somewhere to come back to. Today a link only brings the window forward: there
/// is nothing yet that it would be right to act on, and a handler that acts on
/// input from any application on the machine is not something to leave lying
/// around unused.
fn register_deep_link(app: &tauri::AppHandle) {
    use tauri_plugin_deep_link::DeepLinkExt;
    // Development builds are not registered with the operating system by an
    // installer, so they ask for the scheme at startup; a failure is not fatal.
    #[cfg(any(windows, target_os = "linux"))]
    if let Err(e) = app.deep_link().register_all() {
        tracing::debug!(error = %e, "the link scheme is not registered for this build");
    }
    let handle = app.clone();
    app.deep_link().on_open_url(move |event| {
        tracing::info!(urls = ?event.urls(), "opened by link");
        notify::focus_window(&handle);
    });
}

