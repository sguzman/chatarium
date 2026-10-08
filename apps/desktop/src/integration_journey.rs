//! Deterministic production-path integration smoke for the local conversation lifecycle.
//! Remote responses are synthetic journal observations: no browser, account, or network.

use super::{
    admitted_tool_result_context_messages, context_composer, context_transcript,
    mcp_dispatch_manifest, projected_local_display_messages, remote_turn_payload,
};
use chatarium_core::chat_container::ChatContainerId;
use chatarium_core::routing::{
    RouteClass, RouteEndpointId, RouteGate, RouteId, RoutePolicy, RouteRequest,
};
use chatarium_core::session::{SessionEndpointBinding, SessionId};
use chatarium_core::tool::{
    ToolCallId, ToolOperationName, ToolProviderEndpointBinding, ToolProviderId, ToolProviderName,
};
use chatarium_core::{
    AuthoredUserMessage, EventKind, LocalConversationId, LocalMessageId, LocalTurnId,
};
use chatarium_store::authored::{commit_user_message, local_turn_scope};
use chatarium_store::chat_container_audit::record_chat_container_created;
use chatarium_store::local_conversation_chat_container_audit::record_local_conversation_chat_container_bound;
use chatarium_store::routing_audit::{
    RouteUserDecision, record_route_dispatched, record_route_proposed, record_route_user_decision,
};
use chatarium_store::session_audit::{
    record_local_session_registered, record_session_endpoint_bound,
};
use chatarium_store::tool_call_audit::{record_tool_call, record_tool_call_route_bound};
use chatarium_store::tool_outcome_audit::{
    ToolCallOutcomeKind, append_tool_call_outcome_checked, replay_tool_call_outcome_audit,
};
use chatarium_store::tool_provider_audit::{
    record_tool_provider_endpoint_bound, record_tool_provider_registered,
};
use chatarium_store::tool_result_context_audit::{
    ToolResultContextDecision, append_tool_result_context_decision_checked,
    replay_admitted_tool_results,
};
use chatarium_store::turn_projection::derive_authored_turns;
use chatarium_store::{EventStore, JsonlEventStore, MemoryEventStore};
use std::time::{SystemTime, UNIX_EPOCH};

const SESSION: SessionId = SessionId::new(1);
const SOURCE: RouteEndpointId = RouteEndpointId::new(10);
const DEST: RouteEndpointId = RouteEndpointId::new(20);
const PROVIDER: ToolProviderId = ToolProviderId::new(3);
const CALL: ToolCallId = ToolCallId::new(5);
const ROUTE: RouteId = RouteId::new(7);
const RESULT: &str = "QA_TOOL_EVIDENCE_UNIQUE exact result";

fn author(
    store: &mut impl EventStore,
    conversation: LocalConversationId,
    text: &str,
) -> LocalTurnId {
    let turn = LocalTurnId::new();
    commit_user_message(
        store,
        &AuthoredUserMessage::new(conversation, turn, LocalMessageId::new(), text),
    )
    .unwrap();
    turn
}

fn prepared_context(
    store: &impl EventStore,
    owner: LocalConversationId,
) -> (
    context_composer::ContextPlan,
    Vec<context_composer::ContextSource>,
) {
    let mut transcript =
        context_transcript(&projected_local_display_messages(store.events(), owner));
    let tools = admitted_tool_result_context_messages(store.events(), owner).unwrap();
    let frozen = tools
        .iter()
        .map(|item| item.source.clone())
        .collect::<Vec<_>>();
    transcript.extend(tools);
    transcript.sort_by_key(context_composer::TranscriptMessage::order_sequence);
    let plan = context_composer::ContextPlan::compose(
        context_composer::ContextPolicy::dispatch(),
        "Use only permitted context.",
        "",
        transcript,
    );
    (plan, frozen)
}

fn dispatch(
    store: &mut impl EventStore,
    owner: LocalConversationId,
    turn: LocalTurnId,
    plan: &context_composer::ContextPlan,
    frozen: &[context_composer::ContextSource],
) -> u64 {
    let manifest = mcp_dispatch_manifest::capture(owner, "authored", frozen, plan).unwrap();
    let payload = mcp_dispatch_manifest::attach_to_dispatch_payload(
        remote_turn_payload(turn, &turn.to_string(), Some("fixture-model"), None, None),
        manifest,
    )
    .unwrap();
    store
        .append_scoped(
            Some(local_turn_scope(turn)),
            EventKind::DispatchAttempted,
            payload,
        )
        .unwrap()
}

