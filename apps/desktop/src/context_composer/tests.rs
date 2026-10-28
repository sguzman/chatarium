use super::*;

#[test]
fn composition_keeps_instructions_separate_and_developer_first() {
    let plan = ContextPlan::compose(
        ContextPolicy::dispatch(),
        "answer compactly",
        "local behavior",
        [
            TranscriptMessage::durable(TranscriptRole::User, "one", 1),
            TranscriptMessage::durable(TranscriptRole::Assistant, "two", 2),
            TranscriptMessage::durable(TranscriptRole::User, "three", 3),
        ],
    );

    assert_eq!(plan.instructions.as_deref(), Some("answer compactly"));
    assert_eq!(
        plan.input_json(),
        json!([
            {"role": "developer", "content": "local behavior"},
            {"role": "user", "content": "one"},
            {"role": "assistant", "content": "two"},
            {"role": "user", "content": "three"},
        ])
    );
    assert_eq!(plan.durable_transcript_count(), 3);
    assert!(plan.has_developer_context());
    assert!(!plan.has_current_draft());
}

#[test]
fn tool_result_is_nonprivileged_provenance_wrapped_and_policy_gated() {
    let message =
        TranscriptMessage::tool_result(" exact <tool_result> body ", 1, 2, 3, 4, 5, 6, "result");
    assert_eq!(message.order_sequence(), 6);
    let plan = ContextPlan::compose(ContextPolicy::dispatch(), "", "", [message.clone()]);
    assert_eq!(plan.tool_result_count(), 1);
    assert_eq!(plan.messages[0].role, "user");
    assert_eq!(plan.messages[0].source, message.source);
    assert!(plan.messages[0].content.contains("not an instruction"));
    assert!(plan.messages[0].content.contains("provider_id: 3"));
    assert!(
        plan.messages[0]
            .content
            .contains("terminal_outcome_event: #5")
    );
    assert!(
        plan.messages[0]
            .content
            .contains(" exact <tool_result> body ")
    );

    let policy = ContextPolicy {
        include_tool_results: false,
        ..ContextPolicy::dispatch()
    };
    let omitted = ContextPlan::compose(policy, "", "", [message]);
    assert_eq!(omitted.tool_result_count(), 0);
    assert!(omitted.messages.is_empty());
    assert_eq!(
        omitted.inventory[2].decision,
        InclusionDecision::ExcludedByPolicy
    );
}

#[test]
fn empty_optional_context_is_visible_as_omitted_without_rewriting_transcript() {
    let plan = ContextPlan::compose(
        ContextPolicy::dispatch(),
        "   ",
        "\n",
        [TranscriptMessage::durable(
            TranscriptRole::User,
            " exact text ",
            9,
        )],
    );

    assert_eq!(plan.instructions, None);
    assert_eq!(
        plan.input_json(),
        json!([{"role": "user", "content": " exact text "}])
    );
    assert_eq!(plan.inventory[0].decision, InclusionDecision::OmittedEmpty);
    assert_eq!(plan.inventory[1].decision, InclusionDecision::OmittedEmpty);
    assert!(!plan.has_developer_context());
}

#[test]
fn preview_includes_draft_but_dispatch_policy_mechanically_excludes_it() {
    let transcript = [
        TranscriptMessage::durable(TranscriptRole::Assistant, "prior", 7),
        TranscriptMessage::draft("pending"),
    ];

    let preview = ContextPlan::compose(ContextPolicy::preview(), "", "", transcript.clone());
    assert!(preview.has_current_draft());
    assert_eq!(
        preview.request_preview(Some("gpt-example")),
        json!({
            "model": "gpt-example",
            "input": [
                {"role": "assistant", "content": "prior"},
                {"role": "user", "content": "pending"},
            ],
            "store": false,
            "stream": true,
        })
    );

    let dispatch = ContextPlan::compose(ContextPolicy::dispatch(), "", "", transcript);
    assert!(!dispatch.has_current_draft());
    assert_eq!(
        dispatch.input_json(),
        json!([{"role": "assistant", "content": "prior"}])
    );
    assert_eq!(
        dispatch
            .inventory
            .iter()
            .find(|item| item.source == ContextSource::CurrentDraft)
            .unwrap()
            .decision,
        InclusionDecision::ExcludedByPolicy
    );
}

