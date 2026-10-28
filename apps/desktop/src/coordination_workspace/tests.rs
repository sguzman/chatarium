use super::*;
fn setup() -> (Workspace, u64, u64, u64) {
    let mut s = Workspace::default();
    let c = s.create_container("Team").unwrap();
    let a = s.attach(c, "chat-a", "Director", "Director").unwrap();
    let b = s.attach(c, "chat-b", "Coder", "Worker").unwrap();
    (s, c, a, b)
}
#[test]
fn message_explorer_filters_all_statuses_and_sender_recipient() {
    let make = |status: &str, kind: &str| Message {
        id: 17,
        from: 2,
        to: 3,
        kind: kind.to_owned(),
        body: "Acknowledge directive".to_owned(),
        status: status.to_owned(),
        created_ms: 100,
        delivered_ms: Some(101),
        consumed_ms: (status == "consumed").then_some(102),
        note: String::new(),
    };
    let delivered = make("delivered", "directive");
    let consumed = make("consumed", "ack");
    assert!(message_visible(
        &delivered, "Team", "Director", "Coder", "", "all"
    ));
    assert!(message_visible(
        &consumed, "Team", "Director", "Coder", "ack", "all"
    ));
    assert!(message_visible(
        &consumed, "Team", "Director", "Coder", "coder", "all"
    ));
    assert!(message_visible(
        &consumed, "Team", "Director", "Coder", "#17", "all"
    ));
    assert!(message_visible(
        &delivered, "Team", "Director", "Coder", "", "pending"
    ));
    assert!(!message_visible(
        &consumed, "Team", "Director", "Coder", "", "pending"
    ));
    assert!(message_visible(
        &consumed, "Team", "Director", "Coder", "", "consumed"
    ));
    assert!(!message_visible(
        &delivered, "Team", "Director", "Coder", "", "consumed"
    ));
    assert!(!message_visible(
        &consumed, "Team", "Director", "Coder", "", "other"
    ));
    assert!(message_visible(
        &make("blocked", "request"),
        "Team",
        "Director",
        "Coder",
        "",
        "other"
    ));
}

#[test]
fn delivery_read_consumption_deletion_and_reopen_have_distinct_states() {
    let (mut s, c, a, b) = setup();
    s.set_link(c, a, b, true, true, "allow", 12).unwrap();
    let (message, _) = s
        .send("chat-a", b, "directive", "Do the test", 100)
        .unwrap();
    assert_eq!(s.context_delivered_ids("chat-b"), vec![message]);
    let identity = s.identity_tool("chat-b").unwrap();
    assert_eq!(identity["inbox"][0]["id"], message);
    // Reading identity does NOT consume a message.
    assert_eq!(s.context_delivered_ids("chat-b"), vec![message]);
    assert_eq!(s.consume_ids("chat-a", &[message], 110), 0);
    assert_eq!(s.consume_ids("chat-b", &[message], 110), 1);
    assert_eq!(s.consume_ids("chat-b", &[message], 120), 0);
    assert!(s.context_delivered_ids("chat-b").is_empty());
    assert_eq!(
        s.identity_tool("chat-b").unwrap()["recent_consumed"][0]["id"],
        message
    );
    assert!(s.delete_message(c, message).is_err());
    let path =
        std::env::temp_dir().join(format!("chatarium-consumption-{}.json", std::process::id()));
    s.save_atomic(&path).unwrap();
    let reloaded = Workspace::load(&path).unwrap();
    assert_eq!(reloaded, s);
    assert_eq!(reloaded.containers[0].messages[0].consumed_ms, Some(110));
    let _ = fs::remove_file(path);
}

#[test]
fn pending_deletion_removes_only_selected_item_without_reusing_ids() {
    let (mut s, c, a, b) = setup();
    s.set_link(c, a, b, true, true, "queue", 12).unwrap();
    let (first, _) = s.send("chat-a", b, "directive", "First", 1).unwrap();
    let (second, _) = s.send("chat-a", b, "directive", "Second", 2).unwrap();
    s.delete_message(c, first).unwrap();
    assert!(
        s.containers[0]
            .messages
            .iter()
            .any(|m| m.id == first && m.status == "deleted" && m.body.is_empty())
    );
    assert!(s.containers[0].messages.iter().any(|m| m.id == second));
    let (third, _) = s.send("chat-a", b, "directive", "Third", 3).unwrap();
    assert!(third > second);
}

