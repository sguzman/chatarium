//! Streaming interpretation for the text-turn response shape observed in
//! protocol snapshot 2026-09-29.002.
//!
//! The parser preserves transport uncertainty: unknown frames remain unknown and
//! completion evidence is tracked as separate signals rather than collapsed into one boolean.

use serde_json::Value;

/// One parsed Server-Sent Events frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseFrame {
    /// Optional SSE event name.
    pub event: Option<String>,
    /// Concatenated SSE data lines.
    pub data: String,
}

/// Error while interpreting a complete SSE frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseInterpretationError {
    /// Human-readable diagnostic.
    pub detail: String,
}

/// Incremental SSE frame decoder.
///
/// Browser response chunks are not assumed to align to SSE frame boundaries.
#[derive(Debug, Default, Clone)]
pub struct SseDecoder {
    buffer: String,
}

impl SseDecoder {
    /// Append decoded UTF-8 stream text and return every newly completed SSE frame.
    #[must_use]
    pub fn push(&mut self, text: &str) -> Vec<SseFrame> {
        self.buffer.push_str(text);
        self.buffer = self.buffer.replace("\r\n", "\n");

        let mut frames = Vec::new();
        while let Some(boundary) = self.buffer.find("\n\n") {
            let raw = self.buffer[..boundary].to_owned();
            self.buffer.drain(..boundary + 2);
            if let Some(frame) = parse_frame(&raw) {
                frames.push(frame);
            }
        }
        frames
    }

    /// Return the current unterminated tail, if any.
    #[must_use]
    pub fn pending(&self) -> &str {
        &self.buffer
    }
}

fn parse_frame(raw: &str) -> Option<SseFrame> {
    if raw.trim().is_empty() {
        return None;
    }

    let mut event = None;
    let mut data = Vec::new();

    for line in raw.split('\n') {
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            event = Some(value.trim().to_owned());
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push(value.strip_prefix(' ').unwrap_or(value).to_owned());
        }
    }

    if data.is_empty() {
        return None;
    }

    Some(SseFrame {
        event,
        data: data.join("\n"),
    })
}

/// A protocol observation derived from one v1 text-turn SSE frame.
#[derive(Debug, Clone, PartialEq)]
pub enum TextTurnEvent {
    /// Stream announced its delta encoding.
    DeltaEncoding(String),
    /// The stream echoed one user input message.
    UserInput {
        /// Remote message identifier, if present.
        message_id: Option<String>,
        /// Remote conversation identifier, if present.
        conversation_id: Option<String>,
        /// User-visible text.
        text: String,
    },
    /// The final-channel assistant message object became observable.
    FinalAssistantStart {
        /// Remote assistant message identifier.
        message_id: String,
        /// Remote conversation identifier, if present.
        conversation_id: Option<String>,
        /// Initial assistant text, often empty.
        text: String,
        /// Observed status.
        status: Option<String>,
        /// Observed end-turn flag.
        end_turn: Option<bool>,
    },
    /// Text was appended to the current message path.
    AssistantAppend {
        /// Observed patch path.
        path: String,
        /// Appended text.
        text: String,
    },
    /// A patch operation list was emitted.
    Patch {
        /// Observed patch operations.
        operations: Vec<PatchOperation>,
    },
    /// A message marker was emitted.
    MessageMarker {
        /// Marker name.
        marker: Option<String>,
        /// Marker event such as first/last.
        marker_event: Option<String>,
        /// Remote message identifier, if present.
        message_id: Option<String>,
        /// Remote conversation identifier, if present.
        conversation_id: Option<String>,
    },
    /// Server signaled message-stream completion.
    MessageStreamComplete {
        /// Remote conversation identifier, if present.
        conversation_id: Option<String>,
    },
    /// Other observed typed control frame.
    Control {
        /// Control-frame type.
        kind: String,
        /// Remote conversation identifier, if present.
        conversation_id: Option<String>,
    },
    /// Terminal SSE sentinel.
    Done,
    /// A syntactically valid frame not yet interpreted by this revision.
    Unknown {
        /// Optional SSE event name.
        event: Option<String>,
        /// Optional typed-control discriminator.
        kind: Option<String>,
    },
}