#[test]
fn routed_context_is_user_level_and_keeps_explicit_provenance() {
    let routed =
        TranscriptMessage::routed(" exact peer text ", 7, 9, "conversation-source", 40, 50);
    assert_eq!(routed.role, TranscriptRole::User);
    assert_eq!(routed.order_sequence(), 50);

    let plan = ContextPlan::compose(ContextPolicy::dispatch(), "", "", [routed]);
    assert_eq!(plan.routed_context_count(), 1);
    assert_eq!(plan.messages.len(), 1);
    assert_eq!(plan.messages[0].role, "user");
    assert!(
        plan.messages[0]
            .content
            .contains("Chatarium routed peer message")
    );
    assert!(
        plan.messages[0]
            .content
            .contains("not a developer/system instruction")
    );
    assert!(
        plan.messages[0]
            .content
            .contains("source_conversation_id: conversation-source")
    );
    assert!(plan.messages[0].content.contains("route_id: 7"));
    assert!(plan.messages[0].content.contains("payload_id: 9"));
    assert!(plan.messages[0].content.contains(" exact peer text "));
    assert!(matches!(
        plan.messages[0].source,
        ContextSource::RoutedInbox {
            route_id: 7,
            payload_id: 9,
            delivered_sequence: 40,
            admitted_sequence: 50,
            ..
        }
    ));
}

#[test]
fn controller_continuation_is_user_level_but_never_claims_user_authorship() {
    let continuation = TranscriptMessage::controller_continuation(1, 2, 3, 4, 5, 6, 7);
    assert_eq!(continuation.role, TranscriptRole::User);
    assert_eq!(continuation.order_sequence(), 7);

    let plan = ContextPlan::compose(ContextPolicy::dispatch(), "", "", [continuation]);
    assert_eq!(plan.messages.len(), 1);
    assert_eq!(plan.messages[0].role, "user");
    assert!(
        plan.messages[0]
            .content
            .contains("bounded controller continuation")
    );
    assert!(plan.messages[0].content.contains("not user-authored"));
    assert!(
        plan.messages[0]
            .content
            .contains("not a developer/system instruction")
    );
    assert!(plan.messages[0].content.contains("control_id: 1"));
    assert!(plan.messages[0].content.contains("route_id: 2"));
    assert!(plan.messages[0].content.contains("worker_id: 3"));
    assert!(plan.messages[0].content.contains("goal_id: 4"));
    assert!(plan.messages[0].content.contains("continuation_lease: 5"));
    assert!(plan.messages[0].content.contains("permit_ordinal: 6"));
    assert!(
        plan.messages[0]
            .content
            .contains("Continue working on the current goal")
    );
    assert!(matches!(
        plan.messages[0].source,
        ContextSource::ControllerContinuation {
            control_id: 1,
            route_id: 2,
            worker_id: 3,
            goal_id: 4,
            lease_id: 5,
            permit_ordinal: 6,
            started_sequence: 7,
        }
    ));
}

#[test]
fn coordination_activation_keeps_non_authored_source_and_message_ids() {
    let trigger = TranscriptMessage::coordination_activation("wake-turn", 12, vec![4, 5]);
    assert_eq!(trigger.role, TranscriptRole::User);
    assert_eq!(trigger.order_sequence(), 12);
    let plan = ContextPlan::compose(ContextPolicy::dispatch(), "", "", [trigger]);
    assert_eq!(plan.messages.len(), 1);
    assert!(plan.messages[0].content.contains("non-user-authored"));
    assert!(plan.messages[0].content.contains("message_ids: [4, 5]"));
    assert!(
        plan.messages[0]
            .content
            .contains("not merely written in this chat")
    );
    assert!(matches!(
        plan.messages[0].source,
        ContextSource::CoordinationActivation {
            started_sequence: 12,
            ..
        }
    ));
}

