//! Durable journal append/validation helpers extracted without reordering any operations.
//! These are pure app-side entrypoints; the single writer remains in worker/.
use super::*;

pub(crate) fn append_local_orchestration_topology_checked(
    store: &mut impl EventStore,
    conversation_id: LocalConversationId,
    container_id: ChatContainerId,
    root_session_id: SessionId,
) -> Result<Vec<EventEnvelope>, String> {
    let bindings = replay_local_conversation_chat_container_bindings(store.events())?;
    if let Some(existing) = bindings
        .iter()
        .find(|binding| binding.conversation_id == conversation_id)
    {
        return Err(format!(
            "local conversation {conversation_id} already owns chat container {}",
            existing.container_id.get()
        ));
    }
    if let Some(existing) = bindings
        .iter()
        .find(|binding| binding.container_id == container_id)
    {
        return Err(format!(
            "chat container {} already belongs to local conversation {}",
            container_id.get(),
            existing.conversation_id
        ));
    }
    if replay_session_audit(store.events())?
        .iter()
        .any(|record| record.session_id == root_session_id)
    {
        return Err(format!(
            "local session {} is already registered",
            root_session_id.get()
        ));
    }
    if replay_chat_container_audit(store.events())?
        .iter()
        .any(|record| record.container_id == container_id)
    {
        return Err(format!(
            "chat container {} already exists",
            container_id.get()
        ));
    }

    let before = store.events().len();
    record_local_session_registered(store, root_session_id).map_err(|error| error.to_string())?;
    record_chat_container_created(store, container_id, root_session_id)
        .map_err(|error| error.to_string())?;
    record_local_conversation_chat_container_bound(store, conversation_id, container_id)
        .map_err(|error| error.to_string())?;

    let topology = replay_local_conversation_topologies(store.events())?
        .into_iter()
        .find(|topology| topology.conversation_id == conversation_id)
        .ok_or_else(|| {
            "local orchestration topology append did not replay for its conversation".to_owned()
        })?;
    if topology.container_id != container_id || topology.current_session_id != root_session_id {
        return Err(
            "local orchestration topology replay disagrees with appended identities".to_owned(),
        );
    }

    Ok(store.events()[before..].to_vec())
}

pub(crate) fn append_current_session_route_endpoint_checked(
    store: &mut impl EventStore,
    conversation_id: LocalConversationId,
    session_id: SessionId,
    endpoint_id: RouteEndpointId,
) -> Result<EventEnvelope, String> {
    let topology = replay_local_conversation_topologies(store.events())?
        .into_iter()
        .find(|topology| topology.conversation_id == conversation_id)
        .ok_or_else(|| {
            format!("local conversation {conversation_id} has no orchestration topology")
        })?;
    if topology.current_session_id != session_id {
        return Err(format!(
            "session {} is not current for local conversation {conversation_id}; current session is {}",
            session_id.get(),
            topology.current_session_id.get()
        ));
    }

    let sessions = replay_session_audit(store.events())?;
    let session = sessions
        .iter()
        .find(|record| record.session_id == session_id)
        .ok_or_else(|| format!("current session {} is not registered", session_id.get()))?;
    if let Some(existing) = session.endpoint_binding {
        return Err(format!(
            "current session {} is already bound to routing endpoint {}",
            session_id.get(),
            existing.endpoint_id().get()
        ));
    }
    if let Some(existing) = sessions.iter().find_map(|record| {
        record
            .endpoint_binding
            .filter(|binding| binding.endpoint_id() == endpoint_id)
            .map(|binding| binding.session_id())
    }) {
        return Err(format!(
            "routing endpoint {} is already bound to session {}",
            endpoint_id.get(),
            existing.get()
        ));
    }

    if let Some(provider) = replay_tool_provider_audit(store.events())?
        .into_iter()
        .find(|provider| {
            provider
                .endpoint_binding
                .is_some_and(|binding| binding.endpoint_id() == endpoint_id)
        })
    {
        return Err(format!(
            "routing endpoint {} is already bound to tool provider {}",
            endpoint_id.get(),
            provider.provider_id.get()
        ));
    }

    if replay_routing_audit(store.events())?.iter().any(|route| {
        route.request.source == endpoint_id || route.request.destination == endpoint_id
    }) {
        return Err(format!(
            "routing endpoint {} already appears in durable route history and cannot be claimed by a new session",
            endpoint_id.get()
        ));
    }

    record_session_endpoint_bound(store, SessionEndpointBinding::new(session_id, endpoint_id))
        .map_err(|error| error.to_string())?;

    let replayed = replay_session_audit(store.events())?
        .into_iter()
        .find(|record| record.session_id == session_id)
        .and_then(|record| record.endpoint_binding)
        .ok_or_else(|| {
            "session endpoint append did not replay for the current session".to_owned()
        })?;
    if replayed.endpoint_id() != endpoint_id {
        return Err("session endpoint replay disagrees with appended endpoint".to_owned());
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "session endpoint append produced no durable event".to_owned())
}

pub(crate) fn append_local_session_route_proposal_checked(
    store: &mut impl EventStore,
    route_id: RouteId,
    source_conversation_id: LocalConversationId,
    destination_conversation_id: LocalConversationId,
) -> Result<EventEnvelope, String> {
    if source_conversation_id == destination_conversation_id {
        return Err("a local conversation cannot route a session message to itself".to_owned());
    }
    replay_local_route_payload_audit(store.events())?;
    if replay_routing_audit(store.events())?
        .iter()
        .any(|route| route.request.id == route_id)
    {
        return Err(format!("route {} already exists", route_id.get()));
    }

    let directory = replay_local_routing_directory(store.events())?;
    let source = directory
        .iter()
        .find(|entry| entry.conversation_id == source_conversation_id)
        .ok_or_else(|| {
            format!(
                "source local conversation {source_conversation_id} is not currently addressable"
            )
        })?;
    let destination = directory
        .iter()
        .find(|entry| entry.conversation_id == destination_conversation_id)
        .ok_or_else(|| {
            format!(
                "destination local conversation {destination_conversation_id} is not currently addressable"
            )
        })?;

    if !source.current_session_phase.accepts_ordinary_turns() {
        return Err(format!(
            "source conversation {source_conversation_id} current session {} is {} and cannot accept ordinary session-message routing",
            source.current_session_id.get(),
            session_lifecycle_phase_label(source.current_session_phase),
        ));
    }
    if !destination.current_session_phase.accepts_ordinary_turns() {
        return Err(format!(
            "destination conversation {destination_conversation_id} current session {} is {} and cannot accept ordinary session-message routing",
            destination.current_session_id.get(),
            session_lifecycle_phase_label(destination.current_session_phase),
        ));
    }

    let request = RouteRequest {
        id: route_id,
        source: source.endpoint_id,
        destination: destination.endpoint_id,
        class: RouteClass::SessionMessage,
    };
    record_route_proposed(store, request, RoutePolicy::RequireApproval)
        .map_err(|error| error.to_string())?;

    let replayed = replay_routing_audit(store.events())?
        .into_iter()
        .find(|route| route.request.id == route_id)
        .ok_or_else(|| "local route proposal append did not replay".to_owned())?;
    if replayed.request != request
        || replayed.initial_policy != RoutePolicy::RequireApproval
        || replayed.gate_state != RouteGateState::PendingApproval
    {
        return Err("local route proposal replay disagrees with appended policy".to_owned());
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "local route proposal append produced no durable event".to_owned())
}

pub(crate) fn append_local_route_payload_checked(
    store: &mut impl EventStore,
    payload_id: RoutePayloadId,
    route_id: RouteId,
    text: String,
) -> Result<EventEnvelope, String> {
    if text.trim().is_empty() {
        return Err("route payload text cannot be empty".to_owned());
    }

    let payloads = replay_local_route_payload_audit(store.events())?;
    if let Some(existing) = payloads.iter().find(|payload| payload.route_id == route_id) {
        return Err(format!(
            "route {} already has immutable payload {}",
            route_id.get(),
            existing.payload_id.get()
        ));
    }
    if let Some(existing) = payloads
        .iter()
        .find(|payload| payload.payload_id == payload_id)
    {
        return Err(format!(
            "route payload identity {} already belongs to route {}",
            payload_id.get(),
            existing.route_id.get()
        ));
    }

    let route = replay_routing_audit(store.events())?
        .into_iter()
        .find(|route| route.request.id == route_id)
        .ok_or_else(|| format!("local route {} does not exist", route_id.get()))?;
    if route.request.class != RouteClass::SessionMessage {
        return Err(format!(
            "route {} is not a local session-message route",
            route_id.get()
        ));
    }
    if route.gate_state.is_dispatched() {
        return Err(format!(
            "route {} has already dispatched and cannot acquire a new payload",
            route_id.get()
        ));
    }

    let directory = replay_local_routing_directory(store.events())?;
    let source = directory
        .iter()
        .find(|entry| entry.endpoint_id == route.request.source)
        .ok_or_else(|| {
            format!(
                "route {} source endpoint {} is no longer a current local conversation leaf",
                route_id.get(),
                route.request.source.get()
            )
        })?;
    let destination = directory
        .iter()
        .find(|entry| entry.endpoint_id == route.request.destination)
        .ok_or_else(|| {
            format!(
                "route {} destination endpoint {} is no longer a current local conversation leaf",
                route_id.get(),
                route.request.destination.get()
            )
        })?;

    if source.conversation_id == destination.conversation_id {
        return Err(format!(
            "route {} resolves to the same local conversation on both ends",
            route_id.get()
        ));
    }
    if !source.current_session_phase.accepts_ordinary_turns()
        || !destination.current_session_phase.accepts_ordinary_turns()
    {
        return Err(format!(
            "route {} is stale because one of its current local session leaves cannot accept ordinary turns",
            route_id.get()
        ));
    }

    record_local_route_payload_attached(
        store,
        payload_id,
        route_id,
        source.conversation_id,
        destination.conversation_id,
        text.clone(),
    )
    .map_err(|error| error.to_string())?;

    let replayed = replay_local_route_payload_audit(store.events())?
        .into_iter()
        .find(|payload| payload.route_id == route_id)
        .ok_or_else(|| "local route payload append did not replay".to_owned())?;
    if replayed.payload_id != payload_id
        || replayed.source_conversation_id != source.conversation_id
        || replayed.destination_conversation_id != destination.conversation_id
        || replayed.text != text
    {
        return Err("local route payload replay disagrees with appended payload".to_owned());
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "local route payload append produced no durable event".to_owned())
}

pub(crate) fn append_local_route_user_decision_checked(
    store: &mut impl EventStore,
    route_id: RouteId,
    decision: RouteUserDecision,
) -> Result<EventEnvelope, String> {
    let payloads = replay_local_route_payload_audit(store.events())?;
    let route = replay_routing_audit(store.events())?
        .into_iter()
        .find(|route| route.request.id == route_id)
        .ok_or_else(|| format!("local route {} does not exist", route_id.get()))?;

    if route.request.class != RouteClass::SessionMessage {
        return Err(format!(
            "route {} is not a local session-message route",
            route_id.get()
        ));
    }
    if route.initial_policy != RoutePolicy::RequireApproval {
        return Err(format!(
            "route {} was not created under explicit-approval policy",
            route_id.get()
        ));
    }
    if route.gate_state.is_dispatched() {
        return Err(format!(
            "route {} has already dispatched and its policy history is immutable",
            route_id.get()
        ));
    }
    if route.latest_user_decision == Some(decision) {
        return Err(format!(
            "route {} already has explicit user decision {}",
            route_id.get(),
            route_user_decision_label(decision),
        ));
    }
    if decision == RouteUserDecision::Allow
        && !payloads.iter().any(|payload| payload.route_id == route_id)
    {
        return Err(format!(
            "route {} cannot be allowed before an immutable payload is durably attached",
            route_id.get()
        ));
    }

    let directory = replay_local_routing_directory(store.events())?;
    let source = directory
        .iter()
        .find(|entry| entry.endpoint_id == route.request.source)
        .ok_or_else(|| {
            format!(
                "route {} source endpoint {} is no longer a current local conversation leaf",
                route_id.get(),
                route.request.source.get()
            )
        })?;
    let destination = directory
        .iter()
        .find(|entry| entry.endpoint_id == route.request.destination)
        .ok_or_else(|| {
            format!(
                "route {} destination endpoint {} is no longer a current local conversation leaf",
                route_id.get(),
                route.request.destination.get()
            )
        })?;

    if source.conversation_id == destination.conversation_id {
        return Err(format!(
            "route {} resolves to the same local conversation on both ends",
            route_id.get()
        ));
    }
    if !source.current_session_phase.accepts_ordinary_turns()
        || !destination.current_session_phase.accepts_ordinary_turns()
    {
        return Err(format!(
            "route {} is stale because one of its current local session leaves cannot accept ordinary turns",
            route_id.get()
        ));
    }

    record_route_user_decision(store, route_id, decision).map_err(|error| error.to_string())?;

    let replayed = replay_routing_audit(store.events())?
        .into_iter()
        .find(|candidate| candidate.request.id == route_id)
        .ok_or_else(|| "local route decision append did not replay".to_owned())?;
    let expected_state = match decision {
        RouteUserDecision::Allow => RouteGateState::Allowed {
            by: DecisionAuthority::User,
        },
        RouteUserDecision::Deny => RouteGateState::Denied {
            by: DecisionAuthority::User,
        },
    };
    if replayed.latest_user_decision != Some(decision) || replayed.gate_state != expected_state {
        return Err("local route decision replay disagrees with appended decision".to_owned());
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "local route decision append produced no durable event".to_owned())
}

pub(crate) fn route_gate_before_dispatch(route: &RouteAuditRecord) -> Result<RouteGate, String> {
    let mut gate = RouteGate::new(route.request, route.initial_policy);
    if let Some(decision) = route.latest_user_decision {
        match decision {
            RouteUserDecision::Allow => gate.user_allow(),
            RouteUserDecision::Deny => gate.user_deny(),
        }
        .map_err(|error| {
            format!(
                "cannot reconstruct route {} gate: {error:?}",
                route.request.id.get()
            )
        })?;
    }
    if gate.state() != route.gate_state {
        return Err(format!(
            "route {} gate reconstruction disagrees with durable routing audit",
            route.request.id.get()
        ));
    }
    Ok(gate)
}

