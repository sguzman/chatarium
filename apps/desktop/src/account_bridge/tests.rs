//! Behavior-preserving tests for the paused local browser bridge.
    use super::*;
    use std::thread;

    fn request(address: SocketAddr, raw: &[u8]) -> Vec<u8> {
        let mut stream = TcpStream::connect(address).unwrap();
        stream.write_all(raw).unwrap();
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        response
    }

    fn browser_exchange(
        address: SocketAddr,
        inspect: impl FnOnce(&Value) + Send + 'static,
        result: impl FnOnce(&Value) -> Value + Send + 'static,
    ) -> JoinHandle<()> {
        thread::spawn(move || {
            let raw = request(
                address,
                b"GET /v1/next HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Chatarium-Bridge: edge-mv3-v1\r\n\r\n",
            );
            let split = raw
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .unwrap();
            let command: Value = serde_json::from_slice(&raw[split + 4..]).unwrap();
            inspect(&command);
            let result = result(&command);
            let body = serde_json::to_vec(&result).unwrap();
            let request_head = format!(
                "POST /v1/result HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Chatarium-Bridge: edge-mv3-v1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            let mut raw = request_head.into_bytes();
            raw.extend_from_slice(&body);
            let response = request(address, &raw);
            assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 204"));
        })
    }

    fn fresh_history_candidate(
        first_id: &str,
        first_title: &str,
        second_id: &str,
        second_title: &str,
    ) -> Value {
        let items = vec![
            json!({
                "id": first_id,
                "title": first_title,
                "create_time": null,
                "update_time": null
            }),
            json!({
                "id": second_id,
                "title": second_title,
                "create_time": null,
                "update_time": null
            }),
        ];
        json!({
            "path": "/backend-api/gizmos/snorlax/sidebar",
            "query_keys": ["conversations_per_gizmo", "limit", "owned_only"],
            "surface_kind": "snorlax_sidebar",
            "conversation_count": 2,
            "cursor_count": 1,
            "string_cursor_count": 1,
            "null_cursor_count": 0,
            "top_level_cursor": "string",
            "top_level_keys": ["cursor", "items"],
            "traversal_truncated": false,
            "observations": 1,
            "items": items
        })
    }

    fn extension_auth_result(command: &Value) -> Value {
        json!({
            "version": 1,
            "id": command["id"],
            "kind": "probe_auth",
            "ok": true,
            "authentication": "authenticated",
            "http_status": 200,
            "bridge_transport": "extension",
            "extension_version": "0.2.0",
            "chatgpt_tab_found": true,
            "main_world_execution": true,
            "account_context": true,
            "request_context_observed": false,
            "first_party_http_status": null,
            "context_header_count": 0,
            "request_profile": AUTH_REQUEST_PROFILE
        })
    }

    fn exchange_auth(address: SocketAddr) {
        let raw = request(
            address,
            b"GET /v1/next HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Chatarium-Bridge: edge-mv3-v1\r\n\r\n",
        );
        let split = raw
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap();
        let command: Value = serde_json::from_slice(&raw[split + 4..]).unwrap();
        assert_eq!(command["kind"], json!("probe_auth"));
        assert_eq!(command["request_profile"], json!(AUTH_REQUEST_PROFILE));
        let body = serde_json::to_vec(&extension_auth_result(&command)).unwrap();
        let request_head = format!(
            "POST /v1/result HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Chatarium-Bridge: edge-mv3-v1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        let mut raw = request_head.into_bytes();
        raw.extend_from_slice(&body);
        let response = request(address, &raw);
        assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 204"));
    }

    #[test]
    fn web_page_origins_cannot_reach_the_retired_direct_loopback_path() {
        let runtime = AccountBridgeRuntime::start_on("127.0.0.1:0".parse().unwrap()).unwrap();
        for origin in ["https://chatgpt.com", "https://example.com"] {
            let raw = format!(
                "GET /unknown HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: {origin}\r\nX-Chatarium-Bridge: edge-mv3-v1\r\n\r\n"
            );
            let response = request(runtime.address(), raw.as_bytes());
            assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 403"));
        }
    }

    #[test]
    fn chromium_extension_origin_can_reach_the_typed_loopback_surface() {
        let runtime = AccountBridgeRuntime::start_on("127.0.0.1:0".parse().unwrap()).unwrap();
        let response = request(
            runtime.address(),
            b"GET /unknown HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: chrome-extension://abcdefghijklmnopabcdefghijklmnop\r\nX-Chatarium-Bridge: edge-mv3-v1\r\n\r\n",
        );
        assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 404"));
    }

    #[test]
    fn browser_preflight_is_rejected_instead_of_reviving_page_cors() {
        let runtime = AccountBridgeRuntime::start_on("127.0.0.1:0".parse().unwrap()).unwrap();
        let response = request(
            runtime.address(),
            b"OPTIONS /v1/next HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: https://chatgpt.com\r\nAccess-Control-Request-Method: GET\r\nAccess-Control-Request-Headers: x-chatarium-bridge\r\n\r\n",
        );
        assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 403"));
    }

    #[test]
    fn loopback_endpoint_rejects_requests_without_bridge_marker() {
        let runtime = AccountBridgeRuntime::start_on("127.0.0.1:0".parse().unwrap()).unwrap();
        let response = request(
            runtime.address(),
            b"GET /v1/next HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        );
        assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 403"));
    }

    #[test]
    fn retired_userscript_marker_cannot_consume_extension_commands() {
        let runtime = AccountBridgeRuntime::start_on("127.0.0.1:0".parse().unwrap()).unwrap();
        let response = request(
            runtime.address(),
            b"GET /unknown HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Chatarium-Bridge: 1\r\n\r\n",
        );
        assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 403"));
    }

    #[test]
    fn auth_probe_crosses_only_typed_state_and_extension_proof() {
        let runtime = AccountBridgeRuntime::start_on("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = runtime.address();
        let browser = browser_exchange(
            address,
            |command| {
                assert_eq!(command["kind"], json!("probe_auth"));
                assert_eq!(command["request_profile"], json!(AUTH_REQUEST_PROFILE));
                assert_eq!(command.as_object().unwrap().len(), 4);
            },
            extension_auth_result,
        );

        let mut provider = runtime.provider();
        let observation = provider.probe_authentication().unwrap();
        assert_eq!(
            observation.evidence,
            SessionAuthenticationEvidence::Authenticated
        );
        assert_eq!(observation.http_status, 200);
        assert_eq!(observation.proof.extension_version, "0.2.0");
        assert!(observation.proof.desktop_roundtrip);
        assert!(observation.proof.chatgpt_tab_found);
        assert!(observation.proof.main_world_execution);
        assert!(observation.proof.account_context);
        assert!(!observation.proof.request_context_observed);
        assert_eq!(observation.proof.first_party_http_status, None);
        assert_eq!(observation.proof.context_header_count, 0);
        assert_eq!(observation.proof.request_profile, AUTH_REQUEST_PROFILE);
        browser.join().unwrap();
    }

    #[test]
    fn auth_probe_distinguishes_server_challenge_from_logout() {
        let runtime = AccountBridgeRuntime::start_on("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = runtime.address();
        let browser = browser_exchange(
            address,
            |command| {
                assert_eq!(command["kind"], json!("probe_auth"));
                assert_eq!(command["request_profile"], json!(AUTH_REQUEST_PROFILE));
            },
            |command| {
                let mut result = extension_auth_result(command);
                result["authentication"] = json!("unknown");
                result["http_status"] = json!(403);
                result["security_challenge"] = json!(true);
                result
            },
        );

        let mut provider = runtime.provider();
        let observation = provider.probe_authentication().unwrap();
        assert_eq!(observation.evidence, SessionAuthenticationEvidence::Unknown);
        assert_eq!(observation.http_status, 403);
        assert!(observation.security_challenge);
        browser.join().unwrap();
    }

    #[test]
    fn cdp_history_discovery_crosses_only_typed_candidates_and_proof() {
        let runtime = AccountBridgeRuntime::start_on("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = runtime.address();
        let browser = browser_exchange(
            address,
            |command| {
                assert_eq!(command["kind"], json!("discover_history_surfaces"));
                assert_eq!(command["request_profile"], json!(HISTORY_DISCOVERY_PROFILE));
                assert_eq!(command.as_object().unwrap().len(), 4);
            },
            |command| {
                json!({
                    "version": 1,
                    "id": command["id"],
                    "kind": "discover_history_surfaces",
                    "ok": true,
                    "bridge_transport": "extension-cdp",
                    "extension_version": "0.3.0",
                    "chatgpt_tab_found": true,
                    "main_world_execution": false,
                    "account_context": true,
                    "debugger_attached": true,
                    "network_enabled": true,
                    "reload_started": true,
                    "request_profile": HISTORY_DISCOVERY_PROFILE,
                    "responses_seen": 37,
                    "backend_http_200_seen": 12,
                    "json_candidates_seen": 3,
                    "body_read_failures": 0,
                    "body_too_large": 0,
                    "invalid_json": 0,
                    "candidate_count": 1,
                    "discovery": "candidates_observed",
                    "candidates": [
                        {
                            "path": "/backend-api/gizmos/snorlax/sidebar",
                            "query_keys": [
                                "conversations_per_gizmo",
                                "limit",
                                "owned_only"
                            ],
                            "surface_kind": "snorlax_sidebar",
                            "conversation_count": 1,
                            "cursor_count": 2,
                            "string_cursor_count": 1,
                            "null_cursor_count": 1,
                            "top_level_cursor": "string",
                            "top_level_keys": ["cursor", "items"],
                            "traversal_truncated": false,
                            "observations": 1,
                            "items": [
                                {
                                    "id": "remote-1",
                                    "title": "Observed live",
                                    "create_time": "2026-10-03T00:00:00Z",
                                    "update_time": "2026-10-03T00:01:00Z"
                                }
                            ]
                        }
                    ]
                })
            },
        );

        let mut provider = runtime.provider();
        let observation = provider.discover_history_surfaces().unwrap();
        assert_eq!(observation.discovery, "candidates_observed");
        assert_eq!(observation.proof.extension_version, "0.3.0");
        assert!(observation.proof.desktop_roundtrip);
        assert!(observation.proof.debugger_attached);
        assert!(observation.proof.network_enabled);
        assert!(observation.proof.reload_started);
        assert_eq!(observation.proof.responses_seen, 37);
        assert_eq!(observation.candidates.len(), 1);
        assert_eq!(
            observation.candidates[0].path,
            "/backend-api/gizmos/snorlax/sidebar"
        );
        assert_eq!(observation.candidates[0].items.len(), 1);
        assert_eq!(observation.candidates[0].items[0].id, "remote-1");
        browser.join().unwrap();
    }

    #[test]
    fn fresh_tab_history_discovery_crosses_only_typed_candidates_and_proof() {
        let runtime = AccountBridgeRuntime::start_on("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = runtime.address();
        let browser = browser_exchange(
            address,
            |command| {
                assert_eq!(
                    command["kind"],
                    json!("discover_history_surfaces_fresh_tab")
                );
                assert_eq!(
                    command["request_profile"],
                    json!(FRESH_HISTORY_DISCOVERY_PROFILE)
                );
                assert_eq!(command.as_object().unwrap().len(), 4);
            },
            |command| {
                let candidate =
                    fresh_history_candidate("fresh-1", "Fresh one", "fresh-2", "Fresh two");
                let mut result = json!({
                    "version": 1,
                    "id": command["id"],
                    "kind": "discover_history_surfaces_fresh_tab",
                    "ok": true,
                    "bridge_transport": "extension-cdp",
                    "extension_version": "0.4.10",
                    "chatgpt_tab_found": true,
                    "capture_tab_created": true,
                    "debugger_attached": true,
                    "network_enabled": true,
                    "navigation_started": true,
                    "account_context": true,
                    "request_profile": FRESH_HISTORY_DISCOVERY_PROFILE,
                    "responses_seen": 55,
                    "backend_http_200_seen": 18,
                    "json_candidates_seen": 4,
                    "body_read_failures": 0,
                    "body_too_large": 0,
                    "invalid_json": 0,
                    "candidate_count": 1,
                    "discovery": "candidates_observed"
                });
                result["candidates"] = json!([candidate]);
                result
            },
        );

        let mut provider = runtime.provider();
        let observation = provider.discover_history_surfaces_fresh_tab().unwrap();
        assert_eq!(observation.discovery, "candidates_observed");
        assert_eq!(observation.proof.extension_version, "0.4.10");
        assert!(observation.proof.capture_tab_created);
        assert!(observation.proof.debugger_attached);
        assert!(observation.proof.network_enabled);
        assert!(observation.proof.navigation_started);
        assert_eq!(observation.proof.responses_seen, 55);
        assert_eq!(observation.candidates.len(), 1);
        assert_eq!(observation.candidates[0].items.len(), 2);
        assert_eq!(observation.candidates[0].items[0].id, "fresh-1");
        browser.join().unwrap();
    }

    #[test]
    fn primary_zero_then_fresh_tab_success_roundtrips_sequentially() {
        let runtime = AccountBridgeRuntime::start_on("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = runtime.address();

        let browser = thread::spawn(move || {
            for expected_kind in [
                "probe_auth",
                "discover_history_surfaces",
                "discover_history_surfaces_fresh_tab",
            ] {
                let raw = request(
                    address,
                    b"GET /v1/next HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Chatarium-Bridge: edge-mv3-v1\r\n\r\n",
                );
                let split = raw
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .unwrap();
                let command: Value = serde_json::from_slice(&raw[split + 4..]).unwrap();
                assert_eq!(command["kind"], json!(expected_kind));

                let result = if expected_kind == "probe_auth" {
                    assert_eq!(command["request_profile"], json!(AUTH_REQUEST_PROFILE));
                    extension_auth_result(&command)
                } else if expected_kind == "discover_history_surfaces" {
                    assert_eq!(command["request_profile"], json!(HISTORY_DISCOVERY_PROFILE));
                    json!({
                        "version": 1,
                        "id": command["id"],
                        "kind": expected_kind,
                        "ok": true,
                        "bridge_transport": "extension-cdp",
                        "extension_version": "0.4.10",
                        "chatgpt_tab_found": true,
                        "debugger_attached": true,
                        "network_enabled": true,
                        "reload_started": true,
                        "account_context": true,
                        "request_profile": HISTORY_DISCOVERY_PROFILE,
                        "responses_seen": 21,
                        "backend_http_200_seen": 7,
                        "json_candidates_seen": 1,
                        "body_read_failures": 0,
                        "body_too_large": 0,
                        "invalid_json": 0,
                        "candidate_count": 0,
                        "discovery": "no_candidate_surface_observed",
                        "candidates": []
                    })
                } else {
                    assert_eq!(
                        command["request_profile"],
                        json!(FRESH_HISTORY_DISCOVERY_PROFILE)
                    );
                    let candidate =
                        fresh_history_candidate("fresh-a", "Fresh A", "fresh-b", "Fresh B");
                    let mut result = json!({
                        "version": 1,
                        "id": command["id"],
                        "kind": expected_kind,
                        "ok": true,
                        "bridge_transport": "extension-cdp",
                        "extension_version": "0.4.10",
                        "chatgpt_tab_found": true,
                        "capture_tab_created": true,
                        "debugger_attached": true,
                        "network_enabled": true,
                        "navigation_started": true,
                        "account_context": true,
                        "request_profile": FRESH_HISTORY_DISCOVERY_PROFILE,
                        "responses_seen": 61,
                        "backend_http_200_seen": 19,
                        "json_candidates_seen": 3,
                        "body_read_failures": 0,
                        "body_too_large": 0,
                        "invalid_json": 0,
                        "candidate_count": 1,
                        "discovery": "candidates_observed"
                    });
                    result["candidates"] = json!([candidate]);
                    result
                };

                let body = serde_json::to_vec(&result).unwrap();
                let request_head = format!(
                    "POST /v1/result HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Chatarium-Bridge: edge-mv3-v1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                );
                let mut raw = request_head.into_bytes();
                raw.extend_from_slice(&body);
                let response = request(address, &raw);
                assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 204"));
            }
        });

        let mut provider = runtime.provider();
        let auth = provider.probe_authentication().unwrap();
        assert_eq!(auth.evidence, SessionAuthenticationEvidence::Authenticated);

        let primary = provider.discover_history_surfaces().unwrap();
        assert!(primary.candidates.is_empty());
        assert_eq!(primary.proof.responses_seen, 21);

        let fresh = provider.discover_history_surfaces_fresh_tab().unwrap();
        assert_eq!(fresh.candidates.len(), 1);
        assert_eq!(fresh.candidates[0].items.len(), 2);
        assert_eq!(fresh.candidates[0].items[0].id, "fresh-a");
        assert_eq!(fresh.proof.responses_seen, 61);

        browser.join().unwrap();
    }

    #[test]
    fn c01_command_uses_exact_first_page_resource_and_parses_extension_proof() {
        let runtime = AccountBridgeRuntime::start_on("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = runtime.address();

        let browser = thread::spawn(move || {
            for expected_kind in ["probe_auth", "probe_auth", "list_conversations"] {
                if expected_kind == "probe_auth" {
                    exchange_auth(address);
                    continue;
                }

                let raw = request(
                    address,
                    b"GET /v1/next HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Chatarium-Bridge: edge-mv3-v1\r\n\r\n",
                );
                let split = raw
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .unwrap();
                let command: Value = serde_json::from_slice(&raw[split + 4..]).unwrap();
                assert_eq!(command["kind"], json!(expected_kind));
                assert_eq!(
                    command["resource"],
                    json!(CONVERSATION_LIST_FIRST_PAGE_RESOURCE)
                );
                assert_eq!(
                    command["request_profile"],
                    json!(CONVERSATION_LIST_OBSERVATION)
                );
                assert_eq!(command.as_object().unwrap().len(), 5);

                let result = json!({
                    "version": 1,
                    "id": command["id"],
                    "kind": "list_conversations",
                    "ok": true,
                    "http_status": 200,
                    "content_type": "application/json",
                    "bridge_transport": "extension",
                    "extension_version": "0.2.0",
                    "chatgpt_tab_found": true,
                    "main_world_execution": true,
                    "account_context": true,
                    "request_context_observed": true,
                    "first_party_http_status": 200,
                    "context_header_count": 7,
                    "request_profile": CONVERSATION_LIST_OBSERVATION,
                    "body": {
                        "items": [
                            {
                                "id": "remote-1",
                                "title": "One",
                                "create_time": "2026-09-30T18:32:58Z",
                                "update_time": "2026-09-30T21:04:06Z"
                            }
                        ],
                        "total": 21,
                        "limit": 20,
                        "offset": 0
                    }
                });
                let body = serde_json::to_vec(&result).unwrap();
                let request_head = format!(
                    "POST /v1/result HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Chatarium-Bridge: edge-mv3-v1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                );
                let mut raw = request_head.into_bytes();
                raw.extend_from_slice(&body);
                let response = request(address, &raw);
                assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 204"));
            }
        });

        let mut provider = runtime.provider();
        let observation = provider.list_recent_conversations().unwrap();
        assert_eq!(observation.page.items.len(), 1);
        assert_eq!(observation.page.items[0].id, "remote-1");
        assert_eq!(observation.page.total, 21);
        assert_eq!(observation.http_status, 200);
        assert_eq!(observation.proof.extension_version, "0.2.0");
        assert_eq!(
            observation.proof.request_profile,
            CONVERSATION_LIST_OBSERVATION
        );
        assert!(observation.proof.desktop_roundtrip);
        assert!(observation.proof.chatgpt_tab_found);
        assert!(observation.proof.main_world_execution);
        assert!(observation.proof.account_context);
        assert!(observation.proof.request_context_observed);
        assert_eq!(observation.proof.first_party_http_status, Some(200));
        assert_eq!(observation.proof.context_header_count, 7);
        browser.join().unwrap();
    }

    #[test]
    fn c01_rejects_false_success_without_account_context() {
        let runtime = AccountBridgeRuntime::start_on("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = runtime.address();

        let browser = thread::spawn(move || {
            exchange_auth(address);
            exchange_auth(address);

            let raw = request(
                address,
                b"GET /v1/next HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Chatarium-Bridge: edge-mv3-v1\r\n\r\n",
            );
            let split = raw
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .unwrap();
            let command: Value = serde_json::from_slice(&raw[split + 4..]).unwrap();
            let result = json!({
                "version": 1,
                "id": command["id"],
                "kind": "list_conversations",
                "ok": true,
                "http_status": 200,
                "content_type": "application/json",
                "bridge_transport": "extension",
                "extension_version": "0.2.0",
                "chatgpt_tab_found": true,
                "main_world_execution": true,
                "account_context": false,
                "request_context_observed": true,
                "first_party_http_status": 200,
                "context_header_count": 7,
                "request_profile": CONVERSATION_LIST_OBSERVATION,
                "body": {
                    "items": [],
                    "total": 0,
                    "limit": 20,
                    "offset": 0
                }
            });
            let body = serde_json::to_vec(&result).unwrap();
            let request_head = format!(
                "POST /v1/result HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Chatarium-Bridge: edge-mv3-v1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            let mut raw = request_head.into_bytes();
            raw.extend_from_slice(&body);
            let response = request(address, &raw);
            assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 204"));
        });

        let mut provider = runtime.provider();
        let error = provider.list_recent_conversations().unwrap_err();
        assert!(error.to_string().contains("account context"));
        browser.join().unwrap();
    }

    #[test]
    fn retired_userscript_transport_is_rejected_even_with_success_shape() {
        let runtime = AccountBridgeRuntime::start_on("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = runtime.address();
        let browser = browser_exchange(
            address,
            |command| {
                assert_eq!(command["kind"], json!("probe_auth"));
            },
            |command| {
                let mut result = extension_auth_result(command);
                result["bridge_transport"] = json!("page");
                result
            },
        );

        let mut provider = runtime.provider();
        let error = provider.probe_authentication().unwrap_err();
        assert!(error.to_string().contains("retired userscript transport"));
        browser.join().unwrap();
    }

    #[test]
    fn c02_command_uses_exact_evidence_backed_resource_and_extension_proof() {
        let runtime = AccountBridgeRuntime::start_on("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = runtime.address();
        let browser = browser_exchange(
            address,
            |command| {
                assert_eq!(command["kind"], json!("fetch_conversation"));
                assert_eq!(
                    command["resource"],
                    json!(
                        "/backend-api/conversations/opaque%2Fremote%20id?num_turns=10&include_has_versions=true"
                    )
                );
                assert_eq!(
                    command["request_profile"],
                    json!(CONVERSATION_FETCH_REQUEST_OBSERVATION)
                );
            },
            |command| {
                json!({
                    "version": 1,
                    "id": command["id"],
                    "kind": "fetch_conversation",
                    "ok": true,
                    "http_status": 200,
                    "content_type": "application/json",
                    "bridge_transport": "extension-cdp",
                    "extension_version": "0.4.0",
                    "chatgpt_tab_found": true,
                    "main_world_execution": false,
                    "debugger_attached": true,
                    "network_enabled": true,
                    "capture_tab_created": true,
                    "navigation_started": true,
                    "exact_response_seen": true,
                    "account_context": true,
                    "request_context_observed": false,
                    "first_party_http_status": 200,
                    "context_header_count": 0,
                    "request_profile": CONVERSATION_FETCH_REQUEST_OBSERVATION,
                    "body": {"conversation_id": "opaque/remote id"}
                })
            },
        );

        let mut provider = runtime.provider();
        let observation = provider
            .fetch_conversation_observation(
                &RemoteConversationId::new("opaque/remote id").unwrap(),
                &ProtocolObservationRevision::new(CONVERSATION_FETCH_REQUEST_OBSERVATION).unwrap(),
            )
            .unwrap();
        assert_eq!(
            observation.body["conversation_id"],
            json!("opaque/remote id")
        );
        assert_eq!(observation.http_status, 200);
        assert_eq!(observation.proof.extension_version, "0.4.0");
        assert_eq!(
            observation.proof.request_profile,
            CONVERSATION_FETCH_REQUEST_OBSERVATION
        );
        assert!(observation.proof.debugger_attached);
        assert!(observation.proof.network_enabled);
        assert!(observation.proof.capture_tab_created);
        assert!(observation.proof.navigation_started);
        assert!(observation.proof.exact_response_seen);
        assert!(!observation.proof.request_context_observed);
        assert_eq!(observation.proof.first_party_http_status, Some(200));
        assert_eq!(observation.proof.context_header_count, 0);
        browser.join().unwrap();
    }

    #[test]
    fn timed_out_command_rejects_late_result_and_next_command_still_roundtrips() {
        let runtime = AccountBridgeRuntime::start_on("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = runtime.address();

        let mut slow_provider = runtime.provider();
        let slow = thread::spawn(move || {
            slow_provider.call("slow_test", |_| {}, Duration::from_millis(150))
        });

        let first_raw = request(
            address,
            b"GET /v1/next HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Chatarium-Bridge: edge-mv3-v1\r\n\r\n",
        );
        let first_split = first_raw
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap();
        let first_command: Value = serde_json::from_slice(&first_raw[first_split + 4..]).unwrap();
        assert_eq!(first_command["kind"], json!("slow_test"));

        assert_eq!(slow.join().unwrap(), Err(BrowserBridgeError::Timeout));

        let late_result = json!({
            "version": 1,
            "id": first_command["id"],
            "kind": "slow_test",
            "ok": true
        });
        let late_body = serde_json::to_vec(&late_result).unwrap();
        let late_head = format!(
            "POST /v1/result HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Chatarium-Bridge: edge-mv3-v1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            late_body.len()
        );
        let mut late_request = late_head.into_bytes();
        late_request.extend_from_slice(&late_body);
        let late_response = request(address, &late_request);
        assert!(String::from_utf8_lossy(&late_response).starts_with("HTTP/1.1 409"));

        let mut next_provider = runtime.provider();
        let next =
            thread::spawn(move || next_provider.call("next_test", |_| {}, Duration::from_secs(2)));

        let next_raw = request(
            address,
            b"GET /v1/next HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Chatarium-Bridge: edge-mv3-v1\r\n\r\n",
        );
        let next_split = next_raw
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap();
        let next_command: Value = serde_json::from_slice(&next_raw[next_split + 4..]).unwrap();
        assert_eq!(next_command["kind"], json!("next_test"));

        let next_result = json!({
            "version": 1,
            "id": next_command["id"],
            "kind": "next_test",
            "ok": true
        });
        let next_body = serde_json::to_vec(&next_result).unwrap();
        let next_head = format!(
            "POST /v1/result HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Chatarium-Bridge: edge-mv3-v1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            next_body.len()
        );
        let mut next_request = next_head.into_bytes();
        next_request.extend_from_slice(&next_body);
        let next_response = request(address, &next_request);
        assert!(String::from_utf8_lossy(&next_response).starts_with("HTTP/1.1 204"));

        let returned = next.join().unwrap().unwrap();
        assert_eq!(returned["ok"], json!(true));
    }

    #[test]
    fn rate_limit_is_distinct_and_never_retried_here() {
        let runtime = AccountBridgeRuntime::start_on("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = runtime.address();
        let browser = browser_exchange(
            address,
            |_| {},
            |command| {
                json!({
                    "version": 1,
                    "id": command["id"],
                    "kind": "fetch_conversation",
                    "ok": false,
                    "http_status": 429,
                    "content_type": "application/json",
                    "bridge_transport": "extension",
                    "extension_version": "0.2.0",
                    "chatgpt_tab_found": true,
                    "main_world_execution": true,
                    "account_context": true,
                    "request_profile": CONVERSATION_FETCH_REQUEST_OBSERVATION,
                    "error": "remote_http_status"
                })
            },
        );

        let mut provider = runtime.provider();
        assert_eq!(
            provider.fetch_conversation(
                &RemoteConversationId::new("remote").unwrap(),
                &ProtocolObservationRevision::new(CONVERSATION_FETCH_REQUEST_OBSERVATION).unwrap(),
            ),
            Err(BrowserBridgeError::RateLimited(
                "observed 0 throttled exact response(s) across 0 exact response(s) and 0 total network response(s); no successful exact response arrived before the bounded capture window ended".to_owned()
            ))
        );
        browser.join().unwrap();
    }
