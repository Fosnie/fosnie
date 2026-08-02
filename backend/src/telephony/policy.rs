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

//! What a tool that reaches outside may do while somebody is on the telephone.
//!
//! Everywhere else in the product, a tool call that changes something outside this
//! deployment waits for a person to approve it. That is the right answer for somebody
//! sitting in front of the interface, and the wrong one on a telephone line: there is no
//! card in front of the caller, nobody is watching one on their behalf, and the wait is
//! minutes of silence on a call that is being billed. A caller left listening to nothing
//! hangs up, and the run is approved half an hour later against a conversation that ended.
//!
//! So on a call the decision is made **in advance, by configuration**, rather than during
//! the call by a person: a server or a tool is either marked as usable on a call or it is
//! not, and one that is not is refused in words the agent can read out. The caller gets an
//! answer either way, which is the property that matters.
//!
//! Off a call nothing here applies and nothing changes.

/// What to do with one tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Run it now.
    Run,
    /// Hold it for a person, which is what happens everywhere that is not a telephone.
    Approve,
    /// Do not run it, and tell the caller so.
    Refuse,
}

/// Decide what happens to a tool call.
///
/// `side_effecting` is the tool's own declaration that it changes something; `marked` is
/// the operator having said this server or this tool may be used on a call. A read-only
/// tool runs wherever it is called: reading somebody's free times is not the thing the
/// approval gate exists for.
pub fn decide(on_call: bool, side_effecting: bool, marked: bool) -> Decision {
    match (on_call, side_effecting, marked) {
        // Not a call: unchanged in every case. Whether a tool is marked for use on a call
        // has no bearing on what happens off one.
        (false, true, _) => Decision::Approve,
        (false, false, _) => Decision::Run,
        // On a call: decided already, one way or the other.
        (true, true, true) => Decision::Run,
        (true, true, false) => Decision::Refuse,
        (true, false, _) => Decision::Run,
    }
}

/// What the agent is told when a tool cannot be used on a call, written to be said aloud.
///
/// Addressed to the model as an instruction about what to say next, because the caller
/// hears whatever comes back from here in some form. It names no tool, no system and no
/// fault: a caller can do nothing with the knowledge that a particular server is not
/// marked, and a receptionist who cannot do a thing offers the next best one.
pub const REFUSED_ON_CALL: &str =
    "error: that cannot be done during a telephone call on this line. Do not mention systems, \
     tools or settings to the caller. Tell them you cannot do that on the telephone just now, \
     and offer to take a message so somebody can deal with it.";

/// The same, for a tool that was allowed to run and took too long.
///
/// Deliberately not distinguishable by the caller from the refusal above. It is
/// distinguishable in the audit trail, which is where the difference is actionable.
pub const TIMED_OUT_ON_CALL: &str =
    "error: that is taking too long to answer while the caller is waiting. Do not mention \
     systems, tools or settings to the caller. Tell them you cannot confirm it right now, and \
     offer to take a message so somebody can deal with it.";

/// How long a tool call may take while a caller waits, when nothing says otherwise.
///
/// Eight seconds is already a long silence in a conversation. It is a ceiling on the worst
/// case rather than a target: everything usual answers in well under it, and past it the
/// caller is told something rather than left listening to nothing.
pub const DEFAULT_TOOL_TIMEOUT_SECS: u64 = 8;
/// The runtime key that overrides it.
pub const TOOL_TIMEOUT_KEY: &str = "telephony.tool_timeout_secs";

/// The ceiling in force, from the deployment's settings.
pub async fn tool_timeout_secs(pg: &sqlx::PgPool) -> u64 {
    crate::config::runtime::get(pg, TOOL_TIMEOUT_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|e| e.value.parse::<u64>().ok())
        .filter(|n| (1..=60).contains(n))
        .unwrap_or(DEFAULT_TOOL_TIMEOUT_SECS)
}

/// Read a server's stored policy. Anything unrecognised is a refusal: a value this code
/// does not understand must never be read as permission.
pub fn allowed_on_call(stored: &str) -> bool {
    stored == "allow"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every combination, stated rather than reasoned about, because the wrong answer in
    /// one cell is either a caller listening to silence or an anonymous stranger writing
    /// into somebody's corporate system.
    #[test]
    fn the_whole_decision_table() {
        // Off a call, nothing this module adds changes anything.
        assert_eq!(decide(false, true, false), Decision::Approve);
        assert_eq!(decide(false, true, true), Decision::Approve, "marking is about calls only");
        assert_eq!(decide(false, false, false), Decision::Run);
        assert_eq!(decide(false, false, true), Decision::Run);
        // On a call, nothing ever waits for a person.
        assert_eq!(decide(true, true, true), Decision::Run);
        assert_eq!(decide(true, true, false), Decision::Refuse);
        assert_eq!(decide(true, false, false), Decision::Run);
        assert_eq!(decide(true, false, true), Decision::Run);
    }

    /// The one property this module exists for: on a call there is no such thing as
    /// waiting for a person.
    #[test]
    fn nothing_on_a_call_is_ever_held_for_approval() {
        for side_effecting in [true, false] {
            for marked in [true, false] {
                assert_ne!(
                    decide(true, side_effecting, marked),
                    Decision::Approve,
                    "side_effecting={side_effecting} marked={marked}"
                );
            }
        }
    }

    /// Refused unless the stored value is exactly the one that permits it.
    #[test]
    fn only_the_word_that_permits_it_permits_it() {
        assert!(allowed_on_call("allow"));
        for other in ["refuse", "", "ALLOW", "allowed", "true", "yes", "1"] {
            assert!(!allowed_on_call(other), "{other:?} must not be read as permission");
        }
    }

    /// Both sentences are read out to a member of the public in some form, so neither may
    /// carry anything they cannot act on or should not hear.
    #[test]
    fn what_comes_back_can_be_said_to_a_caller() {
        for sentence in [REFUSED_ON_CALL, TIMED_OUT_ON_CALL] {
            let lower = sentence.to_lowercase();
            for leak in ["mcp", "server", "webhook", "http", "approval", "connector", "policy"] {
                assert!(!lower.contains(leak), "{leak:?} has no place in what a caller hears");
            }
            assert!(lower.contains("take a message"), "a refusal has to offer something");
            assert!(
                lower.contains("do not mention"),
                "the model has to be told not to explain the plumbing"
            );
        }
        // And they are different instructions, or the audit trail could not tell the
        // difference between a tool nobody allowed and one that is simply too slow.
        assert_ne!(REFUSED_ON_CALL, TIMED_OUT_ON_CALL);
    }

    #[test]
    fn the_ceiling_is_a_length_of_silence_somebody_would_tolerate() {
        assert_eq!(DEFAULT_TOOL_TIMEOUT_SECS, 8);
        assert!(DEFAULT_TOOL_TIMEOUT_SECS <= 15, "past this a caller has already given up");
    }
}
