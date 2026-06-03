//! Agent tests (approval_gate_tests), split out of `agent`.

use super::{request_tool_approval, AgentEvent};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, Mutex};

fn pending_map() -> Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>> {
    Arc::new(Mutex::new(HashMap::new()))
}

#[tokio::test]
async fn approval_request_send_failure_cleans_pending_without_waiting_for_timeout() {
    let pending = pending_map();
    let (tx, rx) = mpsc::channel(1);
    drop(rx);

    let granted = request_tool_approval(
        &pending,
        &tx,
        "call_missing_ui",
        "run_shell",
        "{}".to_string(),
        None,
        Duration::from_secs(300),
    )
    .await;

    assert!(!granted);
    assert!(pending.lock().await.is_empty());
}

#[tokio::test]
async fn approval_response_cleans_pending_and_emits_final_event() {
    let pending = pending_map();
    let (tx, mut rx) = mpsc::channel(4);
    let pending_for_task = pending.clone();
    let tx_for_task = tx.clone();

    let task = tokio::spawn(async move {
        request_tool_approval(
            &pending_for_task,
            &tx_for_task,
            "call_ok",
            "run_shell",
            "{}".to_string(),
            None,
            Duration::from_secs(5),
        )
        .await
    });

    match rx.recv().await.unwrap() {
        AgentEvent::ApprovalRequest {
            call_id, tool_name, ..
        } => {
            assert_eq!(call_id, "call_ok");
            assert_eq!(tool_name, "run_shell");
        }
        other => panic!("expected ApprovalRequest, got {other:?}"),
    }

    let sender = pending.lock().await.remove("call_ok").unwrap();
    sender.send(true).unwrap();
    assert!(task.await.unwrap());
    assert!(pending.lock().await.is_empty());

    match rx.recv().await.unwrap() {
        AgentEvent::ApprovalGranted { call_id } => assert_eq!(call_id, "call_ok"),
        other => panic!("expected ApprovalGranted, got {other:?}"),
    }
}
