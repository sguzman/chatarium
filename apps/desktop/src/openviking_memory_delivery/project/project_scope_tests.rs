use super::*;
use crate::local_conversations::LocalConversationCatalog;
use crate::openviking_checkpoint_outbox::{Outbox, State};

#[test]
fn project_binding_does_not_implicitly_grant_memory_sharing() {
    let first = chatarium_core::LocalConversationId::new();
    let second = chatarium_core::LocalConversationId::new();
    let mut catalog = LocalConversationCatalog::default();
    catalog.create(first, 1);
    catalog.create(second, 2);
    let directory = std::env::current_dir().unwrap().display().to_string();
    catalog
        .set_project_directory(first, Some(directory.clone()), 3)
        .unwrap();
    catalog
        .set_project_directory(second, Some(directory), 4)
        .unwrap();
    let mut permissions = ProjectMemoryGrants::default();
    assert!(!permissions.allows(&catalog, first, second));
    permissions.grant(&catalog, first, second).unwrap();
    assert!(permissions.allows(&catalog, first, second));
    assert!(!permissions.allows(&catalog, second, first));
    assert!(permissions.revoke(first, second));
    assert!(!permissions.allows(&catalog, first, second));
}

#[test]
fn project_changes_fail_closed_and_cannot_create_cross_project_grants() {
    let first = chatarium_core::LocalConversationId::new();
    let second = chatarium_core::LocalConversationId::new();
    let mut catalog = LocalConversationCatalog::default();
    catalog.create(first, 1);
    catalog.create(second, 2);
    let directory = std::env::current_dir().unwrap().display().to_string();
    catalog
        .set_project_directory(first, Some(directory.clone()), 3)
        .unwrap();
    catalog
        .set_project_directory(second, Some(directory), 4)
        .unwrap();
    let mut permissions = ProjectMemoryGrants::default();
    permissions.grant(&catalog, first, second).unwrap();
    catalog
        .set_project_directory(second, Some(std::env::temp_dir().display().to_string()), 5)
        .unwrap();
    assert!(!permissions.allows(&catalog, first, second));
    assert!(permissions.grant(&catalog, first, second).is_err());
}

