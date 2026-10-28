//! Browser bridge response parsing and proof validation; transport remains parked.
use super::*;

pub(super) fn parse_history_surface_candidate(
    value: &Value,
    index: usize,
) -> Result<HistorySurfaceCandidate, BrowserBridgeError> {
    let object = value.as_object().ok_or_else(|| {
        BrowserBridgeError::Protocol(format!(
            "history discovery candidate {index} is not an object"
        ))
    })?;
    let string_field = |field: &str| {
        object
            .get(field)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= 4096)
            .map(str::to_owned)
            .ok_or_else(|| {
                BrowserBridgeError::Protocol(format!(
                    "history discovery candidate {index} has invalid {field:?}"
                ))
            })
    };
    let u64_field = |field: &str| {
        object.get(field).and_then(Value::as_u64).ok_or_else(|| {
            BrowserBridgeError::Protocol(format!(
                "history discovery candidate {index} has invalid {field:?}"
            ))
        })
    };

    let query_keys = object
        .get("query_keys")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            BrowserBridgeError::Protocol(format!(
                "history discovery candidate {index} is missing query keys"
            ))
        })?
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.is_empty() && value.len() <= 128)
                .map(str::to_owned)
                .ok_or_else(|| {
                    BrowserBridgeError::Protocol(format!(
                        "history discovery candidate {index} has an invalid query key"
                    ))
                })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let items = object
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            BrowserBridgeError::Protocol(format!(
                "history discovery candidate {index} is missing items"
            ))
        })?
        .iter()
        .enumerate()
        .map(|(item_index, item)| {
            let item = item.as_object().ok_or_else(|| {
                BrowserBridgeError::Protocol(format!(
                    "history discovery candidate {index} item {item_index} is not an object"
                ))
            })?;
            let id = item
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty() && id.len() <= 256)
                .ok_or_else(|| {
                    BrowserBridgeError::Protocol(format!(
                        "history discovery candidate {index} item {item_index} has invalid id"
                    ))
                })?
                .to_owned();
            let title = match item.get("title") {
                None | Some(Value::Null) => None,
                Some(Value::String(title)) if title.len() <= 4096 => Some(title.clone()),
                _ => {
                    return Err(BrowserBridgeError::Protocol(format!(
                        "history discovery candidate {index} item {item_index} has invalid title"
                    )));
                }
            };
            Ok(ConversationListItem {
                id,
                title,
                create_time: item.get("create_time").cloned(),
                update_time: item.get("update_time").cloned(),
            })
        })
        .collect::<Result<Vec<_>, BrowserBridgeError>>()?;

    Ok(HistorySurfaceCandidate {
        path: string_field("path")?,
        query_keys,
        surface_kind: string_field("surface_kind")?,
        conversation_count: u64_field("conversation_count")?,
        cursor_count: u64_field("cursor_count")?,
        top_level_cursor: string_field("top_level_cursor")?,
        traversal_truncated: object
            .get("traversal_truncated")
            .and_then(Value::as_bool)
            .ok_or_else(|| {
                BrowserBridgeError::Protocol(format!(
                    "history discovery candidate {index} is missing traversal_truncated"
                ))
            })?,
        observations: u64_field("observations")?,
        items,
    })
}

pub(super) fn required_http_status(result: &Value, operation: &str) -> Result<u16, BrowserBridgeError> {
    result
        .get("http_status")
        .and_then(Value::as_u64)
        .and_then(|status| u16::try_from(status).ok())
        .ok_or_else(|| {
            BrowserBridgeError::Protocol(format!(
                "successful {operation} result is missing HTTP status"
            ))
        })
}