#[test]
fn controller_worker_result_is_user_level_with_explicit_result_provenance() {
    let result = TranscriptMessage::controller_worker_result(
        1,
        2,
        3,
        4,
        5,
        6,
        "continuation",
        "worker output",
    );
    assert_eq!(result.role, TranscriptRole::User);
    assert_eq!(result.order_sequence(), 6);

    let plan = ContextPlan::compose(ContextPolicy::dispatch(), "", "", [result]);
    assert_eq!(plan.controller_worker_result_count(), 1);
    assert_eq!(plan.messages[0].role, "user");
    assert!(
        plan.messages[0]
            .content
            .contains("controller worker result")
    );
    assert!(plan.messages[0].content.contains("not user-authored"));
    assert!(
        plan.messages[0]
            .content
            .contains("not a developer/system instruction")
    );
    assert!(
        plan.messages[0]
            .content
            .contains("result_kind: continuation")
    );
    assert!(plan.messages[0].content.contains("result_event: #5"));
    assert!(
        plan.messages[0]
            .content
            .contains("context_admitted_event: #6")
    );
    assert!(plan.messages[0].content.contains("worker output"));
    assert!(matches!(
        plan.messages[0].source,
        ContextSource::ControllerWorkerResult {
            control_id: 1,
            route_id: 2,
            worker_id: 3,
            goal_id: 4,
            result_sequence: 5,
            admitted_sequence: 6,
            ..
        }
    ));
}

#[test]
fn controller_coordination_is_user_level_without_control_authority() {
    let coordination = TranscriptMessage::controller_coordination(11, "coordination-turn", 42);
    assert_eq!(coordination.role, TranscriptRole::User);
    assert_eq!(coordination.order_sequence(), 42);

    let plan = ContextPlan::compose(ContextPolicy::dispatch(), "", "", [coordination]);
    assert_eq!(plan.controller_coordination_count(), 1);
    assert_eq!(plan.messages.len(), 1);
    assert_eq!(plan.messages[0].role, "user");
    assert!(
        plan.messages[0]
            .content
            .contains("controller coordination turn")
    );
    assert!(plan.messages[0].content.contains("not user-authored"));
    assert!(
        plan.messages[0]
            .content
            .contains("not a developer/system instruction")
    );
    assert!(
        plan.messages[0]
            .content
            .contains("Do not issue or execute worker controls")
    );
    assert!(plan.messages[0].content.contains("do not mutate lifecycle"));
    assert!(
        plan.messages[0]
            .content
            .contains("do not assume continuation authority")
    );
    assert!(
        plan.messages[0]
            .content
            .contains("output_contract: suggestion_candidates_v1")
    );
    assert!(
        plan.messages[0]
            .content
            .contains("\"suggestion_candidates\"")
    );
    assert!(
        plan.messages[0]
            .content
            .contains("\"basis_result_route_id\"")
    );
    assert!(
        plan.messages[0]
            .content
            .contains("explicit user acceptance")
    );
    assert!(matches!(
        plan.messages[0].source,
        ContextSource::ControllerCoordination {
            controller_session_id: 11,
            started_sequence: 42,
            ..
        }
    ));
}

#[test]
fn controller_coordination_can_be_excluded_by_context_policy() {
    let policy = ContextPolicy {
        include_controller_coordination: false,
        ..ContextPolicy::dispatch()
    };
    let coordination = TranscriptMessage::controller_coordination(1, "turn", 2);
    let plan = ContextPlan::compose(policy, "", "", [coordination]);

    assert_eq!(plan.controller_coordination_count(), 0);
    assert!(plan.messages.is_empty());
    let item = plan
        .inventory
        .iter()
        .find(|item| matches!(item.source, ContextSource::ControllerCoordination { .. }))
        .unwrap();
    assert_eq!(item.decision, InclusionDecision::ExcludedByPolicy);
}

