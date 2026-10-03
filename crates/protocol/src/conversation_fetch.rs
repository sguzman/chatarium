//! Evidence-gated semantic parsing for the validated C02 conversation-fetch response.

use crate::read::{
    LATEST_VALIDATED_CONVERSATION_FETCH_OBSERVATION, VALIDATED_CONVERSATION_FETCH_OBSERVATIONS,
};
use serde_json::{Map, Value};
use std::fmt;

/// Parsed conversation envelope for the validated C02 observation.
#[derive(Debug, Clone, PartialEq)]
pub struct ConversationFetchEnvelope {
    pub conversation_id: String,
    pub title: String,
    pub create_time: f64,
    pub update_time: f64,
    pub messages: Vec<ConversationMessage>,
    pub current_node: String,
    pub page_info: ConversationPageInfo,
}

/// One remote message in the observed conversation envelope.
#[derive(Debug, Clone, PartialEq)]
pub struct ConversationMessage {
    pub id: String,
    pub author: ConversationAuthor,
    pub create_time: f64,
    pub update_time: Option<f64>,
    pub content: ConversationMessageContent,
    pub status: String,
    pub end_turn: Option<bool>,
    pub weight: f64,
    pub metadata: Map<String, Value>,
    /// Optional parent relation exposed by the observed message metadata.
    ///
    /// The validated response does not provide this field for every message, so
    /// absence remains meaningful rather than being inferred from array order.
    pub parent_id: Option<String>,
    pub recipient: String,
    pub channel: Option<String>,
}

/// Author information attached to a remote message.
#[derive(Debug, Clone, PartialEq)]
pub struct ConversationAuthor {
    pub role: String,
    pub name: Option<String>,
    pub metadata: Map<String, Value>,
}

/// Content variants observed in the successful C02 response.
#[derive(Debug, Clone, PartialEq)]
pub enum ConversationMessageContent {
    Parts {
        content_type: String,
        parts: Vec<Value>,
    },
    Content {
        content_type: String,
        content: Value,
    },
    /// Reasoning summary payload observed in the 2026-10-03.001 Edge HAR.
    Thoughts {
        content_type: String,
        thoughts: Vec<Value>,
        source_analysis_msg_id: String,
    },
    /// Current-revision content whose structure is preserved exactly but not semantically
    /// interpreted by Chatarium yet.
    ///
    /// This keeps a valid exact mirror durable without inventing visible transcript semantics
    /// for newly observed system/tool/content variants.
    Opaque {
        content_type: String,
        fields: Map<String, Value>,
    },
}

/// Pagination metadata returned by the conversation fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationPageInfo {
    pub start_cursor: String,
    pub end_cursor: String,
    pub has_previous_page: bool,
    pub has_next_page: bool,
}

/// Fail-closed errors for C02 semantic parsing.
#[derive(Debug, Clone, PartialEq)]
pub enum ConversationFetchParseError {
    NoValidatedBaseline,
    UnsupportedRevision {
        observed: String,
        expected: String,
    },
    TopLevelNotObject,
    MissingField(String),
    WrongType {
        field: String,
        expected: &'static str,
    },
    EmptyField(String),
    IdentityMismatch {
        expected: String,
        observed: String,
    },
    InvalidContentShape {
        message_index: usize,
    },
}

impl fmt::Display for ConversationFetchParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoValidatedBaseline => {
                write!(f, "no validated C02 conversation-fetch baseline exists")
            }
            Self::UnsupportedRevision { observed, expected } => write!(
                f,
                "conversation-fetch revision {observed:?} is not validated baseline {expected:?}"
            ),
            Self::TopLevelNotObject => {
                write!(f, "conversation-fetch response must be a JSON object")
            }
            Self::MissingField(field) => write!(f, "missing required field {field:?}"),
            Self::WrongType { field, expected } => write!(f, "field {field:?} must be {expected}"),
            Self::EmptyField(field) => write!(f, "field {field:?} must not be empty"),
            Self::IdentityMismatch { expected, observed } => write!(
                f,
                "response identity {observed:?} does not match requested identity {expected:?}"
            ),
            Self::InvalidContentShape { message_index } => write!(
                f,
                "message {message_index} has an unsupported content shape"
            ),
        }
    }
}

impl std::error::Error for ConversationFetchParseError {}