#[test]
fn empty_container_first_and_worker_identity_survive_reopen() {
    let (mut s, c, a, b) = setup();
    let path = std::env::temp_dir().join(format!(
        "chatarium-coordination-{}.json",
        std::process::id()
    ));
    assert_eq!(s.containers[0].members.len(), 2);
    s.save_atomic(&path).unwrap();
    let loaded = Workspace::load(&path).unwrap();
    assert_eq!(loaded, s);
    assert_eq!(loaded.member_for("chat-a"), Some((c, a)));
    assert_eq!(loaded.member_for("chat-b"), Some((c, b)));
    assert!(s.attach(c, "chat-b", "Intruder", "Director").is_err());
    let _ = fs::remove_file(path);
}
#[test]
fn reverse_acknowledgment_is_automatically_authorized_but_not_reverse_orders() {
    let (mut workspace, container, director, worker) = setup();
    workspace
        .set_link(container, director, worker, true, false, "allow", 12)
        .unwrap();
    for kind in ["ack", "report", "request", "result"] {
        assert_eq!(
            workspace
                .send("chat-b", director, kind, "Received", 100)
                .unwrap()
                .1,
            "delivered"
        );
    }
    assert_eq!(
        workspace
            .send("chat-b", director, "directive", "Orders from below", 101)
            .unwrap()
            .1,
        "blocked"
    );
    assert_eq!(workspace.containers[0].links.len(), 1);
    workspace
        .set_link(container, worker, director, false, false, "block", 12)
        .unwrap();
    assert_eq!(
        workspace
            .send("chat-b", director, "ack", "Denied by explicit block", 102)
            .unwrap()
            .1,
        "blocked"
    );
}

#[test]
fn revoking_forward_directive_authority_closes_automatic_reply_path() {
    let (mut workspace, container, director, worker) = setup();
    workspace
        .set_link(container, director, worker, true, false, "allow", 12)
        .unwrap();
    assert_eq!(
        workspace
            .send("chat-b", director, "ack", "Before", 100)
            .unwrap()
            .1,
        "delivered"
    );
    workspace
        .set_link(container, director, worker, false, false, "block", 12)
        .unwrap();
    assert_eq!(
        workspace
            .send("chat-b", director, "ack", "After", 101)
            .unwrap()
            .1,
        "blocked"
    );
}

#[test]
fn authority_denies_reverse_orders_and_allows_reports_independently() {
    let (mut s, c, a, b) = setup();
    s.set_link(c, a, b, true, true, "allow", 12).unwrap();
    s.set_link(c, b, a, false, true, "allow", 12).unwrap();
    let (_, state) = s
        .send("chat-b", a, "directive", "I override the director", 10)
        .unwrap();
    assert_eq!(state, "blocked");
    let (_, state) = s.send("chat-b", a, "report", "Job done", 11).unwrap();
    assert_eq!(state, "delivered");
    assert!(s.send("other-chat", b, "directive", "forged", 12).is_err());
    assert!(
        s.send("chat-a", 12345, "directive", "wrong container", 13)
            .is_err()
    );
    assert!(s.identity_context("chat-a").contains("Director"));
    assert!(s.identity_context("chat-a").contains("Job done"));
}
#[test]
fn queue_release_rechecks_revoked_authority_and_rate_limit() {
    let (mut s, c, a, b) = setup();
    s.set_link(c, a, b, true, true, "rate_limit", 1).unwrap();
    let (first, ok) = s.send("chat-a", b, "directive", "One", 100_000).unwrap();
    assert_eq!(ok, "delivered");
    let (second, state) = s.send("chat-a", b, "directive", "Two", 100_001).unwrap();
    assert_eq!(state, "queued");
    assert_ne!(first, second);
    s.set_link(c, a, b, false, false, "block", 1).unwrap();
    s.decide(c, second, true, 100_002).unwrap();
    assert_eq!(s.containers[0].messages.last().unwrap().status, "rejected");
    s.set_link(c, a, b, true, true, "approve", 1).unwrap();
    let (third, state) = s.send("chat-a", b, "request", "New task", 161_000).unwrap();
    assert_eq!(state, "approval");
    s.decide(c, third, true, 161_001).unwrap();
    assert_eq!(s.containers[0].messages.last().unwrap().status, "delivered");
    assert!(s.decide(c, third, true, 161_002).is_err());
}
#[test]
fn rate_limited_fifo_delivery_advances_without_human_queue_release() {
    let (mut s, c, a, b) = setup();
    s.set_link(c, a, b, true, true, "rate_limit", 1).unwrap();
    assert_eq!(
        s.send("chat-a", b, "directive", "one", 1_000).unwrap().1,
        "delivered"
    );
    assert_eq!(
        s.send("chat-a", b, "directive", "two", 1_001).unwrap().1,
        "queued"
    );
    assert_eq!(
        s.send("chat-a", b, "directive", "three", 1_002).unwrap().1,
        "queued"
    );
    assert_eq!(s.advance_rate_queues(59_999), 0);
    assert_eq!(s.advance_rate_queues(61_000), 1);
    assert_eq!(s.containers[0].messages[1].status, "delivered");
    assert_eq!(s.containers[0].messages[2].status, "queued");
    assert_eq!(s.advance_rate_queues(121_000), 1);
    assert_eq!(s.containers[0].messages[2].status, "delivered");
}

