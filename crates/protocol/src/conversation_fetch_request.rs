//! Evidence-backed construction of the observed C02 conversation-fetch resource.
//!
//! This module contains no authentication behavior. It only freezes the exact
//! path/query shape observed in the 2026-10-03.001 Edge HAR so runtime providers
//! do not invent query values.

use std::fmt;
use std::fmt::Write as _;

/// Observation that established the exact safe C02 query literals and order.
pub const CONVERSATION_FETCH_REQUEST_OBSERVATION: &str = "2026-10-03.001";

/// Exact occurrence-ordered query literals observed on the successful C02 GET.
pub const CONVERSATION_FETCH_QUERY_PARAMETERS: [(&str, &str); 2] =
    [("num_turns", "10"), ("include_has_versions", "true")];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationFetchRequestError {
    EmptyRemoteConversationId,
}

impl fmt::Display for ConversationFetchRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRemoteConversationId => {
                write!(formatter, "remote conversation identity must not be empty")
            }
        }
    }
}

impl std::error::Error for ConversationFetchRequestError {}

/// Build the relative C02 resource exactly as observed, with the opaque remote
/// identity encoded as one URL path segment.
pub fn conversation_fetch_resource(
    remote_conversation_id: &str,
) -> Result<String, ConversationFetchRequestError> {
    if remote_conversation_id.is_empty() {
        return Err(ConversationFetchRequestError::EmptyRemoteConversationId);
    }

    let encoded_id = percent_encode_path_segment(remote_conversation_id);
    Ok(format!(
        "/backend-api/conversations/{encoded_id}?num_turns=10&include_has_versions=true"
    ))
}

fn percent_encode_path_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            write!(&mut encoded, "%{byte:02X}").expect("writing into String cannot fail");
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freezes_observed_path_query_values_and_order() {
        assert_eq!(
            conversation_fetch_resource("6ac06718-a3f4-83e8-b001-b5ece9ad001f").unwrap(),
            "/backend-api/conversations/6ac06718-a3f4-83e8-b001-b5ece9ad001f?num_turns=10&include_has_versions=true"
        );
        assert_eq!(
            CONVERSATION_FETCH_QUERY_PARAMETERS,
            [("num_turns", "10"), ("include_has_versions", "true")]
        );
        assert_eq!(CONVERSATION_FETCH_REQUEST_OBSERVATION, "2026-10-03.001");
    }

    #[test]
    fn opaque_identity_is_one_encoded_path_segment() {
        assert_eq!(
            conversation_fetch_resource("opaque/remote id?x=%").unwrap(),
            "/backend-api/conversations/opaque%2Fremote%20id%3Fx%3D%25?num_turns=10&include_has_versions=true"
        );
    }

    #[test]
    fn empty_remote_identity_fails_closed() {
        assert_eq!(
            conversation_fetch_resource(""),
            Err(ConversationFetchRequestError::EmptyRemoteConversationId)
        );
    }
}
