use super::*;
use crate::context_composer::{ContextPolicy, TranscriptMessage};
use chatarium_core::{EventKind, LocalConversationId};

fn admitted_source(call_id: u64) -> ContextSource {
    ContextSource::ToolResult {
        call_id,
        route_id: call_id + 20,
        provider_id: 3,
        source_session_id: 4,
        outcome_sequence: call_id + 100,
        admitted_sequence: call_id + 200,
        outcome_kind: "result".to_owned(),
    }
}

fn plan_with_tool(source: &ContextSource) -> ContextPlan {
    let ContextSource::ToolResult {
        call_id,
        route_id,
        provider_id,
        source_session_id,
        outcome_sequence,
        admitted_sequence,
        outcome_kind,
    } = source
    else {
        panic!("test requires tool result");
    };
    ContextPlan::compose(
        ContextPolicy::dispatch(),
        "",
        "",
        [TranscriptMessage::tool_result(
            "secret adapter bytes",
            *call_id,
            *route_id,
            *provider_id,
            *source_session_id,
            *outcome_sequence,
            *admitted_sequence,
            outcome_kind.as_str(),
        )],
    )
}

fn manifest_for_reverse_test(
    sequence: u64,
    owner: LocalConversationId,
    mut listed: Vec<ListedToolEvidence>,
    truncated: bool,
) -> RecordedDispatchManifest {
    if truncated {
        // Real truncated manifests retain exactly 32 listed identities.
        // The queried call can be absent while eligible, included or
        // omitted among the unlisted frozen admissions.
        let mut next = 1000;
        while listed.len() < MAX_MANIFEST_TOOL_ROWS {
            listed.push(reverse_evidence(next, next + 5000, false));
            next += 1;
        }
    }
    RecordedDispatchManifest {
        sequence,
        turn_id: format!("turn-{sequence}"),
        request_class: "authored".to_owned(),
        conversation_id: owner.to_string(),
        eligible_total: listed.len() + usize::from(truncated) * 10,
        included_total: listed.iter().filter(|evidence| evidence.included).count(),
        omitted_total: listed.iter().filter(|evidence| !evidence.included).count()
            + usize::from(truncated) * 10,
        included_items: 1,
        omitted_items: 0,
        included_bytes: 10,
        listed,
        truncated,
        transport: DispatchTransportTrace::default(),
    }
}

fn reverse_evidence(call_id: u64, admitted_sequence: u64, included: bool) -> ListedToolEvidence {
    ListedToolEvidence {
        call_id,
        route_id: call_id + 100,
        provider_id: 9,
        source_session_id: 4,
        outcome_sequence: call_id + 200,
        admitted_sequence,
        outcome_kind: "result".to_owned(),
        included,
        composition_reason: Some(if included {
            ToolCompositionReason::Included
        } else {
            ToolCompositionReason::NotSelectedForDispatch
        }),
    }
}

#[test]
fn reverse_lookup_distinguishes_included_omitted_and_complete_negative() {
    let owner = LocalConversationId::new();
    let history = [
        manifest_for_reverse_test(40, owner, vec![reverse_evidence(7, 35, true)], false),
        manifest_for_reverse_test(30, owner, vec![reverse_evidence(7, 25, false)], false),
        manifest_for_reverse_test(20, owner, vec![reverse_evidence(8, 15, true)], false),
        manifest_for_reverse_test(10, owner, vec![], false),
    ];
    let report = reverse_tool_provenance(&history, 7).unwrap();
    assert_eq!(report.included, 1);
    assert_eq!(report.admitted_but_omitted, 1);
    assert_eq!(report.not_eligible_in_complete_manifests, 2);
    assert_eq!(report.uncertain_truncated, 0);
    assert_eq!(report.entries.len(), 2);
    assert_eq!(report.entries[0].dispatch.sequence, 40);
    assert_eq!(
        report.entries[0].disposition,
        ToolProvenanceDisposition::Included
    );
    assert_eq!(
        report.entries[1].disposition,
        ToolProvenanceDisposition::AdmittedButOmitted
    );
}

