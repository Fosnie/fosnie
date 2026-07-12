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

//! Sentence aggregation. The LLM token stream is
//! chunked at clause/sentence boundaries so streaming TTS can start on the first
//! finished clause rather than the whole answer — the single biggest perceived-
//! latency win. Boundaries are the hard terminators `. ! ?` and, past a minimum
//! length, the soft `; : —`; guarded so a decimal (`3.14`), an initial (`J.`), or a
//! common abbreviation (`Dr.`, `e.g.`) does not split mid-sentence. A terminator at
//! the very end of the buffer is held until the next delta confirms it (the period
//! could be a decimal/abbreviation continuing).

/// Minimum clause length (chars) before a soft terminator (`; : —`) splits.
const MIN_SOFT: usize = 60;

/// Lower-cased words that take a trailing full stop without ending a sentence.
const ABBREVS: &[&str] = &[
    "mr", "mrs", "ms", "dr", "prof", "sr", "jr", "st", "ltd", "plc", "inc", "co", "vs",
    "etc", "e.g", "i.e", "no", "vol", "fig", "pp", "al", "approx", "dept",
    "jan", "feb", "mar", "apr", "jun", "jul", "aug", "sep", "sept", "oct", "nov", "dec",
];

/// Accumulates streamed token deltas and emits complete clauses at boundaries.
#[derive(Default)]
pub struct SentenceAggregator {
    buf: String,
}

impl SentenceAggregator {
    pub fn new() -> Self {
        Self { buf: String::new() }
    }

    /// Push a streamed delta; return any complete clauses now available (in order).
    pub fn push(&mut self, delta: &str) -> Vec<String> {
        self.buf.push_str(delta);
        let mut out = Vec::new();
        while let Some(end) = self.next_boundary() {
            let clause = self.buf[..end].trim().to_string();
            self.buf.drain(..end);
            if !clause.is_empty() {
                out.push(clause);
            }
        }
        out
    }

    /// Flush whatever remains at end-of-stream as a final clause, if non-empty.
    pub fn flush(&mut self) -> Option<String> {
        let rest = self.buf.trim().to_string();
        self.buf.clear();
        if rest.is_empty() {
            None
        } else {
            Some(rest)
        }
    }

    /// Byte index just past the first complete clause in `buf`, or `None`.
    fn next_boundary(&self) -> Option<usize> {
        let s = &self.buf;
        let mut iter = s.char_indices().peekable();
        let mut clause_chars = 0usize;
        while let Some((i, c)) = iter.next() {
            clause_chars += 1;
            let hard = matches!(c, '.' | '!' | '?');
            let soft = matches!(c, ';' | ':' | '—') && clause_chars >= MIN_SOFT;
            if !(hard || soft) {
                continue;
            }
            // Absorb a run of trailing terminators / closing quotes-brackets (e.g.
            // `?!`, `..."`) so the whole punctuation tail rides with the clause.
            let mut end = i + c.len_utf8();
            while let Some(&(j, nc)) = iter.peek() {
                if matches!(nc, '.' | '!' | '?' | '"' | '\'' | ')' | ']' | '”' | '’' | '…') {
                    end = j + nc.len_utf8();
                    iter.next();
                } else {
                    break;
                }
            }
            // The terminator must be followed by whitespace to be trusted as a real
            // boundary; this alone rejects decimals (`3.14`) and `U.S.A`. A trailing
            // terminator at the buffer end waits for the next delta.
            match s[end..].chars().next() {
                Some(nc) if nc.is_whitespace() => {}
                None => return None,
                Some(_) => continue,
            }
            // Abbreviation / single-initial guard for a full stop.
            if c == '.' {
                let word = trailing_word(&s[..i]);
                let wl = word.trim_end_matches('.').to_lowercase();
                if ABBREVS.contains(&wl.as_str()) {
                    continue;
                }
                let mut wc = word.chars();
                if let (Some(first), None) = (wc.next(), wc.clone().next()) {
                    if first.is_uppercase() {
                        continue; // a lone initial, e.g. "J."
                    }
                }
            }
            return Some(end);
        }
        None
    }
}

/// The trailing run of letters/digits (and internal dots) ending `prefix` — the
/// "word" sitting right before a candidate full stop.
fn trailing_word(prefix: &str) -> String {
    let rev: String = prefix
        .chars()
        .rev()
        .take_while(|ch| ch.is_alphanumeric() || *ch == '.')
        .collect();
    rev.chars().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_streamed_tokens_at_sentence_ends() {
        let mut a = SentenceAggregator::new();
        assert!(a.push("Hello").is_empty()); // no terminator yet
        assert_eq!(a.push(" world. How are"), vec!["Hello world."]);
        assert_eq!(a.push(" you? Fine"), vec!["How are you?"]);
        assert_eq!(a.flush(), Some("Fine".to_string()));
    }

    #[test]
    fn does_not_split_decimals_or_abbreviations() {
        let mut a = SentenceAggregator::new();
        let out = a.push("The fee is 3.14 GBP per Dr. Smith. Next one.");
        assert_eq!(out, vec!["The fee is 3.14 GBP per Dr. Smith."]);
        assert_eq!(a.flush(), Some("Next one.".to_string()));
    }

    #[test]
    fn holds_a_terminator_at_the_buffer_end() {
        let mut a = SentenceAggregator::new();
        // The period is buffer-final — could be a decimal/abbreviation continuing.
        assert!(a.push("Total is 42.").is_empty());
        // The following space confirms it; the clause emerges on the next push.
        assert_eq!(a.push(" Done"), vec!["Total is 42."]);
    }

    #[test]
    fn absorbs_trailing_punctuation_run() {
        let mut a = SentenceAggregator::new();
        assert_eq!(a.push("Really?! Yes"), vec!["Really?!"]);
    }

    #[test]
    fn soft_break_only_past_minimum_length() {
        // A short colon clause does NOT split (held for the next delta).
        let mut a = SentenceAggregator::new();
        assert!(a.push("Note: ok").is_empty());
        assert_eq!(a.flush(), Some("Note: ok".to_string()));

        // A long enough run splits at the semicolon. (Fresh aggregator — `push` is
        // stateful, so a prior unsplit clause would otherwise ride along.)
        let mut b = SentenceAggregator::new();
        let long = "x".repeat(60);
        let out = b.push(&format!("{long}; and then more text follows here"));
        assert_eq!(out, vec![format!("{long};")]);
    }
}
