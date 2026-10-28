//! Desktop routing actions extracted intact from ChatariumApp.
use super::super::super::*;

impl ChatariumApp {
    pub(crate) fn initialize_local_orchestration_topology(&mut self) {
        if self.topology_command_pending {
            return;
        }
        match local_conversation_topology(&self.events, self.local_conversation_id) {
            Ok(Some(topology)) => {
                self.status = format!(
                    "local orchestration topology already exists · container {} · session {}",
                    topology.container_id.get(),
                    topology.current_session_id.get()
                );
                return;
            }
            Ok(None) => {}
            Err(error) => {
                self.status = format!("cannot project local orchestration topology: {error}");
                return;
            }
        }

        let (container_id, root_session_id) =
            match next_available_local_orchestration_ids(&self.events) {
                Ok(ids) => ids,
                Err(error) => {
                    self.status =
                        format!("cannot allocate local orchestration identities: {error}");
                    return;
                }
            };
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot initialize local orchestration topology: persistence unavailable"
                .to_owned();
            return;
        };

        match sender.send(PersistCommand::InitializeLocalOrchestrationTopology {
            conversation_id: self.local_conversation_id,
            container_id,
            root_session_id,
        }) {
            Ok(()) => {
                self.topology_command_pending = true;
                self.status = format!(
                    "initializing local orchestration topology · container {} · root session {}…",
                    container_id.get(),
                    root_session_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue local orchestration topology: {error}");
            }
        }
    }

    pub(crate) fn bind_current_session_route_endpoint(&mut self, session_id: SessionId) {
        if self.route_addressability_command_pending {
            return;
        }
        let endpoint_id = match next_available_route_endpoint_id(&self.events) {
            Ok(endpoint_id) => endpoint_id,
            Err(error) => {
                self.status = format!("cannot allocate routing endpoint identity: {error}");
                return;
            }
        };
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot bind routing endpoint: persistence unavailable".to_owned();
            return;
        };

        match sender.send(PersistCommand::BindCurrentSessionRouteEndpoint {
            conversation_id: self.local_conversation_id,
            session_id,
            endpoint_id,
        }) {
            Ok(()) => {
                self.route_addressability_command_pending = true;
                self.status = format!(
                    "binding current session {} to routing endpoint {}…",
                    session_id.get(),
                    endpoint_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue routing endpoint binding: {error}");
            }
        }
    }

    pub(crate) fn propose_local_session_route(
        &mut self,
        destination_conversation_id: LocalConversationId,
    ) {
        if self.route_policy_command_pending {
            return;
        }
        let route_id = match next_available_route_id(&self.events) {
            Ok(route_id) => route_id,
            Err(error) => {
                self.status = format!("cannot allocate local route identity: {error}");
                return;
            }
        };
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot propose local route: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::ProposeLocalSessionRoute {
            route_id,
            source_conversation_id: self.local_conversation_id,
            destination_conversation_id,
        }) {
            Ok(()) => {
                self.route_policy_command_pending = true;
                self.status = format!(
                    "proposing local route {} for explicit approval…",
                    route_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue local route proposal: {error}");
            }
        }
    }

    pub(crate) fn attach_local_session_route_payload(&mut self, route_id: RouteId) {
        if self.route_payload_command_pending {
            return;
        }
        let text = self
            .route_payload_drafts
            .get(&route_id)
            .cloned()
            .unwrap_or_default();
        if text.trim().is_empty() {
            self.status = "route payload text cannot be empty".to_owned();
            return;
        }
        let payload_id = match next_available_route_payload_id(&self.events) {
            Ok(payload_id) => payload_id,
            Err(error) => {
                self.status = format!("cannot allocate route payload identity: {error}");
                return;
            }
        };
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot attach route payload: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::AttachLocalSessionRoutePayload {
            payload_id,
            route_id,
            text,
        }) {
            Ok(()) => {
                self.route_payload_command_pending = true;
                self.status = format!(
                    "attaching immutable payload {} to local route {}…",
                    payload_id.get(),
                    route_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue local route payload: {error}");
            }
        }
    }

    pub(crate) fn decide_local_session_route(
        &mut self,
        route_id: RouteId,
        decision: RouteUserDecision,
    ) {
        if self.route_policy_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot record local route decision: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::DecideLocalSessionRoute { route_id, decision }) {
            Ok(()) => {
                self.route_policy_command_pending = true;
                self.status = format!(
                    "recording explicit {} decision for local route {}…",
                    route_user_decision_label(decision),
                    route_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue local route decision: {error}");
            }
        }
    }

    pub(crate) fn dispatch_local_session_route(&mut self, route_id: RouteId) {
        if self.route_dispatch_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot dispatch local route: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::DispatchLocalSessionRoute { route_id }) {
            Ok(()) => {
                self.route_dispatch_command_pending = true;
                self.status = format!(
                    "consuming local route {} dispatch authority and recording delivery…",
                    route_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue local route dispatch: {error}");
            }
        }
    }

    pub(crate) fn decide_local_route_context(
        &mut self,
        route_id: RouteId,
        decision: LocalRouteContextDecision,
    ) {
        if self.route_context_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status =
                "cannot update routed context eligibility: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::DecideLocalRouteContext {
            route_id,
            destination_conversation_id: self.local_conversation_id,
            decision,
        }) {
            Ok(()) => {
                self.route_context_command_pending = true;
                self.status = format!(
                    "recording routed context {} decision for route {}…",
                    local_route_context_decision_label(decision),
                    route_id.get(),
                );
            }
            Err(error) => {
                self.status = format!("failed to queue routed context decision: {error}");
            }
        }
    }
}
