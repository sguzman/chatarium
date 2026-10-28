//! Desktop workers actions extracted intact from ChatariumApp.
use super::super::super::*;

impl ChatariumApp {
    pub(crate) fn enable_worker_lifecycle(&mut self) {
        if self.lifecycle_command_pending {
            return;
        }
        let worker_id = match next_available_worker_id(&self.events) {
            Ok(worker_id) => worker_id,
            Err(error) => {
                self.status = format!("cannot allocate worker identity: {error}");
                return;
            }
        };
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot enable worker lifecycle: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::BindLocalConversationWorker {
            conversation_id: self.local_conversation_id,
            worker_id,
        }) {
            Ok(()) => {
                self.lifecycle_command_pending = true;
                self.status = format!("binding local conversation to worker {}…", worker_id.get());
            }
            Err(error) => {
                self.status = format!("failed to queue worker binding: {error}");
            }
        }
    }

    pub(crate) fn bind_worker_to_current_session(&mut self, worker_id: WorkerId) {
        if self.supervision_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot bind worker session: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::BindCurrentSessionWorker {
            conversation_id: self.local_conversation_id,
            worker_id,
        }) {
            Ok(()) => {
                self.supervision_command_pending = true;
                self.status = format!(
                    "binding worker {} to this conversation's current session…",
                    worker_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue worker-session binding: {error}");
            }
        }
    }

    pub(crate) fn designate_current_session_controller(&mut self) {
        if self.supervision_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot designate controller: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::DesignateCurrentSessionController {
            conversation_id: self.local_conversation_id,
        }) {
            Ok(()) => {
                self.supervision_command_pending = true;
                self.status = "designating current session as controller…".to_owned();
            }
            Err(error) => {
                self.status = format!("failed to queue controller designation: {error}");
            }
        }
    }

    pub(crate) fn supervise_local_conversation(&mut self, worker_conversation_id: LocalConversationId) {
        if self.supervision_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot bind controller supervision: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::BindControllerToLocalWorker {
            controller_conversation_id: self.local_conversation_id,
            worker_conversation_id,
        }) {
            Ok(()) => {
                self.supervision_command_pending = true;
                self.status = "binding current controller to local worker session…".to_owned();
            }
            Err(error) => {
                self.status = format!("failed to queue controller-worker binding: {error}");
            }
        }
    }

    pub(crate) fn create_controller_continuation_lease(
        &mut self,
        worker_conversation_id: LocalConversationId,
        allowance: u32,
    ) {
        if self.controller_control_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot create continuation lease: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::CreateControllerContinuationLease {
            controller_conversation_id: self.local_conversation_id,
            worker_conversation_id,
            allowance,
        }) {
            Ok(()) => {
                self.controller_control_command_pending = true;
                self.status =
                    format!("creating bounded continuation lease with allowance {allowance}…");
            }
            Err(error) => {
                self.status = format!("failed to queue continuation lease creation: {error}");
            }
        }
    }

    pub(crate) fn propose_controller_worker_control(
        &mut self,
        worker_conversation_id: LocalConversationId,
        action: ControllerControlAction,
    ) {
        if self.controller_control_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot propose controller control: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::ProposeControllerWorkerControl {
            controller_conversation_id: self.local_conversation_id,
            worker_conversation_id,
            action,
        }) {
            Ok(()) => {
                self.controller_control_command_pending = true;
                self.status = format!("proposing controller {} control…", action.label());
            }
            Err(error) => {
                self.status = format!("failed to queue controller control proposal: {error}");
            }
        }
    }

    pub(crate) fn decide_controller_worker_control_route(
        &mut self,
        route_id: RouteId,
        decision: RouteUserDecision,
    ) {
        if self.controller_control_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status =
                "cannot decide controller control route: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::DecideControllerWorkerControlRoute { route_id, decision })
        {
            Ok(()) => {
                self.controller_control_command_pending = true;
                self.status = format!(
                    "recording explicit {} decision for controller control route {}…",
                    route_user_decision_label(decision),
                    route_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue controller control decision: {error}");
            }
        }
    }

    pub(crate) fn dispatch_controller_worker_control_route(&mut self, route_id: RouteId) {
        if self.controller_control_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status =
                "cannot dispatch controller control route: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::DispatchControllerWorkerControlRoute { route_id }) {
            Ok(()) => {
                self.controller_control_command_pending = true;
                self.status = format!(
                    "revalidating and dispatching controller control route {}…",
                    route_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue controller control dispatch: {error}");
            }
        }
    }

    pub(crate) fn decide_controller_worker_result_context(
        &mut self,
        route_id: RouteId,
        decision: ControllerWorkerResultContextDecision,
    ) {
        if self.controller_result_context_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status =
                "cannot update controller result context: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::DecideControllerWorkerResultContext {
            route_id,
            controller_conversation_id: self.local_conversation_id,
            decision,
        }) {
            Ok(()) => {
                self.controller_result_context_command_pending = true;
                self.status = format!(
                    "recording controller result context {} decision for route {}…",
                    controller_worker_result_context_decision_label(decision),
                    route_id.get(),
                );
            }
            Err(error) => {
                self.status =
                    format!("failed to queue controller result context decision: {error}");
            }
        }
    }

    pub(crate) fn decide_controller_coordination_result_context(
        &mut self,
        coordination_turn_id: LocalTurnId,
        decision: ControllerCoordinationResultContextDecision,
    ) {
        if self.controller_coordination_result_context_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status =
                "cannot update controller coordination result context: persistence unavailable"
                    .to_owned();
            return;
        };
        match sender.send(PersistCommand::DecideControllerCoordinationResultContext {
            coordination_turn_id,
            controller_conversation_id: self.local_conversation_id,
            decision,
        }) {
            Ok(()) => {
                self.controller_coordination_result_context_command_pending = true;
                self.status = format!(
                    "recording coordination result context {} decision for turn {}…",
                    controller_coordination_result_context_decision_label(decision),
                    coordination_turn_id,
                );
            }
            Err(error) => {
                self.status =
                    format!("failed to queue coordination result context decision: {error}");
            }
        }
    }

    pub(crate) fn record_controller_coordination_suggestion(
        &mut self,
        coordination_turn_id: LocalTurnId,
        basis_result_route_id: RouteId,
        action: CoordinationSuggestionAction,
    ) {
        if self.controller_coordination_suggestion_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status =
                "cannot record coordination suggestion: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::RecordControllerCoordinationSuggestion {
            controller_conversation_id: self.local_conversation_id,
            coordination_turn_id,
            basis_result_route_id,
            action,
        }) {
            Ok(()) => {
                self.controller_coordination_suggestion_command_pending = true;
                self.status = format!(
                    "recording powerless coordination suggestion {} from route {}…",
                    coordination_suggestion_action_label(action),
                    basis_result_route_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue coordination suggestion: {error}");
            }
        }
    }

    pub(crate) fn promote_controller_coordination_suggestion(
        &mut self,
        suggestion_id: CoordinationSuggestionId,
    ) {
        if self.controller_coordination_suggestion_command_pending
            || self.controller_control_command_pending
        {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status =
                "cannot promote coordination suggestion: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::PromoteControllerCoordinationSuggestion {
            controller_conversation_id: self.local_conversation_id,
            suggestion_id,
        }) {
            Ok(()) => {
                self.controller_coordination_suggestion_command_pending = true;
                self.controller_control_command_pending = true;
                self.status = format!(
                    "promoting coordination suggestion {} through ordinary control admission…",
                    suggestion_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue suggestion promotion: {error}");
            }
        }
    }

    pub(crate) fn start_controller_coordination(&mut self) {
        if self.controller_coordination_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status =
                "cannot start controller coordination: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::StartControllerCoordination {
            controller_conversation_id: self.local_conversation_id,
        }) {
            Ok(()) => {
                self.controller_coordination_command_pending = true;
                self.status =
                    "starting durable non-authored controller coordination turn…".to_owned();
            }
            Err(error) => {
                self.status = format!("failed to queue controller coordination start: {error}");
            }
        }
    }

    pub(crate) fn dispatch_controller_coordination(&mut self, turn_id: LocalTurnId) {
        if self.pending_remote_turn.is_some() || self.active_remote_turn.is_some() {
            self.status =
                "cannot dispatch controller coordination while another remote turn is active"
                    .to_owned();
            return;
        }
        if !self.remote_connected() {
            self.status =
                "cannot dispatch controller coordination: ChatGPT is not connected".to_owned();
            return;
        }
        if self.capability_probe.running() {
            self.status =
                "cannot dispatch controller coordination while capability probes are running"
                    .to_owned();
            return;
        }
        let Some(model) = self.selected_model.clone() else {
            self.status = "cannot dispatch controller coordination: no model selected".to_owned();
            return;
        };

        let coordination = match replay_controller_coordination_audit(&self.events) {
            Ok(records) => records.into_iter().find(|record| {
                record.coordination_turn_id == turn_id
                    && record.controller_conversation_id == self.local_conversation_id
            }),
            Err(error) => {
                self.status = format!("cannot replay controller coordination: {error}");
                return;
            }
        };
        let Some(coordination) = coordination else {
            self.status = format!(
                "controller coordination turn {} does not belong to this conversation",
                turn_id
            );
            return;
        };
        if coordination.result_sequence.is_some() {
            self.status = format!(
                "controller coordination turn {} is already terminal",
                turn_id
            );
            return;
        }

        let transport = match controller_coordination_transport_state(&self.events, turn_id) {
            Ok(state) => state,
            Err(error) => {
                self.status = format!("cannot replay controller coordination transport: {error}");
                return;
            }
        };
        if transport.was_dispatched() {
            self.status = format!(
                "controller coordination turn {} already has durable dispatch evidence",
                turn_id
            );
            return;
        }

        let request_patch = match self.current_behavior_request_patch() {
            Ok(patch) => patch,
            Err(error) => {
                self.status = format!("cannot coordinate with current behavior profile: {error}");
                return;
            }
        };

        let coordination_prefix = self
            .events
            .iter()
            .take_while(|event| event.sequence <= coordination.started_sequence)
            .cloned()
            .collect::<Vec<_>>();
        let mut transcript = context_transcript(&projected_local_display_messages(
            &coordination_prefix,
            self.local_conversation_id,
        ));
        match admitted_routed_context_messages(&coordination_prefix, self.local_conversation_id) {
            Ok(routed) => transcript.extend(routed),
            Err(error) => {
                self.status =
                    format!("cannot compose controller coordination routed context: {error}");
                return;
            }
        }
        match admitted_controller_worker_result_messages(
            &coordination_prefix,
            self.local_conversation_id,
        ) {
            Ok(results) => transcript.extend(results),
            Err(error) => {
                self.status = format!("cannot compose admitted controller results: {error}");
                return;
            }
        }
        transcript.sort_by_key(context_composer::TranscriptMessage::order_sequence);
        transcript.push(
            context_composer::TranscriptMessage::controller_coordination(
                coordination.controller_session_id.get(),
                coordination.coordination_turn_id.to_string(),
                coordination.started_sequence,
            ),
        );

        let context_plan = match context_composer::ContextPlan::compose_bounded(
            context_composer::ContextPolicy::dispatch(),
            self.conversation_instructions.as_str(),
            self.conversation_developer_context.as_str(),
            transcript,
        ) {
            Ok(plan) => plan,
            Err(error) => {
                self.status = format!("controller coordination context budget rejected: {error}");
                return;
            }
        };
        let frozen_tool_sources = match admitted_tool_result_context_messages(
            &coordination_prefix,
            self.local_conversation_id,
        ) {
            Ok(results) => results
                .into_iter()
                .map(|item| item.source)
                .collect::<Vec<_>>(),
            Err(error) => {
                self.status = format!("cannot audit coordination tool eligibility: {error}");
                return;
            }
        };
        let manifest = match mcp_dispatch_manifest::capture(
            self.local_conversation_id,
            "controller_coordination",
            &frozen_tool_sources,
            &context_plan,
        ) {
            Ok(manifest) => manifest,
            Err(error) => {
                self.status = format!("cannot record coordination context evidence: {error}");
                return;
            }
        };
        let request_id = coordination.coordination_turn_id.to_string();
        self.pending_remote_turn = Some(PendingRemoteTurn {
            conversation_id: self.local_conversation_id,
            turn_id: coordination.coordination_turn_id,
            request_id: request_id.clone(),
            model: model.clone(),
            input: context_plan.input_json(),
            instructions: context_plan.instructions.clone(),
            request_patch,
        });

        let payload = match mcp_dispatch_manifest::attach_to_dispatch_payload(
            remote_turn_payload(
                coordination.coordination_turn_id,
                &request_id,
                Some(&model),
                None,
                Some("non-authored controller coordination remote dispatch"),
            ),
            manifest,
        ) {
            Ok(payload) => payload,
            Err(error) => {
                self.pending_remote_turn = None;
                self.status = format!("cannot journal coordination context evidence: {error}");
                return;
            }
        };
        if self.queue_turn_event(
            coordination.coordination_turn_id,
            EventKind::DispatchAttempted,
            payload,
        ) {
            self.status = format!(
                "controller coordination turn {} durably dispatching through ChatGPT…",
                coordination.coordination_turn_id
            );
        } else {
            self.pending_remote_turn = None;
        }
    }

    pub(crate) fn acknowledge_worker_control(&mut self, route_id: RouteId) {
        if self.worker_control_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot acknowledge worker control: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::AcknowledgeWorkerControl {
            worker_conversation_id: self.local_conversation_id,
            route_id,
        }) {
            Ok(()) => {
                self.worker_control_command_pending = true;
                self.status = format!(
                    "acknowledging delivered worker control route {}…",
                    route_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue worker control acknowledgement: {error}");
            }
        }
    }

    pub(crate) fn record_worker_control_status_result(&mut self, route_id: RouteId) {
        if self.worker_control_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status =
                "cannot record worker control status result: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::RecordWorkerControlStatusResult {
            worker_conversation_id: self.local_conversation_id,
            route_id,
        }) {
            Ok(()) => {
                self.worker_control_command_pending = true;
                self.status = format!(
                    "recording durable status result for worker control route {}…",
                    route_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue worker control status result: {error}");
            }
        }
    }

    pub(crate) fn apply_worker_control(&mut self, route_id: RouteId) {
        if self.worker_control_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot apply worker control: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::ApplyWorkerControl {
            worker_conversation_id: self.local_conversation_id,
            route_id,
        }) {
            Ok(()) => {
                self.worker_control_command_pending = true;
                self.status = format!(
                    "applying acknowledged worker control route {}…",
                    route_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue worker control action: {error}");
            }
        }
    }

    pub(crate) fn start_worker_continuation_execution(&mut self, route_id: RouteId) {
        if self.worker_control_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot start continuation execution: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::StartWorkerContinuationExecution {
            worker_conversation_id: self.local_conversation_id,
            route_id,
        }) {
            Ok(()) => {
                self.worker_control_command_pending = true;
                self.status = format!(
                    "starting durable continuation execution for worker control route {}…",
                    route_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue continuation execution start: {error}");
            }
        }
    }

    pub(crate) fn dispatch_worker_continuation_execution(&mut self, route_id: RouteId) {
        if self.pending_remote_turn.is_some() || self.active_remote_turn.is_some() {
            self.status =
                "cannot dispatch continuation while another remote turn is active".to_owned();
            return;
        }
        if !self.remote_connected() {
            self.status = "cannot dispatch continuation: ChatGPT is not connected".to_owned();
            return;
        }
        if self.capability_probe.running() {
            self.status =
                "cannot dispatch continuation while capability probes are running".to_owned();
            return;
        }
        let Some(model) = self.selected_model.clone() else {
            self.status = "cannot dispatch continuation: no model selected".to_owned();
            return;
        };

        let execution = match replay_worker_continuation_execution_audit(&self.events) {
            Ok(records) => records.into_iter().find(|record| {
                record.route_id == route_id
                    && record.worker_conversation_id == self.local_conversation_id
            }),
            Err(error) => {
                self.status = format!("cannot replay continuation execution: {error}");
                return;
            }
        };
        let Some(execution) = execution else {
            self.status = format!(
                "worker control route {} has no continuation execution start for this conversation",
                route_id.get()
            );
            return;
        };

        let transport =
            match continuation_execution_transport_state(&self.events, execution.execution_turn_id)
            {
                Ok(state) => state,
                Err(error) => {
                    self.status = format!("cannot replay continuation transport: {error}");
                    return;
                }
            };
        if transport.was_dispatched() {
            self.status = format!(
                "continuation execution turn {} already has durable dispatch evidence",
                execution.execution_turn_id
            );
            return;
        }

        let request_patch = match self.current_behavior_request_patch() {
            Ok(patch) => patch,
            Err(error) => {
                self.status = format!("cannot continue with current behavior profile: {error}");
                return;
            }
        };

        let execution_prefix = self
            .events
            .iter()
            .take_while(|event| event.sequence <= execution.started_sequence)
            .cloned()
            .collect::<Vec<_>>();
        let mut transcript = context_transcript(&projected_local_display_messages(
            &execution_prefix,
            self.local_conversation_id,
        ));
        match admitted_routed_context_messages(&execution_prefix, self.local_conversation_id) {
            Ok(routed) => transcript.extend(routed),
            Err(error) => {
                self.status = format!("cannot compose continuation routed context: {error}");
                return;
            }
        }
        transcript.sort_by_key(context_composer::TranscriptMessage::order_sequence);
        transcript.push(
            context_composer::TranscriptMessage::controller_continuation(
                execution.control_id.get(),
                execution.route_id.get(),
                execution.worker_id.get(),
                execution.goal_id.get(),
                execution.lease_id.get(),
                execution.permit_ordinal,
                execution.started_sequence,
            ),
        );

        let context_plan = match context_composer::ContextPlan::compose_bounded(
            context_composer::ContextPolicy::dispatch(),
            self.conversation_instructions.as_str(),
            self.conversation_developer_context.as_str(),
            transcript,
        ) {
            Ok(plan) => plan,
            Err(error) => {
                self.status = format!("worker continuation context budget rejected: {error}");
                return;
            }
        };
        let frozen_tool_sources = match admitted_tool_result_context_messages(
            &execution_prefix,
            self.local_conversation_id,
        ) {
            Ok(results) => results
                .into_iter()
                .map(|item| item.source)
                .collect::<Vec<_>>(),
            Err(error) => {
                self.status = format!("cannot audit continuation tool eligibility: {error}");
                return;
            }
        };
        let manifest = match mcp_dispatch_manifest::capture(
            self.local_conversation_id,
            "worker_continuation",
            &frozen_tool_sources,
            &context_plan,
        ) {
            Ok(manifest) => manifest,
            Err(error) => {
                self.status = format!("cannot record continuation context evidence: {error}");
                return;
            }
        };
        let request_id = execution.execution_turn_id.to_string();
        self.pending_remote_turn = Some(PendingRemoteTurn {
            conversation_id: self.local_conversation_id,
            turn_id: execution.execution_turn_id,
            request_id: request_id.clone(),
            model: model.clone(),
            input: context_plan.input_json(),
            instructions: context_plan.instructions.clone(),
            request_patch,
        });

        let payload = match mcp_dispatch_manifest::attach_to_dispatch_payload(
            remote_turn_payload(
                execution.execution_turn_id,
                &request_id,
                Some(&model),
                None,
                Some("bounded controller continuation remote dispatch"),
            ),
            manifest,
        ) {
            Ok(payload) => payload,
            Err(error) => {
                self.pending_remote_turn = None;
                self.status = format!("cannot journal continuation context evidence: {error}");
                return;
            }
        };
        if self.queue_turn_event(
            execution.execution_turn_id,
            EventKind::DispatchAttempted,
            payload,
        ) {
            self.status = format!(
                "continuation execution turn {} durably dispatching through ChatGPT…",
                execution.execution_turn_id
            );
        } else {
            self.pending_remote_turn = None;
        }
    }

    pub(crate) fn assign_next_worker_goal(&mut self, worker_id: WorkerId) {
        if self.lifecycle_command_pending {
            return;
        }
        let current = match worker_record(&self.events, worker_id) {
            Ok(record) => record,
            Err(error) => {
                self.status = format!("cannot project worker lifecycle: {error}");
                return;
            }
        };
        let goal_id = WorkerGoalId::new(
            current
                .and_then(|record| record.lifecycle.goal_id())
                .map_or(1, |goal_id| goal_id.get().saturating_add(1)),
        );
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot assign worker goal: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::AssignWorkerGoal { worker_id, goal_id }) {
            Ok(()) => {
                self.lifecycle_command_pending = true;
                self.status = format!(
                    "assigning worker {} goal {}…",
                    worker_id.get(),
                    goal_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue worker goal assignment: {error}");
            }
        }
    }

    pub(crate) fn transition_worker(
        &mut self,
        worker_id: WorkerId,
        goal_id: WorkerGoalId,
        action: WorkerAction,
    ) {
        if self.lifecycle_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot update worker lifecycle: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::TransitionWorker {
            worker_id,
            goal_id,
            action,
        }) {
            Ok(()) => {
                self.lifecycle_command_pending = true;
                self.status = format!(
                    "recording worker {} lifecycle action {action:?}…",
                    worker_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue worker lifecycle action: {error}");
            }
        }
    }
}
