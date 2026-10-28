//! Original recorder sanitization and fail-closed regression tests.
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn sample_experiment() -> ExperimentDefinition {
        toml::from_str(
            r#"
schema = "chatarium-experiment"
version = 1
id = "C03-send-text"

[action]
type = "send_text"
text = "respond with exactly CHATARIUM_PROTOCOL_TEST_001"

[success]
type = "assistant_text_contains"
text = "CHATARIUM_PROTOCOL_TEST_001"
"#,
        )
        .unwrap()
    }

    fn read_experiment() -> ExperimentDefinition {
        toml::from_str(
            r#"
schema = "chatarium-experiment"
version = 1
id = "C01-conversation-list"

[action]
type = "observe_conversation_list"

[success]
type = "conversation_list_visible"
"#,
        )
        .unwrap()
    }

    fn export_with_read_body(body: &str, truncated: bool) -> Value {
        let mut export = cumulative_export();
        export["recorderVersion"] = json!("0.7.0");
        export["protocolReadCapture"] = json!({
            "armed": false,
            "armedAt": null,
            "capturedBytes": body.len(),
            "responseCount": 1,
            "responseLimitBytes": 1_000_000,
            "runLimitBytes": 4_000_000,
            "intervals": [{
                "armedAt": "2026-09-29T11:00:00Z",
                "disarmedAt": "2026-09-29T11:00:03Z",
                "reason": "operator"
            }]
        });
        export["events"]
            .as_array_mut()
            .expect("events")
            .push(json!({
                "seq": 15,
                "at": "2026-09-29T11:00:03Z",
                "kind": "protocol-read-response-captured",
                "payload": {
                    "readId": "PRIVATE-read-id",
                    "method": "GET",
                    "endpoint": "/backend-api/conversation/12345678-abcd-1234-abcd-1234567890ab",
                    "queryKeys": ["limit", "offset"],
                    "status": 200,
                    "contentType": "application/json; charset=utf-8",
                    "capturedBytes": body.len(),
                    "truncated": truncated,
                    "bodyPresent": true,
                    "bodyText": body,
                    "privateEvidence": true
                }
            }));
        export
    }

    fn export_with_two_isolated_read_runs() -> Value {
        let mut export = cumulative_export();
        export["recorderVersion"] = json!("0.7.1");
        let events = export["events"].as_array_mut().expect("events");

        events.extend([
            json!({
                "seq": 15,
                "at": "2026-09-29T11:01:00Z",
                "kind": "protocol-read-capture-armed",
                "payload": {"runId": "PRIVATE-RUN-ONE"}
            }),
            json!({
                "seq": 16,
                "at": "2026-09-29T11:01:01Z",
                "kind": "protocol-read-request-observed",
                "payload": {
                    "runId": "PRIVATE-RUN-ONE",
                    "transport": "fetch",
                    "method": "GET",
                    "endpoint": "/backend-api/first",
                    "queryKeys": []
                }
            }),
            json!({
                "seq": 17,
                "at": "2026-09-29T11:01:02Z",
                "kind": "protocol-read-capture-disarmed",
                "payload": {"runId": "PRIVATE-RUN-ONE", "reason": "operator"}
            }),
            json!({
                "seq": 18,
                "at": "2026-09-29T11:02:00Z",
                "kind": "protocol-read-capture-armed",
                "payload": {"runId": "PRIVATE-RUN-TWO"}
            }),
            json!({
                "seq": 19,
                "at": "2026-09-29T11:02:01Z",
                "kind": "protocol-read-request-observed",
                "payload": {
                    "runId": "PRIVATE-RUN-TWO",
                    "transport": "fetch",
                    "method": "GET",
                    "endpoint": "/backend-api/conversations",
                    "queryKeys": ["limit"]
                }
            }),
            json!({
                "seq": 20,
                "at": "2026-09-29T11:02:02Z",
                "kind": "protocol-read-response-captured",
                "payload": {
                    "runId": "PRIVATE-RUN-TWO",
                    "transport": "fetch",
                    "readId": "PRIVATE-READ-TWO",
                    "method": "GET",
                    "endpoint": "/backend-api/conversations",
                    "queryKeys": ["limit"],
                    "status": 200,
                    "contentType": "application/json",
                    "capturedBytes": 48,
                    "truncated": false,
                    "bodyPresent": true,
                    "bodyText": "{\"items\":[{\"title\":\"PRIVATE RUN TWO TITLE\"}]}",
                    "privateEvidence": true
                }
            }),
            json!({
                "seq": 21,
                "at": "2026-09-29T11:02:03Z",
                "kind": "protocol-read-capture-disarmed",
                "payload": {"runId": "PRIVATE-RUN-TWO", "reason": "operator"}
            }),
            json!({
                "seq": 22,
                "at": "2026-09-29T11:02:04Z",
                "kind": "protocol-read-response-captured",
                "payload": {
                    "runId": "PRIVATE-RUN-ONE",
                    "transport": "fetch",
                    "readId": "PRIVATE-LATE-READ-ONE",
                    "method": "GET",
                    "endpoint": "/backend-api/first",
                    "queryKeys": [],
                    "status": 200,
                    "contentType": "application/json",
                    "capturedBytes": 42,
                    "truncated": false,
                    "bodyPresent": true,
                    "bodyText": "{\"secret\":\"LATE PRIVATE RUN ONE\"}",
                    "privateEvidence": true
                }
            }),
        ]);

        export
    }

    #[test]
    fn latest_read_capture_run_excludes_late_prior_run_responses() {
        let export = export_with_two_isolated_read_runs();
        let experiment = read_experiment();
        let selected = select_run_for_experiment(&export, &experiment).unwrap();

        assert_eq!(selected.started_seq, 18);
        let seqs = selected
            .events
            .iter()
            .filter_map(|event| event.get("seq").and_then(Value::as_u64))
            .collect::<Vec<_>>();
        assert_eq!(seqs, vec![18, 19, 20, 21]);

        let derived =
            derive_sanitized_run(&export, &selected, &experiment, &BTreeSet::new()).unwrap();
        assert_eq!(derived.pointer("/sanitized/send_intents"), Some(&json!([])));
        assert_eq!(derived.pointer("/sanitized/messages"), Some(&json!([])));
        assert_eq!(
            derived.pointer("/sanitized/assistant_wal"),
            Some(&Value::Null)
        );

        let text = serde_json::to_string(&derived).unwrap();
        assert!(!text.contains("LATE PRIVATE RUN ONE"));
        assert!(!text.contains("PRIVATE-RUN-ONE"));
        assert!(!text.contains("PRIVATE-RUN-TWO"));
        assert!(!text.contains("old private prompt"));
    }

    #[test]
    fn legacy_v070_read_run_without_run_ids_remains_ingestable() {
        let mut export = cumulative_export();
        export["recorderVersion"] = json!("0.7.0");
        let events = export["events"].as_array_mut().expect("events");
        events.extend([
            json!({
                "seq": 15,
                "at": "2026-09-29T11:03:00Z",
                "kind": "protocol-read-capture-armed",
                "payload": {}
            }),
            json!({
                "seq": 16,
                "at": "2026-09-29T11:03:01Z",
                "kind": "protocol-read-response-captured",
                "payload": {
                    "readId": "PRIVATE-READ",
                    "method": "GET",
                    "endpoint": "/backend-api/conversations",
                    "queryKeys": [],
                    "status": 200,
                    "contentType": "application/json",
                    "capturedBytes": 12,
                    "truncated": false,
                    "bodyPresent": true,
                    "bodyText": "{\"items\":[]}",
                    "privateEvidence": true
                }
            }),
            json!({
                "seq": 17,
                "at": "2026-09-29T11:03:02Z",
                "kind": "protocol-read-capture-disarmed",
                "payload": {"reason": "operator"}
            }),
            json!({
                "seq": 18,
                "at": "2026-09-29T11:03:03Z",
                "kind": "protocol-read-response-captured",
                "payload": {
                    "readId": "PRIVATE-LATE-READ",
                    "method": "GET",
                    "endpoint": "/backend-api/sidebar",
                    "queryKeys": [],
                    "status": 200,
                    "contentType": "application/json",
                    "capturedBytes": 12,
                    "truncated": false,
                    "bodyPresent": true,
                    "bodyText": "{\"items\":[]}",
                    "privateEvidence": true
                }
            }),
        ]);

        let experiment = read_experiment();
        let selected = select_run_for_experiment(&export, &experiment).unwrap();
        let seqs = selected
            .events
            .iter()
            .filter_map(|event| event.get("seq").and_then(Value::as_u64))
            .collect::<Vec<_>>();

        assert_eq!(selected.started_seq, 15);
        assert_eq!(seqs, vec![15, 16, 17]);

        let derived =
            derive_sanitized_run(&export, &selected, &experiment, &BTreeSet::new()).unwrap();
        assert_eq!(
            derived.pointer("/sanitized/read_capture/response_count"),
            Some(&json!(1))
        );
        assert_eq!(
            derived.pointer("/sanitized/read_capture/captured_bytes"),
            Some(&json!(12))
        );
    }

    fn cumulative_export() -> Value {
        let first_chunk = r#"event: delta_encoding
data: "v1"

data: {"type":"resume_conversation_token","token":"signed-secret","conversation_id":"bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"}

data: {"type":"input_message","input_message":{"id":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa","author":{"role":"user"},"content":{"content_type":"text","parts":["respond with exactly CHATARIUM_PROTOCOL_TEST_001"]},"status":"finished_successfully","metadata":{"request_id":"dddddddd-dddd-dddd-dddd-dddddddddddd","private_note":"do not leak me"}},"conversation_id":"bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"}

event: delta
data: {"v":{"message":{"id":"cccccccc-cccc-cccc-cccc-cccccccccccc","author":{"role":"assistant"},"content":{"content_type":"text","parts":[""]},"status":"in_progress","channel":"final"},"conversation_id":"bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"}}

event: delta
data: {"p":"/message/content/parts/0","o":"append","v":"CHATARIUM_PROTOCOL_"#;

        let second_chunk = r#"TEST_001"}

data: {"type":"server_ste_metadata","metadata":{"plan_type":"plus","cluster_region":"secret-region","request_id":"eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee","latency_ms":123},"conversation_id":"bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"}

data: {"type":"message_stream_complete","conversation_id":"bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"}

data: [DONE]

"#;

        json!({
            "format": "chatarium-flight-recorder-export",
            "version": 3,
            "recorderVersion": "0.6.0",
            "sendIntents": [
                {
                    "id": "send-old",
                    "at": "2026-09-29T10:00:01Z",
                    "text": "old private prompt",
                    "state": "pending"
                },
                {
                    "id": "send-new",
                    "at": "2026-09-29T11:00:01Z",
                    "text": "respond with exactly CHATARIUM_PROTOCOL_TEST_001",
                    "state": "confirmed",
                    "observedMessageId": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                    "confirmedConversation": "conversation:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
                    "confirmationEvidence": "protocol-input-message"
                }
            ],
            "assistantWal": {
                "at": "2026-09-29T11:00:02Z",
                "conversation": "conversation:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
                "observedId": "cccccccc-cccc-cccc-cccc-cccccccccccc",
                "text": "CHATARIUM_PROTOCOL_TEST_001",
                "source": "protocol-sse",
                "evidenceSources": ["protocol-sse"],
                "protocolStatus": "finished_successfully",
                "protocolEndTurn": true,
                "protocolIsComplete": true
            },
            "messages": [
                {
                    "observedAt": "2026-09-29T11:00:02Z",
                    "conversation": "conversation:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
                    "observedId": "cccccccc-cccc-cccc-cccc-cccccccccccc",
                    "role": "assistant",
                    "text": "CHATARIUM_PROTOCOL_TEST_001",
                    "source": "protocol-sse",
                    "protocolStatus": "finished_successfully",
                    "protocolEndTurn": true,
                    "protocolEvidence": "delta-completion-patch",
                    "protocolIsComplete": true
                }
            ],
            "events": [
                {
                    "seq": 1,
                    "at": "2026-09-29T10:00:00Z",
                    "kind": "recorder-started",
                    "payload": {"version": "0.5.0"}
                },
                {
                    "seq": 2,
                    "at": "2026-09-29T10:00:02Z",
                    "kind": "network-stream-error",
                    "payload": {"message": "old failure"}
                },
                {
                    "seq": 10,
                    "at": "2026-09-29T11:00:00Z",
                    "kind": "recorder-started",
                    "payload": {"version": "0.6.0"}
                },
                {
                    "seq": 11,
                    "at": "2026-09-29T11:00:01Z",
                    "kind": "network-stream-start",
                    "payload": {
                        "streamId": "secret-stream-id",
                        "endpoint": "/backend-api/f/conversation",
                        "method": "POST",
                        "status": 200,
                        "contentType": "text/event-stream; charset=utf-8"
                    }
                },
                {
                    "seq": 12,
                    "at": "2026-09-29T11:00:01Z",
                    "kind": "network-stream-chunk",
                    "payload": {
                        "streamId": "secret-stream-id",
                        "text": first_chunk
                    }
                },
                {
                    "seq": 13,
                    "at": "2026-09-29T11:00:02Z",
                    "kind": "network-stream-chunk",
                    "payload": {
                        "streamId": "secret-stream-id",
                        "text": second_chunk
                    }
                },
                {
                    "seq": 14,
                    "at": "2026-09-29T11:00:02Z",
                    "kind": "network-stream-end",
                    "payload": {
                        "streamId": "secret-stream-id",
                        "capturedBytes": 1234,
                        "terminal": "sse-done"
                    }
                }
            ]
        })
    }

    #[test]
    fn mismatched_experiment_action_is_rejected() {
        let mut export = cumulative_export();
        export["sendIntents"][1]["text"] = json!("different prompt");
        let selected = select_latest_run(&export).unwrap();
        let experiment = sample_experiment();

        let error = validate_run_matches_experiment(&export, &selected, &experiment).unwrap_err();
        assert!(error.contains("exact action text"));
    }

    #[test]
    fn latest_run_excludes_historical_events_and_records() {
        let export = cumulative_export();
        let selected = select_latest_run(&export).unwrap();
        assert_eq!(selected.started_seq, 10);
        assert_eq!(selected.recorder_version, "0.6.0");
        assert_eq!(selected.events.len(), 5);

        let experiment = sample_experiment();
        let allowed = BTreeSet::from([
            experiment.action.text.clone().unwrap(),
            experiment.success.text.clone().unwrap(),
        ]);
        let derived = derive_sanitized_run(&export, &selected, &experiment, &allowed).unwrap();
        let text = serde_json::to_string(&derived).unwrap();
        assert!(!text.contains("old private prompt"));
        assert!(!text.contains("old failure"));
    }

    #[test]
    fn stream_sanitizer_preserves_canonical_text_and_removes_secrets_and_ids() {
        let export = cumulative_export();
        let selected = select_latest_run(&export).unwrap();
        let experiment = sample_experiment();
        let allowed = BTreeSet::from([
            experiment.action.text.clone().unwrap(),
            experiment.success.text.clone().unwrap(),
        ]);
        let derived = derive_sanitized_run(&export, &selected, &experiment, &allowed).unwrap();
        let text = serde_json::to_string_pretty(&derived).unwrap();

        assert!(text.contains("respond with exactly CHATARIUM_PROTOCOL_TEST_001"));
        assert!(text.contains("CHATARIUM_PROTOCOL_TEST_001"));
        assert!(!text.contains("signed-secret"));
        assert!(!text.contains("do not leak me"));
        assert!(!text.contains("secret-region"));
        for raw in [
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
            "cccccccc-cccc-cccc-cccc-cccccccccccc",
            "dddddddd-dddd-dddd-dddd-dddddddddddd",
            "eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee",
        ] {
            assert!(!text.contains(raw), "raw identifier survived: {raw}");
        }
        assert!(text.contains("<redacted>"));
        assert!(text.contains("<redacted-value>"));
        assert!(text.contains("<conversation:1>"));
        assert!(text.contains("<message:"));
        assert!(text.contains("\"data\": \"v1\""));
        assert!(text.contains("\"request_id\": \"<id>\""));
    }

    #[test]
    fn flight_sanitization_report_counts_transformations_without_leaking_values() {
        let export = cumulative_export();
        let selected = select_latest_run(&export).unwrap();
        let experiment = sample_experiment();
        let allowed = BTreeSet::from([
            experiment.action.text.clone().unwrap(),
            experiment.success.text.clone().unwrap(),
        ]);
        let derived = derive_sanitized_run(&export, &selected, &experiment, &allowed).unwrap();
        let report = derived
            .get("sanitization_report")
            .expect("sanitization report");
        let report_text = serde_json::to_string(report).unwrap();

        assert!(
            report
                .pointer("/counts/sensitive_values_redacted")
                .and_then(Value::as_u64)
                .unwrap_or_default()
                >= 1
        );
        assert!(
            report
                .pointer("/counts/content_values_redacted")
                .and_then(Value::as_u64)
                .unwrap_or_default()
                >= 1
        );
        assert!(
            report
                .pointer("/counts/unknown_scalar_values_redacted")
                .and_then(Value::as_u64)
                .unwrap_or_default()
                >= 1
        );
        assert!(
            report
                .pointer("/counts/numeric_values_generalized")
                .and_then(Value::as_u64)
                .unwrap_or_default()
                >= 1
        );
        assert!(
            report
                .pointer("/counts/conversation_identities_placeholdered")
                .and_then(Value::as_u64)
                .unwrap_or_default()
                >= 1
        );
        assert!(
            report
                .pointer("/counts/message_identities_placeholdered")
                .and_then(Value::as_u64)
                .unwrap_or_default()
                >= 1
        );
        assert!(
            report
                .pointer("/counts/send_identities_placeholdered")
                .and_then(Value::as_u64)
                .unwrap_or_default()
                >= 1
        );
        assert!(
            report
                .pointer("/counts/generic_identities_placeholdered")
                .and_then(Value::as_u64)
                .unwrap_or_default()
                >= 1
        );
        assert!(
            report
                .pointer("/counts/canonical_experiment_literals_retained")
                .and_then(Value::as_u64)
                .unwrap_or_default()
                >= 2
        );
        assert_eq!(
            report.pointer("/counts/raw_network_stream_chunks_copied"),
            Some(&json!(0))
        );
        assert_eq!(
            report.get("publication_safety_proven"),
            Some(&Value::Bool(false))
        );

        for secret in [
            "signed-secret",
            "do not leak me",
            "secret-region",
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
            "cccccccc-cccc-cccc-cccc-cccccccccccc",
            "dddddddd-dddd-dddd-dddd-dddddddddddd",
            "eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee",
        ] {
            assert!(!report_text.contains(secret), "report leaked {secret}");
        }
    }

    #[test]
    fn unknown_server_structural_strings_are_fail_closed() {
        let mut export = cumulative_export();
        {
            let events = export["events"].as_array_mut().expect("events array");
            let chunk = events
                .iter_mut()
                .find(|event| {
                    event.get("kind").and_then(Value::as_str) == Some("network-stream-chunk")
                        && event
                            .pointer("/payload/text")
                            .and_then(Value::as_str)
                            .is_some_and(|text| text.contains("server_ste_metadata"))
                })
                .expect("server metadata chunk");
            chunk["payload"]["text"] = json!(
                "data: {\"type\":\"PRIVATE_CONTROL_SECRET\",\"conversation_id\":\"bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb\"}\n\nevent: PRIVATE_EVENT_SECRET\ndata: {\"p\":\"/message/PRIVATE_PATH_SECRET\",\"o\":\"PRIVATE_OP_SECRET\",\"v\":\"PRIVATE_VALUE_SECRET\"}\n\n"
            );
        }

        let selected = select_latest_run(&export).unwrap();
        let experiment = sample_experiment();
        let allowed = BTreeSet::from([
            experiment.action.text.clone().unwrap(),
            experiment.success.text.clone().unwrap(),
        ]);
        let derived = derive_sanitized_run(&export, &selected, &experiment, &allowed).unwrap();
        let text = serde_json::to_string_pretty(&derived).unwrap();

        for secret in [
            "PRIVATE_CONTROL_SECRET",
            "PRIVATE_EVENT_SECRET",
            "PRIVATE_PATH_SECRET",
            "PRIVATE_OP_SECRET",
            "PRIVATE_VALUE_SECRET",
        ] {
            assert!(
                !text.contains(secret),
                "unknown structural value survived: {secret}"
            );
        }
        assert!(text.contains("<redacted-event-name>"));
        assert!(text.contains("<redacted-path>"));
        assert!(text.contains("<redacted-value>"));
        assert!(
            derived
                .pointer("/sanitization_report/counts/unknown_event_names_redacted")
                .and_then(Value::as_u64)
                .unwrap_or_default()
                >= 1
        );
        assert!(
            derived
                .pointer("/sanitization_report/counts/unknown_structural_values_redacted")
                .and_then(Value::as_u64)
                .unwrap_or_default()
                >= 1
        );
        assert!(
            derived
                .pointer("/sanitization_report/counts/unknown_paths_redacted")
                .and_then(Value::as_u64)
                .unwrap_or_default()
                >= 1
        );
    }

    #[test]
    fn unknown_delta_encoding_is_redacted() {
        let mut export = cumulative_export();
        {
            let events = export["events"].as_array_mut().expect("events array");
            let chunk = events
                .iter_mut()
                .find(|event| {
                    event.get("kind").and_then(Value::as_str) == Some("network-stream-chunk")
                        && event
                            .pointer("/payload/text")
                            .and_then(Value::as_str)
                            .is_some_and(|text| text.contains("delta_encoding"))
                })
                .expect("delta encoding chunk");
            let text = chunk["payload"]["text"].as_str().unwrap().replace(
                "event: delta_encoding\ndata: \"v1\"",
                "event: delta_encoding\ndata: \"PRIVATE_ENCODING_SECRET\"",
            );
            chunk["payload"]["text"] = json!(text);
        }

        let selected = select_latest_run(&export).unwrap();
        let experiment = sample_experiment();
        let allowed = BTreeSet::from([
            experiment.action.text.clone().unwrap(),
            experiment.success.text.clone().unwrap(),
        ]);
        let derived = derive_sanitized_run(&export, &selected, &experiment, &allowed).unwrap();
        let text = serde_json::to_string_pretty(&derived).unwrap();

        assert!(!text.contains("PRIVATE_ENCODING_SECRET"));
        assert!(text.contains("<redacted-value>"));
        assert_eq!(
            derived.pointer("/sanitization_report/counts/unknown_encodings_redacted"),
            Some(&json!(1))
        );
    }

    #[test]
    fn split_sse_chunks_are_reassembled_and_inventory_is_stable() {
        let export = cumulative_export();
        let selected = select_latest_run(&export).unwrap();
        let experiment = sample_experiment();
        let allowed = BTreeSet::from([
            experiment.action.text.clone().unwrap(),
            experiment.success.text.clone().unwrap(),
        ]);
        let derived = derive_sanitized_run(&export, &selected, &experiment, &allowed).unwrap();

        assert_eq!(
            derived.pointer("/inventory/streams/0/frame_count"),
            Some(&json!(8))
        );
        assert_eq!(
            derived.pointer("/inventory/streams/0/delta_encodings/0"),
            Some(&json!("v1"))
        );
        assert_eq!(
            derived.pointer("/inventory/streams/0/control_type_counts/message_stream_complete"),
            Some(&json!(1))
        );
        assert_eq!(
            derived.pointer("/inventory/streams/0/completion/done"),
            Some(&json!(true))
        );
        assert_eq!(
            derived.pointer("/inventory/streams/0/delta_operation_counts/append"),
            Some(&json!(1))
        );
        assert_eq!(
            derived.pointer("/inventory/streams/0/delta_path_counts/~1message~1content~1parts~10"),
            Some(&json!(1))
        );
        assert_eq!(derived.pointer("/inventory/warning_count"), Some(&json!(0)));
    }

    #[test]
    fn malformed_sse_is_fail_closed() {
        let mut export = cumulative_export();
        {
            let events = export["events"].as_array_mut().expect("events array");
            let mut chunks = events.iter_mut().filter(|event| {
                event.get("kind").and_then(Value::as_str) == Some("network-stream-chunk")
            });
            chunks.next().expect("first stream chunk")["payload"]["text"] =
                json!("event: delta\ndata: {PRIVATE RAW BROKEN FRAME}\n\n");
            chunks.next().expect("second stream chunk")["payload"]["text"] = json!("");
        }

        let selected = select_latest_run(&export).unwrap();
        let experiment = sample_experiment();
        let allowed = BTreeSet::from([
            experiment.action.text.clone().unwrap(),
            experiment.success.text.clone().unwrap(),
        ]);
        let derived = derive_sanitized_run(&export, &selected, &experiment, &allowed).unwrap();
        let text = serde_json::to_string_pretty(&derived).unwrap();

        assert!(!text.contains("PRIVATE RAW BROKEN FRAME"));
        assert!(text.contains("<redacted-unparsed-sse>"));
        assert!(
            derived
                .pointer("/inventory/warning_count")
                .and_then(Value::as_u64)
                .unwrap_or_default()
                >= 1
        );
        assert_eq!(
            derived.pointer("/sanitization_report/counts/malformed_sse_frames_fail_closed"),
            Some(&json!(1))
        );
    }

    #[test]
    fn unterminated_sse_tail_is_counted_and_not_copied() {
        let mut export = cumulative_export();
        {
            let events = export["events"].as_array_mut().expect("events array");
            let chunk = events
                .iter_mut()
                .filter(|event| {
                    event.get("kind").and_then(Value::as_str) == Some("network-stream-chunk")
                })
                .last()
                .expect("last stream chunk");
            let text = chunk["payload"]["text"].as_str().unwrap().to_owned()
                + "data: {PRIVATE_UNTERMINATED_SECRET";
            chunk["payload"]["text"] = json!(text);
        }

        let selected = select_latest_run(&export).unwrap();
        let experiment = sample_experiment();
        let allowed = BTreeSet::from([
            experiment.action.text.clone().unwrap(),
            experiment.success.text.clone().unwrap(),
        ]);
        let derived = derive_sanitized_run(&export, &selected, &experiment, &allowed).unwrap();
        let text = serde_json::to_string_pretty(&derived).unwrap();

        assert!(!text.contains("PRIVATE_UNTERMINATED_SECRET"));
        assert_eq!(
            derived.pointer("/sanitization_report/counts/unterminated_sse_tails_redacted"),
            Some(&json!(1))
        );
    }

    #[test]
    fn read_json_reduction_removes_titles_messages_secrets_and_raw_ids() {
        let body = r#"{
            "items": [{
                "id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                "conversation_id": "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
                "message_id": "cccccccc-cccc-cccc-cccc-cccccccccccc",
                "title": "PRIVATE CONVERSATION TITLE",
                "content": "PRIVATE MESSAGE TEXT",
                "active": true,
                "score": 42
            }],
            "access_token": "PRIVATE TOKEN"
        }"#;
        let export = export_with_read_body(body, false);
        let selected = select_latest_run(&export).unwrap();
        let experiment = read_experiment();
        let derived =
            derive_sanitized_run(&export, &selected, &experiment, &BTreeSet::new()).unwrap();
        let text = serde_json::to_string_pretty(&derived).unwrap();

        for secret in [
            "PRIVATE CONVERSATION TITLE",
            "PRIVATE MESSAGE TEXT",
            "PRIVATE TOKEN",
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
            "cccccccc-cccc-cccc-cccc-cccccccccccc",
            "12345678-abcd-1234-abcd-1234567890ab",
            "PRIVATE-read-id",
        ] {
            assert!(!text.contains(secret), "read evidence leaked {secret}");
        }

        assert!(text.contains("<string>"));
        assert!(text.contains("<bool>"));
        assert!(text.contains("<number>"));
        assert!(text.contains("<redacted>"));
        assert!(text.contains("<conversation:"));
        assert!(text.contains("<message:"));
        assert!(text.contains("/backend-api/conversation/<id:"));
        assert_eq!(
            derived.pointer("/inventory/read_responses/0/top_level_type"),
            Some(&json!("object"))
        );
        assert_eq!(
            derived.pointer("/inventory/read_responses/0/query_keys"),
            Some(&json!(["limit", "offset"]))
        );
        assert!(
            derived
                .pointer("/inventory/read_responses/0/shape")
                .and_then(Value::as_array)
                .unwrap()
                .iter()
                .any(|row| {
                    row.get("pointer").and_then(Value::as_str) == Some("/items/0/title")
                        && row.get("type").and_then(Value::as_str) == Some("string")
                })
        );
        assert_eq!(
            derived.pointer("/sanitization_report/counts/raw_read_response_bodies_copied"),
            Some(&json!(0))
        );
    }

    #[test]
    fn v072_c02_safe_query_values_survive_sanitization_in_order() {
        let mut export = export_with_read_body(
            r#"{"conversation_id":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"}"#,
            false,
        );
        export["recorderVersion"] = json!("0.7.2");
        let event = export["events"].as_array_mut().unwrap().last_mut().unwrap();
        event["payload"]["endpoint"] =
            json!("/backend-api/conversations/12345678-abcd-1234-abcd-1234567890ab");
        event["payload"]["queryKeys"] = json!(["include_has_versions", "num_turns"]);
        event["payload"]["queryEvidence"] = json!([
            {"key": "num_turns", "value": "4"},
            {"key": "include_has_versions", "value": "true"},
            {"key": "num_turns", "value": "4"}
        ]);

        let selected = select_latest_run(&export).unwrap();
        let experiment = read_experiment();
        let derived =
            derive_sanitized_run(&export, &selected, &experiment, &BTreeSet::new()).unwrap();

        assert_eq!(
            derived.pointer("/sanitized/read_responses/0/query_parameters"),
            Some(&json!([
                {"key": "num_turns", "value": "4"},
                {"key": "include_has_versions", "value": "true"},
                {"key": "num_turns", "value": "4"}
            ]))
        );
        assert_eq!(
            derived.pointer("/inventory/read_responses/0/query_parameters"),
            derived.pointer("/sanitized/read_responses/0/query_parameters")
        );
    }

    #[test]
    fn v071_read_evidence_keeps_query_values_unknown() {
        let export = export_with_read_body(r#"{"items":[]}"#, false);
        let selected = select_latest_run(&export).unwrap();
        let experiment = read_experiment();
        let derived =
            derive_sanitized_run(&export, &selected, &experiment, &BTreeSet::new()).unwrap();

        assert!(
            derived
                .pointer("/sanitized/read_responses/0/query_parameters")
                .is_none()
        );
    }

    #[test]
    fn unsafe_or_unapproved_query_evidence_fails_closed_without_echoing_value() {
        for query_evidence in [
            json!([{"key": "num_turns", "value": "PRIVATE-QUERY-SECRET"}]),
            json!([{"key": "unapproved", "value": "123"}]),
        ] {
            let mut export = export_with_read_body(r#"{"items":[]}"#, false);
            export["recorderVersion"] = json!("0.7.2");
            let event = export["events"].as_array_mut().unwrap().last_mut().unwrap();
            event["payload"]["endpoint"] =
                json!("/backend-api/conversations/12345678-abcd-1234-abcd-1234567890ab");
            event["payload"]["queryKeys"] =
                json!(["include_has_versions", "num_turns", "unapproved"]);
            event["payload"]["queryEvidence"] = query_evidence;

            let selected = select_latest_run(&export).unwrap();
            let experiment = read_experiment();
            let error = derive_sanitized_run(&export, &selected, &experiment, &BTreeSet::new())
                .unwrap_err();

            assert!(error.contains("queryEvidence"));
            assert!(!error.contains("PRIVATE-QUERY-SECRET"));
        }
    }

    #[test]
    fn private_read_value_changes_do_not_change_structural_inventory() {
        let first = export_with_read_body(
            r#"{"items":[{"id":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa","title":"FIRST PRIVATE TITLE","count":1}]}"#,
            false,
        );
        let second = export_with_read_body(
            r#"{"items":[{"id":"bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb","title":"SECOND PRIVATE TITLE","count":999}]}"#,
            false,
        );
        let experiment = read_experiment();

        let first_selected = select_latest_run(&first).unwrap();
        let second_selected = select_latest_run(&second).unwrap();
        let first_derived =
            derive_sanitized_run(&first, &first_selected, &experiment, &BTreeSet::new()).unwrap();
        let second_derived =
            derive_sanitized_run(&second, &second_selected, &experiment, &BTreeSet::new()).unwrap();

        assert_eq!(
            first_derived.pointer("/inventory/read_responses"),
            second_derived.pointer("/inventory/read_responses")
        );
    }

    #[test]
    fn read_structural_type_change_is_inventory_visible() {
        let first = export_with_read_body(r#"{"items":[{"title":"PRIVATE"}]}"#, false);
        let second = export_with_read_body(r#"{"items":[{"title":{"value":"PRIVATE"}}]}"#, false);
        let experiment = read_experiment();

        let first_selected = select_latest_run(&first).unwrap();
        let second_selected = select_latest_run(&second).unwrap();
        let first_derived =
            derive_sanitized_run(&first, &first_selected, &experiment, &BTreeSet::new()).unwrap();
        let second_derived =
            derive_sanitized_run(&second, &second_selected, &experiment, &BTreeSet::new()).unwrap();

        assert_ne!(
            first_derived.pointer("/inventory/read_responses"),
            second_derived.pointer("/inventory/read_responses")
        );
    }

    #[test]
    fn malformed_declared_read_json_fails_closed_without_copying_body() {
        let export = export_with_read_body("{PRIVATE BROKEN JSON", false);
        let selected = select_latest_run(&export).unwrap();
        let experiment = read_experiment();
        let error =
            derive_sanitized_run(&export, &selected, &experiment, &BTreeSet::new()).unwrap_err();

        assert!(error.contains("declared JSON protocol read body failed closed"));
        assert!(!error.contains("PRIVATE BROKEN JSON"));
    }

    #[test]
    fn truncated_read_json_is_explicit_and_not_parsed_or_copied() {
        let export = export_with_read_body("{\"private\":\"DO NOT LEAK", true);
        let selected = select_latest_run(&export).unwrap();
        let experiment = read_experiment();
        let derived =
            derive_sanitized_run(&export, &selected, &experiment, &BTreeSet::new()).unwrap();
        let text = serde_json::to_string_pretty(&derived).unwrap();

        assert!(!text.contains("DO NOT LEAK"));
        assert!(text.contains(TRUNCATED_READ_BODY));
        assert_eq!(
            derived.pointer("/inventory/read_responses/0/truncated"),
            Some(&json!(true))
        );
        assert!(
            derived
                .pointer("/inventory/warning_count")
                .and_then(Value::as_u64)
                .unwrap_or_default()
                >= 1
        );
    }

    #[test]
    fn head_read_does_not_fabricate_body() {
        let mut export = export_with_read_body("", false);
        let event = export["events"].as_array_mut().unwrap().last_mut().unwrap();
        event["payload"]["method"] = json!("HEAD");
        event["payload"]["bodyPresent"] = json!(false);
        event["payload"]["bodyText"] = Value::Null;

        let selected = select_latest_run(&export).unwrap();
        let experiment = read_experiment();
        let derived =
            derive_sanitized_run(&export, &selected, &experiment, &BTreeSet::new()).unwrap();

        assert_eq!(
            derived.pointer("/sanitized/read_responses/0/body"),
            Some(&Value::Null)
        );
        assert_eq!(
            derived.pointer("/inventory/read_responses/0/top_level_type"),
            Some(&Value::Null)
        );
    }

    #[test]
    fn output_derivation_is_deterministic_for_same_input() {
        let export = cumulative_export();
        let selected = select_latest_run(&export).unwrap();
        let experiment = sample_experiment();
        let allowed = BTreeSet::from([
            experiment.action.text.clone().unwrap(),
            experiment.success.text.clone().unwrap(),
        ]);
        let first = derive_sanitized_run(&export, &selected, &experiment, &allowed).unwrap();
        let second = derive_sanitized_run(&export, &selected, &experiment, &allowed).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn snapshot_does_not_copy_raw_source_or_path() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("chatarium-flight-test-{nonce}"));
        let input = root.join("PRIVATE-user-export.json");
        let experiment = root.join("C03-send-text.toml");
        let output = root.join("snapshot");
        fs::create_dir_all(&root).unwrap();
        let raw = serde_json::to_vec_pretty(&cumulative_export()).unwrap();
        fs::write(&input, &raw).unwrap();
        fs::write(
            &experiment,
            r#"
schema = "chatarium-experiment"
version = 1
id = "C03-send-text"

[action]
type = "send_text"
text = "respond with exactly CHATARIUM_PROTOCOL_TEST_001"

[success]
type = "assistant_text_contains"
text = "CHATARIUM_PROTOCOL_TEST_001"
"#,
        )
        .unwrap();

        snapshot_flight(&input, &experiment, &output, "C03").unwrap();

        let metadata_path = output.join("derived/C03.flight.meta.json");
        let metadata: Value = serde_json::from_slice(&fs::read(&metadata_path).unwrap()).unwrap();
        assert_eq!(metadata["raw_source_sha256"], sha256_hex(&raw));
        assert!(
            !serde_json::to_string(&metadata)
                .unwrap()
                .contains("PRIVATE-user-export.json")
        );
        assert!(!output.join("PRIVATE-user-export.json").exists());

        let report_path = output.join("derived/C03.flight.sanitization.json");
        let report_bytes = fs::read(&report_path).unwrap();
        assert_eq!(
            metadata["sanitization_report_file"],
            "derived/C03.flight.sanitization.json"
        );
        assert_eq!(
            metadata["sanitization_report_sha256"],
            sha256_hex(&report_bytes)
        );
        assert_eq!(
            metadata["sanitization_report_bytes"],
            json!(report_bytes.len())
        );
        let report_text = String::from_utf8(report_bytes).unwrap();
        assert!(!report_text.contains("signed-secret"));
        assert!(!report_text.contains("PRIVATE-user-export.json"));

        let public = fs::read_to_string(output.join("evidence/C03.flight.json")).unwrap();
        assert!(!public.contains("signed-secret"));
        assert!(!public.contains("PRIVATE-user-export.json"));

        let _ = fs::remove_dir_all(root);
    }
