//! Item 5 — per-document ingest status is pushed to the uploader over the
//! WebSocket hub. These are pure transport/contract tests (no DB/ML): the live
//! extract→index→ready pipeline is exercised by the ingest E2E in `rag_chat.rs`.

use fosnie_backend::ws::hub::Hub;
use fosnie_backend::ws::protocol::ServerFrame;
use tokio::sync::mpsc;
use uuid::Uuid;

#[tokio::test]
async fn ingest_status_routes_to_uploader_only() {
    let hub = Hub::new();
    let uploader = Uuid::now_v7();
    let other = Uuid::now_v7();
    let (tx_a, mut rx_a) = mpsc::channel::<ServerFrame>(8);
    let (tx_b, mut rx_b) = mpsc::channel::<ServerFrame>(8);
    hub.register(Uuid::now_v7(), uploader, tx_a);
    hub.register(Uuid::now_v7(), other, tx_b);

    let doc_id = Uuid::now_v7();
    let kb_id = Uuid::now_v7();
    hub.send_to_user(
        uploader,
        ServerFrame::IngestStatus { doc_id, kb_id, status: "ready".into(), error: None },
    );

    match rx_a.try_recv().expect("uploader receives the frame") {
        ServerFrame::IngestStatus { doc_id: d, kb_id: k, status, error } => {
            assert_eq!(d, doc_id);
            assert_eq!(k, kb_id);
            assert_eq!(status, "ready");
            assert!(error.is_none());
        }
        other => panic!("unexpected frame: {other:?}"),
    }
    assert!(rx_b.try_recv().is_err(), "status must not fan out to other users");
}

#[tokio::test]
async fn ingest_status_serialises_with_tagged_type() {
    let frame = ServerFrame::IngestStatus {
        doc_id: Uuid::nil(),
        kb_id: Uuid::nil(),
        status: "error".into(),
        error: Some("extract failed".into()),
    };
    let v = serde_json::to_value(&frame).unwrap();
    assert_eq!(v["type"], "ingest.status");
    assert_eq!(v["status"], "error");
    assert_eq!(v["error"], "extract failed");

    // The error field is omitted on the happy path.
    let ok = ServerFrame::IngestStatus {
        doc_id: Uuid::nil(),
        kb_id: Uuid::nil(),
        status: "indexing".into(),
        error: None,
    };
    let ov = serde_json::to_value(&ok).unwrap();
    assert!(ov.get("error").is_none(), "error omitted when None");
}