#[test]
fn truncated_absence_is_unknown_even_with_other_known_results() {
    let owner = LocalConversationId::new();
    let history = [
        manifest_for_reverse_test(25, owner, vec![reverse_evidence(8, 20, true)], true),
        manifest_for_reverse_test(20, owner, vec![reverse_evidence(7, 15, true)], false),
        manifest_for_reverse_test(15, owner, vec![], true),
        manifest_for_reverse_test(10, owner, vec![], false),
    ];
    let report = reverse_tool_provenance(&history, 7).unwrap();
    assert_eq!(report.included, 1);
    assert_eq!(report.admitted_but_omitted, 0);
    assert_eq!(report.uncertain_truncated, 2);
    assert_eq!(report.not_eligible_in_complete_manifests, 1);
    assert_eq!(report.entries.len(), 3);
    assert_eq!(
        report.entries[0].disposition,
        ToolProvenanceDisposition::UnknownBecauseManifestTruncated
    );
    assert_eq!(report.entries[0].dispatch.sequence, 25);
    assert_eq!(
        report.entries[1].disposition,
        ToolProvenanceDisposition::Included
    );
    assert_eq!(report.entries[2].dispatch.sequence, 15);
}

#[test]
fn exact_result_identity_must_match_across_dispatches_but_readmission_can_change() {
    let owner = LocalConversationId::new();
    let first = manifest_for_reverse_test(10, owner, vec![reverse_evidence(7, 19, false)], false);
    let second = manifest_for_reverse_test(20, owner, vec![reverse_evidence(7, 25, true)], false);
    assert!(reverse_tool_provenance(&[second, first], 7).is_ok());
    let mut corrupted = reverse_evidence(7, 30, true);
    corrupted.source_session_id = 999;
    let bad = manifest_for_reverse_test(30, owner, vec![corrupted], false);
    let consistent =
        manifest_for_reverse_test(40, owner, vec![reverse_evidence(7, 35, true)], false);
    assert!(reverse_tool_provenance(&[consistent, bad], 7).is_err());
    let mut corrupted = reverse_evidence(7, 30, true);
    corrupted.outcome_sequence += 1;
    let bad = manifest_for_reverse_test(30, owner, vec![corrupted], false);
    let consistent =
        manifest_for_reverse_test(40, owner, vec![reverse_evidence(7, 35, true)], false);
    assert!(reverse_tool_provenance(&[consistent, bad], 7).is_err());
}

#[test]
fn reverse_lookup_rejects_mixed_conversation_and_zero_call() {
    let first = LocalConversationId::new();
    let other = LocalConversationId::new();
    let history = [
        manifest_for_reverse_test(20, first, vec![reverse_evidence(7, 15, true)], false),
        manifest_for_reverse_test(10, other, vec![reverse_evidence(7, 5, true)], false),
    ];
    assert!(reverse_tool_provenance(&history, 7).is_err());
    assert!(reverse_tool_provenance(&history, 0).is_err());
}

#[test]
fn reverse_json_explicitly_preserves_unknown_and_legacy_caveats_without_bodies() {
    let owner = LocalConversationId::new();
    let history = [
        manifest_for_reverse_test(20, owner, vec![reverse_evidence(7, 14, true)], false),
        manifest_for_reverse_test(10, owner, vec![reverse_evidence(8, 4, true)], true),
    ];
    let report = reverse_tool_provenance(&history, 7).unwrap();
    let json = reverse_tool_provenance_json(&report);
    assert_eq!(json["call_id"], 7);
    assert_eq!(json["counts"]["included"], 1);
    assert_eq!(json["counts"]["unknown_due_to_truncation"], 1);
    assert_eq!(json["dispatches"][0]["disposition"], "included");
    assert_eq!(json["dispatches"][1]["disposition"], "unknown_truncated");
    assert_eq!(
        json["limits"]["legacy_unmanifested_requests_are_unknown"],
        true
    );
    let serialized = json.to_string();
    assert!(!serialized.contains("secret adapter bytes"));
    assert!(!serialized.contains("historically typed user message"));
}

