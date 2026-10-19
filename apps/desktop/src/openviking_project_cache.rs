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
