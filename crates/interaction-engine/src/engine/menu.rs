//! The host's menu transaction identity and its separately scoped evidence.
use interaction_contracts::desktop::{
    binding::{MenuBindingScope, MenuOpeningRef, instance_fingerprint, normalize_label},
    device::{Receipt, ReceiptStatus},
    effect::{Admission, EffectClass},
    ids::SemanticTargetRef,
    planning::PlanBundle,
    primitive::{AxActionKind, Primitive},
    task::ExpectedValue,
};

use super::Engine;
use crate::execution::DispatchPlan;
use crate::reaction::{
    candidates::{Scope, gate},
    skills::{ActionPlan, Skill, SkillRun},
};

pub(super) fn valid_menu_plan(
    bundle: &PlanBundle,
    target: &SemanticTargetRef,
    desired: &ExpectedValue,
) -> bool {
    bundle.subgoals.iter().all(|step| {
        step.skill != "ui.select_by_label" || step.select.as_ref().is_some_and(|select| {
            step.target.is_some()
                && step.scroll.is_none()
                && select.validate().is_ok()
                && (step.target.as_ref() != Some(target)
                    || matches!(desired, ExpectedValue::Text { value } if value == &select.choice_label))
        })
    })
}

impl Engine {
    pub(super) fn menu_attempt(&self) -> bool {
        self.run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .is_some_and(|attempt| attempt.skill.skill() == Skill::SelectByLabel)
    }

    pub(super) fn menu_same_watch_required(&self) -> bool {
        if self.menu_readonly_result_ready() {
            return false;
        }
        self.run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .is_some_and(|attempt| {
                // A mechanical unknown can never inherit the known-result
                // observation permission from a later semantic state.
                attempt.skill.menu_same_watch_required()
                    || attempt.skill.menu_readonly_result_ready()
            })
    }

    pub(super) fn menu_readonly_result_ready(&self) -> bool {
        self.run
            .as_ref()
            .and_then(|run| run.attempt.as_ref())
            .is_some_and(|attempt| {
                attempt.dispatch.is_none()
                    && !attempt.mechanical_unknown
                    && attempt.menu_opening.is_some()
                    && attempt.skill.menu_readonly_result_ready()
            })
    }

    /// Re-evaluate the immutable result scope against current Source and the
    /// current normal binding. Completion cannot reuse last tick's verdict.
    pub(super) fn menu_readonly_result_scope_valid(
        &self,
        skill: &SkillRun,
        mechanical_unknown: bool,
    ) -> bool {
        if mechanical_unknown || !self.evidence_usable() {
            return false;
        }
        let (Some(env), Some(target)) = (self.state.latest(), skill.menu_target()) else {
            return false;
        };
        let Some(binding) = self.bindings.current(target) else {
            return false;
        };
        skill.menu_readonly_result_scope_matches(env, binding)
    }

    /// A healthy menu read keeps the known opening's observation phase alive.
    /// Input itself still goes through the purpose-specific canonical builder.
    pub(super) fn current_execution_evidence(&self) -> bool {
        if self.run.as_ref().is_some_and(|run| {
            run.goal.execution_domain
                == interaction_contracts::desktop::domain::ExecutionDomain::BackgroundAx
        }) {
            return self.background_ax_evidence_usable(None);
        }
        if self.menu_readonly_result_ready() {
            return self.evidence_usable();
        }
        self.evidence_usable()
            || self.row_root_evidence_usable()
            || self.menu_attempt()
                && self
                    .state
                    .menu_usable(self.now_ms, self.cfg.evidence_max_age_ms)
    }

    pub(super) fn menu_dispatch_matches_current(&self) -> bool {
        let Some(attempt) = self.run.as_ref().and_then(|run| run.attempt.as_ref()) else {
            return false;
        };
        let Some(dispatch) = attempt.dispatch.as_ref() else {
            return true;
        };
        let Some(proof) = dispatch
            .observation
            .native()
            .and_then(|observation| observation.binding_proof.as_ref())
        else {
            return false;
        };
        let Some(scope) = proof.menu_scope.as_ref() else {
            return false;
        };
        let Some(env) = self.state.latest() else {
            return false;
        };
        let Some(object) = env
            .snapshot
            .objects
            .iter()
            .find(|o| o.instance_ref == proof.instance)
        else {
            return false;
        };
        if object.frame != proof.display_frame
            || instance_fingerprint(&env.snapshot, object) != proof.fingerprint
        {
            return false;
        }
        match scope {
            MenuBindingScope::Opener { anchor, .. } => {
                env.snapshot.menu_context.is_none()
                    && env.snapshot.menu_anchors.iter().any(|current| {
                        current.same_identity(anchor) && current.opener_instance == proof.instance
                    })
            }
            MenuBindingScope::Item { context, .. } => {
                env.snapshot.menu_context.as_ref().is_some_and(|current| {
                    current.same_identity(context)
                        && object.parent.as_ref() == Some(&current.menu_instance)
                }) && matches!(
                    &dispatch.plan,
                    DispatchPlan::Generic(plan) if self.menu_choice_authorized(plan)
                )
            }
        }
    }