/// Parse a successful C02 response for the exact validated observation revision.
///
/// Unmodeled remote fields are intentionally ignored. Message content and
/// metadata remain structural JSON; this function does not interpret them as
/// local transcript semantics.
pub fn parse_conversation_fetch_response(
    protocol_revision: &str,
    body: &Value,
    expected_remote_conversation_id: Option<&str>,
) -> Result<ConversationFetchEnvelope, ConversationFetchParseError> {
    let Some(expected_revision) = LATEST_VALIDATED_CONVERSATION_FETCH_OBSERVATION else {
        return Err(ConversationFetchParseError::NoValidatedBaseline);
    };
    if !VALIDATED_CONVERSATION_FETCH_OBSERVATIONS.contains(&protocol_revision) {
        return Err(ConversationFetchParseError::UnsupportedRevision {
            observed: protocol_revision.to_owned(),
            expected: expected_revision.to_owned(),
        });
    }

    let object = body
        .as_object()
        .ok_or(ConversationFetchParseError::TopLevelNotObject)?;
    let conversation_id = required_non_empty_string(object, "conversation_id")?.to_owned();
    if let Some(expected) = expected_remote_conversation_id {
        if expected != conversation_id {
            return Err(ConversationFetchParseError::IdentityMismatch {
                expected: expected.to_owned(),
                observed: conversation_id.to_owned(),
            });
        }
    }

    let title = required_string(object, "title")?.to_owned();
    let create_time = required_number(object, "create_time")?;
    let update_time = required_number(object, "update_time")?;
    let messages_value = required_field(object, "messages")?;
    let messages = messages_value
        .as_array()
        .ok_or_else(|| wrong_type("messages", "an array"))?
        .iter()
        .enumerate()
        .map(|(index, value)| parse_message(protocol_revision, index, value))
        .collect::<Result<Vec<_>, _>>()?;
    let current_node = required_non_empty_string(object, "current_node")?.to_owned();
    let page_info = parse_page_info(required_field(object, "page_info")?)?;

    Ok(ConversationFetchEnvelope {
        conversation_id,
        title,
        create_time,
        update_time,
        messages,
        current_node,
        page_info,
    })
}

fn parse_message(
    protocol_revision: &str,
    index: usize,
    value: &Value,
) -> Result<ConversationMessage, ConversationFetchParseError> {
    let object = value
        .as_object()
        .ok_or_else(|| wrong_type(&format!("messages[{index}]"), "an object"))?;
    let metadata = required_object_at(object, "metadata", index)?.clone();
    let parent_id = optional_parent_id(&metadata, index)?;
    Ok(ConversationMessage {
        id: required_non_empty_string(object, "id")?.to_owned(),
        author: parse_author(index, required_field(object, "author")?)?,
        create_time: required_number_at(object, "create_time", index)?,
        update_time: optional_number_at(object, "update_time", index)?,
        content: parse_content(protocol_revision, index, required_field(object, "content")?)?,
        status: required_string_at(object, "status", index)?.to_owned(),
        end_turn: optional_bool_at(object, "end_turn", index)?,
        weight: required_number_at(object, "weight", index)?,
        metadata,
        parent_id,
        recipient: required_string_at(object, "recipient", index)?.to_owned(),
        channel: optional_string_at(object, "channel", index)?,
    })
}

fn parse_author(
    index: usize,
    value: &Value,
) -> Result<ConversationAuthor, ConversationFetchParseError> {
    let object = value
        .as_object()
        .ok_or_else(|| wrong_type(&format!("messages[{index}].author"), "an object"))?;
    Ok(ConversationAuthor {
        role: required_string_at(object, "role", index)?.to_owned(),
        name: optional_string_at(object, "name", index)?,
        metadata: required_object_at(object, "metadata", index)?.clone(),
    })
}