pub(crate) fn append_local_route_dispatch_and_delivery_checked(
    store: &mut impl EventStore,
    route_id: RouteId,
) -> Result<Vec<EventEnvelope>, String> {
    let deliveries = replay_local_route_delivery_audit(store.events())?;
    if let Some(existing) = deliveries
        .iter()
        .find(|delivery| delivery.route_id == route_id)
    {
        return Err(format!(
            "local route {} is already delivered at event #{}",
            route_id.get(),
            existing.delivered_sequence
        ));
    }

    let payload = replay_local_route_payload_audit(store.events())?
        .into_iter()
        .find(|payload| payload.route_id == route_id)
        .ok_or_else(|| {
            format!(
                "local route {} cannot dispatch before an immutable payload is attached",
                route_id.get()
            )
        })?;
    let route = replay_routing_audit(store.events())?
        .into_iter()
        .find(|route| route.request.id == route_id)
        .ok_or_else(|| format!("local route {} does not exist", route_id.get()))?;

    if route.request.class != RouteClass::SessionMessage {
        return Err(format!(
            "route {} is not a local session-message route",
            route_id.get()
        ));
    }
    if route.initial_policy != RoutePolicy::RequireApproval {
        return Err(format!(
            "route {} was not created under explicit-approval policy",
            route_id.get()
        ));
    }

    let directory = replay_local_routing_directory(store.events())?;
    let source = directory
        .iter()
        .find(|entry| entry.endpoint_id == route.request.source)
        .ok_or_else(|| {
            format!(
                "route {} source endpoint {} is no longer the current local leaf",
                route_id.get(),
                route.request.source.get()
            )
        })?;
    let destination = directory
        .iter()
        .find(|entry| entry.endpoint_id == route.request.destination)
        .ok_or_else(|| {
            format!(
                "route {} destination endpoint {} is no longer the current local leaf",
                route_id.get(),
                route.request.destination.get()
            )
        })?;
    if source.conversation_id != payload.source_conversation_id
        || destination.conversation_id != payload.destination_conversation_id
    {
        return Err(format!(
            "route {} current endpoint ownership disagrees with immutable payload provenance",
            route_id.get()
        ));
    }
    if !source.current_session_phase.accepts_ordinary_turns()
        || !destination.current_session_phase.accepts_ordinary_turns()
    {
        return Err(format!(
            "route {} cannot deliver because one current session leaf no longer accepts ordinary turns",
            route_id.get()
        ));
    }

    let before = store.events().len();
    let dispatch_sequence = match route.gate_state {
        RouteGateState::Allowed { .. } => {
            let mut gate = route_gate_before_dispatch(&route)?;
            let permit = gate.authorize_dispatch(route_id).map_err(|error| {
                format!("route {} dispatch gate rejected: {error:?}", route_id.get())
            })?;
            record_route_dispatched(store, permit).map_err(|error| error.to_string())?
        }
        RouteGateState::Dispatched { .. } => route.dispatch_sequence.ok_or_else(|| {
            format!(
                "route {} is marked dispatched without a durable dispatch sequence",
                route_id.get()
            )
        })?,
        RouteGateState::PendingApproval => {
            return Err(format!(
                "route {} is still awaiting explicit user approval",
                route_id.get()
            ));
        }
        RouteGateState::Denied { .. } => {
            return Err(format!(
                "route {} is denied and cannot dispatch",
                route_id.get()
            ));
        }
    };

    let dispatched = replay_routing_audit(store.events())?
        .into_iter()
        .find(|candidate| candidate.request.id == route_id)
        .ok_or_else(|| "local route disappeared after dispatch append".to_owned())?;
    if !dispatched.gate_state.is_dispatched()
        || dispatched.dispatch_sequence != Some(dispatch_sequence)
    {
        return Err("local route dispatch replay disagrees with durable dispatch".to_owned());
    }

    record_local_route_delivered(
        store,
        route_id,
        payload.payload_id,
        payload.source_conversation_id,
        payload.destination_conversation_id,
        source.current_session_id,
        destination.current_session_id,
        dispatch_sequence,
    )
    .map_err(|error| error.to_string())?;

    let delivery = replay_local_route_delivery_audit(store.events())?
        .into_iter()
        .find(|delivery| delivery.route_id == route_id)
        .ok_or_else(|| "local route delivery append did not replay".to_owned())?;
    if delivery.payload_id != payload.payload_id
        || delivery.source_conversation_id != payload.source_conversation_id
        || delivery.destination_conversation_id != payload.destination_conversation_id
        || delivery.source_session_id != source.current_session_id
        || delivery.destination_session_id != destination.current_session_id
        || delivery.dispatch_sequence != dispatch_sequence
    {
        return Err("local route delivery replay disagrees with appended delivery".to_owned());
    }

    Ok(store.events()[before..].to_vec())
}

pub(crate) fn append_local_route_context_decision_checked(
    store: &mut impl EventStore,
    route_id: RouteId,
    destination_conversation_id: LocalConversationId,
    decision: LocalRouteContextDecision,
) -> Result<EventEnvelope, String> {
    let inbox_item =
        replay_local_routed_inbox_for_conversation(store.events(), destination_conversation_id)?
            .into_iter()
            .find(|item| item.route_id == route_id)
            .ok_or_else(|| {
                format!(
                    "local route {} is not a delivered routed inbox item for conversation {}",
                    route_id.get(),
                    destination_conversation_id,
                )
            })?;

    if let Some(existing) = replay_local_route_context_audit(store.events())?
        .into_iter()
        .find(|record| record.route_id == route_id)
    {
        if existing.destination_conversation_id != destination_conversation_id
            || existing.payload_id != inbox_item.payload_id
        {
            return Err(format!(
                "local route {} context provenance conflicts with delivered inbox item",
                route_id.get()
            ));
        }
        if existing.decision == decision {
            return Err(format!(
                "local route {} already has routed context decision {}",
                route_id.get(),
                local_route_context_decision_label(decision),
            ));
        }
    }

    record_local_route_context_decision(
        store,
        route_id,
        inbox_item.payload_id,
        destination_conversation_id,
        decision,
    )
    .map_err(|error| error.to_string())?;

    let replayed = replay_local_route_context_audit(store.events())?
        .into_iter()
        .find(|record| record.route_id == route_id)
        .ok_or_else(|| "routed context decision append did not replay".to_owned())?;
    if replayed.payload_id != inbox_item.payload_id
        || replayed.destination_conversation_id != destination_conversation_id
        || replayed.decision != decision
    {
        return Err("routed context decision replay disagrees with appended decision".to_owned());
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "routed context decision append produced no durable event".to_owned())
}

pub(crate) fn append_tool_provider_registration_checked(
    store: &mut impl EventStore,
    provider_id: ToolProviderId,
    name: ToolProviderName,
) -> Result<EventEnvelope, String> {
    if replay_tool_provider_audit(store.events())?
        .iter()
        .any(|record| record.provider_id == provider_id)
    {
        return Err(format!(
            "tool provider {} already exists",
            provider_id.get()
        ));
    }

    record_tool_provider_registered(store, provider_id, &name)
        .map_err(|error| error.to_string())?;

    let replayed = replay_tool_provider_audit(store.events())?
        .into_iter()
        .find(|record| record.provider_id == provider_id)
        .ok_or_else(|| "tool provider registration did not replay".to_owned())?;
    if replayed.name != name || replayed.endpoint_binding.is_some() {
        return Err("tool provider replay disagrees with registration".to_owned());
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "tool provider registration produced no durable event".to_owned())
}

pub(crate) fn append_tool_provider_endpoint_checked(
    store: &mut impl EventStore,
    provider_id: ToolProviderId,
    endpoint_id: RouteEndpointId,
) -> Result<EventEnvelope, String> {
    let provider = replay_tool_provider_audit(store.events())?
        .into_iter()
        .find(|record| record.provider_id == provider_id)
        .ok_or_else(|| format!("tool provider {} does not exist", provider_id.get()))?;
    if let Some(existing) = provider.endpoint_binding {
        return Err(format!(
            "tool provider {} is already bound to routing endpoint {}",
            provider_id.get(),
            existing.endpoint_id().get()
        ));
    }

    if replay_session_audit(store.events())?.iter().any(|record| {
        record
            .endpoint_binding
            .is_some_and(|binding| binding.endpoint_id() == endpoint_id)
    }) {
        return Err(format!(
            "routing endpoint {} is already bound to a local session",
            endpoint_id.get()
        ));
    }
    if replay_tool_provider_audit(store.events())?
        .iter()
        .any(|record| {
            record
                .endpoint_binding
                .is_some_and(|binding| binding.endpoint_id() == endpoint_id)
        })
    {
        return Err(format!(
            "routing endpoint {} is already bound to another tool provider",
            endpoint_id.get()
        ));
    }
    if replay_routing_audit(store.events())?.iter().any(|route| {
        route.request.source == endpoint_id || route.request.destination == endpoint_id
    }) {
        return Err(format!(
            "routing endpoint {} already appears in durable route history",
            endpoint_id.get()
        ));
    }

    record_tool_provider_endpoint_bound(
        store,
        ToolProviderEndpointBinding::new(provider_id, endpoint_id),
    )
    .map_err(|error| error.to_string())?;

    let replayed = replay_tool_provider_audit(store.events())?
        .into_iter()
        .find(|record| record.provider_id == provider_id)
        .and_then(|record| record.endpoint_binding)
        .ok_or_else(|| "tool provider endpoint binding did not replay".to_owned())?;
    if replayed.endpoint_id() != endpoint_id {
        return Err("tool provider endpoint replay disagrees with appended endpoint".to_owned());
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "tool provider endpoint binding produced no durable event".to_owned())
}

pub(crate) fn append_tool_call_proposal_checked(
    store: &mut impl EventStore,
    call_id: ToolCallId,
    route_id: RouteId,
    source_conversation_id: LocalConversationId,
    provider_id: ToolProviderId,
    operation: ToolOperationName,
    arguments_text: String,
    policy: RoutePolicy,
) -> Result<Vec<EventEnvelope>, String> {
    if policy == RoutePolicy::Deny {
        return Err("disabled tool cannot be proposed for execution".to_owned());
    }
    if arguments_text.trim_start().starts_with("<tool_call") {
        let parsed = parse_legacy_tool_call(&arguments_text)
            .map_err(|error| format!("invalid legacy tool envelope: {error:?}"))?;
        if parsed.name != operation.as_str() {
            return Err(format!(
                "legacy envelope operation {} disagrees with selected operation {}",
                parsed.name,
                operation.as_str(),
            ));
        }
    }
    if replay_tool_call_audit(store.events())?
        .iter()
        .any(|record| record.call_id == call_id)
    {
        return Err(format!("tool call {} already exists", call_id.get()));
    }
    if replay_routing_audit(store.events())?
        .iter()
        .any(|route| route.request.id == route_id)
    {
        return Err(format!("route {} already exists", route_id.get()));
    }

    let directory = replay_local_routing_directory(store.events())?;
    let source = directory
        .iter()
        .find(|entry| entry.conversation_id == source_conversation_id)
        .ok_or_else(|| {
            format!(
                "source local conversation {source_conversation_id} is not currently addressable"
            )
        })?;
    if !source.current_session_phase.accepts_ordinary_turns() {
        return Err(format!(
            "source conversation {source_conversation_id} current session {} is {} and cannot originate a tool call",
            source.current_session_id.get(),
            session_lifecycle_phase_label(source.current_session_phase),
        ));
    }

    let provider = replay_tool_provider_audit(store.events())?
        .into_iter()
        .find(|record| record.provider_id == provider_id)
        .ok_or_else(|| format!("tool provider {} does not exist", provider_id.get()))?;
    let provider_endpoint = provider.endpoint_binding.ok_or_else(|| {
        format!(
            "tool provider {} is not routing-addressable",
            provider_id.get()
        )
    })?;

    let before = store.events().len();
    record_tool_call(
        store,
        call_id,
        source.current_session_id,
        provider_id,
        &operation,
        arguments_text.clone(),
    )
    .map_err(|error| error.to_string())?;

    let request = RouteRequest {
        id: route_id,
        source: source.endpoint_id,
        destination: provider_endpoint.endpoint_id(),
        class: RouteClass::ToolCall,
    };
    record_route_proposed(store, request, policy).map_err(|error| error.to_string())?;
    record_tool_call_route_bound(store, call_id, route_id).map_err(|error| error.to_string())?;

    let replayed = replay_tool_call_audit(store.events())?
        .into_iter()
        .find(|record| record.call_id == call_id)
        .ok_or_else(|| "tool call proposal did not replay".to_owned())?;
    if replayed.source_session_id != source.current_session_id
        || replayed.provider_id != provider_id
        || replayed.operation != operation
        || replayed.arguments_text != arguments_text
        || replayed.route_id != Some(route_id)
    {
        return Err("tool call replay disagrees with appended proposal".to_owned());
    }

    let route = replay_routing_audit(store.events())?
        .into_iter()
        .find(|route| route.request.id == route_id)
        .ok_or_else(|| "tool call route proposal did not replay".to_owned())?;
    let expected_gate = match policy {
        RoutePolicy::Allow => RouteGateState::Allowed {
            by: DecisionAuthority::Policy,
        },
        RoutePolicy::RequireApproval => RouteGateState::PendingApproval,
        RoutePolicy::Deny => unreachable!(),
    };
    if route.request != request
        || route.initial_policy != policy
        || route.gate_state != expected_gate
    {
        return Err("tool call route replay disagrees with recorded policy".to_owned());
    }

    Ok(store.events()[before..].to_vec())
}

/// Prepare only addressable, checked, journaled routing identities for a
/// model-authored MCP request. The model never chooses a provider ID, route
/// ID, executable, policy authority or a dispatch permit.
pub(crate) fn append_agent_mcp_call_checked(
    store: &mut impl EventStore,
    source_conversation_id: LocalConversationId,
    proposal: &mcp_agent_defaults::ModelToolRequest,
    policy: mcp_agent_defaults::ToolPolicy,
) -> Result<(ToolCallId, RouteId, Vec<EventEnvelope>), String> {
    if !local_http_mcp::allowed_model_operation(&proposal.provider, &proposal.tool)
        && proposal.provider == local_http_mcp::OPENVIKING
        && proposal.tool == "write"
    {
        // Reject before creating a route, even for an old saved Allow policy.
        // The host-owned outbox remains the only checkpoint write authority.
        return Err(
            "OpenViking writes use Chatarium's automatic verified checkpoint lifecycle".to_owned(),
        );
    }
    if matches!(
        policy,
        mcp_agent_defaults::ToolPolicy::Disabled | mcp_agent_defaults::ToolPolicy::Manual
    ) {
        return Err("this MCP tool cannot be invoked automatically by the model".to_owned());
    }
    let starting = store.events().len();
    if local_conversation_topology(store.events(), source_conversation_id)?.is_none() {
        let (container, session) = next_available_local_orchestration_ids(store.events())?;
        append_local_orchestration_topology_checked(
            store,
            source_conversation_id,
            container,
            session,
        )?;
    }
    let topology = local_conversation_topology(store.events(), source_conversation_id)?
        .ok_or("agent MCP source has no local topology")?;
    if session_endpoint_binding(store.events(), topology.current_session_id)?.is_none() {
        let endpoint = next_available_route_endpoint_id(store.events())?;
        append_current_session_route_endpoint_checked(
            store,
            source_conversation_id,
            topology.current_session_id,
            endpoint,
        )?;
    }

    let provider_name = ToolProviderName::new(proposal.provider.clone())
        .map_err(|error| format!("agent MCP provider name: {error:?}"))?;
    let mut registered = replay_tool_provider_audit(store.events())?
        .into_iter()
        .find(|record| record.name == provider_name);
    if registered.is_none()
        && (proposal.provider == local_tool_adapter::BUILTIN_PROVIDER_NAME
            || local_http_mcp::known_provider(&proposal.provider))
    {
        let provider_id = next_available_tool_provider_id(store.events())?;
        append_tool_provider_registration_checked(store, provider_id, provider_name.clone())?;
        let endpoint = next_available_route_endpoint_id(store.events())?;
        append_tool_provider_endpoint_checked(store, provider_id, endpoint)?;
        registered = replay_tool_provider_audit(store.events())?
            .into_iter()
            .find(|record| record.provider_id == provider_id);
    }
    let provider = registered.ok_or("agent requested an unregistered MCP provider")?;
    if provider.endpoint_binding.is_none() {
        return Err("agent MCP provider has no bound routing endpoint".to_owned());
    }
    if proposal.provider == local_tool_adapter::BUILTIN_PROVIDER_NAME {
        if !local_tool_adapter::supports_builtin(&proposal.provider, &proposal.tool) {
            return Err("unrecognized builtin MCP operation".to_owned());
        }
    } else if local_http_mcp::known_provider(&proposal.provider) {
        if !local_http_mcp::allowed_operation(&proposal.provider, &proposal.tool) {
            return Err("unrecognized fixed localhost MCP operation".to_owned());
        }
    } else {
        let config = replay_tool_transport_config_audit(store.events())?
            .into_iter()
            .find(|record| record.provider_id == provider.provider_id)
            .ok_or("agent MCP external provider has no immutable stdio configuration")?;
        if !config
            .config
            .allowed_operations()
            .iter()
            .any(|op| op.as_str() == proposal.tool)
        {
            return Err("agent MCP operation is outside the provider allowlist".to_owned());
        }
        if policy == mcp_agent_defaults::ToolPolicy::Automatic
            && !replay_tool_provider_activation_audit(store.events())?
                .into_iter()
                .any(|record| record.provider_id == provider.provider_id && record.active)
        {
            return Err("agent cannot automatically call an inactive MCP provider".to_owned());
        }
    }
    let call_id = next_available_tool_call_id(store.events())?;
    let route_id = next_available_route_id(store.events())?;
    let operation = ToolOperationName::new(proposal.tool.clone())
        .map_err(|error| format!("agent MCP tool name: {error:?}"))?;
    let route_policy = match policy {
        mcp_agent_defaults::ToolPolicy::Automatic => RoutePolicy::Allow,
        mcp_agent_defaults::ToolPolicy::Ask => RoutePolicy::RequireApproval,
        _ => unreachable!(),
    };
    append_tool_call_proposal_checked(
        store,
        call_id,
        route_id,
        source_conversation_id,
        provider.provider_id,
        operation,
        proposal.arguments.to_string(),
        route_policy,
    )?;
    Ok((call_id, route_id, store.events()[starting..].to_vec()))
}

