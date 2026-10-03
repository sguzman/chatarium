//! Durable safe metadata for P3 read observations.
//!
//! This journal layer stores publication-safe read metadata only. Raw remote response
//! bodies, headers, cookies, authorization material, arbitrary query values, titles,
//! and message text are deliberately outside this typed durable record. V3 may retain
//! only the narrowly typed C02 query evidence admitted by chatarium-protocol.

use crate::{EventEnvelope, EventStore};
use chatarium_core::{EventKind, RemoteReadObservationId};
use chatarium_protocol::Compatibility;
use chatarium_protocol::read::{
    JsonTopLevelType, ReadExperiment, ReadFlow, ReadMethod, ReadObservation,
    ReadQueryParameterEvidence, compatibility_for_read_flow,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const REMOTE_READ_SCHEMA: &str = "chatarium-remote-read-observation";
const REMOTE_READ_VERSION_V1: u64 = 1;
const REMOTE_READ_VERSION_V2: u64 = 2;
const REMOTE_READ_VERSION_V3: u64 = 3;
const SANITIZED_READ_FIXTURE_SOURCE: &str = "sanitized_read_fixture";

const FORBIDDEN_PAYLOAD_FIELDS: &[&str] = &[
    "body",
    "bodyText",
    "body_text",
    "raw_body",
    "headers",
    "request_headers",
    "cookies",
    "authorization",
    "query_values",
    "request_body",
    "title",
    "messages",
];

/// Restart-replayable safe remote read observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteReadObservationAuditRecord {
    /// Local identity of the observation record.
    pub observation_id: RemoteReadObservationId,
    /// Validated safe structural protocol metadata.
    pub observation: ReadObservation,
    /// Optional durable provenance for a publication-safe source fixture.
    pub provenance: Option<RemoteReadObservationProvenance>,
    /// Evidence-gated semantic compatibility for the experiment's P3 flow.
    pub compatibility: Compatibility,
    /// Durable sequence where the observation was recorded.
    pub recorded_sequence: u64,
}

/// Publication-safe provenance for one typed read observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteReadObservationProvenance {
    /// SHA-256 of the exact sanitized fixture bytes selected by the importer.
    pub source_sha256: String,
    /// Zero-based index in the fixture's read_responses array.
    pub source_read_index: u64,
}

impl RemoteReadObservationProvenance {
    /// Construct validated provenance for one sanitized read fixture entry.
    pub fn sanitized_read_fixture(
        source_sha256: impl Into<String>,
        source_read_index: u64,
    ) -> Result<Self, String> {
        let source_sha256 = source_sha256.into();
        if source_sha256.len() != 64 || !source_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(
                "sanitized read fixture SHA-256 must be exactly 64 hex characters".to_owned(),
            );
        }
        Ok(Self {
            source_sha256: source_sha256.to_ascii_lowercase(),
            source_read_index,
        })
    }
}

/// Append one safe structural read observation.
pub fn record_remote_read_observation(
    store: &mut impl EventStore,
    observation_id: RemoteReadObservationId,
    observation: &ReadObservation,
) -> std::io::Result<u64> {
    if observation.query_parameters().is_some() {
        return Err(invalid_data(
            "query-parameter evidence requires sanitized fixture provenance",
        ));
    }
    let payload = serde_json::to_string(&json!({
        "schema": REMOTE_READ_SCHEMA,
        "version": REMOTE_READ_VERSION_V1,
        "record": "remote_read_observation",
        "observation_id": observation_id.to_string(),
        "protocol_revision": observation.protocol_revision(),
        "experiment": observation.experiment().stable_name(),
        "flow": observation.flow().stable_name(),
        "method": observation.method().stable_name(),
        "path": observation.path(),
        "query_keys": observation.query_keys(),
        "status": observation.status(),
        "content_type": observation.content_type(),
        "truncated": observation.truncated(),
        "body_present": observation.body_present(),
        "top_level_type": observation.top_level_type().map(JsonTopLevelType::stable_name),
    }))
    .map_err(invalid_data)?;

    store.append_scoped(
        Some(remote_read_observation_scope(observation_id)),
        EventKind::RemoteReadObservationRecorded,
        payload,
    )
}