pub(super) fn parse_extension_proof(
    result: &Value,
    expected_profile: &str,
    require_account_context: bool,
    require_request_context: bool,
) -> Result<BrowserProof, BrowserBridgeError> {
    let transport = parse_bridge_transport(result)?;
    if !matches!(
        transport,
        BridgeTransport::Extension | BridgeTransport::ExtensionCdp
    ) {
        return Err(BrowserBridgeError::Protocol(
            "critical history result did not use an Edge extension transport".to_owned(),
        ));
    }

    let extension_version = result
        .get("extension_version")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 64)
        .ok_or_else(|| {
            BrowserBridgeError::Protocol(
                "extension result is missing a usable extension version".to_owned(),
            )
        })?
        .to_owned();
    let chatgpt_tab_found = result
        .get("chatgpt_tab_found")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let main_world_execution = result
        .get("main_world_execution")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let debugger_attached = result
        .get("debugger_attached")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let network_enabled = result
        .get("network_enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let capture_tab_created = result
        .get("capture_tab_created")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let navigation_started = result
        .get("navigation_started")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let exact_response_seen = result
        .get("exact_response_seen")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let account_context = result
        .get("account_context")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let request_context_observed = result
        .get("request_context_observed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let first_party_http_status = result
        .get("first_party_http_status")
        .and_then(Value::as_u64)
        .and_then(|status| u16::try_from(status).ok());
    let context_header_count = result
        .get("context_header_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let request_profile = result
        .get("request_profile")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            BrowserBridgeError::Protocol(
                "extension result is missing request-profile evidence".to_owned(),
            )
        })?;
    if request_profile != expected_profile {
        return Err(BrowserBridgeError::Protocol(format!(
            "extension request profile {request_profile:?} does not match expected {expected_profile:?}"
        )));
    }
    if !chatgpt_tab_found {
        return Err(BrowserBridgeError::Protocol(
            "extension result does not prove an exact ChatGPT tab was found".to_owned(),
        ));
    }
    let cdp_execution = debugger_attached && network_enabled && navigation_started;
    if !main_world_execution && !cdp_execution {
        return Err(BrowserBridgeError::Protocol(
            "extension result proves neither MAIN-world execution nor bounded CDP navigation"
                .to_owned(),
        ));
    }
    if require_account_context && !account_context {
        return Err(BrowserBridgeError::Protocol(
            "extension result does not prove ChatGPT account context".to_owned(),
        ));
    }
    if require_request_context && !request_context_observed {
        return Err(BrowserBridgeError::Protocol(
            "extension result does not prove observed first-party request context".to_owned(),
        ));
    }
    if require_request_context && context_header_count == 0 {
        return Err(BrowserBridgeError::Protocol(
            "extension result observed no replayable first-party context headers".to_owned(),
        ));
    }

    Ok(BrowserProof {
        extension_version,
        desktop_roundtrip: true,
        chatgpt_tab_found,
        main_world_execution,
        debugger_attached,
        network_enabled,
        capture_tab_created,
        navigation_started,
        exact_response_seen,
        account_context,
        request_context_observed,
        first_party_http_status,
        context_header_count,
        request_profile: request_profile.to_owned(),
    })
}

pub(super) fn parse_bridge_transport(result: &Value) -> Result<BridgeTransport, BrowserBridgeError> {
    match result.get("bridge_transport").and_then(Value::as_str) {
        Some("extension") => Ok(BridgeTransport::Extension),
        Some("extension-cdp") => Ok(BridgeTransport::ExtensionCdp),
        Some("page") | Some("tampermonkey") => Err(BrowserBridgeError::Protocol(
            "retired userscript transport is not accepted on the critical history path".to_owned(),
        )),
        Some(other) => Err(BrowserBridgeError::Protocol(format!(
            "unsupported bridge transport {other:?}"
        ))),
        None => Err(BrowserBridgeError::Protocol(
            "bridge result is missing transport evidence".to_owned(),
        )),
    }
}

pub(super) fn validate_result(id: &str, kind: &str, result: Value) -> Result<Value, BrowserBridgeError> {
    let object = result.as_object().ok_or_else(|| {
        BrowserBridgeError::Protocol("bridge result must be an object".to_owned())
    })?;
    if object.get("version").and_then(Value::as_u64) != Some(1)
        || object.get("id").and_then(Value::as_str) != Some(id)
        || object.get("kind").and_then(Value::as_str) != Some(kind)
    {
        return Err(BrowserBridgeError::Protocol(
            "bridge result identity/version does not match command".to_owned(),
        ));
    }
    Ok(result)
}