#[test]
fn reverse_provenance_selection_identity_is_conversation_scoped() {
    let first = LocalConversationId::new();
    let second = LocalConversationId::new();
    assert_eq!(reverse_query_id(first), reverse_query_id(first));
    assert_ne!(reverse_query_id(first), reverse_query_id(second));
}

#[test]
fn unknown_call_without_truncated_manifest_is_not_falsely_reported_included() {
    let owner = LocalConversationId::new();
    let history = [
        manifest_for_reverse_test(12, owner, vec![], false),
        manifest_for_reverse_test(8, owner, vec![reverse_evidence(9, 4, true)], false),
    ];
    let report = reverse_tool_provenance(&history, 99).unwrap();
    assert_eq!(report.included, 0);
    assert_eq!(report.admitted_but_omitted, 0);
    assert_eq!(report.not_eligible_in_complete_manifests, 2);
    assert!(report.entries.is_empty());
    assert!(report.identity.is_none());
}

#[test]
fn captures_only_actual_composed_tool_sources_without_raw_content() {
    let owner = LocalConversationId::new();
    let source = admitted_source(11);
    let plan = plan_with_tool(&source);
    let manifest = capture(owner, "authored", &[source], &plan).unwrap();
    assert_eq!(manifest["eligible_tool_results"], 1);
    assert_eq!(manifest["included_tool_results"], 1);
    assert_eq!(
        manifest["listed_tool_results"][0]["disposition"],
        "included"
    );
    assert_eq!(manifest["context_included_items"], 3 - 2); // one tool; no instructions or developer text
    assert!(!manifest.to_string().contains("secret adapter bytes"));
}

#[test]
fn specialized_dispatch_reports_admitted_but_not_included() {
    let owner = LocalConversationId::new();
    let plan = ContextPlan::compose(ContextPolicy::dispatch(), "", "", []);
    let manifest = capture(owner, "worker_continuation", &[admitted_source(12)], &plan).unwrap();
    assert_eq!(manifest["eligible_tool_results"], 1);
    assert_eq!(manifest["included_tool_results"], 0);
    assert_eq!(manifest["omitted_tool_results"], 1);
    assert_eq!(manifest["listed_tool_results"][0]["disposition"], "omitted");
}

#[test]
fn capture_explains_included_and_non_selected_tool_results() {
    let owner = LocalConversationId::new();
    let source = admitted_source(17);
    let included = capture(
        owner,
        "authored",
        &[source.clone()],
        &plan_with_tool(&source),
    )
    .unwrap();
    assert_eq!(
        included["listed_tool_results"][0]["composition_reason"],
        "included"
    );
    let not_selected = capture(
        owner,
        "controller_coordination",
        &[source],
        &ContextPlan::compose(ContextPolicy::dispatch(), "", "", []),
    )
    .unwrap();
    assert_eq!(
        not_selected["listed_tool_results"][0]["composition_reason"],
        "not_selected_for_dispatch"
    );
}

#[test]
fn capture_reports_policy_exclusion_without_inventing_inclusion() {
    let owner = LocalConversationId::new();
    let source = admitted_source(7);
    let ContextSource::ToolResult {
        call_id,
        route_id,
        provider_id,
        source_session_id,
        outcome_sequence,
        admitted_sequence,
        outcome_kind,
    } = &source
    else {
        panic!("expected tool source");
    };
    let mut policy = ContextPolicy::dispatch();
    policy.include_tool_results = false;
    let plan = ContextPlan::compose(
        policy,
        "",
        "",
        [TranscriptMessage::tool_result(
            "tool output remains excluded",
            *call_id,
            *route_id,
            *provider_id,
            *source_session_id,
            *outcome_sequence,
            *admitted_sequence,
            outcome_kind,
        )],
    );
    let manifest = capture(owner, "authored", &[source], &plan).unwrap();
    assert_eq!(manifest["included_tool_results"], 0);
    assert_eq!(manifest["listed_tool_results"][0]["disposition"], "omitted");
    assert_eq!(
        manifest["listed_tool_results"][0]["composition_reason"],
        "excluded_by_policy"
    );
    assert!(
        !manifest
            .to_string()
            .contains("tool output remains excluded")
    );
}