pub(crate) fn append_tool_call_route_user_decision_checked(
    store: &mut impl EventStore,
    route_id: RouteId,
    decision: RouteUserDecision,
) -> Result<EventEnvelope, String> {
    let call = replay_tool_call_audit(store.events())?
        .into_iter()
        .find(|record| record.route_id == Some(route_id))
        .ok_or_else(|| format!("route {} is not bound to a tool call", route_id.get()))?;
    let route = replay_routing_audit(store.events())?
        .into_iter()
        .find(|record| record.request.id == route_id)
        .ok_or_else(|| format!("tool route {} does not exist", route_id.get()))?;

    if route.request.class != RouteClass::ToolCall {
        return Err(format!("route {} is not a tool-call route", route_id.get()));
    }
    if route.initial_policy != RoutePolicy::RequireApproval {
        return Err(format!(
            "tool route {} was not created under explicit-approval policy",
            route_id.get()
        ));
    }
    if route.gate_state.is_dispatched() {
        return Err(format!(
            "tool route {} has already dispatched and its policy history is immutable",
            route_id.get()
        ));
    }
    if route.latest_user_decision == Some(decision) {
        return Err(format!(
            "tool route {} already has explicit user decision {}",
            route_id.get(),
            route_user_decision_label(decision),
        ));
    }
    if call.route_bound_sequence.is_none() {
        return Err(format!(
            "tool call {} has no durable route correlation",
            call.call_id.get()
        ));
    }

    record_route_user_decision(store, route_id, decision).map_err(|error| error.to_string())?;

    let replayed = replay_routing_audit(store.events())?
        .into_iter()
        .find(|candidate| candidate.request.id == route_id)
        .ok_or_else(|| "tool route decision append did not replay".to_owned())?;
    let expected_state = match decision {
        RouteUserDecision::Allow => RouteGateState::Allowed {
            by: DecisionAuthority::User,
        },
        RouteUserDecision::Deny => RouteGateState::Denied {
            by: DecisionAuthority::User,
        },
    };
    if replayed.latest_user_decision != Some(decision) || replayed.gate_state != expected_state {
        return Err("tool route decision replay disagrees with appended decision".to_owned());
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "tool route decision append produced no durable event".to_owned())
}

pub(crate) fn append_builtin_hello_checked(
    store: &mut impl EventStore,
    route_id: RouteId,
) -> Result<Vec<EventEnvelope>, String> {
    let call = replay_tool_call_audit(store.events())?
        .into_iter()
        .find(|call| call.route_id == Some(route_id))
        .ok_or_else(|| {
            format!(
                "route {} is not bound to a durable tool call",
                route_id.get()
            )
        })?;
    let provider = replay_tool_provider_audit(store.events())?
        .into_iter()
        .find(|provider| provider.provider_id == call.provider_id)
        .ok_or_else(|| {
            format!(
                "tool call {} references missing provider {}",
                call.call_id.get(),
                call.provider_id.get()
            )
        })?;
    if !local_tool_adapter::supports_builtin(provider.name.as_str(), call.operation.as_str()) {
        return Err(format!(
            "tool call {} is not supported by the restricted local builtin adapter",
            call.call_id.get()
        ));
    }
    let route = replay_routing_audit(store.events())?
        .into_iter()
        .find(|route| route.request.id == route_id)
        .ok_or_else(|| format!("tool route {} does not exist", route_id.get()))?;
    let allowed = match route.initial_policy {
        RoutePolicy::Allow => {
            route.gate_state
                == (RouteGateState::Allowed {
                    by: DecisionAuthority::Policy,
                })
                && route.latest_user_decision.is_none()
        }
        RoutePolicy::RequireApproval => {
            route.gate_state
                == (RouteGateState::Allowed {
                    by: DecisionAuthority::User,
                })
                && route.latest_user_decision == Some(RouteUserDecision::Allow)
        }
        RoutePolicy::Deny => false,
    };
    if route.request.class != RouteClass::ToolCall || !allowed {
        return Err(format!(
            "tool route {} requires a fresh explicit user Allow or saved exact policy before builtin execution",
            route_id.get()
        ));
    }
    if replay_tool_call_outcome_audit(store.events())?
        .iter()
        .any(|outcome| outcome.call_id == call.call_id || outcome.route_id == route_id)
    {
        return Err(format!(
            "tool route {} already has a durable terminal outcome",
            route_id.get()
        ));
    }

    let source_fresh = replay_local_routing_directory(store.events())?
        .into_iter()
        .any(|entry| {
            entry.current_session_id == call.source_session_id
                && entry.endpoint_id == route.request.source
                && entry.current_session_phase.accepts_ordinary_turns()
        });
    if !source_fresh {
        return Err(format!(
            "tool route {} source session is no longer the current addressable local leaf",
            route_id.get()
        ));
    }
    if !provider
        .endpoint_binding
        .is_some_and(|binding| binding.endpoint_id() == route.request.destination)
    {
        return Err(format!(
            "tool route {} destination no longer matches the registered provider endpoint",
            route_id.get()
        ));
    }

    // Prepare and validate the pure hello output before consuming any permit.
    // This function performs no filesystem, process, network, or tool side effects.
    let result_text = local_tool_adapter::prepare_builtin_response(&call, store.events())?;
    let mut gate = route_gate_before_dispatch(&route)?;
    let permit = gate
        .authorize_dispatch(route_id)
        .map_err(|error| format!("tool route {} dispatch denied: {error:?}", route_id.get()))?;

    let before = store.events().len();
    record_route_dispatched(store, permit).map_err(|error| error.to_string())?;
    // A crash after durable dispatch and before outcome remains unresolved.
    // Never automatically re-run a possibly state-changing adapter.
    append_tool_call_outcome_checked(
        store,
        call.call_id,
        route_id,
        ToolCallOutcomeKind::Result,
        result_text,
    )?;
    Ok(store.events()[before..].to_vec())
}

/// Reserve exactly one already authorized localhost HTTP MCP invocation.
/// The route is durably dispatched before any network access, with no retry
/// after uncertainty. This is distinct from confined stdio configuration.
pub(crate) fn reserve_local_http_mcp_checked(
    store: &mut impl EventStore,
    route_id: RouteId,
) -> Result<(ToolCallId, String, String, String, Value, EventEnvelope), String> {
    let call = replay_tool_call_audit(store.events())?
        .into_iter()
        .find(|call| call.route_id == Some(route_id))
        .ok_or_else(|| {
            format!(
                "localhost MCP route {} has no recorded tool call",
                route_id.get()
            )
        })?;
    let provider = replay_tool_provider_audit(store.events())?
        .into_iter()
        .find(|provider| provider.provider_id == call.provider_id)
        .ok_or("localhost MCP provider missing")?;
    let name = provider.name.as_str().to_owned();
    let operation = call.operation.as_str().to_owned();
    if !local_http_mcp::allowed_model_operation(&name, &operation) {
        return Err(
            "OpenViking model-authored checkpoint writes are blocked; host outbox only".to_owned(),
        );
    }
    let route = replay_routing_audit(store.events())?
        .into_iter()
        .find(|route| route.request.id == route_id)
        .ok_or("localhost MCP tool route missing")?;
    let allowed = match route.initial_policy {
        RoutePolicy::Allow => {
            route.gate_state
                == RouteGateState::Allowed {
                    by: DecisionAuthority::Policy,
                }
                && route.latest_user_decision.is_none()
        }
        RoutePolicy::RequireApproval => {
            route.gate_state
                == RouteGateState::Allowed {
                    by: DecisionAuthority::User,
                }
                && route.latest_user_decision == Some(RouteUserDecision::Allow)
        }
        RoutePolicy::Deny => false,
    };
    if route.request.class != RouteClass::ToolCall || !allowed {
        return Err(
            "localhost MCP call needs a persisted exact standing policy or user Allow".to_owned(),
        );
    }
    if replay_tool_call_outcome_audit(store.events())?
        .iter()
        .any(|outcome| outcome.call_id == call.call_id || outcome.route_id == route_id)
    {
        return Err("localhost MCP call already has a terminal observation".to_owned());
    }
    let source = replay_local_routing_directory(store.events())?
        .into_iter()
        .find(|entry| {
            entry.current_session_id == call.source_session_id
                && entry.endpoint_id == route.request.source
                && entry.current_session_phase.accepts_ordinary_turns()
        })
        .ok_or("localhost MCP source conversation is no longer addressable")?;
    if !provider
        .endpoint_binding
        .is_some_and(|binding| binding.endpoint_id() == route.request.destination)
    {
        return Err("localhost MCP route destination differs from provider endpoint".to_owned());
    }
    let args: Value = serde_json::from_str(&call.arguments_text)
        .map_err(|e| format!("invalid localhost MCP tool arguments: {e}"))?;
    if !args.is_object() || call.arguments_text.len() > 256 * 1024 {
        return Err("localhost MCP arguments must be a bounded JSON object".to_owned());
    }
    let mut gate = route_gate_before_dispatch(&route)?;
    let permit = gate
        .authorize_dispatch(route_id)
        .map_err(|error| format!("localhost MCP dispatch denied: {error:?}"))?;
    record_route_dispatched(store, permit).map_err(|error| error.to_string())?;
    let event = store
        .events()
        .last()
        .cloned()
        .ok_or("localhost MCP dispatch event missing")?;
    Ok((
        call.call_id,
        source.conversation_id.to_string(),
        name,
        operation,
        args,
        event,
    ))
}

pub(crate) fn reserve_builtin_host_shell_checked(
    store: &mut impl EventStore,
    route_id: RouteId,
    settings: &host_shell_mcp::Settings,
) -> Result<(ToolCallId, Value, EventEnvelope), String> {
    let call = replay_tool_call_audit(store.events())?
        .into_iter()
        .find(|call| call.route_id == Some(route_id))
        .ok_or_else(|| {
            format!(
                "route {} is not bound to a durable tool call",
                route_id.get()
            )
        })?;
    let provider = replay_tool_provider_audit(store.events())?
        .into_iter()
        .find(|provider| provider.provider_id == call.provider_id)
        .ok_or_else(|| {
            format!(
                "tool call {} references missing provider {}",
                call.call_id.get(),
                call.provider_id.get()
            )
        })?;
    if provider.name.as_str() != local_tool_adapter::BUILTIN_PROVIDER_NAME
        || call.operation.as_str() != host_shell_mcp::OPERATION
    {
        return Err("route is not an authorized builtin host shell operation".to_owned());
    }
    let route = replay_routing_audit(store.events())?
        .into_iter()
        .find(|route| route.request.id == route_id)
        .ok_or_else(|| format!("tool route {} does not exist", route_id.get()))?;
    let allowed = match route.initial_policy {
        RoutePolicy::Allow => {
            route.gate_state
                == (RouteGateState::Allowed {
                    by: DecisionAuthority::Policy,
                })
                && route.latest_user_decision.is_none()
        }
        RoutePolicy::RequireApproval => {
            route.gate_state
                == (RouteGateState::Allowed {
                    by: DecisionAuthority::User,
                })
                && route.latest_user_decision == Some(RouteUserDecision::Allow)
        }
        RoutePolicy::Deny => false,
    };
    if route.request.class != RouteClass::ToolCall || !allowed {
        return Err(format!(
            "tool route {} requires a fresh explicit user Allow or saved exact policy before builtin execution",
            route_id.get()
        ));
    }
    if replay_tool_call_outcome_audit(store.events())?
        .iter()
        .any(|outcome| outcome.call_id == call.call_id || outcome.route_id == route_id)
    {
        return Err(format!(
            "tool route {} already has a durable terminal outcome",
            route_id.get()
        ));
    }

    let source_fresh = replay_local_routing_directory(store.events())?
        .into_iter()
        .any(|entry| {
            entry.current_session_id == call.source_session_id
                && entry.endpoint_id == route.request.source
                && entry.current_session_phase.accepts_ordinary_turns()
        });
    if !source_fresh {
        return Err(format!(
            "tool route {} source session is no longer the current addressable local leaf",
            route_id.get()
        ));
    }
    if !provider
        .endpoint_binding
        .is_some_and(|binding| binding.endpoint_id() == route.request.destination)
    {
        return Err(format!(
            "tool route {} destination no longer matches the registered provider endpoint",
            route_id.get()
        ));
    }

    // Parse and validate the complete command BEFORE consuming the route
    // permit. Execution itself occurs only on a dedicated subprocess worker.
    let args: Value = serde_json::from_str(&call.arguments_text)
        .map_err(|e| format!("invalid host shell argument object: {e}"))?;
    settings.validate_request(&args)?;
    if !settings.shell_path().is_file() || !std::path::Path::new("/usr/bin/timeout").is_file() {
        return Err("selected shell or /usr/bin/timeout is unavailable".to_owned());
    }
    let mut gate = route_gate_before_dispatch(&route)?;
    let permit = gate
        .authorize_dispatch(route_id)
        .map_err(|error| format!("host shell route denied: {error:?}"))?;
    record_route_dispatched(store, permit).map_err(|e| e.to_string())?;
    let event = store
        .events()
        .last()
        .cloned()
        .ok_or("host shell dispatch journal event missing")?;
    Ok((call.call_id, args, event))
}

pub(crate) fn commit_message_with_one_shot_memory_checked(
    store: &mut impl EventStore,
    message: &AuthoredUserMessage,
    one_shot_memory_ids: &[LocalMemoryId],
    one_shot_memory_snapshot_after_sequence: Option<u64>,
) -> Result<(EventEnvelope, Option<EventEnvelope>), String> {
    commit_message_with_attachment_and_one_shot_memory_checked(
        store,
        message,
        None,
        one_shot_memory_ids,
        one_shot_memory_snapshot_after_sequence,
    )
}