pub(super) fn remote_result_error(result: &Value) -> BrowserBridgeError {
    if let Some(status) = result
        .get("http_status")
        .and_then(Value::as_u64)
        .and_then(|status| u16::try_from(status).ok())
    {
        if status == 429 {
            let exact = result
                .get("exact_response_count")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let throttled = result
                .get("rate_limited_responses")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let responses = result
                .get("responses_seen")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            return BrowserBridgeError::RateLimited(format!(
                "observed {throttled} throttled exact response(s) across {exact} exact response(s) and {responses} total network response(s); no successful exact response arrived before the bounded capture window ended"
            ));
        }
        return status_error(status);
    }
    let reason = result
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("remote fetch failed without structured reason");
    let transport = result
        .get("bridge_transport")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let extension_version = result
        .get("extension_version")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let tab = proof_bool(result, "chatgpt_tab_found");
    let main_world = proof_bool(result, "main_world_execution");
    let debugger = proof_bool(result, "debugger_attached");
    let network = proof_bool(result, "network_enabled");
    let capture_tab = proof_bool(result, "capture_tab_created");
    let navigation = proof_bool(result, "navigation_started");
    let exact_response = proof_bool(result, "exact_response_seen");
    let account_context = proof_bool(result, "account_context");
    let request_context = proof_bool(result, "request_context_observed");
    let first_party_http = result
        .get("first_party_http_status")
        .and_then(Value::as_u64)
        .map(|status| status.to_string())
        .unwrap_or_else(|| "unknown".to_owned());
    let context_header_count = result
        .get("context_header_count")
        .and_then(Value::as_u64)
        .map(|count| count.to_string())
        .unwrap_or_else(|| "unknown".to_owned());
    let number = |field: &str| {
        result
            .get(field)
            .and_then(Value::as_u64)
            .map_or_else(|| "unknown".to_owned(), |value| value.to_string())
    };
    let detached_reason = result
        .get("detached_reason")
        .and_then(Value::as_str)
        .unwrap_or("none");
    let request_profile = result
        .get("request_profile")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    BrowserBridgeError::Protocol(format!(
        "{reason}; transport={transport}; extension={extension_version}; tab={tab}; MAIN={main_world}; debugger={debugger}; network={network}; capture-tab={capture_tab}; navigation={navigation}; exact-response={exact_response}; responses={}; body-read-failures={}; body-too-large={}; invalid-json={}; detached={detached_reason}; account-context={account_context}; request-context={request_context}; first-party-http={first_party_http}; context-headers={context_header_count}; profile={request_profile}",
        number("responses_seen"),
        number("body_read_failures"),
        number("body_too_large"),
        number("invalid_json"),
    ))
}

pub(super) fn proof_bool(result: &Value, field: &str) -> &'static str {
    match result.get(field).and_then(Value::as_bool) {
        Some(true) => "yes",
        Some(false) => "no",
        None => "unknown",
    }
}

pub(super) fn result_diagnostic_summary(result: &Value) -> String {
    let bool_field = |name: &str| {
        result
            .get(name)
            .and_then(Value::as_bool)
            .map(|value| if value { "yes" } else { "no" })
            .unwrap_or("unknown")
    };
    let number_field = |name: &str| {
        result
            .get(name)
            .and_then(Value::as_u64)
            .map_or_else(|| "unknown".to_owned(), |value| value.to_string())
    };
    let text_field = |name: &str| {
        result
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    };

    format!(
        "ok={} transport={} ext={} HTTP={} tab={} debugger={} network={} reload={} temp-tab={} navigate={} account-context={} exact-response={} exact-responses={} rate-limited={} responses={} backend-200={} candidates={}",
        bool_field("ok"),
        text_field("bridge_transport"),
        text_field("extension_version"),
        number_field("http_status"),
        bool_field("chatgpt_tab_found"),
        bool_field("debugger_attached"),
        bool_field("network_enabled"),
        bool_field("reload_started"),
        bool_field("capture_tab_created"),
        bool_field("navigation_started"),
        bool_field("account_context"),
        bool_field("exact_response_seen"),
        number_field("exact_response_count"),
        number_field("rate_limited_responses"),
        number_field("responses_seen"),
        number_field("backend_http_200_seen"),
        number_field("candidate_count"),
    )
}

pub(super) fn map_session_lease_error(error: SessionLeaseError<BrowserBridgeError>) -> BrowserBridgeError {
    match error {
        SessionLeaseError::Provider(error) => error,
        SessionLeaseError::Unauthenticated => BrowserBridgeError::Unauthenticated,
        SessionLeaseError::Unknown => {
            BrowserBridgeError::Unavailable("browser authentication state is unknown".to_owned())
        }
    }
}

pub(super) fn status_error(status: u16) -> BrowserBridgeError {
    match status {
        401 | 403 => BrowserBridgeError::Unauthenticated,
        429 => {
            BrowserBridgeError::RateLimited("no extended rate-limit proof was available".to_owned())
        }
        other => BrowserBridgeError::HttpStatus(other),
    }
}

pub(super) fn cleanup_command(state: &mut BridgeState, id: &str) {
    state.queued.retain(|command| command.id != id);
    state.inflight.remove(id);
    state.results.remove(id);
}
