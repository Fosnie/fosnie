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
    /// A conversation held aloud, over a telephone line.
    ///
    /// The caller has no account and never signs in, so the rule above holds in its
    /// strongest form here: the provenance comes from the carrier's request signature and
    /// a single-use ticket minted against it, and from nothing the media stream says.
    Phone,
}

impl ChatOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            ChatOrigin::Web => "web",
            ChatOrigin::Desktop => "desktop",
            ChatOrigin::Phone => "phone",
        }
    }

    /// Every value the chat row will accept, so a variant added here without the
    /// migration that permits it fails a test rather than a write.
    pub const ALL: [ChatOrigin; 3] = [ChatOrigin::Web, ChatOrigin::Desktop, ChatOrigin::Phone];

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
    /// The scheduled job that produced this turn, when one did. Pure provenance:
    /// it lets the run this turn opens be tied back to its automation (so a pause
    /// or a failure can be reflected on the automation's own record) and marks the
    /// folder binding as made by a schedule rather than by a person.
    pub automation_id: Option<Uuid>,
    /// The telephone call being carried, when this turn is part of one.
    ///
    /// Provenance again, and again not authority: the call runs as the account the
    /// line is registered to and can reach nothing that account cannot. What this
    /// decides is what a turn may write *about* — which call a record of what the
    /// caller wanted belongs to. Carried rather than looked up, because the record
    /// is written while the call is still in progress and nothing has yet linked
    /// the call to the conversation it is producing.
    pub call_id: Option<Uuid>,
}

impl<'a> TurnContext<'a> {
    /// The ordinary case: a turn from the web, or from any caller for which
    /// provenance is not tracked (scheduler, workflows, voice).
    pub fn web(auth: &'a AuthContext) -> Self {
        Self {
            auth,
            origin: ChatOrigin::Web,
            device_id: None,
            workspace_id: None,
            automation_id: None,
            call_id: None,
        }
    }

    pub fn new(auth: &'a AuthContext, origin: ChatOrigin) -> Self {
        Self { auth, origin, device_id: None, workspace_id: None, automation_id: None, call_id: None }
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

    /// The same turn, knowing which scheduled job opened it.
    pub fn with_automation(mut self, automation_id: Option<Uuid>) -> Self {
        self.automation_id = automation_id;
        self
    }

    /// The same turn, knowing which telephone call it is being spoken during.
    pub fn with_call(mut self, call_id: Option<Uuid>) -> Self {
        self.call_id = call_id;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The strings the chat row is stamped with. They are the values a database check
    /// permits, so they are pinned here rather than left to a rename: a variant whose
    /// string drifts writes rows the database refuses, at the end of a telephone call.
    #[test]
    fn every_origin_is_a_value_the_chat_row_accepts() {
        let written: Vec<&str> = ChatOrigin::ALL.iter().map(|o| o.as_str()).collect();
        assert_eq!(written, vec!["web", "desktop", "phone"]);
        // 'api' is deliberately absent: the programmatic surface stamps its own rows and
        // never runs a turn through here.
        assert!(!written.contains(&"api"));
    }

    #[test]
    fn a_device_token_means_a_desktop_client() {
        assert_eq!(ChatOrigin::from_device(Some(Uuid::now_v7())), ChatOrigin::Desktop);
        assert_eq!(ChatOrigin::from_device(None), ChatOrigin::Web);
    }
}
