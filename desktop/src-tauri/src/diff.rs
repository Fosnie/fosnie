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

//! Showing a change before it is made.
//!
//! The instance proposes the contents a file should end up with; it has never
//! seen what is in the file, so it cannot say what would change. This machine
//! can, and does, because agreeing to "write notes.md" and agreeing to the four
//! lines that would actually be replaced are not the same act.

use serde::Serialize;
use similar::{ChangeTag, TextDiff};

/// How many lines of unchanged text are shown either side of a change. Enough to
/// recognise where in the file it lands, short enough that a small edit to a long
/// file is a small card.
const CONTEXT: usize = 3;

/// What a proposed write would do to a file.
#[derive(Debug, Clone, Serialize)]
pub struct Preview {
    /// Was there a file there at all? A creation has nothing to compare against
    /// and is shown as what it would add.
    pub existed: bool,
    /// The change in the form people read changes in.
    pub unified: String,
    pub added: usize,
    pub removed: usize,
    /// The file is not text: no line-by-line difference means anything, and the
    /// card says so instead of showing nonsense.
    pub binary: bool,
}

/// The difference between what is on disk and what is proposed.
pub fn preview(before: Option<&str>, after: &str) -> Preview {
    let Some(before) = before else {
        return Preview {
            existed: false,
            unified: after.lines().map(|l| format!("+{l}\n")).collect(),
            added: after.lines().count(),
            removed: 0,
            binary: false,
        };
    };
    // A file whose bytes are not text reached this as `None` from the reader; a
    // lone null byte in something otherwise text-shaped is the other tell, and
    // comparing it line by line would produce a diff nobody can act on.
    if before.contains('\u{0}') {
        return Preview {
            existed: true,
            unified: String::new(),
            added: 0,
            removed: 0,
            binary: true,
        };
    }

    let diff = TextDiff::from_lines(before, after);
    let mut unified = String::new();
    let mut added = 0usize;
    let mut removed = 0usize;
    for (i, group) in diff.grouped_ops(CONTEXT).iter().enumerate() {
        if i > 0 {
            unified.push_str("…\n");
        }
        for op in group {
            for change in diff.iter_changes(op) {
                let sign = match change.tag() {
                    ChangeTag::Delete => {
                        removed += 1;
                        '-'
                    }
                    ChangeTag::Insert => {
                        added += 1;
                        '+'
                    }
                    ChangeTag::Equal => ' ',
                };
                unified.push(sign);
                unified.push_str(change.value());
                if change.missing_newline() {
                    unified.push('\n');
                }
            }
        }
    }
    Preview { existed: true, unified, added, removed, binary: false }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_file_is_shown_as_what_it_would_add() {
        let p = preview(None, "one\ntwo\n");
        assert!(!p.existed);
        assert_eq!(p.added, 2);
        assert_eq!(p.removed, 0);
        assert!(p.unified.contains("+one"));
    }

    #[test]
    fn an_edit_shows_only_what_moves_with_a_little_around_it() {
        let before = (1..=40).map(|n| format!("line {n}\n")).collect::<String>();
        let after = before.replace("line 20\n", "line twenty\n");
        let p = preview(Some(&before), &after);
        assert!(p.existed);
        assert_eq!((p.added, p.removed), (1, 1));
        assert!(p.unified.contains("-line 20"));
        assert!(p.unified.contains("+line twenty"));
        // Context, not the whole file: line 1 is nowhere near the change.
        assert!(!p.unified.contains("line 1\n"), "the whole file was included:\n{}", p.unified);
    }

    #[test]
    fn no_change_is_an_empty_difference() {
        let p = preview(Some("same\n"), "same\n");
        assert_eq!((p.added, p.removed), (0, 0));
        assert!(p.unified.is_empty());
    }

    #[test]
    fn a_file_that_is_not_text_is_said_to_be_so() {
        let p = preview(Some("bytes\u{0}here"), "replacement");
        assert!(p.binary);
        assert!(p.unified.is_empty());
    }
}
