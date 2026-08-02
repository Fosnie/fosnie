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

//! The first thing a caller hears.
//!
//! A caller has no account, signed nothing, and read no page before ringing. Everything
//! they are ever told about what is happening to what they say has to be said aloud, at the
//! top of the call, before anything they say is acted on. This module composes those words
//! and nothing else: what plays them is the voice session, and what decides a call cannot
//! continue without them is the carrier adapter.
//!
//! **The wording follows what the line actually does.** A line that does not record keeps
//! no audio at all: speech is recognised as it arrives and the samples are discarded, so
//! the notice says the words are written down and says nothing about a recording, because
//! there is none. A line that records says so, in its own sentence, before the caller has
//! said anything. The switch and the sentence are one decision: there is no way to compose
//! an opening for a recording line that does not contain it.
//!
//! Everything here is a pure function over two strings, which is what lets the interface
//! show an operator the exact sentence a caller will hear before a single call comes in.

/// What every line says unless its practice has written its own.
///
/// Four short sentences, in this order for a reason: what they are speaking to, what
/// happens to what they say, how to reach a person instead, and then an invitation, so the
/// caller knows the line has finished talking and it is their turn.
pub const DEFAULT_NOTICE: &str = "You are speaking to an automated assistant. \
What you say is written down so that your enquiry can be dealt with, and a member of \
staff may read it. If you would rather speak to a person, please say so. \
How can I help you today?";

/// Said in addition, and only by a line that records.
///
/// Its own sentence rather than a rewording, so it survives a practice writing a notice of
/// its own: whatever else a recording line says, it says this. Placed second, straight
/// after what they are speaking to, because it is the thing a caller would most want to
/// know before deciding whether to carry on.
pub const RECORDED_SENTENCE: &str = "This call is recorded.";

/// Where the recording sentence goes in a notice.
///
/// After the first sentence when there is one, which is where somebody would say it aloud.
/// A practice's own notice of one long sentence gets it at the front rather than buried.
fn with_recorded(notice: &str) -> String {
    match notice.find(". ") {
        Some(at) => {
            let (first, rest) = notice.split_at(at + 2);
            format!("{first}{RECORDED_SENTENCE} {rest}")
        }
        None => format!("{RECORDED_SENTENCE} {notice}"),
    }
}

/// The longest notice a line may be given, in characters.
///
/// Not a storage limit: it is roughly a minute of speech, and a caller who is still being
/// talked at after a minute has hung up. A notice nobody listens to informs nobody, so the
/// cap is part of the notice working rather than a guard against large values.
pub const MAX_NOTICE: usize = 600;

/// The exact words a caller hears before anything else happens.
///
/// The line's own greeting first, if it has one, then the notice. Blank in either place
/// means absent, and an absent notice is the standard one: a line with nothing to say to a
/// caller is not something this permits.
///
/// `recorded` is the line's own setting, and it is an argument rather than something read
/// here so that every place which shows an operator what a caller will hear is showing the
/// same words the caller will hear.
pub fn opening(greeting: Option<&str>, notice: Option<&str>, recorded: bool) -> String {
    let greeting = tidy(greeting.unwrap_or_default());
    let notice = {
        let own = tidy(notice.unwrap_or_default());
        let base = if own.is_empty() { tidy(DEFAULT_NOTICE) } else { own };
        if recorded {
            with_recorded(&base)
        } else {
            base
        }
    };
    if greeting.is_empty() {
        return notice;
    }
    format!("{} {}", ended(&greeting), notice)
}

