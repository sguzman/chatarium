//! Evidence-gated readiness for future P3 remote conversation mirroring.
//!
//! This module composes already-durable identity and read-observation evidence.
//! It does not parse remote transcript bodies, perform network I/O, or append
//! any new journal state.

use crate::EventEnvelope;
use crate::remote_identity_audit::{
    RemoteConversationBindingAuditRecord, replay_remote_identity_audit,
};
use crate::remote_read_audit::{RemoteReadObservationAuditRecord, replay_remote_read_audit};
use chatarium_core::remote::{ProtocolObservationRevision, RemoteConversationId};
use chatarium_core::{LocalConversationId, RemoteReadObservationId};
use chatarium_protocol::Compatibility;
use chatarium_protocol::read::ReadExperiment;

/// Evidence proving that the protocol prerequisites for future mirroring are satisfied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteMirrorReady {
    /// Local conversation that would receive future mirrored state.
    pub local_conversation_id: LocalConversationId,
    /// Exact opaque remote conversation identity already bound to the local conversation.
    pub remote_conversation_id: RemoteConversationId,
    /// Protocol revision shared by the identity binding and validated C02 evidence.
    pub protocol_revision: ProtocolObservationRevision,
    /// Durable C02 observation establishing the supported conversation-fetch baseline.
    pub read_observation_id: RemoteReadObservationId,
    /// Sequence where local/remote identity correlation became durable.
    pub binding_sequence: u64,
    /// Sequence where the supporting C02 observation became durable.
    pub read_observation_sequence: u64,
}

/// Explicit reason future semantic mirroring is not currently admissible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteMirrorBlocked {
    /// No durable local-to-remote identity binding exists.
    MissingRemoteBinding {
        local_conversation_id: LocalConversationId,
    },
    /// No C02/open-conversation observation exists in durable evidence.
    MissingConversationFetchObservation {
        local_conversation_id: LocalConversationId,
        binding_sequence: u64,
    },
    /// C02 structural evidence exists, but no semantic baseline is validated for its revision.
    NoBaseline {
        local_conversation_id: LocalConversationId,
        protocol_revision: ProtocolObservationRevision,
        observation_ids: Vec<RemoteReadObservationId>,
    },
    /// C02 evidence exists for the binding revision, but the protocol layer reports incompatibility.
    ProtocolMismatch {
        local_conversation_id: LocalConversationId,
        protocol_revision: ProtocolObservationRevision,
        detail: String,
    },
    /// C02 evidence exists, but not for the revision that justified the remote identity binding.
    RevisionMismatch {
        local_conversation_id: LocalConversationId,
        binding_revision: ProtocolObservationRevision,
        observed_revisions: Vec<String>,
    },
    /// More than one validated C02 observation can satisfy the same binding revision.
    AmbiguousEvidence {
        local_conversation_id: LocalConversationId,
        protocol_revision: ProtocolObservationRevision,
        observation_ids: Vec<RemoteReadObservationId>,
    },
}

/// Derived P3 mirror readiness. Derivation never mutates durable state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteMirrorReadiness {
    /// The durable evidence required for future semantic mirroring is unambiguous and validated.
    Ready(RemoteMirrorReady),
    /// One or more evidence requirements prevent future semantic mirroring.
    Blocked(RemoteMirrorBlocked),
}

/// Derive remote-mirror protocol readiness for one local conversation from durable journal events.
///
/// This function reuses the existing strict audit replayers. Malformed identity or read-observation
/// history therefore fails before readiness can be derived.
pub fn derive_remote_mirror_readiness(
    events: &[EventEnvelope],
    local_conversation_id: LocalConversationId,
) -> Result<RemoteMirrorReadiness, String> {
    let bindings = replay_remote_identity_audit(events)?;
    let reads = replay_remote_read_audit(events)?;
    Ok(derive_from_records(
        local_conversation_id,
        &bindings,
        &reads,
    ))
}