#[test]
fn exclusion_from_inventory_without_frozen_admission_fails_closed() {
    let owner = LocalConversationId::new();
    let source = admitted_source(7);
    let ContextSource::ToolResult {
        call_id,
        route_id,
        provider_id,
        source_session_id,
        outcome_sequence,
        admitted_sequence,
        outcome_kind,
    } = &source
    else {
        panic!("expected tool source");
    };
    let mut policy = ContextPolicy::dispatch();
    policy.include_tool_results = false;
    let plan = ContextPlan::compose(
        policy,
        "",
        "",
        [TranscriptMessage::tool_result(
            "unapproved tool result",
            *call_id,
            *route_id,
            *provider_id,
            *source_session_id,
            *outcome_sequence,
            *admitted_sequence,
            outcome_kind,
        )],
    );
    assert!(capture(owner, "authored", &[], &plan).is_err());
}

#[test]
fn older_manifest_without_composition_reason_remains_readable() {
    let owner = LocalConversationId::new();
    let source = admitted_source(7);
    let mut manifest = capture(
        owner,
        "authored",
        &[source.clone()],
        &plan_with_tool(&source),
    )
    .unwrap();
    manifest["listed_tool_results"][0]
        .as_object_mut()
        .unwrap()
        .remove("composition_reason");
    let payload = attach_to_dispatch_payload(
        json!({
            "schema":"chatarium-responses-turn-observation",
            "version":1,
            "details":{"local_turn_id":"old-turn","request_id":"old-turn"}
        })
        .to_string(),
        manifest,
    )
    .unwrap();
    let event = EventEnvelope {
        sequence: 20,
        at_unix_ms: 0,
        scope: Some("local-turn:old-turn".to_owned()),
        kind: EventKind::DispatchAttempted,
        payload,
    };
    let parsed = parse_dispatch(&event).unwrap().unwrap();
    assert_eq!(parsed.listed[0].composition_reason, None);
    assert!(parsed.listed[0].included);
}

#[test]
fn contradictory_or_unknown_composition_reason_blocks_manifest_replay() {
    let owner = LocalConversationId::new();
    let source = admitted_source(7);
    for reason in [
        "not_selected_for_dispatch",
        "nonsense",
        "excluded_by_policy",
    ] {
        let mut manifest = capture(
            owner,
            "authored",
            &[source.clone()],
            &plan_with_tool(&source),
        )
        .unwrap();
        manifest["listed_tool_results"][0]["composition_reason"] = json!(reason);
        let payload = attach_to_dispatch_payload(
            json!({
                "schema":"chatarium-responses-turn-observation",
                "version":1,
                "details":{"local_turn_id":"turn-1","request_id":"turn-1"}
            })
            .to_string(),
            manifest,
        )
        .unwrap();
        let event = EventEnvelope {
            sequence: 30,
            at_unix_ms: 0,
            scope: Some("local-turn:turn-1".to_owned()),
            kind: EventKind::DispatchAttempted,
            payload,
        };
        assert!(parse_dispatch(&event).is_err());
    }
}

#[test]
fn unsupported_or_duplicated_evidence_is_rejected() {
    let owner = LocalConversationId::new();
    let source = admitted_source(7);
    let plan = plan_with_tool(&source);
    assert!(capture(owner, "authored", &[], &plan).is_err());
    assert!(capture(owner, "authored", &[source.clone(), source.clone()], &plan).is_err());
    assert!(capture(owner, "arbitrary", &[source], &plan).is_err());
}

