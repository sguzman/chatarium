//! Behavior-preserving extraction of openviking methods from the desktop application.
use super::super::super::*;

impl ChatariumApp {
    pub(crate) fn observe_openviking_query(
        &mut self,
        owner: LocalConversationId,
        query: &str,
        interactive: bool,
    ) -> bool {
        let (changed, ready) =
            self.openviking_query_tracker
                .observe(owner, query, Instant::now(), interactive);
        if changed {
            self.openviking_retrieval_cache.remove(&owner);
            self.openviking_retrieval_retry_after.remove(&owner);
            if self.openviking_retrieval_in_flight.contains(&owner) {
                self.openviking_retrieval_stale.insert(owner);
            }
            self.openviking_project_cache.invalidate_destination(owner);
        }
        ready
    }

    /// Schedule a source-owner-scoped read on the persistence worker.
    /// Return false only while a read is outstanding, so offline OpenViking
    /// never indefinitely blocks a non-authored coordination wake.
    pub(crate) fn maybe_retrieve_openviking_context_for(
        &mut self,
        owner: LocalConversationId,
        query: &str,
        interactive: bool,
    ) -> bool {
        if std::env::var("CHATARIUM_OPENVIKING_AUTOCAPTURE").as_deref() == Ok("0") {
            return true;
        }
        if !self.observe_openviking_query(owner, query, interactive) {
            return true;
        }
        if self.openviking_retrieval_cache.contains_key(&owner) {
            return true;
        }
        if self.openviking_retrieval_in_flight.contains(&owner) {
            return false;
        }
        if self
            .openviking_retrieval_retry_after
            .get(&owner)
            .is_some_and(|next| Instant::now() < *next)
        {
            return true;
        }
        let Some(sender) = &self.persist_tx else {
            return true;
        };
        if sender
            .send(PersistCommand::RetrieveConfirmedOpenViking {
                conversation_id: owner,
                query: query.chars().take(4096).collect(),
            })
            .is_ok()
        {
            self.openviking_retrieval_in_flight.insert(owner);
            return false;
        }
        true
    }

    /// Shared foreground/worker path: only the destination's exact directed
    /// project grants are considered. A failed/offline read uses backoff and
    /// permits the wake without unverified memory.
    pub(crate) fn maybe_retrieve_project_openviking_context_for(
        &mut self,
        destination: LocalConversationId,
        query: &str,
        interactive: bool,
    ) -> bool {
        if std::env::var("CHATARIUM_OPENVIKING_AUTOCAPTURE").as_deref() == Ok("0") {
            return true;
        }
        if !self.observe_openviking_query(destination, query, interactive) {
            return true;
        }
        if !self
            .openviking_project_cache
            .can_schedule(destination, Instant::now())
        {
            return !self.openviking_project_cache.is_in_flight(destination);
        }
        if !self
            .project_memory_grants
            .has_sources(&self.local_conversation_catalog, destination)
        {
            return true;
        }
        let Some(sender) = &self.persist_tx else {
            return true;
        };
        let Some(generation) = self
            .openviking_project_cache
            .begin(destination, Instant::now())
        else {
            return !self.openviking_project_cache.is_in_flight(destination);
        };
        if sender
            .send(PersistCommand::RetrieveConfirmedProjectOpenViking {
                conversation_id: destination,
                generation,
                catalog: self.local_conversation_catalog.clone(),
                grants: self.project_memory_grants.clone(),
                query: query.chars().take(4096).collect(),
            })
            .is_ok()
        {
            return false;
        }
        self.openviking_project_cache
            .cancel(destination, generation);
        true
    }

    /// Foreground prefetch is also used by inactive workers, but each worker
    /// retains its own conversation identity, journal and directed grants.
    pub(crate) fn maybe_retrieve_openviking_context(&mut self) {
        let owner = self.local_conversation_id;
        // Querying the verified local history is done only in the background
        // worker, not on the UI thread. If an authored draft exists, use it;
        // otherwise use the last durable user message for this conversation.
        // Query-dependent caches invalidate when the normalized topic changes.
        let interactive = !self.draft.trim().is_empty();
        let query = if interactive {
            self.draft.clone()
        } else {
            projected_local_display_messages(&self.events, owner)
                .into_iter()
                .rev()
                .find(|message| matches!(message.role, DisplayRole::User))
                .map_or_else(String::new, |message| message.text)
        };
        self.maybe_retrieve_openviking_context_for(owner, &query, interactive);
    }

    pub(crate) fn maybe_retrieve_project_openviking_context(&mut self) {
        let owner = self.local_conversation_id;
        let interactive = !self.draft.trim().is_empty();
        let query = if interactive {
            self.draft.clone()
        } else {
            projected_local_display_messages(&self.events, owner)
                .into_iter()
                .rev()
                .find(|message| matches!(message.role, DisplayRole::User))
                .map_or_else(String::new, |message| message.text)
        };
        self.maybe_retrieve_project_openviking_context_for(owner, &query, interactive);
    }

    pub(crate) fn background_worker_memory_ready(
        &mut self,
        worker: LocalConversationId,
        directive: &str,
    ) -> bool {
        let own_ready = self.maybe_retrieve_openviking_context_for(worker, directive, false);
        let project_ready =
            self.maybe_retrieve_project_openviking_context_for(worker, directive, false);
        own_ready && project_ready
    }
}
