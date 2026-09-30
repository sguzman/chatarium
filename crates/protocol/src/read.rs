//! Evidence-gated read-side protocol observations for P3.
//!
//! This module describes only the safe structural metadata that Flight Recorder
//! v0.7.0 is allowed to expose. It deliberately does not interpret a read
//! response as a conversation list or conversation fetch until a committed
//! controlled observation establishes that semantic baseline.

use crate::Compatibility;
use std::collections::BTreeSet;
use std::fmt;

/// Controlled C01 evidence exists at `2026-09-30.001`, but it does not establish complete\n/// conversation enumeration, so no semantic baseline exists yet.
pub const LATEST_VALIDATED_CONVERSATION_LIST_OBSERVATION: Option<&str> = None;

/// No committed C02 semantic baseline exists yet.
pub const LATEST_VALIDATED_CONVERSATION_FETCH_OBSERVATION: Option<&str> = None;

/// Controlled read experiment that produced an observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadExperiment {
    /// C01 — observe the conversation-list surface.
    ConversationList,
    /// C02 — open one existing conversation.
    OpenConversation,
}

impl ReadExperiment {
    /// Stable experiment identifier used by the protocol corpus.
    #[must_use]
    pub const fn stable_name(self) -> &'static str {
        match self {
            Self::ConversationList => "C01-conversation-list",
            Self::OpenConversation => "C02-open-conversation",
        }
    }

    /// Parse a stable experiment identifier.
    #[must_use]
    pub fn from_stable_name(value: &str) -> Option<Self> {
        match value {
            "C01-conversation-list" => Some(Self::ConversationList),
            "C02-open-conversation" => Some(Self::OpenConversation),
            _ => None,
        }
    }

    /// P3 semantic flow associated with this controlled experiment.
    #[must_use]
    pub const fn flow(self) -> ReadFlow {
        match self {
            Self::ConversationList => ReadFlow::ConversationList,
            Self::OpenConversation => ReadFlow::ConversationFetch,
        }
    }
}

/// P3 read flow whose semantic compatibility can be evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadFlow {
    /// Enumerating existing remote conversations.
    ConversationList,
    /// Fetching one existing remote conversation.
    ConversationFetch,
}

impl ReadFlow {
    /// Stable local diagnostic name.
    #[must_use]
    pub const fn stable_name(self) -> &'static str {
        match self {
            Self::ConversationList => "conversation_list",
            Self::ConversationFetch => "conversation_fetch",
        }
    }

    /// Parse a stable local diagnostic name.
    #[must_use]
    pub fn from_stable_name(value: &str) -> Option<Self> {
        match value {
            "conversation_list" => Some(Self::ConversationList),
            "conversation_fetch" => Some(Self::ConversationFetch),
            _ => None,
        }
    }
}

/// Read-only HTTP method allowed by the P3.1 capture boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadMethod {
    /// GET response.
    Get,
    /// HEAD response.
    Head,
}

impl ReadMethod {
    /// Stable HTTP method spelling.
    #[must_use]
    pub const fn stable_name(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Head => "HEAD",
        }
    }

    /// Parse an allowed read method.
    #[must_use]
    pub fn from_stable_name(value: &str) -> Option<Self> {
        match value {
            "GET" => Some(Self::Get),
            "HEAD" => Some(Self::Head),
            _ => None,
        }
    }
}

/// JSON top-level shape observed after structural reduction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonTopLevelType {
    Object,
    Array,
    String,
    Number,
    Bool,
    Null,
}

impl JsonTopLevelType {
    /// Stable structural type name.
    #[must_use]
    pub const fn stable_name(self) -> &'static str {
        match self {
            Self::Object => "object",
            Self::Array => "array",
            Self::String => "string",
            Self::Number => "number",
            Self::Bool => "bool",
            Self::Null => "null",
        }
    }

    /// Parse a structural type name.
    #[must_use]
    pub fn from_stable_name(value: &str) -> Option<Self> {
        match value {
            "object" => Some(Self::Object),
            "array" => Some(Self::Array),
            "string" => Some(Self::String),
            "number" => Some(Self::Number),
            "bool" => Some(Self::Bool),
            "null" => Some(Self::Null),
            _ => None,
        }
    }
}

/// Invalid structural read observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadObservationError {
    EmptyProtocolObservationRevision,
    PathOutsideBackendApi,
    NonJsonContentType,
    InvalidStatus(u16),
    EmptyQueryKey,
    DuplicateQueryKey(String),
    HeadCannotHaveBody,
    TopLevelTypeWithoutBody,
    TruncatedBodyCannotClaimTopLevelType,
}