#[test]
fn controller_coordination_result_is_user_level_with_explicit_provenance() {
    let result = TranscriptMessage::controller_coordination_result(
        "coordination-turn",
        50,
        60,
        "completed",
        "synthesis",
    );
    assert_eq!(result.role, TranscriptRole::User);
    assert_eq!(result.order_sequence(), 60);

    let plan = ContextPlan::compose(ContextPolicy::dispatch(), "", "", [result]);
    assert_eq!(plan.controller_coordination_result_count(), 1);
    assert_eq!(plan.messages.len(), 1);
    assert_eq!(plan.messages[0].role, "user");
    assert!(
        plan.messages[0]
            .content
            .contains("controller coordination result")
    );
    assert!(plan.messages[0].content.contains("not user-authored"));
    assert!(
        plan.messages[0]
            .content
            .contains("not a developer/system instruction")
    );
    assert!(
        plan.messages[0]
            .content
            .contains("coordination_turn_id: coordination-turn")
    );
    assert!(plan.messages[0].content.contains("outcome: completed"));
    assert!(plan.messages[0].content.contains("result_event: #50"));
    assert!(
        plan.messages[0]
            .content
            .contains("context_admitted_event: #60")
    );
    assert!(plan.messages[0].content.contains("synthesis"));
    assert!(matches!(
        plan.messages[0].source,
        ContextSource::ControllerCoordinationResult {
            result_sequence: 50,
            admitted_sequence: 60,
            ..
        }
    ));
}

#[test]
fn controller_coordination_result_can_be_excluded_by_context_policy() {
    let policy = ContextPolicy {
        include_controller_coordination_results: false,
        ..ContextPolicy::dispatch()
    };
    let result =
        TranscriptMessage::controller_coordination_result("turn", 2, 3, "completed", "output");
    let plan = ContextPlan::compose(policy, "", "", [result]);

    assert_eq!(plan.controller_coordination_result_count(), 0);
    assert!(plan.messages.is_empty());
    let item = plan
        .inventory
        .iter()
        .find(|item| {
            matches!(
                item.source,
                ContextSource::ControllerCoordinationResult { .. }
            )
        })
        .unwrap();
    assert_eq!(item.decision, InclusionDecision::ExcludedByPolicy);
}

#[test]
fn controller_worker_result_can_be_excluded_by_context_policy() {
    let policy = ContextPolicy {
        include_controller_worker_results: false,
        ..ContextPolicy::dispatch()
    };
    let result = TranscriptMessage::controller_worker_result(1, 2, 3, 4, 5, 6, "status", "working");
    let plan = ContextPlan::compose(policy, "", "", [result]);

    assert_eq!(plan.controller_worker_result_count(), 0);
    assert!(plan.messages.is_empty());
    let item = plan
        .inventory
        .iter()
        .find(|item| matches!(item.source, ContextSource::ControllerWorkerResult { .. }))
        .unwrap();
    assert_eq!(item.decision, InclusionDecision::ExcludedByPolicy);
}

#[test]
fn budget_preserves_recent_turn_checkpoint_and_complete_source_ledger() {
    let mut transcript = (1..=30)
        .map(|sequence| {
            TranscriptMessage::durable(
                if sequence % 2 == 0 {
                    TranscriptRole::Assistant
                } else {
                    TranscriptRole::User
                },
                format!("event {sequence}: {}", "a".repeat(80)),
                sequence,
            )
        })
        .collect::<Vec<_>>();
    transcript.push(TranscriptMessage {
        role: TranscriptRole::User,
        text: "verified historical checkpoint".to_owned(),
        source: ContextSource::OpenVikingCheckpoint {
            source_conversation_id: "owner".to_owned(),
            source_turn_id: "turn-3".to_owned(),
            source_journal_sequence: 3,
            uri: "viking://~/chatarium/conversations/owner/checkpoints/turn-3.md".to_owned(),
        },
    });
    let all = ContextPlan::compose(ContextPolicy::dispatch(), "", "", transcript);
    let bounded = all.clone().within_utf8_budget(1700).unwrap();
    assert!(bounded.size.utf8_bytes <= 1700);
    assert!(bounded.size.utf8_bytes < all.size.utf8_bytes);
    assert!(bounded.size.omitted_items > 0);
    assert!(
        bounded
            .messages
            .iter()
            .any(|m| m.content.contains("event 30:"))
    );
    assert_eq!(bounded.openviking_checkpoint_count(), 1);
    assert!(bounded.inventory.iter().any(|item| {
        item.decision == InclusionDecision::ExcludedByBudget
            && matches!(item.source, ContextSource::DurableTranscript { .. })
    }));
    let included = bounded
        .inventory
        .iter()
        .filter(|item| item.decision == InclusionDecision::Included)
        .map(|item| item.utf8_bytes)
        .sum::<usize>();
    assert_eq!(bounded.size.utf8_bytes, included);
}

