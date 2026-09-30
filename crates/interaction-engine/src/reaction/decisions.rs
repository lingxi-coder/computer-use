//! The decision book: the one open request, and the checks a proposal passes
//! before anything is admitted (design 10 and 11).
//!
//! A backend's answer is a proposal. It counts only for the request that is
//! still open, at its generation, over the candidate set it was asked about,
//! in time, while the things it depended on still hold. Anything else is
//! recorded and dropped; a dropped answer never becomes an action.

use interaction_contracts::desktop::decision::{
    AdmissionOutcome, Candidate, DecisionAdmission, DecisionCancel, DecisionFailure,
    DecisionProposal, DecisionRequest, DecisionSource, DecisionState, PolicyChoice, Precondition,
    Question, QuestionKind, QuestionOption,
};
use interaction_contracts::desktop::error::FailureCode;
use interaction_contracts::desktop::ids::{AttemptId, BindingRef, DecisionId, RootTaskId};

use super::candidates::{OBSERVE_MORE, REQUEST_PLANNING, STOP_OR_YIELD};
use super::store::ReactionStore;
use crate::environment::binding::BindingReducer;
use crate::environment::state_core::EnvSnapshot;
use crate::infrastructure::storage::{Storage, StoreError};
use crate::ports::DecisionLifecycle;

const LANE: &str = "lane-1";

/// What a decision depended on when it was asked. If any of it changes, the
/// answer is for a situation that no longer exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionBasis {
    pub layer_revision: u64,
    pub plan_revision: u64,
    pub task_revision: u64,
    pub root_epoch: u64,
    pub bindings: Vec<BindingRef>,
}

impl DecisionBasis {
    pub fn capture(
        env: &EnvSnapshot,
        bindings: &BindingReducer,
        plan_revision: u64,
        task_revision: u64,
        root_epoch: u64,
    ) -> Self {
        Self {
            layer_revision: env.layer_revision,
            plan_revision,
            task_revision,
            root_epoch,
            bindings: bindings
                .all()
                .map(|binding| binding.binding_ref.clone())
                .collect(),
        }
    }

    /// Why the situation this was asked in no longer holds, if it does not.
    /// A newer snapshot alone is not a reason; only what the answer depended
    /// on counts.
    pub fn broken_by(&self, now: &DecisionBasis) -> Option<&'static str> {
        if self.root_epoch != now.root_epoch {
            Some("the root was cancelled or restarted")
        } else if self.task_revision != now.task_revision {
            Some("the goal changed")
        } else if self.plan_revision != now.plan_revision {
            Some("the plan changed")
        } else if self.layer_revision != now.layer_revision {
            Some("the active layer changed")
        } else if self
            .bindings
            .iter()
            .any(|binding| !now.bindings.contains(binding))
        {
            Some("a binding the decision used is no longer valid")
        } else {
            None
        }
    }
}

/// The action a backend chose, held until the user confirms it.
#[derive(Debug, Clone, PartialEq)]
pub struct Held {
    pub candidate: Candidate,
    pub source: DecisionSource,
    pub backend: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    pub request: DecisionRequest,
    pub candidates: Vec<Candidate>,
    pub basis: DecisionBasis,
    pub subgoal: Option<String>,
    pub held: Option<Held>,
}

impl Pending {
    pub fn decision_id(&self) -> &DecisionId {
        &self.request.decision_id
    }

