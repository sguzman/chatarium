//! Read-only inbox projection for durably delivered worker controls.
//!
//! Delivery, display, lifecycle mutation, and inference context remain separate.
//! This projection joins immutable control admission with delivery provenance and
//! creates no new journal fact.

use crate::EventEnvelope;
use crate::control_audit::replay_control_audit;
use crate::control_delivery_audit::replay_worker_control_delivery_audit;
use chatarium_core::LocalConversationId;
use chatarium_core::control::{ControlId, WorkerControlKind};
use chatarium_core::orchestration::{WorkerGoalId, WorkerId};
use chatarium_core::routing::RouteId;
use chatarium_core::session::SessionId;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerControlInboxItem {
    pub control_id: ControlId,
    pub route_id: RouteId,
    pub worker_id: WorkerId,
    pub worker_conversation_id: LocalConversationId,
    pub worker_session_id: SessionId,
    pub controller_session_id: SessionId,
    pub goal_id: WorkerGoalId,
    pub kind: WorkerControlKind,
    pub admitted_sequence: u64,
    pub dispatch_sequence: u64,
    pub delivered_sequence: u64,
}

pub fn replay_worker_control_inbox(
    events: &[EventEnvelope],
) -> Result<Vec<WorkerControlInboxItem>, String> {
    let controls = replay_control_audit(events)?
        .into_iter()
        .map(|record| (record.control_id, record))
        .collect::<BTreeMap<_, _>>();

    let mut items = Vec::new();
    for delivery in replay_worker_control_delivery_audit(events)? {
        let control = controls.get(&delivery.control_id).ok_or_else(|| {
            format!(
                "delivered worker control {} has no admitted control record",
                delivery.control_id.get()
            )
        })?;
        if control.worker_id != delivery.worker_id {
            return Err(format!(
                "delivered worker control {} worker identity disagrees with admitted control",
                delivery.control_id.get()
            ));
        }
        if control.admitted_sequence >= delivery.dispatch_sequence {
            return Err(format!(
                "delivered worker control {} admission does not precede dispatch",
                delivery.control_id.get()
            ));
        }

        items.push(WorkerControlInboxItem {
            control_id: delivery.control_id,
            route_id: delivery.route_id,
            worker_id: delivery.worker_id,
            worker_conversation_id: delivery.worker_conversation_id,
            worker_session_id: delivery.worker_session_id,
            controller_session_id: delivery.controller_session_id,
            goal_id: control.goal_id,
            kind: control.kind,
            admitted_sequence: control.admitted_sequence,
            dispatch_sequence: delivery.dispatch_sequence,
            delivered_sequence: delivery.delivered_sequence,
        });
    }

    items.sort_by_key(|item| item.delivered_sequence);
    Ok(items)
}

pub fn replay_worker_control_inbox_for_conversation(
    events: &[EventEnvelope],
    conversation_id: LocalConversationId,
) -> Result<Vec<WorkerControlInboxItem>, String> {
    Ok(replay_worker_control_inbox(events)?
        .into_iter()
        .filter(|item| item.worker_conversation_id == conversation_id)
        .collect())
}