#[test]
fn long_context_keeps_recent_turns_and_verified_checkpoint_before_old_turns() {
    // Without an early checkpoint reservation, the sixteen-message
    // recency pass would fill this budget and exclude historical memory.
    let mut transcript = (1..=20)
        .map(|sequence| {
            TranscriptMessage::durable(
                if sequence % 2 == 0 {
                    TranscriptRole::Assistant
                } else {
                    TranscriptRole::User
                },
                format!("durable {sequence}: {}", "x".repeat(90)),
                sequence,
            )
        })
        .collect::<Vec<_>>();
    let evidence = "Previously completed Rust ownership fix. Preserve this decision.";
    transcript.push(TranscriptMessage {
        role: TranscriptRole::User,
        text: evidence.to_owned(),
        source: ContextSource::OpenVikingCheckpoint {
            source_conversation_id: "owner".to_owned(),
            source_turn_id: "checkpoint-8".to_owned(),
            source_journal_sequence: 8,
            uri: "viking://~/chatarium/conversations/owner/checkpoints/checkpoint-8.md".to_owned(),
        },
    });
    let plan = ContextPlan::compose(ContextPolicy::dispatch(), "", "", transcript);
    let bounded = plan.within_utf8_budget(515).unwrap();
    assert!(bounded.size.utf8_bytes <= 515);
    for sequence in 17..=20 {
        assert!(bounded.messages.iter().any(|message| {
            message
                .content
                .starts_with(&format!("durable {sequence}: "))
        }));
    }
    assert_eq!(bounded.openviking_checkpoint_count(), 1);
    assert!(
        bounded
            .messages
            .iter()
            .any(|message| message.content == evidence)
    );
    assert!(bounded.inventory.iter().any(|item| {
        item.decision == InclusionDecision::ExcludedByBudget
            && matches!(
                item.source,
                ContextSource::DurableTranscript { sequence: 16 }
            )
    }));
    assert!(bounded.inventory.iter().any(|item| {
        item.decision == InclusionDecision::Included
            && matches!(item.source, ContextSource::OpenVikingCheckpoint { .. })
    }));
}

#[test]
fn oversized_checkpoint_cannot_displace_current_turn_or_block_other_history() {
    let transcript = vec![
        TranscriptMessage::durable(TranscriptRole::User, "older", 1),
        TranscriptMessage::durable(TranscriptRole::Assistant, "immediate reply", 2),
        TranscriptMessage::durable(TranscriptRole::User, "current question", 3),
        TranscriptMessage {
            role: TranscriptRole::User,
            text: "large".repeat(150),
            source: ContextSource::OpenVikingCheckpoint {
                source_conversation_id: "owner".to_owned(),
                source_turn_id: "checkpoint-1".to_owned(),
                source_journal_sequence: 1,
                uri: "viking://~/chatarium/conversations/owner/checkpoints/checkpoint-1.md"
                    .to_owned(),
            },
        },
    ];
    let bounded = ContextPlan::compose(ContextPolicy::dispatch(), "", "", transcript)
        .within_utf8_budget(100)
        .unwrap();
    assert_eq!(bounded.openviking_checkpoint_count(), 0);
    assert!(
        bounded
            .messages
            .iter()
            .any(|m| m.content == "current question")
    );
    assert!(
        bounded
            .messages
            .iter()
            .any(|m| m.content == "immediate reply")
    );
    assert!(bounded.messages.iter().any(|m| m.content == "older"));
    assert!(bounded.inventory.iter().any(|item| {
        item.decision == InclusionDecision::ExcludedByBudget
            && matches!(item.source, ContextSource::OpenVikingCheckpoint { .. })
    }));
}

