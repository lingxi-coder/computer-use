//! SkillRuntime: short skills that can each be checked (design 15).
//!
//! A skill re-locates its target, checks that it can be operated, asks for one
//! bounded input, waits for evidence newer than that input and judges the
//! progress. It is a state machine: the engine feeds it evidence and the
//! outcome of the input it asked for, and it answers with the next step. It
//! never submits anything itself and never takes a receipt as proof of the
//! effect.

use interaction_contracts::desktop::binding::normalize_label;
use interaction_contracts::desktop::error::FailureCode;
use interaction_contracts::desktop::ids::{BindingRef, SemanticTargetRef};
use interaction_contracts::desktop::primitive::{AxActionKind, Modifiers, MouseButton, Primitive};
use interaction_contracts::desktop::source::{AxActionName, UiObject};
use interaction_contracts::desktop::task::{AttemptQuota, ExpectedValue};

use super::candidates::modal_in_front;
use crate::environment::binding::{Binding, BindingReducer};
use crate::environment::pack::AppPack;
use crate::environment::read::{object_for, value_text, values_equal};
use crate::environment::state_core::{EnvSnapshot, StateCore};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skill {
    ReadValue,
    EnsureValue,
    Activate,
    Revisit,
}

impl Skill {
    /// The skills this build can run. The others are in the contract
    /// vocabulary but need an application fixture that models them.
    pub const IMPLEMENTED: [&'static str; 4] = [
        "ui.read_value",
        "ui.ensure_value",
        "ui.activate",
        "ui.revisit",
    ];

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "ui.read_value" => Some(Self::ReadValue),
            "ui.ensure_value" => Some(Self::EnsureValue),
            "ui.activate" => Some(Self::Activate),
            "ui.revisit" => Some(Self::Revisit),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::ReadValue => "ui.read_value",
            Self::EnsureValue => "ui.ensure_value",
            Self::Activate => "ui.activate",
            Self::Revisit => "ui.revisit",
        }
    }

    /// How much of the root budget one attempt of this skill may use.
    pub fn quota(self) -> AttemptQuota {
        let (actions, observations) = match self {
            Self::ReadValue | Self::Activate => (1, 1),
            Self::EnsureValue => (2, 2),
            Self::Revisit => (2, 3),
        };
        AttemptQuota {
            actions,
            observations,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionPath {
    /// An accessibility action on the element: no coordinates, no pointer.
    Ax,
    /// A click at the element's centre.
    Pointer,
}

/// One bounded input a skill asks for.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionPlan {
    pub binding: Binding,
    pub primitive: Primitive,
    pub path: ExecutionPath,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SkillResult {
    /// Nothing needed doing.
    AlreadyDone,
    Succeeded,
    Failed {
        code: FailureCode,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum SkillStep {
    Act(Box<ActionPlan>),
    /// Evidence observed at or after this time is needed to go on.
    AwaitEvidence {
        after_ms: u64,
    },
    Done(SkillResult),
}

/// What became of the input the skill asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionOutcome {
    /// The bounded window of the input ended.
    Completed {
        end_ms: u64,
    },
    /// It is not known whether the input reached the application.
    Unknown {
        at_ms: u64,
    },
    Rejected(Option<FailureCode>),
    Cancelled,
}

pub struct SkillView<'a> {
    pub state: &'a StateCore,
    pub bindings: &'a mut BindingReducer,
    pub pack: &'a AppPack,
}

/// What the input that was asked for is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Follow {
    Verify,
    Away,
    Back,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Known {
    end_ms: u64,
    unknown: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum Phase {
    Start,
    /// A step asked for an input and its outcome has not been reported.
    AwaitingOutcome(Follow),
    Judging(Follow, Known),
    Ended(SkillResult),
}

#[derive(Debug, Clone)]
pub struct SkillRun {
    skill: Skill,
    target: SemanticTargetRef,
    desired: Option<ExpectedValue>,
    binding: BindingRef,
    phase: Phase,
    actions: u32,
    home_page: Option<String>,
    last_path: Option<ExecutionPath>,
}

fn fail(code: FailureCode, reason: impl Into<String>) -> SkillStep {
    SkillStep::Done(SkillResult::Failed {
        code,
        reason: reason.into(),
    })
}

fn label_of(object: &UiObject) -> &str {
    object
        .title
        .as_deref()
        .or(object.description.as_deref())
        .unwrap_or("")
}

fn click(object: &UiObject) -> Primitive {
    let (x, y) = object.frame.center();
    Primitive::Click {
        button: MouseButton::Left,
        x: x.round() as i32,
        y: y.round() as i32,
        modifiers: Modifiers::default(),
    }
}

/// The newest usable evidence, optionally no older than `floor`.
fn evidence(state: &StateCore, floor: Option<u64>) -> Result<&EnvSnapshot, SkillStep> {
    let env = match floor {
        Some(floor) => state.latest_since(floor),
        None => state.latest(),
    };
    let Some(env) = env else {
        return Err(SkillStep::AwaitEvidence {
            after_ms: floor.unwrap_or(0),
        });
    };
    if !env.snapshot.complete || env.snapshot.active_layer.is_none() {
        // What an incomplete read lacks is unknown, not absent: look again.
        return Err(SkillStep::AwaitEvidence {
            after_ms: env.snapshot.observed_ms + 1,
        });
    }
    Ok(env)
}

/// The control that switches to a page, by the page's name.
fn tab_for<'a>(env: &'a EnvSnapshot, pack: &AppPack, page: &str) -> Option<&'a UiObject> {
    let active = env.snapshot.active_layer.as_deref()?;
    let wanted = normalize_label(page);
    env.snapshot.objects.iter().find(|object| {
        object.window_id == active
            && object.role == pack.nav_role
            && object.enabled
            && object.in_view
            && !object.secure
            && normalize_label(label_of(object)) == wanted
    })
}

fn other_tab<'a>(env: &'a EnvSnapshot, pack: &AppPack, home: &str) -> Option<&'a UiObject> {
    let active = env.snapshot.active_layer.as_deref()?;
    let home = normalize_label(home);
    env.snapshot.objects.iter().find(|object| {
        object.window_id == active
            && object.role == pack.nav_role
            && object.enabled
            && object.in_view
            && !object.secure
            && !label_of(object).is_empty()
            && normalize_label(label_of(object)) != home
    })
}