pub(crate) fn commit_message_with_attachment_and_one_shot_memory_checked(
    store: &mut impl EventStore,
    message: &AuthoredUserMessage,
    attachment: Option<&Value>,
    one_shot_memory_ids: &[LocalMemoryId],
    one_shot_memory_snapshot_after_sequence: Option<u64>,
) -> Result<(EventEnvelope, Option<EventEnvelope>), String> {
    match (
        one_shot_memory_ids.is_empty(),
        one_shot_memory_snapshot_after_sequence,
    ) {
        (true, None) => {}
        (true, Some(_)) => {
            return Err(
                "empty one-shot local memory set cannot carry a snapshot high-water".to_owned(),
            );
        }
        (false, None) => {
            return Err(
                "non-empty one-shot local memory set requires a snapshot high-water".to_owned(),
            );
        }
        (false, Some(snapshot_after_sequence)) => {
            validate_local_memory_one_shot_snapshot(
                store.events(),
                message.conversation_id,
                one_shot_memory_ids,
                snapshot_after_sequence,
            )?;
        }
    }

    commit_user_message_with_attachment(store, message, attachment)
        .map_err(|error| error.to_string())?;
    let commit_event = store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "user-message commit produced no durable event".to_owned())?;

    let one_shot_event = if let Some(snapshot_after_sequence) =
        one_shot_memory_snapshot_after_sequence
    {
        record_local_memory_turn_selection(
            store,
            message.conversation_id,
            message.turn_id,
            one_shot_memory_ids,
            snapshot_after_sequence,
        )
        .map_err(|error| error.to_string())?;

        let event = store.events().last().cloned().ok_or_else(|| {
            "one-shot local memory selection produced no durable event".to_owned()
        })?;

        let replayed = replay_local_memory_turn_selection_audit(store.events())?
            .into_iter()
            .find(|record| record.turn_id == message.turn_id)
            .ok_or_else(|| "one-shot local memory selection append did not replay".to_owned())?;
        if replayed.conversation_id != message.conversation_id
            || replayed.memory_ids != one_shot_memory_ids
            || replayed.selection_snapshot_after_sequence != snapshot_after_sequence
            || replayed.user_message_sequence != commit_event.sequence
            || replayed.recorded_sequence != event.sequence
        {
            return Err(
                "one-shot local memory selection replay disagrees with appended selection"
                    .to_owned(),
            );
        }
        Some(event)
    } else {
        None
    };

    Ok((commit_event, one_shot_event))
}

pub(crate) fn append_local_memory_artifact_checked(
    store: &mut impl EventStore,
    memory_id: LocalMemoryId,
    source_conversation_id: LocalConversationId,
    text: String,
) -> Result<EventEnvelope, String> {
    if text.trim().is_empty() {
        return Err("local memory text cannot be empty".to_owned());
    }
    if replay_local_memory_audit(store.events())?
        .iter()
        .any(|record| record.memory_id == memory_id)
    {
        return Err(format!("local memory {} already exists", memory_id.get()));
    }

    record_local_memory_artifact(store, memory_id, source_conversation_id, text.clone())
        .map_err(|error| error.to_string())?;

    let replayed = replay_local_memory_audit(store.events())?
        .into_iter()
        .find(|record| record.memory_id == memory_id)
        .ok_or_else(|| "local memory append did not replay".to_owned())?;
    if replayed.source_conversation_id != source_conversation_id || replayed.text != text {
        return Err("local memory replay disagrees with appended artifact".to_owned());
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "local memory append produced no durable event".to_owned())
}

pub(crate) fn append_local_memory_context_decision_checked(
    store: &mut impl EventStore,
    memory_id: LocalMemoryId,
    destination_conversation_id: LocalConversationId,
    decision: LocalMemoryContextDecision,
) -> Result<EventEnvelope, String> {
    let artifact = replay_local_memory_audit(store.events())?
        .into_iter()
        .find(|record| record.memory_id == memory_id)
        .ok_or_else(|| format!("local memory {} does not exist", memory_id.get()))?;

    if decision == LocalMemoryContextDecision::Admit {
        let supersessions = replay_local_memory_supersession_audit(store.events())?;
        if let Some(lineage) = supersessions
            .iter()
            .find(|record| record.predecessor_memory_id == memory_id)
        {
            return Err(format!(
                "local memory {} is superseded by {} and cannot be admitted",
                memory_id.get(),
                lineage.successor_memory_id.get(),
            ));
        }
    }

    if let Some(existing) = replay_local_memory_context_audit(store.events())?
        .into_iter()
        .find(|record| {
            record.memory_id == memory_id
                && record.destination_conversation_id == destination_conversation_id
        })
    {
        if existing.source_conversation_id != artifact.source_conversation_id
            || existing.artifact_sequence != artifact.recorded_sequence
        {
            return Err(format!(
                "local memory {} context provenance conflicts with artifact",
                memory_id.get()
            ));
        }
        if existing.decision == decision {
            return Err(format!(
                "local memory {} already has context decision {} for conversation {}",
                memory_id.get(),
                local_memory_context_decision_label(decision),
                destination_conversation_id,
            ));
        }
    }

    record_local_memory_context_decision(store, memory_id, destination_conversation_id, decision)
        .map_err(|error| error.to_string())?;

    let replayed = replay_local_memory_context_audit(store.events())?
        .into_iter()
        .find(|record| {
            record.memory_id == memory_id
                && record.destination_conversation_id == destination_conversation_id
        })
        .ok_or_else(|| "local memory context decision append did not replay".to_owned())?;
    if replayed.source_conversation_id != artifact.source_conversation_id
        || replayed.artifact_sequence != artifact.recorded_sequence
        || replayed.decision != decision
    {
        return Err(
            "local memory context decision replay disagrees with appended decision".to_owned(),
        );
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "local memory context decision append produced no durable event".to_owned())
}

pub(crate) fn append_local_memory_supersession_checked(
    store: &mut impl EventStore,
    predecessor_memory_id: LocalMemoryId,
    successor_memory_id: LocalMemoryId,
) -> Result<EventEnvelope, String> {
    if predecessor_memory_id == successor_memory_id {
        return Err(format!(
            "local memory {} cannot supersede itself",
            predecessor_memory_id.get()
        ));
    }

    let artifacts = replay_local_memory_audit(store.events())?;
    let predecessor = artifacts
        .iter()
        .find(|record| record.memory_id == predecessor_memory_id)
        .ok_or_else(|| {
            format!(
                "predecessor local memory {} does not exist",
                predecessor_memory_id.get()
            )
        })?;
    let successor = artifacts
        .iter()
        .find(|record| record.memory_id == successor_memory_id)
        .ok_or_else(|| {
            format!(
                "successor local memory {} does not exist",
                successor_memory_id.get()
            )
        })?;

    if predecessor.source_conversation_id != successor.source_conversation_id {
        return Err(format!(
            "local memory {} and {} have different source conversations",
            predecessor_memory_id.get(),
            successor_memory_id.get(),
        ));
    }
    if successor.recorded_sequence <= predecessor.recorded_sequence {
        return Err(format!(
            "successor local memory {} must be recorded after predecessor {}",
            successor_memory_id.get(),
            predecessor_memory_id.get(),
        ));
    }

    let existing = replay_local_memory_supersession_audit(store.events())?;
    if let Some(record) = existing
        .iter()
        .find(|record| record.predecessor_memory_id == predecessor_memory_id)
    {
        return Err(format!(
            "local memory {} is already superseded by {}",
            predecessor_memory_id.get(),
            record.successor_memory_id.get(),
        ));
    }
    if let Some(record) = existing
        .iter()
        .find(|record| record.successor_memory_id == successor_memory_id)
    {
        return Err(format!(
            "local memory {} already succeeds memory {}",
            successor_memory_id.get(),
            record.predecessor_memory_id.get(),
        ));
    }

    record_local_memory_superseded(store, predecessor_memory_id, successor_memory_id)
        .map_err(|error| error.to_string())?;

    let replayed = replay_local_memory_supersession_audit(store.events())?
        .into_iter()
        .find(|record| record.predecessor_memory_id == predecessor_memory_id)
        .ok_or_else(|| "local memory supersession append did not replay".to_owned())?;
    if replayed.successor_memory_id != successor_memory_id {
        return Err("local memory supersession replay disagrees with appended lineage".to_owned());
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "local memory supersession append produced no durable event".to_owned())
}

pub(crate) fn append_local_memory_label_change_checked(
    store: &mut impl EventStore,
    memory_id: LocalMemoryId,
    label: LocalMemoryLabel,
    add: bool,
) -> Result<EventEnvelope, String> {
    if !replay_local_memory_audit(store.events())?
        .iter()
        .any(|artifact| artifact.memory_id == memory_id)
    {
        return Err(format!("local memory {} does not exist", memory_id.get()));
    }

    let existing = replay_local_memory_label_audit(store.events())?
        .into_iter()
        .find(|record| record.memory_id == memory_id && record.label == label);

    if add {
        if existing.as_ref().is_some_and(|record| record.active) {
            return Err(format!(
                "local memory {} already has active label '{}'",
                memory_id.get(),
                label,
            ));
        }
        record_local_memory_label_added(store, memory_id, &label)
            .map_err(|error| error.to_string())?;
    } else {
        if !existing.as_ref().is_some_and(|record| record.active) {
            return Err(format!(
                "local memory {} does not have active label '{}'",
                memory_id.get(),
                label,
            ));
        }
        record_local_memory_label_removed(store, memory_id, &label)
            .map_err(|error| error.to_string())?;
    }

    let replayed = replay_local_memory_label_audit(store.events())?
        .into_iter()
        .find(|record| record.memory_id == memory_id && record.label == label)
        .ok_or_else(|| "local memory label append did not replay".to_owned())?;
    if replayed.active != add {
        return Err("local memory label replay disagrees with appended mutation".to_owned());
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "local memory label append produced no durable event".to_owned())
}

pub(crate) fn append_local_worker_binding_checked(
    store: &mut impl EventStore,
    conversation_id: LocalConversationId,
    worker_id: WorkerId,
) -> Result<EventEnvelope, String> {
    let bindings = replay_local_conversation_worker_bindings(store.events())?;
    if let Some(existing) = bindings
        .iter()
        .find(|binding| binding.conversation_id == conversation_id)
    {
        return Err(format!(
            "local conversation {conversation_id} is already bound to worker {}",
            existing.worker_id.get()
        ));
    }
    if let Some(existing) = bindings
        .iter()
        .find(|binding| binding.worker_id == worker_id)
    {
        return Err(format!(
            "worker {} is already bound to local conversation {}",
            worker_id.get(),
            existing.conversation_id
        ));
    }
    if replay_worker_audit(store.events())?
        .iter()
        .any(|record| record.worker_id == worker_id)
    {
        return Err(format!(
            "worker {} already has lifecycle history and cannot be rebound implicitly",
            worker_id.get()
        ));
    }
    if replay_session_audit(store.events())?.iter().any(|record| {
        record
            .worker_binding
            .is_some_and(|binding| binding.worker_id() == worker_id)
    }) {
        return Err(format!(
            "worker {} is already bound to a local session",
            worker_id.get()
        ));
    }

    record_local_conversation_worker_bound(store, conversation_id, worker_id)
        .map_err(|error| error.to_string())?;
    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "worker binding append produced no durable event".to_owned())
}

pub(crate) fn append_current_session_worker_binding_checked(
    store: &mut impl EventStore,
    conversation_id: LocalConversationId,
    worker_id: WorkerId,
) -> Result<EventEnvelope, String> {
    let conversation_worker = require_local_worker_binding(store.events(), worker_id)?;
    if conversation_worker.conversation_id != conversation_id {
        return Err(format!(
            "worker {} belongs to local conversation {}, not {}",
            worker_id.get(),
            conversation_worker.conversation_id,
            conversation_id,
        ));
    }

    let topology =
        local_conversation_topology(store.events(), conversation_id)?.ok_or_else(|| {
            format!("local conversation {conversation_id} has no orchestration topology")
        })?;
    let sessions = replay_session_audit(store.events())?;
    let current = sessions
        .iter()
        .find(|record| record.session_id == topology.current_session_id)
        .copied()
        .ok_or_else(|| {
            format!(
                "current session {} for local conversation {conversation_id} is not registered",
                topology.current_session_id.get()
            )
        })?;

    if replay_supervision_audit(store.events())?
        .controllers
        .iter()
        .any(|record| record.designation.session_id() == topology.current_session_id)
    {
        return Err(format!(
            "current session {} is controller-designated and cannot become worker-bound",
            topology.current_session_id.get()
        ));
    }

    match active_worker_session(&sessions, worker_id) {
        None => {
            if let Some(existing) = current.worker_binding {
                return Err(format!(
                    "current session {} is already bound to worker {}",
                    topology.current_session_id.get(),
                    existing.worker_id().get()
                ));
            }
            record_worker_session_bound(
                store,
                WorkerSessionBinding::new(worker_id, topology.current_session_id),
            )
            .map_err(|error| error.to_string())?;
        }
        Some(active) if active.session_id == topology.current_session_id => {
            return Err(format!(
                "worker {} is already active on current session {}",
                worker_id.get(),
                topology.current_session_id.get()
            ));
        }
        Some(active) => {
            if let Some(existing) = current.worker_binding {
                return Err(format!(
                    "current session {} is already bound to worker {}",
                    topology.current_session_id.get(),
                    existing.worker_id().get()
                ));
            }

            let container = replay_chat_container_audit(store.events())?
                .into_iter()
                .find(|record| record.container_id == topology.container_id)
                .ok_or_else(|| {
                    format!(
                        "chat container {} for local conversation {conversation_id} disappeared",
                        topology.container_id.get()
                    )
                })?;
            let current_leaf = container
                .sessions
                .iter()
                .find(|session| session.session_id == topology.current_session_id)
                .ok_or_else(|| {
                    format!(
                        "chat container {} lacks current session {}",
                        topology.container_id.get(),
                        topology.current_session_id.get()
                    )
                })?;
            if current_leaf.predecessor_session_id != Some(active.session_id) {
                return Err(format!(
                    "worker {} is active on session {}, but current session {} is not its direct chat-container successor",
                    worker_id.get(),
                    active.session_id.get(),
                    topology.current_session_id.get()
                ));
            }

            record_worker_session_successor_bound(
                store,
                WorkerSessionSuccessorBinding::new(
                    worker_id,
                    active.session_id,
                    topology.current_session_id,
                )
                .map_err(|error| format!("invalid worker-session successor: {error:?}"))?,
            )
            .map_err(|error| error.to_string())?;
        }
    }

    let replayed = replay_session_audit(store.events())?;
    let active = active_worker_session(&replayed, worker_id)
        .ok_or_else(|| "worker-session append did not replay".to_owned())?;
    if active.session_id != topology.current_session_id {
        return Err("worker-session replay did not advance to current session".to_owned());
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "worker-session append produced no durable event".to_owned())
}

pub(crate) fn append_current_session_controller_designation_checked(
    store: &mut impl EventStore,
    conversation_id: LocalConversationId,
) -> Result<EventEnvelope, String> {
    let topology =
        local_conversation_topology(store.events(), conversation_id)?.ok_or_else(|| {
            format!("local conversation {conversation_id} has no orchestration topology")
        })?;
    let session = replay_session_audit(store.events())?
        .into_iter()
        .find(|record| record.session_id == topology.current_session_id)
        .ok_or_else(|| {
            format!(
                "current session {} for local conversation {conversation_id} is not registered",
                topology.current_session_id.get()
            )
        })?;
    if let Some(binding) = session.worker_binding {
        return Err(format!(
            "current session {} is worker-bound to worker {} and cannot become a controller",
            topology.current_session_id.get(),
            binding.worker_id().get()
        ));
    }

    let supervision = replay_supervision_audit(store.events())?;
    if supervision
        .controllers
        .iter()
        .any(|record| record.designation.session_id() == topology.current_session_id)
    {
        return Err(format!(
            "current session {} is already controller-designated",
            topology.current_session_id.get()
        ));
    }

    record_controller_session_designated(
        store,
        ControllerDesignation::new(topology.current_session_id),
    )
    .map_err(|error| error.to_string())?;

    let replayed = replay_supervision_audit(store.events())?;
    if !replayed
        .controllers
        .iter()
        .any(|record| record.designation.session_id() == topology.current_session_id)
    {
        return Err("controller designation append did not replay".to_owned());
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "controller designation append produced no durable event".to_owned())
}

