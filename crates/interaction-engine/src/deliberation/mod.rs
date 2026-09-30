//! Deliberation: the planner's requests and what may be done with its answers
//! (design 12). It coordinates; it does not decide. Whatever comes back is a
//! proposal that TaskRuntime checks and installs or refuses.

use interaction_contracts::desktop::ids::{RequestId, RootTaskId};
use interaction_contracts::desktop::planning::{
    PlannerOutput, PlanningContext, PlanningReason, PlanningRequest,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InFlight {
    pub request_id: RequestId,
    pub generation: u64,
    pub root: RootTaskId,
    pub reason: PlanningReason,
    pub deadline_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaleReason {
    /// Nothing is waiting for an answer.
    NoRequest,
    /// It answers some other request.
    WrongRequest,
    /// A newer request replaced the one this answers.
    OldGeneration,
    Expired,
    /// A goal draft turned up outside goal compilation, or the wrong kind of answer.
    WrongPhase,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Arrival {
    Accepted(Box<PlannerOutput>),
    Stale(StaleReason),
}

#[derive(Debug, Default)]
pub struct PlanningCoordinator {
    inflight: Option<InFlight>,
    generation: u64,
    seq: u64,
    dropped: u64,
}

impl PlanningCoordinator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn inflight(&self) -> Option<&InFlight> {
        self.inflight.as_ref()
    }

    /// How many answers were ignored as stale, for diagnostics.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Ask the planner. There is at most one request in flight: a new one
    /// replaces the old, whose answer can then never be installed.
    #[allow(clippy::too_many_arguments)]
    pub fn request(
        &mut self,
        root: &RootTaskId,
        task_revision: u64,
        root_epoch: u64,
        reason: PlanningReason,
        context: PlanningContext,
        now_ms: u64,
        timeout_ms: u64,
    ) -> PlanningRequest {
        self.generation += 1;
        self.seq += 1;
        let request = PlanningRequest {
            request_id: RequestId::new(format!("plan-{}", self.seq))
                .expect("a counter is a valid id"),
            generation: self.generation,
            root_task_id: root.clone(),
            task_revision,
            root_epoch,
            reason,
            context,
            deadline_ms: now_ms.saturating_add(timeout_ms),
        };
        self.inflight = Some(InFlight {
            request_id: request.request_id.clone(),
            generation: request.generation,
            root: root.clone(),
            reason,
            deadline_ms: request.deadline_ms,
        });
        request
    }

    /// An answer arrived. It counts only if it is for the request that is
    /// still waiting, in time, and of the kind that phase allows.
    pub fn arrived(
        &mut self,
        request_id: &RequestId,
        generation: u64,
        output: PlannerOutput,
        now_ms: u64,
    ) -> Arrival {
        let stale = |this: &mut Self, reason| {
            this.dropped += 1;
            Arrival::Stale(reason)
        };
        let Some(inflight) = self.inflight.clone() else {
            return stale(self, StaleReason::NoRequest);
        };
        if &inflight.request_id != request_id {
            return stale(
                self,
                if generation < inflight.generation {
                    StaleReason::OldGeneration
                } else {
                    StaleReason::WrongRequest
                },
            );
        }
        if generation != inflight.generation {
            return stale(self, StaleReason::OldGeneration);
        }
        if now_ms > inflight.deadline_ms {
            self.inflight = None;
            return stale(self, StaleReason::Expired);
        }
        if !Self::phase_allows(inflight.reason, &output) {
            // The request is over either way; a wrong answer does not leave it pending.
            self.inflight = None;
            return stale(self, StaleReason::WrongPhase);
        }
        self.inflight = None;
        Arrival::Accepted(Box::new(output))
    }

    /// A goal draft belongs to goal compilation only, and compilation takes
    /// a plan and nothing else.
    fn phase_allows(reason: PlanningReason, output: &PlannerOutput) -> bool {
        match (reason, output) {
            (PlanningReason::GoalCompile, PlannerOutput::Plan { .. }) => true,
            (PlanningReason::GoalCompile, _) => false,
            (_, PlannerOutput::Plan { proposal }) => proposal.goal_draft.is_none(),
            _ => true,
        }
    }

    /// Give up on whatever is waiting, for example because the root was cancelled.
    pub fn cancel(&mut self) -> Option<InFlight> {
        self.inflight.take()
    }

    /// The request that ran out of time, if any.
    pub fn expired(&mut self, now_ms: u64) -> Option<InFlight> {
        if self
            .inflight
            .as_ref()
            .is_some_and(|i| now_ms > i.deadline_ms)
        {
            return self.inflight.take();
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use interaction_contracts::desktop::planning::{PlanBundle, PlanProposal, ReviewProposal};
    use sim_world::policy::ScriptedPlanner;

    fn context() -> PlanningContext {
        PlanningContext {
            user_text: "turn it off".into(),
            unknown_items: vec![],
            allowed_skills: vec![],
            recent_failures: vec![],
            notes: vec![],
        }
    }

    fn ask(
        coordinator: &mut PlanningCoordinator,
        reason: PlanningReason,
        now: u64,
    ) -> PlanningRequest {
        coordinator.request(
            &RootTaskId::new("root-1").unwrap(),
            1,
            1,
            reason,
            context(),
            now,
            1_000,
        )
    }

    fn compile_answer() -> PlannerOutput {
        ScriptedPlanner::default().plan_text(r#"Turn off "Daily picks" in "Push""#)
    }

    fn plan_without_goal() -> PlannerOutput {
        PlannerOutput::Plan {
            proposal: Box::new(PlanProposal {
                goal_draft: None,
                bundle: PlanBundle {
                    subgoals: vec![],
                    dependencies: vec![],
                    required_observations: vec![],
                    expected_effects: vec![],
                    recovery_branches: vec![],
                    termination: vec![],
                },
            }),
        }
    }

    #[test]
    fn the_answer_to_the_waiting_request_is_accepted_once() {
        let mut coordinator = PlanningCoordinator::new();
        let request = ask(&mut coordinator, PlanningReason::GoalCompile, 0);
        assert!(matches!(
            coordinator.arrived(
                &request.request_id,
                request.generation,
                compile_answer(),
                10
            ),
            Arrival::Accepted(_)
        ));
        assert!(coordinator.inflight().is_none());
        // A duplicate delivery of the same answer finds nothing waiting.
        assert_eq!(
            coordinator.arrived(
                &request.request_id,
                request.generation,
                compile_answer(),
                11
            ),
            Arrival::Stale(StaleReason::NoRequest)
        );
        assert_eq!(coordinator.dropped(), 1);
    }

    #[test]
    fn a_newer_request_replaces_the_old_and_the_old_answer_can_never_be_installed() {
        let mut coordinator = PlanningCoordinator::new();
        let first = ask(&mut coordinator, PlanningReason::Escalation, 0);
        let second = ask(&mut coordinator, PlanningReason::Escalation, 5);
        assert!(second.generation > first.generation);
        assert_eq!(
            coordinator.arrived(&first.request_id, first.generation, plan_without_goal(), 10),
            Arrival::Stale(StaleReason::OldGeneration)
        );
        assert_eq!(
            coordinator.inflight().unwrap().request_id,
            second.request_id,
            "the new request still waits"
        );
        assert!(matches!(
            coordinator.arrived(
                &second.request_id,
                second.generation,
                plan_without_goal(),
                11
            ),
            Arrival::Accepted(_)
        ));
    }

    #[test]
    fn a_generation_that_does_not_match_its_own_request_is_stale() {
        let mut coordinator = PlanningCoordinator::new();
        let request = ask(&mut coordinator, PlanningReason::Escalation, 0);
        assert_eq!(
            coordinator.arrived(
                &request.request_id,
                request.generation + 1,
                plan_without_goal(),
                1
            ),
            Arrival::Stale(StaleReason::OldGeneration)
        );
    }

    #[test]
    fn an_answer_after_the_deadline_is_refused_and_ends_the_request() {
        let mut coordinator = PlanningCoordinator::new();
        let request = ask(&mut coordinator, PlanningReason::GoalCompile, 0);
        assert_eq!(
            coordinator.arrived(
                &request.request_id,
                request.generation,
                compile_answer(),
                5_000
            ),
            Arrival::Stale(StaleReason::Expired)
        );
        assert!(coordinator.inflight().is_none());
    }

    #[test]
    fn a_goal_draft_is_only_valid_while_compiling_the_goal() {
        let mut coordinator = PlanningCoordinator::new();
        let request = ask(&mut coordinator, PlanningReason::Escalation, 0);
        // The scripted planner's answer carries a goal draft.
        assert_eq!(
            coordinator.arrived(&request.request_id, request.generation, compile_answer(), 1),
            Arrival::Stale(StaleReason::WrongPhase)
        );
        // And compilation refuses anything that is not a plan.
        let request = ask(&mut coordinator, PlanningReason::GoalCompile, 2);
        let review = PlannerOutput::Review {
            proposal: ReviewProposal {
                notes: vec![],
                evidence: vec![],
            },
        };
        assert_eq!(
            coordinator.arrived(&request.request_id, request.generation, review, 3),
            Arrival::Stale(StaleReason::WrongPhase)
        );
    }

    #[test]
    fn cancelling_and_timing_out_clear_the_waiting_request() {
        let mut coordinator = PlanningCoordinator::new();
        ask(&mut coordinator, PlanningReason::Escalation, 0);
        assert!(coordinator.expired(500).is_none(), "not yet");
        assert!(coordinator.expired(1_500).is_some());
        assert!(coordinator.inflight().is_none());
        ask(&mut coordinator, PlanningReason::Escalation, 0);
        assert!(coordinator.cancel().is_some());
        assert!(coordinator.cancel().is_none());
    }
}