impl SkillRun {
    pub fn new(
        skill: Skill,
        target: SemanticTargetRef,
        desired: Option<ExpectedValue>,
        binding: BindingRef,
    ) -> Self {
        Self {
            skill,
            target,
            desired,
            binding,
            phase: Phase::Start,
            actions: 0,
            home_page: None,
            last_path: None,
        }
    }

    pub fn skill(&self) -> Skill {
        self.skill
    }

    /// How many inputs this run has asked for.
    pub fn actions(&self) -> u32 {
        self.actions
    }

    pub fn is_finished(&self) -> bool {
        matches!(self.phase, Phase::Ended(_))
    }

    /// Report what became of the input the last step asked for.
    pub fn acted(&mut self, outcome: ActionOutcome) {
        let Phase::AwaitingOutcome(follow) = self.phase else {
            return;
        };
        self.phase = match outcome {
            ActionOutcome::Completed { end_ms } => Phase::Judging(
                follow,
                Known {
                    end_ms,
                    unknown: false,
                },
            ),
            ActionOutcome::Unknown { at_ms } => Phase::Judging(
                follow,
                Known {
                    end_ms: at_ms,
                    unknown: true,
                },
            ),
            ActionOutcome::Rejected(code) => Phase::Ended(SkillResult::Failed {
                code: code.unwrap_or(FailureCode::InputRejected),
                reason: "the device did not accept the input".into(),
            }),
            ActionOutcome::Cancelled => Phase::Ended(SkillResult::Failed {
                code: FailureCode::Cancelled,
                reason: "the input was cancelled".into(),
            }),
        };
    }