pub(crate) fn append_local_controller_worker_binding_checked(
    store: &mut impl EventStore,
    controller_conversation_id: LocalConversationId,
    worker_conversation_id: LocalConversationId,
) -> Result<EventEnvelope, String> {
    if controller_conversation_id == worker_conversation_id {
        return Err("a local conversation cannot supervise itself".to_owned());
    }

    let controller_topology = local_conversation_topology(
        store.events(),
        controller_conversation_id,
    )?
    .ok_or_else(|| {
        format!(
            "controller conversation {controller_conversation_id} has no orchestration topology"
        )
    })?;
    let worker_topology = local_conversation_topology(store.events(), worker_conversation_id)?
        .ok_or_else(|| {
            format!("worker conversation {worker_conversation_id} has no orchestration topology")
        })?;
    let worker_binding = replay_local_conversation_worker_bindings(store.events())?
        .into_iter()
        .find(|binding| binding.conversation_id == worker_conversation_id)
        .ok_or_else(|| {
            format!("worker conversation {worker_conversation_id} has no local WorkerId binding")
        })?;

    let sessions = replay_session_audit(store.events())?;
    let active_worker =
        active_worker_session(&sessions, worker_binding.worker_id).ok_or_else(|| {
            format!(
                "worker {} for local conversation {} has no active session binding",
                worker_binding.worker_id.get(),
                worker_conversation_id
            )
        })?;
    if active_worker.session_id != worker_topology.current_session_id {
        return Err(format!(
            "worker {} is active on session {}, but local conversation {} current session is {}; advance the worker session first",
            worker_binding.worker_id.get(),
            active_worker.session_id.get(),
            worker_conversation_id,
            worker_topology.current_session_id.get()
        ));
    }

    let supervision = replay_supervision_audit(store.events())?;
    if !supervision
        .controllers
        .iter()
        .any(|record| record.designation.session_id() == controller_topology.current_session_id)
    {
        return Err(format!(
            "controller conversation {} current session {} is not controller-designated",
            controller_conversation_id,
            controller_topology.current_session_id.get()
        ));
    }
    if let Some(existing) = supervision
        .bindings
        .iter()
        .find(|record| record.binding.worker_session_id() == active_worker.session_id)
    {
        return Err(format!(
            "worker session {} is already supervised by controller session {}",
            active_worker.session_id.get(),
            existing.binding.controller_session_id().get()
        ));
    }

    let binding = ControllerWorkerBinding::new(
        controller_topology.current_session_id,
        active_worker.session_id,
    )
    .map_err(|error| format!("invalid controller-worker binding: {error:?}"))?;
    record_controller_worker_bound(store, binding).map_err(|error| error.to_string())?;

    let replayed = replay_supervision_audit(store.events())?;
    if !replayed
        .bindings
        .iter()
        .any(|record| record.binding == binding)
    {
        return Err("controller-worker binding append did not replay".to_owned());
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "controller-worker binding append produced no durable event".to_owned())
}

pub(crate) fn append_controller_continuation_lease_checked(
    store: &mut impl EventStore,
    controller_conversation_id: LocalConversationId,
    worker_conversation_id: LocalConversationId,
    allowance: u32,
) -> Result<(WorkerId, EventEnvelope), String> {
    if allowance == 0 {
        return Err("continuation lease allowance must be greater than zero".to_owned());
    }
    if controller_conversation_id == worker_conversation_id {
        return Err(
            "a controller conversation cannot authorize continuation for itself".to_owned(),
        );
    }

    let controller_topology = local_conversation_topology(
        store.events(),
        controller_conversation_id,
    )?
    .ok_or_else(|| {
        format!(
            "controller conversation {controller_conversation_id} has no orchestration topology"
        )
    })?;
    let worker_topology = local_conversation_topology(store.events(), worker_conversation_id)?
        .ok_or_else(|| {
            format!("worker conversation {worker_conversation_id} has no orchestration topology")
        })?;
    let worker_binding = replay_local_conversation_worker_bindings(store.events())?
        .into_iter()
        .find(|binding| binding.conversation_id == worker_conversation_id)
        .ok_or_else(|| {
            format!("worker conversation {worker_conversation_id} has no local WorkerId binding")
        })?;

    let sessions = replay_session_audit(store.events())?;
    let active_worker =
        active_worker_session(&sessions, worker_binding.worker_id).ok_or_else(|| {
            format!(
                "worker {} for local conversation {} has no active session binding",
                worker_binding.worker_id.get(),
                worker_conversation_id
            )
        })?;
    if active_worker.session_id != worker_topology.current_session_id {
        return Err(format!(
            "worker {} is active on session {}, but local conversation {} current session is {}; advance the worker session first",
            worker_binding.worker_id.get(),
            active_worker.session_id.get(),
            worker_conversation_id,
            worker_topology.current_session_id.get()
        ));
    }

    let supervision = replay_supervision_audit(store.events())?;
    if !supervision
        .controllers
        .iter()
        .any(|record| record.designation.session_id() == controller_topology.current_session_id)
    {
        return Err(format!(
            "controller conversation {} current session {} is not controller-designated",
            controller_conversation_id,
            controller_topology.current_session_id.get()
        ));
    }
    if !supervision.bindings.iter().any(|record| {
        record.binding.controller_session_id() == controller_topology.current_session_id
            && record.binding.worker_session_id() == active_worker.session_id
    }) {
        return Err(format!(
            "controller session {} does not supervise worker {} active session {}",
            controller_topology.current_session_id.get(),
            worker_binding.worker_id.get(),
            active_worker.session_id.get()
        ));
    }

    let worker = replay_worker_audit(store.events())?
        .into_iter()
        .find(|record| record.worker_id == worker_binding.worker_id)
        .ok_or_else(|| {
            format!(
                "worker {} has no assigned goal",
                worker_binding.worker_id.get()
            )
        })?;
    let goal_id = worker.lifecycle.goal_id().ok_or_else(|| {
        format!(
            "worker {} has no assigned goal",
            worker_binding.worker_id.get()
        )
    })?;
    if worker.lifecycle.phase().is_terminal() {
        return Err(format!(
            "cannot create continuation lease for worker {} terminal phase {}",
            worker_binding.worker_id.get(),
            worker_phase_label(worker.lifecycle.phase())
        ));
    }

    let existing = replay_continuation_audit(store.events())?
        .into_iter()
        .filter(|record| record.worker_id == worker_binding.worker_id && record.goal_id == goal_id)
        .filter(|record| {
            record.remaining > 0
                || record
                    .permits
                    .iter()
                    .any(|permit| permit.consumed_by.is_none())
        })
        .collect::<Vec<_>>();
    if !existing.is_empty() {
        return Err(format!(
            "worker {} goal {} already has live continuation authority",
            worker_binding.worker_id.get(),
            goal_id.get()
        ));
    }

    let lease_id = next_available_continuation_lease_id(store.events())?;
    let lease = ContinuationLease::new(lease_id, worker_binding.worker_id, goal_id, allowance);
    record_continuation_lease_created(store, &lease).map_err(|error| error.to_string())?;

    let replayed = replay_continuation_audit(store.events())?
        .into_iter()
        .find(|record| record.lease_id == lease_id)
        .ok_or_else(|| "continuation lease append did not replay".to_owned())?;
    if replayed.worker_id != worker_binding.worker_id
        || replayed.goal_id != goal_id
        || replayed.allowance != allowance
        || replayed.issued != 0
        || replayed.remaining != allowance
    {
        return Err("continuation lease replay disagrees with requested authority".to_owned());
    }

    let event = store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "continuation lease append produced no durable event".to_owned())?;
    Ok((worker_binding.worker_id, event))
}

pub(crate) fn append_controller_worker_control_proposal_checked(
    store: &mut impl EventStore,
    controller_conversation_id: LocalConversationId,
    worker_conversation_id: LocalConversationId,
    action: ControllerControlAction,
) -> Result<(ControlId, RouteId, Vec<EventEnvelope>), String> {
    if controller_conversation_id == worker_conversation_id {
        return Err("a controller conversation cannot issue a worker control to itself".to_owned());
    }

    let controller_topology = local_conversation_topology(
        store.events(),
        controller_conversation_id,
    )?
    .ok_or_else(|| {
        format!(
            "controller conversation {controller_conversation_id} has no orchestration topology"
        )
    })?;
    let worker_topology = local_conversation_topology(store.events(), worker_conversation_id)?
        .ok_or_else(|| {
            format!("worker conversation {worker_conversation_id} has no orchestration topology")
        })?;
    let worker_binding = replay_local_conversation_worker_bindings(store.events())?
        .into_iter()
        .find(|binding| binding.conversation_id == worker_conversation_id)
        .ok_or_else(|| {
            format!("worker conversation {worker_conversation_id} has no local WorkerId binding")
        })?;

    let sessions = replay_session_audit(store.events())?;
    let active_worker =
        active_worker_session(&sessions, worker_binding.worker_id).ok_or_else(|| {
            format!(
                "worker {} for local conversation {} has no active session binding",
                worker_binding.worker_id.get(),
                worker_conversation_id
            )
        })?;
    if active_worker.session_id != worker_topology.current_session_id {
        return Err(format!(
            "worker {} is active on session {}, but local conversation {} current session is {}; advance the worker session first",
            worker_binding.worker_id.get(),
            active_worker.session_id.get(),
            worker_conversation_id,
            worker_topology.current_session_id.get()
        ));
    }

    let supervision = replay_supervision_audit(store.events())?;
    if !supervision
        .controllers
        .iter()
        .any(|record| record.designation.session_id() == controller_topology.current_session_id)
    {
        return Err(format!(
            "controller conversation {} current session {} is not controller-designated",
            controller_conversation_id,
            controller_topology.current_session_id.get()
        ));
    }
    if !supervision.bindings.iter().any(|record| {
        record.binding.controller_session_id() == controller_topology.current_session_id
            && record.binding.worker_session_id() == active_worker.session_id
    }) {
        return Err(format!(
            "controller session {} does not supervise worker {} active session {}",
            controller_topology.current_session_id.get(),
            worker_binding.worker_id.get(),
            active_worker.session_id.get()
        ));
    }

    let directory = replay_local_routing_directory(store.events())?;
    let controller_endpoint = directory
        .iter()
        .find(|entry| entry.conversation_id == controller_conversation_id)
        .ok_or_else(|| {
            format!(
                "controller conversation {controller_conversation_id} current session is not routing-addressable"
            )
        })?;
    let worker_endpoint = directory
        .iter()
        .find(|entry| entry.conversation_id == worker_conversation_id)
        .ok_or_else(|| {
            format!(
                "worker conversation {worker_conversation_id} current session is not routing-addressable"
            )
        })?;
    if controller_endpoint.current_session_id != controller_topology.current_session_id
        || worker_endpoint.current_session_id != worker_topology.current_session_id
    {
        return Err(
            "local routing directory disagrees with current controller/worker topology".to_owned(),
        );
    }

    let worker = replay_worker_audit(store.events())?
        .into_iter()
        .find(|record| record.worker_id == worker_binding.worker_id)
        .ok_or_else(|| {
            format!(
                "worker {} has no assigned goal",
                worker_binding.worker_id.get()
            )
        })?;
    let goal_id = worker.lifecycle.goal_id().ok_or_else(|| {
        format!(
            "worker {} has no assigned goal",
            worker_binding.worker_id.get()
        )
    })?;

    let control_id = next_available_control_id(store.events())?;
    let route_id = next_available_route_id(store.events())?;
    let before = store.events().len();
    let control = match action {
        ControllerControlAction::StartOrResume => WorkerControl::start_or_resume(
            control_id,
            worker_binding.worker_id,
            goal_id,
            &worker.lifecycle,
        ),
        ControllerControlAction::Continue => {
            let leases = replay_continuation_audit(store.events())?
                .into_iter()
                .filter(|record| {
                    record.worker_id == worker_binding.worker_id && record.goal_id == goal_id
                })
                .collect::<Vec<_>>();
            let unconsumed = leases
                .iter()
                .flat_map(|record| {
                    record
                        .permits
                        .iter()
                        .filter(|permit| permit.consumed_by.is_none())
                        .map(|permit| (record.lease_id, permit.ordinal))
                })
                .collect::<Vec<_>>();

            let permit = match unconsumed.as_slice() {
                [(lease_id, ordinal)] => {
                    reconstruct_unconsumed_continuation_permit(store.events(), *lease_id, *ordinal)?
                }
                [] => {
                    let live = leases
                        .iter()
                        .filter(|record| record.remaining > 0)
                        .map(|record| record.lease_id)
                        .collect::<Vec<_>>();
                    let lease_id = match live.as_slice() {
                        [lease_id] => *lease_id,
                        [] => {
                            return Err(format!(
                                "worker {} goal {} has no live continuation lease",
                                worker_binding.worker_id.get(),
                                goal_id.get()
                            ));
                        }
                        _ => {
                            return Err(format!(
                                "worker {} goal {} has ambiguous live continuation leases",
                                worker_binding.worker_id.get(),
                                goal_id.get()
                            ));
                        }
                    };
                    let mut lease =
                        reconstruct_continuation_lease_for_next_issue(store.events(), lease_id)?;
                    let permit = lease.authorize(&worker.lifecycle).map_err(|error| {
                        format!(
                            "continuation lease {} cannot issue its next permit: {error:?}",
                            lease_id.get()
                        )
                    })?;
                    record_continuation_permit_issued(store, &permit)
                        .map_err(|error| error.to_string())?;
                    permit
                }
                _ => {
                    return Err(format!(
                        "worker {} goal {} has multiple issued unconsumed continuation permits",
                        worker_binding.worker_id.get(),
                        goal_id.get()
                    ));
                }
            };

            WorkerControl::continue_work(
                control_id,
                worker_binding.worker_id,
                &worker.lifecycle,
                permit,
            )
        }
        ControllerControlAction::Stop => WorkerControl::stop(
            control_id,
            worker_binding.worker_id,
            goal_id,
            &worker.lifecycle,
        ),
        ControllerControlAction::StatusRequest => WorkerControl::status_request(
            control_id,
            worker_binding.worker_id,
            goal_id,
            &worker.lifecycle,
        ),
    }
    .map_err(|error| {
        format!(
            "controller {} control is not admissible: {error}",
            action.label()
        )
    })?;

    let request = RouteRequest {
        id: route_id,
        source: controller_endpoint.endpoint_id,
        destination: worker_endpoint.endpoint_id,
        class: RouteClass::OrchestrationControl,
    };
    let route_binding = ControlRouteBinding::new(control_id, &request)
        .map_err(|error| format!("invalid controller control route binding: {error:?}"))?;

    record_worker_control_admitted(store, &control).map_err(|error| error.to_string())?;
    record_worker_control_issuer_bound(
        store,
        ControlProvenance::new(
            control_id,
            ControlIssuer::ControllerSession(controller_topology.current_session_id),
        ),
    )
    .map_err(|error| error.to_string())?;
    record_route_proposed(store, request, RoutePolicy::RequireApproval)
        .map_err(|error| error.to_string())?;
    record_control_route_bound(store, route_binding).map_err(|error| error.to_string())?;

    let validated = replay_validated_orchestration_routes(store.events())?
        .into_iter()
        .find(|record| record.control_id == control_id && record.route.id == route_id)
        .ok_or_else(|| {
            "controller control proposal did not replay as a validated route".to_owned()
        })?;
    if validated.issuer != ControlIssuer::ControllerSession(controller_topology.current_session_id)
        || validated.worker_id != worker_binding.worker_id
        || validated.worker_session_id != Some(active_worker.session_id)
    {
        return Err(
            "validated controller control route disagrees with requested provenance".to_owned(),
        );
    }

    Ok((control_id, route_id, store.events()[before..].to_vec()))
}