    pub fn generation(&self) -> u64 {
        self.request.request_generation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    ObserveMore,
    RequestPlanning,
    StopOrYield,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dropped {
    /// No such decision is open: a late or duplicate answer.
    Unknown,
    OldGeneration,
    DigestMismatch,
    Expired,
    Stale(&'static str),
    BackendFailed(DecisionFailure),
    Invalid(String),
}

impl std::fmt::Display for Dropped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown => f.write_str("no such decision is open"),
            Self::OldGeneration => f.write_str("the answer is for an older request"),
            Self::DigestMismatch => f.write_str("the answer is for another candidate set"),
            Self::Expired => f.write_str("the answer came after the deadline"),
            Self::Stale(reason) => write!(f, "stale: {reason}"),
            Self::BackendFailed(failure) => write!(f, "the backend failed: {failure:?}"),
            Self::Invalid(reason) => write!(f, "invalid proposal: {reason}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Start this candidate, subject to the effect gate and admission.
    Start {
        decision: DecisionId,
        generation: u64,
        candidate: Box<Candidate>,
        source: DecisionSource,
        backend: String,
    },
    Control {
        decision: DecisionId,
        control: Control,
        reason: String,
        source: DecisionSource,
    },
    Dropped(Dropped),
}

pub struct OpenSpec<'a> {
    pub root: &'a RootTaskId,
    pub task_revision: u64,
    pub plan_revision: u64,
    pub root_epoch: u64,
    pub program: &'a str,
    pub state: DecisionState,
    pub basis: DecisionBasis,
    pub subgoal: Option<String>,
    pub now_ms: u64,
    pub timeout_ms: u64,
}

/// The host's verdict as it is logged.
pub fn admission(
    decision: &DecisionId,
    outcome: AdmissionOutcome,
    selected: Option<&Candidate>,
    attempt: Option<&AttemptId>,
    source: DecisionSource,
    reason: &str,
    failure_code: Option<FailureCode>,
) -> DecisionAdmission {
    DecisionAdmission {
        decision_id: decision.clone(),
        outcome,
        selected: selected.map(|candidate| candidate.candidate_id.clone()),
        attempt: attempt.cloned(),
        source,
        reason: reason.to_string(),
        failure_code,
    }
}

#[derive(Debug, Default)]
pub struct DecisionBook {
    pending: Option<Pending>,
}

impl DecisionBook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn pending(&self) -> Option<&Pending> {
        self.pending.as_ref()
    }

    fn new_id(storage: &Storage) -> Result<DecisionId, StoreError> {
        let seq = ReactionStore::next_seq(storage.read())?;
        Ok(DecisionId::new(format!("dec-{seq}")).expect("a counter is a valid id"))
    }

    /// A decision for a step the plan fixed. It has no backend and no request,
    /// but it still goes through the same admission as every other.
    pub fn open_direct(
        &mut self,
        storage: &mut Storage,
        root: &RootTaskId,
        now_ms: u64,
    ) -> Result<DecisionId, StoreError> {
        let decision = Self::new_id(storage)?;
        storage.transaction(|tx| ReactionStore::open_decision(tx, &decision, root, 1, now_ms))?;
        Ok(decision)
    }

    /// Open a request for a backend. At most one is open; a previous one is
    /// replaced and its answer can no longer be admitted.
    pub fn open(
        &mut self,
        storage: &mut Storage,
        spec: OpenSpec<'_>,
    ) -> Result<DecisionRequest, StoreError> {
        if self.pending.is_some() {
            self.close(storage, DecisionLifecycle::Superseded)?;
        }
        let decision = Self::new_id(storage)?;
        storage.transaction(|tx| {
            ReactionStore::open_decision(tx, &decision, spec.root, 1, spec.now_ms)
        })?;
        let options = spec
            .state
            .candidates
            .iter()
            .filter(|candidate| candidate.precondition == Precondition::Executable)
            .map(|candidate| QuestionOption {
                key: candidate.candidate_id.as_str().to_string(),
                description: candidate.description.clone(),
            })
            .collect();
        let candidates = spec.state.candidates.clone();
        let request = DecisionRequest {
            decision_id: decision,
            lane_id: LANE.into(),
            request_generation: 1,
            root_task_id: spec.root.clone(),
            task_revision: spec.task_revision,
            plan_revision: spec.plan_revision,
            root_epoch: spec.root_epoch,
            decision_program: spec.program.into(),
            calibration_revision: "none".into(),
            state: spec.state,
            questions: vec![Question {
                id: "q1".into(),
                kind: QuestionKind::Choice,
                prompt: "Which action should run next?".into(),
                options,
            }],
            created_ms: spec.now_ms,
            result_deadline_ms: spec.now_ms.saturating_add(spec.timeout_ms),
        };
        self.pending = Some(Pending {
            request: request.clone(),
            candidates,
            basis: spec.basis,
            subgoal: spec.subgoal,
            held: None,
        });
        Ok(request)
    }

    /// Close the open decision without using it.
    pub fn close(
        &mut self,
        storage: &mut Storage,
        to: DecisionLifecycle,
    ) -> Result<Option<DecisionCancel>, StoreError> {
        let Some(pending) = self.pending.take() else {
            return Ok(None);
        };
        storage.transaction(|tx| ReactionStore::close(tx, pending.decision_id(), to))?;
        Ok(Some(DecisionCancel {
            decision_id: pending.request.decision_id,
            request_generation: pending.request.request_generation,
        }))
    }

    /// The decision was consumed by an admitted attempt; only the in-memory
    /// record goes, the ledger already says consumed.
    pub fn consumed(&mut self) {
        self.pending = None;
    }

    /// The request whose time ran out, closed as expired. A decision waiting
    /// for the user's confirmation has already been answered and does not
    /// expire with the backend's clock.
    pub fn expire(
        &mut self,
        storage: &mut Storage,
        now_ms: u64,
    ) -> Result<Option<DecisionCancel>, StoreError> {
        let expired = self.pending.as_ref().is_some_and(|pending| {
            pending.held.is_none() && now_ms > pending.request.result_deadline_ms
        });
        if expired {
            self.close(storage, DecisionLifecycle::Expired)
        } else {
            Ok(None)
        }
    }

    /// Ask again after a backend failure: the same logical decision, a higher
    /// generation, so the failed request's answer can never be admitted.
    pub fn retry(
        &mut self,
        storage: &mut Storage,
        now_ms: u64,
        timeout_ms: u64,
    ) -> Result<Option<DecisionRequest>, StoreError> {
        let Some(pending) = self.pending.as_mut() else {
            return Ok(None);
        };
        let generation = pending.generation() + 1;
        let decision = pending.decision_id().clone();
        let replaced =
            storage.transaction(|tx| ReactionStore::supersede(tx, &decision, generation))?;
        if !replaced {
            self.pending = None;
            return Ok(None);
        }
        pending.request.request_generation = generation;
        pending.request.created_ms = now_ms;
        pending.request.result_deadline_ms = now_ms.saturating_add(timeout_ms);
        Ok(Some(pending.request.clone()))
    }

    pub fn hold(&mut self, held: Held) {
        if let Some(pending) = self.pending.as_mut() {
            pending.held = Some(held);
        }
    }

    pub fn held(&self) -> Option<&Held> {
        self.pending
            .as_ref()
            .and_then(|pending| pending.held.as_ref())
    }

    pub fn release_hold(&mut self) -> Option<Held> {
        self.pending
            .as_mut()
            .and_then(|pending| pending.held.take())
    }

    fn typed_choice(proposal: &DecisionProposal) -> Option<PolicyChoice> {
        if let Some(choice) = &proposal.choice {
            return Some(choice.clone());
        }
        // A choice question answered without a typed choice names a candidate.
        proposal
            .answers
            .iter()
            .find_map(|answer| answer.selected.clone())
            .and_then(|key| {
                interaction_contracts::desktop::ids::CandidateId::new(key)
                    .ok()
                    .map(|candidate| PolicyChoice::StartSkill { candidate })
            })
    }

    /// Check a proposal against the open request. The request ends with the
    /// answer, except when the answer is for another generation or the backend
    /// failed, where the decision stays open for a retry.
    pub fn evaluate(
        &mut self,
        storage: &mut Storage,
        proposal: &DecisionProposal,
        now_ms: u64,
        basis_now: &DecisionBasis,
    ) -> Result<Verdict, StoreError> {
        let Some(pending) = self.pending.as_ref() else {
            return Ok(Verdict::Dropped(Dropped::Unknown));
        };
        if pending.decision_id() != &proposal.decision_id {
            return Ok(Verdict::Dropped(Dropped::Unknown));
        }
        if proposal.request_generation != pending.generation() {
            return Ok(Verdict::Dropped(Dropped::OldGeneration));
        }
        let decision = pending.decision_id().clone();
        let generation = pending.generation();

        if let Some(failure) = proposal.failure {
            return Ok(Verdict::Dropped(Dropped::BackendFailed(failure)));
        }
        if now_ms > pending.request.result_deadline_ms {
            self.close(storage, DecisionLifecycle::Expired)?;
            return Ok(Verdict::Dropped(Dropped::Expired));
        }
        if let Some(reason) = pending.basis.broken_by(basis_now) {
            self.close(storage, DecisionLifecycle::Superseded)?;
            return Ok(Verdict::Dropped(Dropped::Stale(reason)));
        }
        if proposal.candidate_set_digest != pending.request.state.candidate_set_digest {
            self.close(storage, DecisionLifecycle::Superseded)?;
            return Ok(Verdict::Dropped(Dropped::DigestMismatch));
        }
        let invalid = |this: &mut Self, storage: &mut Storage, reason: String| {
            this.close(storage, DecisionLifecycle::Superseded)?;
            Ok(Verdict::Dropped(Dropped::Invalid(reason)))
        };
        // Only a model can make a semantic choice for a backend; a plan step,
        // a rule or a person is not something a backend may claim to be.
        if !matches!(
            proposal.source,
            DecisionSource::S1 | DecisionSource::Teacher | DecisionSource::Llm
        ) {
            return invalid(
                self,
                storage,
                format!("a backend cannot claim {:?} as its source", proposal.source),
            );
        }
        if proposal.truncated {
            return invalid(self, storage, "the backend saw a truncated state".into());
        }
        let Some(choice) = Self::typed_choice(proposal) else {
            return invalid(self, storage, "the proposal chose nothing".into());
        };
        let control = |control: Control, reason: String| Verdict::Control {
            decision: decision.clone(),
            control,
            reason,
            source: proposal.source,
        };
        match choice {
            PolicyChoice::ObserveMore { facts } => {
                Ok(control(Control::ObserveMore, facts.join("; ")))
            }
            PolicyChoice::RequestPlanning { reason } => {
                Ok(control(Control::RequestPlanning, reason))
            }
            PolicyChoice::StopOrYield { reason } => Ok(control(Control::StopOrYield, reason)),
            PolicyChoice::ContinueSkill { .. } => invalid(
                self,
                storage,
                "continuing a running skill is not offered".into(),
            ),
            PolicyChoice::StartSkill { candidate } => {
                let chosen = pending
                    .candidates
                    .iter()
                    .find(|known| known.candidate_id == candidate)
                    .cloned();
                let Some(chosen) = chosen else {
                    return invalid(
                        self,
                        storage,
                        format!("candidate {candidate} was not offered"),
                    );
                };
                if chosen.precondition != Precondition::Executable {
                    return invalid(
                        self,
                        storage,
                        format!("candidate {candidate} is not executable"),
                    );
                }
                Ok(match chosen.skill.as_str() {
                    OBSERVE_MORE => control(Control::ObserveMore, chosen.description),
                    REQUEST_PLANNING => control(Control::RequestPlanning, chosen.description),
                    STOP_OR_YIELD => control(Control::StopOrYield, chosen.description),
                    _ => Verdict::Start {
                        decision,
                        generation,
                        candidate: Box::new(chosen),
                        source: proposal.source,
                        backend: proposal.backend.clone(),
                    },
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reaction::candidates::{Basis, Resolution, Scope, build};
    use crate::reaction::compiler::{CompileInput, CompilerLimits, compile};
    use crate::testkit::*;
    use interaction_contracts::desktop::decision::Answer;
    use interaction_contracts::desktop::effect::EffectClass;
    use interaction_contracts::desktop::task::ConfirmationScope;
    use sim_world::policy::ScriptedTeacher;
    use std::collections::BTreeSet;

    const TEXT: &str = r#"Turn off "Daily picks" in "Push""#;

    struct Fixture {
        storage: Storage,
        book: DecisionBook,
        rig: EnvRig,
        root: RootTaskId,
        request: DecisionRequest,
    }

    fn fixture() -> Fixture {
        let mut storage = Storage::open_in_memory().unwrap();
        let mut rig = EnvRig::new();
        let env = rig.observe(10);
        let binding = rig.bind_goal_target();
        let goal = goal(ConfirmationScope::UiRevisit, EffectClass::ReversibleEdit);
        let bundle = bundle(TEXT);
        let plan_targets = BTreeSet::from([TARGET.to_string()]);
        let resolution = Resolution::Bound(binding);
        let candidates = build(
            &Scope {
                goal: &goal,
                plan_targets: &plan_targets,
                pack: &rig.pack,
                env: &env,
            },
            &Basis::Step {
                subgoal: &bundle.subgoals[0],
                resolution: &resolution,
            },
        );
        let state = compile(
            CompileInput {
                goal: &goal,
                subgoal: "ensure",
                env: &env,
                now_ms: 10,
                candidates,
                recent_results: &[],
                missing_fields: vec![],
            },
            &CompilerLimits::default(),
        )
        .unwrap();
        let root = goal.root_task_id.clone();
        let basis = DecisionBasis::capture(&env, &rig.bindings, 1, 1, 1);
        let mut book = DecisionBook::new();
        let request = book
            .open(
                &mut storage,
                OpenSpec {
                    root: &root,
                    task_revision: 1,
                    plan_revision: 1,
                    root_epoch: 1,
                    program: "ui.ensure_value@1",
                    state,
                    basis,
                    subgoal: Some("ensure".into()),
                    now_ms: 10,
                    timeout_ms: 500,
                },
            )
            .unwrap();
        Fixture {
            storage,
            book,
            rig,
            root,
            request,
        }
    }

    fn basis_now(fixture: &Fixture) -> DecisionBasis {
        let env = fixture.rig.state.latest().unwrap();
        DecisionBasis::capture(env, &fixture.rig.bindings, 1, 1, 1)
    }

    fn evaluate(fixture: &mut Fixture, proposal: &DecisionProposal, now: u64) -> Verdict {
        let basis = basis_now(fixture);
        fixture
            .book
            .evaluate(&mut fixture.storage, proposal, now, &basis)
            .unwrap()
    }

    fn state_of(fixture: &Fixture) -> DecisionLifecycle {
        ReactionStore::load(fixture.storage.read(), fixture.request.decision_id.as_str())
            .unwrap()
            .unwrap()
            .state
    }

    fn proposal(fixture: &Fixture) -> DecisionProposal {
        ScriptedTeacher::default().decide(&fixture.request)
    }

    #[test]
    fn opening_records_a_pending_decision_and_asks_one_choice_question() {
        let fixture = fixture();
        assert_eq!(state_of(&fixture), DecisionLifecycle::Pending);
        let request = &fixture.request;
        assert!(request.validate().is_ok());
        assert_eq!(request.decision_id.as_str(), "dec-1");
        assert_eq!(request.questions.len(), 1);
        assert_eq!(
            request.questions[0].options.len(),
            4,
            "all four are executable"
        );
        assert_eq!(request.result_deadline_ms, 510);
        assert_eq!(fixture.book.pending().unwrap().generation(), 1);
    }

    #[test]
    fn the_teachers_choice_of_the_step_becomes_a_start() {
        let mut fixture = fixture();
        let proposal = proposal(&fixture);
        match evaluate(&mut fixture, &proposal, 50) {
            Verdict::Start {
                candidate,
                source,
                generation,
                ..
            } => {
                assert_eq!(candidate.skill, "ui.ensure_value");
                assert_eq!(source, DecisionSource::Teacher);
                assert_eq!(generation, 1);
            }
            other => panic!("expected a start: {other:?}"),
        }
    }

    #[test]
    fn an_answer_for_another_generation_or_decision_changes_nothing() {
        let mut fixture = fixture();
        let mut old = proposal(&fixture);
        old.request_generation = 7;
        assert_eq!(
            evaluate(&mut fixture, &old, 50),
            Verdict::Dropped(Dropped::OldGeneration)
        );
        let mut other = proposal(&fixture);
        other.decision_id = DecisionId::new("dec-99").unwrap();
        assert_eq!(
            evaluate(&mut fixture, &other, 50),
            Verdict::Dropped(Dropped::Unknown)
        );
        assert!(
            fixture.book.pending().is_some(),
            "the request is still open"
        );
        assert_eq!(state_of(&fixture), DecisionLifecycle::Pending);
    }

    #[test]
    fn an_answer_after_the_deadline_expires_the_decision() {
        let mut fixture = fixture();
        let proposal = proposal(&fixture);
        assert_eq!(
            evaluate(&mut fixture, &proposal, 511),
            Verdict::Dropped(Dropped::Expired)
        );
        assert_eq!(state_of(&fixture), DecisionLifecycle::Expired);
        assert!(fixture.book.pending().is_none());
        // And a second copy of it finds nothing open.
        assert_eq!(
            evaluate(&mut fixture, &proposal, 512),
            Verdict::Dropped(Dropped::Unknown)
        );
    }

    #[test]
    fn a_proposal_for_another_candidate_set_is_refused() {
        let mut fixture = fixture();
        let mut proposal = proposal(&fixture);
        proposal.candidate_set_digest = "cs1:other".into();
        assert_eq!(
            evaluate(&mut fixture, &proposal, 50),
            Verdict::Dropped(Dropped::DigestMismatch)
        );
        assert_eq!(state_of(&fixture), DecisionLifecycle::Superseded);
    }

    #[test]
    fn unknown_inexecutable_and_unsupported_choices_are_invalid() {
        for choice in [
            PolicyChoice::StartSkill {
                candidate: interaction_contracts::desktop::ids::CandidateId::new("c99").unwrap(),
            },
            PolicyChoice::ContinueSkill {
                attempt: AttemptId::new("att-1").unwrap(),
            },
        ] {
            let mut fixture = fixture();
            let mut proposal = proposal(&fixture);
            proposal.choice = Some(choice);
            let verdict = evaluate(&mut fixture, &proposal, 50);
            assert!(
                matches!(verdict, Verdict::Dropped(Dropped::Invalid(_))),
                "{verdict:?}"
            );
            assert_eq!(state_of(&fixture), DecisionLifecycle::Superseded);
        }
    }

    #[test]
    fn a_backend_cannot_claim_to_be_the_plan_a_rule_or_a_person() {
        for source in [
            DecisionSource::Plan,
            DecisionSource::Rule,
            DecisionSource::Human,
        ] {
            let mut fixture = fixture();
            let mut proposal = proposal(&fixture);
            proposal.source = source;
            let verdict = evaluate(&mut fixture, &proposal, 50);
            assert!(
                matches!(&verdict, Verdict::Dropped(Dropped::Invalid(reason)) if reason.contains("claim")),
                "{verdict:?}"
            );
        }
    }

    #[test]
    fn a_truncated_view_makes_the_answer_unusable() {
        let mut fixture = fixture();
        let mut proposal = proposal(&fixture);
        proposal.truncated = true;
        assert!(matches!(
            evaluate(&mut fixture, &proposal, 50),
            Verdict::Dropped(Dropped::Invalid(_))
        ));
    }

    #[test]
    fn the_control_paths_map_to_their_own_verdicts() {
        let mut fixture = fixture();
        let mut proposal = proposal(&fixture);
        proposal.choice = Some(PolicyChoice::RequestPlanning {
            reason: "the page is unfamiliar".into(),
        });
        match evaluate(&mut fixture, &proposal, 50) {
            Verdict::Control {
                control, reason, ..
            } => {
                assert_eq!(control, Control::RequestPlanning);
                assert_eq!(reason, "the page is unfamiliar");
            }
            other => panic!("{other:?}"),
        }

        // Choosing the observe candidate by id is the same thing.
        let mut fixture = self::fixture();
        let observe = fixture
            .request
            .state
            .candidates
            .iter()
            .find(|c| c.skill == OBSERVE_MORE)
            .unwrap()
            .candidate_id
            .clone();
        let mut proposal = self::proposal(&fixture);
        proposal.choice = Some(PolicyChoice::StartSkill { candidate: observe });
        assert!(matches!(
            evaluate(&mut fixture, &proposal, 50),
            Verdict::Control {
                control: Control::ObserveMore,
                ..
            }
        ));
    }

    #[test]
    fn a_selected_answer_without_a_typed_choice_names_a_candidate() {
        let mut fixture = fixture();
        let mut proposal = proposal(&fixture);
        proposal.choice = None;
        proposal.answers = vec![Answer {
            question_id: "q1".into(),
            selected: Some("c1".into()),
            confidence_milli: None,
        }];
        assert!(matches!(
            evaluate(&mut fixture, &proposal, 50),
            Verdict::Start { .. }
        ));
    }

    #[test]
    fn a_failed_backend_leaves_the_decision_open_for_a_retry_at_a_higher_generation() {
        let mut fixture = fixture();
        let mut failed = proposal(&fixture);
        failed.failure = Some(DecisionFailure::Unavailable);
        failed.choice = None;
        assert_eq!(
            evaluate(&mut fixture, &failed, 50),
            Verdict::Dropped(Dropped::BackendFailed(DecisionFailure::Unavailable))
        );
        assert_eq!(state_of(&fixture), DecisionLifecycle::Pending);

        let retry = fixture
            .book
            .retry(&mut fixture.storage, 100, 500)
            .unwrap()
            .expect("the decision is still pending");
        assert_eq!(retry.decision_id, fixture.request.decision_id);
        assert_eq!(retry.request_generation, 2);
        assert_eq!(retry.result_deadline_ms, 600);

        // The first request's answer can no longer be admitted.
        let late = proposal(&fixture);
        assert_eq!(
            evaluate(&mut fixture, &late, 110),
            Verdict::Dropped(Dropped::OldGeneration)
        );
        fixture.request = retry;
        let fresh = proposal(&fixture);
        assert!(matches!(
            evaluate(&mut fixture, &fresh, 110),
            Verdict::Start { .. }
        ));
    }

    #[test]
    fn a_change_the_answer_depended_on_makes_it_stale_but_a_newer_snapshot_does_not() {
        let mut fixture = fixture();
        // A newer snapshot, same page and layer, same binding: still valid.
        fixture.rig.observe(10);
        let proposal_a = proposal(&fixture);
        assert!(matches!(
            evaluate(&mut fixture, &proposal_a, 30),
            Verdict::Start { .. }
        ));

        // A dialog appears: the layer changes and the binding is dropped.
        let mut fixture = self::fixture();
        fixture
            .rig
            .app
            .inject_modal(sim_world::app::Modal::UnsavedChanges);
        fixture.rig.observe(10);
        let proposal_b = self::proposal(&fixture);
        let verdict = evaluate(&mut fixture, &proposal_b, 30);
        assert!(
            matches!(&verdict, Verdict::Dropped(Dropped::Stale(reason)) if reason.contains("layer")),
            "{verdict:?}"
        );
        assert_eq!(state_of(&fixture), DecisionLifecycle::Superseded);
    }

    #[test]
    fn a_decision_waiting_for_the_user_does_not_expire_but_an_unanswered_one_does() {
        let mut fixture = fixture();
        assert!(
            fixture
                .book
                .expire(&mut fixture.storage, 400)
                .unwrap()
                .is_none()
        );
        let candidate = fixture.request.state.candidates[0].clone();
        fixture.book.hold(Held {
            candidate,
            source: DecisionSource::Teacher,
            backend: "t".into(),
        });
        assert!(
            fixture
                .book
                .expire(&mut fixture.storage, 10_000)
                .unwrap()
                .is_none(),
            "the backend already answered; the user is the one being waited for"
        );
        assert!(fixture.book.release_hold().is_some());
        let cancel = fixture
            .book
            .expire(&mut fixture.storage, 10_000)
            .unwrap()
            .expect("now it expires");
        assert_eq!(cancel.decision_id, fixture.request.decision_id);
        assert_eq!(state_of(&fixture), DecisionLifecycle::Expired);
    }

    #[test]
    fn a_new_request_replaces_the_open_one_and_ids_are_never_reused() {
        let mut fixture = fixture();
        let first = fixture.request.decision_id.clone();
        let again = fixture
            .book
            .open_direct(&mut fixture.storage, &fixture.root, 20)
            .unwrap();
        assert_ne!(first, again);
        assert_eq!(again.as_str(), "dec-2");
        // Opening a request while one is open supersedes the old one.
        let mut other = self::fixture();
        let previous = other.request.clone();
        let spec_state = previous.state.clone();
        let basis = basis_now(&other);
        let root = other.root.clone();
        let replacement = other
            .book
            .open(
                &mut other.storage,
                OpenSpec {
                    root: &root,
                    task_revision: 1,
                    plan_revision: 1,
                    root_epoch: 1,
                    program: "ui.ensure_value@1",
                    state: spec_state,
                    basis,
                    subgoal: None,
                    now_ms: 20,
                    timeout_ms: 500,
                },
            )
            .unwrap();
        assert_eq!(replacement.decision_id.as_str(), "dec-2");
        assert_eq!(
            ReactionStore::load(other.storage.read(), "dec-1")
                .unwrap()
                .unwrap()
                .state,
            DecisionLifecycle::Superseded
        );
    }
}