/// Append one safe structural read observation selected from a sanitized fixture.
pub fn record_remote_read_observation_from_fixture(
    store: &mut impl EventStore,
    observation_id: RemoteReadObservationId,
    observation: &ReadObservation,
    provenance: &RemoteReadObservationProvenance,
) -> std::io::Result<u64> {
    let version = if observation.query_parameters().is_some() {
        REMOTE_READ_VERSION_V3
    } else {
        REMOTE_READ_VERSION_V2
    };
    let mut payload = json!({
        "schema": REMOTE_READ_SCHEMA,
        "version": version,
        "record": "remote_read_observation",
        "observation_id": observation_id.to_string(),
        "protocol_revision": observation.protocol_revision(),
        "experiment": observation.experiment().stable_name(),
        "flow": observation.flow().stable_name(),
        "method": observation.method().stable_name(),
        "path": observation.path(),
        "query_keys": observation.query_keys(),
        "status": observation.status(),
        "content_type": observation.content_type(),
        "truncated": observation.truncated(),
        "body_present": observation.body_present(),
        "top_level_type": observation.top_level_type().map(JsonTopLevelType::stable_name),
        "provenance": {
            "source_kind": SANITIZED_READ_FIXTURE_SOURCE,
            "source_sha256": provenance.source_sha256.as_str(),
            "source_read_index": provenance.source_read_index,
        },
    });

    if let Some(parameters) = observation.query_parameters() {
        payload["query_parameters"] = query_parameters_json(parameters);
    }

    let payload = serde_json::to_string(&payload).map_err(invalid_data)?;

    store.append_scoped(
        Some(remote_read_observation_scope(observation_id)),
        EventKind::RemoteReadObservationRecorded,
        payload,
    )
}

/// Replay all durable safe read observations.
pub fn replay_remote_read_audit(
    events: &[EventEnvelope],
) -> Result<Vec<RemoteReadObservationAuditRecord>, String> {
    let mut by_id = BTreeMap::<RemoteReadObservationId, RemoteReadObservationAuditRecord>::new();

    for event in events {
        if event.kind != EventKind::RemoteReadObservationRecorded {
            continue;
        }

        let (payload, version) = typed_payload(event)?;
        reject_forbidden_fields(event, &payload)?;

        let observation_id = required_string(&payload, "observation_id")?
            .parse::<RemoteReadObservationId>()
            .map_err(|error| {
                format!(
                    "invalid remote read observation_id at sequence {}: {error}",
                    event.sequence
                )
            })?;
        validate_scope(event, observation_id)?;

        if by_id.contains_key(&observation_id) {
            return Err(format!(
                "duplicate remote read observation {} at sequence {}",
                observation_id, event.sequence
            ));
        }

        let experiment_name = required_string(&payload, "experiment")?;
        let experiment = ReadExperiment::from_stable_name(experiment_name).ok_or_else(|| {
            format!(
                "remote read observation at sequence {} has unknown experiment {:?}",
                event.sequence, experiment_name
            )
        })?;

        let flow_name = required_string(&payload, "flow")?;
        let flow = ReadFlow::from_stable_name(flow_name).ok_or_else(|| {
            format!(
                "remote read observation at sequence {} has unknown flow {:?}",
                event.sequence, flow_name
            )
        })?;
        if experiment.flow() != flow {
            return Err(format!(
                "remote read observation at sequence {} has experiment/flow mismatch: {} vs {}",
                event.sequence,
                experiment.stable_name(),
                flow.stable_name()
            ));
        }

        let method_name = required_string(&payload, "method")?;
        let method = ReadMethod::from_stable_name(method_name).ok_or_else(|| {
            format!(
                "remote read observation at sequence {} has non-read method {:?}",
                event.sequence, method_name
            )
        })?;

        let protocol_revision = required_string(&payload, "protocol_revision")?.to_owned();
        let path = required_string(&payload, "path")?.to_owned();
        let query_keys = required_string_array(&payload, "query_keys")?;
        let query_parameters = parse_query_parameters(&payload, version, event.sequence)?;
        let status = required_u16(&payload, "status")?;
        let content_type = required_string(&payload, "content_type")?.to_owned();
        let truncated = required_bool(&payload, "truncated")?;
        let body_present = required_bool(&payload, "body_present")?;
        let top_level_type = optional_top_level_type(&payload, event.sequence)?;

        let observation = ReadObservation::new_with_query_parameters(
            protocol_revision.clone(),
            experiment,
            method,
            path,
            query_keys,
            query_parameters,
            status,
            content_type,
            truncated,
            body_present,
            top_level_type,
        )
        .map_err(|error| {
            format!(
                "invalid remote read observation at sequence {}: {error}",
                event.sequence
            )
        })?;

        let provenance = parse_provenance(&payload, version, event.sequence)?;
        let compatibility = compatibility_for_read_flow(flow, &protocol_revision);
        by_id.insert(
            observation_id,
            RemoteReadObservationAuditRecord {
                observation_id,
                observation,
                provenance,
                compatibility,
                recorded_sequence: event.sequence,
            },
        );
    }

    let mut records = by_id.into_values().collect::<Vec<_>>();
    records.sort_by_key(|record| record.recorded_sequence);
    Ok(records)
}