#[test]
fn bounded_manifest_discloses_incomplete_details_not_incomplete_totals() {
    let owner = LocalConversationId::new();
    let sources = (1..=40).map(admitted_source).collect::<Vec<_>>();
    let plan = ContextPlan::compose(ContextPolicy::dispatch(), "", "", []);
    let manifest = capture(owner, "controller_coordination", &sources, &plan).unwrap();
    assert_eq!(manifest["eligible_tool_results"], 40);
    assert_eq!(manifest["omitted_tool_results"], 40);
    assert_eq!(
        manifest["listed_tool_results"].as_array().unwrap().len(),
        32
    );
    assert_eq!(manifest["list_truncated"], true);
}

#[test]
fn durable_envelope_replays_and_detects_scope_or_count_corruption() {
    let owner = LocalConversationId::new();
    let turn = "turn-test";
    let manifest = capture(
        owner,
        "authored",
        &[admitted_source(1)],
        &ContextPlan::compose(ContextPolicy::dispatch(), "", "", []),
    )
    .unwrap();
    let payload = json!({
        "schema":"chatarium-responses-turn-observation",
        "version":1,
        "details":{"local_turn_id":turn,"request_id":turn}
    })
    .to_string();
    let payload = attach_to_dispatch_payload(payload, manifest).unwrap();
    let mut event = EventEnvelope {
        sequence: 9,
        at_unix_ms: 0,
        scope: Some("local-turn:turn-test".to_owned()),
        kind: EventKind::DispatchAttempted,
        payload,
    };
    let parsed = parse_dispatch(&event).unwrap().unwrap();
    assert_eq!(parsed.eligible_total, 1);
    assert_eq!(parsed.included_total, 0);
    assert_eq!(parsed.omitted_total, 1);
    event.scope = Some("local-turn:wrong".to_owned());
    assert!(parse_dispatch(&event).is_err());
    event.scope = Some("local-turn:turn-test".to_owned());
    let mut corrupt: Value = serde_json::from_str(&event.payload).unwrap();
    corrupt["details"]["context_evidence"]["included_tool_results"] = json!(2);
    event.payload = corrupt.to_string();
    assert!(parse_dispatch(&event).is_err());
}

fn append_authored_attempt(
    store: &mut chatarium_store::MemoryEventStore,
    owner: LocalConversationId,
) -> (String, u64) {
    use chatarium_core::{AuthoredUserMessage, LocalMessageId, LocalTurnId};
    use chatarium_store::EventStore;
    use chatarium_store::authored::{commit_user_message, local_turn_scope};

    let turn = LocalTurnId::new();
    let message = AuthoredUserMessage::new(
        owner,
        turn,
        LocalMessageId::new(),
        "historically typed user message",
    );
    commit_user_message(store, &message).unwrap();
    let manifest = capture(
        owner,
        "authored",
        &[],
        &ContextPlan::compose(ContextPolicy::dispatch(), "", "", []),
    )
    .unwrap();
    let turn_id = turn.to_string();
    let payload = json!({
        "schema": "chatarium-responses-turn-observation",
        "version": 1,
        "details": {
            "local_turn_id": turn_id,
            "request_id": turn_id,
        },
    })
    .to_string();
    let payload = attach_to_dispatch_payload(payload, manifest).unwrap();
    let sequence = store
        .append_scoped(
            Some(local_turn_scope(turn)),
            EventKind::DispatchAttempted,
            payload,
        )
        .unwrap();
    (turn_id, sequence)
}

fn append_transport_observation(
    store: &mut chatarium_store::MemoryEventStore,
    turn: &str,
    request: &str,
    kind: EventKind,
) -> u64 {
    use chatarium_store::EventStore;
    store
        .append_scoped(
            Some(format!("local-turn:{turn}")),
            kind,
            json!({
                "schema": "chatarium-responses-turn-observation",
                "version": 1,
                "details": {
                    "local_turn_id": turn,
                    "request_id": request,
                },
            })
            .to_string(),
        )
        .unwrap()
}

