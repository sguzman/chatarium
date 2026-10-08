//! Linux sandbox concurrency regression: process work must not block the journal.

use super::*;

#[cfg(target_os = "linux")]
#[test]
fn live_stdio_dispatch_worker_contract_keeps_journal_responsive() {
    if std::env::var_os("CHATARIUM_TEST_LINUX_MCP_SANDBOX").is_none() {
        return;
    }
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "chatarium-async-mcp-{}-{nonce}.jsonl",
        std::process::id()
    ));
    let mut store = JsonlEventStore::open(&path).expect("open journal");
    let source = LocalConversationId::new();
    append_local_orchestration_topology_checked(
        &mut store,
        source,
        ChatContainerId::new(1),
        SessionId::new(1),
    )
    .unwrap();
    append_current_session_route_endpoint_checked(
        &mut store,
        source,
        SessionId::new(1),
        RouteEndpointId::new(1),
    )
    .unwrap();

    let provider = ToolProviderId::new(30);
    let call = ToolCallId::new(71);
    let route = RouteId::new(72);
    append_tool_provider_registration_checked(
        &mut store,
        provider,
        ToolProviderName::new("external.slow").unwrap(),
    )
    .unwrap();
    append_tool_provider_endpoint_checked(
        &mut store,
        provider,
        RouteEndpointId::new(50),
    )
    .unwrap();
    let config = StdioToolProviderConfig::new(
        "/usr/bin/sleep",
        vec!["3".to_owned()],
        vec![ToolOperationName::new("fixture.slow").unwrap()],
    )
    .unwrap();
    let configured = append_tool_transport_config_checked(&mut store, provider, &config)
        .unwrap();
    local_tool_provider_control::append_activation_with_inspection(
        &mut store,
        provider,
        configured.sequence,
        ProviderActivationDecision::Activate,
    )
    .unwrap();
    append_tool_call_proposal_checked(
        &mut store,
        call,
        route,
        source,
        provider,
        ToolOperationName::new("fixture.slow").unwrap(),
        "{}".to_owned(),
    )
    .unwrap();
    append_tool_call_route_user_decision_checked(
        &mut store,
        route,
        RouteUserDecision::Allow,
    )
    .unwrap();

    let (command_tx, command_rx) = mpsc::channel();
    let (notice_tx, notice_rx) = mpsc::channel();
    let completion_tx = command_tx.clone();
    let worker_data_dir = path.parent().expect("journal parent").to_path_buf();
    let worker = thread::spawn(move || {
        persistence_worker(store, worker_data_dir, command_rx, completion_tx, notice_tx)
    });
    command_tx
        .send(PersistCommand::ExecuteApprovedStdioTool { call_id: call })
        .unwrap();
    let dispatch = match notice_rx.recv_timeout(Duration::from_secs(10)).unwrap() {
        PersistNotice::StdioToolDispatchReserved { call_id, event } => {
            assert_eq!(call_id, call);
            assert_eq!(event.kind, EventKind::RouteDispatched);
            event
        }
        _ => panic!("expected a durable external MCP dispatch"),
    };
    command_tx
        .send(PersistCommand::RecordLocalMemory {
            memory_id: LocalMemoryId::new(73),
            source_conversation_id: source,
            text: "journal progress during slow external process".to_owned(),
        })
        .unwrap();
    // The child sleeps three seconds. The independent journal write must
    // complete before it, rather than blocking behind the subprocess.
    let memory = match notice_rx.recv_timeout(Duration::from_secs(2)).unwrap() {
        PersistNotice::LocalMemoryArtifactRecorded { event, .. } => event,
        _ => panic!("persistence was blocked behind the sandbox"),
    };
    assert!(memory.sequence > dispatch.sequence);
    let terminal = match notice_rx.recv_timeout(Duration::from_secs(12)).unwrap() {
        PersistNotice::StdioToolDispatchFinished { call_id, event } => {
            assert_eq!(call_id, call);
            event
        }
        _ => panic!("expected a durable terminal MCP observation"),
    };
    assert_eq!(terminal.kind, EventKind::ToolCallOutcomeObserved);
    assert!(terminal.sequence > memory.sequence);
    command_tx.send(PersistCommand::Shutdown).unwrap();
    worker.join().unwrap();

    let reopened = JsonlEventStore::open(&path).expect("reopen journal");
    assert!(
        replay_unresolved_external_tool_dispatches(reopened.events())
            .unwrap()
            .is_empty()
    );
    let outcome = replay_tool_call_outcome_audit(reopened.events())
        .unwrap()
        .into_iter()
        .find(|record| record.call_id == call)
        .unwrap();
    assert_eq!(outcome.kind, ToolCallOutcomeKind::Error);
    assert_eq!(outcome.dispatch_sequence, dispatch.sequence);
    fs::remove_file(&path).unwrap();
}
