//! Registered, budgeted startup navigation to the fixed authorized window.
use super::{Engine, Step};
use crate::{
    execution::{DeviceCommand, Expect},
    infrastructure::storage::StoreError,
    reaction::{candidates::scoped_radio_choice_values, skills::Skill, store::ReactionStore},
};
use interaction_contracts::desktop::{
    binding::instance_fingerprint,
    decision::{Candidate, DecisionSource, Precondition},
    effect::EffectClass,
    ids::{CandidateId, DecisionId},
    task::RowTableScope,
};

impl Engine {
    pub(super) fn activation_pending(&self) -> bool {
        self.cfg.native_window_scope.is_some()
            && self.run.as_ref().is_some_and(|r| {
                r.goal.execution_domain
                    != interaction_contracts::desktop::domain::ExecutionDomain::BackgroundAx
                    && !r.window_activated
                    && !r.effect_verification_only
            })
    }
    pub(super) fn window_attempt(&self) -> bool {
        self.run
            .as_ref()
            .and_then(|r| r.attempt.as_ref())
            .is_some_and(|a| a.skill.skill() == Skill::ActivateWindow)
    }
    pub(super) fn window_evidence_usable(&self) -> bool {
        self.window_activation_root().is_some()
    }
    fn frozen_window_row_scope(&self) -> Option<RowTableScope> {
        self.run
            .as_ref()?
            .goal
            .target
            .row
            .as_ref()
            .map(|row| row.table_scope.clone())
    }
    fn partial_row_window_activation_scope(&self) -> Option<RowTableScope> {
        let scope = self.frozen_window_row_scope()?;
        let snapshot = &self.state.latest()?.snapshot;
        (!snapshot.complete).then_some(scope)
    }
    /// Resolve only the root registered for this run. A frozen row goal may
    /// use its exact local census as metadata for WindowActivation; the generic
    /// input path still requires the globally complete window root.
    fn window_activation_root(&self) -> Option<&interaction_contracts::desktop::source::UiObject> {
        let scope = self.cfg.native_window_scope.as_ref()?;
        let max_age_ms = self.cfg.evidence_max_age_ms.min(2000);
        match self.partial_row_window_activation_scope() {
            Some(row_scope) => self.state.row_window_root(
                &row_scope,
                &scope.app,
                scope.pid,
                &scope.window,
                self.now_ms,
                max_age_ms,
            ),
            None => self.state.window_root(
                &scope.app,
                scope.pid,
                &scope.window,
                self.now_ms,
                max_age_ms,
            ),
        }
    }
    pub(super) fn attempt_evidence_usable(&self) -> bool {
        if self.run.as_ref().is_some_and(|run| {
            run.goal.execution_domain
                == interaction_contracts::desktop::domain::ExecutionDomain::BackgroundAx
        }) {
            let expected_hash = self
                .run
                .as_ref()
                .and_then(|run| run.attempt.as_ref())
                .and_then(|attempt| attempt.dispatch.as_ref())
                .and_then(|dispatch| dispatch.observation.native())
                .and_then(|observation| observation.binding_proof.as_ref())
                .and_then(|proof| match &proof.continuity.scope {
                    interaction_contracts::desktop::binding::BindingProofScope::BackgroundAx {
                        target_context_hash,
                    } => Some(target_context_hash.as_str()),
                    _ => None,
                });
            return self.goal_evidence_usable()
                && self.background_ax_evidence_usable(expected_hash);
        }
        if self.run.as_ref().is_some_and(|run| {
            run.goal.execution_domain
                == interaction_contracts::desktop::domain::ExecutionDomain::HeadlessBrowser
        }) {
            return self.browser_goal_evidence_usable() && self.browser_dispatch_matches_current();
        }
        if self.owned_document_attempt_active() {
            return self.owned_document_attempt_evidence_usable();
        }
        if self
            .run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .and_then(|attempt| attempt.dispatch.as_ref())
            .is_some_and(|dispatch| {
                dispatch.observation.native().is_some_and(|observation| {
                    observation.purpose
                        == interaction_contracts::desktop::device::ObservationPurpose::FocusedControl
                })
            })
        {
            return self.evidence_usable() && self.keyboard_dispatch_matches_current();
        }
        let radio_choice_attempt = self.run.as_ref().is_some_and(|run| {
            run.attempt.as_ref().is_some_and(|attempt| {
                attempt.skill.skill() == Skill::SelectByLabel
                    && scoped_radio_choice_values(&run.goal, &run.pack).is_some()
            })
        });
        if radio_choice_attempt {
            let Some(dispatch) = self
                .run
                .as_ref()
                .and_then(|run| run.attempt.as_ref())
                .and_then(|attempt| attempt.dispatch.as_ref())
            else {
                return self.evidence_usable();
            };
            let Some(observation) = dispatch.observation.native() else {
                return false;
            };
            return observation.purpose
                == interaction_contracts::desktop::device::ObservationPurpose::TargetInput
                && self.evidence_usable()
                && self.scoped_revisit_dispatch_matches_current(dispatch);
        }
        if self.menu_attempt() {
            let purpose = self
                .run
                .as_ref()
                .and_then(|run| run.attempt.as_ref())
                .and_then(|attempt| attempt.dispatch.as_ref())
                .filter(|dispatch| {
                    matches!(
                        dispatch.stage,
                        crate::execution::Stage::Observing | crate::execution::Stage::Granting
                    )
                })
                .and_then(|dispatch| dispatch.observation.native())
                .map(|observation| observation.purpose);
            let usable = match purpose {
                Some(interaction_contracts::desktop::device::ObservationPurpose::MenuSelection) => {
                    self.state
                        .menu_usable(self.now_ms, self.cfg.evidence_max_age_ms)
                }
                Some(interaction_contracts::desktop::device::ObservationPurpose::TargetInput) => {
                    self.evidence_usable()
                }
                Some(
                    interaction_contracts::desktop::device::ObservationPurpose::WindowActivation,
                )
                | Some(
                    interaction_contracts::desktop::device::ObservationPurpose::FocusedControl,
                )
                | Some(
                    interaction_contracts::desktop::device::ObservationPurpose::OwnedDocumentValueSetter,
                )
                | Some(
                    interaction_contracts::desktop::device::ObservationPurpose::OwnedDocumentSave,
                )
                | Some(
                    interaction_contracts::desktop::device::ObservationPurpose::RowSelection,
                )
                | Some(
                    interaction_contracts::desktop::device::ObservationPurpose::BackgroundAxAction,
                )
                | Some(
                    interaction_contracts::desktop::device::ObservationPurpose::HeadlessBrowserAction,
                )
                | Some(
                    interaction_contracts::desktop::device::ObservationPurpose::AppIntegrationAction,
                ) => false,
                None => self.current_execution_evidence(),
            };
            usable && (purpose.is_none() || self.menu_dispatch_matches_current())
        } else if self.window_attempt()
            || (self.activation_pending() && self.run.as_ref().is_some_and(|r| r.attempt.is_none()))
        {
            self.window_evidence_usable()
        } else {
            self.evidence_usable()
        }
    }