#[test]
fn exact_turn_outcomes_replay_without_cross_conversation_leakage() {
    use chatarium_store::{EventStore, MemoryEventStore};

    let first = LocalConversationId::new();
    let second = LocalConversationId::new();
    let mut store = MemoryEventStore::default();
    let (first_turn, first_dispatch) = append_authored_attempt(&mut store, first);
    let (second_turn, second_dispatch) = append_authored_attempt(&mut store, second);
    let accepted = append_transport_observation(
        &mut store,
        &first_turn,
        &first_turn,
        EventKind::RemoteAcceptanceObserved,
    );
    let completed = append_transport_observation(
        &mut store,
        &first_turn,
        &first_turn,
        EventKind::AssistantCompletionObserved,
    );
    assert!(completed > accepted && accepted > first_dispatch);
    assert!(second_dispatch > first_dispatch);
    let first_rows = dispatch_history(store.events(), first).unwrap();
    assert_eq!(first_rows.len(), 1);
    assert_eq!(first_rows[0].turn_id, first_turn);
    assert_eq!(first_rows[0].transport.accepted_sequence, Some(accepted));
    assert_eq!(first_rows[0].transport.completed_sequence, Some(completed));
    assert_eq!(first_rows[0].transport.status(), "COMPLETION OBSERVED");
    let second_rows = dispatch_history(store.events(), second).unwrap();
    assert_eq!(second_rows.len(), 1);
    assert_eq!(second_rows[0].turn_id, second_turn);
    assert_eq!(second_rows[0].transport.accepted_sequence, None);
    assert_eq!(
        second_rows[0].transport.status(),
        "DISPATCH ATTEMPT RECORDED · OUTCOME NOT OBSERVED",
    );
}

#[test]
fn foreign_request_id_in_same_turn_scope_blocks_projection() {
    use chatarium_store::{EventStore, MemoryEventStore};
    let owner = LocalConversationId::new();
    let mut store = MemoryEventStore::default();
    let (turn, _) = append_authored_attempt(&mut store, owner);
    append_transport_observation(
        &mut store,
        &turn,
        "a-different-request",
        EventKind::RemoteAcceptanceObserved,
    );
    assert!(dispatch_history(store.events(), owner).is_err());
}

#[test]
fn manifest_owner_must_match_durable_typed_turn_owner() {
    use chatarium_core::{AuthoredUserMessage, LocalMessageId, LocalTurnId};
    use chatarium_store::authored::{commit_user_message, local_turn_scope};
    use chatarium_store::{EventStore, MemoryEventStore};

    let own = LocalConversationId::new();
    let forged = LocalConversationId::new();
    let turn = LocalTurnId::new();
    let mut store = MemoryEventStore::default();
    commit_user_message(
        &mut store,
        &AuthoredUserMessage::new(own, turn, LocalMessageId::new(), "owned"),
    )
    .unwrap();
    let manifest = capture(
        forged,
        "authored",
        &[],
        &ContextPlan::compose(ContextPolicy::dispatch(), "", "", []),
    )
    .unwrap();
    let payload = attach_to_dispatch_payload(
        json!({
            "schema": "chatarium-responses-turn-observation",
            "version": 1,
            "details": {
                "local_turn_id": turn.to_string(),
                "request_id": turn.to_string(),
            },
        })
        .to_string(),
        manifest,
    )
    .unwrap();
    store
        .append_scoped(
            Some(local_turn_scope(turn)),
            EventKind::DispatchAttempted,
            payload,
        )
        .unwrap();
    assert!(dispatch_history(store.events(), own).is_err());
    assert!(dispatch_history(store.events(), forged).is_err());
}

