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

//! Stable-prefix token-budget allocator. The seven-layer prompt is sized in
//! priority order: the stable
//! cacheable prefix — [1] system prompt, [2] tool/skill metadata, [3] user
//! context, [4] memory — is reserved FIRST and is never trimmed to make room
//! for a lower slot. Whatever budget remains (after reserving room for the
//! answer) is split between [5] RAG context and [6] history, RAG taking its
//! share first so it can be trimmed before history is compacted. Keeping the
//! prefix byte-stable across turns is what preserves the LLM prompt cache.

/// How much of the post-prefix remainder RAG context [5] may claim before it is
/// trimmed. History [6] gets the rest. Conservative for legal (history matters),
/// so RAG does not crowd out the conversation.
const RAG_FRACTION: f64 = 0.5;

/// The token budgets for the variable slots, derived from the fixed prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Allocation {
    /// Tokens reserved for the stable prefix [1]–[4] (never trimmed).
    pub reserved_prefix: i64,
    /// Max tokens [5] RAG context may occupy (trim to fit before history).
    pub rag_budget: i64,
    /// Max tokens [6] history may occupy (compact to fit).
    pub history_budget: i64,
}

/// Allocate the variable slots from `budget` (the model's context window) given
/// the measured `prefix_tokens` ([1]–[4]), the desired `rag_tokens` ([5]), and
/// the `answer_reserve` kept free for the model's reply. The prefix is reserved
/// first; RAG is capped at `RAG_FRACTION` of the remainder (and never more than
/// it actually needs); history gets everything left. All outputs are clamped to
/// be non-negative — a prefix that already overflows yields zero variable
/// budget rather than a negative one (the prefix is still sent; it is mandatory).
pub fn allocate(budget: i64, prefix_tokens: i64, rag_tokens: i64, answer_reserve: i64) -> Allocation {
    let remainder = (budget - prefix_tokens - answer_reserve).max(0);
    let rag_cap = ((remainder as f64) * RAG_FRACTION) as i64;
    // RAG takes the smaller of what it wants and its cap — never steals history's
    // share for context it doesn't have.
    let rag_budget = rag_tokens.clamp(0, rag_cap);
    let history_budget = (remainder - rag_budget).max(0);
    Allocation { reserved_prefix: prefix_tokens, rag_budget, history_budget }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_is_reserved_and_never_negative() {
        // Prefix alone already exceeds the budget → no variable budget, but the
        // allocator never returns a negative (the prefix is still mandatory).
        let a = allocate(1000, 1200, 500, 256);
        assert_eq!(a.rag_budget, 0);
        assert_eq!(a.history_budget, 0);
        assert_eq!(a.reserved_prefix, 1200);
    }

    #[test]
    fn rag_capped_at_fraction_then_history_gets_rest() {
        // budget 10000, prefix 1000, answer 1000 → remainder 8000.
        // RAG wants 6000 but is capped at 50% (4000); history gets 4000.
        let a = allocate(10_000, 1000, 6000, 1000);
        assert_eq!(a.rag_budget, 4000);
        assert_eq!(a.history_budget, 4000);
    }

    #[test]
    fn small_rag_leaves_more_for_history() {
        // RAG wants only 500 (< the 4000 cap) → history gets 7500.
        let a = allocate(10_000, 1000, 500, 1000);
        assert_eq!(a.rag_budget, 500);
        assert_eq!(a.history_budget, 7500);
    }

    #[test]
    fn trimming_rag_never_shrinks_the_prefix() {
        // Whatever RAG/history do, the reserved prefix is reported unchanged so a
        // caller can assert slots [1]–[4] are honoured.
        let a = allocate(8000, 2000, 9000, 1000);
        assert_eq!(a.reserved_prefix, 2000);
        // remainder = 5000; rag cap = 2500; history = 2500.
        assert_eq!(a.rag_budget, 2500);
        assert_eq!(a.history_budget, 2500);
    }
}
