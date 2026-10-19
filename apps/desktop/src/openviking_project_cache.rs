//! Permission-epoch project-memory retrieval cache.
//!
//! An old worker can finish AFTER grant revocation, project-folder changes, or
//! a newly verified source checkpoint. A generation token makes its result
//! inadmissible even if permissions are later granted again. No GUI-network I/O.
use crate::local_conversations::LocalConversationCatalog;
use crate::openviking_memory_delivery::{ProjectMemoryGrants, RetrievedCheckpoint};
use chatarium_core::LocalConversationId;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

const RETRY_DELAY: Duration = Duration::from_secs(300);

#[derive(Debug, PartialEq, Eq)]
pub enum Completion {
    /// A result from an earlier epoch, or one with no outstanding request.
    Discarded,
    /// A successful fresh result, including an authorized empty result.
    Loaded,
    /// A fresh failed request; ordinary conversation work is unaffected.
    Deferred(String),
}

#[derive(Default)]
pub struct RetrievalCache {
    generation: u64,
    cached: BTreeMap<LocalConversationId, Vec<RetrievedCheckpoint>>,
    in_flight: BTreeMap<LocalConversationId, u64>,
    retry_after: BTreeMap<LocalConversationId, Instant>,
}

impl RetrievalCache {
    pub fn invalidate(&mut self) {
        // Even if a grant is revoked and regranted while an old request is
        // running, that request will not carry this newer generation.
        self.generation = self.generation.wrapping_add(1);
        self.cached.clear();
        self.retry_after.clear();
        // Retain old in-flight claims: do not issue concurrent network reads.
        // A stale completion frees its claim without filling the cache.
    }

    pub fn checkpoints(&self, destination: LocalConversationId) -> Option<&[RetrievedCheckpoint]> {
        self.cached.get(&destination).map(Vec::as_slice)
    }

    /// The exact shared-memory admission used by BOTH Send and preview.
    /// Verify the current directed grant, source URI and durable identity
    /// header for every cached checkpoint at the final request snapshot.
    pub fn admitted_context(
        &self,
        destination: LocalConversationId,
        catalog: &LocalConversationCatalog,
        grants: &ProjectMemoryGrants,
    ) -> Vec<crate::context_composer::TranscriptMessage> {
        self.checkpoints(destination)
            .into_iter()
            .flatten()
            .filter_map(|checkpoint| {
                crate::context_composer::TranscriptMessage::verified_project_openviking_checkpoint(
                    checkpoint,
                    destination,
                    catalog,
                    grants,
                )
                .ok()
            })
            .collect()
    }

    pub fn can_schedule(&self, destination: LocalConversationId, now: Instant) -> bool {
        !self.cached.contains_key(&destination)
            && !self.in_flight.contains_key(&destination)
            && !self
                .retry_after
                .get(&destination)
                .is_some_and(|after| now < *after)
    }

    /// Reserve exactly one read claim; publish its generation with the worker
    /// command and return the same generation in the completion notice.
    pub fn begin(&mut self, destination: LocalConversationId, now: Instant) -> Option<u64> {
        if !self.can_schedule(destination, now) {
            return None;
        }
        self.in_flight.insert(destination, self.generation);
        Some(self.generation)
    }

    /// Revert a claim when enqueueing to the background worker fails.
    pub fn cancel(&mut self, destination: LocalConversationId, generation: u64) {
        if self.in_flight.get(&destination) == Some(&generation) {
            self.in_flight.remove(&destination);
        }
    }

