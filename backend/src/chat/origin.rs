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

//! Where a conversation came from, and the context a turn carries about it.

use uuid::Uuid;

use crate::auth::AuthContext;

/// Which client started a conversation. Recorded on the chat row so the owner can
/// tell at a glance which of their clients began it. Derived from **how the
/// request authenticated**, never from anything the client declares in a message
/// body: a self-identifying frame is descriptive telemetry, not evidence.
///
/// There is no `Api` variant on purpose. The programmatic surface writes
/// `origin='api'` on the row it creates itself and never runs a turn through this
/// path, so an `Api` here would be a dead branch inviting a second, divergent way
/// to stamp the same column.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ChatOrigin {
    #[default]
    Web,
    Desktop,
}

impl ChatOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            ChatOrigin::Web => "web",
            ChatOrigin::Desktop => "desktop",
        }
    }

    /// A connection authenticated by a device token is a desktop client; any
    /// other is web.
    pub fn from_device(device_id: Option<Uuid>) -> Self {
        match device_id {
            Some(_) => ChatOrigin::Desktop,
            None => ChatOrigin::Web,
        }
    }
}

/// What one turn knows about who is asking and from where. Passed by value in
/// place of a bare `&AuthContext` so the connection's provenance travels with the
/// identity it belongs to and cannot be dropped on the way to chat creation.
#[derive(Clone, Copy)]
pub struct TurnContext<'a> {
    pub auth: &'a AuthContext,
    pub origin: ChatOrigin,
    /// Which paired machine this turn came in from, when it came in from one.
    ///
    /// Still provenance and still not authority — a device carries exactly its
    /// owner's rights. What it decides is where a request can be *sent*: work in
    /// a folder happens on one particular computer, and the only computer this
    /// turn can reach is the one holding the socket it arrived on.
    pub device_id: Option<Uuid>,
    /// A folder the composer chose for this chat, carried on the send so a
    /// brand-new chat's first message already works in it: the chat is created by
    /// this very turn, and there is no chat to bind a folder to until then.
    pub workspace_id: Option<Uuid>,
}

impl<'a> TurnContext<'a> {
    /// The ordinary case: a turn from the web, or from any caller for which
    /// provenance is not tracked (scheduler, workflows, voice).
    pub fn web(auth: &'a AuthContext) -> Self {
        Self { auth, origin: ChatOrigin::Web, device_id: None, workspace_id: None }
    }

    pub fn new(auth: &'a AuthContext, origin: ChatOrigin) -> Self {
        Self { auth, origin, device_id: None, workspace_id: None }
    }

    /// The same turn, knowing which machine it arrived from.
    pub fn with_device(mut self, device_id: Option<Uuid>) -> Self {
        self.device_id = device_id;
        self
    }

    /// The same turn, carrying the folder the composer chose for this chat.
    pub fn with_workspace(mut self, workspace_id: Option<Uuid>) -> Self {
        self.workspace_id = workspace_id;
        self
    }
}