fn derive_from_records(
    local_conversation_id: LocalConversationId,
    bindings: &[RemoteConversationBindingAuditRecord],
    reads: &[RemoteReadObservationAuditRecord],
) -> RemoteMirrorReadiness {
    let Some(binding_record) = bindings
        .iter()
        .find(|record| record.binding.local_conversation_id() == local_conversation_id)
    else {
        return RemoteMirrorReadiness::Blocked(RemoteMirrorBlocked::MissingRemoteBinding {
            local_conversation_id,
        });
    };

    let fetch_reads = reads
        .iter()
        .filter(|record| record.observation.experiment() == ReadExperiment::OpenConversation)
        .collect::<Vec<_>>();

    if fetch_reads.is_empty() {
        return RemoteMirrorReadiness::Blocked(
            RemoteMirrorBlocked::MissingConversationFetchObservation {
                local_conversation_id,
                binding_sequence: binding_record.bound_sequence,
            },
        );
    }

    let binding_revision = binding_record.binding.protocol_revision().as_str();
    let matching_revision = fetch_reads
        .iter()
        .copied()
        .filter(|record| record.observation.protocol_revision() == binding_revision)
        .collect::<Vec<_>>();

    if matching_revision.is_empty() {
        let mut observed_revisions = fetch_reads
            .iter()
            .map(|record| record.observation.protocol_revision().to_owned())
            .collect::<Vec<_>>();
        observed_revisions.sort();
        observed_revisions.dedup();

        return RemoteMirrorReadiness::Blocked(RemoteMirrorBlocked::RevisionMismatch {
            local_conversation_id,
            binding_revision: binding_record.binding.protocol_revision().clone(),
            observed_revisions,
        });
    }

    let mut validated = Vec::new();
    let mut no_baseline = Vec::new();
    let mut mismatch_details = Vec::new();

    for record in matching_revision {
        match &record.compatibility {
            Compatibility::ValidatedAgainst(expected)
                if expected == record.observation.protocol_revision() =>
            {
                validated.push(record);
            }
            Compatibility::ValidatedAgainst(expected) => mismatch_details.push(format!(
                "validated baseline {:?} does not match observed revision {:?}",
                expected,
                record.observation.protocol_revision()
            )),
            Compatibility::NoBaseline => no_baseline.push(record),
            Compatibility::Mismatch {
                expected_revision,
                detail,
            } => mismatch_details.push(format!(
                "expected revision {:?}: {}",
                expected_revision, detail
            )),
        }
    }

    if validated.len() > 1 {
        let mut observation_ids = validated
            .iter()
            .map(|record| record.observation_id)
            .collect::<Vec<_>>();
        observation_ids.sort();

        return RemoteMirrorReadiness::Blocked(RemoteMirrorBlocked::AmbiguousEvidence {
            local_conversation_id,
            protocol_revision: binding_record.binding.protocol_revision().clone(),
            observation_ids,
        });
    }

    if let Some(record) = validated.first() {
        return RemoteMirrorReadiness::Ready(RemoteMirrorReady {
            local_conversation_id,
            remote_conversation_id: binding_record.binding.remote_conversation_id().clone(),
            protocol_revision: binding_record.binding.protocol_revision().clone(),
            read_observation_id: record.observation_id,
            binding_sequence: binding_record.bound_sequence,
            read_observation_sequence: record.recorded_sequence,
        });
    }

    if !mismatch_details.is_empty() {
        mismatch_details.sort();
        mismatch_details.dedup();
        return RemoteMirrorReadiness::Blocked(RemoteMirrorBlocked::ProtocolMismatch {
            local_conversation_id,
            protocol_revision: binding_record.binding.protocol_revision().clone(),
            detail: mismatch_details.join("; "),
        });
    }

    let mut observation_ids = no_baseline
        .iter()
        .map(|record| record.observation_id)
        .collect::<Vec<_>>();
    observation_ids.sort();

    RemoteMirrorReadiness::Blocked(RemoteMirrorBlocked::NoBaseline {
        local_conversation_id,
        protocol_revision: binding_record.binding.protocol_revision().clone(),
        observation_ids,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote_identity_audit::record_remote_conversation_bound;
    use crate::remote_read_audit::record_remote_read_observation;
    use crate::{EventStore, JsonlEventStore, MemoryEventStore};
    use chatarium_core::remote::RemoteConversationBinding;
    use chatarium_protocol::read::{JsonTopLevelType, ReadMethod, ReadObservation};
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn binding_record(
        local: LocalConversationId,
        remote: &str,
        revision: &str,
        sequence: u64,
    ) -> RemoteConversationBindingAuditRecord {
        RemoteConversationBindingAuditRecord {
            binding: RemoteConversationBinding::new(
                local,
                RemoteConversationId::new(remote).unwrap(),
                ProtocolObservationRevision::new(revision).unwrap(),
            ),
            bound_sequence: sequence,
        }
    }

    fn read_record(
        experiment: ReadExperiment,
        revision: &str,
        compatibility: Compatibility,
        sequence: u64,
    ) -> RemoteReadObservationAuditRecord {
        RemoteReadObservationAuditRecord {
            observation_id: RemoteReadObservationId::new(),
            observation: ReadObservation::new(
                revision,
                experiment,
                ReadMethod::Get,
                "/backend-api/observed",
                vec![],
                200,
                "application/json",
                false,
                true,
                Some(JsonTopLevelType::Object),
            )
            .unwrap(),
            provenance: None,
            compatibility,
            recorded_sequence: sequence,
        }
    }

    fn bound(
        local: LocalConversationId,
        revision: &str,
    ) -> Vec<RemoteConversationBindingAuditRecord> {
        vec![binding_record(local, "opaque-remote", revision, 1)]
    }

    #[test]
    fn missing_remote_binding_blocks() {
        let local = LocalConversationId::new();
        assert_eq!(
            derive_from_records(local, &[], &[]),
            RemoteMirrorReadiness::Blocked(RemoteMirrorBlocked::MissingRemoteBinding {
                local_conversation_id: local,
            })
        );
    }

    #[test]
    fn missing_c02_observation_blocks_even_when_c01_exists() {
        let local = LocalConversationId::new();
        let reads = vec![read_record(
            ReadExperiment::ConversationList,
            "rev-a",
            Compatibility::ValidatedAgainst("rev-a".to_owned()),
            2,
        )];

        assert!(matches!(
            derive_from_records(local, &bound(local, "rev-a"), &reads),
            RemoteMirrorReadiness::Blocked(
                RemoteMirrorBlocked::MissingConversationFetchObservation { .. }
            )
        ));
    }

    #[test]
    fn current_production_compatibility_blocks_as_no_baseline() {
        let mut store = MemoryEventStore::default();
        let local = LocalConversationId::new();
        let revision = "future-c02-observation";
        let binding = RemoteConversationBinding::new(
            local,
            RemoteConversationId::new("opaque-remote").unwrap(),
            ProtocolObservationRevision::new(revision).unwrap(),
        );
        record_remote_conversation_bound(&mut store, &binding).unwrap();

        let observation = ReadObservation::new(
            revision,
            ReadExperiment::OpenConversation,
            ReadMethod::Get,
            "/backend-api/observed",
            vec![],
            200,
            "application/json",
            false,
            true,
            Some(JsonTopLevelType::Object),
        )
        .unwrap();
        record_remote_read_observation(&mut store, RemoteReadObservationId::new(), &observation)
            .unwrap();

        assert!(matches!(
            derive_remote_mirror_readiness(store.events(), local).unwrap(),
            RemoteMirrorReadiness::Blocked(RemoteMirrorBlocked::NoBaseline { .. })
        ));
    }

    #[test]
    fn synthetic_validated_matching_revision_is_ready() {
        let local = LocalConversationId::new();
        let reads = vec![read_record(
            ReadExperiment::OpenConversation,
            "rev-a",
            Compatibility::ValidatedAgainst("rev-a".to_owned()),
            2,
        )];

        let readiness = derive_from_records(local, &bound(local, "rev-a"), &reads);
        let RemoteMirrorReadiness::Ready(ready) = readiness else {
            panic!("expected ready");
        };
        assert_eq!(ready.local_conversation_id, local);
        assert_eq!(ready.remote_conversation_id.as_str(), "opaque-remote");
        assert_eq!(ready.protocol_revision.as_str(), "rev-a");
        assert_eq!(ready.binding_sequence, 1);
        assert_eq!(ready.read_observation_sequence, 2);
    }

    #[test]
    fn revision_mismatch_blocks() {
        let local = LocalConversationId::new();
        let reads = vec![read_record(
            ReadExperiment::OpenConversation,
            "rev-b",
            Compatibility::ValidatedAgainst("rev-b".to_owned()),
            2,
        )];

        assert!(matches!(
            derive_from_records(local, &bound(local, "rev-a"), &reads),
            RemoteMirrorReadiness::Blocked(RemoteMirrorBlocked::RevisionMismatch { .. })
        ));
    }

    #[test]
    fn protocol_mismatch_blocks() {
        let local = LocalConversationId::new();
        let reads = vec![read_record(
            ReadExperiment::OpenConversation,
            "rev-a",
            Compatibility::Mismatch {
                expected_revision: "rev-z".to_owned(),
                detail: "shape changed".to_owned(),
            },
            2,
        )];

        let readiness = derive_from_records(local, &bound(local, "rev-a"), &reads);
        let RemoteMirrorReadiness::Blocked(RemoteMirrorBlocked::ProtocolMismatch {
            detail, ..
        }) = readiness
        else {
            panic!("expected protocol mismatch");
        };
        assert!(detail.contains("shape changed"));
    }

    #[test]
    fn multiple_validated_candidates_are_ambiguous() {
        let local = LocalConversationId::new();
        let reads = vec![
            read_record(
                ReadExperiment::OpenConversation,
                "rev-a",
                Compatibility::ValidatedAgainst("rev-a".to_owned()),
                2,
            ),
            read_record(
                ReadExperiment::OpenConversation,
                "rev-a",
                Compatibility::ValidatedAgainst("rev-a".to_owned()),
                3,
            ),
        ];

        assert!(matches!(
            derive_from_records(local, &bound(local, "rev-a"), &reads),
            RemoteMirrorReadiness::Blocked(RemoteMirrorBlocked::AmbiguousEvidence { .. })
        ));
    }

    #[test]
    fn unrelated_binding_does_not_satisfy_requested_conversation() {
        let requested = LocalConversationId::new();
        let other = LocalConversationId::new();
        let reads = vec![read_record(
            ReadExperiment::OpenConversation,
            "rev-a",
            Compatibility::ValidatedAgainst("rev-a".to_owned()),
            2,
        )];

        assert!(matches!(
            derive_from_records(requested, &bound(other, "rev-a"), &reads),
            RemoteMirrorReadiness::Blocked(RemoteMirrorBlocked::MissingRemoteBinding { .. })
        ));
    }

    #[test]
    fn derivation_is_read_only() {
        let mut store = MemoryEventStore::default();
        let local = LocalConversationId::new();
        let binding = RemoteConversationBinding::new(
            local,
            RemoteConversationId::new("opaque-remote").unwrap(),
            ProtocolObservationRevision::new("rev-a").unwrap(),
        );
        record_remote_conversation_bound(&mut store, &binding).unwrap();
        let before = store.events().to_vec();

        let _ = derive_remote_mirror_readiness(store.events(), local).unwrap();
        assert_eq!(store.events(), before.as_slice());
    }

    #[test]
    fn torn_tail_cannot_fabricate_readiness() {
        let path = temp_path("torn-tail");
        let local = LocalConversationId::new();
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            let binding = RemoteConversationBinding::new(
                local,
                RemoteConversationId::new("opaque-remote").unwrap(),
                ProtocolObservationRevision::new("rev-a").unwrap(),
            );
            record_remote_conversation_bound(&mut store, &binding).unwrap();
        }
        {
            let mut raw = OpenOptions::new().append(true).open(&path).unwrap();
            raw.write_all(br#"{"v":2,"sequence":2,"kind":"remote_read_observation_recorded""#)
                .unwrap();
            raw.sync_data().unwrap();
        }

        let reopened = JsonlEventStore::open(&path).unwrap();
        assert!(matches!(
            derive_remote_mirror_readiness(reopened.events(), local).unwrap(),
            RemoteMirrorReadiness::Blocked(
                RemoteMirrorBlocked::MissingConversationFetchObservation { .. }
            )
        ));

        let _ = fs::remove_file(path);
    }

    fn temp_path(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "chatarium-remote-mirror-readiness-{label}-{}-{nonce}.jsonl",
            std::process::id()
        ))
    }
}
