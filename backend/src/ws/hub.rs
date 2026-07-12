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

//! Process-local socket registry: per-socket outbound senders + per-turn cancel
//! signals. Single process, so fan-out is in-memory; shared session state
//! (resume/presence) lives in Redis ([`super::session`]), ready for a
//! multi-process split later.
//!
//! Cancel is graceful via [`Notify`]: the turn task selects on it, persists the
//! partial assistant message, and drops its generation stream (which cancels
//! the LLM upstream). We never `abort()` the task, so cleanup always runs.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, Notify};
use uuid::Uuid;

use super::protocol::ServerFrame;

#[derive(Clone, Default)]
pub struct Hub {
    inner: Arc<Mutex<HashMap<Uuid, SocketEntry>>>,
}

struct SocketEntry {
    user_id: Uuid,
    tx: mpsc::Sender<ServerFrame>,
    turns: HashMap<Uuid, Arc<Notify>>,
}

impl Hub {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, socket_id: Uuid, user_id: Uuid, tx: mpsc::Sender<ServerFrame>) {
        let mut guard = self.inner.lock().unwrap();
        guard.insert(
            socket_id,
            SocketEntry {
                user_id,
                tx,
                turns: HashMap::new(),
            },
        );
        metrics::gauge!("ws_connections").set(guard.len() as f64);
    }

    /// Remove a socket and return its turns' cancel signals, so the caller can
    /// notify each (auto-cancel on disconnect).
    pub fn deregister(&self, socket_id: Uuid) -> Vec<Arc<Notify>> {
        let mut guard = self.inner.lock().unwrap();
        let out = match guard.remove(&socket_id) {
            Some(entry) => entry.turns.into_values().collect(),
            None => Vec::new(),
        };
        metrics::gauge!("ws_connections").set(guard.len() as f64);
        out
    }

    pub fn add_turn(&self, socket_id: Uuid, turn_id: Uuid, cancel: Arc<Notify>) {
        if let Some(e) = self.inner.lock().unwrap().get_mut(&socket_id) {
            e.turns.insert(turn_id, cancel);
        }
    }

    pub fn remove_turn(&self, socket_id: Uuid, turn_id: Uuid) {
        if let Some(e) = self.inner.lock().unwrap().get_mut(&socket_id) {
            e.turns.remove(&turn_id);
        }
    }

    /// Best-effort push of a frame to every socket a user has open. Used by
    /// background work (e.g. the tabular generator) to notify a user outside the
    /// chat-turn path. Postgres remains the source of truth; a dropped frame
    /// (full/closed socket) is fine — the client can re-fetch.
    pub fn send_to_user(&self, user_id: Uuid, frame: ServerFrame) {
        let guard = self.inner.lock().unwrap();
        for entry in guard.values() {
            if entry.user_id == user_id {
                let _ = entry.tx.try_send(frame.clone());
            }
        }
    }

    /// Push a cache-invalidation hint to a set of users (deduped by socket scan).
    /// After a write, their open clients refetch the given React-Query keys so views
    /// refresh without a reload. Best-effort like [`Hub::send_to_user`].
    pub fn send_invalidate(&self, recipients: &[Uuid], keys: Vec<Vec<String>>) {
        if recipients.is_empty() || keys.is_empty() {
            return;
        }
        let frame = ServerFrame::Invalidate { keys };
        let guard = self.inner.lock().unwrap();
        for entry in guard.values() {
            if recipients.contains(&entry.user_id) {
                let _ = entry.tx.try_send(frame.clone());
            }
        }
    }

    /// Push a cache-invalidation hint to EVERY connected socket (one scan), for
    /// platform-wide changes (announcement banners / welcome message). Best-effort
    /// like [`Hub::send_invalidate`]. NOTE: process-local (see module header) — in
    /// a multi-process split this reaches only this process's sockets, so the
    /// client's react-query `refetchOnMount` is the backstop, not this push.
    pub fn broadcast_invalidate(&self, keys: Vec<Vec<String>>) {
        if keys.is_empty() {
            return;
        }
        let frame = ServerFrame::Invalidate { keys };
        let guard = self.inner.lock().unwrap();
        for entry in guard.values() {
            let _ = entry.tx.try_send(frame.clone());
        }
    }

    /// Force-close every socket a user has open (e.g. on deactivation).
    /// Removing the entry drops its `tx`, which ends the writer task → the sink
    /// is dropped → the WebSocket closes and the reader loop exits. In-flight
    /// turns are cancelled so their tasks persist their partial and clean up.
    pub fn close_user(&self, user_id: Uuid) {
        let mut guard = self.inner.lock().unwrap();
        let socket_ids: Vec<Uuid> = guard
            .iter()
            .filter(|(_, e)| e.user_id == user_id)
            .map(|(id, _)| *id)
            .collect();
        for id in socket_ids {
            if let Some(entry) = guard.remove(&id) {
                for cancel in entry.turns.into_values() {
                    cancel.notify_one();
                }
                // entry.tx dropped here → writer task ends → socket closes.
            }
        }
        metrics::gauge!("ws_connections").set(guard.len() as f64);
    }

    /// Signal a specific turn to cancel. Returns false if not found.
    pub fn cancel_turn(&self, socket_id: Uuid, turn_id: Uuid) -> bool {
        if let Some(e) = self.inner.lock().unwrap().get_mut(&socket_id) {
            if let Some(cancel) = e.turns.remove(&turn_id) {
                cancel.notify_one();
                return true;
            }
        }
        false
    }
}
