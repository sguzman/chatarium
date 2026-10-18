//! Deterministic arbitration among independently authorized wake leases.
use chatarium_core::LocalConversationId;

#[must_use]
pub fn oldest_candidate(candidates: &[(LocalConversationId, u64)]) -> Option<LocalConversationId> {
    candidates
        .iter()
        .min_by(|(a, id_a), (b, id_b)| {
            id_a.cmp(id_b)
                .then_with(|| a.to_string().cmp(&b.to_string()))
        })
        .map(|(conversation, _)| *conversation)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oldest_id_wins_regardless_of_input_order() {
        let a = LocalConversationId::new();
        let b = LocalConversationId::new();
        assert_eq!(oldest_candidate(&[]), None);
        assert_eq!(oldest_candidate(&[(a, 20), (b, 10)]), Some(b));
        assert_eq!(oldest_candidate(&[(b, 10), (a, 20)]), Some(b));
    }

    #[test]
    fn ties_resolve_by_conversation_identity() {
        let a = LocalConversationId::new();
        let b = LocalConversationId::new();
        let expected = if a.to_string() < b.to_string() { a } else { b };
        assert_eq!(oldest_candidate(&[(a, 1), (b, 1)]), Some(expected));
        assert_eq!(oldest_candidate(&[(b, 1), (a, 1)]), Some(expected));
    }
}
