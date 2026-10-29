//! Native reader preparation that never scans an entire transcript on egui's
//! render thread. Source identity is the immutable search-index Arc; stale
//! results from another conversation or query are not exposed.
use super::*;
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, TryRecvError};

type AsyncResult<T> = Arc<Mutex<Receiver<T>>>;

#[derive(Clone, Default)]
struct ReaderProjectionState {
    ready: Option<(
        Arc<NativeConversationSearchIndex>,
        LocalConversationId,
        Arc<Vec<DisplayMessage>>,
    )>,
    active: Option<(
        Arc<NativeConversationSearchIndex>,
        LocalConversationId,
        AsyncResult<Arc<Vec<DisplayMessage>>>,
    )>,
    failed: Option<(
        Arc<NativeConversationSearchIndex>,
        LocalConversationId,
        String,
    )>,
}

/// Return an activity-filtered native reader projection only once it is ready.
/// In particular, do not clone all message bodies during a GUI frame.
///
/// A new request waits for the prior worker to complete, but never exposes
/// its stale result. The unfiltered transcript is used directly when enabled.
pub fn background_without_activity(
    ctx: &egui::Context,
    index: &Arc<NativeConversationSearchIndex>,
    conversation: LocalConversationId,
) -> Result<Option<Arc<Vec<DisplayMessage>>>, String> {
    let id = egui::Id::new("chatarium-reader-without-activity");
    let mut state = ctx
        .data_mut(|data| data.get_temp::<ReaderProjectionState>(id))
        .unwrap_or_default();

    if let Some((source, owner, receiver)) = &state.active {
        match receiver.try_lock().ok().map(|lock| lock.try_recv()) {
            Some(Ok(filtered)) => {
                state.ready = Some((Arc::clone(source), *owner, filtered));
                state.active = None;
            }
            Some(Err(TryRecvError::Disconnected)) => {
                state.failed = Some((
                    Arc::clone(source),
                    *owner,
                    "native reader projection worker stopped unexpectedly".to_owned(),
                ));
                state.active = None;
            }
            _ => {}
        }
    }
    if let Some((source, owner, filtered)) = &state.ready {
        if Arc::ptr_eq(source, index) && *owner == conversation {
            let result = Arc::clone(filtered);
            ctx.data_mut(|data| data.insert_temp(id, state));
            return Ok(Some(result));
        }
    }
    if let Some((source, owner, error)) = &state.failed {
        if Arc::ptr_eq(source, index) && *owner == conversation {
            let error = error.clone();
            ctx.data_mut(|data| data.insert_temp(id, state));
            return Err(error);
        }
    }
    if state.active.is_none() {
        let source = Arc::clone(index);
        let messages = index.message_projection(conversation).unwrap_or_default();
        let repaint = ctx.clone();
        let (sender, receiver) = mpsc::channel();
        match std::thread::Builder::new()
            .name("chatarium-reader-filter".to_owned())
            .spawn(move || {
                // Retain the exact Arc when no activity card needs removing.
                let filtered = if messages
                    .iter()
                    .any(|message| message.role == DisplayRole::Activity)
                {
                    Arc::new(
                        messages
                            .iter()
                            .filter(|message| message.role != DisplayRole::Activity)
                            .cloned()
                            .collect(),
                    )
                } else {
                    messages
                };
                let _ = sender.send(filtered);
                repaint.request_repaint();
            }) {
            Ok(_handle) => {
                state.active = Some((
                    Arc::clone(index),
                    conversation,
                    Arc::new(Mutex::new(receiver)),
                ))
            }
            Err(error) => {
                let error = format!("failed to start native reader filter: {error}");
                state.failed = Some((source, conversation, error.clone()));
                ctx.data_mut(|data| data.insert_temp(id, state));
                return Err(error);
            }
        }
    }
    ctx.data_mut(|data| data.insert_temp(id, state));
    Ok(None)
}

#[derive(Clone, Default)]
struct ReaderHitsState {
    ready: Option<(
        Arc<Vec<DisplayMessage>>,
        String,
        Arc<Vec<(usize, usize, usize)>>,
    )>,
    active: Option<(
        Arc<Vec<DisplayMessage>>,
        String,
        AsyncResult<Arc<Vec<(usize, usize, usize)>>>,
    )>,
    failed: Option<(Arc<Vec<DisplayMessage>>, String, String)>,
}