#[test]
fn budget_does_not_silently_drop_latest_user_or_one_shot() {
    let transcript = [
        TranscriptMessage::durable(TranscriptRole::User, "u".repeat(180), 1),
        TranscriptMessage::durable(TranscriptRole::Assistant, "assistant", 2),
        TranscriptMessage::durable(TranscriptRole::User, "current".repeat(120), 3),
    ];
    let plan = ContextPlan::compose(ContextPolicy::dispatch(), "", "", transcript);
    assert!(plan.within_utf8_budget(100).is_err());

    let plan = ContextPlan::compose(
        ContextPolicy::dispatch(),
        "",
        "",
        [TranscriptMessage::local_memory_one_shot(
            &"x".repeat(160),
            1,
            "owner",
            3,
            4,
            Some(5),
        )],
    );
    assert!(plan.within_utf8_budget(100).is_err());
}

#[test]
fn budget_is_identity_when_everything_fits_and_omits_without_mutating_text() {
    let plan = ContextPlan::compose(
        ContextPolicy::dispatch(),
        "top",
        "developer",
        [TranscriptMessage::durable(TranscriptRole::User, "hello", 2)],
    );
    assert_eq!(plan.clone().within_utf8_budget(100).unwrap(), plan);
}

#[test]
fn verified_openviking_checkpoint_is_user_level_and_keeps_source_provenance() {
    let item = crate::openviking_memory_delivery::RetrievedCheckpoint {
        source_conversation_id: "owner".to_owned(),
        source_turn_id: "turn-8".to_owned(),
        source_journal_sequence: 77,
        uri: "viking://~/chatarium/conversations/owner/checkpoints/turn-8.md".to_owned(),
        text: "Durable decision".to_owned(),
    };
    let message = TranscriptMessage::verified_openviking_checkpoint(&item, "owner").unwrap();
    assert_eq!(message.role, TranscriptRole::User);
    assert_eq!(message.order_sequence(), u64::MAX - 1);
    let plan = ContextPlan::compose(ContextPolicy::dispatch(), "", "", [message]);
    assert_eq!(plan.openviking_checkpoint_count(), 1);
    assert_eq!(plan.messages.len(), 1);
    assert!(
        plan.messages[0]
            .content
            .contains("not a developer/system instruction")
    );
    assert!(
        plan.messages[0]
            .content
            .contains("source_journal_sequence: #77")
    );
    assert!(plan.messages[0].content.contains("Durable decision"));

    let excluded = ContextPlan::compose(
        ContextPolicy {
            include_openviking_checkpoints: false,
            ..ContextPolicy::dispatch()
        },
        "",
        "",
        [TranscriptMessage::verified_openviking_checkpoint(&item, "owner").unwrap()],
    );
    assert_eq!(excluded.openviking_checkpoint_count(), 0);
    assert!(excluded.messages.is_empty());
    let item = excluded
        .inventory
        .iter()
        .find(|item| matches!(item.source, ContextSource::OpenVikingCheckpoint { .. }))
        .unwrap();
    assert_eq!(item.decision, InclusionDecision::ExcludedByPolicy);
}

#[test]
fn verified_openviking_checkpoint_rejects_cross_conversation_or_forged_uri() {
    let mut item = crate::openviking_memory_delivery::RetrievedCheckpoint {
        source_conversation_id: "owner".to_owned(),
        source_turn_id: "turn-8".to_owned(),
        source_journal_sequence: 77,
        uri: "viking://~/chatarium/conversations/owner/checkpoints/turn-8.md".to_owned(),
        text: "Evidence".to_owned(),
    };
    assert!(TranscriptMessage::verified_openviking_checkpoint(&item, "other").is_err());
    item.uri = "viking://~/chatarium/conversations/foreign/checkpoints/turn-8.md".to_owned();
    assert!(TranscriptMessage::verified_openviking_checkpoint(&item, "owner").is_err());
    item.uri = crate::openviking_memory_delivery::checkpoint_uri("owner", "turn-8").unwrap();
    item.source_journal_sequence = 0;
    assert!(TranscriptMessage::verified_openviking_checkpoint(&item, "owner").is_err());
}