/// One line of speech: no line breaks, no runs of spaces, nothing on either end.
///
/// Both halves arrive from a text box, so both arrive with whatever somebody typed. A
/// stray newline in the middle is not a problem for a synthesiser, but two spaces and a
/// missing full stop between the two halves are audible, so they are dealt with here where
/// the preview in the interface can show the result.
fn tidy(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The text with a sentence ending, so the next sentence does not run into it.
///
/// A greeting typed as "Good morning, Smith and Company" is spoken straight into the
/// notice without this, which sounds like one long clause and buries the first thing the
/// caller is meant to notice.
fn ended(text: &str) -> String {
    match text.chars().last() {
        Some('.') | Some('!') | Some('?') | Some(',') | Some(';') | Some(':') => text.to_string(),
        _ => format!("{text}."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three things the standard notice exists to say. Pinned as a test because the
    /// wording will be edited for tone one day, and an edit that drops one of these turns a
    /// compliant line into a non-compliant one with no other symptom.
    #[test]
    fn the_standard_notice_says_the_three_things_it_must() {
        let said = DEFAULT_NOTICE.to_lowercase();
        assert!(said.contains("automated assistant"), "it must say what they are speaking to");
        assert!(said.contains("written down"), "it must say what happens to what they say");
        assert!(said.contains("may read it"), "it must say a person may read it");
        assert!(said.contains("speak to a person"), "it must say a person can be asked for");
        // And it must not claim a recording of its own accord: a line that does not record
        // keeps no audio at all, and saying otherwise would be as wrong as the reverse.
        assert!(!said.contains("record"), "the standard notice must not claim a recording");
    }

    /// The two directions of the one rule this feature turns on. A line that records says
    /// so before the caller has spoken; a line that does not must never say it. Both are
    /// asserted, because either failing is the difference between a recording and a covert
    /// one, and neither has any other symptom.
    #[test]
    fn a_recording_line_says_so_and_a_line_that_does_not_never_does() {
        let quiet = opening(None, None, false);
        assert!(!quiet.to_lowercase().contains("record"), "{quiet}");

        let recorded = opening(None, None, true);
        assert!(recorded.contains(RECORDED_SENTENCE), "{recorded}");
        // Second, straight after what they are speaking to, which is where somebody would
        // say it and before anything a caller has to decide about.
        assert!(
            recorded.starts_with("You are speaking to an automated assistant. This call is recorded."),
            "{recorded}"
        );
        // And everything the standard notice said is still said.
        for must in ["written down", "may read it", "speak to a person", "help you today"] {
            assert!(recorded.contains(must), "{must:?} was lost: {recorded}");
        }
    }

    /// Whatever else a practice writes, a recording line says it is recording. A notice of
    /// its own is the case where this could most easily be lost.
    #[test]
    fn a_practices_own_notice_still_says_it_is_recorded() {
        let one_sentence = opening(None, Some("Thank you for calling Smith and Company"), true);
        assert!(one_sentence.starts_with(RECORDED_SENTENCE), "{one_sentence}");
        let several = opening(None, Some("Hello there. We are open until five."), true);
        assert_eq!(several, "Hello there. This call is recorded. We are open until five.");
        // And a greeting still comes before all of it.
        let with_greeting = opening(Some("Smith and Company"), Some("Hello there. Open until five."), true);
        assert!(with_greeting.starts_with("Smith and Company. Hello there. This call is recorded."), "{with_greeting}");
    }

    #[test]
    fn a_line_with_nothing_of_its_own_says_the_standard_notice() {
        assert_eq!(opening(None, None, false), DEFAULT_NOTICE);
        assert_eq!(opening(Some(""), Some(""), false), DEFAULT_NOTICE);
        assert_eq!(opening(Some("   "), Some("\n\t "), false), DEFAULT_NOTICE);
    }

    #[test]
    fn a_greeting_comes_first_and_ends_before_the_notice_begins() {
        let said = opening(Some("Good morning, Smith and Company"), None, false);
        assert!(said.starts_with("Good morning, Smith and Company. You are speaking"), "{said}");
        assert!(said.ends_with("How can I help you today?"), "{said}");
    }

    /// One space between the halves and one full stop, whatever the operator typed. Both
    /// of these are audible: a doubled stop is a pause in the wrong place, and a missing
    /// one runs the greeting into the notice.
    #[test]
    fn the_two_halves_are_joined_once() {
        for greeting in ["Thank you for calling.", "Thank you for calling", "Thank you for calling  "] {
            let said = opening(Some(greeting), Some("This is a test notice."), false);
            assert_eq!(said, "Thank you for calling. This is a test notice.", "from {greeting:?}");
        }
        // A greeting that already ends mid-sentence keeps its own punctuation.
        assert_eq!(
            opening(Some("Smith and Company,"), Some("hello."), false),
            "Smith and Company, hello."
        );
    }

    #[test]
    fn a_typed_notice_replaces_the_standard_one_and_nothing_else() {
        let said = opening(None, Some("This call is written down."), false);
        assert_eq!(said, "This call is written down.");
        assert!(!said.contains("automated assistant"), "the practice's own wording stands alone");
    }

    /// Line breaks and runs of spaces come from a text box and would be read out as
    /// pauses in the wrong places.
    #[test]
    fn what_a_text_box_produces_is_spoken_as_one_line() {
        let said = opening(Some("Hello\n\nthere"), Some("One   two\tthree."), false);
        assert_eq!(said, "Hello there. One two three.");
    }

    /// The cap is what the write endpoint enforces, so it is stated where the notice is
    /// composed and asserted to be a length somebody would actually speak.
    #[test]
    fn the_cap_is_about_a_minute_of_speech() {
        assert_eq!(MAX_NOTICE, 600);
        assert!(DEFAULT_NOTICE.chars().count() < MAX_NOTICE, "the standard notice must fit its own cap");
    }
}
