//! Bounded ASCIIZ framing and Windows DevTools pipe transport.

use crate::transport::TransportError;

/// Maximum bytes in one unterminated or complete CDP pipe message.
pub const MAX_PIPE_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

/// Incremental NUL-delimited UTF-8 decoder used by Chromium's ASCIIZ pipe mode.
#[derive(Debug, Default)]
pub struct AsciizDecoder {
    pending: Vec<u8>,
}

impl AsciizDecoder {
    /// Add an arbitrary byte chunk and return each complete NUL-terminated JSON text message.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, TransportError> {
        let mut messages = Vec::new();
        for chunk in bytes.split_inclusive(|byte| *byte == 0) {
            let delimited = chunk.last() == Some(&0);
            let content = if delimited {
                &chunk[..chunk.len() - 1]
            } else {
                chunk
            };
            if self.pending.len().saturating_add(content.len()) > MAX_PIPE_MESSAGE_BYTES {
                return Err(TransportError::MalformedMessage(format!(
                    "ASCIIZ CDP message exceeds {MAX_PIPE_MESSAGE_BYTES} bytes"
                )));
            }
            self.pending.extend_from_slice(content);
            if delimited {
                let bytes = std::mem::take(&mut self.pending);
                let text = String::from_utf8(bytes).map_err(|error| {
                    TransportError::MalformedMessage(format!(
                        "ASCIIZ CDP message is not valid UTF-8: {error}"
                    ))
                })?;
                messages.push(text);
            }
        }
        Ok(messages)
    }

    /// Validate that EOF did not cut off a message after the last delimiter.
    pub fn finish(&self) -> Result<(), TransportError> {
        if self.pending.is_empty() {
            Ok(())
        } else {
            Err(TransportError::Eof {
                unterminated_message: true,
            })
        }
    }

    /// Whether EOF would cut off a message.
    #[must_use]
    pub fn has_pending_bytes(&self) -> bool {
        !self.pending.is_empty()
    }
}