    pub fn advance(&mut self, view: &mut SkillView<'_>) -> SkillStep {
        let step = match self.phase.clone() {
            Phase::Ended(result) => return SkillStep::Done(result),
            Phase::AwaitingOutcome(_) => fail(
                FailureCode::ContextUnresolved,
                "the previous input has no reported outcome",
            ),
            Phase::Start => self.start(view),
            Phase::Judging(follow, known) => self.judge(view, follow, known),
        };
        if let SkillStep::Done(result) = &step {
            self.phase = Phase::Ended(result.clone());
        }
        step
    }

    /// The binding the attempt began with, if it still holds.
    fn bound(&self, view: &SkillView<'_>) -> Result<Binding, SkillStep> {
        match view.bindings.current(&self.target) {
            Some(binding) if binding.binding_ref == self.binding => Ok(binding.clone()),
            Some(_) => Err(fail(
                FailureCode::ContextUnresolved,
                "the target was bound again during the attempt",
            )),
            None => Err(fail(
                FailureCode::ContextUnresolved,
                "the target is no longer bound",
            )),
        }
    }

    fn act(
        &mut self,
        binding: Binding,
        primitive: Primitive,
        path: ExecutionPath,
        follow: Follow,
        note: impl Into<String>,
    ) -> SkillStep {
        self.actions += 1;
        self.last_path = Some(path);
        self.phase = Phase::AwaitingOutcome(follow);
        SkillStep::Act(Box::new(ActionPlan {
            binding,
            primitive,
            path,
            note: note.into(),
        }))
    }

    /// Press a control: the accessibility action when it declares one, a
    /// click otherwise.
    fn press(&mut self, binding: Binding, object: &UiObject, follow: Follow) -> SkillStep {
        if object.actions.contains(&AxActionName::Press) {
            self.act(
                binding,
                Primitive::AxAction {
                    action: AxActionKind::Press,
                },
                ExecutionPath::Ax,
                follow,
                format!(
                    "press \"{}\" through its accessibility action",
                    label_of(object)
                ),
            )
        } else {
            self.act(
                binding,
                click(object),
                ExecutionPath::Pointer,
                follow,
                format!("click \"{}\"", label_of(object)),
            )
        }
    }

    fn start(&mut self, view: &mut SkillView<'_>) -> SkillStep {
        let binding = match self.bound(view) {
            Ok(binding) => binding,
            Err(step) => return step,
        };
        let env = match evidence(view.state, None) {
            Ok(env) => env,
            Err(step) => return step,
        };
        let Some(object) = object_for(env, &binding.fingerprint_digest) else {
            return fail(
                FailureCode::ContextUnresolved,
                "the bound control is not in the active layer",
            );
        };
        match self.skill {
            Skill::ReadValue => match object.value {
                Some(_) => SkillStep::Done(SkillResult::Succeeded),
                None => fail(
                    FailureCode::ContextInsufficient,
                    "the control shows no value",
                ),
            },
            Skill::EnsureValue => self.ensure(&binding, object, false),
            Skill::Activate => {
                if object.enabled {
                    self.press(binding, object, Follow::Verify)
                } else {
                    fail(FailureCode::ContextUnresolved, "the control is disabled")
                }
            }
            Skill::Revisit => {
                let Some(home) = env.page.clone() else {
                    return fail(
                        FailureCode::ContextUnresolved,
                        "the current page is not known",
                    );
                };
                let Some(away) = other_tab(env, view.pack, &home) else {
                    return fail(
                        FailureCode::CandidateMissing,
                        "there is no other page to go to",
                    );
                };
                let Ok(semantic) = SemanticTargetRef::new("nav/away") else {
                    return fail(FailureCode::ContextUnresolved, "no navigation target");
                };
                let Some(nav) = view
                    .bindings
                    .bind_instance(env, semantic, &away.instance_ref)
                else {
                    return fail(
                        FailureCode::ContextUnresolved,
                        "cannot bind the navigation control",
                    );
                };
                self.home_page = Some(home);
                self.press(nav, away, Follow::Away)
            }
        }
    }

