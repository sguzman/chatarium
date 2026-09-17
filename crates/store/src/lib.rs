//! Durable-storage boundary for Chatarium.
//!
//! The initial implementation is deliberately small. SQLite migrations and the append-only
//! journal land in P2; this crate exists now so persistence does not leak into UI/protocol code.

use chatarium_core::EventKind;

/// Minimal event envelope used while the durable schema is being designed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventEnvelope {
    /// Monotonic local sequence assigned by a concrete store.
    pub sequence: u64,
    /// Semantic event kind.
    pub kind: EventKind,
    /// Exact textual payload when applicable.
    pub payload: String,
}

/// Append/read contract required by the application core.
pub trait EventStore {
    /// Store one event and return its durable sequence.
    fn append(&mut self, kind: EventKind, payload: String) -> u64;

    /// Return events in durable sequence order.
    fn events(&self) -> &[EventEnvelope];
}

/// In-memory reference store used by bootstrap tests; not a durability implementation.
#[derive(Debug, Default)]
pub struct MemoryEventStore {
    events: Vec<EventEnvelope>,
}

impl EventStore for MemoryEventStore {
    fn append(&mut self, kind: EventKind, payload: String) -> u64 {
        let sequence = u64::try_from(self.events.len()).expect("event count fits u64") + 1;
        self.events.push(EventEnvelope { sequence, kind, payload });
        sequence
    }

    fn events(&self) -> &[EventEnvelope] {
        &self.events
    }
}