#[test]
fn history_preserves_more_than_twelve_attempts_newest_first() {
    use chatarium_store::{EventStore, MemoryEventStore};
    let owner = LocalConversationId::new();
    let mut store = MemoryEventStore::default();
    let mut last = String::new();
    for _ in 0..27 {
        let (turn, _) = append_authored_attempt(&mut store, owner);
        last = turn;
    }
    let rows = dispatch_history(store.events(), owner).unwrap();
    assert_eq!(rows.len(), 27);
    assert_eq!(rows[0].turn_id, last);
    assert!(
        rows.windows(2)
            .all(|pair| pair[0].sequence > pair[1].sequence)
    );
}

#[test]
fn transport_interruption_does_not_erase_positive_acceptance_evidence() {
    use chatarium_store::{EventStore, MemoryEventStore};
    let owner = LocalConversationId::new();
    let mut store = MemoryEventStore::default();
    let (turn, _) = append_authored_attempt(&mut store, owner);
    let accepted = append_transport_observation(
        &mut store,
        &turn,
        &turn,
        EventKind::RemoteAcceptanceObserved,
    );
    let interrupted =
        append_transport_observation(&mut store, &turn, &turn, EventKind::TransportInterrupted);
    let rows = dispatch_history(store.events(), owner).unwrap();
    assert_eq!(rows[0].transport.accepted_sequence, Some(accepted));
    assert_eq!(rows[0].transport.interrupted_sequence, Some(interrupted));
    assert_eq!(
        rows[0].transport.status(),
        "INTERRUPTED AFTER REMOTE ACCEPTANCE"
    );
}

#[test]
fn conflicting_terminal_observations_are_labeled_not_misreported() {
    use chatarium_store::{EventStore, MemoryEventStore};
    let owner = LocalConversationId::new();
    let mut store = MemoryEventStore::default();
    let (turn, _) = append_authored_attempt(&mut store, owner);
    append_transport_observation(&mut store, &turn, &turn, EventKind::RemoteFailureObserved);
    append_transport_observation(
        &mut store,
        &turn,
        &turn,
        EventKind::AssistantCompletionObserved,
    );
    let rows = dispatch_history(store.events(), owner).unwrap();
    assert_eq!(
        rows[0].transport.status(),
        "CONFLICTING TERMINAL EVIDENCE · inspect turn audit"
    );
}

#[test]
fn transport_timeline_uses_journal_order_not_fixed_status_order() {
    let trace = DispatchTransportTrace {
        accepted_sequence: Some(19),
        first_output_sequence: Some(21),
        failure_sequence: Some(34),
        completed_sequence: None,
        interrupted_sequence: Some(38),
        last_observation_sequence: Some(38),
    };
    assert_eq!(
        trace.timeline(),
        vec![
            (19, "remote acceptance observed"),
            (21, "assistant output observed"),
            (34, "remote failure observed"),
            (38, "transport interrupted"),
        ]
    );
}

#[test]
fn copied_audit_is_content_free_but_preserves_full_typed_provenance() {
    use chatarium_store::{EventStore, MemoryEventStore};
    let owner = LocalConversationId::new();
    let mut store = MemoryEventStore::default();
    let (turn, _) = append_authored_attempt(&mut store, owner);
    append_transport_observation(
        &mut store,
        &turn,
        &turn,
        EventKind::RemoteAcceptanceObserved,
    );
    let mut rows = dispatch_history(store.events(), owner).unwrap();
    let row = rows.first_mut().unwrap();
    row.listed.push(ListedToolEvidence {
        call_id: 10,
        route_id: 20,
        provider_id: 30,
        source_session_id: 40,
        outcome_sequence: 50,
        admitted_sequence: 60,
        outcome_kind: "result".to_owned(),
        included: true,
        composition_reason: Some(ToolCompositionReason::Included),
    });
    row.eligible_total = 1;
    row.included_total = 1;
    let export = portable_audit_json(row);
    assert_eq!(export["request"]["conversation_id"], owner.to_string());
    assert_eq!(export["tool_results"]["listed"][0]["source_session_id"], 40);
    assert_eq!(export["tool_results"]["listed"][0]["outcome_sequence"], 50);
    assert_eq!(
        export["tool_results"]["listed"][0]["disposition"],
        "included"
    );
    assert!(export["transport"]["accepted_sequence"].is_number());
    let serialized = export.to_string();
    assert!(!serialized.contains("secret adapter bytes"));
    assert!(!serialized.contains("historically typed user message"));
    assert!(!serialized.contains("Chatarium tool result"));
}