impl fmt::Display for ReadObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyProtocolObservationRevision => {
                write!(formatter, "protocol observation revision must not be empty")
            }
            Self::PathOutsideBackendApi => {
                write!(
                    formatter,
                    "read observation path must be under /backend-api/"
                )
            }
            Self::NonJsonContentType => {
                write!(
                    formatter,
                    "read observation content type must be JSON-family"
                )
            }
            Self::InvalidStatus(status) => {
                write!(
                    formatter,
                    "read observation HTTP status {status} is invalid"
                )
            }
            Self::EmptyQueryKey => {
                write!(formatter, "read observation query key must not be empty")
            }
            Self::DuplicateQueryKey(key) => {
                write!(formatter, "duplicate read observation query key {key:?}")
            }
            Self::HeadCannotHaveBody => write!(formatter, "HEAD observation cannot have a body"),
            Self::TopLevelTypeWithoutBody => {
                write!(formatter, "top-level JSON type requires an observed body")
            }
            Self::TruncatedBodyCannotClaimTopLevelType => {
                write!(
                    formatter,
                    "truncated body cannot claim a semantic top-level JSON type"
                )
            }
        }
    }
}

impl std::error::Error for ReadObservationError {}

/// Safe structural metadata for one P3 read observation.
///
/// No raw response body, title, message text, cookie, header, authorization
/// value, query value, or request body belongs in this type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadObservation {
    protocol_revision: String,
    experiment: ReadExperiment,
    method: ReadMethod,
    path: String,
    query_keys: Vec<String>,
    status: u16,
    content_type: String,
    truncated: bool,
    body_present: bool,
    top_level_type: Option<JsonTopLevelType>,
}

impl ReadObservation {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        protocol_revision: impl Into<String>,
        experiment: ReadExperiment,
        method: ReadMethod,
        path: impl Into<String>,
        query_keys: Vec<String>,
        status: u16,
        content_type: impl Into<String>,
        truncated: bool,
        body_present: bool,
        top_level_type: Option<JsonTopLevelType>,
    ) -> Result<Self, ReadObservationError> {
        let protocol_revision = protocol_revision.into();
        if protocol_revision.is_empty() {
            return Err(ReadObservationError::EmptyProtocolObservationRevision);
        }

        let path = path.into();
        if !path.starts_with("/backend-api/") {
            return Err(ReadObservationError::PathOutsideBackendApi);
        }

        let content_type = content_type.into();
        if !json_content_type(&content_type) {
            return Err(ReadObservationError::NonJsonContentType);
        }

        if !(100..=599).contains(&status) {
            return Err(ReadObservationError::InvalidStatus(status));
        }

        let mut unique = BTreeSet::new();
        for key in &query_keys {
            if key.is_empty() {
                return Err(ReadObservationError::EmptyQueryKey);
            }
            if !unique.insert(key.clone()) {
                return Err(ReadObservationError::DuplicateQueryKey(key.clone()));
            }
        }
        let query_keys = unique.into_iter().collect::<Vec<_>>();

        if method == ReadMethod::Head && body_present {
            return Err(ReadObservationError::HeadCannotHaveBody);
        }
        if !body_present && top_level_type.is_some() {
            return Err(ReadObservationError::TopLevelTypeWithoutBody);
        }
        if truncated && top_level_type.is_some() {
            return Err(ReadObservationError::TruncatedBodyCannotClaimTopLevelType);
        }

        Ok(Self {
            protocol_revision,
            experiment,
            method,
            path,
            query_keys,
            status,
            content_type,
            truncated,
            body_present,
            top_level_type,
        })
    }

    #[must_use]
    pub fn protocol_revision(&self) -> &str {
        &self.protocol_revision
    }

    #[must_use]
    pub const fn experiment(&self) -> ReadExperiment {
        self.experiment
    }

    #[must_use]
    pub const fn flow(&self) -> ReadFlow {
        self.experiment.flow()
    }

    #[must_use]
    pub const fn method(&self) -> ReadMethod {
        self.method
    }

    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    #[must_use]
    pub fn query_keys(&self) -> &[String] {
        &self.query_keys
    }

    #[must_use]
    pub const fn status(&self) -> u16 {
        self.status
    }

    #[must_use]
    pub fn content_type(&self) -> &str {
        &self.content_type
    }

    #[must_use]
    pub const fn truncated(&self) -> bool {
        self.truncated
    }

    #[must_use]
    pub const fn body_present(&self) -> bool {
        self.body_present
    }

    #[must_use]
    pub const fn top_level_type(&self) -> Option<JsonTopLevelType> {
        self.top_level_type
    }
}

/// Evaluate semantic support for one named read flow and protocol revision.
///
/// This is intentionally independent from the global latest validated
/// observation, which currently describes the C03 text-turn baseline and cannot
/// establish C01/C02 read semantics.
#[must_use]
pub fn compatibility_for_read_flow(flow: ReadFlow, observed_revision: &str) -> Compatibility {
    let baseline = match flow {
        ReadFlow::ConversationList => LATEST_VALIDATED_CONVERSATION_LIST_OBSERVATION,
        ReadFlow::ConversationFetch => LATEST_VALIDATED_CONVERSATION_FETCH_OBSERVATION,
    };

    match baseline {
        None => Compatibility::NoBaseline,
        Some(expected) if expected == observed_revision => {
            Compatibility::ValidatedAgainst(expected.to_owned())
        }
        Some(expected) => Compatibility::Mismatch {
            expected_revision: expected.to_owned(),
            detail: format!(
                "{} observation revision {observed_revision:?} differs from validated baseline {expected:?}",
                flow.stable_name()
            ),
        },
    }
}