    /// Decide what `ensure_value` does about what the control shows.
    fn ensure(&mut self, binding: &Binding, object: &UiObject, unknown: bool) -> SkillStep {
        let Some(desired) = self.desired.clone() else {
            return fail(FailureCode::ContextInsufficient, "there is no value to set");
        };
        match object
            .value
            .as_ref()
            .and_then(|value| values_equal(value, &desired))
        {
            Some(true) => SkillStep::Done(if self.actions == 0 {
                SkillResult::AlreadyDone
            } else {
                SkillResult::Succeeded
            }),
            None => fail(
                FailureCode::ContextInsufficient,
                "the value could not be read",
            ),
            Some(false) if unknown => fail(
                FailureCode::EffectUnknown,
                "the input's outcome is unknown and the value has not changed",
            ),
            Some(false) => {
                let toggle = matches!(desired, ExpectedValue::Bool { .. });
                match (self.actions, self.last_path) {
                    (0, _) if object.actions.contains(&AxActionName::SetValue) => self.act(
                        binding.clone(),
                        Primitive::AxAction {
                            action: AxActionKind::SetValue {
                                value: value_text(&desired),
                            },
                        },
                        ExecutionPath::Ax,
                        Follow::Verify,
                        format!("set \"{}\" to {}", label_of(object), value_text(&desired)),
                    ),
                    // The value is known to differ, so one press switches it.
                    (0, _) if toggle && object.actions.contains(&AxActionName::Press) => self.act(
                        binding.clone(),
                        Primitive::AxAction {
                            action: AxActionKind::Press,
                        },
                        ExecutionPath::Ax,
                        Follow::Verify,
                        format!("press \"{}\" to switch it", label_of(object)),
                    ),
                    (0, _) if toggle => self.act(
                        binding.clone(),
                        click(object),
                        ExecutionPath::Pointer,
                        Follow::Verify,
                        format!("click \"{}\" to switch it", label_of(object)),
                    ),
                    (1, Some(ExecutionPath::Ax)) if toggle => self.act(
                        binding.clone(),
                        click(object),
                        ExecutionPath::Pointer,
                        Follow::Verify,
                        format!(
                            "the action had no effect; click \"{}\" instead",
                            label_of(object)
                        ),
                    ),
                    (0, _) => fail(
                        FailureCode::CandidateMissing,
                        "there is no way to set this kind of value",
                    ),
                    _ => fail(
                        FailureCode::EffectUnknown,
                        "the value did not change on either path",
                    ),
                }
            }
        }
    }