#[test]
fn portable_export_explicitly_marks_bounded_detail() {
    use chatarium_store::{EventStore, MemoryEventStore};
    let owner = LocalConversationId::new();
    let mut store = MemoryEventStore::default();
    append_authored_attempt(&mut store, owner);
    let mut rows = dispatch_history(store.events(), owner).unwrap();
    let row = rows.first_mut().unwrap();
    row.eligible_total = 40;
    row.omitted_total = 40;
    row.truncated = true;
    let export = portable_audit_json(row);
    assert_eq!(export["tool_results"]["eligible"], 40);
    assert_eq!(export["tool_results"]["list_truncated"], true);
}

#[test]
fn transient_projection_cache_invalidates_on_journal_append_or_conversation_change() {
    use chatarium_store::{EventStore, MemoryEventStore};
    let owner = LocalConversationId::new();
    let foreign = LocalConversationId::new();
    let mut store = MemoryEventStore::default();
    append_authored_attempt(&mut store, owner);
    let cache = DispatchHistoryCache::build(store.events(), owner);
    assert!(cache.rows.is_ok());
    assert!(cache.matches(store.events(), owner));
    assert!(!cache.matches(store.events(), foreign));
    append_authored_attempt(&mut store, owner);
    assert!(!cache.matches(store.events(), owner));
    let rebuilt = DispatchHistoryCache::build(store.events(), owner);
    assert!(rebuilt.matches(store.events(), owner));
    assert_eq!(rebuilt.rows.as_ref().unwrap().len(), 2);
}

#[test]
fn transient_cache_does_not_reuse_a_different_terminal_scope_with_same_sequences() {
    use chatarium_store::{EventStore, MemoryEventStore};
    let owner = LocalConversationId::new();
    let mut store = MemoryEventStore::default();
    append_authored_attempt(&mut store, owner);
    let cached = DispatchHistoryCache::build(store.events(), owner);
    let mut altered = store.events().to_vec();
    altered.last_mut().unwrap().scope = Some("local-turn:another-turn".to_owned());
    assert_eq!(altered.len(), store.events().len());
    assert_eq!(
        altered.last().unwrap().sequence,
        store.events().last().unwrap().sequence
    );
    assert!(!cached.matches(&altered, owner));
}

#[test]
fn transient_cache_preserves_a_failed_closed_projection_until_revision_changes() {
    use chatarium_store::{EventStore, MemoryEventStore};
    let owner = LocalConversationId::new();
    let mut store = MemoryEventStore::default();
    let (turn, _) = append_authored_attempt(&mut store, owner);
    append_transport_observation(
        &mut store,
        &turn,
        "wrong-request",
        EventKind::RemoteAcceptanceObserved,
    );
    let cache = DispatchHistoryCache::build(store.events(), owner);
    assert!(cache.rows.is_err());
    assert!(cache.matches(store.events(), owner));
    append_transport_observation(&mut store, &turn, &turn, EventKind::TransportInterrupted);
    assert!(!cache.matches(store.events(), owner));
}

#[test]
fn legacy_dispatch_does_not_fabricate_manifest() {
    let owner = LocalConversationId::new();
    let event = EventEnvelope {
        sequence: 1,
        at_unix_ms: 0,
        scope: Some("local-turn:legacy".to_owned()),
        kind: EventKind::DispatchAttempted,
        payload: "{}".to_owned(),
    };
    assert!(dispatch_history(&[event], owner).unwrap().is_empty());
}