fn parse_content(
    protocol_revision: &str,
    index: usize,
    value: &Value,
) -> Result<ConversationMessageContent, ConversationFetchParseError> {
    let object = value
        .as_object()
        .ok_or_else(|| wrong_type(&format!("messages[{index}].content"), "an object"))?;
    let content_type = required_string_at(object, "content_type", index)?.to_owned();
    match (
        object.contains_key("parts"),
        object.contains_key("content"),
        object.contains_key("thoughts"),
    ) {
        (true, false, false) => {
            let parts = object
                .get("parts")
                .and_then(Value::as_array)
                .ok_or_else(|| wrong_type(&format!("messages[{index}].content.parts"), "an array"))?
                .clone();
            Ok(ConversationMessageContent::Parts {
                content_type,
                parts,
            })
        }
        (false, true, false) => Ok(ConversationMessageContent::Content {
            content_type,
            content: object.get("content").expect("checked above").clone(),
        }),
        (false, false, true) if protocol_revision == "2026-10-03.001" => {
            let thoughts = object
                .get("thoughts")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    wrong_type(&format!("messages[{index}].content.thoughts"), "an array")
                })?
                .clone();
            let source_analysis_msg_id = object
                .get("source_analysis_msg_id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    wrong_type(
                        &format!("messages[{index}].content.source_analysis_msg_id"),
                        "a non-empty string",
                    )
                })?
                .to_owned();
            Ok(ConversationMessageContent::Thoughts {
                content_type,
                thoughts,
                source_analysis_msg_id,
            })
        }
        _ if protocol_revision == "2026-10-03.001" => {
            Ok(ConversationMessageContent::Opaque {
                content_type,
                fields: object.clone(),
            })
        }
        _ => Err(ConversationFetchParseError::InvalidContentShape {
            message_index: index,
        }),
    }
}

fn parse_page_info(value: &Value) -> Result<ConversationPageInfo, ConversationFetchParseError> {
    let object = value
        .as_object()
        .ok_or_else(|| wrong_type("page_info", "an object"))?;
    Ok(ConversationPageInfo {
        start_cursor: required_non_empty_string(object, "start_cursor")?.to_owned(),
        end_cursor: required_non_empty_string(object, "end_cursor")?.to_owned(),
        has_previous_page: required_bool_at_path(
            object,
            "has_previous_page",
            "page_info.has_previous_page",
        )?,
        has_next_page: required_bool_at_path(object, "has_next_page", "page_info.has_next_page")?,
    })
}

fn required_field<'a>(
    object: &'a Map<String, Value>,
    field: &str,
) -> Result<&'a Value, ConversationFetchParseError> {
    object
        .get(field)
        .ok_or_else(|| ConversationFetchParseError::MissingField(field.to_owned()))
}

fn required_string<'a>(
    object: &'a Map<String, Value>,
    field: &str,
) -> Result<&'a str, ConversationFetchParseError> {
    required_field(object, field)?
        .as_str()
        .ok_or_else(|| wrong_type(field, "a string"))
}

fn required_non_empty_string<'a>(
    object: &'a Map<String, Value>,
    field: &str,
) -> Result<&'a str, ConversationFetchParseError> {
    let value = required_string(object, field)?;
    if value.is_empty() {
        return Err(ConversationFetchParseError::EmptyField(field.to_owned()));
    }
    Ok(value)
}

fn required_number(
    object: &Map<String, Value>,
    field: &str,
) -> Result<f64, ConversationFetchParseError> {
    object
        .get(field)
        .and_then(Value::as_f64)
        .ok_or_else(|| wrong_type(field, "a number"))
}

fn required_number_at(
    object: &Map<String, Value>,
    field: &str,
    index: usize,
) -> Result<f64, ConversationFetchParseError> {
    object
        .get(field)
        .and_then(Value::as_f64)
        .ok_or_else(|| wrong_type(&format!("messages[{index}].{field}"), "a number"))
}

fn optional_number_at(
    object: &Map<String, Value>,
    field: &str,
    index: usize,
) -> Result<Option<f64>, ConversationFetchParseError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_f64()
            .map(Some)
            .ok_or_else(|| wrong_type(&format!("messages[{index}].{field}"), "a number or null")),
    }
}

fn required_string_at<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    index: usize,
) -> Result<&'a str, ConversationFetchParseError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| wrong_type(&format!("messages[{index}].{field}"), "a string"))
}

fn optional_string_at(
    object: &Map<String, Value>,
    field: &str,
    index: usize,
) -> Result<Option<String>, ConversationFetchParseError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(ToOwned::to_owned)
            .map(Some)
            .ok_or_else(|| wrong_type(&format!("messages[{index}].{field}"), "a string or null")),
    }
}

fn required_object_at<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    index: usize,
) -> Result<&'a Map<String, Value>, ConversationFetchParseError> {
    object
        .get(field)
        .and_then(Value::as_object)
        .ok_or_else(|| wrong_type(&format!("messages[{index}].{field}"), "an object"))
}

