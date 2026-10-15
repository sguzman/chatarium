//! A bounded coordination tool budget belongs to an individual remote turn,
//! not the lifetime of a selected conversation.
use chatarium_core::LocalTurnId;

#[derive(Debug, Clone, Default)]
pub struct Budget {
    turn: Option<LocalTurnId>,
    used: u32,
}

impl Budget {
    /// Admit one operation for this turn. A fresh turn starts a fresh budget;
    /// continuations of the same turn retain its existing bound.
    pub fn admit(&mut self, turn: LocalTurnId, max_calls: u32) -> bool {
        if self.turn != Some(turn) {
            self.turn = Some(turn);
            self.used = 0;
        }
        if self.used >= max_calls {
            return false;
        }
        self.used += 1;
        true
    }

    pub fn used(&self) -> u32 {
        self.used
    }

    pub fn reset(&mut self) {
        self.turn = None;
        self.used = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_new_turn_gets_its_own_budget() {
        let mut budget = Budget::default();
        let first = LocalTurnId::new();
        let second = LocalTurnId::new();
        assert!(budget.admit(first, 2));
        assert!(budget.admit(first, 2));
        assert!(!budget.admit(first, 2));
        assert!(budget.admit(second, 2));
        assert!(budget.admit(second, 2));
        assert!(!budget.admit(second, 2));
    }

    #[test]
    fn reset_does_not_change_the_limit_within_one_turn() {
        let mut budget = Budget::default();
        let first = LocalTurnId::new();
        assert!(!budget.admit(first, 0));
        assert!(budget.admit(first, 1));
        assert!(!budget.admit(first, 1));
        budget.reset();
        assert!(budget.admit(first, 1));
    }
}