fn observe_reply(store: &mut impl EventStore, turn: LocalTurnId) {
    for (kind, text) in [
        (EventKind::RemoteAcceptanceObserved, None),
        (
            EventKind::AssistantCompletionObserved,
            Some("synthetic assistant response"),
        ),
    ] {
        store
            .append_scoped(
                Some(local_turn_scope(turn)),
                kind,
                remote_turn_payload(turn, &turn.to_string(), None, text, None),
            )
            .unwrap();
    }
}

fn tool_route(store: &mut impl EventStore, owner: LocalConversationId) -> RouteRequest {
    record_local_session_registered(store, SESSION).unwrap();
    record_chat_container_created(store, ChatContainerId::new(1), SESSION).unwrap();
    record_local_conversation_chat_container_bound(store, owner, ChatContainerId::new(1)).unwrap();
    record_session_endpoint_bound(store, SessionEndpointBinding::new(SESSION, SOURCE)).unwrap();
    record_tool_provider_registered(
        store,
        PROVIDER,
        &ToolProviderName::new("qa-fixture").unwrap(),
    )
    .unwrap();
    record_tool_provider_endpoint_bound(store, ToolProviderEndpointBinding::new(PROVIDER, DEST))
        .unwrap();
    record_tool_call(
        store,
        CALL,
        SESSION,
        PROVIDER,
        &ToolOperationName::new("qa.inspect").unwrap(),
        "{}",
    )
    .unwrap();
    let route = RouteRequest {
        id: ROUTE,
        source: SOURCE,
        destination: DEST,
        class: RouteClass::ToolCall,
    };
    record_route_proposed(store, route, RoutePolicy::RequireApproval).unwrap();
    record_tool_call_route_bound(store, CALL, ROUTE).unwrap();
    route
}

fn approve(store: &mut impl EventStore, route: RouteRequest) {
    record_route_user_decision(store, ROUTE, RouteUserDecision::Allow).unwrap();
    let mut gate = RouteGate::new(route, RoutePolicy::RequireApproval);
    gate.user_allow().unwrap();
    record_route_dispatched(store, gate.authorize_dispatch(ROUTE).unwrap()).unwrap();
}

#[test]
fn integration_journey_durable_tool_admission_followup_revocation_and_restart() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "chatarium-integration-{}-{nonce}.jsonl",
        std::process::id()
    ));
    let owner = LocalConversationId::new();
    let foreign = LocalConversationId::new();
    let second_dispatch;
    let expected_events;
    {
        let mut store = JsonlEventStore::open(&path).unwrap();
        let first_turn = author(&mut store, owner, "First request");
        let (first_plan, first_frozen) = prepared_context(&store, owner);
        assert_eq!(first_plan.tool_result_count(), 0);
        assert!(first_frozen.is_empty());
        let first_dispatch = dispatch(&mut store, owner, first_turn, &first_plan, &first_frozen);
        observe_reply(&mut store, first_turn);

        let route = tool_route(&mut store, owner);
        assert!(
            append_tool_result_context_decision_checked(
                &mut store,
                CALL,
                owner,
                ToolResultContextDecision::Admit
            )
            .is_err(),
            "an undispatched call cannot become model context"
        );
        approve(&mut store, route);
        let outcome = append_tool_call_outcome_checked(
            &mut store,
            CALL,
            ROUTE,
            ToolCallOutcomeKind::Result,
            RESULT,
        )
        .expect("approved tool outcome is checked before persistence");
        assert_eq!(outcome.kind, EventKind::ToolCallOutcomeObserved);
        assert!(
            replay_admitted_tool_results(store.events(), owner)
                .unwrap()
                .is_empty()
        );

        let count_before_foreign = store.events().len();
        assert!(
            append_tool_result_context_decision_checked(
                &mut store,
                CALL,
                foreign,
                ToolResultContextDecision::Admit
            )
            .is_err()
        );
        assert_eq!(store.events().len(), count_before_foreign);
        append_tool_result_context_decision_checked(
            &mut store,
            CALL,
            owner,
            ToolResultContextDecision::Admit,
        )
        .expect("separate explicit context admission");

        let second_turn = author(&mut store, owner, "Follow-up using observed tool evidence");
        let (second_plan, second_frozen) = prepared_context(&store, owner);
        assert_eq!(second_plan.tool_result_count(), 1);
        assert_eq!(second_frozen.len(), 1);
        assert!(second_plan.input_json().to_string().contains(RESULT));
        second_dispatch = dispatch(&mut store, owner, second_turn, &second_plan, &second_frozen);
        observe_reply(&mut store, second_turn);

        append_tool_result_context_decision_checked(
            &mut store,
            CALL,
            owner,
            ToolResultContextDecision::Exclude,
        )
        .expect("revocation of future admission");
        let third_turn = author(&mut store, owner, "Follow-up after revocation");
        let (third_plan, third_frozen) = prepared_context(&store, owner);
        assert_eq!(third_plan.tool_result_count(), 0);
        assert!(third_frozen.is_empty());
        assert!(!third_plan.input_json().to_string().contains(RESULT));
        dispatch(&mut store, owner, third_turn, &third_plan, &third_frozen);

        let history = mcp_dispatch_manifest::dispatch_history(store.events(), owner).unwrap();
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].included_total, 0);
        assert_eq!(history[1].sequence, second_dispatch);
        assert_eq!(history[1].included_total, 1);
        assert_eq!(history[1].transport.status(), "COMPLETION OBSERVED");
        assert_eq!(history[2].sequence, first_dispatch);
        let reverse = mcp_dispatch_manifest::reverse_tool_provenance(&history, CALL.get()).unwrap();
        assert_eq!(reverse.included, 1);
        assert_eq!(reverse.not_eligible_in_complete_manifests, 2);
        assert!(
            !mcp_dispatch_manifest::portable_audit_json(&history[1])
                .to_string()
                .contains(RESULT),
            "content-free manifest export cannot leak tool bodies"
        );
        expected_events = store.events().len();
    }
    {
        let reopened = JsonlEventStore::open(&path).expect("replay after process restart");
        assert_eq!(reopened.events().len(), expected_events);
        assert_eq!(derive_authored_turns(reopened.events()).unwrap().len(), 3);
        assert_eq!(
            replay_tool_call_outcome_audit(reopened.events())
                .unwrap()
                .len(),
            1
        );
        assert!(
            replay_admitted_tool_results(reopened.events(), owner)
                .unwrap()
                .is_empty()
        );
        let history = mcp_dispatch_manifest::dispatch_history(reopened.events(), owner).unwrap();
        assert_eq!(history[1].sequence, second_dispatch);
        assert_eq!(history[1].included_total, 1);
        assert!(
            mcp_dispatch_manifest::dispatch_history(reopened.events(), foreign)
                .unwrap()
                .is_empty()
        );
    }
    std::fs::remove_file(path).expect("cleanup synthetic journal");
}

