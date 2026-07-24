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

//! Byte snapshots of the frames that matter, so that a change to the wire format
//! cannot happen quietly.
//!
//! Every fixture here is what one end actually puts on the socket. The server
//! ships with the instance; a desktop client does not, and the two meet at
//! whatever versions the user happens to have. A field renamed, a tag altered or
//! a default quietly dropped breaks that meeting in a way no type checker sees —
//! so the bytes are pinned, and changing them is a deliberate act with a red test
//! in front of it.
//!
//! Regenerating: `FOSNIE_UPDATE_FIXTURES=1 cargo test --test golden`. Do that
//! only when the wire format is meant to change, and read the diff.

use std::fs;
use std::path::PathBuf;

use fosnie_protocol::{CitationOut, ClientFrame, ReasoningSpec, ServerFrame};
use uuid::Uuid;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Compare one frame's wire bytes with its snapshot (or write the snapshot when
/// regenerating). Reads back through the same path the other end would.
fn check(name: &str, actual: String) {
    let path = fixtures_dir().join(format!("{name}.json"));
    if std::env::var("FOSNIE_UPDATE_FIXTURES").is_ok() {
        fs::create_dir_all(fixtures_dir()).expect("fixtures directory");
        fs::write(&path, &actual).expect("write fixture");
        return;
    }
    let expected = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("missing fixture {}: {e}", path.display()));
    assert_eq!(expected, actual, "wire format changed for {name}");
}

/// Fixed ids, so the snapshots are stable and a diff is only ever about shape.
fn id(n: u8) -> Uuid {
    Uuid::from_bytes([n; 16])
}

fn server_cases() -> Vec<(&'static str, ServerFrame)> {
    vec![
        (
            "hello",
            ServerFrame::Hello {
                socket_id: id(1),
                user_id: id(2),
                resume_token: "resume-token".into(),
                server_version: "0.3.0".into(),
                features: vec!["voice".into(), "mcp".into()],
            },
        ),
        (
            "chat_started",
            ServerFrame::ChatStarted { turn_id: id(3), chat_id: id(4), message_id: id(5) },
        ),
        ("chat_token", ServerFrame::ChatToken { turn_id: id(3), delta: "Hello".into() }),
        (
            "chat_reasoning",
            ServerFrame::ChatReasoning { turn_id: id(3), delta: "thinking".into() },
        ),
        (
            "chat_completed",
            ServerFrame::ChatCompleted {
                turn_id: id(3),
                chat_id: id(4),
                message_id: id(5),
                reasoning_tokens: None,
            },
        ),
        (
            "chat_completed_reasoning_tokens",
            ServerFrame::ChatCompleted {
                turn_id: id(3),
                chat_id: id(4),
                message_id: id(5),
                reasoning_tokens: Some(128),
            },
        ),
        (
            "chat_citations",
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
            },
        ),
        (
            "agent_approval",
            ServerFrame::AgentApproval {
                run_id: id(7),
                turn_id: id(3),
                tool: "web_search".into(),
                summary: "search the web for rainfall data".into(),
                args: serde_json::json!({ "query": "rainfall", "depth": "deep" }),
                detail: None,
            },
        ),
        (
            "agent_approval_detail",
            ServerFrame::AgentApproval {
                run_id: id(7),
                turn_id: id(3),
                tool: "desktop.fs_write".into(),
                summary: "Write notes.md in the connected folder?".into(),
                args: serde_json::json!({ "path": "notes.md" }),
                detail: Some(serde_json::json!({
                    "kind": "diff",
                    "path": "notes.md",
                    "diff": "@@ -1 +1 @@\n-old\n+new\n",
                })),
            },
        ),
        (
            "agent_approval_resolved",
            ServerFrame::AgentApprovalResolved { run_id: id(7), approved: true },
        ),
        (
            "chat_error",
            ServerFrame::ChatError {
                turn_id: Some(id(3)),
                message: "the task failed".into(),
                chat_id: Some(id(4)),
            },
        ),
        (
            "desktop_tool_call",
            ServerFrame::DesktopToolCall {
                call_id: id(10),
                turn_id: id(3),
                tool: "desktop.fs_read".into(),
                args: serde_json::json!({ "path": "notes.md" }),
            },
        ),
        (
            "voice_audio",
            ServerFrame::VoiceAudio { audio_base64: "AAAA".into(), mime: "audio/wav".into() },
        ),
        (
            "voice_state",
            ServerFrame::VoiceLiveState { state: "listening".into(), retrieving: false },
        ),
        (
            "research_progress",
            ServerFrame::ResearchProgress {
                chat_id: id(4),
                run_id: id(8),
                phase: "write".into(),
                detail: Some("section 3".into()),
                sources_read: Some(42),
                sections_done: Some(3),
                sections_total: Some(8),
                sections: None,
            },
        ),
        ("pong", ServerFrame::Pong),
    ]
}