#[test]
fn new_rate_limited_messages_cannot_overtake_queued_work_after_restart() {
    let (mut workspace, container, director, worker) = setup();
    workspace
        .set_link(container, director, worker, true, true, "rate_limit", 1)
        .unwrap();
    let first = workspace
        .send("chat-a", worker, "directive", "first", 1_000)
        .unwrap();
    let second = workspace
        .send("chat-a", worker, "directive", "second", 1_001)
        .unwrap();
    assert_eq!(first.1, "delivered");
    assert_eq!(second.1, "queued");

    let path =
        std::env::temp_dir().join(format!("chatarium-rate-fifo-{}.json", std::process::id()));
    workspace.save_atomic(&path).unwrap();
    let mut restored = Workspace::load(&path).unwrap();
    let _ = fs::remove_file(path);

    // A new message arrives just as the first delivery expires. It must
    // join the backlog, not take the newly available delivery slot.
    let third = restored
        .send("chat-a", worker, "directive", "third", 61_000)
        .unwrap();
    assert_eq!(third.1, "queued");
    assert_eq!(restored.advance_rate_queues(61_000), 1);
    assert_eq!(restored.containers[0].messages[1].status, "delivered");
    assert_eq!(restored.containers[0].messages[2].status, "queued");
    assert_eq!(restored.advance_rate_queues(121_000), 1);
    assert_eq!(restored.containers[0].messages[2].status, "delivered");
    assert_eq!(
        restored.containers[0]
            .messages
            .iter()
            .map(|message| message.id)
            .collect::<Vec<_>>(),
        vec![first.0, second.0, third.0]
    );
}

#[test]
fn archive_rejects_ambiguous_conversation_owners_and_reused_global_ids() {
    let (mut workspace, first_container, first_member, _) = setup();
    let other_container = workspace.create_container("Other").unwrap();
    workspace
        .attach(other_container, "chat-c", "Researcher", "Worker")
        .unwrap();
    let path = std::env::temp_dir().join(format!(
        "chatarium-global-identity-{}.json",
        std::process::id()
    ));
    workspace.save_atomic(&path).unwrap();
    let original: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();

    let mut duplicate_conversation = original.clone();
    duplicate_conversation["containers"][1]["members"][0]["conversation"] = json!("chat-a");
    fs::write(&path, duplicate_conversation.to_string()).unwrap();
    assert!(
        Workspace::load(&path)
            .unwrap_err()
            .contains("duplicate worker or conversation membership")
    );

    let mut duplicate_member = original.clone();
    duplicate_member["containers"][1]["members"][0]["id"] = json!(first_member);
    fs::write(&path, duplicate_member.to_string()).unwrap();
    assert!(
        Workspace::load(&path)
            .unwrap_err()
            .contains("duplicate worker or conversation membership")
    );

    let mut duplicate_message = original.clone();
    // A valid message minted by the normal allocator must not be allowed
    // to impersonate an existing container identity after a file edit.
    let sender = workspace.member_for("chat-a").unwrap().1;
    let recipient = workspace.member_for("chat-b").unwrap().1;
    duplicate_message["containers"][0]["messages"] = json!([{
        "id": first_container,
        "from": sender,
        "to": recipient,
        "kind": "report",
        "body": "collision",
        "status": "blocked",
        "created_ms": 1,
        "delivered_ms": null,
        "consumed_ms": null,
        "note": "fixture"
    }]);
    fs::write(&path, duplicate_message.to_string()).unwrap();
    assert!(
        Workspace::load(&path)
            .unwrap_err()
            .contains("invalid or duplicate coordination message")
    );

    fs::write(&path, original.to_string()).unwrap();
    assert_eq!(Workspace::load(&path).unwrap(), workspace);
    let _ = fs::remove_file(path);
}

