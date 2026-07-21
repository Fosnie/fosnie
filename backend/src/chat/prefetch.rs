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

//! A retrieval result the turn did not perform itself.
//!
//! Live voice can start the knowledge-base search from a partial transcript, while
//! the speaker is still talking, and hand the finished result to the turn. When it
//! does, the turn skips its own retrieval call and everything downstream — budget,
//! trim, the seven-layer compose, citations — proceeds unchanged, because the shape
//! handed over is exactly the shape the retrieval stream's terminal event carries.
//!
//! The one subtlety, and the reason this lives in its own testable module: an empty
//! context is not the same as no result. The streaming path assigns the gap
//! diagnostics unconditionally but leaves context, citations and parts untouched when
//! the context came back blank — so a turn that retrieved nothing still reports
//! *why* it found nothing, and the top-up tool can act on that. Injection has to
//! reproduce that asymmetry exactly or a blank prefetch would silently wipe
//! diagnostics the streaming path would have kept.

use crate::ml;

/// A retrieval performed ahead of the turn that will use it.
#[derive(Debug, Clone)]
pub struct PrefetchedRag {
    pub context: String,
    pub citations: Vec<ml::Citation>,
    pub parts: Vec<ml::SynthPart>,
    pub debug: ml::RetrieveDebug,
    /// The transcript this was retrieved for, kept for the turn log: it is what an
    /// operator compares against the committed transcript when a turn looks off.
    pub source_query: String,
}

/// Apply a prefetched result to the turn's retrieval slots, exactly as consuming the
/// retrieval stream's terminal event would.
pub fn apply_prefetch(
    p: PrefetchedRag,
    rag_context: &mut Option<String>,
    rag_citations: &mut Vec<ml::Citation>,
    rag_parts: &mut Vec<ml::SynthPart>,
    rag_gap_debug: &mut ml::RetrieveDebug,
) {
    if !p.context.trim().is_empty() {
        *rag_context = Some(p.context);
        *rag_citations = p.citations;
        *rag_parts = p.parts;
    }
    *rag_gap_debug = p.debug;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn citation(quote: &str) -> ml::Citation {
        ml::Citation {
            doc_id: Some(uuid::Uuid::now_v7()),
            chunk_index: Some(3),
            page_number: Some(7),
            clause_section_ref: Some("12.4".into()),
            quote_text: quote.into(),
        }
    }

    fn debug() -> ml::RetrieveDebug {
        ml::RetrieveDebug {
            gap_needs_exhausted: 2,
            gap_stop_reason: "deadline".into(),
            gap_unresolved: vec!["notice period".into()],
        }
    }

    /// The reference behaviour: what consuming the stream's terminal event does.
    fn apply_done(
        ev: ml::RetrieveEvent,
        rag_context: &mut Option<String>,
        rag_citations: &mut Vec<ml::Citation>,
        rag_parts: &mut Vec<ml::SynthPart>,
        rag_gap_debug: &mut ml::RetrieveDebug,
    ) {
        if let ml::RetrieveEvent::Done { context, citations, parts, debug } = ev {
            if !context.trim().is_empty() {
                *rag_context = Some(context);
                *rag_citations = citations;
                *rag_parts = parts;
            }
            *rag_gap_debug = debug;
        }
    }

    #[test]
    fn injection_matches_the_streamed_result() {
        let ctx_text = "[D1] Contractors accrue holiday pro rata.";
        let cits = vec![citation("accrue holiday pro rata")];
        let parts = vec![ml::SynthPart {
            title: "Holiday".into(),
            context: ctx_text.into(),
            has_evidence: true,
        }];

        let (mut c1, mut ci1, mut p1, mut d1) =
            (None, Vec::new(), Vec::new(), ml::RetrieveDebug::default());
        apply_done(
            ml::RetrieveEvent::Done {
                context: ctx_text.into(),
                citations: cits.clone(),
                parts: parts.clone(),
                debug: debug(),
            },
            &mut c1,
            &mut ci1,
            &mut p1,
            &mut d1,
        );

        let (mut c2, mut ci2, mut p2, mut d2) =
            (None, Vec::new(), Vec::new(), ml::RetrieveDebug::default());
        apply_prefetch(
            PrefetchedRag {
                context: ctx_text.into(),
                citations: cits,
                parts,
                debug: debug(),
                source_query: "what is the holiday allowance for contractors".into(),
            },
            &mut c2,
            &mut ci2,
            &mut p2,
            &mut d2,
        );

        assert_eq!(c1, c2);
        assert_eq!(format!("{ci1:?}"), format!("{ci2:?}"));
        assert_eq!(format!("{p1:?}"), format!("{p2:?}"));
        assert_eq!(format!("{d1:?}"), format!("{d2:?}"));
        assert!(c2.is_some());
    }

    #[test]
    fn a_blank_context_still_carries_the_gap_diagnostics() {
        // This is the asymmetry that a hand-written injection gets wrong: nothing
        // was found, so there is no context to install — but the reason nothing was
        // found is exactly what the top-up tool needs, and it must survive.
        let existing = vec![citation("a citation from an earlier assignment")];

        let (mut c1, mut ci1, mut p1, mut d1) =
            (None, existing.clone(), Vec::new(), ml::RetrieveDebug::default());
        apply_done(
            ml::RetrieveEvent::Done {
                context: "   ".into(),
                citations: vec![citation("would be discarded")],
                parts: Vec::new(),
                debug: debug(),
            },
            &mut c1,
            &mut ci1,
            &mut p1,
            &mut d1,
        );

        let (mut c2, mut ci2, mut p2, mut d2) =
            (None, existing, Vec::new(), ml::RetrieveDebug::default());
        apply_prefetch(
            PrefetchedRag {
                context: "   ".into(),
                citations: vec![citation("would be discarded")],
                parts: Vec::new(),
                debug: debug(),
                source_query: "mumbled half sentence".into(),
            },
            &mut c2,
            &mut ci2,
            &mut p2,
            &mut d2,
        );

        assert!(c1.is_none() && c2.is_none(), "a blank context installs nothing");
        assert_eq!(format!("{ci1:?}"), format!("{ci2:?}"), "existing citations are left alone");
        assert_eq!(d2.gap_stop_reason, "deadline", "the diagnostics survive regardless");
        assert_eq!(format!("{d1:?}"), format!("{d2:?}"));
    }
}