/// Stable local-only scope for one safe read observation.
#[must_use]
pub fn remote_read_observation_scope(observation_id: RemoteReadObservationId) -> String {
    format!("remote-read-observation:{observation_id}")
}

fn typed_payload(event: &EventEnvelope) -> Result<(Value, u64), String> {
    let value: Value = serde_json::from_str(&event.payload).map_err(|error| {
        format!(
            "malformed remote read observation payload at sequence {}: {error}",
            event.sequence
        )
    })?;

    if value.get("schema").and_then(Value::as_str) != Some(REMOTE_READ_SCHEMA) {
        return Err(format!(
            "remote read observation at sequence {} has missing/unsupported schema",
            event.sequence
        ));
    }

    let version = value
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            format!(
                "remote read observation at sequence {} is missing integer field 'version'",
                event.sequence
            )
        })?;
    if !matches!(
        version,
        REMOTE_READ_VERSION_V1 | REMOTE_READ_VERSION_V2 | REMOTE_READ_VERSION_V3
    ) {
        return Err(format!(
            "unsupported remote read observation version {version} at sequence {}",
            event.sequence
        ));
    }

    if required_string(&value, "record")? != "remote_read_observation" {
        return Err(format!(
            "remote read observation at sequence {} has wrong record kind",
            event.sequence
        ));
    }

    Ok((value, version))
}

fn query_parameters_json(parameters: &[ReadQueryParameterEvidence]) -> Value {
    Value::Array(
        parameters
            .iter()
            .map(|parameter| match parameter.literal_evidence() {
                Some(literal) => json!({
                    "key": parameter.key(),
                    "value": literal.literal(),
                }),
                None => json!({
                    "key": parameter.key(),
                    "unsupported": true,
                }),
            })
            .collect(),
    )
}

fn parse_query_parameters(
    payload: &Value,
    version: u64,
    sequence: u64,
) -> Result<Option<Vec<ReadQueryParameterEvidence>>, String> {
    if version < REMOTE_READ_VERSION_V3 {
        if payload.get("query_parameters").is_some() {
            return Err(format!(
                "v{version} remote read observation at sequence {sequence} must not contain query_parameters"
            ));
        }
        return Ok(None);
    }

    let parameters = payload
        .get("query_parameters")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            format!(
                "v3 remote read observation at sequence {sequence} is missing query_parameters array"
            )
        })?;

    let mut parsed = Vec::with_capacity(parameters.len());
    for (index, parameter) in parameters.iter().enumerate() {
        let object = parameter.as_object().ok_or_else(|| {
            format!(
                "v3 remote read observation at sequence {sequence} query parameter {index} must be an object"
            )
        })?;
        if object
            .keys()
            .any(|key| !matches!(key.as_str(), "key" | "value" | "unsupported"))
        {
            return Err(format!(
                "v3 remote read observation at sequence {sequence} query parameter {index} has unsupported fields"
            ));
        }
        let key = object.get("key").and_then(Value::as_str).ok_or_else(|| {
            format!(
                "v3 remote read observation at sequence {sequence} query parameter {index} is missing key"
            )
        })?;

        let value = object.get("value");
        let unsupported = object.get("unsupported");
        let evidence = match (value, unsupported) {
            (Some(Value::String(literal)), None) => {
                ReadQueryParameterEvidence::known(key, literal)
            }
            (None, Some(Value::Bool(true))) => {
                ReadQueryParameterEvidence::unsupported_redacted(key)
            }
            _ => {
                return Err(format!(
                    "v3 remote read observation at sequence {sequence} query parameter {index} must contain exactly one safe value or unsupported=true marker"
                ));
            }
        }
        .map_err(|error| {
            format!(
                "invalid v3 remote read observation query parameter at sequence {sequence}: {error}"
            )
        })?;
        parsed.push(evidence);
    }

    Ok(Some(parsed))
}