fn optional_parent_id(
    metadata: &Map<String, Value>,
    index: usize,
) -> Result<Option<String>, ConversationFetchParseError> {
    let field = format!("messages[{index}].metadata.parent_id");
    match metadata.get("parent_id") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.is_empty() => {
            Err(ConversationFetchParseError::EmptyField(field))
        }
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(wrong_type(&field, "a string or null")),
    }
}

fn optional_bool_at(
    object: &Map<String, Value>,
    field: &str,
    index: usize,
) -> Result<Option<bool>, ConversationFetchParseError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| wrong_type(&format!("messages[{index}].{field}"), "a boolean or null")),
    }
}

fn required_bool_at_path(
    object: &Map<String, Value>,
    field: &str,
    diagnostic_path: &str,
) -> Result<bool, ConversationFetchParseError> {
    object
        .get(field)
        .and_then(Value::as_bool)
        .ok_or_else(|| wrong_type(diagnostic_path, "a boolean"))
}

fn wrong_type(field: &str, expected: &'static str) -> ConversationFetchParseError {
    ConversationFetchParseError::WrongType {
        field: field.to_owned(),
        expected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn materialized_fixture() -> Value {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../protocol/fixtures/2026-10-01.001/c02-open-conversation.json"
        ))
        .expect("fixture JSON");
        let body = fixture
            .pointer("/read_responses/0/body")
            .expect("fixture body")
            .clone();

        fn materialize(value: Value) -> Value {
            match value {
                Value::Object(map) => {
                    Value::Object(map.into_iter().map(|(k, v)| (k, materialize(v))).collect())
                }
                Value::Array(items) => Value::Array(items.into_iter().map(materialize).collect()),
                Value::String(value) if value == "<empty-string>" => Value::String(String::new()),
                Value::String(value) if value == "<redacted-text>" => {
                    Value::String("fixture-redacted-text".to_owned())
                }
                Value::String(value) if value == "<string>" => {
                    Value::String("fixture-string".to_owned())
                }
                Value::String(value) if value == "<number>" => json!(1.0),
                Value::String(value) if value == "<bool>" => json!(true),
                Value::String(value) if value == "<url>" => {
                    Value::String("https://example.invalid/".to_owned())
                }
                Value::String(value) if value.starts_with("<id:") && value.ends_with('>') => {
                    let id = value.trim_start_matches("<id:").trim_end_matches('>');
                    Value::String(format!("fixture-id-{id}"))
                }
                other => other,
            }
        }

        materialize(body)
    }

    #[test]
    fn parses_committed_successful_c02_fixture_shape() {
        let parsed = parse_conversation_fetch_response(
            "2026-10-01.001",
            &materialized_fixture(),
            Some("fixture-id-1"),
        )
        .expect("validated C02 fixture should parse");
        assert_eq!(parsed.conversation_id, "fixture-id-1");
        assert_eq!(parsed.title, "fixture-redacted-text");
        assert_eq!(parsed.messages.len(), 5);
        assert_eq!(parsed.current_node, "fixture-id-10");
        assert_eq!(parsed.page_info.has_previous_page, true);
        assert_eq!(parsed.page_info.has_next_page, true);
        assert!(matches!(
            parsed.messages[0].content,
            ConversationMessageContent::Parts { .. }
        ));
        assert!(matches!(
            parsed.messages[3].content,
            ConversationMessageContent::Content { .. }
        ));
        assert_eq!(parsed.messages[0].parent_id, None);
        assert_eq!(
            parsed.messages[2].parent_id.as_deref(),
            Some("fixture-id-7")
        );
        assert_eq!(
            parsed.messages[4].parent_id.as_deref(),
            Some("fixture-id-8")
        );
    }

    fn materialized_latest_fixture() -> Value {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../protocol/fixtures/2026-10-03.001/c02-open-conversation.json"
        ))
        .expect("fixture JSON");
        let body = fixture
            .pointer("/read_responses/0/body")
            .expect("fixture body")
            .clone();

        fn materialize(value: Value) -> Value {
            match value {
                Value::Object(map) => {
                    Value::Object(map.into_iter().map(|(k, v)| (k, materialize(v))).collect())
                }
                Value::Array(items) => Value::Array(items.into_iter().map(materialize).collect()),
                Value::String(value) if value == "<empty-string>" => Value::String(String::new()),
                Value::String(value) if value == "<redacted-text>" => {
                    Value::String("fixture-redacted-text".to_owned())
                }
                Value::String(value) if value == "<string>" => {
                    Value::String("fixture-string".to_owned())
                }
                Value::String(value) if value == "<number>" => json!(1.0),
                Value::String(value) if value == "<bool>" => json!(true),
                Value::String(value) if value == "<url>" => {
                    Value::String("https://example.invalid/".to_owned())
                }
                Value::String(value) if value.starts_with("<id:") && value.ends_with('>') => {
                    let id = value.trim_start_matches("<id:").trim_end_matches('>');
                    Value::String(format!("fixture-id-{id}"))
                }
                other => other,
            }
        }

        materialize(body)
    }

    #[test]
    fn parses_latest_har_fixture_and_thoughts_variant() {
        let parsed = parse_conversation_fetch_response(
            "2026-10-03.001",
            &materialized_latest_fixture(),
            Some("fixture-id-7"),
        )
        .expect("latest C02 fixture should parse");
        assert_eq!(parsed.messages.len(), 4);
        assert!(matches!(
            parsed.messages[1].content,
            ConversationMessageContent::Thoughts { .. }
        ));
        assert_eq!(parsed.current_node, "fixture-id-6");
    }

    #[test]
    fn latest_revision_preserves_uninterpreted_content_shape_as_opaque() {
        let mut body = materialized_latest_fixture();
        body["messages"][1]["content"] = json!({
            "content_type": "future_private_content",
            "payload": {
                "shape": "not-yet-modeled"
            }
        });

        let parsed = parse_conversation_fetch_response(
            "2026-10-03.001",
            &body,
            Some("fixture-id-7"),
        )
        .expect("current live revision should preserve unknown content opaquely");

        assert!(matches!(
            &parsed.messages[1].content,
            ConversationMessageContent::Opaque { content_type, fields }
                if content_type == "future_private_content"
                    && fields.get("payload").is_some()
        ));
    }

    #[test]
    fn older_revision_does_not_retroactively_accept_thoughts_shape() {
        assert_eq!(
            parse_conversation_fetch_response(
                "2026-10-01.001",
                &materialized_latest_fixture(),
                None,
            ),
            Err(ConversationFetchParseError::InvalidContentShape { message_index: 1 })
        );
    }

    #[test]
    fn rejects_unvalidated_revision() {
        let error =
            parse_conversation_fetch_response("2026-09-30.002", &materialized_fixture(), None)
                .expect_err("historical rate-limited revision must not parse");
        assert_eq!(
            error,
            ConversationFetchParseError::UnsupportedRevision {
                observed: "2026-09-30.002".to_owned(),
                expected: "2026-10-03.001".to_owned(),
            }
        );
    }

    #[test]
    fn rejects_identity_mismatch() {
        let error = parse_conversation_fetch_response(
            "2026-10-01.001",
            &materialized_fixture(),
            Some("different-remote"),
        )
        .expect_err("response identity must match requested identity");
        assert_eq!(
            error,
            ConversationFetchParseError::IdentityMismatch {
                expected: "different-remote".to_owned(),
                observed: "fixture-id-1".to_owned(),
            }
        );
    }

    #[test]
    fn ignores_unmodeled_top_level_fields() {
        let mut body = materialized_fixture();
        body.as_object_mut()
            .unwrap()
            .insert("future_server_field".to_owned(), json!({"new": true}));
        assert!(parse_conversation_fetch_response("2026-10-01.001", &body, None).is_ok());
    }

    #[test]
    fn rejects_missing_required_envelope_field() {
        let mut body = materialized_fixture();
        body.as_object_mut().unwrap().remove("messages");
        assert_eq!(
            parse_conversation_fetch_response("2026-10-01.001", &body, None),
            Err(ConversationFetchParseError::MissingField(
                "messages".to_owned()
            ))
        );
    }

    #[test]
    fn rejects_ambiguous_message_content_shape() {
        let mut body = materialized_fixture();
        body.pointer_mut("/messages/0/content")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("content".to_owned(), Value::String("both".to_owned()));
        assert_eq!(
            parse_conversation_fetch_response("2026-10-01.001", &body, None),
            Err(ConversationFetchParseError::InvalidContentShape { message_index: 0 })
        );
    }

    #[test]
    fn rejects_wrong_page_info_shape() {
        let mut body = materialized_fixture();
        body.pointer_mut("/page_info/has_next_page").unwrap().take();
        assert!(matches!(
            parse_conversation_fetch_response("2026-10-01.001", &body, None),
            Err(ConversationFetchParseError::WrongType { field, .. }) if field == "page_info.has_next_page"
        ));
    }
}
