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

/// Exact user-authored organization label for a local memory artifact.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LocalMemoryLabel(String);

impl LocalMemoryLabel {
    pub const MAX_UNICODE_SCALARS: usize = 64;

    pub fn new(value: impl Into<String>) -> Result<Self, LocalMemoryLabelError> {
        let value = value.into();
        if value.is_empty() {
            return Err(LocalMemoryLabelError::Empty);
        }
        if value.trim() != value {
            return Err(LocalMemoryLabelError::SurroundingWhitespace);
        }
        if value.chars().any(char::is_control) {
            return Err(LocalMemoryLabelError::ControlCharacter);
        }
        let unicode_scalars = value.chars().count();
        if unicode_scalars > Self::MAX_UNICODE_SCALARS {
            return Err(LocalMemoryLabelError::TooLong { unicode_scalars });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for LocalMemoryLabel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalMemoryLabelError {
    Empty,
    SurroundingWhitespace,
    ControlCharacter,
    TooLong { unicode_scalars: usize },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_memory_label_preserves_exact_valid_text() {
        let label = LocalMemoryLabel::new("project:chatarium").unwrap();
        assert_eq!(label.as_str(), "project:chatarium");
        assert_eq!(label.to_string(), "project:chatarium");
    }

    #[test]
    fn local_memory_label_rejects_noncanonical_or_unsafe_text() {
        assert_eq!(LocalMemoryLabel::new(""), Err(LocalMemoryLabelError::Empty));
        assert_eq!(
            LocalMemoryLabel::new(" spaced "),
            Err(LocalMemoryLabelError::SurroundingWhitespace)
        );
        assert_eq!(
            LocalMemoryLabel::new("bad\nlabel"),
            Err(LocalMemoryLabelError::ControlCharacter)
        );
        let too_long = "x".repeat(LocalMemoryLabel::MAX_UNICODE_SCALARS + 1);
        assert_eq!(
            LocalMemoryLabel::new(too_long),
            Err(LocalMemoryLabelError::TooLong {
                unicode_scalars: LocalMemoryLabel::MAX_UNICODE_SCALARS + 1,
            })
        );
    }

    #[test]
    fn local_memory_id_round_trips_opaque_value() {
        let id = LocalMemoryId::new(7);
        assert_eq!(id.get(), 7);
        assert_eq!(id.to_string(), "7");
    }
}
