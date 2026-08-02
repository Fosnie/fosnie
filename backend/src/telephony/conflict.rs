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

//! Whether a caller is already somewhere in the practice's own records.
//!
//! Comparing a name said aloud against a list of names is not a
//! similarity-of-meaning problem, and it is not something to ask a language model. It is
//! a matter of reducing both sides to one form and looking. So both sides go through
//! [`normalise`] on the way in, the comparison here is exact and total, and the answer is
//! one of three words rather than a score somebody has to interpret.
//!
//! Everything in this module is a pure function, and the tests below are the substance of
//! the check. A wrong answer here either lets a caller through who should have been
//! handed to a person, or refuses somebody for sharing a surname.
//!
//! **The verdict is the whole of what leaves.** Which stored name matched never travels
//! anywhere near the caller, the model, or the record of the call, because the fact that
//! somebody appears in a practice's records is itself confidential: told to the wrong
//! caller it discloses that the practice acts in a matter involving them, and the wrong
//! caller may be the other side of it.

/// What a check concluded.
///
/// A closed set mirroring the values the call row accepts. Only [`Clear`](Verdict::Clear)
/// means "carry on"; the other two mean the same thing as each other, which is what makes
/// the check fail closed rather than fail quietly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing on the list resembles this caller.
    Clear,
    /// Something does, and a person has to deal with it.
    Possible,
    /// There was not enough to check with, or the check could not be made.
    Unknown,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Clear => "clear",
            Verdict::Possible => "possible",
            Verdict::Unknown => "unknown",
        }
    }

    /// Read back a verdict recorded against a call.
    ///
    /// Anything unrecognised is `None`, which the gate treats as not checked: a value this
    /// code does not understand must not be read as permission.
    pub fn from_str(raw: &str) -> Option<Verdict> {
        Verdict::ALL.into_iter().find(|v| v.as_str() == raw)
    }

    /// Pinned against the migration by a test.
    pub const ALL: [Verdict; 3] = [Verdict::Clear, Verdict::Possible, Verdict::Unknown];

    /// May a call with this verdict be handed to a person?
    ///
    /// Only a clear one, and that includes never having been checked at all: a call whose
    /// verdict is missing is a call nobody looked at.
    pub fn lets_a_call_through(verdict: Option<Verdict>) -> bool {
        verdict == Some(Verdict::Clear)
    }
}

/// Words that carry no identity, so they are dropped before comparing.
///
/// Titles because a caller says them and a list rarely records them, and company forms
/// because "Acme", "Acme Ltd" and "Acme Limited" are one organisation. Kept short on
/// purpose: every word removed is a word two different names no longer differ by.
const NOISE: [&str; 14] = [
    "mr", "mrs", "miss", "ms", "dr", "prof", "sir", // titles
    "ltd", "limited", "plc", "llp", "llc", "inc", // company forms
    "the",
];

/// Reduce a name to the one form both sides are compared in.
///
/// Case, punctuation, spacing and word order are all discarded, because a name said down
/// a telephone and typed into a list will differ in every one of them. Word order goes so
/// that "Fraser, Jane" and "Jane Fraser" are the same name, which is how a client list
/// actually gets written.
pub fn normalise(raw: &str) -> String {
    let mut words: Vec<String> = raw
        .to_lowercase()
        .chars()
        // An apostrophe is dropped rather than treated as a break, because it sits inside
        // a name: turning it into a space makes O'Brien two words, one of which is "o".
        // A hyphen is a break, because a double-barrelled surname is two names.
        .filter(|c| !matches!(c, '\'' | '\u{2019}' | '\u{02bc}'))
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .filter(|w| !NOISE.contains(w))
        .map(str::to_string)
        .collect();
    words.sort();
    words.dedup();
    words.join(" ")
}

/// The words of a normalised name.
fn words(normalised: &str) -> Vec<&str> {
    normalised.split_whitespace().collect()
}

/// Do these two names look like the same party?
///
/// One is contained in the other, or they are equal. Containment is what catches a middle
/// name given on the telephone and absent from the list, or the other way round, and it
/// is deliberately not partial overlap: "Jane Fraser" and "Jane Bell" share a word and
/// are two people.
fn same_party(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let (aw, bw) = (words(a), words(b));
    // A single word is never enough to call two names the same party, in either
    // direction: half the callers to a practice share a surname with somebody in its
    // records, and blocking all of them is not a check, it is a closed line.
    if aw.len() < 2 || bw.len() < 2 {
        return false;
    }
    let (short, long) = if aw.len() <= bw.len() { (&aw, &bw) } else { (&bw, &aw) };
    short.iter().all(|w| long.contains(w))
}

/// Check a caller against a practice's list.
///
/// The name and the organisation are separate arguments rather than a list of strings,
/// because the rule about how much is enough to check differs between them. A person who
/// gives one word has given a surname, and a surname identifies nobody: half the callers
/// to a practice share one with somebody in its records. An organisation that is one word
/// is an organisation, and refusing to check it would mean never clearing anybody who
/// rings from a company with a short name.
///
/// `stored` is the list, already normalised.
pub fn screen(name: &str, organisation: &str, stored: &[String]) -> Verdict {
    let person = normalise(name);
    let firm = normalise(organisation);
    if person.is_empty() && firm.is_empty() {
        return Verdict::Unknown;
    }
    for candidate in [&person, &firm] {
        if candidate.is_empty() {
            continue;
        }
        if stored.iter().any(|s| same_party(candidate, s)) {
            return Verdict::Possible;
        }
    }
    // Nothing matched. Whether that means anything depends on whether there was enough to
    // match with: a full name, or an organisation of any length.
    if words(&person).len() >= 2 || !firm.is_empty() {
        Verdict::Clear
    } else {
        Verdict::Unknown
    }
}

