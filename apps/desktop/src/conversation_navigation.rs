//! Keyboard-first navigation between native local conversations.
//!
//! Navigation uses a stable creation order, not last-opened order. Switching
//! tabs updates the active catalog's timestamp and must never reorder the
//! list underneath an ongoing Ctrl+Tab sequence.

use chatarium_core::LocalConversationId;

use super::local_conversations::LocalConversationEntry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CycleDirection {
    Forward,
    Backward,
}

/// All unarchived conversation identities in stable newest-created order.
/// Modified timestamps and mutable titles never change the cycle order.
pub fn cycle_order(entries: &[LocalConversationEntry]) -> Vec<LocalConversationId> {
    let mut eligible = entries
        .iter()
        .filter(|entry| !entry.archived)
        .collect::<Vec<_>>();
    eligible.sort_by(|left, right| {
        right
            .created_at_unix_ms
            .cmp(&left.created_at_unix_ms)
            .then_with(|| left.id.to_string().cmp(&right.id.to_string()))
    });
    eligible.into_iter().map(|entry| entry.id).collect()
}

/// Resolve a keyboard switch without changing the catalog, current draft,
/// remote inference or anything else. A remote/mirror view may use current
/// None to select the newest native conversation instead of guessing.
pub fn cycle_target(
    entries: &[LocalConversationEntry],
    current: Option<LocalConversationId>,
    direction: CycleDirection,
) -> Option<LocalConversationId> {
    let ordered = cycle_order(entries);
    if ordered.is_empty() {
        return None;
    }
    let Some(index) = current.and_then(|current| ordered.iter().position(|id| *id == current))
    else {
        return ordered.first().copied();
    };
    if ordered.len() == 1 {
        return None;
    }
    let target_index = match direction {
        CycleDirection::Forward => (index + 1) % ordered.len(),
        CycleDirection::Backward => (index + ordered.len() - 1) % ordered.len(),
    };
    ordered.get(target_index).copied()
}

#[cfg(test)]
mod tests {
    use super::super::local_conversations::LocalConversationCatalog;
    use super::*;

    #[test]
    fn cycle_wraps_both_directions_and_skips_archived_without_restoring() {
        let a = LocalConversationId::new();
        let b = LocalConversationId::new();
        let c = LocalConversationId::new();
        let mut catalog = LocalConversationCatalog::default();
        catalog.create(a, 1);
        catalog.create(b, 2);
        catalog.create(c, 3);
        catalog.set_archived(b, true, 4).unwrap();
        let entries = catalog.entries();
        assert_eq!(cycle_order(&entries), vec![c, a]);
        assert_eq!(
            cycle_target(&entries, Some(c), CycleDirection::Forward),
            Some(a)
        );
        assert_eq!(
            cycle_target(&entries, Some(a), CycleDirection::Forward),
            Some(c)
        );
        assert_eq!(
            cycle_target(&entries, Some(a), CycleDirection::Backward),
            Some(c)
        );
        assert_eq!(
            cycle_target(&entries, Some(c), CycleDirection::Backward),
            Some(a)
        );
        assert!(catalog.entry(b).unwrap().archived);
    }

    #[test]
    fn opening_a_chat_does_not_reorder_keyboard_cycle() {
        let first = LocalConversationId::new();
        let second = LocalConversationId::new();
        let third = LocalConversationId::new();
        let mut catalog = LocalConversationCatalog::default();
        catalog.create(first, 1);
        catalog.create(second, 2);
        catalog.create(third, 3);
        let before = cycle_order(&catalog.entries());
        catalog.set_active(first, 100_000).unwrap();
        assert_eq!(cycle_order(&catalog.entries()), before);
        catalog.set_active(second, 200_000).unwrap();
        assert_eq!(cycle_order(&catalog.entries()), before);
        assert_eq!(
            cycle_target(&catalog.entries(), Some(first), CycleDirection::Forward),
            Some(third)
        );
    }

    #[test]
    fn unknown_and_mirror_selection_can_open_newest_native_without_guessing() {
        let older = LocalConversationId::new();
        let newer = LocalConversationId::new();
        let foreign = LocalConversationId::new();
        let mut catalog = LocalConversationCatalog::default();
        catalog.create(older, 1);
        catalog.create(newer, 2);
        for direction in [CycleDirection::Forward, CycleDirection::Backward] {
            assert_eq!(
                cycle_target(&catalog.entries(), None, direction),
                Some(newer)
            );
            assert_eq!(
                cycle_target(&catalog.entries(), Some(foreign), direction),
                Some(newer)
            );
        }
    }

    #[test]
    fn zero_or_one_unarchived_chat_never_switches_to_self() {
        let first = LocalConversationId::new();
        let mut catalog = LocalConversationCatalog::default();
        assert_eq!(
            cycle_target(&catalog.entries(), None, CycleDirection::Forward),
            None
        );
        catalog.create(first, 1);
        assert_eq!(
            cycle_target(&catalog.entries(), Some(first), CycleDirection::Forward),
            None
        );
        assert_eq!(
            cycle_target(&catalog.entries(), None, CycleDirection::Forward),
            Some(first)
        );
        catalog.set_archived(first, true, 2).unwrap();
        assert_eq!(
            cycle_target(&catalog.entries(), Some(first), CycleDirection::Backward),
            None
        );
    }

    #[test]
    fn equal_creation_timestamps_use_stable_identity_tiebreaker() {
        let a = LocalConversationId::new();
        let b = LocalConversationId::new();
        let mut catalog = LocalConversationCatalog::default();
        catalog.create(a, 1);
        catalog.create(b, 1);
        let before = cycle_order(&catalog.entries());
        assert_eq!(before.len(), 2);
        catalog
            .rename(a, Some("Different title".to_owned()), 5)
            .unwrap();
        catalog.set_active(b, 8).unwrap();
        assert_eq!(cycle_order(&catalog.entries()), before);
    }
}
