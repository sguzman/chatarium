//! Evidence-gated first page of the ordinary ChatGPT account conversation list.

use serde_json::Value;
use std::fmt;

/// Composite observation revision for the currently supported ordinary-history page.
pub const CONVERSATION_LIST_OBSERVATION: &str = "2026-10-03.002";

/// Exact first-page resource observed in the user's Edge capture.
pub const CONVERSATION_LIST_FIRST_PAGE_RESOURCE: &str = "/backend-api/conversations?exclude_conversation_origin=tpp&expand=false&hide_snorlax=false&is_archived=false&is_starred=false&limit=20&order=updated&offset=0";

/// One account-history summary.
#[derive(Debug, Clone, PartialEq)]
pub struct ConversationListItem {
    pub id: String,
    pub title: Option<String>,
    pub create_time: Option<Value>,
    pub update_time: Option<Value>,
}

/// Parsed first account-history page.
#[derive(Debug, Clone, PartialEq)]
pub struct ConversationListPage {
    pub items: Vec<ConversationListItem>,
    pub total: u64,
    pub limit: u64,
    pub offset: u64,
}

/// Fail-closed parse errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversationListParseError {
    TopLevelNotObject,
    MissingField(String),
    InvalidField(String),
    InvalidItem(usize),
    InvalidItemIdentity(usize),
    UnsupportedPage { limit: u64, offset: u64 },
}

impl fmt::Display for ConversationListParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TopLevelNotObject => write!(formatter, "conversation list must be a JSON object"),
            Self::MissingField(field) => {
                write!(formatter, "conversation list is missing {field:?}")
            }
            Self::InvalidField(field) => write!(
                formatter,
                "conversation list field {field:?} has an unsupported value"
            ),
            Self::InvalidItem(index) => {
                write!(formatter, "conversation list item {index} is not an object")
            }
            Self::InvalidItemIdentity(index) => write!(
                formatter,
                "conversation list item {index} has no usable remote id"
            ),
            Self::UnsupportedPage { limit, offset } => write!(
                formatter,
                "conversation list page metadata limit={limit} offset={offset} is outside the evidenced first-page profile"
            ),
        }
    }
}

impl std::error::Error for ConversationListParseError {}

/// Parse the only currently supported account-history page.
pub fn parse_conversation_list_first_page(
    body: &Value,
) -> Result<ConversationListPage, ConversationListParseError> {
    let object = body
        .as_object()
        .ok_or(ConversationListParseError::TopLevelNotObject)?;
    let items = object
        .get("items")
        .ok_or_else(|| ConversationListParseError::MissingField("items".to_owned()))?
        .as_array()
        .ok_or_else(|| ConversationListParseError::InvalidField("items".to_owned()))?;
    let total = required_u64(object.get("total"), "total")?;
    let limit = required_u64(object.get("limit"), "limit")?;
    let offset = required_u64(object.get("offset"), "offset")?;
    if limit != 20 || offset != 0 {
        return Err(ConversationListParseError::UnsupportedPage { limit, offset });
    }
    if total < items.len() as u64 {
        return Err(ConversationListParseError::InvalidField("total".to_owned()));
    }

    let mut parsed = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let item = item
            .as_object()
            .ok_or(ConversationListParseError::InvalidItem(index))?;
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or(ConversationListParseError::InvalidItemIdentity(index))?
            .to_owned();
        let title = match item.get("title") {
            None | Some(Value::Null) => None,
            Some(Value::String(title)) => Some(title.clone()),
            Some(_) => {
                return Err(ConversationListParseError::InvalidField(format!(
                    "items[{index}].title"
                )));
            }
        };

        parsed.push(ConversationListItem {
            id,
            title,
            create_time: item.get("create_time").cloned(),
            update_time: item.get("update_time").cloned(),
        });
    }

    Ok(ConversationListPage {
        items: parsed,
        total,
        limit,
        offset,
    })
}

fn required_u64(value: Option<&Value>, field: &str) -> Result<u64, ConversationListParseError> {
    value.and_then(Value::as_u64).ok_or_else(|| match value {
        None => ConversationListParseError::MissingField(field.to_owned()),
        Some(_) => ConversationListParseError::InvalidField(field.to_owned()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn freezes_exact_observed_first_page_resource() {
        assert_eq!(
            CONVERSATION_LIST_FIRST_PAGE_RESOURCE,
            "/backend-api/conversations?exclude_conversation_origin=tpp&expand=false&hide_snorlax=false&is_archived=false&is_starred=false&limit=20&order=updated&offset=0"
        );
    }

    #[test]
    fn parses_minimal_success_shape() {
        let page = parse_conversation_list_first_page(&json!({
            "items": [
                {
                    "id": "remote-1",
                    "title": "First",
                    "create_time": "2026-09-30T18:32:58Z",
                    "update_time": "2026-09-30T21:04:06Z"
                },
                {
                    "id": "remote-2",
                    "title": null,
                    "create_time": 1.0,
                    "update_time": 2.0
                }
            ],
            "total": 21,
            "limit": 20,
            "offset": 0
        }))
        .expect("supported page");

        assert_eq!(page.items.len(), 2);
        assert_eq!(page.items[0].id, "remote-1");
        assert_eq!(page.items[0].title.as_deref(), Some("First"));
        assert_eq!(page.total, 21);
    }

    #[test]
    fn rejects_guessed_pagination_page() {
        assert_eq!(
            parse_conversation_list_first_page(&json!({
                "items": [],
                "total": 21,
                "limit": 20,
                "offset": 20
            })),
            Err(ConversationListParseError::UnsupportedPage {
                limit: 20,
                offset: 20
            })
        );
    }

    #[test]
    fn rejects_empty_remote_identity() {
        assert_eq!(
            parse_conversation_list_first_page(&json!({
                "items": [{"id": "", "title": "bad"}],
                "total": 1,
                "limit": 20,
                "offset": 0
            })),
            Err(ConversationListParseError::InvalidItemIdentity(0))
        );
    }
}
