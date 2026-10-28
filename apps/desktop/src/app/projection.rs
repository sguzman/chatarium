//! Desktop projection helpers mechanically relocated from the original crate root.
use super::super::*;

pub(crate) fn projected_local_display_messages(
    events: &[EventEnvelope],
    local_conversation_id: LocalConversationId,
) -> Vec<DisplayMessage> {
    // Every assistant scope must have exactly one native conversation owner.
    // A duplicate turn ID claimed across conversations is ambiguous and must
    // never make assistant text visible in either transcript or model context.
    // User-authored messages remain visible to their original conversation.
    let mut owners = BTreeMap::<String, Option<LocalConversationId>>::new();
    for event in events {
        let Ok(Some(DecodedUserMessageCommit::Typed(message))) = decode_user_message_commit(event)
        else {
            continue;
        };
        match owners.entry(local_turn_scope(message.turn_id)) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(Some(message.conversation_id));
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                if entry
                    .get()
                    .is_some_and(|previous| previous != message.conversation_id)
                {
                    entry.insert(None);
                }
            }
        }
    }
    let local_turn_scopes = owners
        .into_iter()
        .filter_map(|(scope, owner)| (owner == Some(local_conversation_id)).then_some(scope))
        .collect::<HashSet<_>>();

    // Project borrowed journal events directly; cloning large raw payloads
    // into a temporary vector is unnecessary during transcript rendering
    // and before request-context composition.
    projected_display_messages_iter(events.iter().filter(|event| {
        match event.kind {
            EventKind::UserMessageCommitted => matches!(
                decode_user_message_commit(event),
                Ok(Some(DecodedUserMessageCommit::Typed(message)))
                    if message.conversation_id == local_conversation_id
            ),
            EventKind::AssistantSnapshotObserved | EventKind::AssistantCompletionObserved => event
                .scope
                .as_ref()
                .is_some_and(|scope| local_turn_scopes.contains(scope)),
            _ => false,
        }
    }))
}

pub(crate) fn projected_display_messages(events: &[EventEnvelope]) -> Vec<DisplayMessage> {
    projected_display_messages_iter(events.iter())
}

/// Metadata-only native reader provenance for a durably committed file.
/// The complete base64 payload stays in the journal, never in the transcript.
pub(crate) fn authored_attachment_provenance(event: &EventEnvelope) -> Option<String> {
    if event.kind != EventKind::UserMessageCommitted {
        return None;
    }
    let attachment = decode_user_message_attachment(event).ok()??;
    let filename = attachment.get("filename")?.as_str()?;
    let bytes = attachment.get("byte_len")?.as_u64()?;
    let read_ms = attachment.get("read_elapsed_ms")?.as_f64()?;
    let encode_ms = attachment.get("encoding_elapsed_ms")?.as_f64()?;
    Some(format!(
        "local journal · authored · attached {filename} ({bytes} bytes; read {read_ms:.1} ms, encode {encode_ms:.1} ms)"
    ))
}

pub(crate) fn projected_display_messages_iter<'a>(
    events: impl IntoIterator<Item = &'a EventEnvelope>,
) -> Vec<DisplayMessage> {
    let mut messages = Vec::<DisplayMessage>::new();
    let mut keyed = BTreeMap::<String, usize>::new();

    for event in events {
        let role = match event.kind {
            EventKind::UserMessageCommitted | EventKind::TranscriptUserMessageObserved => {
                DisplayRole::User
            }
            EventKind::AssistantSnapshotObserved | EventKind::AssistantCompletionObserved => {
                DisplayRole::Assistant
            }
            _ => continue,
        };

        let text = event_text(&event.payload);
        if text.trim().is_empty() {
            continue;
        }

        let key = payload_message_identity(&event.payload, role)
            .map(|identity| format!("{role:?}:{identity}"))
            .unwrap_or_else(|| format!("event:{}", event.sequence));

        if let Some(index) = keyed.get(&key).copied() {
            messages[index].text = text;
            messages[index].sequence = event.sequence;
            continue;
        }

        keyed.insert(key, messages.len());
        messages.push(DisplayMessage {
            role,
            text,
            sequence: event.sequence,
            timestamp: Some(event.at_unix_ms as f64 / 1000.0),
            provenance_label: Some(match role {
                DisplayRole::User => authored_attachment_provenance(event)
                    .unwrap_or_else(|| "local journal · authored".to_owned()),
                DisplayRole::Assistant => "ChatGPT Responses · remote assistant".to_owned(),
                DisplayRole::Mcp => "verified local MCP audit".to_owned(),
                DisplayRole::Activity => "verified SIWC lifecycle observation".to_owned(),
            }),
        });
    }

    messages
}