#[test]
fn integration_journey_denied_tool_never_creates_result_or_context() {
    let mut store = MemoryEventStore::default();
    let owner = LocalConversationId::new();
    let foreign = LocalConversationId::new();
    author(&mut store, owner, "Do not execute denied tool");
    tool_route(&mut store, owner);
    record_route_user_decision(&mut store, ROUTE, RouteUserDecision::Deny).unwrap();
    let before = store.events().len();
    assert!(
        append_tool_call_outcome_checked(
            &mut store,
            CALL,
            ROUTE,
            ToolCallOutcomeKind::Result,
            "forbidden evidence"
        )
        .is_err()
    );
    assert_eq!(store.events().len(), before);
    assert!(
        append_tool_result_context_decision_checked(
            &mut store,
            CALL,
            owner,
            ToolResultContextDecision::Admit
        )
        .is_err()
    );
    assert_eq!(store.events().len(), before);
    for conversation in [owner, foreign] {
        let (plan, frozen) = prepared_context(&store, conversation);
        assert!(frozen.is_empty());
        assert_eq!(plan.tool_result_count(), 0);
        assert!(!plan.input_json().to_string().contains("forbidden evidence"));
    }
}

#[test]
fn integration_journey_send_click_snapshot_survives_later_revoke() {
    let mut store = MemoryEventStore::default();
    let owner = LocalConversationId::new();
    let route = tool_route(&mut store, owner);
    approve(&mut store, route);
    append_tool_call_outcome_checked(&mut store, CALL, ROUTE, ToolCallOutcomeKind::Result, RESULT)
        .unwrap();
    append_tool_result_context_decision_checked(
        &mut store,
        CALL,
        owner,
        ToolResultContextDecision::Admit,
    )
    .unwrap();
    let turn = author(&mut store, owner, "Frozen send-click admission");
    let (frozen_plan, frozen_eligible) = prepared_context(&store, owner);
    assert_eq!(frozen_plan.tool_result_count(), 1);
    append_tool_result_context_decision_checked(
        &mut store,
        CALL,
        owner,
        ToolResultContextDecision::Exclude,
    )
    .unwrap();
    let (future_plan, future_eligible) = prepared_context(&store, owner);
    assert_eq!(future_plan.tool_result_count(), 0);
    assert!(future_eligible.is_empty());

    let sequence = dispatch(&mut store, owner, turn, &frozen_plan, &frozen_eligible);
    let history = mcp_dispatch_manifest::dispatch_history(store.events(), owner).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].sequence, sequence);
    assert_eq!(history[0].included_total, 1);
    assert_eq!(
        mcp_dispatch_manifest::reverse_tool_provenance(&history, CALL.get())
            .unwrap()
            .included,
        1
    );
}
