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

// Use the same bounded lexical rule for historical preselection and context
// ordering. Owner/turn metadata must never increase a checkpoint's score.
use crate::openviking_memory_delivery::{checkpoint_relevance, relevance_terms};

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

    /// Combine exact-owner and explicitly granted project checkpoints for one
    /// request. The existing final authority checks run BEFORE ranking.
    ///
    /// When the composer exceeds its context budget it examines OpenViking
    /// checkpoint messages from the END of the transcript backward. Place
    /// relevant evidence last so it wins that bounded, whole-document selection.
    /// Unmatched/empty queries preserve ordinary admission order.
    pub fn admitted_relevant_context(
        &self,
        own: Option<&[RetrievedCheckpoint]>,
        destination: LocalConversationId,
        catalog: &LocalConversationCatalog,
        grants: &ProjectMemoryGrants,
        query: &str,
    ) -> Vec<crate::context_composer::TranscriptMessage> {
        let mut evidence = Vec::<(usize, crate::context_composer::TranscriptMessage)>::new();
        let terms = relevance_terms(query);
        for checkpoint in own.into_iter().flatten() {
            if let Ok(message) =
                crate::context_composer::TranscriptMessage::verified_openviking_checkpoint(
                    checkpoint,
                    &destination.to_string(),
                )
            {
                evidence.push((checkpoint_relevance(&checkpoint.text, &terms), message));
            }
        }
        for checkpoint in self.checkpoints(destination).into_iter().flatten() {
            if let Ok(source) = checkpoint.source_conversation_id.parse::<LocalConversationId>()
                && grants.allows(catalog, source, destination)
                && let Ok(message) =
                    crate::context_composer::TranscriptMessage::verified_project_openviking_checkpoint(
                        checkpoint,
                        destination,
                        catalog,
                        grants,
                    )
            {
                evidence.push((checkpoint_relevance(&checkpoint.text, &terms), message));
            }
        }
        if !terms.is_empty() && evidence.iter().any(|(score, _)| *score > 0) {
            // Stable sort keeps deterministic original order among ties.
            evidence.sort_by_key(|(score, _)| *score);
        }
        evidence.into_iter().map(|(_, message)| message).collect()
    }

    /// A background worker must not dispatch with a memory snapshot that is
    /// still being populated. Failed/offline reads use ordinary retry backoff
    /// and must not permanently block a durable coordination wake.
    pub fn is_in_flight(&self, destination: LocalConversationId) -> bool {
        self.in_flight.contains_key(&destination)
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

    fn verified_checkpoint(
        owner: LocalConversationId,
        turn: &str,
        sequence: u64,
        body: &str,
    ) -> RetrievedCheckpoint {
        RetrievedCheckpoint {
            source_conversation_id: owner.to_string(),
            source_turn_id: turn.to_owned(),
            source_journal_sequence: sequence,
            uri: crate::openviking_memory_delivery::checkpoint_uri(&owner.to_string(), turn)
                .unwrap(),
            text: format!(
                "# Chatarium memory checkpoint\n\nConversation: {owner}\nTurn: {turn}\nSource journal sequence: {sequence}\n\n{body}\n"
            ),
        }
    }

    #[test]
    fn query_relevance_orders_verified_project_memory_without_widening_grants() {
        let (catalog, mut grants, source, destination) = fixture();
        let own = verified_checkpoint(
            destination,
            "my-turn",
            4,
            "Calendar review and meeting reminders",
        );
        let project = verified_checkpoint(
            source,
            "project-turn",
            9,
            "Rust ownership checkpoint and borrow checker decisions",
        );
        let mut cache = RetrievalCache::default();
        let now = Instant::now();
        let generation = cache.begin(destination, now).unwrap();
        assert_eq!(
            cache.finish(
                destination,
                generation,
                Ok(vec![project.clone()]),
                now,
                &catalog,
                &grants,
            ),
            Completion::Loaded
        );
        let baseline = cache.admitted_relevant_context(
            Some(std::slice::from_ref(&own)),
            destination,
            &catalog,
            &grants,
            "",
        );
        assert_eq!(baseline.len(), 2);
        assert!(baseline[0].text.contains("Calendar review"));
        assert!(baseline[1].text.contains("Rust ownership"));

        // A relevant project checkpoint moves to the last (highest-budget-
        // priority) position, but its exact source identity is preserved.
        let ranked = cache.admitted_relevant_context(
            Some(std::slice::from_ref(&own)),
            destination,
            &catalog,
            &grants,
            "Can you recall the calendar meeting reminders?",
        );
        assert_eq!(ranked.len(), 2);
        assert!(ranked[0].text.contains("Rust ownership"));
        assert!(ranked[1].text.contains("Calendar review"));
        assert!(ranked[0].text.contains(&source.to_string()));
        assert!(ranked[1].text.contains(&destination.to_string()));

        // Integration: the real bounded composer examines checkpoint evidence
        // from the end of the snapshot. The matching checkpoint must survive
        // a budget where only one complete historical source can fit.
        use crate::context_composer::{
            ContextPlan, ContextPolicy, TranscriptMessage, TranscriptRole,
        };
        let question =
            TranscriptMessage::durable(TranscriptRole::User, "Recall the calendar reminders", 17);
        let limit = question.text.len() + ranked[1].text.len() + 20;
        let plan = ContextPlan::compose(
            ContextPolicy::dispatch(),
            "",
            "",
            std::iter::once(question).chain(ranked.iter().cloned()),
        )
        .within_utf8_budget(limit)
        .unwrap();
        assert!(
            plan.messages
                .iter()
                .any(|m| m.content.contains("Calendar review"))
        );
        assert!(
            !plan
                .messages
                .iter()
                .any(|m| m.content.contains("Rust ownership"))
        );
        assert!(plan.size.omitted_items > 0);

        let no_match = cache.admitted_relevant_context(
            Some(std::slice::from_ref(&own)),
            destination,
            &catalog,
            &grants,
            "unrelated galaxies",
        );
        assert_eq!(no_match, baseline);

        assert!(grants.revoke(source, destination));
        let after_revoke = cache.admitted_relevant_context(
            Some(std::slice::from_ref(&own)),
            destination,
            &catalog,
            &grants,
            "rust ownership",
        );
        assert_eq!(after_revoke.len(), 1);
        assert!(after_revoke[0].text.contains("Calendar review"));
        assert!(!after_revoke[0].text.contains("Rust ownership"));
    }

    #[test]
    fn query_terms_are_bounded_casefolded_and_ignore_metadata_noise() {
        let evidence = verified_checkpoint(
            LocalConversationId::new(),
            "turn-1",
            10,
            "Español México: TÉRMINOS de lingüística y Rust 3090.",
        );
        let terms = relevance_terms("¿Qué sabemos de RUST y lingüística en México?");
        assert!(checkpoint_relevance(&evidence.text, &terms) >= 3);
        assert_eq!(
            checkpoint_relevance(
                &evidence.text,
                &relevance_terms("conversation turn journal")
            ),
            0,
        );
        assert!(relevance_terms(&"token ".repeat(1000)).len() <= 1);
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
    fn inactive_worker_uses_only_own_or_directly_granted_verified_memory() {
        use crate::context_composer::{
            ContextPlan, ContextPolicy, ContextSource, TranscriptMessage, TranscriptRole,
        };
        let (mut catalog, mut grants, source, worker) = fixture();
        let controller = LocalConversationId::new();
        catalog.create(controller, 3);
        catalog
            .set_project_directory(
                controller,
                Some(std::env::current_dir().unwrap().display().to_string()),
                4,
            )
            .unwrap();
        // Merely sharing a project directory is not a directed memory grant.
        assert!(!grants.allows(&catalog, controller, worker));
        let own = verified_checkpoint(worker, "own", 2, "worker local identity context");
        let granted = verified_checkpoint(source, "shared", 3, "authorized shared decision");
        let foreign = verified_checkpoint(controller, "secret", 4, "controller private strategy");

        let mut cache = RetrievalCache::default();
        let now = Instant::now();
        let generation = cache.begin(worker, now).unwrap();
        assert_eq!(
            cache.finish(
                worker,
                generation,
                Ok(vec![foreign.clone(), granted.clone()]),
                now,
                &catalog,
                &grants,
            ),
            Completion::Loaded
        );
        let own_and_foreign = [own.clone(), foreign.clone()];
        let context = cache.admitted_relevant_context(
            Some(&own_and_foreign),
            worker,
            &catalog,
            &grants,
            "Recall authorized shared decision for worker",
        );
        assert_eq!(context.len(), 2);
        assert!(
            context
                .iter()
                .any(|entry| entry.text.contains("worker local identity"))
        );
        assert!(
            context
                .iter()
                .any(|entry| entry.text.contains("authorized shared decision"))
        );
        assert!(
            context
                .iter()
                .all(|entry| !entry.text.contains("controller private strategy"))
        );
        assert!(context.iter().all(|entry| {
            matches!(entry.source, ContextSource::OpenVikingCheckpoint { .. })
                && entry.role == TranscriptRole::User
        }));

        let mut transcript = vec![TranscriptMessage::durable(
            TranscriptRole::User,
            "Worker's latest durable directive",
            100,
        )];
        transcript.extend(context);
        let plan =
            ContextPlan::compose_bounded(ContextPolicy::dispatch(), "", "", transcript).unwrap();
        assert_eq!(plan.openviking_checkpoint_count(), 2);
        assert!(
            plan.messages
                .iter()
                .any(|entry| { entry.content == "Worker's latest durable directive" })
        );
        assert!(
            !plan
                .messages
                .iter()
                .any(|entry| { entry.content.contains("controller private strategy") })
        );

        // Revocation after prefetch removes project sharing at final admission.
        assert!(grants.revoke(source, worker));
        let revoked = cache.admitted_relevant_context(
            Some(&own_and_foreign),
            worker,
            &catalog,
            &grants,
            "authorized shared decision",
        );
        assert_eq!(revoked.len(), 1);
        assert!(revoked[0].text.contains("worker local identity"));

        // A new controller grant cannot resurrect a stale worker snapshot.
        grants.grant(&catalog, controller, worker).unwrap();
        cache.invalidate();
        let after_epoch = cache.admitted_relevant_context(
            Some(&own_and_foreign),
            worker,
            &catalog,
            &grants,
            "controller private strategy",
        );
        assert_eq!(after_epoch.len(), 1);
        assert!(after_epoch[0].text.contains("worker local identity"));
    }

    #[test]
    fn prefetch_in_flight_is_distinct_from_offline_retry_backoff() {
        let (catalog, grants, _, worker) = fixture();
        let now = Instant::now();
        let mut cache = RetrievalCache::default();
        assert!(!cache.is_in_flight(worker));
        let epoch = cache.begin(worker, now).unwrap();
        assert!(cache.is_in_flight(worker));
        assert_eq!(
            cache.finish(
                worker,
                epoch,
                Err("offline".to_owned()),
                now,
                &catalog,
                &grants
            ),
            Completion::Deferred("offline".to_owned())
        );
        assert!(!cache.is_in_flight(worker));
        assert!(!cache.can_schedule(worker, now));
        assert!(cache.can_schedule(worker, now + RETRY_DELAY));
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
