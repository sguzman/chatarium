//! Evidence-gated read-side protocol observations for P3.
//!
//! This module describes the publication-safe structural metadata exposed by the
//! Flight Recorder read boundary. Historical v0.7.0/v0.7.1 observations know
//! query-key names only. v0.7.2 may additionally preserve narrowly approved C02
//! query values whose grammar cannot carry arbitrary credential/text material.
//! Semantic support still requires committed controlled evidence.

use crate::Compatibility;
use std::collections::BTreeSet;
use std::fmt;

/// Controlled C01 evidence exists at `2026-09-30.001`, but it does not establish complete
/// conversation enumeration, so no semantic baseline exists yet.
pub const LATEST_VALIDATED_CONVERSATION_LIST_OBSERVATION: Option<&str> = None;

/// The first successful controlled C02 conversation-fetch response is validated by snapshot `2026-10-01.001`.
pub const LATEST_VALIDATED_CONVERSATION_FETCH_OBSERVATION: Option<&str> = Some("2026-10-01.001");

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

/// C02 query keys whose values may be retained by the narrow v0.7.2 evidence boundary.
pub const APPROVED_C02_QUERY_KEYS: [&str; 2] = ["include_has_versions", "num_turns"];

/// Publication-safe value evidence for one approved C02 query parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadQueryValue {
    /// The parameter was present with an empty value.
    Empty,
    /// Exact ASCII boolean literal.
    Boolean(bool),
    /// Canonical bounded ASCII integer text.
    Integer(String),
    /// The parameter existed but its value did not match the safe literal grammar.
    Redacted,
}

impl ReadQueryValue {
    /// Stable representation kind used by sanitized fixtures and durable audit records.
    #[must_use]
    pub const fn stable_kind(&self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Boolean(_) => "boolean",
            Self::Integer(_) => "integer",
            Self::Redacted => "redacted",
        }
    }
}

/// One occurrence of an approved C02 query parameter.
///
/// Order and repeated occurrences are preserved because they are empirical request
/// semantics. This is separate from query_keys, which remains the historical
/// de-duplicated structural view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadQueryParameter {
    key: String,
    value: ReadQueryValue,
}