fn parse_provenance(
    payload: &Value,
    version: u64,
    sequence: u64,
) -> Result<Option<RemoteReadObservationProvenance>, String> {
    if version == REMOTE_READ_VERSION_V1 {
        if payload.get("provenance").is_some() {
            return Err(format!(
                "v1 remote read observation at sequence {sequence} must not contain provenance"
            ));
        }
        return Ok(None);
    }

    let provenance = payload
        .get("provenance")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            format!(
                "v2 remote read observation at sequence {sequence} is missing provenance object"
            )
        })?;
    if provenance.get("source_kind").and_then(Value::as_str) != Some(SANITIZED_READ_FIXTURE_SOURCE)
    {
        return Err(format!(
            "v2 remote read observation at sequence {sequence} has unsupported provenance source_kind"
        ));
    }
    let source_sha256 = provenance
        .get("source_sha256")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            format!(
                "v2 remote read observation at sequence {sequence} is missing provenance source_sha256"
            )
        })?;
    let source_read_index = provenance
        .get("source_read_index")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            format!(
                "v2 remote read observation at sequence {sequence} is missing provenance source_read_index"
            )
        })?;

    RemoteReadObservationProvenance::sanitized_read_fixture(source_sha256, source_read_index)
        .map(Some)
        .map_err(|error| {
            format!("invalid v2 remote read observation provenance at sequence {sequence}: {error}")
        })
}

fn reject_forbidden_fields(event: &EventEnvelope, payload: &Value) -> Result<(), String> {
    let object = payload.as_object().ok_or_else(|| {
        format!(
            "remote read observation at sequence {} must be a JSON object",
            event.sequence
        )
    })?;

    for field in FORBIDDEN_PAYLOAD_FIELDS {
        if object.contains_key(*field) {
            return Err(format!(
                "remote read observation at sequence {} contains forbidden private field {:?}",
                event.sequence, field
            ));
        }
    }
    Ok(())
}

fn optional_top_level_type(
    payload: &Value,
    sequence: u64,
) -> Result<Option<JsonTopLevelType>, String> {
    match payload.get("top_level_type") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => JsonTopLevelType::from_stable_name(value)
            .map(Some)
            .ok_or_else(|| {
                format!(
                    "remote read observation at sequence {sequence} has unknown top_level_type {value:?}"
                )
            }),
        Some(_) => Err(format!(
            "remote read observation at sequence {sequence} top_level_type must be string or null"
        )),
    }
}

fn validate_scope(
    event: &EventEnvelope,
    observation_id: RemoteReadObservationId,
) -> Result<(), String> {
    let expected = remote_read_observation_scope(observation_id);
    if event.scope.as_deref() != Some(expected.as_str()) {
        return Err(format!(
            "remote read observation at sequence {} has scope {:?}, expected {:?}",
            event.sequence, event.scope, expected
        ));
    }
    Ok(())
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("typed remote read observation is missing string field '{field}'"))
}

fn required_bool(value: &Value, field: &str) -> Result<bool, String> {
    value
        .get(field)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("typed remote read observation is missing bool field '{field}'"))
}

fn required_u16(value: &Value, field: &str) -> Result<u16, String> {
    let raw = value.get(field).and_then(Value::as_u64).ok_or_else(|| {
        format!("typed remote read observation is missing integer field '{field}'")
    })?;
    u16::try_from(raw)
        .map_err(|_| format!("typed remote read observation field '{field}' exceeds u16"))
}