#[test]
fn wake_dispatch_is_durable_and_binds_the_exact_oldest_directive() {
    let (mut workspace, container, director, worker) = setup();
    workspace
        .set_link(container, director, worker, true, true, "allow", 12)
        .unwrap();
    let target = workspace
        .send("chat-a", worker, "directive", "first <end> \"quoted\"", 1)
        .unwrap()
        .0;
    for index in 0..20 {
        workspace
            .send(
                "chat-a",
                worker,
                "directive",
                &format!("later directive {} {}", index, "x".repeat(350)),
                index + 2,
            )
            .unwrap();
    }
    assert!(!workspace.context_delivered_ids("chat-b").contains(&target));
    let path = std::env::temp_dir().join(format!(
        "chatarium-wake-handoff-{}.json",
        std::process::id()
    ));
    workspace
        .grant_wake_lease_durable(&path, "chat-b", 1)
        .unwrap();
    let envelope = workspace
        .claim_next_wake_durable(&path, "chat-b")
        .unwrap()
        .unwrap();
    assert_eq!(envelope.message_id, target);
    assert_eq!(envelope.recipient_conversation, "chat-b");
    assert_eq!(envelope.sender_id, director);
    assert_eq!(envelope.sender_name, "Director");
    assert_eq!(envelope.directive_text, "first <end> \"quoted\"");
    let serialized = envelope.model_context();
    assert!(serialized.contains("NON-AUTHORED COORDINATION WAKE"));
    let json = serialized.lines().last().unwrap();
    let value: Value = serde_json::from_str(json).unwrap();
    assert_eq!(value["directive_text"], envelope.directive_text);
    assert_eq!(value["message_id"], target);
    assert!(!serialized.contains("later directive 19"));

    let mut reopened = Workspace::load(&path).unwrap();
    assert_eq!(
        reopened.claim_next_wake_durable(&path, "chat-b").unwrap(),
        None
    );
    assert_eq!(
        reopened.wake_dispatch_envelope("chat-b", target).unwrap(),
        envelope
    );
    reopened.stop_wake_lease_durable(&path, "chat-b").unwrap();
    assert!(
        Workspace::load(&path)
            .unwrap()
            .wake_dispatch_envelope("chat-b", target)
            .is_err()
    );
    let _ = fs::remove_file(path);
}

#[test]
fn failed_claim_persistence_does_not_spend_a_wake_permit() {
    let (mut workspace, container, director, worker) = setup();
    workspace
        .set_link(container, director, worker, true, true, "allow", 12)
        .unwrap();
    let target = workspace
        .send("chat-a", worker, "directive", "task", 1)
        .unwrap()
        .0;
    workspace.grant_wake_lease("chat-b", 1).unwrap();
    let missing_parent = std::env::temp_dir().join(format!(
        "chatarium-wake-missing-parent-{}",
        std::process::id()
    ));
    // A regular file is not a directory; atomic save must fail.
    fs::write(&missing_parent, b"not a directory").unwrap();
    let blocked = missing_parent.join("workspace.json");
    let snapshot = workspace.clone();
    assert!(
        workspace
            .claim_next_wake_durable(&blocked, "chat-b")
            .is_err()
    );
    assert_eq!(workspace, snapshot);
    assert_eq!(workspace.wake_leases[0].remaining, 1);
    assert_eq!(workspace.wake_leases[0].active_message_id, None);
    assert_eq!(workspace.claim_next_wake("chat-b").unwrap(), Some(target));
    let _ = fs::remove_file(missing_parent);
}