    /// Admit only the exact outstanding generation. Independently recheck
    /// grants against the CURRENT catalog, not the worker's old snapshots.
    pub fn finish(
        &mut self,
        destination: LocalConversationId,
        generation: u64,
        result: Result<Vec<RetrievedCheckpoint>, String>,
        now: Instant,
        catalog: &LocalConversationCatalog,
        grants: &ProjectMemoryGrants,
    ) -> Completion {
        if self.in_flight.get(&destination) != Some(&generation) {
            return Completion::Discarded;
        }
        self.in_flight.remove(&destination);
        if generation != self.generation {
            return Completion::Discarded;
        }
        match result {
            Ok(checkpoints) => {
                let permitted = checkpoints
                    .into_iter()
                    .filter(|checkpoint| {
                        checkpoint
                            .source_conversation_id
                            .parse::<LocalConversationId>()
                            .ok()
                            .is_some_and(|source| grants.allows(catalog, source, destination))
                    })
                    .collect();
                self.cached.insert(destination, permitted);
                self.retry_after.remove(&destination);
                Completion::Loaded
            }
            Err(error) => {
                self.retry_after.insert(destination, now + RETRY_DELAY);
                Completion::Deferred(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (
        LocalConversationCatalog,
        ProjectMemoryGrants,
        LocalConversationId,
        LocalConversationId,
    ) {
        let source = LocalConversationId::new();
        let dest = LocalConversationId::new();
        let mut catalog = LocalConversationCatalog::default();
        for (id, n) in [(source, 1), (dest, 2)] {
            catalog.create(id, n);
            catalog
                .set_project_directory(
                    id,
                    Some(std::env::current_dir().unwrap().display().to_string()),
                    n,
                )
                .unwrap();
        }
        let mut grants = ProjectMemoryGrants::default();
        grants.grant(&catalog, source, dest).unwrap();
        (catalog, grants, source, dest)
    }

    fn checkpoint(source: LocalConversationId) -> RetrievedCheckpoint {
        RetrievedCheckpoint {
            source_conversation_id: source.to_string(),
            source_turn_id: "turn-1".to_owned(),
            source_journal_sequence: 5,
            uri: "viking://~/chatarium/synthetic".to_owned(),
            text: "known local evidence".to_owned(),
        }
    }

    #[test]
    fn old_result_cannot_survive_revoke_then_regrant_while_in_flight() {
        let (catalog, mut grants, source, dest) = fixture();
        let now = Instant::now();
        let mut state = RetrievalCache::default();
        let generation = state.begin(dest, now).unwrap();
        grants.revoke(source, dest);
        state.invalidate();
        grants.grant(&catalog, source, dest).unwrap();
        state.invalidate();
        assert!(!state.can_schedule(dest, now));
        assert_eq!(
            state.finish(
                dest,
                generation,
                Ok(vec![checkpoint(source)]),
                now,
                &catalog,
                &grants
            ),
            Completion::Discarded
        );
        assert!(state.checkpoints(dest).is_none());
        let new_generation = state.begin(dest, now).unwrap();
        assert_ne!(new_generation, generation);
        assert_eq!(
            state.finish(
                dest,
                new_generation,
                Ok(vec![checkpoint(source)]),
                now,
                &catalog,
                &grants
            ),
            Completion::Loaded
        );
        assert_eq!(state.checkpoints(dest).unwrap().len(), 1);
    }

    #[test]
    fn stale_error_does_not_install_backoff_after_checkpoint_confirmation() {
        let (catalog, grants, _, dest) = fixture();
        let now = Instant::now();
        let mut state = RetrievalCache::default();
        let stale = state.begin(dest, now).unwrap();
        state.invalidate();
        assert_eq!(
            state.finish(
                dest,
                stale,
                Err("offline".to_owned()),
                now,
                &catalog,
                &grants
            ),
            Completion::Discarded
        );
        assert!(state.can_schedule(dest, now));
        let current = state.begin(dest, now).unwrap();
        assert_eq!(
            state.finish(
                dest,
                current,
                Err("offline".to_owned()),
                now,
                &catalog,
                &grants
            ),
            Completion::Deferred("offline".to_owned())
        );
        assert!(!state.can_schedule(dest, now));
        assert!(!state.can_schedule(dest, now + Duration::from_secs(299)));
        assert!(state.can_schedule(dest, now + Duration::from_secs(300)));
        state.invalidate();
        assert!(state.can_schedule(dest, now));
    }

    #[test]
    fn current_grant_is_checked_even_if_worker_result_was_verified() {
        let (catalog, mut grants, source, dest) = fixture();
        let now = Instant::now();
        let mut state = RetrievalCache::default();
        let current = state.begin(dest, now).unwrap();
        grants.revoke(source, dest);
        assert_eq!(
            state.finish(
                dest,
                current,
                Ok(vec![checkpoint(source)]),
                now,
                &catalog,
                &grants
            ),
            Completion::Loaded
        );
        assert!(state.checkpoints(dest).unwrap().is_empty());
    }

    /// Reopening reloads durable grants and project bindings but never trusts
    /// an old in-memory checkpoint. Only a new background result can restore
    /// shared context. Both preview and the actual Send snapshot use this
    /// exact admission method.
    #[test]
    fn restart_and_revocation_control_exact_model_context() {
        use crate::context_composer::{
            ContextPlan, ContextPolicy, ContextSource, TranscriptMessage, TranscriptRole,
        };
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "chatarium-project-resume-{}-{nonce}",
            std::process::id()
        ));
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let catalog_path = root.join("catalog.json");
        let grants_path = root.join("grants.json");
        let source = LocalConversationId::new();
        let dest = LocalConversationId::new();
        let foreign = LocalConversationId::new();
        let mut original = LocalConversationCatalog::default();
        for (id, folder, n) in [
            (source, project.as_path(), 1),
            (dest, project.as_path(), 2),
            (foreign, root.as_path(), 3),
        ] {
            original.create(id, n);
            original
                .set_project_directory(id, Some(folder.display().to_string()), n + 1)
                .unwrap();
        }
        original.save_atomic(&catalog_path).unwrap();
        let catalog = LocalConversationCatalog::load(&catalog_path).unwrap();
        let mut grants = ProjectMemoryGrants::default();
        crate::openviking_project_ui::change_permission_durable(
            &grants_path,
            &catalog,
            &mut grants,
            source,
            dest,
            true,
        )
        .unwrap();
        let mut grants = ProjectMemoryGrants::load(&grants_path).unwrap();
        let now = Instant::now();
        let mut cache = RetrievalCache::default();
        assert!(cache.admitted_context(dest, &catalog, &grants).is_empty());

        let evidence = RetrievedCheckpoint {
            source_conversation_id: source.to_string(),
            source_turn_id: "resumed-turn".to_owned(),
            source_journal_sequence: 35,
            uri: crate::openviking_memory_delivery::checkpoint_uri(
                &source.to_string(),
                "resumed-turn",
            )
            .unwrap(),
            text: format!(
                "# Chatarium memory checkpoint\n\nConversation: {source}\nTurn: resumed-turn\nSource journal sequence: 35\n\nShared milestone: beta passed."
            ),
        };
        let foreign_evidence = RetrievedCheckpoint {
            source_conversation_id: foreign.to_string(),
            ..evidence.clone()
        };
        let generation = cache.begin(dest, now).unwrap();
        assert_eq!(
            cache.finish(
                dest,
                generation,
                Ok(vec![evidence.clone(), foreign_evidence]),
                now,
                &catalog,
                &grants
            ),
            Completion::Loaded
        );

        let compose = |state: &RetrievalCache, grants: &ProjectMemoryGrants| {
            ContextPlan::compose_bounded(
                ContextPolicy::dispatch(),
                "",
                "",
                [TranscriptMessage::durable(
                    TranscriptRole::User,
                    "Resume this project",
                    50,
                )]
                .into_iter()
                .chain(state.admitted_context(dest, &catalog, grants)),
            )
            .unwrap()
        };
        let sent = compose(&cache, &grants);
        assert_eq!(sent.openviking_checkpoint_count(), 1);
        assert_eq!(sent.messages.len(), 2);
        assert_eq!(sent.messages[0].content, "Resume this project");
        assert_eq!(sent.messages[1].role, "user");
        assert!(
            sent.messages[1]
                .content
                .contains("Shared milestone: beta passed.")
        );
        assert!(
            sent.messages[1]
                .content
                .contains("explicitly granted project-shared")
        );
        assert!(matches!(
            &sent.messages[1].source,
            ContextSource::OpenVikingCheckpoint {
                source_journal_sequence: 35,
                ..
            }
        ));
        let preview = ContextPlan::compose_bounded(
            ContextPolicy::preview(),
            "",
            "",
            [TranscriptMessage::durable(
                TranscriptRole::User,
                "Resume this project",
                50,
            )]
            .into_iter()
            .chain(cache.admitted_context(dest, &catalog, &grants)),
        )
        .unwrap();
        assert_eq!(sent.messages, preview.messages);

        // Even before cache invalidation, every Send checks current grants.
        crate::openviking_project_ui::change_permission_durable(
            &grants_path,
            &catalog,
            &mut grants,
            source,
            dest,
            false,
        )
        .unwrap();
        let revoked = ProjectMemoryGrants::load(&grants_path).unwrap();
        assert_eq!(compose(&cache, &revoked).openviking_checkpoint_count(), 0);
        assert_eq!(compose(&cache, &revoked).messages.len(), 1);
        cache.invalidate();
        crate::openviking_project_ui::change_permission_durable(
            &grants_path,
            &catalog,
            &mut grants,
            source,
            dest,
            true,
        )
        .unwrap();
        let regranted = ProjectMemoryGrants::load(&grants_path).unwrap();
        assert_eq!(compose(&cache, &regranted).openviking_checkpoint_count(), 0);
        let new_generation = cache.begin(dest, now).unwrap();
        assert_eq!(
            cache.finish(
                dest,
                new_generation,
                Ok(vec![evidence]),
                now,
                &catalog,
                &regranted
            ),
            Completion::Loaded
        );
        assert_eq!(compose(&cache, &regranted).openviking_checkpoint_count(), 1);
        assert!(
            cache
                .admitted_context(source, &catalog, &regranted)
                .is_empty()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn canceled_or_duplicate_out_of_order_notices_cannot_admit_memories() {
        let (catalog, grants, source, dest) = fixture();
        let now = Instant::now();
        let mut state = RetrievalCache::default();
        let current = state.begin(dest, now).unwrap();
        state.cancel(dest, current);
        assert!(state.can_schedule(dest, now));
        assert_eq!(
            state.finish(
                dest,
                current,
                Ok(vec![checkpoint(source)]),
                now,
                &catalog,
                &grants
            ),
            Completion::Discarded
        );
        let current = state.begin(dest, now).unwrap();
        assert_eq!(
            state.finish(
                dest,
                current,
                Ok(vec![checkpoint(source)]),
                now,
                &catalog,
                &grants
            ),
            Completion::Loaded
        );
        assert_eq!(
            state.finish(
                dest,
                current,
                Err("duplicate".to_owned()),
                now,
                &catalog,
                &grants
            ),
            Completion::Discarded
        );
        assert_eq!(state.checkpoints(dest).unwrap().len(), 1);
    }
}
