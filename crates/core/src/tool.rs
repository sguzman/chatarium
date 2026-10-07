//! Local tool/MCP identity and naming primitives.
//!
//! These types deliberately do not define the wire envelope or execute tools.
//! Permission authority remains in the routing layer.

use crate::routing::RouteEndpointId;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ToolProviderId(u64);

impl ToolProviderId {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for ToolProviderId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ToolCallId(u64);

impl ToolCallId {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for ToolCallId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ToolProviderName(String);

impl ToolProviderName {
    pub const MAX_UNICODE_SCALARS: usize = 128;

    pub fn new(value: impl Into<String>) -> Result<Self, ToolNameError> {
        validate_name(value.into()).map(Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for ToolProviderName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ToolOperationName(String);

impl ToolOperationName {
    pub const MAX_UNICODE_SCALARS: usize = 128;

    pub fn new(value: impl Into<String>) -> Result<Self, ToolNameError> {
        validate_name(value.into()).map(Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for ToolOperationName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolNameError {
    Empty,
    SurroundingWhitespace,
    ControlCharacter,
    TooLong { unicode_scalars: usize },
}

fn validate_name(value: String) -> Result<String, ToolNameError> {
    if value.is_empty() {
        return Err(ToolNameError::Empty);
    }
    if value.trim() != value {
        return Err(ToolNameError::SurroundingWhitespace);
    }
    if value.chars().any(char::is_control) {
        return Err(ToolNameError::ControlCharacter);
    }
    let unicode_scalars = value.chars().count();
    if unicode_scalars > ToolProviderName::MAX_UNICODE_SCALARS {
        return Err(ToolNameError::TooLong { unicode_scalars });
    }
    Ok(value)
}

/// Explicit routing correlation for one registered tool/provider surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolProviderEndpointBinding {
    provider_id: ToolProviderId,
    endpoint_id: RouteEndpointId,
}

impl ToolProviderEndpointBinding {
    #[must_use]
    pub const fn new(provider_id: ToolProviderId, endpoint_id: RouteEndpointId) -> Self {
        Self {
            provider_id,
            endpoint_id,
        }
    }

    #[must_use]
    pub const fn provider_id(self) -> ToolProviderId {
        self.provider_id
    }

    #[must_use]
    pub const fn endpoint_id(self) -> RouteEndpointId {
        self.endpoint_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_and_call_ids_remain_distinct_local_domains() {
        let provider = ToolProviderId::new(7);
        let call = ToolCallId::new(7);
        assert_eq!(provider.get(), 7);
        assert_eq!(call.get(), 7);
        assert_eq!(provider.to_string(), "7");
        assert_eq!(call.to_string(), "7");
    }

    #[test]
    fn names_preserve_exact_valid_text_and_reject_noncanonical_values() {
        let provider = ToolProviderName::new("local-files").unwrap();
        let operation = ToolOperationName::new("read_file").unwrap();
        assert_eq!(provider.as_str(), "local-files");
        assert_eq!(operation.as_str(), "read_file");

        assert_eq!(ToolProviderName::new(""), Err(ToolNameError::Empty));
        assert_eq!(
            ToolOperationName::new(" read_file "),
            Err(ToolNameError::SurroundingWhitespace)
        );
        assert_eq!(
            ToolProviderName::new("bad\nname"),
            Err(ToolNameError::ControlCharacter)
        );
    }

    #[test]
    fn provider_endpoint_binding_preserves_identity_domains() {
        let provider = ToolProviderId::new(3);
        let endpoint = RouteEndpointId::new(9);
        let binding = ToolProviderEndpointBinding::new(provider, endpoint);
        assert_eq!(binding.provider_id(), provider);
        assert_eq!(binding.endpoint_id(), endpoint);
    }
}