#[test]
fn only_latest_confirmed_per_authorized_source_is_selected() {
    let first = chatarium_core::LocalConversationId::new();
    let second = chatarium_core::LocalConversationId::new();
    let third = chatarium_core::LocalConversationId::new();
    let mut catalog = LocalConversationCatalog::default();
    for (id, n) in [(first, 1), (second, 2), (third, 3)] {
        catalog.create(id, n);
        catalog
            .set_project_directory(
                id,
                Some(std::env::current_dir().unwrap().display().to_string()),
                n + 10,
            )
            .unwrap();
    }
    let mut permissions = ProjectMemoryGrants::default();
    permissions.grant(&catalog, first, third).unwrap();
    let mut outbox = Outbox::default();
    // Selection checks only durable state; remote calls are not needed.
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "chatarium-project-scope-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join(crate::openviking_checkpoint_outbox::FILE_NAME);
    outbox.activate_durable(&path, 0).unwrap();
    for (owner, turn, seq, confirmed) in [
        (first, "old", 1, true),
        (second, "foreign", 2, true),
        (first, "uncertain", 3, false),
        (first, "new", 4, true),
    ] {
        let owner = owner.to_string();
        outbox.enqueue_durable(&path, &owner, turn, seq).unwrap();
        outbox
            .save_summary_durable(&path, &owner, turn, seq, "summary")
            .unwrap();
        if confirmed {
            outbox
                .transition_durable(&path, &owner, turn, seq, State::Ready, State::Dispatched)
                .unwrap();
            outbox
                .transition_durable(
                    &path,
                    &owner,
                    turn,
                    seq,
                    State::Dispatched,
                    State::Confirmed,
                )
                .unwrap();
        }
    }
    let chosen = confirmed_project_sources(&outbox, &catalog, &permissions, third);
    assert_eq!(chosen.len(), 1);
    assert_eq!(chosen[0].turn_id, "new");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn granted_project_history_preserves_newest_and_recalls_old_relevant_memory() {
    let source = chatarium_core::LocalConversationId::new();
    let foreign = chatarium_core::LocalConversationId::new();
    let destination = chatarium_core::LocalConversationId::new();
    let mut catalog = LocalConversationCatalog::default();
    let dir = std::env::current_dir().unwrap().display().to_string();
    for (id, sequence) in [(source, 1), (foreign, 2), (destination, 3)] {
        catalog.create(id, sequence);
        catalog
            .set_project_directory(id, Some(dir.clone()), sequence + 10)
            .unwrap();
    }
    let mut grants = ProjectMemoryGrants::default();
    grants.grant(&catalog, source, destination).unwrap();

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "chatarium-project-history-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join(crate::openviking_checkpoint_outbox::FILE_NAME);
    let mut outbox = Outbox::default();
    outbox.activate_durable(&path, 0).unwrap();
    for (owner, turn, seq, summary, confirm) in [
        (
            source,
            "old-rust",
            1,
            "Rust ownership borrow checker decision",
            true,
        ),
        (
            foreign,
            "foreign",
            2,
            "Rust ownership private controller data",
            true,
        ),
        (source, "old-ui", 3, "UI typography spacing decision", true),
        (
            source,
            "unconfirmed",
            4,
            "Rust ownership unconfirmed",
            false,
        ),
        (source, "recent", 5, "Newer weather checkpoint", true),
        (
            source,
            "latest",
            6,
            "Latest work and outstanding tasks",
            true,
        ),
    ] {
        let owner = owner.to_string();
        outbox.enqueue_durable(&path, &owner, turn, seq).unwrap();
        outbox
            .save_summary_durable(&path, &owner, turn, seq, summary)
            .unwrap();
        if confirm {
            outbox
                .transition_durable(&path, &owner, turn, seq, State::Ready, State::Dispatched)
                .unwrap();
            outbox
                .transition_durable(
                    &path,
                    &owner,
                    turn,
                    seq,
                    State::Dispatched,
                    State::Confirmed,
                )
                .unwrap();
        }
    }

    let selected = select_confirmed_project_history(
        &outbox,
        &path,
        &catalog,
        &grants,
        destination,
        "Review Rust ownership",
    )
    .unwrap();
    let names: Vec<_> = selected
        .iter()
        .map(|(entry, _)| entry.turn_id.as_str())
        .collect();
    assert_eq!(names, ["latest", "old-rust"]);
    assert!(!names.contains(&"foreign"));
    assert!(!names.contains(&"unconfirmed"));

    let mut remote_reads = Vec::new();
    let retrieved = retrieve_confirmed_for_project_with_query_and_read(
        &path,
        &catalog,
        &grants,
        destination,
        "Review Rust ownership",
        |uri, owner, turn| {
            assert_eq!(owner, source.to_string());
            assert_eq!(uri, checkpoint_uri(owner, turn).unwrap());
            remote_reads.push(turn.to_owned());
            let path = Outbox::summary_path(&path, owner, turn).unwrap();
            let bytes = std::fs::read_to_string(path).unwrap();
            Ok(json!({"content": [{"type": "text", "text": bytes}]}))
        },
    )
    .unwrap();
    assert_eq!(remote_reads, ["latest", "old-rust"]);
    assert_eq!(retrieved.len(), 2);
    assert!(
        retrieved
            .iter()
            .all(|memory| memory.source_conversation_id == source.to_string())
    );

    // Empty query keeps the previous newest-per-source behavior.
    let no_query =
        select_confirmed_project_history(&outbox, &path, &catalog, &grants, destination, "")
            .unwrap();
    assert_eq!(no_query.len(), 1);
    assert_eq!(no_query[0].0.turn_id, "latest");

    assert!(grants.revoke(source, destination));
    let revoked = retrieve_confirmed_for_project_with_query_and_read(
        &path,
        &catalog,
        &grants,
        destination,
        "Rust ownership",
        |_, _, _| panic!("revoked project must never issue a remote read"),
    )
    .unwrap();
    assert!(revoked.is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn historical_project_read_mismatch_fails_closed() {
    let source = chatarium_core::LocalConversationId::new();
    let dest = chatarium_core::LocalConversationId::new();
    let mut catalog = LocalConversationCatalog::default();
    let project = std::env::current_dir().unwrap().display().to_string();
    for (id, n) in [(source, 1), (dest, 2)] {
        catalog.create(id, n);
        catalog
            .set_project_directory(id, Some(project.clone()), n + 2)
            .unwrap();
    }
    let mut grants = ProjectMemoryGrants::default();
    grants.grant(&catalog, source, dest).unwrap();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "chatarium-project-drift-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join(crate::openviking_checkpoint_outbox::FILE_NAME);
    let mut outbox = Outbox::default();
    outbox.activate_durable(&path, 0).unwrap();
    outbox
        .enqueue_durable(&path, &source.to_string(), "turn", 1)
        .unwrap();
    outbox
        .save_summary_durable(&path, &source.to_string(), "turn", 1, "verified Rust plan")
        .unwrap();
    outbox
        .transition_durable(
            &path,
            &source.to_string(),
            "turn",
            1,
            State::Ready,
            State::Dispatched,
        )
        .unwrap();
    outbox
        .transition_durable(
            &path,
            &source.to_string(),
            "turn",
            1,
            State::Dispatched,
            State::Confirmed,
        )
        .unwrap();
    let error = retrieve_confirmed_for_project_with_query_and_read(
        &path,
        &catalog,
        &grants,
        dest,
        "Rust",
        |_, _, _| Ok(json!({"content": [{"type": "text", "text": "TAMPERED"}]})),
    )
    .unwrap_err();
    assert!(error.contains("differs from local evidence"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn source_scoped_transport_read_cannot_impersonate_destination() {
    let source = chatarium_core::LocalConversationId::new().to_string();
    let destination = chatarium_core::LocalConversationId::new().to_string();
    let turn = "verified-turn";
    let uri = checkpoint_uri(&source, turn).unwrap();
    let mut recorded = Vec::new();
    let value = read_project_checkpoint_as_source(
        &source,
        turn,
        &uri,
        |identity, call_key, provider, operation, args| {
            recorded.push((
                identity.to_owned(),
                call_key.to_owned(),
                provider.to_owned(),
                operation.to_owned(),
                args.clone(),
            ));
            Ok(json!({"content": [{"type":"text", "text":"exact"}]}))
        },
    )
    .unwrap();
    assert!(readback_matches(&value, "exact"));
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].0, source);
    assert_ne!(recorded[0].0, destination);
    assert_eq!(
        recorded[0].1,
        format!("memory-project-read:{source}:{turn}")
    );
    assert_eq!(recorded[0].2, local_http_mcp::OPENVIKING);
    assert_eq!(recorded[0].3, "read");
    assert_eq!(recorded[0].4, json!({"uris":uri,"limit":100}));

    // Forged owner/URI pairs must be rejected before any transport call.
    let mut invoked = false;
    assert!(
        read_project_checkpoint_as_source(&destination, turn, &uri, |_, _, _, _, _| {
            invoked = true;
            Ok(Value::Null)
        },)
        .is_err()
    );
    assert!(!invoked);
}

/// A complete synthetic host lifecycle: durable outbox, one create,
/// eventual read-back, confirmation, restarted grants, exact foreign-chat
/// admission, and revocation. No actual OpenViking data is modified.
#[test]
fn openviking_checkpoint_create_verify_restore_and_authorized_recall() {
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    let source = chatarium_core::LocalConversationId::new();
    let destination = chatarium_core::LocalConversationId::new();
    let other = chatarium_core::LocalConversationId::new();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "chatarium-openviking-full-host-{}-{nonce}",
        std::process::id()
    ));
    let shared = root.join("project");
    let unrelated = root.join("other");
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::create_dir_all(&unrelated).unwrap();
    let mut catalog = LocalConversationCatalog::default();
    for (id, directory) in [
        (source, &shared),
        (destination, &shared),
        (other, &unrelated),
    ] {
        catalog.create(id, 1);
        catalog
            .set_project_directory(id, Some(directory.display().to_string()), 2)
            .unwrap();
    }
    let owner = source.to_string();
    let turn = "turn-1";
    let sequence = 42;
    let outbox_path = root.join(crate::openviking_checkpoint_outbox::FILE_NAME);
    let grants_path = root.join(PROJECT_MEMORY_GRANTS_FILE_NAME);
    let mut outbox = Outbox::default();
    outbox.activate_durable(&outbox_path, 0).unwrap();
    assert!(
        outbox
            .enqueue_durable(&outbox_path, &owner, turn, sequence)
            .unwrap()
    );
    outbox
        .save_summary_durable(
            &outbox_path,
            &owner,
            turn,
            sequence,
            "The Rust OpenViking integration requires verified recall.",
        )
        .unwrap();
    outbox
        .transition_durable(
            &outbox_path,
            &owner,
            turn,
            sequence,
            State::Ready,
            State::Dispatched,
        )
        .unwrap();
    let body =
        std::fs::read_to_string(Outbox::summary_path(&outbox_path, &owner, turn).unwrap()).unwrap();
    let remote = RefCell::new(BTreeMap::<String, String>::new());
    let mut writes = 0;
    let mut reads = 0;
    deliver_create_once_with(
        &owner,
        turn,
        &body,
        |request_owner, _, provider, operation, args| {
            assert_eq!(request_owner, owner);
            assert_eq!(provider, local_http_mcp::OPENVIKING);
            match operation {
                "write" => {
                    writes += 1;
                    assert_eq!(args["mode"], "create");
                    assert_eq!(args["wait"], false);
                    let uri = args["uri"].as_str().unwrap().to_owned();
                    assert!(remote.borrow_mut().insert(uri, body.clone()).is_none());
                    Ok(json!({"content": [{"type": "text", "text": "accepted"}]}))
                }
                "read" => {
                    reads += 1;
                    let uri = args["uris"].as_str().unwrap();
                    if reads == 1 {
                        return Err("async checkpoint is not visible yet".to_owned());
                    }
                    let text = remote.borrow().get(uri).unwrap().clone();
                    Ok(json!({"content": [{"type": "text", "text": text}]}))
                }
                _ => panic!("only create and read are permitted"),
            }
        },
        |_| {},
    )
    .unwrap();
    assert_eq!((writes, reads), (1, 2));
    outbox
        .transition_durable(
            &outbox_path,
            &owner,
            turn,
            sequence,
            State::Dispatched,
            State::Confirmed,
        )
        .unwrap();
    assert_eq!(
        Outbox::load(&outbox_path).unwrap().entries()[0].state,
        State::Confirmed
    );

    let mut grants = ProjectMemoryGrants::default();
    let before_grant = retrieve_confirmed_for_project_with_read(
        &outbox_path,
        &catalog,
        &grants,
        destination,
        |_, _, _| panic!("an ungranted source must never be read"),
    )
    .unwrap();
    assert!(before_grant.is_empty());

    grants.grant(&catalog, source, destination).unwrap();
    grants.save_atomic(&grants_path).unwrap();
    let mut restarted_grants = ProjectMemoryGrants::load(&grants_path).unwrap();
    let mut retrievals = 0;
    let memories = retrieve_confirmed_for_project_with_read(
        &outbox_path,
        &catalog,
        &restarted_grants,
        destination,
        |uri, request_owner, request_turn| {
            retrievals += 1;
            assert_eq!(request_owner, owner);
            assert_eq!(request_turn, turn);
            let expected = remote.borrow().get(uri).unwrap().clone();
            Ok(json!({"content": [{"type": "text", "text": expected}]}))
        },
    )
    .unwrap();
    assert_eq!(retrievals, 1);
    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0].text, body);
    assert_eq!(memories[0].source_journal_sequence, sequence);
    assert!(
        crate::context_composer::TranscriptMessage::verified_project_openviking_checkpoint(
            &memories[0],
            destination,
            &catalog,
            &restarted_grants,
        )
        .unwrap()
        .text
        .contains("verified recall")
    );
    assert!(
        crate::context_composer::TranscriptMessage::verified_project_openviking_checkpoint(
            &memories[0],
            other,
            &catalog,
            &restarted_grants,
        )
        .is_err()
    );

    assert!(restarted_grants.revoke(source, destination));
    restarted_grants.save_atomic(&grants_path).unwrap();
    let revoked = ProjectMemoryGrants::load(&grants_path).unwrap();
    assert!(
        crate::context_composer::TranscriptMessage::verified_project_openviking_checkpoint(
            &memories[0],
            destination,
            &catalog,
            &revoked,
        )
        .is_err()
    );
    assert!(
        retrieve_confirmed_for_project_with_read(
            &outbox_path,
            &catalog,
            &revoked,
            destination,
            |_, _, _| panic!("revoked source must not trigger a read"),
        )
        .unwrap()
        .is_empty()
    );
    assert_eq!(writes, 1);
    std::fs::remove_dir_all(root).unwrap();
}