fn client_cases() -> Vec<(&'static str, ClientFrame)> {
    vec![
        (
            "client_hello",
            ClientFrame::ClientHello {
                client_kind: Some("desktop".into()),
                client_version: Some("0.1.0".into()),
                capabilities: vec![],
            },
        ),
        (
            "client_chat_send",
            ClientFrame::ChatSend {
                chat_id: Some(id(4)),
                content: "what is the rainfall?".into(),
                agent_id: None,
                project_id: None,
                attachment_ids: vec![id(9)],
                thinking: None,
                reasoning: Some(ReasoningSpec {
                    enabled: true,
                    level: Some("medium".into()),
                    return_trace: true,
                }),
                llm_provider_id: None,
                workspace_id: None,
            },
        ),
        ("client_chat_cancel", ClientFrame::ChatCancel { turn_id: id(3) }),
        (
            "client_desktop_tool_result",
            ClientFrame::DesktopToolResult {
                call_id: id(10),
                ok: true,
                result: serde_json::json!({ "content": "the file's text" }),
            },
        ),
        (
            "client_desktop_tool_progress",
            ClientFrame::DesktopToolProgress {
                call_id: id(10),
                chunk: "compiling…\n".into(),
            },
        ),
        ("client_ping", ClientFrame::Ping),
    ]
}

#[test]
fn server_frames_match_their_snapshots() {
    for (name, frame) in server_cases() {
        check(name, frame.to_json());
    }
}

#[test]
fn client_frames_match_their_snapshots() {
    for (name, frame) in client_cases() {
        check(&format!("{name}"), frame.to_json());
    }
}

#[test]
fn snapshots_parse_back_into_the_frame_they_came_from() {
    // The reader's half of the contract: a client compiled against these types
    // has to be able to take the server's bytes apart again.
    for (name, frame) in server_cases() {
        let json = frame.to_json();
        let parsed: ServerFrame = serde_json::from_str(&json)
            .unwrap_or_else(|e| panic!("{name} does not parse back: {e}"));
        assert!(
            !matches!(parsed, ServerFrame::Unknown),
            "{name} parsed as an unknown frame"
        );
        assert_eq!(json, parsed.to_json(), "{name} does not survive a round trip");
    }
    for (name, frame) in client_cases() {
        let json = frame.to_json();
        let parsed: ClientFrame = serde_json::from_str(&json)
            .unwrap_or_else(|e| panic!("{name} does not parse back: {e}"));
        assert!(
            !matches!(parsed, ClientFrame::Unknown),
            "{name} parsed as an unknown frame"
        );
    }
}

#[test]
fn a_frame_type_neither_end_knows_is_tolerated_in_both_directions() {
    // The whole point of the two `Unknown` variants: a newer peer's frame is
    // ignored, not fatal. Losing this is how a version skew becomes an outage.
    let unknown = r#"{"version":1,"type":"chat.telepathy","payload":{"a":1}}"#;
    assert!(matches!(
        serde_json::from_str::<ServerFrame>(unknown).expect("server side tolerates it"),
        ServerFrame::Unknown
    ));
    assert!(matches!(
        serde_json::from_str::<ClientFrame>(unknown).expect("client side tolerates it"),
        ClientFrame::Unknown
    ));
}