/// What the agent is told, and the whole of what it is told.
///
/// A decision and what to do about it. Never which name matched, never how close it was,
/// and on a match an explicit instruction not to explain the refusal to the caller: "you
/// are on our records" is a disclosure about a matter, made to somebody who may be on the
/// other side of it.
pub fn advice(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Clear => {
            "Checked: nothing on record for that name. Carry on as normal."
        }
        Verdict::Possible => {
            "Checked: this needs a person, so do not offer an appointment and do not put the \
             caller through. Take a message instead, and say only that somebody will be in \
             touch. Do not tell the caller why, do not say anything about records or checks, \
             and do not mention anybody else."
        }
        Verdict::Unknown => {
            "Not checked: a first name and a surname are needed. Ask for their full name and \
             check again. If they will not give it, take a message and do not put them through."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The strings written into the call row. They are values a database check permits, so
    /// a rename here without the migration that allows it must fail a build rather than a
    /// write at the end of somebody's telephone call.
    #[test]
    fn every_verdict_is_a_value_the_row_accepts() {
        let written: Vec<&str> = Verdict::ALL.iter().map(|v| v.as_str()).collect();
        assert_eq!(written, vec!["clear", "possible", "unknown"]);
        let mut sorted = written.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), written.len(), "two verdicts share a value");
    }

    #[test]
    fn a_name_is_reduced_to_one_form_however_it_is_written() {
        // Case, punctuation, spacing, titles, company forms and word order all go.
        for (raw, want) in [
            ("Jane Fraser", "fraser jane"),
            ("  jane   FRASER ", "fraser jane"),
            ("Fraser, Jane", "fraser jane"),
            ("Mrs. Jane Fraser", "fraser jane"),
            ("Dr Jane Fraser", "fraser jane"),
            ("Acme Ltd", "acme"),
            ("ACME LIMITED", "acme"),
            ("The Acme Company", "acme company"),
            ("O'Brien", "obrien"),
            ("Smith-Jones", "jones smith"),
            ("", ""),
            ("Mr", ""),
            ("!!!", ""),
        ] {
            assert_eq!(normalise(raw), want, "normalising {raw:?}");
        }
    }

    #[test]
    fn two_writings_of_one_name_are_one_name() {
        let stored = vec![normalise("Jane Alice Fraser"), normalise("Acme Ltd")];
        // Equal, reordered, punctuated, titled, and missing a middle name: all the same
        // party.
        for given in ["Jane Alice Fraser", "jane fraser", "Fraser, Jane", "Mrs Jane Fraser"] {
            assert_eq!(screen(given, "", &stored), Verdict::Possible, "{given}");
        }
        // The organisation is checked in its own right, so an unknown person ringing about
        // a known company is still a match.
        assert_eq!(screen("Peter Bell", "Acme Limited", &stored), Verdict::Possible);
    }

    #[test]
    fn two_different_people_are_two_people() {
        let stored = vec![normalise("Jane Alice Fraser")];
        for given in [
            "Jane Bell",       // shares one word
            "Peter Fraser",    // shares the other
            "Jane Fraserson",  // near miss, not the same word
            "Alice Bell",
        ] {
            assert_eq!(screen(given, "", &stored), Verdict::Clear, "{given}");
        }
        // An organisation is enough to have checked with, however short its name, so a
        // caller from an unrelated company is cleared rather than left unchecked.
        assert_eq!(screen("", "Acme Ltd", &stored), Verdict::Clear);
        assert_eq!(screen("Bell", "Acme Ltd", &stored), Verdict::Clear);
    }

    #[test]
    fn one_word_is_not_a_check() {
        let stored = vec![normalise("Jane Alice Fraser")];
        // Not clear, because nothing was really checked; and not a match, because half a
        // town shares a surname with somebody in a practice's records.
        for given in ["Fraser", "Jane", "Mrs", "", "   "] {
            assert_eq!(screen(given, "", &stored), Verdict::Unknown, "{given}");
        }
        // A single stored word behaves the same way from the other side: a list containing
        // only "Fraser" does not match every Fraser who rings.
        let thin = vec![normalise("Fraser")];
        assert_eq!(screen("Jane Fraser", "", &thin), Verdict::Clear);
        // Except when it is exactly what was given, which is a real match.
        assert_eq!(screen("fraser", "", &thin), Verdict::Possible);
    }

    #[test]
    fn nothing_to_check_against_is_not_a_pass() {
        // An empty list cannot clear anybody, and it is why the tool is not offered at all
        // when a practice keeps no list: being told "clear" by a check that checked
        // nothing is worse than not having checked.
        assert_eq!(screen("Jane Fraser", "", &[]), Verdict::Clear);
        assert_eq!(screen("", "", &[]), Verdict::Unknown);
    }

    #[test]
    fn only_a_clear_answer_lets_a_call_through() {
        assert!(Verdict::lets_a_call_through(Some(Verdict::Clear)));
        assert!(!Verdict::lets_a_call_through(Some(Verdict::Possible)));
        assert!(!Verdict::lets_a_call_through(Some(Verdict::Unknown)));
        // A call nobody checked is a call nobody checked.
        assert!(!Verdict::lets_a_call_through(None));
    }

    /// The advice may say what to do and must not say who.
    #[test]
    fn the_advice_never_carries_a_name() {
        let advice = advice(Verdict::Possible);
        assert!(advice.contains("do not put the caller through"));
        assert!(advice.contains("Do not tell the caller why"));
        // The list is a practice's most confidential holding, and the thing reading this
        // is about to speak to a stranger.
        for word in ["fraser", "acme", "match", "record for"] {
            assert!(
                !advice.to_lowercase().contains(word),
                "the advice on a match mentions {word:?}"
            );
        }
    }
}