fn required_string_array(value: &Value, field: &str) -> Result<Vec<String>, String> {
    value
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("typed remote read observation is missing array field '{field}'"))?
        .iter()
        .map(|item| {
            item.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                format!("typed remote read observation field '{field}' contains non-string")
            })
        })
        .collect()
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::SqliteProjection;
    use crate::{JsonlEventStore, MemoryEventStore};
    use chatarium_core::TurnEvidence;
    use chatarium_protocol::read::ReadQueryParameterEvidence;
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn observation(experiment: ReadExperiment, method: ReadMethod) -> ReadObservation {
        ReadObservation::new(
            "2026-09-30.read-test",
            experiment,
            method,
            "/backend-api/observed-shape",
            vec!["offset".to_owned(), "limit".to_owned()],
            200,
            "application/json; charset=utf-8",
            false,
            method == ReadMethod::Get,
            (method == ReadMethod::Get).then_some(JsonTopLevelType::Object),
        )
        .unwrap()
    }

    fn temp_path(label: &str, extension: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "chatarium-remote-read-{label}-{}-{nonce}.{extension}",
            std::process::id()
        ))
    }

    #[test]
    fn c01_no_baseline_and_unvalidated_c02_mismatch_round_trip() {
        let mut store = MemoryEventStore::default();
        let c01 = RemoteReadObservationId::new();
        let c02 = RemoteReadObservationId::new();

        record_remote_read_observation(
            &mut store,
            c01,
            &observation(ReadExperiment::ConversationList, ReadMethod::Get),
        )
        .unwrap();
        record_remote_read_observation(
            &mut store,
            c02,
            &observation(ReadExperiment::OpenConversation, ReadMethod::Head),
        )
        .unwrap();

        let records = replay_remote_read_audit(store.events()).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].observation_id, c01);
        assert_eq!(records[0].compatibility, Compatibility::NoBaseline);
        assert_eq!(records[1].observation_id, c02);
        assert!(matches!(
            &records[1].compatibility,
            Compatibility::Mismatch {
                expected_revision,
                ..
            } if expected_revision == "2026-10-03.001"
        ));
    }

    #[test]
    fn fixture_provenance_round_trips_in_v2() {
        let mut store = MemoryEventStore::default();
        let id = RemoteReadObservationId::new();
        let observation = observation(ReadExperiment::OpenConversation, ReadMethod::Get);
        let provenance = RemoteReadObservationProvenance::sanitized_read_fixture(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            3,
        )
        .unwrap();

        record_remote_read_observation_from_fixture(&mut store, id, &observation, &provenance)
            .unwrap();

        let records = replay_remote_read_audit(store.events()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].observation_id, id);
        assert_eq!(records[0].provenance, Some(provenance));
    }

    #[test]
    fn fixture_query_evidence_round_trips_in_v3_with_order_and_duplicates() {
        let mut store = MemoryEventStore::default();
        let id = RemoteReadObservationId::new();
        let parameters = vec![
            ReadQueryParameterEvidence::known("include_has_versions", "true").unwrap(),
            ReadQueryParameterEvidence::known("num_turns", "4").unwrap(),
            ReadQueryParameterEvidence::known("num_turns", "4").unwrap(),
            ReadQueryParameterEvidence::unsupported_redacted("num_turns").unwrap(),
        ];
        let observation = ReadObservation::new_with_query_parameters(
            "2026-10-02.001",
            ReadExperiment::OpenConversation,
            ReadMethod::Get,
            "/backend-api/conversations/<id>",
            vec!["include_has_versions".to_owned(), "num_turns".to_owned()],
            Some(parameters.clone()),
            200,
            "application/json",
            false,
            true,
            Some(JsonTopLevelType::Object),
        )
        .unwrap();
        let provenance = RemoteReadObservationProvenance::sanitized_read_fixture(
            "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            0,
        )
        .unwrap();

        record_remote_read_observation_from_fixture(&mut store, id, &observation, &provenance)
            .unwrap();

        let event = store.events().last().unwrap();
        let payload: Value = serde_json::from_str(&event.payload).unwrap();
        assert_eq!(payload["version"], json!(REMOTE_READ_VERSION_V3));
        assert_eq!(
            payload["query_parameters"],
            json!([
                {"key": "include_has_versions", "value": "true"},
                {"key": "num_turns", "value": "4"},
                {"key": "num_turns", "value": "4"},
                {"key": "num_turns", "unsupported": true}
            ])
        );

        let records = replay_remote_read_audit(store.events()).unwrap();
        assert_eq!(
            records[0].observation.query_parameters(),
            Some(parameters.as_slice())
        );
    }

    #[test]
    fn v2_cannot_smuggle_query_parameter_values() {
        let mut store = MemoryEventStore::default();
        let id = RemoteReadObservationId::new();
        let mut payload = base_payload(id);
        payload["version"] = json!(REMOTE_READ_VERSION_V2);
        payload["provenance"] = json!({
            "source_kind": SANITIZED_READ_FIXTURE_SOURCE,
            "source_sha256": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
            "source_read_index": 0
        });
        payload["query_parameters"] = json!([
            {"key": "num_turns", "value": "2"}
        ]);
        append_raw(&mut store, id, payload);

        let error = replay_remote_read_audit(store.events()).unwrap_err();
        assert!(error.contains("must not contain query_parameters"));
    }

    #[test]
    fn legacy_v1_observation_replays_without_fixture_provenance() {
        let mut store = MemoryEventStore::default();
        let id = RemoteReadObservationId::new();
        record_remote_read_observation(
            &mut store,
            id,
            &observation(ReadExperiment::ConversationList, ReadMethod::Get),
        )
        .unwrap();

        let records = replay_remote_read_audit(store.events()).unwrap();
        assert_eq!(records[0].provenance, None);
    }

    #[test]
    fn duplicate_observation_id_is_rejected() {
        let mut store = MemoryEventStore::default();
        let id = RemoteReadObservationId::new();
        let observation = observation(ReadExperiment::ConversationList, ReadMethod::Get);
        record_remote_read_observation(&mut store, id, &observation).unwrap();
        record_remote_read_observation(&mut store, id, &observation).unwrap();

        let error = replay_remote_read_audit(store.events()).unwrap_err();
        assert!(error.contains("duplicate remote read observation"));
    }

    #[test]
    fn experiment_flow_mismatch_is_rejected() {
        let mut store = MemoryEventStore::default();
        let id = RemoteReadObservationId::new();
        append_raw(
            &mut store,
            id,
            json!({
                "schema": REMOTE_READ_SCHEMA,
                "version": REMOTE_READ_VERSION_V1,
                "record": "remote_read_observation",
                "observation_id": id.to_string(),
                "protocol_revision": "rev",
                "experiment": "C01-conversation-list",
                "flow": "conversation_fetch",
                "method": "GET",
                "path": "/backend-api/example",
                "query_keys": [],
                "status": 200,
                "content_type": "application/json",
                "truncated": false,
                "body_present": true,
                "top_level_type": "object",
            }),
        );

        let error = replay_remote_read_audit(store.events()).unwrap_err();
        assert!(error.contains("experiment/flow mismatch"));
    }

    #[test]
    fn raw_body_fields_are_rejected() {
        for field in ["body", "bodyText", "raw_body"] {
            let mut store = MemoryEventStore::default();
            let id = RemoteReadObservationId::new();
            let mut payload = base_payload(id);
            payload
                .as_object_mut()
                .unwrap()
                .insert(field.to_owned(), Value::String("PRIVATE".to_owned()));
            append_raw(&mut store, id, payload);

            let error = replay_remote_read_audit(store.events()).unwrap_err();
            assert!(error.contains("forbidden private field"));
            assert!(!error.contains("PRIVATE"));
        }
    }

    #[test]
    fn scope_mismatch_is_rejected() {
        let mut store = MemoryEventStore::default();
        let id = RemoteReadObservationId::new();
        store
            .append_scoped(
                Some("remote-read-observation:wrong".to_owned()),
                EventKind::RemoteReadObservationRecorded,
                base_payload(id).to_string(),
            )
            .unwrap();

        let error = replay_remote_read_audit(store.events()).unwrap_err();
        assert!(error.contains("scope"));
    }

    #[test]
    fn malformed_payload_is_rejected() {
        let mut store = MemoryEventStore::default();
        let id = RemoteReadObservationId::new();
        store
            .append_scoped(
                Some(remote_read_observation_scope(id)),
                EventKind::RemoteReadObservationRecorded,
                "{not-json".to_owned(),
            )
            .unwrap();

        assert!(replay_remote_read_audit(store.events()).is_err());
    }

    #[test]
    fn typed_event_does_not_mutate_turn_evidence() {
        let mut evidence = TurnEvidence::default();
        evidence
            .apply_event_kind(EventKind::RemoteReadObservationRecorded)
            .unwrap();
        assert_eq!(evidence, TurnEvidence::default());
    }

    #[test]
    fn torn_tail_cannot_fabricate_observation() {
        let path = temp_path("torn-tail", "jsonl");
        {
            let _store = JsonlEventStore::open(&path).unwrap();
        }
        {
            let mut raw = OpenOptions::new().append(true).open(&path).unwrap();
            raw.write_all(br#"{"v":2,"sequence":1,"kind":"remote_read_observation_recorded""#)
                .unwrap();
            raw.sync_data().unwrap();
        }

        let reopened = JsonlEventStore::open(&path).unwrap();
        assert!(
            replay_remote_read_audit(reopened.events())
                .unwrap()
                .is_empty()
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn generic_sqlite_projection_carries_v2_fixture_provenance_without_schema_change() {
        let path = temp_path("projection-v2", "sqlite");
        let mut store = MemoryEventStore::default();
        let id = RemoteReadObservationId::new();
        let provenance = RemoteReadObservationProvenance::sanitized_read_fixture(
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            2,
        )
        .unwrap();
        record_remote_read_observation_from_fixture(
            &mut store,
            id,
            &observation(ReadExperiment::OpenConversation, ReadMethod::Get),
            &provenance,
        )
        .unwrap();

        let mut projection = SqliteProjection::open(&path).unwrap();
        projection.rebuild(store.events()).unwrap();
        assert_eq!(
            projection
                .events_of_kind(EventKind::RemoteReadObservationRecorded)
                .unwrap()
                .len(),
            1
        );

        let replayed = replay_remote_read_audit(store.events()).unwrap();
        assert_eq!(replayed[0].provenance, Some(provenance));

        drop(projection);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn generic_sqlite_projection_carries_observation_without_schema_change() {
        let path = temp_path("projection", "sqlite");
        let mut store = MemoryEventStore::default();
        let id = RemoteReadObservationId::new();
        record_remote_read_observation(
            &mut store,
            id,
            &observation(ReadExperiment::ConversationList, ReadMethod::Get),
        )
        .unwrap();

        let mut projection = SqliteProjection::open(&path).unwrap();
        projection.rebuild(store.events()).unwrap();
        assert_eq!(
            projection
                .events_of_kind(EventKind::RemoteReadObservationRecorded)
                .unwrap()
                .len(),
            1
        );

        drop(projection);
        let _ = fs::remove_file(path);
    }

    fn append_raw(store: &mut impl EventStore, id: RemoteReadObservationId, payload: Value) {
        store
            .append_scoped(
                Some(remote_read_observation_scope(id)),
                EventKind::RemoteReadObservationRecorded,
                payload.to_string(),
            )
            .unwrap();
    }

    fn base_payload(id: RemoteReadObservationId) -> Value {
        json!({
            "schema": REMOTE_READ_SCHEMA,
            "version": REMOTE_READ_VERSION_V1,
            "record": "remote_read_observation",
            "observation_id": id.to_string(),
            "protocol_revision": "rev",
            "experiment": "C01-conversation-list",
            "flow": "conversation_list",
            "method": "GET",
            "path": "/backend-api/example",
            "query_keys": [],
            "status": 200,
            "content_type": "application/json",
            "truncated": false,
            "body_present": true,
            "top_level_type": "object",
        })
    }
}