pub(crate) fn append_controller_control_route_decision_checked(
    store: &mut impl EventStore,
    route_id: RouteId,
    decision: RouteUserDecision,
) -> Result<EventEnvelope, String> {
    let validated = replay_validated_orchestration_routes(store.events())?
        .into_iter()
        .find(|record| record.route.id == route_id)
        .ok_or_else(|| {
            format!(
                "route {} is not a validated orchestration control",
                route_id.get()
            )
        })?;
    if !matches!(validated.issuer, ControlIssuer::ControllerSession(_)) {
        return Err(format!(
            "route {} is not controller-issued and cannot use the controller control UI",
            route_id.get()
        ));
    }

    let route = replay_routing_audit(store.events())?
        .into_iter()
        .find(|record| record.request.id == route_id)
        .ok_or_else(|| format!("controller control route {} does not exist", route_id.get()))?;
    if route.initial_policy != RoutePolicy::RequireApproval {
        return Err(format!(
            "controller control route {} was not created under explicit-approval policy",
            route_id.get()
        ));
    }
    if route.gate_state.is_dispatched() {
        return Err(format!(
            "controller control route {} has already dispatched",
            route_id.get()
        ));
    }
    if route.latest_user_decision == Some(decision) {
        return Err(format!(
            "controller control route {} already has explicit user decision {}",
            route_id.get(),
            route_user_decision_label(decision)
        ));
    }

    let control = replay_control_audit(store.events())?
        .into_iter()
        .find(|record| record.control_id == validated.control_id)
        .ok_or_else(|| {
            format!(
                "controller control {} disappeared",
                validated.control_id.get()
            )
        })?;
    let next_sequence = store
        .events()
        .last()
        .map_or(1, |event| event.sequence.saturating_add(1));
    validate_control_freshness_before(store.events(), &control, next_sequence)?;

    record_route_user_decision(store, route_id, decision).map_err(|error| error.to_string())?;

    let replayed = replay_routing_audit(store.events())?
        .into_iter()
        .find(|record| record.request.id == route_id)
        .ok_or_else(|| "controller control route decision did not replay".to_owned())?;
    let expected = match decision {
        RouteUserDecision::Allow => RouteGateState::Allowed {
            by: DecisionAuthority::User,
        },
        RouteUserDecision::Deny => RouteGateState::Denied {
            by: DecisionAuthority::User,
        },
    };
    if replayed.gate_state != expected {
        return Err("controller control route gate disagrees with appended decision".to_owned());
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "controller control decision append produced no durable event".to_owned())
}

pub(crate) fn append_controller_control_dispatch_checked(
    store: &mut impl EventStore,
    route_id: RouteId,
) -> Result<Vec<EventEnvelope>, String> {
    if let Some(existing) = replay_worker_control_delivery_audit(store.events())?
        .into_iter()
        .find(|record| record.route_id == route_id)
    {
        return Err(format!(
            "controller control route {} is already delivered at event #{}",
            route_id.get(),
            existing.delivered_sequence
        ));
    }

    let validated = replay_validated_orchestration_routes(store.events())?
        .into_iter()
        .find(|record| record.route.id == route_id)
        .ok_or_else(|| {
            format!(
                "route {} is not a validated orchestration control",
                route_id.get()
            )
        })?;
    let controller_session_id = match validated.issuer {
        ControlIssuer::ControllerSession(session_id) => session_id,
        ControlIssuer::User => {
            return Err(format!(
                "route {} is not controller-issued and cannot use the controller control UI",
                route_id.get()
            ));
        }
    };

    let route = replay_routing_audit(store.events())?
        .into_iter()
        .find(|record| record.request.id == route_id)
        .ok_or_else(|| format!("controller control route {} does not exist", route_id.get()))?;
    let control = replay_control_audit(store.events())?
        .into_iter()
        .find(|record| record.control_id == validated.control_id)
        .ok_or_else(|| {
            format!(
                "controller control {} disappeared",
                validated.control_id.get()
            )
        })?;

    let before = store.events().len();
    let dispatch_sequence = match route.gate_state {
        RouteGateState::Allowed { .. } => {
            let next_sequence = store
                .events()
                .last()
                .map_or(1, |event| event.sequence.saturating_add(1));
            validate_control_freshness_before(store.events(), &control, next_sequence)?;

            let mut gate = route_gate_before_dispatch(&route)?;
            let permit = gate.authorize_dispatch(route_id).map_err(|error| {
                format!(
                    "controller control route {} dispatch gate rejected: {error:?}",
                    route_id.get()
                )
            })?;
            record_route_dispatched(store, permit).map_err(|error| error.to_string())?
        }
        RouteGateState::Dispatched { .. } => route.dispatch_sequence.ok_or_else(|| {
            format!(
                "controller control route {} is marked dispatched without a durable dispatch sequence",
                route_id.get()
            )
        })?,
        RouteGateState::PendingApproval => {
            return Err(format!(
                "controller control route {} is still awaiting explicit user approval",
                route_id.get()
            ));
        }
        RouteGateState::Denied { .. } => {
            return Err(format!(
                "controller control route {} is denied and cannot dispatch",
                route_id.get()
            ));
        }
    };

    let dispatched = replay_validated_control_dispatches(store.events())?
        .into_iter()
        .find(|record| record.route.id == route_id)
        .ok_or_else(|| "controller control dispatch did not replay as validated".to_owned())?;
    if dispatched.control_id != validated.control_id
        || dispatched.dispatch_sequence != dispatch_sequence
        || dispatched.authorized_by != DecisionAuthority::User
    {
        return Err(
            "validated controller control dispatch disagrees with durable dispatch".to_owned(),
        );
    }

    let worker_session_id = dispatched.worker_session_id.ok_or_else(|| {
        format!(
            "controller control {} has no resolved worker session at dispatch",
            dispatched.control_id.get()
        )
    })?;
    if dispatched.controller_session_id != Some(controller_session_id) {
        return Err(
            "validated controller control dispatch controller provenance disagrees".to_owned(),
        );
    }

    let worker_owner = replay_local_conversation_worker_bindings(store.events())?
        .into_iter()
        .find(|binding| binding.worker_id == dispatched.worker_id)
        .ok_or_else(|| {
            format!(
                "controller control {} targets worker {} without local conversation ownership",
                dispatched.control_id.get(),
                dispatched.worker_id.get()
            )
        })?;

    record_worker_control_delivered(
        store,
        dispatched.control_id,
        route_id,
        dispatched.worker_id,
        worker_owner.conversation_id,
        worker_session_id,
        controller_session_id,
        dispatch_sequence,
    )
    .map_err(|error| error.to_string())?;

    let delivery = replay_worker_control_delivery_audit(store.events())?
        .into_iter()
        .find(|record| record.route_id == route_id)
        .ok_or_else(|| "controller control delivery did not replay".to_owned())?;
    if delivery.control_id != dispatched.control_id
        || delivery.worker_id != dispatched.worker_id
        || delivery.worker_conversation_id != worker_owner.conversation_id
        || delivery.worker_session_id != worker_session_id
        || delivery.controller_session_id != controller_session_id
        || delivery.dispatch_sequence != dispatch_sequence
    {
        return Err(
            "worker control delivery replay disagrees with dispatched control provenance"
                .to_owned(),
        );
    }

    Ok(store.events()[before..].to_vec())
}

pub(crate) fn append_controller_worker_result_context_decision_checked(
    store: &mut impl EventStore,
    route_id: RouteId,
    controller_conversation_id: LocalConversationId,
    decision: ControllerWorkerResultContextDecision,
) -> Result<EventEnvelope, String> {
    let result = replay_controller_worker_results_for_conversation(
        store.events(),
        controller_conversation_id,
    )?
    .into_iter()
    .find(|item| item.route_id == route_id)
    .ok_or_else(|| {
        format!(
            "route {} has no terminal worker result owned by controller conversation {}",
            route_id.get(),
            controller_conversation_id,
        )
    })?;

    if let Some(existing) = replay_controller_worker_result_context_audit(store.events())?
        .into_iter()
        .find(|record| record.route_id == route_id)
    {
        if existing.control_id != result.control_id
            || existing.controller_conversation_id != controller_conversation_id
            || existing.result_sequence != result.result_sequence
        {
            return Err(format!(
                "controller result route {} context provenance conflicts with terminal result",
                route_id.get()
            ));
        }
        if existing.decision == decision {
            return Err(format!(
                "controller result route {} already has context decision {}",
                route_id.get(),
                controller_worker_result_context_decision_label(decision),
            ));
        }
    }

    record_controller_worker_result_context_decision(
        store,
        result.control_id,
        route_id,
        controller_conversation_id,
        result.result_sequence,
        decision,
    )
    .map_err(|error| error.to_string())?;

    let replayed = replay_controller_worker_result_context_audit(store.events())?
        .into_iter()
        .find(|record| record.route_id == route_id)
        .ok_or_else(|| "controller result context decision append did not replay".to_owned())?;
    if replayed.control_id != result.control_id
        || replayed.controller_conversation_id != controller_conversation_id
        || replayed.result_sequence != result.result_sequence
        || replayed.decision != decision
    {
        return Err(
            "controller result context decision replay disagrees with appended decision".to_owned(),
        );
    }

    store.events().last().cloned().ok_or_else(|| {
        "controller result context decision append produced no durable event".to_owned()
    })
}

pub(crate) fn append_controller_coordination_start_checked(
    store: &mut impl EventStore,
    controller_conversation_id: LocalConversationId,
) -> Result<(LocalTurnId, EventEnvelope), String> {
    if replay_controller_coordination_audit(store.events())?
        .into_iter()
        .any(|record| {
            record.controller_conversation_id == controller_conversation_id
                && record.result_sequence.is_none()
        })
    {
        return Err(format!(
            "controller conversation {} already has an unfinished coordination turn",
            controller_conversation_id
        ));
    }

    let topology = local_conversation_topology(store.events(), controller_conversation_id)?
        .ok_or_else(|| {
            format!(
                "controller conversation {controller_conversation_id} has no orchestration topology"
            )
        })?;
    let supervision = replay_supervision_audit(store.events())?;
    if !supervision
        .controllers
        .iter()
        .any(|record| record.designation.session_id() == topology.current_session_id)
    {
        return Err(format!(
            "controller conversation {} current session {} is not controller-designated",
            controller_conversation_id,
            topology.current_session_id.get()
        ));
    }

    let mut admitted_routes = replay_admitted_controller_worker_result_context(
        store.events(),
        controller_conversation_id,
    )?
    .into_iter()
    .map(|record| record.route_id)
    .collect::<Vec<_>>();
    admitted_routes.sort_by_key(|route_id| route_id.get());
    if admitted_routes.is_empty() {
        return Err(
            "controller coordination requires at least one admitted terminal worker result"
                .to_owned(),
        );
    }

    let turn_id = LocalTurnId::new();
    record_controller_coordination_started(
        store,
        controller_conversation_id,
        topology.current_session_id,
        turn_id,
        &admitted_routes,
    )
    .map_err(|error| error.to_string())?;

    let replayed = replay_controller_coordination_audit(store.events())?
        .into_iter()
        .find(|record| record.coordination_turn_id == turn_id)
        .ok_or_else(|| "controller coordination start append did not replay".to_owned())?;
    if replayed.controller_conversation_id != controller_conversation_id
        || replayed.controller_session_id != topology.current_session_id
        || replayed.admitted_result_routes != admitted_routes
        || replayed.result_sequence.is_some()
    {
        return Err(
            "controller coordination replay disagrees with durable start provenance".to_owned(),
        );
    }

    let event = store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "controller coordination start produced no durable event".to_owned())?;
    Ok((turn_id, event))
}

pub(crate) fn append_controller_coordination_result_if_terminal(
    store: &mut impl EventStore,
    coordination_turn_id: LocalTurnId,
) -> Result<Option<EventEnvelope>, String> {
    let record = replay_controller_coordination_audit(store.events())?
        .into_iter()
        .find(|record| record.coordination_turn_id == coordination_turn_id);
    let Some(record) = record else {
        return Ok(None);
    };
    if record.result_sequence.is_some() {
        return Ok(None);
    }

    let transport = controller_coordination_transport_state(store.events(), coordination_turn_id)?;
    let Some((outcome, terminal_sequence)) = coordination_terminal_outcome(transport) else {
        return Ok(None);
    };

    record_controller_coordination_result(store, &record, outcome, terminal_sequence)
        .map_err(|error| error.to_string())?;

    let replayed = replay_controller_coordination_audit(store.events())?
        .into_iter()
        .find(|candidate| candidate.coordination_turn_id == coordination_turn_id)
        .ok_or_else(|| "controller coordination terminal result did not replay".to_owned())?;
    if replayed.outcome != Some(outcome)
        || replayed.terminal_sequence != Some(terminal_sequence)
        || replayed.result_sequence != store.events().last().map(|event| event.sequence)
    {
        return Err(
            "controller coordination result replay disagrees with terminal transport evidence"
                .to_owned(),
        );
    }

    Ok(store.events().last().cloned())
}

pub(crate) fn append_controller_coordination_result_context_decision_checked(
    store: &mut impl EventStore,
    coordination_turn_id: LocalTurnId,
    controller_conversation_id: LocalConversationId,
    decision: ControllerCoordinationResultContextDecision,
) -> Result<EventEnvelope, String> {
    let coordination = replay_controller_coordination_audit(store.events())?
        .into_iter()
        .find(|record| record.coordination_turn_id == coordination_turn_id)
        .ok_or_else(|| {
            format!(
                "controller coordination turn {} does not exist",
                coordination_turn_id
            )
        })?;
    if coordination.controller_conversation_id != controller_conversation_id {
        return Err(format!(
            "controller coordination turn {} belongs to conversation {}, not {}",
            coordination_turn_id,
            coordination.controller_conversation_id,
            controller_conversation_id,
        ));
    }
    let result_sequence = coordination.result_sequence.ok_or_else(|| {
        format!(
            "controller coordination turn {} has no terminal result to admit",
            coordination_turn_id
        )
    })?;

    if let Some(existing) = replay_controller_coordination_result_context_audit(store.events())?
        .into_iter()
        .find(|record| record.coordination_turn_id == coordination_turn_id)
    {
        if existing.controller_conversation_id != controller_conversation_id
            || existing.result_sequence != result_sequence
        {
            return Err(format!(
                "controller coordination turn {} result context provenance conflicts with terminal result",
                coordination_turn_id
            ));
        }
        if existing.decision == decision {
            return Err(format!(
                "controller coordination turn {} already has result context decision {}",
                coordination_turn_id,
                controller_coordination_result_context_decision_label(decision),
            ));
        }
    }

    record_controller_coordination_result_context_decision(
        store,
        controller_conversation_id,
        coordination_turn_id,
        result_sequence,
        decision,
    )
    .map_err(|error| error.to_string())?;

    let replayed = replay_controller_coordination_result_context_audit(store.events())?
        .into_iter()
        .find(|record| record.coordination_turn_id == coordination_turn_id)
        .ok_or_else(|| {
            "controller coordination result context decision append did not replay".to_owned()
        })?;
    if replayed.controller_conversation_id != controller_conversation_id
        || replayed.result_sequence != result_sequence
        || replayed.decision != decision
    {
        return Err(
            "controller coordination result context replay disagrees with appended decision"
                .to_owned(),
        );
    }

    store.events().last().cloned().ok_or_else(|| {
        "controller coordination result context decision append produced no durable event"
            .to_owned()
    })
}