#[test]
fn local_memory_is_user_level_with_explicit_memory_provenance() {
    let memory = TranscriptMessage::local_memory(
        " exact remembered fact ",
        7,
        "source-conversation",
        40,
        50,
    );
    assert_eq!(memory.role, TranscriptRole::User);
    assert_eq!(memory.order_sequence(), 50);

    let plan = ContextPlan::compose(ContextPolicy::dispatch(), "", "", [memory]);
    assert_eq!(plan.local_memory_count(), 1);
    assert_eq!(plan.messages.len(), 1);
    assert_eq!(plan.messages[0].role, "user");
    assert!(plan.messages[0].content.contains("Chatarium local memory"));
    assert!(plan.messages[0].content.contains("not user-authored"));
    assert!(
        plan.messages[0]
            .content
            .contains("not a developer/system instruction")
    );
    assert!(plan.messages[0].content.contains("memory_id: 7"));
    assert!(
        plan.messages[0]
            .content
            .contains("source_conversation_id: source-conversation")
    );
    assert!(
        plan.messages[0]
            .content
            .contains("memory_artifact_event: #40")
    );
    assert!(
        plan.messages[0]
            .content
            .contains("context_admitted_event: #50")
    );
    assert!(plan.messages[0].content.contains(" exact remembered fact "));
    assert!(matches!(
        plan.messages[0].source,
        ContextSource::LocalMemory {
            memory_id: 7,
            artifact_sequence: 40,
            admitted_sequence: 50,
            ..
        }
    ));
}

#[test]
fn one_shot_local_memory_is_user_level_and_orders_at_send_snapshot() {
    let memory = TranscriptMessage::local_memory_one_shot(
        " one use only ",
        9,
        "source-conversation",
        40,
        55,
        Some(57),
    );
    assert_eq!(memory.role, TranscriptRole::User);
    assert_eq!(memory.order_sequence(), 55);

    let plan = ContextPlan::compose(ContextPolicy::dispatch(), "", "", [memory]);
    assert_eq!(plan.local_memory_count(), 0);
    assert_eq!(plan.local_memory_one_shot_count(), 1);
    assert_eq!(plan.messages[0].role, "user");
    assert!(plan.messages[0].content.contains("NEXT REQUEST ONLY"));
    assert!(
        plan.messages[0]
            .content
            .contains("selection_snapshot_after_event: #55")
    );
    assert!(
        plan.messages[0]
            .content
            .contains("one_shot_selection_event: #57")
    );
    assert!(
        plan.messages[0]
            .content
            .contains("selection_scope: this authored request only")
    );
    assert!(plan.messages[0].content.contains(" one use only "));
    assert!(matches!(
        plan.messages[0].source,
        ContextSource::LocalMemoryOneShot {
            memory_id: 9,
            artifact_sequence: 40,
            snapshot_after_sequence: 55,
            selection_sequence: Some(57),
            ..
        }
    ));
}

#[test]
fn local_memory_can_be_excluded_by_context_policy() {
    let policy = ContextPolicy {
        include_local_memory: false,
        ..ContextPolicy::dispatch()
    };
    let memory = TranscriptMessage::local_memory("memory", 1, "source", 2, 3);
    let plan = ContextPlan::compose(policy, "", "", [memory]);

    assert_eq!(plan.local_memory_count(), 0);
    assert!(plan.messages.is_empty());
    let item = plan
        .inventory
        .iter()
        .find(|item| matches!(item.source, ContextSource::LocalMemory { .. }))
        .unwrap();
    assert_eq!(item.decision, InclusionDecision::ExcludedByPolicy);
}

#[test]
fn routed_context_can_be_excluded_by_context_policy() {
    let policy = ContextPolicy {
        include_routed_context: false,
        ..ContextPolicy::dispatch()
    };
    let routed = TranscriptMessage::routed("peer", 1, 2, "source", 3, 4);
    let plan = ContextPlan::compose(policy, "", "", [routed]);

    assert_eq!(plan.routed_context_count(), 0);
    assert!(plan.messages.is_empty());
    let item = plan
        .inventory
        .iter()
        .find(|item| matches!(item.source, ContextSource::RoutedInbox { .. }))
        .unwrap();
    assert_eq!(item.decision, InclusionDecision::ExcludedByPolicy);
}