    pub(super) fn menu_input_scope(
        &self,
        plan: &ActionPlan,
    ) -> Result<Option<MenuBindingScope>, &'static str> {
        let Some(run) = self.run.as_ref() else {
            return Err("no active root");
        };
        let Some(attempt) = run.attempt.as_ref() else {
            return Err("no active attempt");
        };
        if attempt.skill.skill() != Skill::SelectByLabel {
            return Ok(None);
        }
        if self.menu_readonly_result_ready() {
            return Err(
                "the completed menu choice permits only result observation, not another input",
            );
        }
        let env = self.state.latest().ok_or("no current menu evidence")?;
        let deadline = self
            .tasks
            .active()
            .and_then(|active| active.row.deadline_ms)
            .ok_or("menu root has no frozen deadline")?
            .min(attempt.deadline_ms);
        match &plan.primitive {
            Primitive::AxAction {
                action: AxActionKind::ShowMenu,
            } => {
                if attempt.menu_opening.is_some()
                    || attempt.sequence != 0
                    || env.snapshot.menu_context.is_some()
                {
                    return Err("this menu opening cannot be repeated");
                }
                let anchor = attempt
                    .skill
                    .menu_anchor()
                    .ok_or("the skill has no current source opener anchor")?;
                Ok(Some(MenuBindingScope::Opener {
                    anchor: anchor.clone(),
                    attempt_deadline_ms: deadline,
                }))
            }
            Primitive::AxAction {
                action: AxActionKind::Press,
            } => {
                let opening = attempt
                    .menu_opening
                    .as_ref()
                    .ok_or("menu selection lacks a known completed opening")?;
                let floor = attempt
                    .skill
                    .menu_opening_end_ms()
                    .ok_or("menu opening has no actual input end")?;
                let context = attempt
                    .skill
                    .menu_context()
                    .ok_or("the skill has no current menu context")?;
                if opening.attempt_id != attempt.id
                    || opening.attempt_deadline_ms != deadline
                    || env.snapshot.observed_ms <= floor
                    || env.snapshot.menu_context.as_ref() != Some(context)
                    || plan.binding.rule != "menu_item"
                    || attempt.sequence != 1
                    || !self.menu_choice_authorized(plan)
                {
                    return Err(
                        "the current menu choice is not authorized by the original opening and root",
                    );
                }
                Ok(Some(MenuBindingScope::Item {
                    context: context.clone(),
                    opened_by: opening.clone(),
                }))
            }
            _ => Err("menu selection only submits one ShowMenu and one AXPress"),
        }
    }

    pub(super) fn menu_choice_authorized(&self, plan: &ActionPlan) -> bool {
        let Some(run) = self.run.as_ref() else {
            return false;
        };
        let Some(attempt) = run.attempt.as_ref() else {
            return false;
        };
        let Some(env) = self.state.latest() else {
            return false;
        };
        let Some(object) = env
            .snapshot
            .objects
            .iter()
            .find(|o| o.instance_ref == plan.binding.instance)
        else {
            return false;
        };
        let label = object
            .title
            .as_deref()
            .or(object.description.as_deref())
            .unwrap_or("");
        let class = run.pack.effect_class(&object.role, label);
        object.role == "AXMenuItem"
            && object.enabled
            && object.in_view
            && !object.secure
            && matches!(attempt.skill.menu_desired(), Some(ExpectedValue::Text { value }) if normalize_label(value) == normalize_label(label))
            && class <= EffectClass::ReversibleEdit
            && attempt.skill.skill() == Skill::SelectByLabel
            && gate(
                &Scope {
                    goal: &run.goal,
                    plan_targets: &run.plan_targets,
                    pack: &run.pack,
                    env,
                },
                class,
                attempt.skill.menu_target(),
                false,
            ) == Admission::Allow
    }

    /// Only a receipt for the exact submitted opener action establishes the
    /// historical fact needed by a later menu item proof; it grants no permit.
    pub(super) fn record_menu_opening(&mut self, receipt: &Receipt) -> bool {
        let Some(attempt) = self.run.as_mut().and_then(|run| run.attempt.as_mut()) else {
            return false;
        };
        let Some(dispatch) = attempt.dispatch.as_ref() else {
            return false;
        };
        if attempt.skill.skill() != Skill::SelectByLabel
            || !matches!(
                &dispatch.plan,
                DispatchPlan::Generic(plan)
                    if matches!(
                        plan.primitive,
                        Primitive::AxAction {
                            action: AxActionKind::ShowMenu
                        }
                    )
            )
        {
            return true;
        }
        if receipt.status != ReceiptStatus::Completed {
            return true;
        }
        let Some(MenuBindingScope::Opener {
            attempt_deadline_ms,
            ..
        }) = dispatch
            .observation
            .native()
            .and_then(|observation| observation.binding_proof.as_ref())
            .and_then(|proof| proof.menu_scope.as_ref())
        else {
            return false;
        };
        let Some(end_ms) = receipt.input_end_ms else {
            return false;
        };
        if receipt.attempt_id.as_deref() != Some(attempt.id.as_str())
            || dispatch.intent_id.as_deref() != Some(receipt.intent_id.as_str())
            || end_ms > *attempt_deadline_ms
            || receipt.submitted.len() != 1
            || receipt.submitted[0].cleanup()
            || end_ms < receipt.submitted[0].at_ms()
            || receipt.submitted[0].native_primitive()
                != Some(&Primitive::AxAction {
                    action: AxActionKind::ShowMenu,
                })
            || attempt.menu_opening.is_some()
        {
            return false;
        }
        attempt.menu_opening = Some(MenuOpeningRef {
            attempt_id: attempt.id.clone(),
            intent_id: receipt.intent_id.clone(),
            attempt_deadline_ms: *attempt_deadline_ms,
        });
        true
    }
}