pub(crate) fn append_controller_coordination_suggestion_checked(
    store: &mut impl EventStore,
    controller_conversation_id: LocalConversationId,
    coordination_turn_id: LocalTurnId,
    basis_result_route_id: RouteId,
    action: CoordinationSuggestionAction,
) -> Result<(CoordinationSuggestionId, EventEnvelope), String> {
    let coordination = replay_controller_coordination_audit(store.events())?
        .into_iter()
        .find(|record| record.coordination_turn_id == coordination_turn_id)
        .ok_or_else(|| {
            format!(
                "controller coordination turn {} does not exist",
                coordination_turn_id
            )
        })?;
    if coordination.controller_conversation_id != controller_conversation_id {
        return Err(format!(
            "controller coordination turn {} belongs to conversation {}, not {}",
            coordination_turn_id,
            coordination.controller_conversation_id,
            controller_conversation_id,
        ));
    }
    if coordination.outcome != Some(ControllerCoordinationOutcome::Completed) {
        return Err(format!(
            "controller coordination turn {} is not completed and cannot produce suggestions",
            coordination_turn_id
        ));
    }
    let result_sequence = coordination.result_sequence.ok_or_else(|| {
        format!(
            "controller coordination turn {} has no terminal result",
            coordination_turn_id
        )
    })?;
    if !coordination
        .admitted_result_routes
        .contains(&basis_result_route_id)
    {
        return Err(format!(
            "worker result route {} was not frozen into coordination turn {}",
            basis_result_route_id.get(),
            coordination_turn_id
        ));
    }

    let basis = replay_controller_worker_results_for_conversation(
        store.events(),
        controller_conversation_id,
    )?
    .into_iter()
    .find(|item| item.route_id == basis_result_route_id)
    .ok_or_else(|| {
        format!(
            "worker result route {} is not visible to controller conversation {}",
            basis_result_route_id.get(),
            controller_conversation_id
        )
    })?;

    if replay_controller_coordination_suggestion_audit(store.events())?
        .into_iter()
        .any(|record| {
            record.coordination_turn_id == coordination_turn_id
                && record.basis_result_route_id == basis_result_route_id
                && record.suggestion.action() == action
        })
    {
        return Err(format!(
            "coordination turn {} already has {} suggestion for basis route {}",
            coordination_turn_id,
            coordination_suggestion_action_label(action),
            basis_result_route_id.get()
        ));
    }

    let suggestion_id = next_available_coordination_suggestion_id(store.events())?;
    let record = ControllerCoordinationSuggestionRecord {
        suggestion: CoordinationSuggestion::new(
            suggestion_id,
            basis.worker_id,
            basis.goal_id,
            action,
        ),
        controller_conversation_id,
        coordination_turn_id,
        coordination_result_sequence: result_sequence,
        basis_result_route_id,
        worker_conversation_id: basis.worker_conversation_id,
        recorded_sequence: 0,
        promoted_control_id: None,
        promoted_route_id: None,
        promoted_sequence: None,
    };
    record_controller_coordination_suggestion(store, &record).map_err(|error| error.to_string())?;

    let replayed = replay_controller_coordination_suggestion_audit(store.events())?
        .into_iter()
        .find(|candidate| candidate.suggestion.id() == suggestion_id)
        .ok_or_else(|| "coordination suggestion append did not replay".to_owned())?;
    if replayed.controller_conversation_id != controller_conversation_id
        || replayed.coordination_turn_id != coordination_turn_id
        || replayed.coordination_result_sequence != result_sequence
        || replayed.basis_result_route_id != basis_result_route_id
        || replayed.worker_conversation_id != basis.worker_conversation_id
        || replayed.suggestion.worker_id() != basis.worker_id
        || replayed.suggestion.goal_id() != basis.goal_id
        || replayed.suggestion.action() != action
    {
        return Err(
            "coordination suggestion replay disagrees with authoritative result provenance"
                .to_owned(),
        );
    }

    let event = store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "coordination suggestion append produced no durable event".to_owned())?;
    Ok((suggestion_id, event))
}

pub(crate) fn append_controller_coordination_suggestion_promotion_checked(
    store: &mut impl EventStore,
    controller_conversation_id: LocalConversationId,
    suggestion_id: CoordinationSuggestionId,
) -> Result<(ControlId, RouteId, Vec<EventEnvelope>), String> {
    let suggestion = replay_controller_coordination_suggestion_audit(store.events())?
        .into_iter()
        .find(|record| record.suggestion.id() == suggestion_id)
        .ok_or_else(|| {
            format!(
                "coordination suggestion {} does not exist",
                suggestion_id.get()
            )
        })?;
    if suggestion.controller_conversation_id != controller_conversation_id {
        return Err(format!(
            "coordination suggestion {} belongs to controller conversation {}, not {}",
            suggestion_id.get(),
            suggestion.controller_conversation_id,
            controller_conversation_id,
        ));
    }
    if suggestion.promoted_sequence.is_some() {
        return Err(format!(
            "coordination suggestion {} is already promoted to control {} route {}",
            suggestion_id.get(),
            suggestion.promoted_control_id.map_or(0, ControlId::get),
            suggestion.promoted_route_id.map_or(0, RouteId::get),
        ));
    }

    let worker_owner = replay_local_conversation_worker_bindings(store.events())?
        .into_iter()
        .find(|binding| binding.conversation_id == suggestion.worker_conversation_id)
        .ok_or_else(|| {
            format!(
                "suggestion {} target conversation {} no longer has a WorkerId binding",
                suggestion_id.get(),
                suggestion.worker_conversation_id
            )
        })?;
    if worker_owner.worker_id != suggestion.suggestion.worker_id() {
        return Err(format!(
            "suggestion {} target conversation now owns worker {}, expected worker {}",
            suggestion_id.get(),
            worker_owner.worker_id.get(),
            suggestion.suggestion.worker_id().get()
        ));
    }

    let worker = replay_worker_audit(store.events())?
        .into_iter()
        .find(|record| record.worker_id == suggestion.suggestion.worker_id())
        .ok_or_else(|| {
            format!(
                "suggestion {} target worker {} has no lifecycle state",
                suggestion_id.get(),
                suggestion.suggestion.worker_id().get()
            )
        })?;
    let current_goal = worker.lifecycle.goal_id().ok_or_else(|| {
        format!(
            "suggestion {} target worker {} no longer has an assigned goal",
            suggestion_id.get(),
            suggestion.suggestion.worker_id().get()
        )
    })?;
    if current_goal != suggestion.suggestion.goal_id() {
        return Err(format!(
            "coordination suggestion {} is stale: worker {} current goal is {}, suggestion observed goal {}",
            suggestion_id.get(),
            suggestion.suggestion.worker_id().get(),
            current_goal.get(),
            suggestion.suggestion.goal_id().get()
        ));
    }

    let action = controller_control_action_from_suggestion(suggestion.suggestion.action());
    let before = store.events().len();
    let (control_id, route_id, _) = append_controller_worker_control_proposal_checked(
        store,
        controller_conversation_id,
        suggestion.worker_conversation_id,
        action,
    )?;

    let control = replay_control_audit(store.events())?
        .into_iter()
        .find(|record| record.control_id == control_id)
        .ok_or_else(|| "promoted suggestion control disappeared after proposal".to_owned())?;
    if control.worker_id != suggestion.suggestion.worker_id()
        || control.goal_id != suggestion.suggestion.goal_id()
        || !coordination_suggestion_matches_control_kind(
            suggestion.suggestion.action(),
            control.kind,
        )
    {
        return Err(
            "promoted control does not match coordination suggestion provenance".to_owned(),
        );
    }

    record_controller_coordination_suggestion_promoted(
        store,
        controller_conversation_id,
        suggestion_id,
        control_id,
        route_id,
    )
    .map_err(|error| error.to_string())?;

    let replayed = replay_controller_coordination_suggestion_audit(store.events())?
        .into_iter()
        .find(|record| record.suggestion.id() == suggestion_id)
        .ok_or_else(|| "promoted coordination suggestion did not replay".to_owned())?;
    if replayed.promoted_control_id != Some(control_id)
        || replayed.promoted_route_id != Some(route_id)
        || replayed.promoted_sequence != store.events().last().map(|event| event.sequence)
    {
        return Err(
            "coordination suggestion promotion replay disagrees with control proposal".to_owned(),
        );
    }

    Ok((control_id, route_id, store.events()[before..].to_vec()))
}

pub(crate) fn controller_control_action_from_suggestion(
    action: CoordinationSuggestionAction,
) -> ControllerControlAction {
    match action {
        CoordinationSuggestionAction::StartOrResume => ControllerControlAction::StartOrResume,
        CoordinationSuggestionAction::Continue => ControllerControlAction::Continue,
        CoordinationSuggestionAction::Stop => ControllerControlAction::Stop,
        CoordinationSuggestionAction::StatusRequest => ControllerControlAction::StatusRequest,
    }
}

pub(crate) fn coordination_suggestion_matches_control_kind(
    action: CoordinationSuggestionAction,
    kind: WorkerControlKind,
) -> bool {
    matches!(
        (action, kind),
        (
            CoordinationSuggestionAction::StartOrResume,
            WorkerControlKind::StartOrResume
        ) | (
            CoordinationSuggestionAction::Continue,
            WorkerControlKind::Continue { .. }
        ) | (CoordinationSuggestionAction::Stop, WorkerControlKind::Stop)
            | (
                CoordinationSuggestionAction::StatusRequest,
                WorkerControlKind::StatusRequest
            )
    )
}

pub(crate) fn append_worker_control_acknowledgement_checked(
    store: &mut impl EventStore,
    worker_conversation_id: LocalConversationId,
    route_id: RouteId,
) -> Result<EventEnvelope, String> {
    let item =
        replay_worker_control_inbox_for_conversation(store.events(), worker_conversation_id)?
            .into_iter()
            .find(|item| item.route_id == route_id)
            .ok_or_else(|| {
                format!(
                    "worker control route {} is not a delivered inbox item for conversation {}",
                    route_id.get(),
                    worker_conversation_id,
                )
            })?;

    if item.acknowledged_sequence.is_some() {
        return Err(format!(
            "worker control route {} is already acknowledged",
            route_id.get()
        ));
    }

    let owner = replay_local_conversation_worker_bindings(store.events())?
        .into_iter()
        .find(|binding| binding.conversation_id == worker_conversation_id)
        .ok_or_else(|| {
            format!(
                "worker conversation {} has no durable WorkerId binding",
                worker_conversation_id
            )
        })?;
    if owner.worker_id != item.worker_id {
        return Err(format!(
            "worker control route {} inbox worker disagrees with durable conversation ownership",
            route_id.get()
        ));
    }

    record_worker_control_acknowledged(
        store,
        item.control_id,
        item.route_id,
        item.worker_id,
        worker_conversation_id,
        item.delivered_sequence,
    )
    .map_err(|error| error.to_string())?;

    let replayed = replay_worker_control_acknowledgement_audit(store.events())?
        .into_iter()
        .find(|record| record.route_id == route_id)
        .ok_or_else(|| "worker control acknowledgement append did not replay".to_owned())?;
    if replayed.control_id != item.control_id
        || replayed.worker_id != item.worker_id
        || replayed.worker_conversation_id != worker_conversation_id
        || replayed.delivered_sequence != item.delivered_sequence
    {
        return Err(
            "worker control acknowledgement replay disagrees with delivered inbox item".to_owned(),
        );
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "worker control acknowledgement append produced no durable event".to_owned())
}

pub(crate) fn append_worker_control_status_result_checked(
    store: &mut impl EventStore,
    worker_conversation_id: LocalConversationId,
    route_id: RouteId,
) -> Result<EventEnvelope, String> {
    let item =
        replay_worker_control_inbox_for_conversation(store.events(), worker_conversation_id)?
            .into_iter()
            .find(|item| item.route_id == route_id)
            .ok_or_else(|| {
                format!(
                    "worker control route {} is not a delivered inbox item for conversation {}",
                    route_id.get(),
                    worker_conversation_id,
                )
            })?;

    if item.kind != WorkerControlKind::StatusRequest {
        return Err(format!(
            "worker control route {} is {}, not a StatusRequest",
            route_id.get(),
            worker_control_kind_label(item.kind),
        ));
    }

    let acknowledged_sequence = item.acknowledged_sequence.ok_or_else(|| {
        format!(
            "worker control route {} must be acknowledged before recording a status result",
            route_id.get()
        )
    })?;

    if replay_worker_control_status_results(store.events())?
        .into_iter()
        .any(|record| record.route_id == route_id || record.control_id == item.control_id)
    {
        return Err(format!(
            "worker control route {} already has a durable status result",
            route_id.get()
        ));
    }

    let owner = replay_local_conversation_worker_bindings(store.events())?
        .into_iter()
        .find(|binding| binding.conversation_id == worker_conversation_id)
        .ok_or_else(|| {
            format!(
                "worker conversation {} has no durable WorkerId binding",
                worker_conversation_id
            )
        })?;
    if owner.worker_id != item.worker_id {
        return Err(format!(
            "worker control route {} inbox worker disagrees with durable conversation ownership",
            route_id.get()
        ));
    }

    let worker = replay_worker_audit(store.events())?
        .into_iter()
        .find(|record| record.worker_id == item.worker_id)
        .ok_or_else(|| {
            format!(
                "worker control route {} references worker {} without lifecycle state",
                route_id.get(),
                item.worker_id.get()
            )
        })?;
    let goal_id = worker.lifecycle.goal_id().ok_or_else(|| {
        format!(
            "worker {} has no active goal to report for status route {}",
            item.worker_id.get(),
            route_id.get()
        )
    })?;
    if goal_id != item.goal_id {
        return Err(format!(
            "worker control route {} targets goal {}, but worker {} current goal is {}",
            route_id.get(),
            item.goal_id.get(),
            item.worker_id.get(),
            goal_id.get()
        ));
    }
    let phase = worker.lifecycle.phase();

    record_worker_control_status_result(
        store,
        item.control_id,
        item.route_id,
        item.worker_id,
        worker_conversation_id,
        goal_id,
        phase,
        acknowledged_sequence,
    )
    .map_err(|error| error.to_string())?;

    let replayed = replay_worker_control_status_results(store.events())?
        .into_iter()
        .find(|record| record.route_id == route_id)
        .ok_or_else(|| "worker control status result append did not replay".to_owned())?;
    if replayed.control_id != item.control_id
        || replayed.worker_id != item.worker_id
        || replayed.worker_conversation_id != worker_conversation_id
        || replayed.goal_id != goal_id
        || replayed.phase != phase
        || replayed.acknowledged_sequence != acknowledged_sequence
    {
        return Err(
            "worker control status result replay disagrees with acknowledged inbox item".to_owned(),
        );
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "worker control status result append produced no durable event".to_owned())
}