#[test]
fn a_known_frame_that_is_wrong_is_an_error_and_not_an_unknown_one() {
    // The tolerance above is for frames from the future, not for corruption. A
    // frame whose type this build knows has to parse or fail loudly: quietly
    // reading a broken `chat.token` as "something I have not heard of" would
    // turn a bug into a stream that silently stops.
    let bad_id = r#"{"version":1,"type":"chat.token","turn_id":"not-a-uuid","delta":"hi"}"#;
    assert!(serde_json::from_str::<ServerFrame>(bad_id).is_err());

    let missing_field = r#"{"version":1,"type":"hello","socket_id":"00000000-0000-0000-0000-000000000000",
        "user_id":"00000000-0000-0000-0000-000000000000","server_version":"0.3.0","features":[]}"#;
    assert!(serde_json::from_str::<ServerFrame>(missing_field).is_err());

    // The same on the way in: a `chat.send` without its content is not a send.
    let no_content = r#"{"version":1,"type":"chat.send"}"#;
    assert!(serde_json::from_str::<ClientFrame>(no_content).is_err());

    // A result nobody can match to a call is not a result. Reading it as an
    // unknown frame would leave the turn that asked waiting for its whole
    // timeout on an answer that already arrived broken.
    let no_call_id = r#"{"version":1,"type":"desktop.tool.result","ok":true,"result":{}}"#;
    assert!(serde_json::from_str::<ClientFrame>(no_call_id).is_err());
    let bad_call_id =
        r#"{"version":1,"type":"desktop.tool.progress","call_id":"not-a-uuid","chunk":"x"}"#;
    assert!(serde_json::from_str::<ClientFrame>(bad_call_id).is_err());
    // And a request without the tool it is requesting is not a request.
    let no_tool = r#"{"version":1,"type":"desktop.tool.call",
        "call_id":"00000000-0000-0000-0000-000000000000",
        "turn_id":"00000000-0000-0000-0000-000000000000","args":{}}"#;
    assert!(serde_json::from_str::<ServerFrame>(no_tool).is_err());
}

#[test]
fn an_approval_from_a_newer_release_still_asks_the_old_question() {
    // The structured detail is additive: a client built before it existed has to
    // keep seeing exactly the frame it always saw, because the sentence in
    // `summary` is what it puts in front of the user.
    let with_detail = ServerFrame::AgentApproval {
        run_id: id(7),
        turn_id: id(3),
        tool: "desktop.terminal_run".into(),
        summary: "Run `npm test` in the connected folder?".into(),
        args: serde_json::json!({ "command": "npm test" }),
        detail: Some(serde_json::json!({ "kind": "command", "command": "npm test" })),
    }
    .to_json();
    let parsed: serde_json::Value = serde_json::from_str(&with_detail).expect("valid JSON");
    assert_eq!(parsed["summary"], "Run `npm test` in the connected folder?");
    assert_eq!(parsed["detail"]["kind"], "command");

    // Without it, the bytes carry no trace of the field at all.
    let without = ServerFrame::AgentApproval {
        run_id: id(7),
        turn_id: id(3),
        tool: "web_search".into(),
        summary: "search?".into(),
        args: serde_json::json!({}),
        detail: None,
    }
    .to_json();
    assert!(!without.contains("detail"), "an absent detail must not appear on the wire");
}

#[test]
fn an_error_without_a_chat_carries_no_trace_of_the_field() {
    // The chat id on an error is additive: an error not tied to a turn (a
    // rate-limit refusal) has none, and its bytes must not mention the field, so
    // a client built before it existed reads exactly the frame it always read.
    let untied = ServerFrame::ChatError {
        turn_id: None,
        message: "slow down".into(),
        chat_id: None,
    }
    .to_json();
    assert!(!untied.contains("chat_id"), "an absent chat_id must not appear on the wire");

    let tied = ServerFrame::ChatError {
        turn_id: Some(id(3)),
        message: "the task failed".into(),
        chat_id: Some(id(4)),
    }
    .to_json();
    let parsed: serde_json::Value = serde_json::from_str(&tied).expect("valid JSON");
    assert_eq!(parsed["chat_id"], "04040404-0404-0404-0404-040404040404");
}

#[test]
fn a_citation_from_a_newer_release_keeps_its_known_fields() {
    // Citations gained fields twice already. A reader that dropped the frame over
    // an unrecognised one would cost the user their sources.
    let json = r#"{"doc_id":null,"quote_text":"q","page_number":null,
        "clause_section_ref":null,"provenance_score":0.9}"#;
    let c: CitationOut = serde_json::from_str(json).expect("tolerates the new field");
    assert_eq!(c.quote_text, "q");
}