impl ReadQueryParameter {
    /// Construct one typed parameter occurrence. Full context validation happens
    /// when the parameter is attached to a ReadObservation.
    #[must_use]
    pub fn new(key: impl Into<String>, value: ReadQueryValue) -> Self {
        Self {
            key: key.into(),
            value,
        }
    }

    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    #[must_use]
    pub const fn value(&self) -> &ReadQueryValue {
        &self.value
    }
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
    QueryParametersOutsideConversationFetch,
    UnsupportedQueryParameterKey(String),
    QueryParameterKeyNotObserved(String),
    InvalidQueryParameterInteger(String),
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
            Self::QueryParametersOutsideConversationFetch => write!(
                formatter,
                "approved query-value evidence is allowed only for C02 conversation fetches"
            ),
            Self::UnsupportedQueryParameterKey(key) => write!(
                formatter,
                "query-value evidence uses unsupported key {key:?}"
            ),
            Self::QueryParameterKeyNotObserved(key) => write!(
                formatter,
                "query-value evidence key {key:?} is absent from observed query keys"
            ),
            Self::InvalidQueryParameterInteger(key) => write!(
                formatter,
                "query-value evidence for key {key:?} is not a canonical bounded ASCII integer"
            ),
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
/// material, arbitrary query value, or request body belongs in this type.
/// Optional query-value evidence is limited to APPROVED_C02_QUERY_KEYS and
/// the closed ReadQueryValue grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadObservation {
    protocol_revision: String,
    experiment: ReadExperiment,
    method: ReadMethod,
    path: String,
    query_keys: Vec<String>,
    query_parameters: Option<Vec<ReadQueryParameter>>,
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
        Self::new_with_query_parameters(
            protocol_revision,
            experiment,
            method,
            path,
            query_keys,
            None,
            status,
            content_type,
            truncated,
            body_present,
            top_level_type,
        )
    }

    /// Construct a read observation with optional narrowly approved C02
    /// query-value evidence. None means the historical capture did not observe
    /// values; Some(empty) means a value-capable capture observed no approved
    /// parameter occurrences.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_query_parameters(
        protocol_revision: impl Into<String>,
        experiment: ReadExperiment,
        method: ReadMethod,
        path: impl Into<String>,
        query_keys: Vec<String>,
        query_parameters: Option<Vec<ReadQueryParameter>>,
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

        if let Some(parameters) = query_parameters.as_ref() {
            if experiment != ReadExperiment::OpenConversation || !conversation_fetch_path(&path) {
                return Err(ReadObservationError::QueryParametersOutsideConversationFetch);
            }
            for parameter in parameters {
                if !APPROVED_C02_QUERY_KEYS.contains(&parameter.key.as_str()) {
                    return Err(ReadObservationError::UnsupportedQueryParameterKey(
                        parameter.key.clone(),
                    ));
                }
                if !query_keys.iter().any(|key| key == &parameter.key) {
                    return Err(ReadObservationError::QueryParameterKeyNotObserved(
                        parameter.key.clone(),
                    ));
                }
                if let ReadQueryValue::Integer(value) = &parameter.value {
                    if !valid_safe_query_integer(value) {
                        return Err(ReadObservationError::InvalidQueryParameterInteger(
                            parameter.key.clone(),
                        ));
                    }
                }
            }
        }

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
            query_parameters,
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

    /// Exact approved parameter occurrences when the capture supported value
    /// evidence. Historical captures return None.
    #[must_use]
    pub fn query_parameters(&self) -> Option<&[ReadQueryParameter]> {
        self.query_parameters.as_deref()
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

fn conversation_fetch_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/backend-api/conversations/") else {
        return false;
    };
    !rest.is_empty() && !rest.contains('/')
}