pub(crate) fn historical_display_messages(
    messages: Vec<HistoricalTranscriptMessage>,
    imported_sequence: u64,
) -> Vec<DisplayMessage> {
    messages
        .into_iter()
        .map(|message| DisplayMessage {
            role: match message.role {
                HistoricalTranscriptRole::User => DisplayRole::User,
                HistoricalTranscriptRole::Assistant => DisplayRole::Assistant,
            },
            text: message.text,
            sequence: imported_sequence,
            timestamp: message.create_time,
            provenance_label: Some(format!(
                "historical snapshot · import event #{imported_sequence}"
            )),
        })
        .collect()
}

pub(crate) fn remote_display_messages(
    messages: Vec<RemoteTranscriptMessage>,
    snapshot_sequence: u64,
) -> Vec<DisplayMessage> {
    messages
        .into_iter()
        .map(|message| DisplayMessage {
            role: match message.role {
                RemoteTranscriptRole::User => DisplayRole::User,
                RemoteTranscriptRole::Assistant => DisplayRole::Assistant,
            },
            text: message.text,
            sequence: snapshot_sequence,
            timestamp: Some(message.create_time),
            provenance_label: Some(format!(
                "live mirror · remote snapshot event #{snapshot_sequence}"
            )),
        })
        .collect()
}

pub(crate) fn payload_message_identity(payload: &str, role: DisplayRole) -> Option<String> {
    let value = serde_json::from_str::<Value>(payload).ok()?;
    let paths: &[&str] = match role {
        DisplayRole::User => &["/details/observed_message_id", "/details/observed_id"],
        DisplayRole::Assistant => &["/details/local_turn_id", "/details/observed_id"],
        DisplayRole::Mcp | DisplayRole::Activity => return None,
    };

    paths
        .iter()
        .find_map(|path| value.pointer(path).and_then(Value::as_str))
        .filter(|identity| !identity.is_empty())
        .map(ToOwned::to_owned)
}

pub(crate) fn derived_conversation_title(messages: &[DisplayMessage]) -> String {
    let Some(first) = messages
        .iter()
        .find(|message| message.role == DisplayRole::User)
    else {
        return "New local conversation".to_owned();
    };

    // Build at most the first 42 Unicode scalar values of the normalized
    // title. A large pasted first message must not allocate or scan the
    // entire body just to derive the sidebar label on each redraw.
    const LIMIT: usize = 42;
    let mut title = String::new();
    let mut count = 0;
    let mut has_word = false;
    for word in first.text.split_whitespace() {
        if has_word {
            if count == LIMIT {
                title.push('…');
                return title;
            }
            title.push(' ');
            count += 1;
        }
        for character in word.chars() {
            if count == LIMIT {
                title.push('…');
                return title;
            }
            title.push(character);
            count += 1;
        }
        has_word = true;
    }
    if title.is_empty() {
        "Local conversation".to_owned()
    } else {
        title
    }
}

/// Show an expanded first-line title only when the persisted title is the
/// default 42-scalar generated preview. Explicit renames remain untouched.
/// The egui label alone handles truncation to the currently available width.
pub(crate) fn expanded_native_conversation_header_title(
    stored_title: &str,
    messages: &[DisplayMessage],
) -> String {
    if !stored_title.ends_with('…') || stored_title != derived_conversation_title(messages) {
        return stored_title.to_owned();
    }

    let Some(first) = messages
        .iter()
        .find(|message| message.role == DisplayRole::User)
    else {
        return stored_title.to_owned();
    };
    let Some(first_line) = first.text.lines().find(|line| !line.trim().is_empty()) else {
        return stored_title.to_owned();
    };

    // Avoid putting a full pasted prompt into the header. This upper bound
    // only protects pathological single-line messages, not ordinary titles.
    const MAX_HEADER_SCALARS: usize = 180;
    let mut expanded = String::new();
    let mut count = 0_usize;
    for word in first_line.split_whitespace() {
        if !expanded.is_empty() {
            if count >= MAX_HEADER_SCALARS {
                expanded.push('…');
                return expanded;
            }
            expanded.push(' ');
            count += 1;
        }
        for ch in word.chars() {
            if count >= MAX_HEADER_SCALARS {
                expanded.push('…');
                return expanded;
            }
            expanded.push(ch);
            count += 1;
        }
    }

    if expanded.chars().count() > stored_title.chars().count() {
        expanded
    } else {
        stored_title.to_owned()
    }
}

pub(crate) fn event_text(payload: &str) -> String {
    serde_json::from_str::<Value>(payload)
        .ok()
        .and_then(|value| {
            value
                .get("text")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| payload.to_owned())
}

pub(crate) fn payload_preview(payload: &str) -> String {
    const LIMIT: usize = 100;
    let text = event_text(payload);
    let mut preview = text.chars().take(LIMIT).collect::<String>();
    if text.chars().count() > LIMIT {
        preview.push('…');
    }
    preview.replace('\n', " ↵ ")
}
