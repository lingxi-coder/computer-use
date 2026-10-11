//! Converts a computer-use proposal into ordinary, authorized Reaction steps.
//!
//! Every point is resolved against the exact frozen full-window frame and AX
//! structure pair. Only a centered, unmodified left click that is exactly the
//! pointer action produced by `ui.activate` is currently representable. The
//! generic engine does not yet preserve the distinct semantics of double
//! click, drag, free pointer motion, raw scroll deltas, arbitrary keypresses,
//! or typed keystrokes; the compiler rejects those actions before returning a
//! batch. Wait and Screenshot remain observation-only steps.

use std::collections::{BTreeMap, BTreeSet};

use interaction_contracts::desktop::binding::{
    BindingDerivationEvidence, BindingProofScope, derive_binding_evidence, normalize_label,
};
use interaction_contracts::desktop::computer::{
    ComputerActionProposal, ComputerBatchProposal, ComputerMouseButton,
};
use interaction_contracts::desktop::decision::{Candidate, Precondition};
use interaction_contracts::desktop::effect::{Admission, DenyReason, EffectClass};
use interaction_contracts::desktop::ids::{CandidateId, ObjectRef, SemanticTargetRef};
use interaction_contracts::desktop::planning::Subgoal;
use interaction_contracts::desktop::source::{
    AxActionName, ObjectIdentityContinuity, StructureSnapshot, UiObject,
};
use interaction_contracts::desktop::task::{GoalSpec, TargetSpec};

use super::candidates::{self, Basis, Scope, is_control};
use super::computer_batch::ComputerActionKind;
use super::skills::Skill;
use crate::environment::binding::{Binding, BindingReducer};
use crate::environment::computer_observation::FrozenComputerObservation;
use crate::environment::pack::AppPack;
use crate::environment::read::focused_window_id;
use crate::environment::state_core::{EnvSnapshot, StateCore};

/// The supported mapping remains intentionally narrow. Conditions for
/// `TargetCenterClick` are checked against every action before compilation
/// succeeds, not deferred until after a prefix has run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComputerActionSupport {
    TargetCenterClick,
    ObservationOnly,
    Unsupported(&'static str),
}