fn valid_safe_query_integer(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > 11 || !bytes.is_ascii() {
        return false;
    }

    let digits = if bytes[0] == b'-' {
        if bytes.len() == 1 {
            return false;
        }
        &bytes[1..]
    } else {
        bytes
    };
    if digits.len() > 10 || !digits.iter().all(u8::is_ascii_digit) {
        return false;
    }
    if digits.len() > 1 && digits[0] == b'0' {
        return false;
    }
    if bytes[0] == b'-' && digits == b"0" {
        return false;
    }
    true
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
    fn c01_remains_unbaselined_while_c02_uses_the_successful_fetch_revision() {
        assert_eq!(
            compatibility_for_read_flow(ReadFlow::ConversationList, "2026-09-30.001"),
            Compatibility::NoBaseline
        );
        assert_eq!(
            compatibility_for_read_flow(ReadFlow::ConversationFetch, "2026-10-01.001"),
            Compatibility::ValidatedAgainst("2026-10-01.001".to_owned())
        );
        assert!(matches!(
            compatibility_for_read_flow(ReadFlow::ConversationFetch, "2026-09-30.002"),
            Compatibility::Mismatch { expected_revision, .. } if expected_revision == "2026-10-01.001"
        ));
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
    fn approved_c02_query_parameters_preserve_order_and_repetition() {
        let parameters = vec![
            ReadQueryParameter::new("include_has_versions", ReadQueryValue::Boolean(true)),
            ReadQueryParameter::new("num_turns", ReadQueryValue::Integer("33".to_owned())),
            ReadQueryParameter::new("num_turns", ReadQueryValue::Integer("64".to_owned())),
        ];
        let observation = ReadObservation::new_with_query_parameters(
            "future-c02-observation",
            ReadExperiment::OpenConversation,
            ReadMethod::Get,
            "/backend-api/conversations/<id>",
            vec!["include_has_versions".to_owned(), "num_turns".to_owned()],
            Some(parameters.clone()),
            200,
            "application/json",
            false,
            true,
            Some(JsonTopLevelType::Object),
        )
        .unwrap();

        assert_eq!(observation.query_parameters(), Some(parameters.as_slice()));
    }

    #[test]
    fn historical_observation_keeps_query_values_unknown() {
        let observation = ReadObservation::new(
            "2026-10-01.001",
            ReadExperiment::OpenConversation,
            ReadMethod::Get,
            "/backend-api/conversations/<id>",
            vec!["include_has_versions".to_owned(), "num_turns".to_owned()],
            200,
            "application/json",
            false,
            true,
            Some(JsonTopLevelType::Object),
        )
        .unwrap();

        assert_eq!(observation.query_parameters(), None);
    }

    #[test]
    fn query_value_evidence_fails_closed_outside_narrow_c02_boundary() {
        let parameter =
            ReadQueryParameter::new("num_turns", ReadQueryValue::Integer("33".to_owned()));

        assert!(matches!(
            ReadObservation::new_with_query_parameters(
                "rev",
                ReadExperiment::ConversationList,
                ReadMethod::Get,
                "/backend-api/gizmos/snorlax/sidebar",
                vec!["num_turns".to_owned()],
                Some(vec![parameter.clone()]),
                200,
                "application/json",
                false,
                true,
                Some(JsonTopLevelType::Object),
            ),
            Err(ReadObservationError::QueryParametersOutsideConversationFetch)
        ));
        assert!(matches!(
            ReadObservation::new_with_query_parameters(
                "rev",
                ReadExperiment::OpenConversation,
                ReadMethod::Get,
                "/backend-api/conversations/<id>",
                vec!["num_turns".to_owned()],
                Some(vec![ReadQueryParameter::new(
                    "secret",
                    ReadQueryValue::Redacted,
                )]),
                200,
                "application/json",
                false,
                true,
                Some(JsonTopLevelType::Object),
            ),
            Err(ReadObservationError::UnsupportedQueryParameterKey(_))
        ));
        assert!(matches!(
            ReadObservation::new_with_query_parameters(
                "rev",
                ReadExperiment::OpenConversation,
                ReadMethod::Get,
                "/backend-api/conversations/<id>",
                vec!["num_turns".to_owned()],
                Some(vec![ReadQueryParameter::new(
                    "include_has_versions",
                    ReadQueryValue::Boolean(true),
                )]),
                200,
                "application/json",
                false,
                true,
                Some(JsonTopLevelType::Object),
            ),
            Err(ReadObservationError::QueryParameterKeyNotObserved(_))
        ));
        assert!(matches!(
            ReadObservation::new_with_query_parameters(
                "rev",
                ReadExperiment::OpenConversation,
                ReadMethod::Get,
                "/backend-api/conversations/<id>",
                vec!["num_turns".to_owned()],
                Some(vec![ReadQueryParameter::new(
                    "num_turns",
                    ReadQueryValue::Integer("00033".to_owned()),
                )]),
                200,
                "application/json",
                false,
                true,
                Some(JsonTopLevelType::Object),
            ),
            Err(ReadObservationError::InvalidQueryParameterInteger(_))
        ));
    }

    #[test]
    fn redacted_approved_query_parameter_retains_presence_without_raw_value() {
        let observation = ReadObservation::new_with_query_parameters(
            "rev",
            ReadExperiment::OpenConversation,
            ReadMethod::Get,
            "/backend-api/conversations/<id>",
            vec!["include_has_versions".to_owned()],
            Some(vec![ReadQueryParameter::new(
                "include_has_versions",
                ReadQueryValue::Redacted,
            )]),
            200,
            "application/json",
            false,
            true,
            Some(JsonTopLevelType::Object),
        )
        .unwrap();
        assert!(matches!(
            observation.query_parameters().unwrap()[0].value(),
            ReadQueryValue::Redacted
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