/// Find native reader hits on a worker, keyed by exact source projection and
/// query. Empty-query hits are cheap and returned immediately. A stale request
/// never supplies navigation targets for the current query.
pub fn background_reader_hits(
    ctx: &egui::Context,
    messages: &Arc<Vec<DisplayMessage>>,
    query: &str,
) -> Result<Option<Arc<Vec<(usize, usize, usize)>>>, String> {
    if query.trim().is_empty() {
        return Ok(Some(Arc::new(Vec::new())));
    }
    let id = egui::Id::new("chatarium-reader-background-hits");
    let mut state = ctx
        .data_mut(|data| data.get_temp::<ReaderHitsState>(id))
        .unwrap_or_default();
    if let Some((source, term, receiver)) = &state.active {
        match receiver.try_lock().ok().map(|lock| lock.try_recv()) {
            Some(Ok(hits)) => {
                state.ready = Some((Arc::clone(source), term.clone(), hits));
                state.active = None;
            }
            Some(Err(TryRecvError::Disconnected)) => {
                state.failed = Some((
                    Arc::clone(source),
                    term.clone(),
                    "native reader search worker stopped unexpectedly".to_owned(),
                ));
                state.active = None;
            }
            _ => {}
        }
    }
    if let Some((source, term, hits)) = &state.ready {
        if Arc::ptr_eq(source, messages) && term == query {
            let result = Arc::clone(hits);
            ctx.data_mut(|data| data.insert_temp(id, state));
            return Ok(Some(result));
        }
    }
    if let Some((source, term, error)) = &state.failed {
        if Arc::ptr_eq(source, messages) && term == query {
            let error = error.clone();
            ctx.data_mut(|data| data.insert_temp(id, state));
            return Err(error);
        }
    }
    if state.active.is_none() {
        let source = Arc::clone(messages);
        let term = query.to_owned();
        let worker_messages = Arc::clone(messages);
        let worker_query = query.to_owned();
        let repaint = ctx.clone();
        let (sender, receiver) = mpsc::channel();
        match std::thread::Builder::new()
            .name("chatarium-reader-search".to_owned())
            .spawn(move || {
                let hits = worker_messages
                    .iter()
                    .enumerate()
                    .flat_map(|(position, message)| {
                        crate::offline_reader::search_hits(&message.text, &worker_query)
                            .into_iter()
                            .map(move |(start, end)| (position, start, end))
                    })
                    .collect::<Vec<_>>();
                let _ = sender.send(Arc::new(hits));
                repaint.request_repaint();
            }) {
            Ok(_handle) => state.active = Some((source, term, Arc::new(Mutex::new(receiver)))),
            Err(error) => {
                let error = format!("failed to start native reader search: {error}");
                state.failed = Some((source, term, error.clone()));
                ctx.data_mut(|data| data.insert_temp(id, state));
                return Err(error);
            }
        }
    }
    ctx.data_mut(|data| data.insert_temp(id, state));
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn wait_for<T>(mut request: impl FnMut() -> Result<Option<T>, String>) -> T {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match request().unwrap() {
                Some(result) => return result,
                None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(2)),
                None => panic!("reader background worker did not complete"),
            }
        }
    }

    fn message(text: &str, role: DisplayRole, sequence: u64) -> DisplayMessage {
        DisplayMessage {
            role,
            text: text.to_owned(),
            sequence,
            timestamp: None,
            provenance_label: None,
        }
    }

    #[test]
    fn background_hits_reject_old_queries_and_other_sources() {
        let ctx = egui::Context::default();
        let first = Arc::new(vec![
            message("alpha one", DisplayRole::User, 1),
            message("beta alpha", DisplayRole::Assistant, 2),
        ]);
        let result = wait_for(|| background_reader_hits(&ctx, &first, "alpha"));
        assert_eq!(result.len(), 2);
        assert!(Arc::ptr_eq(
            &result,
            &background_reader_hits(&ctx, &first, "alpha")
                .unwrap()
                .unwrap()
        ));
        assert!(
            background_reader_hits(&ctx, &first, "beta")
                .unwrap()
                .is_none()
        );
        let other = Arc::new(vec![message("foreign alpha", DisplayRole::User, 3)]);
        assert!(
            background_reader_hits(&ctx, &other, "alpha")
                .unwrap()
                .is_none()
        );
        let new = wait_for(|| background_reader_hits(&ctx, &other, "alpha"));
        assert_eq!(new.len(), 1);
        assert_eq!(new[0].0, 0);
        assert!(
            background_reader_hits(&ctx, &first, "")
                .unwrap()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn filtered_reader_projection_is_async_and_owner_scoped() {
        let ctx = egui::Context::default();
        let index = Arc::new(NativeConversationSearchIndex::build(&[]));
        let first = LocalConversationId::new();
        let other = LocalConversationId::new();
        assert!(
            background_without_activity(&ctx, &index, first)
                .unwrap()
                .is_none()
        );
        let empty = wait_for(|| background_without_activity(&ctx, &index, first));
        assert!(empty.is_empty());
        assert!(Arc::ptr_eq(
            &empty,
            &background_without_activity(&ctx, &index, first)
                .unwrap()
                .unwrap(),
        ));
        assert!(
            background_without_activity(&ctx, &index, other)
                .unwrap()
                .is_none()
        );
        let different = wait_for(|| background_without_activity(&ctx, &index, other));
        assert!(different.is_empty());
    }
}
