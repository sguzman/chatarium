//! Durable journal notice handling; no changes to original ordered match logic.
use super::super::*;

mod durable;
mod failures;
mod memory;
mod openviking;
mod remote;
mod routing;
mod tool_outcomes;
mod tool_proposals;
mod workers;

impl ChatariumApp {
    pub(crate) fn process_notices(&mut self) {
        let notices = self
            .notice_rx
            .as_ref()
            .map(|receiver| receiver.try_iter().collect::<Vec<_>>())
            .unwrap_or_default();

        for notice in notices {
            match notice {
                notice @ (
                    PersistNotice::ProjectOpenVikingRetrievalFinished { .. }
                    | PersistNotice::OpenVikingRetrievalFinished { .. }
                    | PersistNotice::OpenVikingMemoryWriteFinished { .. }
                    | PersistNotice::OpenVikingMemorySummarySaved { .. }
                ) => self.apply_openviking_notice(notice),
                notice @ (
                    PersistNotice::DraftSaved { .. }
                    | PersistNotice::MessageCommitted { .. }
                    | PersistNotice::AgentActivityAppended { .. }
                    | PersistNotice::MemoryCheckpointQueued { .. }
                    | PersistNotice::TurnEventAppended { .. }
                ) => self.apply_durable_notice(notice),
                notice @ (
                    PersistNotice::HistoricalConversationLoaded { .. }
                    | PersistNotice::HistoricalConversationLoadFailed { .. }
                    | PersistNotice::LiveConversationLoaded { .. }
                    | PersistNotice::LiveConversationLoadFailed { .. }
                    | PersistNotice::HistoricalLiveMirrorPromoted { .. }
                    | PersistNotice::HistoricalLiveMirrorPromotionFailed { .. }
                    | PersistNotice::DiscoveredLiveMirrorPromoted { .. }
                    | PersistNotice::DiscoveredLiveMirrorPromotionFailed { .. }
                    | PersistNotice::MirrorQueueUpdated { .. }
                    | PersistNotice::RemoteHealthUpdated { .. }
                ) => self.apply_remote_notice(notice),
                notice @ (
                    PersistNotice::OrchestrationTopologyInitialized { .. }
                    | PersistNotice::RouteEndpointBound { .. }
                    | PersistNotice::LocalRoutePolicyEventAppended { .. }
                    | PersistNotice::LocalRoutePayloadAttached { .. }
                    | PersistNotice::LocalRouteDispatchUpdated { .. }
                    | PersistNotice::LocalRouteContextDecisionUpdated { .. }
                ) => self.apply_routing_notice(notice),
                notice @ (
                    PersistNotice::ToolProviderUpdated { .. }
                    | PersistNotice::AgentMcpCallProposed { .. }
                ) => self.apply_tool_proposals_notice(notice),
                notice @ (
                    PersistNotice::ToolCallProposed { .. }
                    | PersistNotice::ToolCallRoutePolicyUpdated { .. }
                    | PersistNotice::HostShellDispatched { .. }
                    | PersistNotice::BuiltinHelloDispatched { .. }
                    | PersistNotice::SandboxHostChecked { .. }
                    | PersistNotice::StdioToolDispatchReserved { .. }
                    | PersistNotice::StdioToolDispatchFinished { .. }
                    | PersistNotice::ToolResultContextDecisionUpdated { .. }
                ) => self.apply_tool_outcomes_notice(notice),
                notice @ (
                    PersistNotice::LocalMemoryArtifactRecorded { .. }
                    | PersistNotice::LocalMemoryContextDecisionUpdated { .. }
                    | PersistNotice::LocalMemorySupersessionRecorded { .. }
                    | PersistNotice::LocalMemoryLabelUpdated { .. }
                ) => self.apply_memory_notice(notice),
                notice @ (
                    PersistNotice::LifecycleEventAppended { .. }
                    | PersistNotice::SupervisionEventAppended { .. }
                    | PersistNotice::ControllerContinuationLeaseCreated { .. }
                    | PersistNotice::ControllerControlProposed { .. }
                    | PersistNotice::ControllerControlRoutePolicyUpdated { .. }
                    | PersistNotice::ControllerControlDispatched { .. }
                    | PersistNotice::ControllerWorkerResultContextDecisionUpdated { .. }
                    | PersistNotice::ControllerCoordinationStarted { .. }
                    | PersistNotice::ControllerCoordinationResultRecorded { .. }
                    | PersistNotice::ControllerCoordinationResultContextDecisionUpdated { .. }
                    | PersistNotice::ControllerCoordinationSuggestionRecorded { .. }
                    | PersistNotice::ControllerCoordinationSuggestionPromoted { .. }
                    | PersistNotice::WorkerControlAcknowledged { .. }
                    | PersistNotice::WorkerControlStatusResultRecorded { .. }
                    | PersistNotice::WorkerControlActionAdvanced { .. }
                    | PersistNotice::WorkerContinuationExecutionStarted { .. }
                    | PersistNotice::WorkerContinuationExecutionResultRecorded { .. }
                ) => self.apply_workers_notice(notice),
                notice @ PersistNotice::Failed { .. } => self.apply_failures_notice(notice),
            }
        }
    }
}