#[test]
fn consumed_wake_claim_remains_valid_for_same_turn_coordination_reply() {
    let (mut workspace, container, director, worker) = setup();
    workspace
        .set_link(container, director, worker, true, true, "approve", 12)
        .unwrap();
    let directive = workspace
        .send("chat-a", worker, "directive", "Approved", 10)
        .unwrap()
        .0;
    workspace.decide(container, directive, true, 11).unwrap();
    workspace.grant_wake_lease("chat-b", 1).unwrap();
    assert_eq!(
        workspace.claim_next_wake("chat-b").unwrap(),
        Some(directive)
    );
    assert_eq!(workspace.consume_ids("chat-b", &[directive], 12), 1);
    assert_eq!(workspace.next_eligible_wake_id("chat-b"), None);
    workspace.validate_wake_claim("chat-b", directive).unwrap();
    workspace
        .set_link(container, director, worker, false, false, "block", 12)
        .unwrap();
    assert!(workspace.validate_wake_claim("chat-b", directive).is_err());
    workspace.stop_wake_lease("chat-b").unwrap();
    assert!(workspace.validate_wake_claim("chat-b", directive).is_err());
}

#[test]
fn wake_scheduler_only_sees_authorized_unclaimed_directives() {
    let (mut workspace, container, director, worker) = setup();
    workspace
        .set_link(container, director, worker, true, true, "allow", 12)
        .unwrap();
    let first = workspace
        .send("chat-a", worker, "directive", "First", 1)
        .unwrap()
        .0;
    workspace
        .send("chat-a", worker, "report", "Not a directive", 2)
        .unwrap();
    workspace.grant_wake_lease("chat-b", 2).unwrap();
    assert_eq!(workspace.next_eligible_wake_id("chat-b"), Some(first));
    assert_eq!(workspace.claim_next_wake("chat-b").unwrap(), Some(first));
    assert_eq!(workspace.next_eligible_wake_id("chat-b"), None);
    workspace.finish_wake_claim("chat-b", first).unwrap();
    assert_eq!(workspace.next_eligible_wake_id("chat-b"), None);
    let later = workspace
        .send("chat-a", worker, "directive", "Second", 3)
        .unwrap()
        .0;
    workspace
        .set_link(container, director, worker, false, false, "block", 12)
        .unwrap();
    assert_eq!(workspace.next_eligible_wake_id("chat-b"), None);
    workspace
        .set_link(container, director, worker, true, true, "allow", 12)
        .unwrap();
    assert_eq!(workspace.next_eligible_wake_id("chat-b"), Some(later));
    workspace.stop_wake_lease("chat-b").unwrap();
    assert_eq!(workspace.next_eligible_wake_id("chat-b"), None);
}

#[test]
fn wake_is_default_off_bounded_and_restart_safe() {
    let (mut workspace, container, director, worker) = setup();
    workspace
        .set_link(container, director, worker, true, true, "allow", 12)
        .unwrap();
    let first = workspace
        .send("chat-a", worker, "directive", "First task", 100)
        .unwrap()
        .0;
    let second = workspace
        .send("chat-a", worker, "directive", "Second task", 101)
        .unwrap()
        .0;
    assert_eq!(workspace.claim_next_wake("chat-b").unwrap(), None);
    assert!(workspace.grant_wake_lease("chat-b", 0).is_err());
    assert!(workspace.grant_wake_lease("chat-b", 9).is_err());
    workspace.grant_wake_lease("chat-b", 2).unwrap();
    assert_eq!(workspace.claim_next_wake("chat-b").unwrap(), Some(first));
    assert_eq!(workspace.claim_next_wake("chat-b").unwrap(), None);
    workspace.validate_wake_claim("chat-b", first).unwrap();

    let path = std::env::temp_dir().join(format!(
        "chatarium-wake-restart-{}.json",
        std::process::id()
    ));
    workspace.save_atomic(&path).unwrap();
    let mut restored = Workspace::load(&path).unwrap();
    let _ = fs::remove_file(path);
    assert_eq!(restored, workspace);
    assert_eq!(restored.claim_next_wake("chat-b").unwrap(), None);
    restored.finish_wake_claim("chat-b", first).unwrap();
    assert_eq!(restored.claim_next_wake("chat-b").unwrap(), Some(second));
    restored.finish_wake_claim("chat-b", second).unwrap();
    assert_eq!(restored.claim_next_wake("chat-b").unwrap(), None);
    // Re-arming cannot duplicate a previously claimed, undelivered turn.
    restored.grant_wake_lease("chat-b", 1).unwrap();
    assert_eq!(restored.claim_next_wake("chat-b").unwrap(), None);
}

