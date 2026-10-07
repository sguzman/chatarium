//! Explicit local memory artifact identity.
//!
//! A local memory artifact is immutable user-controlled local data. It is not
//! transcript authorship, routing state, lifecycle state, or inference context
//! merely because it exists.

use std::fmt;

/// Opaque local identity for one immutable memory artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LocalMemoryId(u64);

impl LocalMemoryId {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for LocalMemoryId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_memory_id_round_trips_opaque_value() {
        let id = LocalMemoryId::new(7);
        assert_eq!(id.get(), 7);
        assert_eq!(id.to_string(), "7");
    }
}
