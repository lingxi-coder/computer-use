//! The root budget (design 13.4): one ledger spent by attempts, observations,
//! recoveries and replans alike, never reset by a retry or a restart.

use interaction_contracts::desktop::task::{AttemptQuota, RootBudgetSpec};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetKind {
    Attempt,
    Observation,
    Recovery,
    Replan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetExhausted(pub BudgetKind);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetState {
    pub spec: RootBudgetSpec,
    pub attempts_used: u32,
    pub observations_used: u32,
    pub recoveries_used: u32,
    pub replans_used: u32,
}

impl BudgetState {
    pub fn new(spec: RootBudgetSpec) -> Self {
        Self {
            spec,
            attempts_used: 0,
            observations_used: 0,
            recoveries_used: 0,
            replans_used: 0,
        }
    }

    fn limit(&self, kind: BudgetKind) -> u32 {
        match kind {
            BudgetKind::Attempt => self.spec.max_attempts,
            BudgetKind::Observation => self.spec.max_observations,
            BudgetKind::Recovery => self.spec.max_recoveries,
            BudgetKind::Replan => self.spec.max_replans,
        }
    }

    fn used(&self, kind: BudgetKind) -> u32 {
        match kind {
            BudgetKind::Attempt => self.attempts_used,
            BudgetKind::Observation => self.observations_used,
            BudgetKind::Recovery => self.recoveries_used,
            BudgetKind::Replan => self.replans_used,
        }
    }

    fn used_mut(&mut self, kind: BudgetKind) -> &mut u32 {
        match kind {
            BudgetKind::Attempt => &mut self.attempts_used,
            BudgetKind::Observation => &mut self.observations_used,
            BudgetKind::Recovery => &mut self.recoveries_used,
            BudgetKind::Replan => &mut self.replans_used,
        }
    }

    pub fn remaining(&self, kind: BudgetKind) -> u32 {
        self.limit(kind).saturating_sub(self.used(kind))
    }

    pub fn can_spend(&self, kind: BudgetKind, amount: u32) -> bool {
        self.remaining(kind) >= amount
    }

    pub fn spend(&mut self, kind: BudgetKind, amount: u32) -> Result<(), BudgetExhausted> {
        if !self.can_spend(kind, amount) {
            return Err(BudgetExhausted(kind));
        }
        *self.used_mut(kind) += amount;
        Ok(())
    }

    /// Give back what an attempt did not use. Never goes below zero.
    pub fn refund(&mut self, kind: BudgetKind, amount: u32) {
        let used = self.used_mut(kind);
        *used = used.saturating_sub(amount);
    }

    /// The most an attempt may be granted right now.
    pub fn grantable(&self, asked: AttemptQuota) -> Option<AttemptQuota> {
        if self.remaining(BudgetKind::Attempt) == 0 {
            return None;
        }
        Some(AttemptQuota {
            actions: asked.actions.max(1),
            observations: asked
                .observations
                .min(self.remaining(BudgetKind::Observation)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> RootBudgetSpec {
        RootBudgetSpec {
            deadline_ms: 60_000,
            max_attempts: 2,
            max_observations: 3,
            max_recoveries: 1,
            max_replans: 1,
        }
    }

    #[test]
    fn spending_stops_exactly_at_the_limit() {
        let mut budget = BudgetState::new(spec());
        assert!(budget.spend(BudgetKind::Attempt, 1).is_ok());
        assert!(budget.spend(BudgetKind::Attempt, 1).is_ok());
        assert_eq!(
            budget.spend(BudgetKind::Attempt, 1),
            Err(BudgetExhausted(BudgetKind::Attempt))
        );
        assert_eq!(budget.remaining(BudgetKind::Attempt), 0);
        assert_eq!(budget.attempts_used, 2, "a refused spend changes nothing");
    }

    #[test]
    fn kinds_are_independent_ledgers() {
        let mut budget = BudgetState::new(spec());
        budget.spend(BudgetKind::Observation, 3).unwrap();
        assert!(budget.can_spend(BudgetKind::Attempt, 2));
        assert!(!budget.can_spend(BudgetKind::Observation, 1));
        assert!(!budget.can_spend(BudgetKind::Recovery, 2));
    }

    #[test]
    fn a_refund_cannot_push_usage_below_zero() {
        let mut budget = BudgetState::new(spec());
        budget.spend(BudgetKind::Attempt, 1).unwrap();
        budget.refund(BudgetKind::Attempt, 5);
        assert_eq!(budget.attempts_used, 0);
    }

    #[test]
    fn an_attempt_is_granted_at_least_one_action_and_no_more_observations_than_remain() {
        let mut budget = BudgetState::new(spec());
        budget.spend(BudgetKind::Observation, 2).unwrap();
        let granted = budget
            .grantable(AttemptQuota {
                actions: 0,
                observations: 5,
            })
            .unwrap();
        assert_eq!(
            granted,
            AttemptQuota {
                actions: 1,
                observations: 1
            }
        );
        budget.spend(BudgetKind::Attempt, 2).unwrap();
        assert!(
            budget
                .grantable(AttemptQuota {
                    actions: 1,
                    observations: 1
                })
                .is_none()
        );
    }
}
