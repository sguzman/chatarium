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

/// Inert Linux stdio transport configuration for one explicitly registered
/// external tool provider. It is data only: no process is started by this type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StdioToolProviderConfig {
    executable: String,
    args: Vec<String>,
    allowed_operations: Vec<ToolOperationName>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdioToolConfigError {
    NonAbsoluteExecutable,
    NonCanonicalExecutable,
    InvalidArgument,
    TooManyArguments,
    EmptyOperationAllowlist,
    TooManyOperations,
    DuplicateOperation,
}

impl StdioToolProviderConfig {
    pub const MAX_ARGUMENTS: usize = 16;
    pub const MAX_ARGUMENT_BYTES: usize = 1024;
    pub const MAX_EXECUTABLE_BYTES: usize = 4096;
    pub const MAX_ALLOWED_OPERATIONS: usize = 32;

    /// Validate an absolute path, bounded individual argv strings, and an
    /// exact allowlist. This does not check whether the executable exists, is
    /// safe, trusted, or runnable. That remains a separate activation policy.
    pub fn new(
        executable: impl Into<String>,
        args: Vec<String>,
        allowed_operations: Vec<ToolOperationName>,
    ) -> Result<Self, StdioToolConfigError> {
        let executable = executable.into();
        // This type deliberately records *Linux* stdio launch configuration.
        // Validate Unix lexical path syntax even when cargo tests run on Windows.
        if !executable.starts_with('/') {
            return Err(StdioToolConfigError::NonAbsoluteExecutable);
        }
        if executable == "/"
            || executable.len() > Self::MAX_EXECUTABLE_BYTES
            || executable.chars().any(char::is_control)
            || executable.contains('\\')
            || executable[1..]
                .split('/')
                .any(|component| component.is_empty() || matches!(component, "." | ".."))
        {
            return Err(StdioToolConfigError::NonCanonicalExecutable);
        }
        if args.len() > Self::MAX_ARGUMENTS {
            return Err(StdioToolConfigError::TooManyArguments);
        }
        if args
            .iter()
            .any(|arg| arg.len() > Self::MAX_ARGUMENT_BYTES || arg.chars().any(char::is_control))
        {
            return Err(StdioToolConfigError::InvalidArgument);
        }
        if allowed_operations.is_empty() {
            return Err(StdioToolConfigError::EmptyOperationAllowlist);
        }
        if allowed_operations.len() > Self::MAX_ALLOWED_OPERATIONS {
            return Err(StdioToolConfigError::TooManyOperations);
        }
        let distinct = allowed_operations
            .iter()
            .map(ToolOperationName::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        if distinct.len() != allowed_operations.len() {
            return Err(StdioToolConfigError::DuplicateOperation);
        }
        Ok(Self {
            executable,
            args,
            allowed_operations,
        })
    }

    #[must_use]
    pub fn executable(&self) -> &str {
        &self.executable
    }

    #[must_use]
    pub fn args(&self) -> &[String] {
        &self.args
    }

    #[must_use]
    pub fn allowed_operations(&self) -> &[ToolOperationName] {
        &self.allowed_operations
    }

    #[must_use]
    pub fn allows(&self, operation: &ToolOperationName) -> bool {
        self.allowed_operations.iter().any(|name| name == operation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdio_transport_config_is_inert_and_bounded() {
        let op = ToolOperationName::new("search").unwrap();
        let config = StdioToolProviderConfig::new(
            "/usr/bin/local-mcp",
            vec!["--stdio".to_owned()],
            vec![op.clone()],
        )
        .unwrap();
        assert_eq!(config.executable(), "/usr/bin/local-mcp");
        assert_eq!(config.args(), &["--stdio"]);
        assert!(config.allows(&op));
        assert!(!config.allows(&ToolOperationName::new("write").unwrap()));

        assert_eq!(
            StdioToolProviderConfig::new("local-mcp", Vec::new(), vec![op.clone()]),
            Err(StdioToolConfigError::NonAbsoluteExecutable)
        );
        assert_eq!(
            StdioToolProviderConfig::new("/usr/../bin/local-mcp", Vec::new(), vec![op.clone()]),
            Err(StdioToolConfigError::NonCanonicalExecutable)
        );
        assert_eq!(
            StdioToolProviderConfig::new(
                "/usr/bin/local-mcp",
                vec!["bad\narg".to_owned()],
                vec![op.clone()]
            ),
            Err(StdioToolConfigError::InvalidArgument)
        );
        assert_eq!(
            StdioToolProviderConfig::new("/usr/bin/local-mcp", Vec::new(), Vec::new()),
            Err(StdioToolConfigError::EmptyOperationAllowlist)
        );
        assert_eq!(
            StdioToolProviderConfig::new("/usr/bin/local-mcp", Vec::new(), vec![op.clone(), op]),
            Err(StdioToolConfigError::DuplicateOperation)
        );
    }

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