#[test]
fn size_ledger_counts_exact_included_content_units_not_tokens() {
    let plan = ContextPlan::compose(
        ContextPolicy::dispatch(),
        "abc",
        "é",
        [TranscriptMessage::durable(TranscriptRole::User, "x\ny", 42)],
    );

    assert_eq!(
        plan.size,
        ContextSizeLedger {
            included_items: 3,
            omitted_items: 0,
            utf8_bytes: 8,
            unicode_scalars: 7,
            lines: 4,
        }
    );
}

#[test]
fn capability_gate_requires_ready_contract_for_exact_profile_and_model() {
    use std::collections::BTreeMap;

    let capabilities = CapabilitySlot::ALL
        .into_iter()
        .map(|slot| (slot.contract_name().to_owned(), "supported".to_owned()))
        .collect::<BTreeMap<_, _>>();
    let contract = LoadedContract {
        state: "ready".to_owned(),
        profile_id: Some("profile-1".to_owned()),
        model: "gpt-example".to_owned(),
        probe_generated_unix_ms: 1234,
        capabilities,
    };

    let active =
        CapabilityGate::from_contract(Some(&contract), Some("profile-1"), Some("gpt-example"));
    assert!(active.allows(CapabilitySlot::WebSearch));
    assert!(active.allows(CapabilitySlot::Reasoning));

    let wrong_model =
        CapabilityGate::from_contract(Some(&contract), Some("profile-1"), Some("gpt-other"));
    assert!(!wrong_model.allows(CapabilitySlot::WebSearch));
    assert!(
        wrong_model
            .admissions
            .iter()
            .all(|admission| { admission.state == CapabilityAdmissionState::BlockedContract })
    );
}

#[test]
fn focused_web_search_evidence_cannot_unlock_other_capabilities() {
    let mut capabilities = std::collections::BTreeMap::new();
    capabilities.insert("web_search".to_owned(), "supported".to_owned());
    capabilities.insert("reasoning".to_owned(), "supported".to_owned());
    let contract = LoadedContract {
        state: "incomplete".to_owned(),
        profile_id: Some("p".to_owned()),
        model: "gpt-6.1-sol".to_owned(),
        probe_generated_unix_ms: 5,
        capabilities,
    };
    let gate = CapabilityGate::from_contract(Some(&contract), Some("p"), Some("gpt-6.1-sol"));
    assert!(gate.allows(CapabilitySlot::WebSearch));
    assert!(!gate.allows(CapabilitySlot::Reasoning));
    let wrong = CapabilityGate::from_contract(Some(&contract), Some("p"), Some("other"));
    assert!(!wrong.allows(CapabilitySlot::WebSearch));
}

#[test]
fn capability_gate_preserves_unsupported_route() {
    use std::collections::BTreeMap;

    let mut capabilities = CapabilitySlot::ALL
        .into_iter()
        .map(|slot| (slot.contract_name().to_owned(), "supported".to_owned()))
        .collect::<BTreeMap<_, _>>();
    capabilities.insert("web_search".to_owned(), "unsupported_route".to_owned());
    let contract = LoadedContract {
        state: "ready".to_owned(),
        profile_id: Some("profile-1".to_owned()),
        model: "gpt-example".to_owned(),
        probe_generated_unix_ms: 1234,
        capabilities,
    };

    let gate =
        CapabilityGate::from_contract(Some(&contract), Some("profile-1"), Some("gpt-example"));
    assert!(!gate.allows(CapabilitySlot::WebSearch));
    assert_eq!(
        gate.admissions
            .iter()
            .find(|admission| admission.slot == CapabilitySlot::WebSearch)
            .unwrap()
            .state,
        CapabilityAdmissionState::UnsupportedRoute
    );
}

#[test]
fn provenance_retains_durable_sequence_without_entering_wire_input() {
    let plan = ContextPlan::compose(
        ContextPolicy::dispatch(),
        "",
        "",
        [TranscriptMessage::durable(
            TranscriptRole::User,
            "hello",
            42,
        )],
    );

    assert_eq!(
        plan.messages[0].source,
        ContextSource::DurableTranscript { sequence: 42 }
    );
    assert_eq!(
        plan.input_json(),
        json!([{"role": "user", "content": "hello"}])
    );
}