fn json_content_type(value: &str) -> bool {
    let mime = value
        .split(';')
        .next()
        .unwrap_or(value)
        .trim()
        .to_ascii_lowercase();
    mime == "application/json" || mime == "text/json" || mime.ends_with("+json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LATEST_VALIDATED_OBSERVATION;

    fn valid(
        experiment: ReadExperiment,
        method: ReadMethod,
    ) -> Result<ReadObservation, ReadObservationError> {
        ReadObservation::new(
            "future-read-observation",
            experiment,
            method,
            "/backend-api/example",
            vec!["offset".to_owned(), "limit".to_owned()],
            200,
            "application/json; charset=utf-8",
            false,
            method == ReadMethod::Get,
            (method == ReadMethod::Get).then_some(JsonTopLevelType::Object),
        )
    }

    #[test]
    fn c01_and_c02_have_no_semantic_baseline_yet() {
        assert_eq!(
            compatibility_for_read_flow(ReadFlow::ConversationList, "2026-09-30.001"),
            Compatibility::NoBaseline
        );
        assert_eq!(
            compatibility_for_read_flow(ReadFlow::ConversationFetch, "2026-09-29.002"),
            Compatibility::NoBaseline
        );
        assert_eq!(LATEST_VALIDATED_OBSERVATION, Some("2026-09-29.002"));
    }

    #[test]
    fn post_is_not_an_allowed_read_method() {
        assert_eq!(ReadMethod::from_stable_name("POST"), None);
    }

    #[test]
    fn get_and_bodyless_head_are_valid_safe_observations() {
        let get = valid(ReadExperiment::ConversationList, ReadMethod::Get).unwrap();
        assert!(get.body_present());
        assert_eq!(get.top_level_type(), Some(JsonTopLevelType::Object));

        let head = valid(ReadExperiment::OpenConversation, ReadMethod::Head).unwrap();
        assert!(!head.body_present());
        assert_eq!(head.top_level_type(), None);
    }

    #[test]
    fn query_keys_are_sorted_but_duplicates_and_empty_keys_fail() {
        let observation = valid(ReadExperiment::ConversationList, ReadMethod::Get).unwrap();
        assert_eq!(
            observation.query_keys(),
            &["limit".to_owned(), "offset".to_owned()]
        );

        assert!(matches!(
            ReadObservation::new(
                "rev",
                ReadExperiment::ConversationList,
                ReadMethod::Get,
                "/backend-api/example",
                vec!["limit".to_owned(), "limit".to_owned()],
                200,
                "application/json",
                false,
                true,
                Some(JsonTopLevelType::Object),
            ),
            Err(ReadObservationError::DuplicateQueryKey(_))
        ));
        assert!(matches!(
            ReadObservation::new(
                "rev",
                ReadExperiment::ConversationList,
                ReadMethod::Get,
                "/backend-api/example",
                vec!["".to_owned()],
                200,
                "application/json",
                false,
                true,
                Some(JsonTopLevelType::Object),
            ),
            Err(ReadObservationError::EmptyQueryKey)
        ));
    }

    #[test]
    fn unsafe_or_impossible_read_metadata_is_rejected() {
        assert!(matches!(
            ReadObservation::new(
                "rev",
                ReadExperiment::ConversationList,
                ReadMethod::Get,
                "/other-api/example",
                vec![],
                200,
                "application/json",
                false,
                true,
                Some(JsonTopLevelType::Object),
            ),
            Err(ReadObservationError::PathOutsideBackendApi)
        ));
        assert!(matches!(
            ReadObservation::new(
                "rev",
                ReadExperiment::ConversationList,
                ReadMethod::Get,
                "/backend-api/example",
                vec![],
                200,
                "text/html",
                false,
                true,
                Some(JsonTopLevelType::Object),
            ),
            Err(ReadObservationError::NonJsonContentType)
        ));
        assert!(matches!(
            ReadObservation::new(
                "rev",
                ReadExperiment::OpenConversation,
                ReadMethod::Head,
                "/backend-api/example",
                vec![],
                200,
                "application/json",
                false,
                true,
                None,
            ),
            Err(ReadObservationError::HeadCannotHaveBody)
        ));
        assert!(matches!(
            ReadObservation::new(
                "rev",
                ReadExperiment::OpenConversation,
                ReadMethod::Get,
                "/backend-api/example",
                vec![],
                200,
                "application/json",
                true,
                true,
                Some(JsonTopLevelType::Object),
            ),
            Err(ReadObservationError::TruncatedBodyCannotClaimTopLevelType)
        ));
    }
}