/// One observed patch operation.
#[derive(Debug, Clone, PartialEq)]
pub struct PatchOperation {
    /// Slash-delimited observed path.
    pub path: Option<String>,
    /// Operation name.
    pub operation: Option<String>,
    /// Raw operation value.
    pub value: Value,
}

/// Interpret one SSE frame according to snapshot 2026-09-29.002.
pub fn interpret_v1_frame(frame: &SseFrame) -> Result<TextTurnEvent, SseInterpretationError> {
    if frame.data == "[DONE]" {
        return Ok(TextTurnEvent::Done);
    }

    let payload: Value =
        serde_json::from_str(&frame.data).map_err(|error| SseInterpretationError {
            detail: format!("invalid JSON SSE data: {error}"),
        })?;

    if frame.event.as_deref() == Some("delta_encoding") {
        return payload
            .as_str()
            .map(|value| TextTurnEvent::DeltaEncoding(value.to_owned()))
            .ok_or_else(|| SseInterpretationError {
                detail: "delta_encoding data was not a string".to_owned(),
            });
    }

    if frame.event.as_deref() == Some("delta") {
        return interpret_delta(&payload);
    }

    let kind = payload
        .get("type")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let conversation_id = payload
        .get("conversation_id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);

    match kind.as_deref() {
        Some("input_message") => {
            let message = payload.get("input_message").unwrap_or(&Value::Null);
            if message.pointer("/author/role").and_then(Value::as_str) != Some("user") {
                return Ok(TextTurnEvent::Unknown {
                    event: frame.event.clone(),
                    kind,
                });
            }
            Ok(TextTurnEvent::UserInput {
                message_id: message
                    .get("id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                conversation_id,
                text: text_parts(message),
            })
        }
        Some("message_marker") => Ok(TextTurnEvent::MessageMarker {
            marker: payload
                .get("marker")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            marker_event: payload
                .get("event")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            message_id: payload
                .get("message_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            conversation_id,
        }),
        Some("message_stream_complete") => {
            Ok(TextTurnEvent::MessageStreamComplete { conversation_id })
        }
        Some(
            kind @ ("resume_conversation_token"
            | "title_generation"
            | "server_ste_metadata"
            | "conversation_detail_metadata"),
        ) => Ok(TextTurnEvent::Control {
            kind: kind.to_owned(),
            conversation_id,
        }),
        _ => Ok(TextTurnEvent::Unknown {
            event: frame.event.clone(),
            kind,
        }),
    }
}

fn interpret_delta(payload: &Value) -> Result<TextTurnEvent, SseInterpretationError> {
    if let Some(message) = payload.pointer("/v/message")
        && message.pointer("/author/role").and_then(Value::as_str) == Some("assistant")
        && message.get("channel").and_then(Value::as_str) == Some("final")
        && message
            .pointer("/content/content_type")
            .and_then(Value::as_str)
            == Some("text")
    {
        let Some(message_id) = message.get("id").and_then(Value::as_str) else {
            return Err(SseInterpretationError {
                detail: "final assistant message lacked id".to_owned(),
            });
        };
        return Ok(TextTurnEvent::FinalAssistantStart {
            message_id: message_id.to_owned(),
            conversation_id: payload
                .pointer("/v/conversation_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            text: text_parts(message),
            status: message
                .get("status")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            end_turn: message.get("end_turn").and_then(Value::as_bool),
        });
    }

    if payload.get("o").and_then(Value::as_str) == Some("append") {
        let path = payload
            .get("p")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if path == "/message/content/parts/0" {
            let Some(text) = payload.get("v").and_then(Value::as_str) else {
                return Err(SseInterpretationError {
                    detail: "assistant append value was not text".to_owned(),
                });
            };
            return Ok(TextTurnEvent::AssistantAppend {
                path,
                text: text.to_owned(),
            });
        }
    }

    if payload.get("o").and_then(Value::as_str) == Some("patch") {
        let operations = payload
            .get("v")
            .and_then(Value::as_array)
            .ok_or_else(|| SseInterpretationError {
                detail: "patch value was not an array".to_owned(),
            })?
            .iter()
            .map(|operation| PatchOperation {
                path: operation
                    .get("p")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                operation: operation
                    .get("o")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                value: operation.get("v").cloned().unwrap_or(Value::Null),
            })
            .collect();
        return Ok(TextTurnEvent::Patch { operations });
    }

    Ok(TextTurnEvent::Unknown {
        event: Some("delta".to_owned()),
        kind: None,
    })
}

fn text_parts(message: &Value) -> String {
    message
        .pointer("/content/parts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

/// Incrementally reconstructed state for one text turn.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TextTurnProjection {
    /// Conversation identifier observed in the stream.
    pub conversation_id: Option<String>,
    /// User message identifier echoed by the stream.
    pub user_message_id: Option<String>,
    /// Exact echoed user text.
    pub user_text: Option<String>,
    /// Final assistant message identifier.
    pub assistant_message_id: Option<String>,
    /// Incrementally reconstructed final assistant text.
    pub assistant_text: String,
    /// Last observed assistant status.
    pub assistant_status: Option<String>,
    /// Last observed end-turn value.
    pub assistant_end_turn: Option<bool>,
    /// Whether metadata positively observed is_complete=true.
    pub assistant_is_complete: bool,
    /// Whether message_stream_complete was observed.
    pub message_stream_complete: bool,
    /// Whether terminal [DONE] was observed.
    pub done: bool,
}

impl TextTurnProjection {
    /// Apply one interpreted event while preserving completion signals separately.
    pub fn apply(&mut self, event: &TextTurnEvent) {
        match event {
            TextTurnEvent::UserInput {
                message_id,
                conversation_id,
                text,
            } => {
                self.user_message_id.clone_from(message_id);
                self.user_text = Some(text.clone());
                if conversation_id.is_some() {
                    self.conversation_id.clone_from(conversation_id);
                }
            }
            TextTurnEvent::FinalAssistantStart {
                message_id,
                conversation_id,
                text,
                status,
                end_turn,
            } => {
                self.assistant_message_id = Some(message_id.clone());
                self.assistant_text.clone_from(text);
                self.assistant_status.clone_from(status);
                self.assistant_end_turn = *end_turn;
                if conversation_id.is_some() {
                    self.conversation_id.clone_from(conversation_id);
                }
            }
            TextTurnEvent::AssistantAppend { text, .. } => self.assistant_text.push_str(text),
            TextTurnEvent::Patch { operations } => {
                for operation in operations {
                    if operation.operation.as_deref() != Some("replace")
                        && operation.operation.as_deref() != Some("append")
                    {
                        continue;
                    }
                    match operation.path.as_deref() {
                        Some("/message/status") => {
                            self.assistant_status = operation.value.as_str().map(ToOwned::to_owned);
                        }
                        Some("/message/end_turn") => {
                            self.assistant_end_turn = operation.value.as_bool();
                        }
                        Some("/message/metadata") => {
                            if operation.value.get("is_complete").and_then(Value::as_bool)
                                == Some(true)
                            {
                                self.assistant_is_complete = true;
                            }
                        }
                        _ => {}
                    }
                }
            }
            TextTurnEvent::MessageStreamComplete { conversation_id } => {
                self.message_stream_complete = true;
                if conversation_id.is_some() {
                    self.conversation_id.clone_from(conversation_id);
                }
            }
            TextTurnEvent::Done => self.done = true,
            TextTurnEvent::DeltaEncoding(_)
            | TextTurnEvent::MessageMarker { .. }
            | TextTurnEvent::Control { .. }
            | TextTurnEvent::Unknown { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_one(raw: &str) -> SseFrame {
        let mut decoder = SseDecoder::default();
        let frames = decoder.push(raw);
        assert_eq!(frames.len(), 1);
        frames.into_iter().next().unwrap()
    }

    #[test]
    fn decoder_handles_browser_chunk_boundaries() {
        let mut decoder = SseDecoder::default();
        assert!(decoder.push("event: delta_encoding\ndata: \"v").is_empty());
        let frames = decoder.push("1\"\n\ndata: [DONE]\n\n");
        assert_eq!(
            frames,
            vec![
                SseFrame {
                    event: Some("delta_encoding".to_owned()),
                    data: "\"v1\"".to_owned(),
                },
                SseFrame {
                    event: None,
                    data: "[DONE]".to_owned(),
                },
            ]
        );
        assert!(decoder.pending().is_empty());
    }

    #[test]
    fn interprets_canonical_c03_lifecycle() {
        let raw = [
            "event: delta_encoding\ndata: \"v1\"\n\n",
            "data: {\"type\":\"input_message\",\"input_message\":{\"id\":\"u1\",\"author\":{\"role\":\"user\"},\"content\":{\"content_type\":\"text\",\"parts\":[\"respond with exactly CHATARIUM_PROTOCOL_TEST_001\"]},\"status\":\"finished_successfully\"},\"conversation_id\":\"c1\"}\n\n",
            "event: delta\ndata: {\"v\":{\"message\":{\"id\":\"a1\",\"author\":{\"role\":\"assistant\"},\"content\":{\"content_type\":\"text\",\"parts\":[\"\"]},\"status\":\"in_progress\",\"end_turn\":null,\"channel\":\"final\"},\"conversation_id\":\"c1\"},\"c\":12}\n\n",
            "event: delta\ndata: {\"p\":\"/message/content/parts/0\",\"o\":\"append\",\"v\":\"CHATARIUM_PROTOCOL_TEST_001\"}\n\n",
            "event: delta\ndata: {\"p\":\"\",\"o\":\"patch\",\"v\":[{\"p\":\"/message/status\",\"o\":\"replace\",\"v\":\"finished_successfully\"},{\"p\":\"/message/end_turn\",\"o\":\"replace\",\"v\":true},{\"p\":\"/message/metadata\",\"o\":\"append\",\"v\":{\"is_complete\":true}}]}\n\n",
            "data: {\"type\":\"message_stream_complete\",\"conversation_id\":\"c1\"}\n\n",
            "data: [DONE]\n\n",
        ]
        .concat();

        let mut decoder = SseDecoder::default();
        let mut projection = TextTurnProjection::default();
        for frame in decoder.push(&raw) {
            let event = interpret_v1_frame(&frame).unwrap();
            projection.apply(&event);
        }

        assert_eq!(projection.conversation_id.as_deref(), Some("c1"));
        assert_eq!(projection.user_message_id.as_deref(), Some("u1"));
        assert_eq!(
            projection.user_text.as_deref(),
            Some("respond with exactly CHATARIUM_PROTOCOL_TEST_001")
        );
        assert_eq!(projection.assistant_message_id.as_deref(), Some("a1"));
        assert_eq!(projection.assistant_text, "CHATARIUM_PROTOCOL_TEST_001");
        assert_eq!(
            projection.assistant_status.as_deref(),
            Some("finished_successfully")
        );
        assert_eq!(projection.assistant_end_turn, Some(true));
        assert!(projection.assistant_is_complete);
        assert!(projection.message_stream_complete);
        assert!(projection.done);
    }

    #[test]
    fn resume_control_does_not_expose_signed_value() {
        let frame = decode_one(
            "data: {\"type\":\"resume_conversation_token\",\"kind\":\"topic\",\"token\":\"secret-signed-value\",\"conversation_id\":\"c1\"}\n\n",
        );
        assert_eq!(
            interpret_v1_frame(&frame).unwrap(),
            TextTurnEvent::Control {
                kind: "resume_conversation_token".to_owned(),
                conversation_id: Some("c1".to_owned()),
            }
        );
    }

    #[test]
    fn malformed_json_is_explicit_error() {
        let frame = decode_one("event: delta\ndata: {nope}\n\n");
        assert!(interpret_v1_frame(&frame).is_err());
    }
}