pub(crate) fn append_worker_control_action_checked(
    store: &mut impl EventStore,
    worker_conversation_id: LocalConversationId,
    route_id: RouteId,
) -> Result<Vec<EventEnvelope>, String> {
    let before = store.events().len();
    let item =
        replay_worker_control_inbox_for_conversation(store.events(), worker_conversation_id)?
            .into_iter()
            .find(|item| item.route_id == route_id)
            .ok_or_else(|| {
                format!(
                    "worker control route {} is not a delivered inbox item for conversation {}",
                    route_id.get(),
                    worker_conversation_id,
                )
            })?;

    let acknowledged_sequence = item.acknowledged_sequence.ok_or_else(|| {
        format!(
            "worker control route {} must be acknowledged before applying the control",
            route_id.get()
        )
    })?;
    let action = action_for_kind(item.kind)?;

    let owner = replay_local_conversation_worker_bindings(store.events())?
        .into_iter()
        .find(|binding| binding.conversation_id == worker_conversation_id)
        .ok_or_else(|| {
            format!(
                "worker conversation {} has no durable WorkerId binding",
                worker_conversation_id
            )
        })?;
    if owner.worker_id != item.worker_id {
        return Err(format!(
            "worker control route {} inbox worker disagrees with durable conversation ownership",
            route_id.get()
        ));
    }

    let mut action_record = replay_worker_control_action_audit(store.events())?
        .into_iter()
        .find(|record| record.route_id == route_id);

    if let Some(existing) = action_record {
        if existing.control_id != item.control_id
            || existing.worker_id != item.worker_id
            || existing.worker_conversation_id != worker_conversation_id
            || existing.goal_id != item.goal_id
            || existing.kind != item.kind
            || existing.acknowledged_sequence != acknowledged_sequence
        {
            return Err(format!(
                "worker control route {} action provenance disagrees with delivered inbox item",
                route_id.get()
            ));
        }
        if existing.is_complete() {
            return Err(format!(
                "worker control route {} already has a complete action result",
                route_id.get()
            ));
        }
    } else {
        let worker = replay_worker_audit(store.events())?
            .into_iter()
            .find(|record| record.worker_id == item.worker_id)
            .ok_or_else(|| {
                format!(
                    "worker control route {} references worker {} without lifecycle state",
                    route_id.get(),
                    item.worker_id.get()
                )
            })?;
        let from_phase = validate_control_admission(item.goal_id, item.kind, &worker.lifecycle)
            .map_err(|error| {
                format!(
                    "worker control route {} is stale before action start: {error}",
                    route_id.get()
                )
            })?;

        record_worker_control_action_started(
            store,
            item.control_id,
            item.route_id,
            item.worker_id,
            worker_conversation_id,
            item.goal_id,
            item.kind,
            acknowledged_sequence,
            from_phase,
        )
        .map_err(|error| error.to_string())?;

        action_record = replay_worker_control_action_audit(store.events())?
            .into_iter()
            .find(|record| record.route_id == route_id);
    }

    let mut action_record =
        action_record.ok_or_else(|| "worker control action start did not replay".to_owned())?;

    if action_record.lifecycle_sequence.is_none() {
        let worker = replay_worker_audit(store.events())?
            .into_iter()
            .find(|record| record.worker_id == item.worker_id)
            .ok_or_else(|| {
                format!(
                    "worker control route {} lost worker {} lifecycle state",
                    route_id.get(),
                    item.worker_id.get()
                )
            })?;

        if worker.last_sequence > action_record.started_sequence {
            return Err(format!(
                "worker {} lifecycle changed at event #{} after control route {} action started at event #{} without matching control correlation",
                item.worker_id.get(),
                worker.last_sequence,
                route_id.get(),
                action_record.started_sequence,
            ));
        }

        let replayed_from = validate_control_admission(item.goal_id, item.kind, &worker.lifecycle)
            .map_err(|error| {
                format!(
                    "worker control route {} became stale before lifecycle mutation: {error}",
                    route_id.get()
                )
            })?;
        if replayed_from != action_record.from_phase {
            return Err(format!(
                "worker control route {} source phase changed after action start",
                route_id.get()
            ));
        }

        let mut lifecycle = worker.lifecycle;
        match action {
            WorkerAction::StartOrResume => {
                lifecycle
                    .start_or_resume(item.goal_id)
                    .map_err(|error| error.to_string())?;
            }
            WorkerAction::Stop => {
                lifecycle
                    .stop(item.goal_id)
                    .map_err(|error| error.to_string())?;
            }
            _ => {
                return Err(format!(
                    "worker control route {} action {:?} is not implemented by the mutating control path",
                    route_id.get(),
                    action
                ));
            }
        }

        record_worker_control_transition(
            store,
            item.worker_id,
            item.goal_id,
            action,
            WorkerControlTransitionProvenance {
                control_id: item.control_id,
                route_id: item.route_id,
                action_started_sequence: action_record.started_sequence,
            },
        )
        .map_err(|error| error.to_string())?;

        action_record = replay_worker_control_action_audit(store.events())?
            .into_iter()
            .find(|record| record.route_id == route_id)
            .ok_or_else(|| "worker control lifecycle transition did not replay".to_owned())?;

        if action_record.resulting_phase != Some(lifecycle.phase()) {
            return Err(format!(
                "worker control route {} replayed result phase disagrees with core transition",
                route_id.get()
            ));
        }
    }

    if action_record.result_sequence.is_none() {
        let lifecycle_sequence = action_record.lifecycle_sequence.ok_or_else(|| {
            format!(
                "worker control route {} has no correlated lifecycle transition",
                route_id.get()
            )
        })?;
        let resulting_phase = action_record.resulting_phase.ok_or_else(|| {
            format!(
                "worker control route {} has no correlated resulting phase",
                route_id.get()
            )
        })?;

        record_worker_control_action_result(
            store,
            action_record,
            lifecycle_sequence,
            resulting_phase,
        )
        .map_err(|error| error.to_string())?;

        let replayed = replay_worker_control_action_audit(store.events())?
            .into_iter()
            .find(|record| record.route_id == route_id)
            .ok_or_else(|| "worker control action result did not replay".to_owned())?;
        if !replayed.is_complete()
            || replayed.lifecycle_sequence != Some(lifecycle_sequence)
            || replayed.resulting_phase != Some(resulting_phase)
        {
            return Err(
                "worker control action result replay disagrees with correlated lifecycle transition"
                    .to_owned(),
            );
        }
    }

    Ok(store.events()[before..].to_vec())
}

pub(crate) fn append_worker_continuation_execution_start_checked(
    store: &mut impl EventStore,
    worker_conversation_id: LocalConversationId,
    route_id: RouteId,
) -> Result<EventEnvelope, String> {
    let item =
        replay_worker_control_inbox_for_conversation(store.events(), worker_conversation_id)?
            .into_iter()
            .find(|item| item.route_id == route_id)
            .ok_or_else(|| {
                format!(
                    "worker control route {} is not a delivered inbox item for conversation {}",
                    route_id.get(),
                    worker_conversation_id,
                )
            })?;

    let WorkerControlKind::Continue { permit_ordinal } = item.kind else {
        return Err(format!(
            "worker control route {} is {}, not CONTINUE",
            route_id.get(),
            worker_control_kind_label(item.kind),
        ));
    };
    let acknowledged_sequence = item.acknowledged_sequence.ok_or_else(|| {
        format!(
            "worker control route {} must be acknowledged before continuation execution starts",
            route_id.get()
        )
    })?;

    if replay_worker_continuation_execution_audit(store.events())?
        .into_iter()
        .any(|record| record.route_id == route_id || record.control_id == item.control_id)
    {
        return Err(format!(
            "worker control route {} already has a continuation execution start",
            route_id.get()
        ));
    }

    let owner = replay_local_conversation_worker_bindings(store.events())?
        .into_iter()
        .find(|binding| binding.conversation_id == worker_conversation_id)
        .ok_or_else(|| {
            format!(
                "worker conversation {} has no durable WorkerId binding",
                worker_conversation_id
            )
        })?;
    if owner.worker_id != item.worker_id {
        return Err(format!(
            "worker control route {} inbox worker disagrees with durable conversation ownership",
            route_id.get()
        ));
    }

    let control = replay_control_audit(store.events())?
        .into_iter()
        .find(|record| record.control_id == item.control_id)
        .ok_or_else(|| {
            format!(
                "worker control route {} references missing admitted control {}",
                route_id.get(),
                item.control_id.get()
            )
        })?;
    let permit = control.continuation_permit.ok_or_else(|| {
        format!(
            "worker Continue control {} has no durable continuation permit provenance",
            item.control_id.get()
        )
    })?;
    if permit.ordinal != permit_ordinal {
        return Err(format!(
            "worker Continue control {} permit ordinal disagrees with inbox",
            item.control_id.get()
        ));
    }

    let worker = replay_worker_audit(store.events())?
        .into_iter()
        .find(|record| record.worker_id == item.worker_id)
        .ok_or_else(|| {
            format!(
                "worker control route {} references worker {} without lifecycle state",
                route_id.get(),
                item.worker_id.get()
            )
        })?;
    if worker.lifecycle.goal_id() != Some(item.goal_id) {
        return Err(format!(
            "worker control route {} targets stale goal {}",
            route_id.get(),
            item.goal_id.get()
        ));
    }
    if !worker.lifecycle.phase().allows_continuation() {
        return Err(format!(
            "worker control route {} cannot continue while worker {} is {}",
            route_id.get(),
            item.worker_id.get(),
            worker_phase_label(worker.lifecycle.phase()),
        ));
    }

    let execution_turn_id = LocalTurnId::new();
    record_worker_continuation_execution_started(
        store,
        item.control_id,
        item.route_id,
        item.worker_id,
        worker_conversation_id,
        item.goal_id,
        permit.lease_id,
        permit.ordinal,
        acknowledged_sequence,
        execution_turn_id,
    )
    .map_err(|error| error.to_string())?;

    let replayed = replay_worker_continuation_execution_audit(store.events())?
        .into_iter()
        .find(|record| record.route_id == route_id)
        .ok_or_else(|| "worker continuation execution start did not replay".to_owned())?;
    if replayed.control_id != item.control_id
        || replayed.worker_id != item.worker_id
        || replayed.worker_conversation_id != worker_conversation_id
        || replayed.goal_id != item.goal_id
        || replayed.lease_id != permit.lease_id
        || replayed.permit_ordinal != permit.ordinal
        || replayed.acknowledged_sequence != acknowledged_sequence
        || replayed.execution_turn_id != execution_turn_id
    {
        return Err(
            "worker continuation execution replay disagrees with acknowledged Continue provenance"
                .to_owned(),
        );
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "worker continuation execution append produced no durable event".to_owned())
}

pub(crate) fn append_worker_continuation_result_if_terminal(
    store: &mut impl EventStore,
    execution_turn_id: LocalTurnId,
) -> Result<Option<(RouteId, EventEnvelope)>, String> {
    let execution = replay_worker_continuation_execution_audit(store.events())?
        .into_iter()
        .find(|record| record.execution_turn_id == execution_turn_id);
    let Some(execution) = execution else {
        return Ok(None);
    };
    if execution.result_sequence.is_some() {
        return Ok(None);
    }

    let transport =
        continuation_execution_transport_state(store.events(), execution.execution_turn_id)?;
    let Some((outcome, terminal_sequence)) = transport_terminal_outcome(transport) else {
        return Ok(None);
    };

    record_worker_continuation_execution_result(store, execution, outcome, terminal_sequence)
        .map_err(|error| error.to_string())?;

    let replayed = replay_worker_continuation_execution_audit(store.events())?
        .into_iter()
        .find(|record| record.route_id == execution.route_id)
        .ok_or_else(|| "worker continuation execution result did not replay".to_owned())?;
    if replayed.outcome != Some(outcome)
        || replayed.terminal_sequence != Some(terminal_sequence)
        || replayed.result_sequence != store.events().last().map(|event| event.sequence)
    {
        return Err(
            "worker continuation execution result replay disagrees with terminal transport evidence"
                .to_owned(),
        );
    }

    let event =
        store.events().last().cloned().ok_or_else(|| {
            "worker continuation result append produced no durable event".to_owned()
        })?;
    Ok(Some((execution.route_id, event)))
}

pub(crate) fn append_worker_goal_checked(
    store: &mut impl EventStore,
    worker_id: WorkerId,
    goal_id: WorkerGoalId,
) -> Result<EventEnvelope, String> {
    require_local_worker_binding(store.events(), worker_id)?;
    let mut lifecycle = replay_worker_audit(store.events())?
        .into_iter()
        .find(|record| record.worker_id == worker_id)
        .map_or_else(WorkerLifecycle::default, |record| record.lifecycle);
    lifecycle
        .assign_goal(goal_id)
        .map_err(|error| error.to_string())?;

    record_worker_goal_assigned(store, worker_id, goal_id).map_err(|error| error.to_string())?;
    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "worker goal append produced no durable event".to_owned())
}

pub(crate) fn append_worker_transition_checked(
    store: &mut impl EventStore,
    worker_id: WorkerId,
    goal_id: WorkerGoalId,
    action: WorkerAction,
) -> Result<EventEnvelope, String> {
    require_local_worker_binding(store.events(), worker_id)?;
    let mut lifecycle = replay_worker_audit(store.events())?
        .into_iter()
        .find(|record| record.worker_id == worker_id)
        .ok_or_else(|| format!("worker {} has no assigned goal", worker_id.get()))?
        .lifecycle;
    apply_worker_action(&mut lifecycle, goal_id, action)?;

    record_worker_transition(store, worker_id, goal_id, action)
        .map_err(|error| error.to_string())?;
    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "worker transition append produced no durable event".to_owned())
}

pub(crate) fn require_local_worker_binding(
    events: &[EventEnvelope],
    worker_id: WorkerId,
) -> Result<LocalConversationWorkerBindingRecord, String> {
    replay_local_conversation_worker_bindings(events)?
        .into_iter()
        .find(|binding| binding.worker_id == worker_id)
        .ok_or_else(|| {
            format!(
                "worker {} is not bound to a local Chatarium conversation",
                worker_id.get()
            )
        })
}

pub(crate) fn apply_worker_action(
    lifecycle: &mut WorkerLifecycle,
    goal_id: WorkerGoalId,
    action: WorkerAction,
) -> Result<(), String> {
    match action {
        WorkerAction::AssignGoal => {
            Err("AssignGoal must use the dedicated durable goal-assignment command".to_owned())
        }
        WorkerAction::StartOrResume => lifecycle
            .start_or_resume(goal_id)
            .map_err(|error| error.to_string()),
        WorkerAction::ReportProgress => lifecycle
            .report_progress(goal_id)
            .map_err(|error| error.to_string()),
        WorkerAction::RequestInput => lifecycle
            .request_input(goal_id)
            .map_err(|error| error.to_string()),
        WorkerAction::MarkBlocked => lifecycle
            .mark_blocked(goal_id)
            .map_err(|error| error.to_string()),
        WorkerAction::Complete => lifecycle
            .complete(goal_id)
            .map(|_| ())
            .map_err(|error| error.to_string()),
        WorkerAction::Fail => lifecycle
            .fail(goal_id)
            .map(|_| ())
            .map_err(|error| error.to_string()),
        WorkerAction::Stop => lifecycle
            .stop(goal_id)
            .map(|_| ())
            .map_err(|error| error.to_string()),
    }
}
