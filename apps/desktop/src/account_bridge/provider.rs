//! Browser bridge provider operations; parked legacy transport remains unchanged.
use super::*;

impl BrowserBridgeProvider {
    /// Probe the instrumented extension against an authenticated ChatGPT browser session.
    pub fn probe_authentication(
        &mut self,
    ) -> Result<AuthenticationObservation, BrowserBridgeError> {
        self.probe_authentication_observation()
    }

    /// Discover the current successful first-party history/list surfaces through a bounded CDP
    /// observation window. Candidate observation is not proof of complete account enumeration.
    pub fn discover_history_surfaces(
        &mut self,
    ) -> Result<HistoryDiscoveryObservation, BrowserBridgeError> {
        let result = self.call(
            "discover_history_surfaces",
            |object| {
                object.insert(
                    "request_profile".to_owned(),
                    json!(HISTORY_DISCOVERY_PROFILE),
                );
            },
            DISCOVERY_RESULT_WAIT,
        )?;

        if result.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(remote_result_error(&result));
        }
        if parse_bridge_transport(&result)? != BridgeTransport::ExtensionCdp {
            return Err(BrowserBridgeError::Protocol(
                "history discovery did not use the CDP extension transport".to_owned(),
            ));
        }

        let request_profile = result
            .get("request_profile")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BrowserBridgeError::Protocol(
                    "history discovery result is missing request profile".to_owned(),
                )
            })?;
        if request_profile != HISTORY_DISCOVERY_PROFILE {
            return Err(BrowserBridgeError::Protocol(format!(
                "history discovery profile {request_profile:?} does not match expected {HISTORY_DISCOVERY_PROFILE:?}"
            )));
        }

        let extension_version = result
            .get("extension_version")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= 64)
            .ok_or_else(|| {
                BrowserBridgeError::Protocol(
                    "history discovery result is missing extension version".to_owned(),
                )
            })?
            .to_owned();
        let required_bool = |field: &str| {
            result.get(field).and_then(Value::as_bool).ok_or_else(|| {
                BrowserBridgeError::Protocol(format!(
                    "history discovery result is missing boolean proof field {field:?}"
                ))
            })
        };
        let required_u64 = |field: &str| {
            result.get(field).and_then(Value::as_u64).ok_or_else(|| {
                BrowserBridgeError::Protocol(format!(
                    "history discovery result is missing numeric proof field {field:?}"
                ))
            })
        };

        let proof = HistoryDiscoveryProof {
            extension_version,
            desktop_roundtrip: true,
            chatgpt_tab_found: required_bool("chatgpt_tab_found")?,
            debugger_attached: required_bool("debugger_attached")?,
            network_enabled: required_bool("network_enabled")?,
            reload_started: required_bool("reload_started")?,
            account_context: required_bool("account_context")?,
            responses_seen: required_u64("responses_seen")?,
            backend_http_200_seen: required_u64("backend_http_200_seen")?,
            json_candidates_seen: required_u64("json_candidates_seen")?,
            body_read_failures: required_u64("body_read_failures")?,
            body_too_large: required_u64("body_too_large")?,
            invalid_json: required_u64("invalid_json")?,
            request_profile: request_profile.to_owned(),
        };
        if !proof.chatgpt_tab_found
            || !proof.debugger_attached
            || !proof.network_enabled
            || !proof.reload_started
            || proof.responses_seen == 0
        {
            return Err(BrowserBridgeError::Protocol(
                "history discovery returned success without complete CDP boundary proof".to_owned(),
            ));
        }

        let candidates = result
            .get("candidates")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                BrowserBridgeError::Protocol(
                    "history discovery result is missing candidates".to_owned(),
                )
            })?
            .iter()
            .enumerate()
            .map(|(index, value)| parse_history_surface_candidate(value, index))
            .collect::<Result<Vec<_>, _>>()?;

        let discovery = result
            .get("discovery")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BrowserBridgeError::Protocol(
                    "history discovery result is missing semantic state".to_owned(),
                )
            })?
            .to_owned();

        Ok(HistoryDiscoveryObservation {
            discovery,
            proof,
            candidates,
        })
    }

    /// Recovery-only discovery path that boots ChatGPT in a temporary background tab and
    /// observes the first-party application startup traffic. This composes around the frozen
    /// 0.3 active-tab discovery path instead of mutating it.
    pub fn discover_history_surfaces_fresh_tab(
        &mut self,
    ) -> Result<FreshTabHistoryDiscoveryObservation, BrowserBridgeError> {
        let result = self.call(
            "discover_history_surfaces_fresh_tab",
            |object| {
                object.insert(
                    "request_profile".to_owned(),
                    json!(FRESH_HISTORY_DISCOVERY_PROFILE),
                );
            },
            DISCOVERY_RESULT_WAIT,
        )?;

        if result.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(remote_result_error(&result));
        }
        if parse_bridge_transport(&result)? != BridgeTransport::ExtensionCdp {
            return Err(BrowserBridgeError::Protocol(
                "fresh-tab history discovery did not use the CDP extension transport".to_owned(),
            ));
        }

        let request_profile = result
            .get("request_profile")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BrowserBridgeError::Protocol(
                    "fresh-tab history discovery result is missing request profile".to_owned(),
                )
            })?;
        if request_profile != FRESH_HISTORY_DISCOVERY_PROFILE {
            return Err(BrowserBridgeError::Protocol(format!(
                "fresh-tab history discovery profile {request_profile:?} does not match expected {FRESH_HISTORY_DISCOVERY_PROFILE:?}"
            )));
        }

        let extension_version = result
            .get("extension_version")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= 64)
            .ok_or_else(|| {
                BrowserBridgeError::Protocol(
                    "fresh-tab history discovery result is missing extension version".to_owned(),
                )
            })?
            .to_owned();
        let required_bool = |field: &str| {
            result.get(field).and_then(Value::as_bool).ok_or_else(|| {
                BrowserBridgeError::Protocol(format!(
                    "fresh-tab history discovery result is missing boolean proof field {field:?}"
                ))
            })
        };
        let required_u64 = |field: &str| {
            result.get(field).and_then(Value::as_u64).ok_or_else(|| {
                BrowserBridgeError::Protocol(format!(
                    "fresh-tab history discovery result is missing numeric proof field {field:?}"
                ))
            })
        };

        let proof = FreshTabHistoryDiscoveryProof {
            extension_version,
            desktop_roundtrip: true,
            chatgpt_tab_found: required_bool("chatgpt_tab_found")?,
            capture_tab_created: required_bool("capture_tab_created")?,
            debugger_attached: required_bool("debugger_attached")?,
            network_enabled: required_bool("network_enabled")?,
            navigation_started: required_bool("navigation_started")?,
            account_context: required_bool("account_context")?,
            responses_seen: required_u64("responses_seen")?,
            backend_http_200_seen: required_u64("backend_http_200_seen")?,
            json_candidates_seen: required_u64("json_candidates_seen")?,
            body_read_failures: required_u64("body_read_failures")?,
            body_too_large: required_u64("body_too_large")?,
            invalid_json: required_u64("invalid_json")?,
            request_profile: request_profile.to_owned(),
        };
        if !proof.chatgpt_tab_found
            || !proof.capture_tab_created
            || !proof.debugger_attached
            || !proof.network_enabled
            || !proof.navigation_started
            || proof.responses_seen == 0
        {
            return Err(BrowserBridgeError::Protocol(
                "fresh-tab history discovery returned success without complete CDP boundary proof"
                    .to_owned(),
            ));
        }

        let candidates = result
            .get("candidates")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                BrowserBridgeError::Protocol(
                    "fresh-tab history discovery result is missing candidates".to_owned(),
                )
            })?
            .iter()
            .enumerate()
            .map(|(index, value)| parse_history_surface_candidate(value, index))
            .collect::<Result<Vec<_>, _>>()?;

        let discovery = result
            .get("discovery")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BrowserBridgeError::Protocol(
                    "fresh-tab history discovery result is missing semantic state".to_owned(),
                )
            })?
            .to_owned();

        Ok(FreshTabHistoryDiscoveryObservation {
            discovery,
            proof,
            candidates,
        })
    }

    /// Historical exact-C01 replay is retained only for forensic compatibility and must not be
    /// used by the desktop history-discovery flow.
    pub fn list_recent_conversations(
        &mut self,
    ) -> Result<ConversationListObservation, BrowserBridgeError> {
        let mut lease =
            AuthenticatedSessionLease::acquire(self).map_err(map_session_lease_error)?;
        lease
            .with_authenticated_provider(|provider| provider.fetch_history_first_page())
            .map_err(map_session_lease_error)?
    }

    pub(super) fn fetch_history_first_page(
        &self,
    ) -> Result<ConversationListObservation, BrowserBridgeError> {
        let result = self.call(
            "list_conversations",
            |object| {
                object.insert(
                    "resource".to_owned(),
                    json!(CONVERSATION_LIST_FIRST_PAGE_RESOURCE),
                );
                object.insert(
                    "request_profile".to_owned(),
                    json!(CONVERSATION_LIST_OBSERVATION),
                );
            },
            FETCH_RESULT_WAIT,
        )?;

        if result.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(remote_result_error(&result));
        }

        let status = result
            .get("http_status")
            .and_then(Value::as_u64)
            .and_then(|status| u16::try_from(status).ok())
            .ok_or_else(|| {
                BrowserBridgeError::Protocol(
                    "successful conversation-list result is missing HTTP status".to_owned(),
                )
            })?;
        if status != 200 {
            return Err(status_error(status));
        }
        let proof = parse_extension_proof(&result, CONVERSATION_LIST_OBSERVATION, true, true)?;

        let body = result.get("body").ok_or_else(|| {
            BrowserBridgeError::Protocol(
                "successful conversation-list result is missing body".to_owned(),
            )
        })?;
        let page = parse_conversation_list_first_page(body)
            .map_err(|error| BrowserBridgeError::Protocol(error.to_string()))?;
        Ok(ConversationListObservation {
            page,
            proof,
            http_status: status,
        })
    }

    /// Fetch one exact existing conversation using only the live browser-held session.
    ///
    /// Reusable authentication material never crosses this boundary. Authentication is probed
    /// and revalidated through the same browser bridge immediately before the C02 read.
    pub fn fetch_authenticated_conversation(
        &mut self,
        remote_conversation_id: &str,
    ) -> Result<ConversationFetchObservation, BrowserBridgeError> {
        let remote_conversation_id = RemoteConversationId::new(remote_conversation_id)
            .map_err(|error| BrowserBridgeError::Protocol(error.to_string()))?;
        let protocol_revision =
            ProtocolObservationRevision::new(CONVERSATION_FETCH_REQUEST_OBSERVATION)
                .expect("hard-coded C02 observation revision is non-empty");
        let mut lease =
            AuthenticatedSessionLease::acquire(self).map_err(map_session_lease_error)?;
        lease
            .with_authenticated_provider(|provider| {
                provider.fetch_conversation_observation(&remote_conversation_id, &protocol_revision)
            })
            .map_err(map_session_lease_error)?
    }

    pub(super) fn probe_authentication_observation(
        &self,
    ) -> Result<AuthenticationObservation, BrowserBridgeError> {
        let result = self.call(
            "probe_auth",
            |object| {
                object.insert("request_profile".to_owned(), json!(AUTH_REQUEST_PROFILE));
            },
            AUTH_RESULT_WAIT,
        )?;
        if result.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(remote_result_error(&result));
        }
        let http_status = required_http_status(&result, "authentication")?;
        let security_challenge = result
            .get("security_challenge")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let evidence = match result.get("authentication").and_then(Value::as_str) {
            Some("authenticated") => SessionAuthenticationEvidence::Authenticated,
            Some("unauthenticated") => SessionAuthenticationEvidence::Unauthenticated,
            Some("unknown") => SessionAuthenticationEvidence::Unknown,
            _ => {
                return Err(BrowserBridgeError::Protocol(
                    "probe_auth result has no supported authentication state".to_owned(),
                ));
            }
        };
        let proof = parse_extension_proof(&result, AUTH_REQUEST_PROFILE, false, false)?;
        Ok(AuthenticationObservation {
            evidence,
            proof,
            http_status,
            security_challenge,
        })
    }

    pub(super) fn fetch_conversation_observation(
        &self,
        remote_conversation_id: &RemoteConversationId,
        protocol_revision: &ProtocolObservationRevision,
    ) -> Result<ConversationFetchObservation, BrowserBridgeError> {
        if protocol_revision.as_str() != CONVERSATION_FETCH_REQUEST_OBSERVATION {
            return Err(BrowserBridgeError::UnsupportedRevision(
                protocol_revision.as_str().to_owned(),
            ));
        }

        let resource = conversation_fetch_resource(remote_conversation_id.as_str())
            .map_err(|error| BrowserBridgeError::Protocol(error.to_string()))?;
        let result = self.call(
            "fetch_conversation",
            |object| {
                object.insert(
                    "remote_conversation_id".to_owned(),
                    json!(remote_conversation_id.as_str()),
                );
                object.insert("resource".to_owned(), json!(resource));
                object.insert(
                    "request_profile".to_owned(),
                    json!(protocol_revision.as_str()),
                );
            },
            FETCH_RESULT_WAIT,
        )?;

        if result.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(remote_result_error(&result));
        }

        let http_status = required_http_status(&result, "conversation fetch")?;
        if http_status != 200 {
            return Err(status_error(http_status));
        }
        let proof = parse_extension_proof(&result, protocol_revision.as_str(), true, false)?;
        if !proof.exact_response_seen {
            return Err(BrowserBridgeError::Protocol(
                "conversation mirror result did not prove the exact first-party response was observed"
                    .to_owned(),
            ));
        }
        let body = result.get("body").cloned().ok_or_else(|| {
            BrowserBridgeError::Protocol("successful fetch result is missing body".to_owned())
        })?;

        let number = |field: &str| result.get(field).and_then(Value::as_u64).unwrap_or(0);
        let content_type = result
            .get("content_type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();

        Ok(ConversationFetchObservation {
            body,
            proof,
            http_status,
            content_type,
            responses_seen: number("responses_seen"),
            exact_response_count: number("exact_response_count"),
            rate_limited_responses: number("rate_limited_responses"),
            rate_limit_reload_count: number("rate_limit_reload_count"),
            body_read_failures: number("body_read_failures"),
            body_too_large: number("body_too_large"),
            invalid_json: number("invalid_json"),
        })
    }

    pub(super) fn call(
        &self,
        kind: &'static str,
        fields: impl FnOnce(&mut serde_json::Map<String, Value>),
        timeout: Duration,
    ) -> Result<Value, BrowserBridgeError> {
        let id;
        {
            let mut state =
                self.shared.state.lock().map_err(|_| {
                    BrowserBridgeError::Unavailable("bridge state poisoned".to_owned())
                })?;
            if state.shutdown {
                return Err(BrowserBridgeError::Unavailable(
                    "bridge server is shutting down".to_owned(),
                ));
            }
            state.next_id = state.next_id.saturating_add(1);
            id = format!("c{}", state.next_id);

            let mut object = serde_json::Map::new();
            object.insert("version".to_owned(), json!(1));
            object.insert("id".to_owned(), json!(id));
            object.insert("kind".to_owned(), json!(kind));
            fields(&mut object);
            state.queued.push_back(QueuedCommand {
                id: id.clone(),
                kind,
                body: Value::Object(object),
            });
            self.shared.changed.notify_all();
        }

        diagnostics::debug(
            "bridge",
            format!(
                "queued command id={id} kind={kind} timeout={}ms",
                timeout.as_millis()
            ),
        );
        let started = Instant::now();
        let deadline = started + timeout;
        let mut state = self
            .shared
            .state
            .lock()
            .map_err(|_| BrowserBridgeError::Unavailable("bridge state poisoned".to_owned()))?;

        loop {
            if let Some(result) = state.results.remove(&id) {
                diagnostics::debug(
                    "bridge",
                    format!(
                        "command result id={id} kind={kind} elapsed={}ms {}",
                        started.elapsed().as_millis(),
                        result_diagnostic_summary(&result)
                    ),
                );
                return validate_result(&id, kind, result);
            }
            if state.shutdown {
                cleanup_command(&mut state, &id);
                return Err(BrowserBridgeError::Unavailable(
                    "bridge server shut down before result".to_owned(),
                ));
            }

            let now = Instant::now();
            if now >= deadline {
                cleanup_command(&mut state, &id);
                diagnostics::error(
                    "bridge",
                    format!(
                        "command timeout id={id} kind={kind} after {}ms",
                        started.elapsed().as_millis()
                    ),
                );
                return Err(BrowserBridgeError::Timeout);
            }
            let remaining = deadline.saturating_duration_since(now);
            let waited = self
                .shared
                .changed
                .wait_timeout(state, remaining)
                .map_err(|_| BrowserBridgeError::Unavailable("bridge state poisoned".to_owned()))?;
            state = waited.0;
        }
    }
}

impl UserAuthenticatedSessionProvider for BrowserBridgeProvider {
    type Error = BrowserBridgeError;

    fn authentication_evidence(&mut self) -> Result<SessionAuthenticationEvidence, Self::Error> {
        self.probe_authentication_observation()
            .map(|observation| observation.evidence)
    }
}

impl RemoteConversationFetchProvider for BrowserBridgeProvider {
    type FetchError = BrowserBridgeError;

    fn fetch_conversation(
        &mut self,
        remote_conversation_id: &RemoteConversationId,
        protocol_revision: &ProtocolObservationRevision,
    ) -> Result<Value, Self::FetchError> {
        self.fetch_conversation_observation(remote_conversation_id, protocol_revision)
            .map(|observation| observation.body)
    }
}