/// Exercises durable checkpoint creation, exact URI retrieval, explicit
/// cross-chat permission, model context admission, revocation and restart
/// with a fake OpenViking read backend. Never contacts a real service.
#[test]
fn project_checkpoint_fake_backend_roundtrip_and_revocation() {
    use std::collections::BTreeMap;
    let source = chatarium_core::LocalConversationId::new();
    let destination = chatarium_core::LocalConversationId::new();
    let unrelated = chatarium_core::LocalConversationId::new();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "chatarium-project-roundtrip-{}-{nonce}",
        std::process::id()
    ));
    let shared = root.join("shared");
    let foreign = root.join("foreign");
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::create_dir_all(&foreign).unwrap();
    let mut catalog = LocalConversationCatalog::default();
    for (id, dir) in [
        (source, &shared),
        (destination, &shared),
        (unrelated, &foreign),
    ] {
        catalog.create(id, 1);
        catalog
            .set_project_directory(id, Some(dir.display().to_string()), 2)
            .unwrap();
    }
    let outbox_path = root.join(crate::openviking_checkpoint_outbox::FILE_NAME);
    let grant_path = root.join(PROJECT_MEMORY_GRANTS_FILE_NAME);
    let mut outbox = Outbox::default();
    outbox.activate_durable(&outbox_path, 0).unwrap();

    let mut fake_server = BTreeMap::<String, String>::new();
    for (owner, turn, sequence, confirmed) in [
        (source, "older", 10, true),
        (source, "latest", 12, true),
        (source, "uncertain", 14, false),
        (unrelated, "foreign-secret", 99, true),
    ] {
        let identity = owner.to_string();
        outbox
            .enqueue_durable(&outbox_path, &identity, turn, sequence)
            .unwrap();
        outbox
            .save_summary_durable(
                &outbox_path,
                &identity,
                turn,
                sequence,
                "Durable project fact: Rust client",
            )
            .unwrap();
        if confirmed {
            outbox
                .transition_durable(
                    &outbox_path,
                    &identity,
                    turn,
                    sequence,
                    State::Ready,
                    State::Dispatched,
                )
                .unwrap();
            outbox
                .transition_durable(
                    &outbox_path,
                    &identity,
                    turn,
                    sequence,
                    State::Dispatched,
                    State::Confirmed,
                )
                .unwrap();
            let path = Outbox::summary_path(&outbox_path, &identity, turn).unwrap();
            let uri = checkpoint_uri(&identity, turn).unwrap();
            fake_server.insert(uri, std::fs::read_to_string(path).unwrap());
        }
    }
    let mut grants = ProjectMemoryGrants::default();
    let mut attempted = Vec::new();
    let before_grant = retrieve_confirmed_for_project_with_read(
        &outbox_path,
        &catalog,
        &grants,
        destination,
        |uri, _, _| {
            attempted.push(uri.to_owned());
            Err("a read must never occur without a grant".to_owned())
        },
    )
    .unwrap();
    assert!(before_grant.is_empty());
    assert!(attempted.is_empty());

    grants.grant(&catalog, source, destination).unwrap();
    grants.save_atomic(&grant_path).unwrap();
    let mut grants = ProjectMemoryGrants::load(&grant_path).unwrap();
    let mut observed = Vec::new();
    let retrieved = retrieve_confirmed_for_project_with_read(
        &outbox_path,
        &catalog,
        &grants,
        destination,
        |uri, owner, turn| {
            observed.push((uri.to_owned(), owner.to_owned(), turn.to_owned()));
            let remote = fake_server.get(uri).expect("fake OpenViking immutable URI");
            Ok(json!({"content": [{"type": "text", "text": remote}]}))
        },
    )
    .unwrap();
    assert_eq!(retrieved.len(), 1);
    assert_eq!(retrieved[0].source_conversation_id, source.to_string());
    assert_eq!(retrieved[0].source_turn_id, "latest");
    assert_eq!(retrieved[0].source_journal_sequence, 12);
    assert_eq!(
        observed,
        vec![(
            checkpoint_uri(&source.to_string(), "latest").unwrap(),
            source.to_string(),
            "latest".to_owned(),
        )]
    );
    assert!(!retrieved[0].text.contains("foreign-secret"));
    let admitted =
        crate::context_composer::TranscriptMessage::verified_project_openviking_checkpoint(
            &retrieved[0],
            destination,
            &catalog,
            &grants,
        )
        .unwrap();
    assert!(admitted.text.contains("Durable project fact: Rust client"));
    assert!(admitted.text.contains("explicitly granted project-shared"));
    assert!(
        crate::context_composer::TranscriptMessage::verified_project_openviking_checkpoint(
            &retrieved[0],
            unrelated,
            &catalog,
            &grants,
        )
        .is_err()
    );

    // A successful read may remain cached after explicit revocation.
    // Context assembly must re-check authorization rather than trust it.
    assert!(grants.revoke(source, destination));
    grants.save_atomic(&grant_path).unwrap();
    let revoked = ProjectMemoryGrants::load(&grant_path).unwrap();
    assert!(
        crate::context_composer::TranscriptMessage::verified_project_openviking_checkpoint(
            &retrieved[0],
            destination,
            &catalog,
            &revoked,
        )
        .is_err()
    );
    let mut calls = 0;
    let after_revocation = retrieve_confirmed_for_project_with_read(
        &outbox_path,
        &catalog,
        &revoked,
        destination,
        |_, _, _| {
            calls += 1;
            Err("revoked read".to_owned())
        },
    )
    .unwrap();
    assert!(after_revocation.is_empty());
    assert_eq!(calls, 0);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn fake_backend_rejects_remote_drift_and_offline_errors() {
    let source = chatarium_core::LocalConversationId::new();
    let destination = chatarium_core::LocalConversationId::new();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "chatarium-project-drift-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let mut catalog = LocalConversationCatalog::default();
    for id in [source, destination] {
        catalog.create(id, 1);
        catalog
            .set_project_directory(id, Some(root.display().to_string()), 2)
            .unwrap();
    }
    let outbox_path = root.join(crate::openviking_checkpoint_outbox::FILE_NAME);
    let mut outbox = Outbox::default();
    outbox.activate_durable(&outbox_path, 0).unwrap();
    let owner = source.to_string();
    outbox
        .enqueue_durable(&outbox_path, &owner, "first", 1)
        .unwrap();
    outbox
        .save_summary_durable(&outbox_path, &owner, "first", 1, "Correct memory")
        .unwrap();
    outbox
        .transition_durable(
            &outbox_path,
            &owner,
            "first",
            1,
            State::Ready,
            State::Dispatched,
        )
        .unwrap();
    outbox
        .transition_durable(
            &outbox_path,
            &owner,
            "first",
            1,
            State::Dispatched,
            State::Confirmed,
        )
        .unwrap();
    let mut grants = ProjectMemoryGrants::default();
    grants.grant(&catalog, source, destination).unwrap();

    let drift = retrieve_confirmed_for_project_with_read(
        &outbox_path,
        &catalog,
        &grants,
        destination,
        |_, _, _| Ok(json!({"content": [{"type": "text", "text": "Tampered"}]})),
    );
    assert!(drift.unwrap_err().contains("differs from local evidence"));

    let offline = retrieve_confirmed_for_project_with_read(
        &outbox_path,
        &catalog,
        &grants,
        destination,
        |_, _, _| Err("service is offline".to_owned()),
    );
    assert!(offline.unwrap_err().contains("offline"));
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn project_grant_saves_are_private_atomic_and_ignore_predictable_symlink_staging() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "chatarium-private-grants-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join(PROJECT_MEMORY_GRANTS_FILE_NAME);
    let victim = root.join("other-local-file");
    std::fs::write(&victim, "leave this unrelated file alone").unwrap();

    // Older implementations used a predictable temporary path. A user
    // or stale process may have left a symlink there; never follow it.
    let predictable = path.with_extension("json.tmp");
    symlink(&victim, &predictable).unwrap();

    let grants = ProjectMemoryGrants::default();
    grants.save_atomic(&path).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::read_to_string(&victim).unwrap(),
        "leave this unrelated file alone"
    );
    assert_eq!(std::fs::read_link(&predictable).unwrap(), victim);
    assert!(ProjectMemoryGrants::load(&path).unwrap().grants.is_empty());

    // Replacing an older, overly permissive file must not preserve the
    // old permissions or change unrelated staged paths.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    grants.save_atomic(&path).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let remaining = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(remaining.len(), 3);
    assert!(remaining.contains(&"other-local-file".to_owned()));
    assert!(remaining.contains(&"openviking-project-memory-grants.json".to_owned()));
    assert!(remaining.contains(&"openviking-project-memory-grants.json.tmp".to_owned()));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn revoked_project_grant_is_absent_after_durable_restart() {
    let source = chatarium_core::LocalConversationId::new();
    let destination = chatarium_core::LocalConversationId::new();
    let mut catalog = LocalConversationCatalog::default();
    catalog.create(source, 1);
    catalog.create(destination, 2);
    let directory = std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
    for id in [source, destination] {
        catalog
            .set_project_directory(id, Some(directory.to_str().unwrap().to_owned()), 3)
            .unwrap();
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "chatarium-grant-restart-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join(PROJECT_MEMORY_GRANTS_FILE_NAME);

    let mut grants = ProjectMemoryGrants::default();
    grants.grant(&catalog, source, destination).unwrap();
    grants.save_atomic(&path).unwrap();
    assert!(
        ProjectMemoryGrants::load(&path)
            .unwrap()
            .allows(&catalog, source, destination)
    );

    assert!(grants.revoke(source, destination));
    grants.save_atomic(&path).unwrap();
    let restarted = ProjectMemoryGrants::load(&path).unwrap();
    assert!(!restarted.allows(&catalog, source, destination));
    assert!(!restarted.has_sources(&catalog, destination));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn grants_roundtrip_and_reject_tampered_schema() {
    let first = chatarium_core::LocalConversationId::new();
    let second = chatarium_core::LocalConversationId::new();
    let mut catalog = LocalConversationCatalog::default();
    catalog.create(first, 1);
    catalog.create(second, 2);
    for id in [first, second] {
        catalog
            .set_project_directory(
                id,
                Some(std::env::current_dir().unwrap().display().to_string()),
                3,
            )
            .unwrap();
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "chatarium-project-grants-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join(PROJECT_MEMORY_GRANTS_FILE_NAME);
    let mut grants = ProjectMemoryGrants::default();
    grants.grant(&catalog, first, second).unwrap();
    grants.save_atomic(&file).unwrap();
    let loaded = ProjectMemoryGrants::load(&file).unwrap();
    assert_eq!(grants, loaded);
    std::fs::write(&file, r#"{"schema":"other","version":1,"grants":[]}"#).unwrap();
    assert!(ProjectMemoryGrants::load(&file).is_err());
    std::fs::remove_dir_all(root).unwrap();
}
