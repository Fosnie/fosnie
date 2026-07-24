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
    /// The paired machine this socket belongs to, when it authenticated as one.
    /// Lets a frame be addressed to a particular device rather than to every
    /// socket the user has open.
    device_id: Option<Uuid>,
    tx: mpsc::Sender<ServerFrame>,
    turns: HashMap<Uuid, Arc<Notify>>,
}

impl Hub {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        &self,
        socket_id: Uuid,
        user_id: Uuid,
        device_id: Option<Uuid>,
        tx: mpsc::Sender<ServerFrame>,
    ) {
        let mut guard = self.inner.lock().unwrap();
        guard.insert(
            socket_id,
            SocketEntry {
                user_id,
                device_id,
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

    /// Push a frame to one paired machine of one user, choosing its freshest live
    /// socket. Returns whether it was handed off. A `false` means the machine is
    /// not connected (or its socket is wedged): the caller treats that as "nothing
    /// was delivered" and acts accordingly, rather than assuming success.
    ///
    /// The user scope is not optional: a device only ever belongs to its owner, so
    /// a frame addressed to `(user, device)` can never reach anyone else's socket
    /// even if two accounts somehow named the same device id.
    ///
    /// Socket ids are time-ordered (`Uuid::now_v7`), so the greatest id is the most
    /// recent connection: after a client restart the newest socket wins and a
    /// stale, about-to-close one is not chosen.
    pub fn send_to_device(&self, user_id: Uuid, device_id: Uuid, frame: ServerFrame) -> bool {
        let guard = self.inner.lock().unwrap();
        guard
            .iter()
            .filter(|(_, e)| e.user_id == user_id && e.device_id == Some(device_id))
            .max_by_key(|(sid, _)| **sid)
            .map(|(_, e)| e.tx.try_send(frame).is_ok())
            .unwrap_or(false)
    }

    /// Whether one user's paired machine has a live socket right now. Used to
    /// decide, before a scheduled job tries to reach a machine, whether it is
    /// there to be reached at all.
    pub fn is_device_online(&self, user_id: Uuid, device_id: Uuid) -> bool {
        self.inner
            .lock()
            .unwrap()
            .values()
            .any(|e| e.user_id == user_id && e.device_id == Some(device_id))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> ServerFrame {
        ServerFrame::Pong
    }

    // Time-ordered ids: a later socket must sort greater so the freshest wins.
    fn later_than(a: Uuid) -> Uuid {
        loop {
            let b = Uuid::now_v7();
            if b > a {
                return b;
            }
        }
    }

    #[test]
    fn device_route_is_scoped_to_owner_and_device() {
        let hub = Hub::new();
        let owner = Uuid::now_v7();
        let stranger = Uuid::now_v7();
        let device = Uuid::now_v7();

        let (tx_owner, mut rx_owner) = mpsc::channel(4);
        let (tx_stranger, mut rx_stranger) = mpsc::channel(4);
        // Same device id, different owner: the stranger must never be reachable.
        hub.register(Uuid::now_v7(), owner, Some(device), tx_owner);
        hub.register(Uuid::now_v7(), stranger, Some(device), tx_stranger);

        assert!(hub.send_to_device(owner, device, frame()));
        assert!(hub.is_device_online(owner, device));
        // The stranger's socket got nothing, and the wrong owner is not "online".
        assert!(rx_owner.try_recv().is_ok());
        assert!(rx_stranger.try_recv().is_err());

        // A device the owner does not have, and the owner's device under nobody.
        assert!(!hub.send_to_device(owner, Uuid::now_v7(), frame()));
        assert!(!hub.is_device_online(Uuid::now_v7(), device));
    }

    #[test]
    fn device_route_picks_the_freshest_socket() {
        let hub = Hub::new();
        let owner = Uuid::now_v7();
        let device = Uuid::now_v7();

        let old_id = Uuid::now_v7();
        let new_id = later_than(old_id);
        let (tx_old, mut rx_old) = mpsc::channel(4);
        let (tx_new, mut rx_new) = mpsc::channel(4);
        // Register the newer one first to prove selection is by id, not insertion.
        hub.register(new_id, owner, Some(device), tx_new);
        hub.register(old_id, owner, Some(device), tx_old);

        assert!(hub.send_to_device(owner, device, frame()));
        assert!(rx_new.try_recv().is_ok(), "freshest socket should receive");
        assert!(rx_old.try_recv().is_err(), "stale socket should be skipped");
    }

    #[test]
    fn a_web_socket_has_no_device_presence() {
        let hub = Hub::new();
        let owner = Uuid::now_v7();
        let device = Uuid::now_v7();
        let (tx, _rx) = mpsc::channel(4);
        hub.register(Uuid::now_v7(), owner, None, tx); // browser connection
        assert!(!hub.is_device_online(owner, device));
        assert!(!hub.send_to_device(owner, device, frame()));
    }
}