    fn browser_dispatch_matches_current(&self) -> bool {
        let Some(dispatch) = self
            .run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .and_then(|attempt| attempt.dispatch.as_ref())
        else {
            return true;
        };
        let crate::execution::DispatchPlan::Browser(plan) = &dispatch.plan else {
            return false;
        };
        let Some(observation) = dispatch.observation.browser() else {
            return false;
        };
        let Some(scope) = self.state.browser_scope() else {
            return false;
        };
        let Some(latest) = self.state.browser_latest() else {
            return false;
        };
        let Some(reference) = self.browser_read_binding_ref.as_ref() else {
            return false;
        };
        self.state
            .current_browser_binding(reference)
            .is_some_and(|current| {
                latest.scope_id == scope.scope_id
                    && latest.generation == scope.generation
                    && observation.scope.as_ref() == scope
                    && observation.environment_revision == latest.revision
                    && observation.observation_id == latest.snapshot.snapshot_id
                    && observation.binding_ref == current.binding_ref
                    && observation.binding.as_ref() == &current.proof
                    && plan.binding == current.proof
            })
    }

    pub(super) fn sync_window_evidence(&mut self) -> Result<(), super::EngineError> {
        if self.cfg.native_window_scope.is_none()
            || self.run.as_ref().is_some_and(|run| {
                run.goal.execution_domain
                    == interaction_contracts::desktop::domain::ExecutionDomain::BackgroundAx
            })
        {
            return Ok(());
        }
        let signature = self.window_activation_root().and_then(|root| {
            self.state
                .latest()
                .map(|e| instance_fingerprint(&e.snapshot, root).digest())
        });
        if signature != self.window_context {
            let valid = signature.is_some();
            self.window_context = signature;
            self.send(
                Expect::Dependency,
                DeviceCommand::UpdateDependency(
                    interaction_contracts::desktop::device::DependencyUpdate {
                        name: "window".into(),
                        valid: Some(valid),
                    },
                ),
            );
            if self.window_attempt() {
                let unresolved = self
                    .run
                    .as_ref()
                    .and_then(|r| r.attempt.as_ref())
                    .is_some_and(|a| {
                        a.dispatch.as_ref().is_some_and(|d| {
                            matches!(
                                d.stage,
                                crate::execution::Stage::Submitting
                                    | crate::execution::Stage::Running
                            )
                        })
                    });
                if unresolved {
                    self.pause_root("window metadata changed during an unresolved activation")?;
                } else if self
                    .run
                    .as_ref()
                    .and_then(|r| r.attempt.as_ref())
                    .is_some_and(|a| !a.input_reached)
                {
                    self.end_attempt_cancelled()?;
                }
            }
        }
        Ok(())
    }
    pub(super) fn start_window_attempt(&mut self) -> Step {
        if !self.window_evidence_usable() || self.native_context_settling() {
            return Ok(false);
        }
        if self.device.is_waiting(|e| matches!(e, Expect::Dependency)) {
            return Ok(false);
        }
        let env = self.state.latest().cloned().unwrap();
        let Some(instance) = self
            .window_activation_root()
            .map(|root| root.instance_ref.clone())
        else {
            return Ok(false);
        };
        let binding = match self.partial_row_window_activation_scope() {
            Some(row_scope) => self.bindings.bind_window_root(&env, &instance, &row_scope),
            None => self.bindings.bind_window(&env, &instance),
        };
        let Some(binding) = binding else {
            return Ok(false);
        };
        let root = self.run.as_ref().unwrap().goal.root_task_id.clone();
        let decision = DecisionId::new(format!(
            "window-decision-{}",
            ReactionStore::next_seq(self.storage.read())?
        ))
        .unwrap();
        self.storage.transaction::<_, StoreError>(|tx| {
            ReactionStore::open_decision(tx, &decision, &root, 1, self.now_ms)
        })?;
        self.admit_candidate(
            decision,
            1,
            Candidate {
                candidate_id: CandidateId::new("activate-authorized-window").unwrap(),
                skill: Skill::ActivateWindow.name().into(),
                semantic_target: Some(binding.semantic_target.clone()),
                binding: Some(binding.binding_ref),
                parameters: Default::default(),
                effect_class: EffectClass::Navigation,
                precondition: Precondition::Executable,
                description: "activate the authorized task window".into(),
            },
            DecisionSource::Plan,
            false,
        )
    }
}