    fn judge(&mut self, view: &mut SkillView<'_>, follow: Follow, known: Known) -> SkillStep {
        let env = match evidence(view.state, Some(known.end_ms)) {
            Ok(env) => env,
            Err(step) => return step,
        };
        match (follow, self.skill) {
            (Follow::Verify, Skill::EnsureValue) => {
                let binding = match self.bound(view) {
                    Ok(binding) => binding,
                    Err(step) => return step,
                };
                let Some(object) = object_for(env, &binding.fingerprint_digest) else {
                    return fail(
                        FailureCode::ContextUnresolved,
                        "the control is gone after the input",
                    );
                };
                self.ensure(&binding, object, known.unknown)
            }
            // What a press does is judged by the goal's own evidence; the
            // skill only reports that it pressed and saw the application again.
            (Follow::Verify, _) if known.unknown => fail(
                FailureCode::EffectUnknown,
                "the input's outcome is unknown; check the effect before pressing again",
            ),
            (Follow::Verify, _) => SkillStep::Done(SkillResult::Succeeded),
            (Follow::Away, _) => {
                let Some(home) = self.home_page.clone() else {
                    return fail(
                        FailureCode::ContextUnresolved,
                        "the home page was not noted",
                    );
                };
                if env.page.as_deref() == Some(home.as_str()) {
                    return if modal_in_front(env) {
                        fail(
                            FailureCode::ContextUnresolved,
                            "a dialog opened instead of the page changing",
                        )
                    } else {
                        fail(FailureCode::EffectUnknown, "the page did not change")
                    };
                }
                let Some(back) = tab_for(env, view.pack, &home) else {
                    return fail(FailureCode::CandidateMissing, "cannot find the way back");
                };
                let Ok(semantic) = SemanticTargetRef::new("nav/back") else {
                    return fail(FailureCode::ContextUnresolved, "no navigation target");
                };
                let Some(nav) = view
                    .bindings
                    .bind_instance(env, semantic, &back.instance_ref)
                else {
                    return fail(
                        FailureCode::ContextUnresolved,
                        "cannot bind the navigation control",
                    );
                };
                self.press(nav, back, Follow::Back)
            }
            (Follow::Back, _) => {
                let home = self.home_page.clone().unwrap_or_default();
                if env.page.as_deref() == Some(home.as_str()) {
                    SkillStep::Done(SkillResult::Succeeded)
                } else if modal_in_front(env) {
                    fail(
                        FailureCode::ContextUnresolved,
                        "a dialog is in the way back",
                    )
                } else {
                    fail(FailureCode::EffectUnknown, "did not return to the page")
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::*;
    use interaction_contracts::desktop::ids::SemanticTargetRef;
    use sim_world::app::{Applied, Modal, Page};

    fn target() -> SemanticTargetRef {
        SemanticTargetRef::new(TARGET).unwrap()
    }

    fn off() -> Option<ExpectedValue> {
        Some(ExpectedValue::Bool { value: false })
    }

    /// Drive the skill one step against the rig's state.
    fn advance(run: &mut SkillRun, rig: &mut EnvRig) -> SkillStep {
        run.advance(&mut SkillView {
            state: &rig.state,
            bindings: &mut rig.bindings,
            pack: &rig.pack,
        })
    }

    /// What the device would do: apply the input to the app.
    fn apply(rig: &mut EnvRig, plan: &ActionPlan) {
        let applied = Applied {
            primitive: plan.primitive.clone(),
            binding_fingerprint: Some(plan.binding.fingerprint_digest.clone()),
        };
        rig.sequence += 1;
        let m = meta(rig.sequence, rig.now_ms);
        rig.app.apply(&m, &applied);
    }

    /// Someone else changes the switch while the skill is not running.
    fn switch_off_behind_the_scenes(rig: &mut EnvRig) {
        let proof = rig
            .app
            .fingerprint_of(&meta(1, 1), "sw-push-daily")
            .unwrap();
        rig.app.apply(
            &meta(2, 2),
            &Applied {
                primitive: Primitive::AxAction {
                    action: AxActionKind::SetValue {
                        value: "off".into(),
                    },
                },
                binding_fingerprint: Some(proof),
            },
        );
    }

    fn act(step: SkillStep) -> ActionPlan {
        match step {
            SkillStep::Act(plan) => *plan,
            other => panic!("expected an action: {other:?}"),
        }
    }

    fn ready() -> (EnvRig, SkillRun) {
        let mut rig = EnvRig::new();
        rig.observe(10);
        let binding = rig.bind_goal_target();
        let run = SkillRun::new(Skill::EnsureValue, target(), off(), binding.binding_ref);
        (rig, run)
    }

    #[test]
    fn ensure_value_sets_the_value_through_the_accessibility_action_then_reads_it_back() {
        let (mut rig, mut run) = ready();
        let plan = act(advance(&mut run, &mut rig));
        assert_eq!(plan.path, ExecutionPath::Ax);
        assert_eq!(
            plan.primitive,
            Primitive::AxAction {
                action: AxActionKind::SetValue {
                    value: "off".into()
                }
            }
        );
        assert_eq!(run.actions(), 1);
        apply(&mut rig, &plan);
        run.acted(ActionOutcome::Completed { end_ms: 110 });

        // The frame from before the input cannot judge it.
        assert_eq!(
            advance(&mut run, &mut rig),
            SkillStep::AwaitEvidence { after_ms: 110 }
        );
        rig.observe(200);
        assert_eq!(
            advance(&mut run, &mut rig),
            SkillStep::Done(SkillResult::Succeeded)
        );
        assert!(run.is_finished());
        assert_eq!(rig.app.switch_on("Push", "Daily picks"), Some(false));
        // A finished run stays finished.
        assert_eq!(
            advance(&mut run, &mut rig),
            SkillStep::Done(SkillResult::Succeeded)
        );
    }

    #[test]
    fn a_value_that_is_already_right_needs_no_input() {
        let mut rig = EnvRig::new();
        switch_off_behind_the_scenes(&mut rig);
        rig.observe(20);
        let binding = rig.bind_goal_target();
        let mut run = SkillRun::new(Skill::EnsureValue, target(), off(), binding.binding_ref);
        assert_eq!(
            advance(&mut run, &mut rig),
            SkillStep::Done(SkillResult::AlreadyDone)
        );
        assert_eq!(run.actions(), 0);
    }

    #[test]
    fn when_the_accessibility_action_changes_nothing_it_tries_the_pointer_once() {
        let (mut rig, mut run) = ready();
        let first = act(advance(&mut run, &mut rig));
        assert_eq!(first.path, ExecutionPath::Ax);
        // The application ignored it: apply nothing.
        run.acted(ActionOutcome::Completed { end_ms: 110 });
        rig.observe(200);
        let second = act(advance(&mut run, &mut rig));
        assert_eq!(second.path, ExecutionPath::Pointer);
        assert!(
            matches!(second.primitive, Primitive::Click { x: 420, y: 70, .. }),
            "{:?}",
            second.primitive
        );
        assert_eq!(run.actions(), 2);
        apply(&mut rig, &second);
        run.acted(ActionOutcome::Completed { end_ms: 310 });
        rig.observe(400);
        assert_eq!(
            advance(&mut run, &mut rig),
            SkillStep::Done(SkillResult::Succeeded)
        );
    }

    #[test]
    fn when_neither_path_changes_the_value_it_gives_up_without_a_third_input() {
        let (mut rig, mut run) = ready();
        for end in [110, 310] {
            let plan = act(advance(&mut run, &mut rig));
            let _ = plan;
            run.acted(ActionOutcome::Completed { end_ms: end });
            rig.observe(200);
        }
        match advance(&mut run, &mut rig) {
            SkillStep::Done(SkillResult::Failed { code, .. }) => {
                assert_eq!(code, FailureCode::EffectUnknown)
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(run.actions(), 2);
    }

    #[test]
    fn an_unknown_receipt_is_settled_by_the_evidence_and_never_retried() {
        // The input did land.
        let (mut rig, mut run) = ready();
        let plan = act(advance(&mut run, &mut rig));
        apply(&mut rig, &plan);
        run.acted(ActionOutcome::Unknown { at_ms: 120 });
        rig.observe(200);
        assert_eq!(
            advance(&mut run, &mut rig),
            SkillStep::Done(SkillResult::Succeeded)
        );

        // It did not: no second attempt is made on a guess.
        let (mut rig, mut run) = ready();
        act(advance(&mut run, &mut rig));
        run.acted(ActionOutcome::Unknown { at_ms: 120 });
        rig.observe(200);
        match advance(&mut run, &mut rig) {
            SkillStep::Done(SkillResult::Failed { code, .. }) => {
                assert_eq!(code, FailureCode::EffectUnknown)
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(run.actions(), 1);
    }

    #[test]
    fn a_refused_or_cancelled_input_ends_the_skill_with_its_reason() {
        let (mut rig, mut run) = ready();
        act(advance(&mut run, &mut rig));
        run.acted(ActionOutcome::Rejected(Some(FailureCode::TargetOccluded)));
        match advance(&mut run, &mut rig) {
            SkillStep::Done(SkillResult::Failed { code, .. }) => {
                assert_eq!(code, FailureCode::TargetOccluded)
            }
            other => panic!("{other:?}"),
        }
        let (mut rig, mut run) = ready();
        act(advance(&mut run, &mut rig));
        run.acted(ActionOutcome::Cancelled);
        match advance(&mut run, &mut rig) {
            SkillStep::Done(SkillResult::Failed { code, .. }) => {
                assert_eq!(code, FailureCode::Cancelled)
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_skill_whose_binding_is_gone_does_nothing() {
        let (mut rig, mut run) = ready();
        rig.bindings.invalidate_all("test");
        match advance(&mut run, &mut rig) {
            SkillStep::Done(SkillResult::Failed { code, .. }) => {
                assert_eq!(code, FailureCode::ContextUnresolved)
            }
            other => panic!("{other:?}"),
        }
        // Bound again under another reference is also not "the same target".
        let (mut rig, mut run) = ready();
        rig.bindings.invalidate_all("test");
        rig.bind_goal_target();
        match advance(&mut run, &mut rig) {
            SkillStep::Done(SkillResult::Failed { code, reason }) => {
                assert_eq!(code, FailureCode::ContextUnresolved);
                assert!(reason.contains("bound again"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_incomplete_read_is_looked_at_again_rather_than_acted_on() {
        let mut rig = EnvRig::new();
        rig.observe(10);
        let binding = rig.bind_goal_target();
        rig.sequence += 1;
        let partial = rig.app.partial_snapshot(&meta(rig.sequence, 50), 3);
        rig.state.admit(partial, 50, &rig.pack).unwrap();
        let mut run = SkillRun::new(Skill::EnsureValue, target(), off(), binding.binding_ref);
        assert_eq!(
            advance(&mut run, &mut rig),
            SkillStep::AwaitEvidence { after_ms: 51 }
        );
    }

    #[test]
    fn read_value_reports_without_any_input() {
        let mut rig = EnvRig::new();
        rig.observe(10);
        let binding = rig.bind_goal_target();
        let mut run = SkillRun::new(Skill::ReadValue, target(), None, binding.binding_ref);
        assert_eq!(
            advance(&mut run, &mut rig),
            SkillStep::Done(SkillResult::Succeeded)
        );
        assert_eq!(run.actions(), 0);
    }

    fn bind_apply(rig: &mut EnvRig) -> Binding {
        let env = rig.state.latest().unwrap().clone();
        let apply = env
            .snapshot
            .objects
            .iter()
            .find(|o| o.instance_ref.as_str() == "btn-apply")
            .unwrap();
        rig.bindings
            .bind_instance(
                &env,
                SemanticTargetRef::new("apply-button").unwrap(),
                &apply.instance_ref,
            )
            .unwrap()
    }

    #[test]
    fn activate_presses_once_and_reports_after_fresh_evidence() {
        let mut rig = EnvRig::new();
        rig.observe(10);
        let binding = bind_apply(&mut rig);
        let mut run = SkillRun::new(
            Skill::Activate,
            SemanticTargetRef::new("apply-button").unwrap(),
            None,
            binding.binding_ref,
        );
        let plan = act(advance(&mut run, &mut rig));
        assert_eq!(
            plan.primitive,
            Primitive::AxAction {
                action: AxActionKind::Press
            }
        );
        apply(&mut rig, &plan);
        run.acted(ActionOutcome::Completed { end_ms: 110 });
        assert_eq!(
            advance(&mut run, &mut rig),
            SkillStep::AwaitEvidence { after_ms: 110 }
        );
        rig.observe(200);
        assert_eq!(
            advance(&mut run, &mut rig),
            SkillStep::Done(SkillResult::Succeeded)
        );
        assert_eq!(rig.app.save_state(), sim_world::app::SaveState::Saved);
    }

    #[test]
    fn a_press_with_an_unknown_outcome_is_not_reported_as_done() {
        let mut rig = EnvRig::new();
        rig.observe(10);
        let binding = bind_apply(&mut rig);
        let mut run = SkillRun::new(
            Skill::Activate,
            SemanticTargetRef::new("apply-button").unwrap(),
            None,
            binding.binding_ref,
        );
        act(advance(&mut run, &mut rig));
        run.acted(ActionOutcome::Unknown { at_ms: 120 });
        rig.observe(200);
        match advance(&mut run, &mut rig) {
            SkillStep::Done(SkillResult::Failed { code, .. }) => {
                assert_eq!(code, FailureCode::EffectUnknown)
            }
            other => panic!("{other:?}"),
        }
    }

    fn revisit() -> (EnvRig, SkillRun) {
        let mut rig = EnvRig::new();
        rig.observe(10);
        let binding = rig.bind_goal_target();
        let run = SkillRun::new(Skill::Revisit, target(), None, binding.binding_ref);
        (rig, run)
    }

    #[test]
    fn revisit_leaves_the_page_and_comes_back() {
        let (mut rig, mut run) = revisit();
        let away = act(advance(&mut run, &mut rig));
        assert_eq!(away.binding.semantic_target.as_str(), "nav/away");
        apply(&mut rig, &away);
        run.acted(ActionOutcome::Completed { end_ms: 110 });
        rig.observe(200);
        assert_eq!(rig.app.page(), Page::General);

        let back = act(advance(&mut run, &mut rig));
        assert_eq!(back.binding.semantic_target.as_str(), "nav/back");
        apply(&mut rig, &back);
        run.acted(ActionOutcome::Completed { end_ms: 310 });
        assert_eq!(
            advance(&mut run, &mut rig),
            SkillStep::AwaitEvidence { after_ms: 310 }
        );
        rig.observe(400);
        assert_eq!(
            advance(&mut run, &mut rig),
            SkillStep::Done(SkillResult::Succeeded)
        );
        assert_eq!(rig.app.page(), Page::Notifications);
        assert_eq!(run.actions(), 2);
    }

    #[test]
    fn revisit_stops_and_reports_when_a_dialog_takes_the_navigation() {
        let mut rig = EnvRig::new();
        rig.app.sheet_on_unsaved_navigation = true;
        // An unsaved change makes leaving the page ask first.
        switch_off_behind_the_scenes(&mut rig);
        rig.observe(10);
        let binding = rig.bind_goal_target();
        let mut run = SkillRun::new(Skill::Revisit, target(), None, binding.binding_ref);
        let away = act(advance(&mut run, &mut rig));
        apply(&mut rig, &away);
        assert_eq!(rig.app.modal(), Some(Modal::UnsavedChanges));
        run.acted(ActionOutcome::Completed { end_ms: 110 });
        rig.observe(200);
        match advance(&mut run, &mut rig) {
            SkillStep::Done(SkillResult::Failed { code, reason }) => {
                assert_eq!(code, FailureCode::ContextUnresolved);
                assert!(reason.contains("dialog"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn only_the_implemented_skills_have_a_runtime() {
        for name in Skill::IMPLEMENTED {
            assert_eq!(Skill::from_name(name).unwrap().name(), name);
        }
        assert!(Skill::from_name("ui.enter_text").is_none());
        assert!(Skill::from_name("ui.scroll_to").is_none());
        assert_eq!(Skill::EnsureValue.quota().actions, 2);
    }
}