/// Encode one raw JSON message using Chromium's NUL-terminated ASCIIZ framing.
pub fn encode_asciiz_message(json: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(json.len() + 1);
    bytes.extend_from_slice(json.as_bytes());
    bytes.push(0);
    bytes
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;

#[cfg(windows)]
pub use windows::{PipeCdpBrowserTransport, WindowsPipePair, spawn_edge_with_pipe};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{CdpMessageChannel, CdpPageSession, PageSession};
    use serde_json::{Value, json};
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    struct MockAsciizChannel {
        incoming: VecDeque<Vec<u8>>,
        pending: VecDeque<String>,
        decoder: AsciizDecoder,
        outgoing: Arc<Mutex<Vec<Vec<u8>>>>,
        timeout: bool,
    }

    impl CdpMessageChannel for MockAsciizChannel {
        fn send_message(&mut self, text: &str) -> Result<(), TransportError> {
            self.outgoing
                .lock()
                .unwrap()
                .push(encode_asciiz_message(text));
            Ok(())
        }
        fn receive_message(
            &mut self,
            _timeout: Duration,
        ) -> Result<Option<String>, TransportError> {
            if let Some(message) = self.pending.pop_front() {
                return Ok(Some(message));
            }
            if self.timeout {
                return Ok(None);
            }
            while let Some(chunk) = self.incoming.pop_front() {
                self.pending.extend(self.decoder.push(&chunk)?);
                if let Some(message) = self.pending.pop_front() {
                    return Ok(Some(message));
                }
            }
            Ok(None)
        }
        fn close(&mut self) -> Result<(), TransportError> {
            Ok(())
        }
    }

    fn mock_pipe(messages: &[&str]) -> (MockAsciizChannel, Arc<Mutex<Vec<Vec<u8>>>>) {
        let outgoing = Arc::new(Mutex::new(Vec::new()));
        let chunks = messages
            .iter()
            .flat_map(|message| {
                let mut bytes = message.as_bytes().to_vec();
                bytes.push(0);
                bytes.chunks(5).map(<[u8]>::to_vec).collect::<Vec<_>>()
            })
            .collect();
        (
            MockAsciizChannel {
                incoming: chunks,
                pending: VecDeque::new(),
                decoder: AsciizDecoder::default(),
                outgoing: outgoing.clone(),
                timeout: false,
            },
            outgoing,
        )
    }

    #[test]
    fn outbound_json_has_exactly_one_trailing_nul() {
        assert_eq!(
            encode_asciiz_message(r#"{"id":1,"method":"Browser.getVersion","params":{}}"#).last(),
            Some(&0)
        );
        assert_eq!(encode_asciiz_message("{}"), vec![b'{', b'}', 0]);
    }

    #[test]
    fn partial_message_is_accumulated_across_reads() {
        let mut decoder = AsciizDecoder::default();
        assert!(decoder.push(br#"{"id":1,"met"#).unwrap().is_empty());
        assert_eq!(
            decoder.push(b"hod\":\"A\"}\0").unwrap(),
            vec![r#"{"id":1,"method":"A"}"#]
        );
        assert!(decoder.finish().is_ok());
    }

    #[test]
    fn multiple_messages_and_delimiter_at_chunk_boundary_are_preserved() {
        let mut decoder = AsciizDecoder::default();
        assert_eq!(decoder.push(b"{}\0{\"id\":2}").unwrap(), vec!["{}"]);
        assert_eq!(decoder.push(b"\0").unwrap(), vec![r#"{"id":2}"#]);
        assert!(decoder.finish().is_ok());
    }

    #[test]
    fn malformed_utf8_is_visible() {
        let mut decoder = AsciizDecoder::default();
        assert!(matches!(
            decoder.push(&[0xff, 0]),
            Err(TransportError::MalformedMessage(_))
        ));
    }

    #[test]
    fn eof_before_delimiter_is_distinct_from_clean_eof_after_message() {
        let mut partial = AsciizDecoder::default();
        partial.push(b"partial").unwrap();
        assert_eq!(
            partial.finish(),
            Err(TransportError::Eof {
                unterminated_message: true
            })
        );

        let mut complete = AsciizDecoder::default();
        assert_eq!(complete.push(b"{}\0").unwrap(), vec!["{}"]);
        assert!(complete.finish().is_ok());
    }

    #[test]
    fn unterminated_message_is_bounded() {
        let mut decoder = AsciizDecoder::default();
        decoder.pending = vec![b'x'; MAX_PIPE_MESSAGE_BYTES];
        assert!(matches!(
            decoder.push(b"x"),
            Err(TransportError::MalformedMessage(_))
        ));
    }

    #[test]
    fn raw_cdp_correlation_and_unsolicited_events_work_over_asciiz_pipe() {
        let messages = [
            r#"{"method":"Target.targetCreated","params":{"targetInfo":{"targetId":"t"}}}"#,
            r#"{"id":91,"result":{"ignored":true}}"#,
            r#"{"id":1,"result":{"product":"Edge","protocolVersion":"1.3"}}"#,
        ];
        let (channel, outgoing) = mock_pipe(&messages);
        let mut session = CdpPageSession::with_channel(Box::new(channel));
        let result = session
            .command("Browser.getVersion", json!({}), Duration::from_secs(1))
            .unwrap();
        assert_eq!(result["product"], "Edge");
        let event = session.next_event(Duration::from_secs(1)).unwrap().unwrap();
        assert_eq!(event.method, "Target.targetCreated");
        assert_eq!(event.params["targetInfo"]["targetId"], "t");
        let bytes = outgoing.lock().unwrap();
        assert_eq!(bytes.len(), 1);
        assert_eq!(bytes[0].last(), Some(&0));
        assert_eq!(bytes[0].iter().filter(|byte| **byte == 0).count(), 1);
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes[0][..bytes[0].len() - 1]).unwrap()["id"],
            1
        );
    }

    #[test]
    fn pipe_cdp_timeout_and_eof_remain_distinguishable() {
        let outgoing = Arc::new(Mutex::new(Vec::new()));
        let channel = MockAsciizChannel {
            incoming: VecDeque::new(),
            pending: VecDeque::new(),
            decoder: AsciizDecoder::default(),
            outgoing: outgoing.clone(),
            timeout: true,
        };
        let mut session = CdpPageSession::with_channel(Box::new(channel));
        assert!(
            matches!(session.command("Browser.getVersion", json!({}), Duration::from_millis(1)), Err(TransportError::CommandOutcomeUnknown { reason, .. }) if reason.contains("timed out"))
        );

        let mut decoder = AsciizDecoder::default();
        assert!(decoder.push(b"{\"partial\"").unwrap().is_empty());
        assert_eq!(
            decoder.finish(),
            Err(TransportError::Eof {
                unterminated_message: true
            })
        );
    }
}
