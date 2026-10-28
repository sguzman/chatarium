use super::*;

#[test]
fn cross_conversation_admission_requires_a_current_directed_grant() {
    let source = chatarium_core::LocalConversationId::new();
    let destination = chatarium_core::LocalConversationId::new();
    let foreign = chatarium_core::LocalConversationId::new();
    let mut catalog = crate::local_conversations::LocalConversationCatalog::default();
    for (id, n) in [(source, 1), (destination, 2), (foreign, 3)] {
        catalog.create(id, n);
        catalog
            .set_project_directory(
                id,
                Some(std::env::current_dir().unwrap().display().to_string()),
                n,
            )
            .unwrap();
    }
    let checkpoint = crate::openviking_memory_delivery::RetrievedCheckpoint {
        source_conversation_id: source.to_string(),
        source_turn_id: "turn-1".to_owned(),
        source_journal_sequence: 9,
        uri: crate::openviking_memory_delivery::checkpoint_uri(&source.to_string(), "turn-1")
            .unwrap(),
        text: format!(
            "# Chatarium memory checkpoint\n\nConversation: {source}\nTurn: turn-1\nSource journal sequence: 9\n\nThe project uses Rust."
        ),
    };
    let mut grants = crate::openviking_memory_delivery::ProjectMemoryGrants::default();
    assert!(
        TranscriptMessage::verified_project_openviking_checkpoint(
            &checkpoint,
            destination,
            &catalog,
            &grants,
        )
        .is_err()
    );
    grants.grant(&catalog, source, destination).unwrap();
    let admitted = TranscriptMessage::verified_project_openviking_checkpoint(
        &checkpoint,
        destination,
        &catalog,
        &grants,
    )
    .unwrap();
    assert!(admitted.text.contains("explicitly granted project-shared"));
    assert!(
        TranscriptMessage::verified_project_openviking_checkpoint(
            &checkpoint,
            foreign,
            &catalog,
            &grants,
        )
        .is_err()
    );
    let mut tampered = checkpoint.clone();
    tampered.text = "other conversation's data".to_owned();
    assert!(
        TranscriptMessage::verified_project_openviking_checkpoint(
            &tampered,
            destination,
            &catalog,
            &grants,
        )
        .is_err()
    );
    grants.revoke(source, destination);
    assert!(
        TranscriptMessage::verified_project_openviking_checkpoint(
            &checkpoint,
            destination,
            &catalog,
            &grants,
        )
        .is_err()
    );
}