pub fn action_support(kind: ComputerActionKind) -> ComputerActionSupport {
    match kind {
        ComputerActionKind::Click => ComputerActionSupport::TargetCenterClick,
        ComputerActionKind::Wait | ComputerActionKind::Screenshot => {
            ComputerActionSupport::ObservationOnly
        }
        ComputerActionKind::DoubleClick => ComputerActionSupport::Unsupported(
            "the ordinary Skill/Primitive path has no double-click primitive",
        ),
        ComputerActionKind::Drag => ComputerActionSupport::Unsupported(
            "drag needs a held-pointer lifecycle that ordinary attempts do not expose",
        ),
        ComputerActionKind::Move => ComputerActionSupport::Unsupported(
            "free pointer motion has no equivalent ordinary Skill candidate",
        ),
        ComputerActionKind::Scroll => ComputerActionSupport::Unsupported(
            "ui.scroll_to performs a bounded search, not one raw scroll delta",
        ),
        ComputerActionKind::Keypress => ComputerActionSupport::Unsupported(
            "the ordinary keyboard Skill only supports its registered fixed Space toggle",
        ),
        ComputerActionKind::Type => ComputerActionSupport::Unsupported(
            "ui.enter_text uses AXSetValue, not the proposed TypeText keystroke semantics",
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComputerCompileError {
    InvalidBatch(String),
    FrozenFenceMismatch,
    GoalFenceMismatch,
    StateUnavailable(String),
    StaleCurrentEnvironment(String),
    UnsupportedAction { index: usize, reason: &'static str },
    PointNotInsideUniqueObject { index: usize },
    AmbiguousPoint { index: usize },
    UnauthorizedTarget { index: usize },
    UnsupportedActivation { index: usize, reason: &'static str },
    IdentityChanged { index: usize },
    SubgoalChanged,
    UnsupportedSubgoal { skill: String },
    EffectNotAuthorized { index: usize, reason: String },
    InvalidStepIndex,
    BindingUnavailable { index: usize },
}

impl std::fmt::Display for ComputerCompileError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidBatch(reason) => write!(formatter, "invalid computer batch: {reason}"),
            Self::FrozenFenceMismatch => {
                formatter.write_str("computer batch does not use its frozen request fence")
            }
            Self::GoalFenceMismatch => {
                formatter.write_str("computer request is outside the frozen root goal revision")
            }
            Self::StateUnavailable(reason) => {
                write!(formatter, "computer compilation is unavailable: {reason}")
            }
            Self::StaleCurrentEnvironment(reason) => write!(
                formatter,
                "computer target cannot be rebound from current evidence: {reason}"
            ),
            Self::UnsupportedAction { index, reason } => write!(
                formatter,
                "computer action {index} is unsupported: {reason}"
            ),
            Self::PointNotInsideUniqueObject { index } => write!(
                formatter,
                "computer action {index} does not hit a unique visible object"
            ),
            Self::AmbiguousPoint { index } => write!(
                formatter,
                "computer action {index} overlaps multiple visible objects or target meanings"
            ),
            Self::UnauthorizedTarget { index } => write!(
                formatter,
                "computer action {index} does not identify an authorized goal or plan target"
            ),
            Self::UnsupportedActivation { index, reason } => write!(
                formatter,
                "computer click {index} has no exactly equivalent ordinary activation: {reason}"
            ),
            Self::IdentityChanged { index } => write!(
                formatter,
                "computer target identity or context changed before action {index}"
            ),
            Self::SubgoalChanged => {
                formatter.write_str("the installed plan sub-goal changed during the computer batch")
            }
            Self::UnsupportedSubgoal { skill } => write!(
                formatter,
                "computer click has no exact meaning in the installed {skill} sub-goal"
            ),
            Self::EffectNotAuthorized { index, reason } => write!(
                formatter,
                "computer action {index} exceeds the frozen goal: {reason}"
            ),
            Self::InvalidStepIndex => {
                formatter.write_str("computer step index is outside the compiled batch")
            }
            Self::BindingUnavailable { index } => write!(
                formatter,
                "computer action {index} cannot obtain a fresh binding for its frozen instance"
            ),
        }
    }
}

impl std::error::Error for ComputerCompileError {}

#[derive(Debug, Clone, PartialEq)]
enum CompiledStepKind {
    Candidate(Box<CompiledCandidateStep>),
    Observation,
}

#[derive(Debug, Clone, PartialEq)]
struct CompiledCandidateStep {
    instance: ObjectRef,
    semantic_target: SemanticTargetRef,
    original_evidence: BindingDerivationEvidence,
    original_identity_boundary:
        Option<interaction_contracts::desktop::source::SourceIdentityBoundary>,
    original_display_frame: interaction_contracts::desktop::source::Rect,
}

#[derive(Debug, Clone, PartialEq)]
struct CompiledStep {
    action: ComputerActionProposal,
    kind: CompiledStepKind,
}

/// A preflighted ordered batch. Its frozen request/snapshot pair and original
/// instance choices cannot be replaced by later image points or caller-built
/// identity metadata.
#[derive(Debug, Clone)]
pub struct CompiledComputerBatch {
    frozen: FrozenComputerObservation,
    proposal: ComputerBatchProposal,
    goal: GoalSpec,
    subgoal: Subgoal,
    plan_targets: BTreeSet<String>,
    pack: AppPack,
    steps: Vec<CompiledStep>,
}

impl CompiledComputerBatch {
    pub fn frozen(&self) -> &FrozenComputerObservation {
        &self.frozen
    }

    pub fn proposal(&self) -> &ComputerBatchProposal {
        &self.proposal
    }

    pub fn action_count(&self) -> usize {
        self.steps.len()
    }

    /// Resolve the next typed suggestion from a fresh Environment read. The
    /// old pixel point is never hit-tested against the new screen: it keeps
    /// the original `ObjectRef` and rebinds that semantic target only if the
    /// current closure still proves the same object.
    pub fn suggest_step(
        &self,
        action_index: usize,
        state: &StateCore,
        bindings: &mut BindingReducer,
        current_subgoal: &Subgoal,
        now_ms: u64,
        max_age_ms: u64,
    ) -> Result<ComputerStepSuggestion, ComputerCompileError> {
        if current_subgoal != &self.subgoal {
            return Err(ComputerCompileError::SubgoalChanged);
        }
        let step = self
            .steps
            .get(action_index)
            .ok_or(ComputerCompileError::InvalidStepIndex)?;
        state
            .validate_frozen_computer_observation(&self.frozen, now_ms, max_age_ms)
            .map_err(ComputerCompileError::StateUnavailable)?;
        let current = state.latest().ok_or_else(|| {
            ComputerCompileError::StateUnavailable("Source has no current structure".into())
        })?;
        validate_current_environment(&self.frozen, current, &self.goal, now_ms, max_age_ms)
            .map_err(ComputerCompileError::StaleCurrentEnvironment)?;

        match &step.kind {
            CompiledStepKind::Observation => Ok(ComputerStepSuggestion::Observation {
                action_index,
                action: step.action.clone(),
            }),
            CompiledStepKind::Candidate(candidate_step) => {
                let object = find_exact_instance(&current.snapshot, &candidate_step.instance)
                    .ok_or(ComputerCompileError::IdentityChanged {
                        index: action_index,
                    })?;
                let proof = derive_binding_evidence(
                    &current.snapshot,
                    object,
                    BindingProofScope::WholeStructure,
                )
                .map_err(|_| ComputerCompileError::IdentityChanged {
                    index: action_index,
                })?;
                if !same_live_target(
                    &self.frozen,
                    current,
                    object,
                    &candidate_step.original_evidence,
                    &candidate_step.original_identity_boundary,
                    candidate_step.original_display_frame,
                    &proof,
                ) {
                    return Err(ComputerCompileError::IdentityChanged {
                        index: action_index,
                    });
                }

                if bindings
                    .current(&candidate_step.semantic_target)
                    .is_some_and(|binding| binding.instance != candidate_step.instance)
                {
                    return Err(ComputerCompileError::IdentityChanged {
                        index: action_index,
                    });
                }
                let binding = bindings
                    .bind_instance(
                        current,
                        candidate_step.semantic_target.clone(),
                        &candidate_step.instance,
                    )
                    .ok_or(ComputerCompileError::BindingUnavailable {
                        index: action_index,
                    })?;
                if binding.continuity.kind == ObjectIdentityContinuity::ReadScoped
                    && (binding.continuity.source_sequence != current.snapshot.source_sequence
                        || current.snapshot.snapshot_id != self.frozen.snapshot().snapshot_id)
                {
                    return Err(ComputerCompileError::IdentityChanged {
                        index: action_index,
                    });
                }
                let candidate = self.candidate_for(action_index, current, binding)?;
                Ok(ComputerStepSuggestion::Candidate {
                    action_index,
                    action: step.action.clone(),
                    skill: Skill::Activate,
                    source_instance: candidate_step.instance.clone(),
                    candidate: Box::new(candidate),
                })
            }
        }
    }

    fn candidate_for(
        &self,
        action_index: usize,
        env: &EnvSnapshot,
        binding: Binding,
    ) -> Result<Candidate, ComputerCompileError> {
        let target = self
            .subgoal
            .target
            .as_ref()
            .unwrap_or(&self.goal.target.semantic_target);
        if target != &binding.semantic_target {
            return Err(ComputerCompileError::SubgoalChanged);
        }
        let resolution = candidates::Resolution::Bound(Box::new(binding));
        let scope = Scope {
            goal: &self.goal,
            plan_targets: &self.plan_targets,
            pack: &self.pack,
            env,
        };
        let built = candidates::build(
            &scope,
            &Basis::Step {
                subgoal: &self.subgoal,
                resolution: &resolution,
            },
        );
        let mut candidate = built
            .into_iter()
            .find(|candidate| !is_control(&candidate.skill))
            .ok_or(ComputerCompileError::BindingUnavailable {
                index: action_index,
            })?;
        candidate.candidate_id = CandidateId::new(format!("computer-step-{action_index}"))
            .map_err(|_| ComputerCompileError::InvalidStepIndex)?;
        match &candidate.precondition {
            Precondition::Executable => {}
            Precondition::NeedsObservation => {
                return Err(ComputerCompileError::BindingUnavailable {
                    index: action_index,
                });
            }
            Precondition::Rejected { reason } => {
                return Err(ComputerCompileError::EffectNotAuthorized {
                    index: action_index,
                    reason: reason.clone(),
                });
            }
        }
        Ok(candidate)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ComputerStepSuggestion {
    Candidate {
        action_index: usize,
        action: ComputerActionProposal,
        skill: Skill,
        source_instance: ObjectRef,
        candidate: Box<Candidate>,
    },
    Observation {
        action_index: usize,
        action: ComputerActionProposal,
    },
}

/// Trusted Engine context used to preflight every action before exposing any
/// suggestion. The plan target set must come from the installed approved plan.
#[derive(Debug, Clone, Copy)]
pub struct ComputerCompileContext<'a> {
    goal: &'a GoalSpec,
    subgoal: &'a Subgoal,
    approved_plan_targets: &'a BTreeSet<String>,
    pack: &'a AppPack,
    state: &'a StateCore,
    now_ms: u64,
    max_age_ms: u64,
}

impl<'a> ComputerCompileContext<'a> {
    pub fn new(
        goal: &'a GoalSpec,
        subgoal: &'a Subgoal,
        approved_plan_targets: &'a BTreeSet<String>,
        pack: &'a AppPack,
        state: &'a StateCore,
        now_ms: u64,
        max_age_ms: u64,
    ) -> Self {
        Self {
            goal,
            subgoal,
            approved_plan_targets,
            pack,
            state,
            now_ms,
            max_age_ms,
        }
    }
}

/// Validate the whole proposal and map every input point against the original
/// frozen observation before a caller may ask for step zero.
pub fn compile_batch(
    frozen: FrozenComputerObservation,
    proposal: ComputerBatchProposal,
    context: ComputerCompileContext<'_>,
) -> Result<CompiledComputerBatch, ComputerCompileError> {
    let ComputerCompileContext {
        goal,
        subgoal,
        approved_plan_targets,
        pack,
        state,
        now_ms,
        max_age_ms,
    } = context;
    proposal
        .validate()
        .map_err(ComputerCompileError::InvalidBatch)?;
    validate_batch_capabilities(&proposal.actions)?;
    if subgoal.skill != "ui.activate" || subgoal.scroll.is_some() || subgoal.select.is_some() {
        return Err(ComputerCompileError::UnsupportedSubgoal {
            skill: subgoal.skill.clone(),
        });
    }
    if proposal.fence != frozen.request().fence {
        return Err(ComputerCompileError::FrozenFenceMismatch);
    }
    if proposal.fence.root_task_id != goal.root_task_id
        || proposal.fence.task_revision != goal.task_revision
        || pack.bundle_id != goal.scope.app
        || frozen.snapshot().app.bundle_id != goal.scope.app
    {
        return Err(ComputerCompileError::GoalFenceMismatch);
    }
    state
        .validate_frozen_computer_observation(&frozen, now_ms, max_age_ms)
        .map_err(ComputerCompileError::StateUnavailable)?;
    let current = state.latest().ok_or_else(|| {
        ComputerCompileError::StateUnavailable("Source has no current structure".into())
    })?;
    validate_current_environment(&frozen, current, goal, now_ms, max_age_ms)
        .map_err(ComputerCompileError::StaleCurrentEnvironment)?;

    let authorized_target =
        authorized_target_for_subgoal(goal, subgoal, approved_plan_targets, pack)?;
    let authorized_targets = BTreeMap::from([(
        authorized_target.semantic_target.as_str().to_string(),
        AuthorizedTarget {
            specification: authorized_target,
        },
    )]);
    let plan_targets = approved_plan_targets.clone();
    let current_scope = Scope {
        goal,
        plan_targets: &plan_targets,
        pack,
        env: current,
    };
    let mut steps = Vec::with_capacity(proposal.actions.len());
    for (index, action) in proposal.actions.iter().enumerate() {
        let kind = ComputerActionKind::for_action(action);
        match (action, action_support(kind)) {
            (
                ComputerActionProposal::Wait | ComputerActionProposal::Screenshot,
                ComputerActionSupport::ObservationOnly,
            ) => {
                steps.push(CompiledStep {
                    action: action.clone(),
                    kind: CompiledStepKind::Observation,
                });
            }
            (
                ComputerActionProposal::Click { x, y, .. },
                ComputerActionSupport::TargetCenterClick,
            ) => {
                let (screen_x, screen_y) = frozen
                    .image_pixel_to_screen(*x, *y)
                    .map_err(|_| ComputerCompileError::PointNotInsideUniqueObject { index })?;
                let (object, target) = resolve_unique_authorized_hit(
                    &frozen,
                    screen_x,
                    screen_y,
                    &authorized_targets,
                    index,
                )?;
                if object.actions.contains(&AxActionName::Press) {
                    return Err(ComputerCompileError::UnsupportedActivation {
                        index,
                        reason: "ui.activate would issue AXPress rather than a pointer click",
                    });
                }
                let center = object.frame.center();
                if screen_x.round() as i32 != center.0.round() as i32
                    || screen_y.round() as i32 != center.1.round() as i32
                {
                    return Err(ComputerCompileError::UnsupportedActivation {
                        index,
                        reason: "ordinary ui.activate clicks the target center, not this point",
                    });
                }

                let source_proof = derive_binding_evidence(
                    frozen.snapshot(),
                    object,
                    BindingProofScope::WholeStructure,
                )
                .map_err(|_| ComputerCompileError::IdentityChanged { index })?;
                if !source_proof.device_context.supports_native_input() {
                    return Err(ComputerCompileError::UnsupportedActivation {
                        index,
                        reason: "the ordinary Engine cannot admit this Device witness scope",
                    });
                }
                let current_object = find_exact_instance(&current.snapshot, &object.instance_ref)
                    .ok_or(ComputerCompileError::IdentityChanged { index })?;
                let current_proof = derive_binding_evidence(
                    &current.snapshot,
                    current_object,
                    BindingProofScope::WholeStructure,
                )
                .map_err(|_| ComputerCompileError::IdentityChanged { index })?;
                if !same_live_target(
                    &frozen,
                    current,
                    current_object,
                    &source_proof,
                    &frozen.snapshot().identity_boundary,
                    object.frame,
                    &current_proof,
                ) {
                    return Err(ComputerCompileError::IdentityChanged { index });
                }
                if source_proof.continuity.kind == ObjectIdentityContinuity::ReadScoped
                    && (index != 0
                        || proposal.actions.iter().enumerate().any(|(later, next)| {
                            later > index
                                && matches!(
                                    next,
                                    ComputerActionProposal::Click { .. }
                                        | ComputerActionProposal::DoubleClick { .. }
                                        | ComputerActionProposal::Drag { .. }
                                        | ComputerActionProposal::Move { .. }
                                        | ComputerActionProposal::Scroll { .. }
                                        | ComputerActionProposal::Keypress { .. }
                                        | ComputerActionProposal::Type { .. }
                                )
                        }))
                {
                    return Err(ComputerCompileError::UnsupportedActivation {
                        index,
                        reason: "a read-scoped target cannot survive a later input's fresh read",
                    });
                }

                let class = skill_class_for_activation(&label_of(object), object, pack);
                let admission =
                    candidates::gate(&current_scope, class, Some(&target.semantic_target), false);
                if let Admission::Deny(reason) = admission {
                    return Err(ComputerCompileError::EffectNotAuthorized {
                        index,
                        reason: deny_text(reason).into(),
                    });
                }
                steps.push(CompiledStep {
                    action: action.clone(),
                    kind: CompiledStepKind::Candidate(Box::new(CompiledCandidateStep {
                        instance: object.instance_ref.clone(),
                        semantic_target: target.semantic_target.clone(),
                        original_evidence: source_proof,
                        original_identity_boundary: frozen.snapshot().identity_boundary.clone(),
                        original_display_frame: object.frame,
                    })),
                });
            }
            (_, ComputerActionSupport::Unsupported(reason)) => {
                return Err(ComputerCompileError::UnsupportedAction { index, reason });
            }
            _ => {
                return Err(ComputerCompileError::UnsupportedAction {
                    index,
                    reason: "action representation does not match its support class",
                });
            }
        }
    }
    Ok(CompiledComputerBatch {
        frozen,
        proposal,
        goal: goal.clone(),
        subgoal: subgoal.clone(),
        plan_targets,
        pack: pack.clone(),
        steps,
    })
}

fn validate_current_environment(
    frozen: &FrozenComputerObservation,
    current: &EnvSnapshot,
    goal: &GoalSpec,
    now_ms: u64,
    max_age_ms: u64,
) -> Result<(), String> {
    let old = frozen.snapshot();
    let snapshot = &current.snapshot;
    if !snapshot.complete
        || snapshot.menu_context.is_some()
        || snapshot.observer_coverage.state
            == interaction_contracts::desktop::source::SourceState::Unavailable
        || snapshot.global_focus != old.global_focus
        || snapshot.app != old.app
        || snapshot.session_epoch != old.session_epoch
        || snapshot.surface != old.surface
        || snapshot.clock_domain != old.clock_domain
        || snapshot.identity_boundary != old.identity_boundary
        || current.layer_revision != frozen.layer_revision()
        || current.page.as_deref() != frozen.page()
        || snapshot.source_sequence < frozen.request().fence.structure_sequence
        || snapshot.observed_ms > now_ms
        || now_ms.saturating_sub(snapshot.observed_ms) > max_age_ms
        || snapshot.app.bundle_id != goal.scope.app
    {
        return Err("current Source snapshot no longer matches the frozen active context".into());
    }
    if snapshot.source_sequence == old.source_sequence && snapshot.snapshot_id != old.snapshot_id {
        return Err("Source reused a sequence for a different snapshot".into());
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct AuthorizedTarget {
    specification: TargetSpec,
}

fn authorized_target_for_subgoal(
    goal: &GoalSpec,
    subgoal: &Subgoal,
    plan_targets: &BTreeSet<String>,
    pack: &AppPack,
) -> Result<TargetSpec, ComputerCompileError> {
    let semantic_target = subgoal
        .target
        .as_ref()
        .unwrap_or(&goal.target.semantic_target);
    if semantic_target == &goal.target.semantic_target {
        return Ok(goal.target.clone());
    }
    if !plan_targets.contains(semantic_target.as_str()) {
        return Err(ComputerCompileError::GoalFenceMismatch);
    }
    if let Some(control) = pack.control(semantic_target) {
        return Ok(TargetSpec {
            row: None,
            semantic_target: semantic_target.clone(),
            label: control.label.clone(),
            group: None,
            role_hint: Some(control.role.clone()),
        });
    }
    // Match `Running::describe_target`: unknown approved names are readable
    // labels only; unique live object checks remain required.
    Ok(TargetSpec {
        row: None,
        semantic_target: semantic_target.clone(),
        label: semantic_target.as_str().replace(['-', '_', '/'], " "),
        group: None,
        role_hint: None,
    })
}

fn resolve_unique_authorized_hit<'a>(
    frozen: &'a FrozenComputerObservation,
    screen_x: f64,
    screen_y: f64,
    targets: &'a BTreeMap<String, AuthorizedTarget>,
    index: usize,
) -> Result<(&'a UiObject, &'a TargetSpec), ComputerCompileError> {
    let snapshot = frozen.snapshot();
    let active = focused_window_id(snapshot)
        .ok_or(ComputerCompileError::PointNotInsideUniqueObject { index })?;
    let hits: Vec<&UiObject> = snapshot
        .objects
        .iter()
        .filter(|object| {
            object.window_id == active
                && object.enabled
                && object.in_view
                && !object.secure
                && object.frame.valid_bounded()
                && object.frame.contains(screen_x, screen_y)
                && is_pointer_target_role(&object.role)
        })
        .collect();
    let object = match hits.as_slice() {
        [] => return Err(ComputerCompileError::PointNotInsideUniqueObject { index }),
        [object] => *object,
        _ => return Err(ComputerCompileError::AmbiguousPoint { index }),
    };
    let label = label_of(object);
    let matching_targets: Vec<&TargetSpec> = targets
        .values()
        .filter_map(|target| {
            let specification = &target.specification;
            target_matches(specification, object, &label).then_some(specification)
        })
        .collect();
    match matching_targets.as_slice() {
        [] => Err(ComputerCompileError::UnauthorizedTarget { index }),
        [target] => Ok((object, target)),
        _ => Err(ComputerCompileError::AmbiguousPoint { index }),
    }
}

fn target_matches(target: &TargetSpec, object: &UiObject, label: &str) -> bool {
    let group_matches = target.group.as_deref().is_none_or(|wanted| {
        object
            .group
            .as_deref()
            .is_some_and(|actual| normalize_label(actual) == normalize_label(wanted))
    });
    normalize_label(label) == normalize_label(&target.label)
        && target
            .role_hint
            .as_deref()
            .is_none_or(|role| role == object.role)
        && group_matches
}

pub(crate) fn validate_batch_capabilities(
    actions: &[ComputerActionProposal],
) -> Result<(), ComputerCompileError> {
    for (index, action) in actions.iter().enumerate() {
        match action_support(ComputerActionKind::for_action(action)) {
            ComputerActionSupport::Unsupported(reason) => {
                return Err(ComputerCompileError::UnsupportedAction { index, reason });
            }
            ComputerActionSupport::TargetCenterClick => {
                let ComputerActionProposal::Click { button, keys, .. } = action else {
                    return Err(ComputerCompileError::UnsupportedAction {
                        index,
                        reason: "action representation does not match its support class",
                    });
                };
                if *button != ComputerMouseButton::Left
                    || keys.as_ref().is_some_and(|keys| !keys.is_empty())
                {
                    return Err(ComputerCompileError::UnsupportedAction {
                        index,
                        reason: "only left click with no modifiers has an existing Skill equivalent",
                    });
                }
            }
            ComputerActionSupport::ObservationOnly => {}
        }
    }
    Ok(())
}

fn is_pointer_target_role(role: &str) -> bool {
    !matches!(
        role,
        "AXApplication"
            | "AXWindow"
            | "AXSheet"
            | "AXGroup"
            | "AXSplitGroup"
            | "AXToolbar"
            | "AXScrollArea"
            | "AXTable"
            | "AXRow"
            | "AXCell"
            | "AXList"
            | "AXOutline"
            | "AXTabGroup"
            | "AXStaticText"
            | "AXText"
            | "AXHeading"
    )
}

fn label_of(object: &UiObject) -> String {
    object
        .title
        .as_deref()
        .or(object.description.as_deref())
        .unwrap_or("")
        .to_string()
}

fn skill_class_for_activation(label: &str, object: &UiObject, pack: &AppPack) -> EffectClass {
    let base = crate::tasks::goal::skill_class("ui.activate").unwrap_or(EffectClass::Unknown);
    base.raise(pack.effect_class(&object.role, label))
}

fn deny_text(reason: DenyReason) -> &'static str {
    match reason {
        DenyReason::OutOfScope => "target application is outside the frozen task scope",
        DenyReason::TargetNotCovered => "target is not the frozen goal or an approved plan target",
        DenyReason::DestructiveNotNamed => "destructive target is not explicitly named by the goal",
        DenyReason::ExternalNotSupported => "external effects are not supported",
        DenyReason::UnknownEffect => "the registered effect class is unknown",
    }
}

fn same_live_target(
    frozen: &FrozenComputerObservation,
    current: &EnvSnapshot,
    object: &UiObject,
    original_evidence: &BindingDerivationEvidence,
    original_identity_boundary: &Option<
        interaction_contracts::desktop::source::SourceIdentityBoundary,
    >,
    original_frame: interaction_contracts::desktop::source::Rect,
    current_evidence: &BindingDerivationEvidence,
) -> bool {
    let Some(original) = frozen
        .snapshot()
        .objects
        .iter()
        .find(|candidate| candidate.instance_ref == object.instance_ref)
    else {
        return false;
    };
    current.snapshot.identity_boundary.as_ref() == original_identity_boundary.as_ref()
        && object.window_id == focused_window_id(&current.snapshot).unwrap_or_default()
        && object.enabled
        && object.in_view
        && !object.secure
        && pointer_click_equivalent(original, object)
        && original.frame == original_frame
        && object.frame == original_frame
        && original_evidence
            .continuity
            .same_closure(&current_evidence.continuity)
        && current_evidence.device_context == original_evidence.device_context
        && (original_evidence.continuity.kind != ObjectIdentityContinuity::ReadScoped
            || current.snapshot.source_sequence == original_evidence.continuity.source_sequence
                && current.snapshot.snapshot_id == frozen.snapshot().snapshot_id)
}

fn pointer_click_equivalent(original: &UiObject, current: &UiObject) -> bool {
    !original.actions.contains(&AxActionName::Press)
        && !current.actions.contains(&AxActionName::Press)
}

fn find_exact_instance<'a>(
    snapshot: &'a StructureSnapshot,
    instance: &ObjectRef,
) -> Option<&'a UiObject> {
    let mut found = snapshot
        .objects
        .iter()
        .filter(|object| &object.instance_ref == instance);
    let object = found.next()?;
    found.next().is_none().then_some(object)
}

#[cfg(test)]
mod tests {
    use super::*;
    use interaction_contracts::desktop::computer::{
        ComputerActionProposal, ComputerFence, ComputerFrameRequest, ComputerPoint,
    };
    use interaction_contracts::desktop::device::DeviceIdentity;
    use interaction_contracts::desktop::ids::{RequestId, RootTaskId};
    use interaction_contracts::desktop::source::{FrameMeta, FrameStatus, FrameStreamInfo, Rect};
    use interaction_contracts::desktop::task::ConfirmationScope;
    use std::collections::BTreeMap;

    fn click_fixture() -> (
        StateCore,
        ComputerFrameRequest,
        GoalSpec,
        Subgoal,
        AppPack,
        ComputerBatchProposal,
    ) {
        let mut snapshot = sim_world::app::SimApp::new().snapshot(&crate::testkit::meta(1, 100));
        snapshot
            .objects
            .iter_mut()
            .find(|object| object.instance_ref.as_str() == "sw-push-daily")
            .unwrap()
            .actions
            .clear();
        snapshot.observer_coverage =
            interaction_contracts::desktop::source::ObserverCoverage::for_projected_objects(
                &snapshot.objects,
            );
        let window_id = snapshot
            .global_focus
            .known_window_for_app(&snapshot.app)
            .unwrap()
            .to_owned();
        let window = snapshot
            .windows
            .iter()
            .find(|window| window.window_id == window_id)
            .unwrap();
        let frame = FrameMeta {
            frame_id: "frame-1".into(),
            session_epoch: snapshot.session_epoch,
            surface: snapshot.surface.clone(),
            clock_domain: snapshot.clock_domain.clone(),
            source_sequence: 1,
            source_ms: snapshot.observed_ms,
            status: FrameStatus::Complete,
            geometry_revision: 1,
            width_px: 600,
            height_px: 400,
            scale_factor: 1.0,
            byte_length: 600 * 400 * 4,
            checksum: "sha256:fixture-frame".into(),
            provenance: Some(
                interaction_contracts::desktop::source::FrameProvenance::Stream {
                    stream: FrameStreamInfo {
                        stream_id: "fixture-stream".into(),
                        image_frame_id: Some("fixture-image-1".into()),
                        display_time_ticks: snapshot.observed_ms * 1_000_000,
                        display_time_ns: snapshot.observed_ms * 1_000_000,
                        received_uptime_ns: snapshot.observed_ms * 1_000_000,
                        received_unix_ms: 100_000 + snapshot.observed_ms,
                        callback_ms: snapshot.observed_ms,
                        source_clock: "mach_absolute".into(),
                        window_id,
                        pid: snapshot.app.pid,
                        window_frame: window.frame,
                        content_rect: Rect {
                            x: 0.0,
                            y: 0.0,
                            width: 600.0,
                            height: 400.0,
                        },
                        content_scale: 1.0,
                        dirty_rects: Vec::new(),
                        dropped_frames: 0,
                        capture_interval_ms: 100,
                    },
                },
            ),
        };
        snapshot.frame_ref = frame.stream_info().unwrap().image_frame_id.clone();
        let goal = crate::testkit::goal(ConfirmationScope::UiRevisit, EffectClass::ReversibleEdit);
        let subgoal = Subgoal {
            id: "activate-daily-picks".into(),
            description: "Activate Daily picks".into(),
            skill: "ui.activate".into(),
            target: Some(goal.target.semantic_target.clone()),
            expects: Vec::new(),
            scroll: None,
            select: None,
        };
        let pack = AppPack::settings_fixture();
        let request = ComputerFrameRequest {
            fence: ComputerFence {
                request_id: RequestId::new("computer-request-1").unwrap(),
                generation: 1,
                root_task_id: RootTaskId::new("root-1").unwrap(),
                task_revision: 1,
                root_epoch: 1,
                snapshot_id: snapshot.snapshot_id.as_str().into(),
                structure_sequence: snapshot.source_sequence,
                deadline_ms: 1_000,
            },
            expected_frame: frame.clone(),
        };
        let identity = DeviceIdentity {
            session_epoch: snapshot.session_epoch,
            surface: snapshot.surface.clone(),
            clock_domain: snapshot.clock_domain.clone(),
            dependencies: BTreeMap::new(),
        };
        let mut state = StateCore::new();
        state.set_identity(&identity, false);
        state.admit(snapshot, 100, &pack).unwrap();
        state.admit_frame(frame, 100, 1_000).unwrap();
        let proposal = ComputerBatchProposal {
            fence: request.fence.clone(),
            actions: vec![ComputerActionProposal::Click {
                button: ComputerMouseButton::Left,
                x: 420.0,
                y: 70.0,
                keys: None,
            }],
        };
        (state, request, goal, subgoal, pack, proposal)
    }

    #[test]
    fn support_matrix_keeps_unimplemented_action_semantics_explicit() {
        assert_eq!(
            action_support(ComputerActionKind::Click),
            ComputerActionSupport::TargetCenterClick
        );
        assert_eq!(
            action_support(ComputerActionKind::Wait),
            ComputerActionSupport::ObservationOnly
        );
        for (kind, action) in [
            (
                ComputerActionKind::DoubleClick,
                ComputerActionProposal::DoubleClick {
                    x: 1.0,
                    y: 1.0,
                    keys: None,
                },
            ),
            (
                ComputerActionKind::Drag,
                ComputerActionProposal::Drag {
                    path: vec![
                        ComputerPoint { x: 1.0, y: 1.0 },
                        ComputerPoint { x: 2.0, y: 2.0 },
                    ],
                    keys: None,
                },
            ),
            (
                ComputerActionKind::Move,
                ComputerActionProposal::Move {
                    x: 1.0,
                    y: 1.0,
                    keys: None,
                },
            ),
            (
                ComputerActionKind::Scroll,
                ComputerActionProposal::Scroll {
                    scroll_x: 0.0,
                    scroll_y: 1.0,
                    x: 1.0,
                    y: 1.0,
                    keys: None,
                },
            ),
            (
                ComputerActionKind::Keypress,
                ComputerActionProposal::Keypress {
                    keys: vec!["Space".into()],
                },
            ),
            (
                ComputerActionKind::Type,
                ComputerActionProposal::Type { text: "x".into() },
            ),
        ] {
            let _ = action.validate();
            assert!(matches!(
                action_support(kind),
                ComputerActionSupport::Unsupported(_)
            ));
        }
    }

    #[test]
    fn complete_batch_capabilities_are_checked_before_any_candidate_is_available() {
        let actions = vec![
            ComputerActionProposal::Click {
                button: ComputerMouseButton::Left,
                x: 20.0,
                y: 20.0,
                keys: None,
            },
            ComputerActionProposal::Type {
                text: "later".into(),
            },
        ];
        assert_eq!(
            validate_batch_capabilities(&actions),
            Err(ComputerCompileError::UnsupportedAction {
                index: 1,
                reason: "ui.enter_text uses AXSetValue, not the proposed TypeText keystroke semantics",
            })
        );

        for (button, keys) in [
            (ComputerMouseButton::Right, None),
            (ComputerMouseButton::Left, Some(vec!["Shift".into()])),
        ] {
            let actions = [ComputerActionProposal::Click {
                button,
                x: 20.0,
                y: 20.0,
                keys,
            }];
            assert!(matches!(
                validate_batch_capabilities(&actions),
                Err(ComputerCompileError::UnsupportedAction { index: 0, .. })
            ));
        }
    }

    #[test]
    fn a_new_axpress_action_invalidates_the_pointer_click_mapping() {
        let rig = crate::testkit::EnvRig::new();
        let env = rig.app.snapshot(&crate::testkit::meta(1, 10));
        let original = env
            .objects
            .iter()
            .find(|object| object.instance_ref.as_str() == "sw-push-daily")
            .unwrap()
            .clone();
        let mut frozen_without_press = original.clone();
        frozen_without_press.actions.clear();
        assert!(pointer_click_equivalent(
            &frozen_without_press,
            &frozen_without_press
        ));

        let mut current_with_press = frozen_without_press.clone();
        current_with_press.actions.push(AxActionName::Press);
        assert!(!pointer_click_equivalent(
            &frozen_without_press,
            &current_with_press
        ));
        assert!(!pointer_click_equivalent(&original, &current_with_press));
    }

    #[test]
    fn centered_authorized_click_compiles_to_an_ordinary_candidate() {
        let (state, request, goal, subgoal, pack, proposal) = click_fixture();
        let frozen = state
            .freeze_computer_observation(request, 100, 1_000)
            .unwrap();
        let batch = compile_batch(
            frozen,
            proposal,
            ComputerCompileContext::new(
                &goal,
                &subgoal,
                &BTreeSet::new(),
                &pack,
                &state,
                100,
                1_000,
            ),
        )
        .unwrap();
        let mut bindings = BindingReducer::new();
        let ComputerStepSuggestion::Candidate {
            action_index,
            skill,
            source_instance,
            candidate,
            ..
        } = batch
            .suggest_step(0, &state, &mut bindings, &subgoal, 100, 1_000)
            .unwrap()
        else {
            panic!("an input click must compile to a normal Candidate");
        };
        assert_eq!(action_index, 0);
        assert_eq!(skill, Skill::Activate);
        assert_eq!(source_instance.as_str(), "sw-push-daily");
        assert_eq!(candidate.skill, "ui.activate");
        assert_eq!(candidate.precondition, Precondition::Executable);
    }

    #[test]
    fn a_fresh_read_that_adds_axpress_cannot_become_a_pointer_candidate() {
        let (mut state, request, goal, subgoal, pack, proposal) = click_fixture();
        let frozen = state
            .freeze_computer_observation(request, 100, 1_000)
            .unwrap();
        let batch = compile_batch(
            frozen,
            proposal,
            ComputerCompileContext::new(
                &goal,
                &subgoal,
                &BTreeSet::new(),
                &pack,
                &state,
                100,
                1_000,
            ),
        )
        .unwrap();
        let mut current = state.latest().unwrap().snapshot.clone();
        current.frame_ref = None;
        current.snapshot_id = RequestId::new("snap-2").unwrap();
        current.source_sequence = 2;
        current.observed_ms = 150;
        current
            .objects
            .iter_mut()
            .find(|object| object.instance_ref.as_str() == "sw-push-daily")
            .unwrap()
            .actions
            .push(AxActionName::Press);
        current.observer_coverage =
            interaction_contracts::desktop::source::ObserverCoverage::for_projected_objects(
                &current.objects,
            );
        state.admit(current, 150, &pack).unwrap();

        let mut bindings = BindingReducer::new();
        assert!(matches!(
            batch.suggest_step(0, &state, &mut bindings, &subgoal, 150, 1_000),
            Err(ComputerCompileError::IdentityChanged { index: 0 })
        ));
    }
}