#[test]
fn wake_revalidates_authority_and_stop_cannot_replay_claimed_work() {
    let (mut workspace, container, director, worker) = setup();
    workspace
        .set_link(container, director, worker, true, true, "approve", 12)
        .unwrap();
    let pending = workspace
        .send("chat-a", worker, "directive", "Await approval", 100)
        .unwrap()
        .0;
    workspace.grant_wake_lease("chat-b", 2).unwrap();
    assert_eq!(workspace.claim_next_wake("chat-b").unwrap(), None);
    workspace.decide(container, pending, true, 110).unwrap();
    assert_eq!(workspace.claim_next_wake("chat-b").unwrap(), Some(pending));
    workspace.validate_wake_claim("chat-b", pending).unwrap();
    workspace
        .set_link(container, director, worker, false, false, "block", 12)
        .unwrap();
    assert!(workspace.validate_wake_claim("chat-b", pending).is_err());
    workspace.stop_wake_lease("chat-b").unwrap();
    assert!(workspace.validate_wake_claim("chat-b", pending).is_err());
    assert_eq!(workspace.claim_next_wake("chat-b").unwrap(), None);
    workspace
        .set_link(container, director, worker, true, true, "allow", 12)
        .unwrap();
    workspace.grant_wake_lease("chat-b", 1).unwrap();
    assert_eq!(workspace.claim_next_wake("chat-b").unwrap(), None);
    let report = workspace
        .send("chat-a", worker, "report", "Non-command", 120)
        .unwrap()
        .0;
    assert_eq!(workspace.claim_next_wake("chat-b").unwrap(), None);
    assert!(
        workspace.containers[0]
            .messages
            .iter()
            .any(|message| message.id == report && message.status == "delivered")
    );
}

#[test]
fn malformed_wake_claim_history_fails_closed_on_reopen() {
    let (mut workspace, container, director, worker) = setup();
    workspace
        .set_link(container, director, worker, true, true, "allow", 12)
        .unwrap();
    let first = workspace
        .send("chat-a", worker, "directive", "Task", 100)
        .unwrap()
        .0;
    workspace.grant_wake_lease("chat-b", 1).unwrap();
    assert_eq!(workspace.claim_next_wake("chat-b").unwrap(), Some(first));
    let path = std::env::temp_dir().join(format!(
        "chatarium-wake-validation-{}.json",
        std::process::id()
    ));
    workspace.save_atomic(&path).unwrap();
    let mut root: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    root["wake_leases"][0]["claimed_message_ids"] = json!([first, first]);
    fs::write(&path, root.to_string()).unwrap();
    assert!(Workspace::load(&path).is_err());
    let _ = fs::remove_file(path);
}

#[test]
fn separate_containers_are_isolated_and_allow_cycles_explicitly() {
    let (mut s, c, a, b) = setup();
    let d = s.create_container("Research").unwrap();
    let x = s.attach(d, "chat-c", "Reader", "Analyst").unwrap();
    assert!(s.set_link(c, a, x, true, true, "allow", 1).is_err());
    assert!(
        s.send("chat-a", x, "directive", "not a member", 10)
            .is_err()
    );
    s.set_link(c, a, b, true, true, "allow", 1).unwrap();
    s.set_link(c, b, a, false, true, "allow", 1).unwrap();
    assert_eq!(
        s.send("chat-a", b, "directive", "Ship", 11).unwrap().1,
        "delivered"
    );
}
