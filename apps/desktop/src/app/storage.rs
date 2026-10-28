//! Desktop storage helpers mechanically relocated from the original crate root.
use super::super::*;

pub(crate) fn projected_local_conversation_id(
    events: &[EventEnvelope],
) -> Result<Option<LocalConversationId>, String> {
    for event in events.iter().rev() {
        match decode_user_message_commit(event)? {
            Some(DecodedUserMessageCommit::Typed(message)) => {
                return Ok(Some(message.conversation_id));
            }
            Some(DecodedUserMessageCommit::LegacyText(_)) | None => {}
        }
    }
    Ok(None)
}

pub(crate) const REMOTE_HISTORY_CACHE_FILE: &str = "remote-history-cache.json";

pub(crate) fn remote_history_cache_path(journal_path: &Path) -> PathBuf {
    journal_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(REMOTE_HISTORY_CACHE_FILE)
}

pub(crate) fn local_reader_state_path(journal_path: &Path) -> PathBuf {
    journal_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("local-reader-state.json")
}

pub(crate) fn local_inference_settings_path(journal_path: &Path) -> PathBuf {
    journal_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("local-inference-settings.json")
}

pub(crate) fn local_behavior_profile_path(journal_path: &Path) -> PathBuf {
    journal_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("behavior-profiles.json")
}

pub(crate) fn local_conversation_catalog_path(journal_path: &Path) -> PathBuf {
    journal_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("local-conversations.json")
}

pub(crate) fn local_conversation_scope(conversation_id: LocalConversationId) -> String {
    format!("local-conversation:{conversation_id}")
}

pub(crate) fn discovered_local_conversation_ids(
    events: &[EventEnvelope],
) -> Result<Vec<LocalConversationId>, String> {
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for event in events {
        let Some(decoded) = decode_user_message_commit(event)? else {
            continue;
        };
        if let DecodedUserMessageCommit::Typed(message) = decoded {
            if seen.insert(message.conversation_id) {
                ids.push(message.conversation_id);
            }
        }
    }
    Ok(ids)
}

pub(crate) fn local_conversation_display_title(
    catalog: &local_conversations::LocalConversationCatalog,
    conversation_id: LocalConversationId,
    events: &[EventEnvelope],
) -> String {
    catalog
        .entry(conversation_id)
        .and_then(|entry| entry.title.clone())
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| {
            derived_conversation_title(&projected_local_display_messages(events, conversation_id))
        })
}

pub(crate) fn load_local_conversation_workspace(
    path: &Path,
    events: &[EventEnvelope],
) -> Result<
    (
        local_conversations::LocalConversationCatalog,
        LocalConversationId,
    ),
    String,
> {
    let mut catalog = local_conversations::LocalConversationCatalog::load(path)?;
    let now_ms = unix_now_ms();
    let mut changed = false;
    for conversation_id in discovered_local_conversation_ids(events)? {
        let title =
            derived_conversation_title(&projected_local_display_messages(events, conversation_id));
        changed |= catalog.ensure(conversation_id, Some(title), now_ms);
    }

    let mut active = catalog
        .active()
        .filter(|id| catalog.entry(*id).is_some_and(|entry| !entry.archived));
    if active.is_none() {
        active = catalog.first_unarchived();
    }
    let active = match active {
        Some(id) => id,
        None => {
            let id = LocalConversationId::new();
            catalog.create(id, now_ms);
            changed = true;
            id
        }
    };

    if catalog.active() != Some(active) {
        catalog.set_active(active, now_ms)?;
        changed = true;
    }
    if changed || !path.exists() {
        catalog.save_atomic(path)?;
    }
    Ok((catalog, active))
}

pub(crate) fn merge_history_discovery_catalog(
    existing: Vec<ConversationListItem>,
    candidates: &[account_bridge::HistorySurfaceCandidate],
) -> (Vec<ConversationListItem>, usize) {
    let mut catalog = existing
        .into_iter()
        .map(|item| (item.id.clone(), item))
        .collect::<BTreeMap<_, _>>();
    let mut current_pass_ids = HashSet::new();

    for candidate in candidates {
        for item in &candidate.items {
            current_pass_ids.insert(item.id.clone());
            catalog
                .entry(item.id.clone())
                .or_insert_with(|| item.clone());
        }
    }

    (catalog.into_values().collect(), current_pass_ids.len())
}

pub(crate) fn load_remote_history_cache(path: &Path) -> Result<Vec<ConversationListItem>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("failed to read {}: {error}", path.display())),
    };
    let value = serde_json::from_str::<Value>(&text)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
    if value.get("schema").and_then(Value::as_str) != Some("chatarium-remote-history-cache")
        || value.get("version").and_then(Value::as_u64) != Some(1)
    {
        return Err(format!(
            "unsupported remote history cache schema in {}",
            path.display()
        ));
    }
    let items = value
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("remote history cache {} is missing items", path.display()))?;

    let mut catalog = BTreeMap::new();
    for (index, item) in items.iter().enumerate() {
        let object = item.as_object().ok_or_else(|| {
            format!(
                "remote history cache {} item {index} is not an object",
                path.display()
            )
        })?;
        let id = object
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && id.len() <= 512)
            .ok_or_else(|| {
                format!(
                    "remote history cache {} item {index} has invalid id",
                    path.display()
                )
            })?
            .to_owned();
        let title = match object.get("title") {
            Some(Value::String(title)) => Some(title.clone()),
            Some(Value::Null) | None => None,
            Some(_) => {
                return Err(format!(
                    "remote history cache {} item {index} has invalid title",
                    path.display()
                ));
            }
        };
        let optional_value = |field: &str| match object.get(field) {
            Some(Value::Null) | None => None,
            Some(value) => Some(value.clone()),
        };
        catalog.entry(id.clone()).or_insert(ConversationListItem {
            id,
            title,
            create_time: optional_value("create_time"),
            update_time: optional_value("update_time"),
        });
    }

    Ok(catalog.into_values().collect())
}

pub(crate) fn persist_remote_history_cache(path: &Path, items: &[ConversationListItem]) -> Result<(), String> {
    let body = serde_json::json!({
        "schema": "chatarium-remote-history-cache",
        "version": 1,
        "items": items
            .iter()
            .map(|item| serde_json::json!({
                "id": item.id,
                "title": item.title,
                "create_time": item.create_time,
                "update_time": item.update_time,
            }))
            .collect::<Vec<_>>(),
    });
    let bytes = serde_json::to_vec_pretty(&body)
        .map_err(|error| format!("failed to encode remote history cache: {error}"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create remote history cache directory {}: {error}",
                parent.display()
            )
        })?;
    }
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, bytes).map_err(|error| {
        format!(
            "failed to write temporary remote history cache {}: {error}",
            temporary.display()
        )
    })?;
    std::fs::rename(&temporary, path).map_err(|error| {
        format!(
            "failed to replace remote history cache {}: {error}",
            path.display()
        )
    })
}

pub(crate) fn default_journal_path() -> PathBuf {
    if let Some(override_dir) = std::env::var_os("CHATARIUM_DATA_DIR") {
        return PathBuf::from(override_dir).join("journal.jsonl");
    }
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local_app_data)
            .join("Chatarium")
            .join("journal.jsonl");
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".chatarium")
        .join("journal.jsonl")
}
