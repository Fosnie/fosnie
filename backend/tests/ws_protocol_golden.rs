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

//! The server's half of the wire snapshots.
//!
//! The frames are defined once and used from here through `ws::protocol`, which
//! is the path every backend module takes. This test compares the bytes that
//! path produces against the very same fixtures the protocol crate's own test
//! reads, so if the definitions were ever quietly reintroduced on this side, the
//! two would disagree and this would go red.

use fosnie_backend::ws::protocol::{CitationOut, ServerFrame};
use uuid::Uuid;

/// Fixed ids, matching the protocol crate's fixtures.
fn id(n: u8) -> Uuid {
    Uuid::from_bytes([n; 16])
}

#[test]
fn the_server_writes_exactly_the_snapshotted_bytes() {
    let cases: Vec<(&str, String)> = vec![
        (
            include_str!("../../crates/fosnie-protocol/tests/fixtures/hello.json"),
            ServerFrame::Hello {
                socket_id: id(1),
                user_id: id(2),
                resume_token: "resume-token".into(),
                server_version: "0.3.0".into(),
                features: vec!["voice".into(), "mcp".into()],
            }
            .to_json(),
        ),
        (
            include_str!("../../crates/fosnie-protocol/tests/fixtures/chat_started.json"),
            ServerFrame::ChatStarted { turn_id: id(3), chat_id: id(4), message_id: id(5) }.to_json(),
        ),
        (
            include_str!("../../crates/fosnie-protocol/tests/fixtures/chat_token.json"),
            ServerFrame::ChatToken { turn_id: id(3), delta: "Hello".into() }.to_json(),
        ),
        (
            include_str!("../../crates/fosnie-protocol/tests/fixtures/chat_completed.json"),
            ServerFrame::ChatCompleted {
                turn_id: id(3),
                chat_id: id(4),
                message_id: id(5),
                reasoning_tokens: None,
            }
            .to_json(),
        ),
        (
            include_str!("../../crates/fosnie-protocol/tests/fixtures/chat_citations.json"),
            ServerFrame::ChatCitations {
                turn_id: id(3),
                message_id: id(5),
                citations: vec![
                    CitationOut {
                        doc_id: Some(id(6)),
                        quote_text: "a quoted passage".into(),
                        page_number: Some(4),
                        ..Default::default()
                    },
                    CitationOut {
                        quote_text: "a web snippet".into(),
                        url: Some("https://example.com/a".into()),
                        title: Some("Example".into()),
                        domain: Some("example.com".into()),
                        snippet_only: Some(true),
                        ..Default::default()
                    },
                ],
            }
            .to_json(),
        ),
        (
            include_str!("../../crates/fosnie-protocol/tests/fixtures/agent_approval.json"),
            ServerFrame::AgentApproval {
                run_id: id(7),
                turn_id: id(3),
                tool: "web_search".into(),
                summary: "search the web for rainfall data".into(),
                args: serde_json::json!({ "query": "rainfall", "depth": "deep" }),
            }
            .to_json(),
        ),
        (
            include_str!("../../crates/fosnie-protocol/tests/fixtures/voice_audio.json"),
            ServerFrame::VoiceAudio { audio_base64: "AAAA".into(), mime: "audio/wav".into() }
                .to_json(),
        ),
    ];

    for (expected, actual) in cases {
        assert_eq!(expected, actual, "the wire format has moved under the server");
    }
}
