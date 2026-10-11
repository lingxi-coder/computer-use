//! SkillRuntime: short skills that can each be checked (design 15).
//!
//! A skill re-locates its target, checks that it can be operated, asks for one
//! bounded input, waits for evidence newer than that input and judges the
//! progress. It is a state machine: the engine feeds it evidence and the
//! outcome of the input it asked for, and it answers with the next step. It
//! never submits anything itself and never takes a receipt as proof of the
//! effect.

use interaction_contracts::desktop::binding::{
    BindingProofScope, DeviceContextWitness, LayoutIndependentSpecialKey,
    derive_background_ax_binding_evidence, instance_fingerprint, normalize_label,
};
use interaction_contracts::desktop::browser::{BrowserCheckboxBinding, BrowserPageProfile};
use interaction_contracts::desktop::browser_execution::BrowserAction;
use interaction_contracts::desktop::domain::ExecutionDomain;
use interaction_contracts::desktop::effect::EffectClass;
use interaction_contracts::desktop::error::FailureCode;
use interaction_contracts::desktop::ids::{BindingRef, SemanticTargetRef};
use interaction_contracts::desktop::planning::{ScrollToSpec, SelectByLabelSpec};
use interaction_contracts::desktop::primitive::{AxActionKind, Modifiers, MouseButton, Primitive};
use interaction_contracts::desktop::source::{
    AxActionName, FocusContext, LayerKind, SourceState, TargetContext, UiObject, UiValue,
};
use interaction_contracts::desktop::task::{
    AttemptQuota, ExpectedValue, GoalSpec, MAX_SCOPED_FORM_SAFE_INTEGER, ScopedCommitKind,
    ScopedUiRevisit, TargetSpec,
};

#[path = "menu.rs"]
mod menu;
#[path = "row.rs"]
pub mod row;
#[path = "scroll.rs"]
mod scroll;

use super::candidates::modal_in_front;
use crate::environment::binding::{Binding, BindingReducer, background_readback_matches};
use crate::environment::pack::{
    AppPack, ControlDef, ScopedControlContextKind, ScopedControlValueKind,
};
use crate::environment::read::{
    focused_window_id, object_for, scoped_number_text_i64, scoped_number_text_matches,
    scoped_number_text_value, value_text, values_equal,
};
use crate::environment::row::row_scope_coverage;
use crate::environment::state_core::{EnvSnapshot, StateCore};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skill {
    ReadValue,
    EnsureValue,
    EnterText,
    SaveDocument,
    Activate,
    ActivateWindow,
    Revisit,
    ScrollTo,
    SelectByLabel,
    SelectRow,
}

impl Skill {
    /// The skills this build can run. The others are in the contract
    /// vocabulary but need an application fixture that models them.
    pub const IMPLEMENTED: [&'static str; 10] = [
        "ui.read_value",
        "ui.ensure_value",
        "ui.enter_text",
        "ui.save_document",
        "ui.activate",
        "ui.activate_window",
        "ui.revisit",
        "ui.scroll_to",
        "ui.select_by_label",
        "ui.select_row",
    ];

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "ui.read_value" => Some(Self::ReadValue),
            "ui.ensure_value" => Some(Self::EnsureValue),
            "ui.enter_text" => Some(Self::EnterText),
            "ui.save_document" => Some(Self::SaveDocument),
            "ui.activate" => Some(Self::Activate),
            "ui.activate_window" => Some(Self::ActivateWindow),
            "ui.revisit" => Some(Self::Revisit),
            "ui.scroll_to" => Some(Self::ScrollTo),
            "ui.select_by_label" => Some(Self::SelectByLabel),
            "ui.select_row" => Some(Self::SelectRow),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::ReadValue => "ui.read_value",
            Self::EnsureValue => "ui.ensure_value",
            Self::EnterText => "ui.enter_text",
            Self::SaveDocument => "ui.save_document",
            Self::Activate => "ui.activate",
            Self::ActivateWindow => "ui.activate_window",
            Self::Revisit => "ui.revisit",
            Self::ScrollTo => "ui.scroll_to",
            Self::SelectByLabel => "ui.select_by_label",
            Self::SelectRow => "ui.select_row",
        }
    }

    /// How much of the root budget one attempt of this skill may use.
    pub fn quota(self) -> AttemptQuota {
        let (actions, observations) = match self {
            Self::ReadValue | Self::Activate => (1, 1),
            Self::ActivateWindow => (1, 3),
            Self::EnsureValue | Self::EnterText => (2, 2),
            Self::SaveDocument => (1, 2),
            Self::Revisit => (2, 3),
            Self::ScrollTo => (16, 17),
            Self::SelectByLabel => (2, 3),
            Self::SelectRow => (1, 2),
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
    /// One registered atomic key pair on the actual focused Source receiver.
    Keyboard,
    Window,
}

/// One bounded input a skill asks for.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionPlan {
    pub binding: Binding,
    pub primitive: Primitive,
    pub path: ExecutionPath,
    pub note: String,
}

/// One typed action on the fixed browser checkbox. The proof is a Source
/// binding, not a native Binding or Primitive.
#[derive(Debug, Clone, PartialEq)]
pub struct BrowserActionPlan {
    pub binding: BrowserCheckboxBinding,
    pub action: BrowserAction,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SkillResult {
    /// Nothing needed doing.
    AlreadyDone,
    Succeeded,
    NotFound {
        observation_complete: bool,
        search_complete: bool,
        reason: String,
    },
    Failed {
        code: FailureCode,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum SkillStep {
    Act(Box<ActionPlan>),
    BrowserAct(Box<BrowserActionPlan>),
    /// One proof-bound absolute AXSelected=true action. It is not a generic
    /// pointer or AXPress ActionPlan.
    SelectRow(Box<row::RowSelectionActionPlan>),
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
    pub now_ms: u64,
    pub max_age_ms: u64,
}

/// What the input that was asked for is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Follow {
    Verify,
    FocusText,
    PrepareScroll,
    OpenMenu,
    ChooseMenuItem,
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
    execution_domain: ExecutionDomain,
    activation: Option<(String, u32, String)>,
    input_floor: Option<u64>,
    skill: Skill,
    target: SemanticTargetRef,
    desired: Option<ExpectedValue>,
    binding: BindingRef,
    phase: Phase,
    actions: u32,
    home_page: Option<String>,
    scoped_revisit: Option<ScopedUiRevisit>,
    scoped_form_text: bool,
    scoped_form_focus_source_sequence: Option<u64>,
    scoped_radio_choice: Option<ScopedRadioChoiceSpec>,
    radio_input_source_sequence: Option<u64>,
    last_scoped_source_sequence: Option<u64>,
    last_path: Option<ExecutionPath>,
    scroll: Option<scroll::ScrollRun>,
    scroll_pointer_preparation: bool,
    menu: Option<menu::MenuRun>,
    keyboard_context: Option<FocusContext>,
    row_target: Option<TargetSpec>,
    row_input_sequence: Option<u64>,
    /// The read that authorized a WindowRoot activation. Later source reads
    /// may refresh a covered continuity proof, so keep the action's original
    /// sequence separately for post-action focus verification.
    window_root_source_sequence: Option<u64>,
    background_input_source_sequence: Option<u64>,
    background_target_context_hash: Option<String>,
    /// The completed input's proof is retained only to compare stable
    /// BackgroundAX semantic context during read-only post-action verification.
    background_input_binding: Option<Binding>,
    browser_page_profile: Option<BrowserPageProfile>,
    browser_original_binding: Option<BrowserCheckboxBinding>,
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

fn background_object_in_scope<'a>(
    view: &SkillView<'_>,
    env: &'a EnvSnapshot,
    binding: &Binding,
) -> Option<(&'a UiObject, String)> {
    if binding.rule != "background_ax"
        || !env.snapshot.complete
        || env.snapshot.menu_context.is_some()
        || env.snapshot.validate().is_err()
    {
        return None;
    }
    let BindingProofScope::BackgroundAx {
        target_context_hash,
    } = &binding.continuity.scope
    else {
        return None;
    };
    let TargetContext::NativeWindow { app, source } = env.snapshot.target_context.as_ref()? else {
        return None;
    };
    if app != &env.snapshot.app
        || source.target_context_hash.as_str() != target_context_hash.as_str()
        || !source.is_visible_standard_window()
        || !view.state.background_ax_usable(
            (&app.bundle_id, app.pid),
            &source.window.origin_window_id,
            Some((
                source.window.process.start_seconds,
                source.window.process.start_micros,
            )),
            Some(source.target_context_hash.as_str()),
            view.now_ms,
            view.max_age_ms.min(2_000),
        )
    {
        return None;
    }
    let object = env
        .snapshot
        .objects
        .iter()
        .find(|object| object.instance_ref == binding.instance)?;
    if !object.enabled
        || !object.in_view
        || object.secure
        || object.scroll.is_some()
        || instance_fingerprint(&env.snapshot, object).digest() != binding.fingerprint_digest
    {
        return None;
    }
    let evidence = derive_background_ax_binding_evidence(&env.snapshot, object).ok()?;
    if !evidence.device_context.supports_background_ax_press()
        || !binding.continuity.same_closure(&evidence.continuity)
        || binding.continuity.source_sequence > evidence.continuity.source_sequence
        || env.snapshot.source_sequence != source.window.verified_source_sequence
        || env.snapshot.observed_ms != source.window.verified_observed_ms
    {
        return None;
    }
    let DeviceContextWitness::BackgroundAxV1 {
        source: admitted_source,
        ..
    } = &binding.device_context
    else {
        return None;
    };
    if admitted_source.window.source_session_id != source.window.source_session_id
        || admitted_source.window.session_epoch != source.window.session_epoch
        || admitted_source.window.process != source.window.process
        || admitted_source.window.origin_window_id != source.window.origin_window_id
        || admitted_source.window.origin_window_frame != source.window.origin_window_frame
    {
        return None;
    }
    Some((object, source.target_context_hash.clone()))
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
    if !env.snapshot.complete || focused_window_id(&env.snapshot).is_none() {
        // What an incomplete read lacks is unknown, not absent: look again.
        return Err(SkillStep::AwaitEvidence {
            after_ms: env.snapshot.observed_ms + 1,
        });
    }
    Ok(env)
}

/// The control that switches to a page, by the page's name.
fn tab_for<'a>(env: &'a EnvSnapshot, pack: &AppPack, page: &str) -> Option<&'a UiObject> {
    let active = focused_window_id(&env.snapshot)?;
    let wanted = normalize_label(page);
    env.snapshot.objects.iter().find(|object| {
        object.window_id == active
            && pack
                .navigation_page(object)
                .is_some_and(|candidate| normalize_label(&candidate) == wanted)
            && object.enabled
            && object.in_view
            && !object.secure
    })
}

fn other_tab<'a>(env: &'a EnvSnapshot, pack: &AppPack, home: &str) -> Option<&'a UiObject> {
    let active = focused_window_id(&env.snapshot)?;
    let home = normalize_label(home);
    env.snapshot.objects.iter().find(|object| {
        object.window_id == active
            && pack
                .navigation_page(object)
                .is_some_and(|page| !page.is_empty() && normalize_label(&page) != home)
            && object.enabled
            && object.in_view
            && !object.secure
    })
}

pub(crate) struct ScopedRevisitControls<'a> {
    pub home: &'a UiObject,
    pub away: &'a UiObject,
    pub home_page: String,
    pub away_page: String,
}

fn endpoint_matches_control(
    endpoint: &TargetSpec,
    control: &crate::environment::pack::ControlDef,
) -> bool {
    endpoint.semantic_target.as_str() == control.semantic
        && endpoint.label == control.label
        && endpoint.role_hint.as_deref() == Some(control.role.as_str())
        && endpoint.group.is_none()
        && endpoint.row.is_none()
}

/// Check the host-frozen page pair against the primary control's exact trusted
/// registration, then locate both unique controls in the active foreground
/// window. This intentionally has no generic "other tab" fallback.
pub(crate) fn scoped_revisit_controls<'a>(
    primary: &SemanticTargetRef,
    pair: &ScopedUiRevisit,
    pack: &AppPack,
    env: &'a EnvSnapshot,
) -> Option<ScopedRevisitControls<'a>> {
    pair.validate().ok()?;
    let primary_control = pack.control(primary)?;
    if !(matches!(
        primary_control.role.as_str(),
        "AXCheckBox" | "AXTextField" | "AXTextArea" | "AXPopUpButton"
    ) || primary_control.role == "AXRadioGroup"
        && pack.scoped_revisit_pages(primary_control).is_some())
    {
        return None;
    }
    let (home_control, away_control) = pack.scoped_revisit_pages(primary_control)?;
    if !endpoint_matches_control(&pair.home, home_control)
        || !endpoint_matches_control(&pair.away, away_control)
        || env.snapshot.app.bundle_id != pack.bundle_id
        || !env.snapshot.complete
        || env.snapshot.validate().is_err()
        || env.snapshot.menu_context.is_some()
        || env.snapshot.observer_coverage.state == SourceState::Unavailable
        || env.snapshot.windows.iter().any(|window| window.modal)
    {
        return None;
    }
    let active = focused_window_id(&env.snapshot)?;
    let active_window = env
        .snapshot
        .windows
        .iter()
        .find(|window| window.window_id == active)?;
    if active_window.kind != LayerKind::Window
        || active_window.modal
        || active_window.owner_bundle_id != pack.bundle_id
    {
        return None;
    }

    let matching = |endpoint: &TargetSpec, control: &crate::environment::pack::ControlDef| {
        let wanted = normalize_label(&endpoint.label);
        let objects: Vec<_> = env
            .snapshot
            .objects
            .iter()
            .filter(|object| {
                object.window_id == active
                    && object.role == control.role
                    && normalize_label(label_of(object)) == wanted
                    && object.enabled
                    && object.in_view
                    && !object.secure
                    && object.scroll.is_none()
                    && object.actions.contains(&AxActionName::Press)
                    && pack.navigation_page(object).is_some()
            })
            .collect();
        (objects.len() == 1).then(|| objects[0])
    };
    let home = matching(&pair.home, home_control)?;
    let away = matching(&pair.away, away_control)?;
    if home.instance_ref == away.instance_ref {
        return None;
    }
    let home_page = pack.navigation_page(home)?;
    let away_page = pack.navigation_page(away)?;
    if home_page == away_page
        || pack.page_of(&env.snapshot).as_deref() != env.page.as_deref()
        || !matches!(home.value.as_ref(), Some(UiValue::Bool(_)))
        || !matches!(away.value.as_ref(), Some(UiValue::Bool(_)))
    {
        return None;
    }
    Some(ScopedRevisitControls {
        home,
        away,
        home_page,
        away_page,
    })
}

/// Host-derived configuration for the registered radio-group value. The
/// public target remains the AXRadioGroup; only the exact desired child is
/// bound for input.
#[derive(Debug, Clone)]
pub(crate) struct ScopedRadioChoiceSpec {
    pub(crate) bundle_id: String,
    pub(crate) group_label: String,
    pub(crate) form_label: String,
    pub(crate) choices: &'static [&'static str],
    pub(crate) revisit: ScopedUiRevisit,
}

#[derive(Clone, Copy)]
pub(crate) struct ScopedRadioControl<'a> {
    pub(crate) group: &'a UiObject,
    pub(crate) desired: &'a UiObject,
    pub(crate) selected_label: &'static str,
    pub(crate) home_page: &'a str,
}

/// Return a runtime radio specification only for the exact frozen Host
/// registration and Text goal. Popup `Choice` remains a separate menu flow.
pub(crate) fn scoped_radio_choice_spec(
    goal: &GoalSpec,
    pack: &AppPack,
) -> Option<ScopedRadioChoiceSpec> {
    if goal.validate().is_err()
        || goal.execution_domain != ExecutionDomain::ForegroundNative
        || goal.confirmation_scope
            != interaction_contracts::desktop::task::ConfirmationScope::UiRevisit
        || goal.scoped_commit.is_some()
        || goal.scope.app != pack.bundle_id
    {
        return None;
    }
    let allowed_scope = goal.allowed_scope.as_ref()?;
    let [allowed_target] = allowed_scope.targets.as_slice() else {
        return None;
    };
    if allowed_scope.validate().is_err()
        || (allowed_scope.application != pack.name && allowed_scope.application != pack.bundle_id)
        || allowed_target.group.is_some()
    {
        return None;
    }
    let scoped = pack.scoped_control(allowed_target)?;
    let ScopedControlValueKind::RadioChoice(choices) = scoped.value_kind else {
        return None;
    };
    let context = scoped.context_label?;
    let ExpectedValue::Text { value } = &goal.desired else {
        return None;
    };
    if choices.len() != 3
        || choices
            .iter()
            .enumerate()
            .any(|(index, choice)| choices[index + 1..].contains(choice))
        || scoped.context_kind != ScopedControlContextKind::Form
        || scoped.control.role != "AXRadioGroup"
        || allowed_target.form.as_deref() != Some(context)
        || goal.target.semantic_target.as_str() != scoped.control.semantic
        || goal.target.label != scoped.control.label
        || goal.target.role_hint.as_deref() != Some("AXRadioGroup")
        || goal.target.group.as_deref() != Some(context)
        || goal.target.row.is_some()
    {
        return None;
    }
    if !choices.contains(&value.as_str()) {
        return None;
    }
    let revisit = goal.scoped_revisit.as_ref()?.clone();
    let (home, away) = pack.scoped_revisit_pages(scoped.control)?;
    if !endpoint_matches_control(&revisit.home, home)
        || !endpoint_matches_control(&revisit.away, away)
    {
        return None;
    }
    Some(ScopedRadioChoiceSpec {
        bundle_id: pack.bundle_id.clone(),
        group_label: scoped.control.label.clone(),
        form_label: context.to_owned(),
        choices,
        revisit,
    })
}

/// Resolve the complete radio-group shape from one normal, complete Source
/// read. The AXRadioGroup is passive: its disabled/unavailable AXEnabled,
/// value, and actions are never used as an input receiver.
fn scoped_radio_control_from_source<'a>(
    pack: &AppPack,
    env: &'a EnvSnapshot,
    spec: &ScopedRadioChoiceSpec,
    desired_label: &str,
) -> Option<ScopedRadioControl<'a>> {
    if pack.bundle_id != spec.bundle_id
        || env.snapshot.app.bundle_id != spec.bundle_id
        || !env.snapshot.complete
        || env.snapshot.validate().is_err()
        || env.snapshot.menu_context.is_some()
        || env.snapshot.observer_coverage.state == SourceState::Unavailable
        || env.snapshot.windows.iter().any(|window| window.modal)
        || !spec.choices.contains(&desired_label)
    {
        return None;
    }
    let active = focused_window_id(&env.snapshot)?;
    let active_window = env
        .snapshot
        .windows
        .iter()
        .find(|window| window.window_id == active)?;
    if active_window.kind != LayerKind::Window
        || active_window.modal
        || active_window.owner_bundle_id != spec.bundle_id
    {
        return None;
    }
    let labelled = |object: &UiObject, label: &str| {
        object.title.as_deref().or(object.description.as_deref()) == Some(label)
    };
    let contexts: Vec<_> = env
        .snapshot
        .objects
        .iter()
        .filter(|object| {
            object.window_id == active
                && object.role == "AXGroup"
                && labelled(object, &spec.form_label)
                && object.in_view
                && !object.secure
                && object.scroll.is_none()
        })
        .collect();
    let [context] = contexts.as_slice() else {
        return None;
    };
    let groups: Vec<_> = env
        .snapshot
        .objects
        .iter()
        .filter(|object| {
            object.window_id == active
                && object.role == "AXRadioGroup"
                && labelled(object, &spec.group_label)
                && object.parent.as_ref() == Some(&context.instance_ref)
                && object.group.as_deref() == Some(spec.form_label.as_str())
                && object.in_view
                && !object.secure
                && object.scroll.is_none()
                && object.value.is_none()
                && object.actions.is_empty()
        })
        .collect();
    let [group] = groups.as_slice() else {
        return None;
    };
    let group = *group;
    let children: Vec<_> = env
        .snapshot
        .objects
        .iter()
        .filter(|object| {
            object.window_id == active && object.parent.as_ref() == Some(&group.instance_ref)
        })
        .collect();
    if children.len() != spec.choices.len() {
        return None;
    }
    let mut selected_label = None;
    let mut desired = None;
    for choice in spec.choices {
        let matching: Vec<_> = children
            .iter()
            .copied()
            .filter(|object| {
                object.role == "AXRadioButton"
                    && labelled(object, choice)
                    && object.parent.as_ref() == Some(&group.instance_ref)
                    && object.group.as_deref() == Some(spec.group_label.as_str())
                    && object.enabled
                    && object.in_view
                    && !object.secure
                    && object.scroll.is_none()
                    && object.actions.contains(&AxActionName::Press)
            })
            .collect();
        let [child] = matching.as_slice() else {
            return None;
        };
        let child = *child;
        let Some(UiValue::Bool(is_selected)) = child.value.as_ref() else {
            return None;
        };
        if *is_selected && selected_label.replace(*choice).is_some() {
            return None;
        }
        if *choice == desired_label {
            desired = Some(child);
        }
    }
    Some(ScopedRadioControl {
        group,
        desired: desired?,
        selected_label: selected_label?,
        home_page: env.page.as_deref()?,
    })
}

/// Resolve the group and exact desired radio on the frozen home page.
pub(crate) fn scoped_radio_choice_control<'a>(
    goal: &GoalSpec,
    pack: &AppPack,
    env: &'a EnvSnapshot,
) -> Option<ScopedRadioControl<'a>> {
    let spec = scoped_radio_choice_spec(goal, pack)?;
    let controls = scoped_revisit_controls(&goal.target.semantic_target, &spec.revisit, pack, env)?;
    if env.page.as_deref() != Some(controls.home_page.as_str()) {
        return None;
    }
    let desired = match &goal.desired {
        ExpectedValue::Text { value } => value.as_str(),
        _ => return None,
    };
    let radio = scoped_radio_control_from_source(pack, env, &spec, desired)?;
    (radio.home_page == controls.home_page).then_some(radio)
}

/// Validate that a binding's physical receiver is exactly the desired child,
/// while its semantic target remains the frozen group target.
fn scoped_radio_binding_object<'a>(
    target: &SemanticTargetRef,
    env: &'a EnvSnapshot,
    radio: &ScopedRadioControl<'a>,
    binding: &Binding,
    require_current_snapshot: bool,
) -> Option<&'a UiObject> {
    if binding.semantic_target != *target
        || binding.instance != radio.desired.instance_ref
        || radio.desired.parent.as_ref() != Some(&radio.group.instance_ref)
        || binding.rule != "instance"
        || binding.page.as_deref() != Some(radio.home_page)
        || binding.identity_boundary != env.snapshot.identity_boundary
        || binding.continuity.scope != BindingProofScope::WholeStructure
        || binding.continuity.source_sequence > env.snapshot.source_sequence
        || binding.continuity.validate().is_err()
        || !binding.device_context.supports_native_input()
        || instance_fingerprint(&env.snapshot, radio.desired).digest() != binding.fingerprint_digest
    {
        return None;
    }
    if require_current_snapshot
        && (binding.snapshot_id != env.snapshot.snapshot_id
            || binding.observed_ms != env.snapshot.observed_ms
            || binding.env_revision != env.revision
            || binding.layer_revision != env.layer_revision
            || binding.continuity.source_sequence != env.snapshot.source_sequence)
    {
        return None;
    }
    Some(radio.desired)
}

/// Read the parent Text value from the one currently selected registered
/// child. The child binding is identity evidence only; its Bool is not exposed
/// as the task value.
pub(crate) fn scoped_radio_value_from_binding(
    goal: &GoalSpec,
    pack: &AppPack,
    env: &EnvSnapshot,
    binding: &Binding,
) -> Option<UiValue> {
    let radio = scoped_radio_choice_control(goal, pack, env)?;
    scoped_radio_binding_object(&goal.target.semantic_target, env, &radio, binding, false)?;
    Some(UiValue::Text(radio.selected_label.to_owned()))
}

/// The one typed Text-backed numeric control currently admitted for native
/// form input. It keeps the public Goal Number while the exact AX field uses a
/// canonical decimal Text representation.
pub(crate) fn scoped_form_number_control<'a>(
    goal: &GoalSpec,
    pack: &'a AppPack,
) -> Option<&'a ControlDef> {
    if goal.validate().is_err()
        || goal.execution_domain != ExecutionDomain::ForegroundNative
        || goal.scope.app != pack.bundle_id
        || goal.confirmation_scope
            != interaction_contracts::desktop::task::ConfirmationScope::UiRevisit
        || goal.scoped_revisit.is_none()
        || goal.target.row.is_some()
    {
        return None;
    }
    let ExpectedValue::Number { value } = &goal.desired else {
        return None;
    };
    if !(-MAX_SCOPED_FORM_SAFE_INTEGER..=MAX_SCOPED_FORM_SAFE_INTEGER).contains(value) {
        return None;
    }
    let scope = goal.allowed_scope.as_ref()?;
    if scope.validate().is_err() || scope.targets.len() != 1 {
        return None;
    }
    let allowed = scope.targets.first()?;
    let scoped = pack.scoped_control(allowed)?;
    if scoped.context_kind != ScopedControlContextKind::Form
        || scoped.value_kind != ScopedControlValueKind::Number
        || scoped.control.role != "AXTextField"
        || allowed.context_label() != scoped.context_label
        || goal.target.semantic_target.as_str() != scoped.control.semantic
        || goal.target.label != scoped.control.label
        || goal.target.role_hint.as_deref() != Some(scoped.control.role.as_str())
        || goal.target.group.as_deref() != scoped.context_label
        || pack.scoped_form_scroll_container(allowed).is_none()
        || pack.scoped_revisit_pages(scoped.control).is_none()
    {
        return None;
    }
    Some(scoped.control)
}

fn scoped_form_number_scroll_relation(
    goal: &GoalSpec,
    pack: &AppPack,
    env: &EnvSnapshot,
    field: &UiObject,
) -> bool {
    let Some(control) = scoped_form_number_control(goal, pack) else {
        return false;
    };
    let Some(allowed) = goal
        .allowed_scope
        .as_ref()
        .and_then(|scope| scope.targets.first())
    else {
        return false;
    };
    let Some(container) = pack.scoped_form_scroll_container(allowed) else {
        return false;
    };
    let Some(active) = focused_window_id(&env.snapshot) else {
        return false;
    };
    let matches_container: Vec<_> = env
        .snapshot
        .objects
        .iter()
        .filter(|object| {
            object.window_id == active
                && object.role == container.role
                && normalize_label(label_of(object)) == normalize_label(&container.label)
                && object.enabled
                && object.in_view
                && !object.secure
                && object.scroll.as_ref().is_some_and(|scroll| scroll.valid())
        })
        .collect();
    let [container_object] = matches_container.as_slice() else {
        return false;
    };
    field.window_id == active
        && field.role == control.role
        && field.in_view
        && !field.secure
        && BindingReducer::in_container(env, &field.instance_ref, &container_object.instance_ref)
}

/// Read the frozen form Number from the exact currently visible bound AX field.
pub(crate) fn scoped_form_number_value_from_binding(
    goal: &GoalSpec,
    pack: &AppPack,
    env: &EnvSnapshot,
    binding: &Binding,
) -> Option<UiValue> {
    scoped_form_number_control(goal, pack)?;
    let object = scoped_revisit_primary_binding_object(goal, pack, env, binding)?;
    scoped_number_text_value(object.value.as_ref()?)
}

/// Revalidate the scoped primary control from the current Source read before
/// an ensure/read action. Form authority is resolved by the trusted pack; the
/// public `TargetSpec.group` carries the registered form label so the normal
/// binder and this final check use the same context.
pub(crate) fn scoped_revisit_primary_binding_object<'a>(
    goal: &GoalSpec,
    pack: &AppPack,
    env: &'a EnvSnapshot,
    binding: &Binding,
) -> Option<&'a UiObject> {
    let manual_commit = goal.scoped_commit.as_ref().is_some_and(|commit| {
        commit.receipt.kind == ScopedCommitKind::Manual && commit.action.is_some()
    });
    if goal.validate().is_err()
        || goal.execution_domain != ExecutionDomain::ForegroundNative
        || !(goal.confirmation_scope
            == interaction_contracts::desktop::task::ConfirmationScope::UiRevisit
            && goal.scoped_revisit.is_some()
            || goal.confirmation_scope
                == interaction_contracts::desktop::task::ConfirmationScope::Commit
                && manual_commit)
        || env.snapshot.app.bundle_id != pack.bundle_id
        || !env.snapshot.complete
        || env.snapshot.validate().is_err()
        || env.snapshot.menu_context.is_some()
        || env.snapshot.observer_coverage.state == SourceState::Unavailable
        || env.snapshot.windows.iter().any(|window| window.modal)
    {
        return None;
    }
    let allowed_scope = goal.allowed_scope.as_ref()?;
    if goal.scope.app != pack.bundle_id
        || (allowed_scope.application != pack.name && allowed_scope.application != pack.bundle_id)
    {
        return None;
    }
    let allowed_target = allowed_scope.targets.first()?;
    if allowed_scope.targets.len() != 1 || allowed_scope.validate().is_err() {
        return None;
    }
    let scoped = pack.scoped_control(allowed_target)?;
    let target_context_matches = matches!(
        (
            allowed_target.group.as_deref(),
            allowed_target.form.as_deref(),
            scoped.context_kind,
        ),
        (Some(_), None, ScopedControlContextKind::Group)
            | (None, Some(_), ScopedControlContextKind::Form)
            | (None, None, ScopedControlContextKind::Unspecified)
    );
    let value_kind_matches = match (&goal.desired, scoped.value_kind) {
        (ExpectedValue::Bool { .. }, ScopedControlValueKind::Bool) => {
            scoped.control.role == "AXCheckBox"
        }
        (ExpectedValue::Text { value }, ScopedControlValueKind::Text) => {
            let text_is_bounded_and_plain = value.len() <= 2_048
                && value.chars().all(|character| {
                    !character.is_control() || matches!(character, '\n' | '\r' | '\t')
                });
            let field_has_no_line_separators = scoped.control.role != "AXTextField"
                || !value
                    .chars()
                    .any(|character| matches!(character, '\n' | '\r' | '\u{2028}' | '\u{2029}'));
            matches!(scoped.control.role.as_str(), "AXTextField" | "AXTextArea")
                && text_is_bounded_and_plain
                && field_has_no_line_separators
        }
        (ExpectedValue::Text { value }, ScopedControlValueKind::Choice(choices)) => {
            scoped.control.role == "AXPopUpButton" && choices.contains(&value.as_str())
        }
        (ExpectedValue::Text { value }, ScopedControlValueKind::RadioChoice(choices)) => {
            scoped.control.role == "AXRadioGroup" && choices.contains(&value.as_str())
        }
        (ExpectedValue::Number { value }, ScopedControlValueKind::Number) => {
            scoped_form_number_control(goal, pack).is_some()
                && (-MAX_SCOPED_FORM_SAFE_INTEGER..=MAX_SCOPED_FORM_SAFE_INTEGER).contains(value)
        }
        _ => false,
    };
    if !target_context_matches
        || !value_kind_matches
        || allowed_target.context_label() != scoped.context_label
        || goal.target.semantic_target.as_str() != scoped.control.semantic
        || goal.target.label != scoped.control.label
        || goal.target.role_hint.as_deref() != Some(scoped.control.role.as_str())
        || goal.target.group.as_deref() != scoped.context_label
        || goal.target.row.is_some()
    {
        return None;
    }
    let expected_page = if let Some(pair) = goal.scoped_revisit.as_ref() {
        let controls = scoped_revisit_controls(&goal.target.semantic_target, pair, pack, env)?;
        controls.home_page
    } else {
        let commit = goal.scoped_commit.as_ref()?;
        if commit.receipt.kind != ScopedCommitKind::Manual
            || pack.scoped_commit_binding(scoped.control).as_ref() != Some(commit)
        {
            return None;
        }
        env.page.clone()?
    };
    if env.page.as_deref() != Some(expected_page.as_str()) {
        return None;
    }
    if matches!(scoped.value_kind, ScopedControlValueKind::RadioChoice(_)) {
        let radio = scoped_radio_choice_control(goal, pack, env)?;
        return scoped_radio_binding_object(
            &goal.target.semantic_target,
            env,
            &radio,
            binding,
            true,
        );
    }
    let active = focused_window_id(&env.snapshot)?;
    let wanted_label = normalize_label(&scoped.control.label);
    let wanted_context = scoped.context_label.map(normalize_label);
    let matches_target = |object: &&UiObject| {
        object.window_id == active
            && object.role == scoped.control.role
            && normalize_label(label_of(object)) == wanted_label
            && object.group.as_deref().map(normalize_label) == wanted_context
            && object.enabled
            && object.in_view
            && !object.secure
            && object.scroll.is_none()
    };
    let matching: Vec<_> = env.snapshot.objects.iter().filter(matches_target).collect();
    let [object] = matching.as_slice() else {
        return None;
    };
    let value_kind_in_source = match (scoped.value_kind, object.value.as_ref()) {
        (ScopedControlValueKind::Bool, Some(UiValue::Bool(_)))
        | (ScopedControlValueKind::Text, Some(UiValue::Text(_))) => true,
        (ScopedControlValueKind::Choice(choices), Some(UiValue::Text(value))) => {
            choices.contains(&value.as_str())
                && matches!(
                    &goal.desired,
                    ExpectedValue::Text { value: desired } if choices.contains(&desired.as_str())
                )
        }
        (ScopedControlValueKind::Number, Some(UiValue::Text(value))) => {
            scoped_form_number_control(goal, pack).is_some()
                && scoped_number_text_i64(value).is_some()
                && scoped_form_number_scroll_relation(goal, pack, env, object)
        }
        (ScopedControlValueKind::RadioChoice(_), _) => false,
        (ScopedControlValueKind::Bool, _)
        | (ScopedControlValueKind::Text, _)
        | (ScopedControlValueKind::Choice(_), _) => false,
        (ScopedControlValueKind::Number, _) => false,
    };
    let has_required_input = match scoped.value_kind {
        ScopedControlValueKind::Text => object.actions.contains(&AxActionName::SetValue),
        ScopedControlValueKind::Bool => {
            object.actions.contains(&AxActionName::SetValue)
                || object.actions.contains(&AxActionName::Press)
        }
        ScopedControlValueKind::Choice(_) => {
            object.role == "AXPopUpButton" && object.actions.contains(&AxActionName::ShowMenu)
        }
        ScopedControlValueKind::Number => {
            object.role == "AXTextField" && object.actions.contains(&AxActionName::SetValue)
        }
        ScopedControlValueKind::RadioChoice(_) => false,
    };
    if !value_kind_in_source
        || !has_required_input
        || binding.semantic_target != goal.target.semantic_target
        || binding.instance != object.instance_ref
        || binding.snapshot_id != env.snapshot.snapshot_id
        || binding.observed_ms != env.snapshot.observed_ms
        || binding.env_revision != env.revision
        || binding.layer_revision != env.layer_revision
        || binding.page.as_deref() != Some(expected_page.as_str())
        || binding.identity_boundary != env.snapshot.identity_boundary
        || binding.continuity.scope != BindingProofScope::WholeStructure
        || binding.continuity.source_sequence != env.snapshot.source_sequence
        || binding.continuity.validate().is_err()
        || !binding.device_context.supports_native_input()
        || instance_fingerprint(&env.snapshot, object).digest() != binding.fingerprint_digest
    {
        return None;
    }
    Some(object)
}

/// Revalidate the exact Host-frozen Save target and the primary Boolean value
/// together from one current Source snapshot before each manual Commit input.
pub(crate) fn scoped_commit_action_binding_object<'a>(
    goal: &GoalSpec,
    pack: &AppPack,
    env: &'a EnvSnapshot,
    binding: &Binding,
) -> Option<&'a UiObject> {
    let commit = goal.scoped_commit.as_ref()?;
    let action = commit.action.as_ref()?;
    if goal.validate().is_err()
        || commit.receipt.kind != ScopedCommitKind::Manual
        || goal.confirmation_scope
            != interaction_contracts::desktop::task::ConfirmationScope::Commit
        || goal.execution_domain != ExecutionDomain::ForegroundNative
        || goal.scope.app != pack.bundle_id
        || pack.bundle_id != env.snapshot.app.bundle_id
        || !env.snapshot.complete
        || env.snapshot.validate().is_err()
        || env.snapshot.menu_context.is_some()
        || env.snapshot.observer_coverage.state == SourceState::Unavailable
        || env.snapshot.windows.iter().any(|window| window.modal)
    {
        return None;
    }
    let allowed = goal.allowed_scope.as_ref()?;
    if allowed.targets.len() != 1 || allowed.validate().is_err() {
        return None;
    }
    let primary = pack.scoped_control(&allowed.targets[0])?;
    if primary.value_kind != ScopedControlValueKind::Bool
        || primary.control.role != "AXCheckBox"
        || pack.scoped_commit_binding(primary.control).as_ref() != Some(commit)
    {
        return None;
    }
    let active = focused_window_id(&env.snapshot)?;
    let wanted_primary_label = normalize_label(&primary.control.label);
    let wanted_primary_group = primary.context_label.map(normalize_label);
    let primary_matches: Vec<_> = env
        .snapshot
        .objects
        .iter()
        .filter(|object| {
            object.window_id == active
                && object.role == primary.control.role
                && normalize_label(label_of(object)) == wanted_primary_label
                && object.group.as_deref().map(normalize_label) == wanted_primary_group
                && object.enabled
                && object.in_view
                && !object.secure
                && object.scroll.is_none()
        })
        .collect();
    let [primary_object] = primary_matches.as_slice() else {
        return None;
    };
    if !matches!(primary_object.value.as_ref(), Some(UiValue::Bool(true))) {
        return None;
    }
    let wanted_action_group = action.group.as_deref()?;
    let action_matches: Vec<_> = env
        .snapshot
        .objects
        .iter()
        .filter(|object| {
            object.window_id == active
                && object.role == "AXButton"
                && label_of(object) == action.label
                && object.group.as_deref() == Some(wanted_action_group)
                && object.enabled
                && object.in_view
                && !object.secure
                && object.scroll.is_none()
                && object.actions.contains(&AxActionName::Press)
        })
        .collect();
    let [object] = action_matches.as_slice() else {
        return None;
    };
    if pack.effect_class(&object.role, &action.label) != EffectClass::Commit
        || binding.semantic_target != action.semantic_target
        || binding.rule != "unique"
        || binding.instance != object.instance_ref
        || binding.snapshot_id != env.snapshot.snapshot_id
        || binding.observed_ms != env.snapshot.observed_ms
        || binding.env_revision != env.revision
        || binding.layer_revision != env.layer_revision
        || binding.page.as_deref() != env.page.as_deref()
        || binding.identity_boundary != env.snapshot.identity_boundary
        || binding.continuity.scope != BindingProofScope::WholeStructure
        || binding.continuity.source_sequence != env.snapshot.source_sequence
        || binding.continuity.validate().is_err()
        || !binding.device_context.supports_native_input()
        || instance_fingerprint(&env.snapshot, object).digest() != binding.fingerprint_digest
    {
        return None;
    }
    Some(object)
}

/// Revalidate a scoped navigation binding against the current exact page pair.
/// A later Source snapshot, replacement binding, second matching radio, or a
/// radio selected as the current page makes the proof unusable.
pub(crate) fn scoped_revisit_binding_object<'a>(
    primary: &SemanticTargetRef,
    pair: &ScopedUiRevisit,
    pack: &AppPack,
    env: &'a EnvSnapshot,
    binding: &Binding,
    target: &TargetSpec,
) -> Option<&'a UiObject> {
    let controls = scoped_revisit_controls(primary, pair, pack, env)?;
    let (object, expected_current_page) = if target == &pair.away {
        (controls.away, controls.home_page.as_str())
    } else if target == &pair.home {
        (controls.home, controls.away_page.as_str())
    } else {
        return None;
    };
    if env.page.as_deref() != Some(expected_current_page)
        || binding.semantic_target != target.semantic_target
        || binding.instance != object.instance_ref
        || binding.rule != "instance"
        || binding.snapshot_id != env.snapshot.snapshot_id
        || binding.observed_ms != env.snapshot.observed_ms
        || binding.env_revision != env.revision
        || binding.layer_revision != env.layer_revision
        || binding.page.as_deref() != Some(expected_current_page)
        || binding.identity_boundary != env.snapshot.identity_boundary
        || binding.continuity.scope != BindingProofScope::WholeStructure
        || binding.continuity.source_sequence != env.snapshot.source_sequence
        || binding.continuity.validate().is_err()
        || !binding.device_context.supports_native_input()
        || instance_fingerprint(&env.snapshot, object).digest() != binding.fingerprint_digest
        || !matches!(object.value.as_ref(), Some(UiValue::Bool(false)))
        || pack.effect_class(&object.role, label_of(object)) != EffectClass::Navigation
    {
        return None;
    }
    Some(object)
}

impl SkillRun {
    pub fn new(
        skill: Skill,
        target: SemanticTargetRef,
        desired: Option<ExpectedValue>,
        binding: BindingRef,
    ) -> Self {
        Self {
            execution_domain: ExecutionDomain::ForegroundNative,
            activation: None,
            input_floor: None,
            skill,
            target,
            desired,
            binding,
            phase: Phase::Start,
            actions: 0,
            home_page: None,
            scoped_revisit: None,
            scoped_form_text: false,
            scoped_form_focus_source_sequence: None,
            scoped_radio_choice: None,
            radio_input_source_sequence: None,
            last_scoped_source_sequence: None,
            last_path: None,
            scroll: None,
            scroll_pointer_preparation: false,
            menu: None,
            keyboard_context: None,
            row_target: None,
            row_input_sequence: None,
            window_root_source_sequence: None,
            background_input_source_sequence: None,
            background_target_context_hash: None,
            background_input_binding: None,
            browser_page_profile: None,
            browser_original_binding: None,
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
        self.input_floor = match outcome {
            ActionOutcome::Completed { end_ms } => Some(end_ms),
            ActionOutcome::Unknown { at_ms } => Some(at_ms),
            _ => self.input_floor,
        };
        let Phase::AwaitingOutcome(follow) = self.phase else {
            return;
        };
        if self.skill == Skill::SelectRow {
            self.phase = match outcome {
                ActionOutcome::Completed { end_ms } => Phase::Judging(
                    follow,
                    Known {
                        end_ms,
                        unknown: false,
                    },
                ),
                ActionOutcome::Unknown { at_ms } => Phase::Ended(SkillResult::Failed {
                    code: FailureCode::EffectUnknown,
                    reason: format!(
                        "row selection outcome is unknown at {at_ms}; the action will not be replayed"
                    ),
                }),
                ActionOutcome::Rejected(code) => Phase::Ended(SkillResult::Failed {
                    code: code.unwrap_or(FailureCode::InputRejected),
                    reason: "the device rejected the row selection input".into(),
                }),
                ActionOutcome::Cancelled => Phase::Ended(SkillResult::Failed {
                    code: FailureCode::Cancelled,
                    reason: "the row selection input was cancelled".into(),
                }),
            };
            return;
        }
        self.phase = match outcome {
            ActionOutcome::Completed { end_ms } => Phase::Judging(
                follow,
                Known {
                    end_ms,
                    unknown: false,
                },
            ),
            ActionOutcome::Unknown { .. } if self.last_path == Some(ExecutionPath::Keyboard) => Phase::Ended(SkillResult::Failed {
                code: FailureCode::EffectUnknown,
                reason: "the keyboard input outcome is unknown; a matching value cannot confirm the input".into(),
            }),
            ActionOutcome::Unknown { at_ms } if self.skill == Skill::Revisit => {
                Phase::Ended(SkillResult::Failed {
                    code: FailureCode::EffectUnknown,
                    reason: format!(
                        "revisit navigation outcome is unknown at {at_ms}; no further navigation will be sent"
                    ),
                })
            }
            ActionOutcome::Unknown { at_ms } if follow == Follow::FocusText => {
                Phase::Ended(SkillResult::Failed {
                    code: FailureCode::EffectUnknown,
                    reason: format!(
                        "the form-field focus click outcome is unknown at {at_ms}; it will not be replayed"
                    ),
                })
            }
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
        if self.execution_domain == ExecutionDomain::HeadlessBrowser {
            let step = self.advance_headless_browser(view);
            if let SkillStep::Done(result) = &step {
                self.phase = Phase::Ended(result.clone());
            }
            return step;
        }
        if self.execution_domain == ExecutionDomain::BackgroundAx {
            let step = self.advance_background_ax(view);
            if let SkillStep::Done(result) = &step {
                self.phase = Phase::Ended(result.clone());
            }
            return step;
        }
        if self.scoped_radio_choice.is_some()
            && matches!(self.skill, Skill::ReadValue | Skill::SelectByLabel)
        {
            let step = self.advance_scoped_radio_choice(view);
            if let SkillStep::Done(result) = &step {
                self.phase = Phase::Ended(result.clone());
            }
            return step;
        }
        if self.skill == Skill::SelectRow {
            return self.advance_row_selection(view);
        }
        if self.skill == Skill::SelectByLabel {
            return self.advance_menu(view);
        }
        if self.skill == Skill::ScrollTo {
            return self.advance_scroll(view);
        }
        if self.skill == Skill::ActivateWindow {
            return self.advance_window(view);
        }
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

    fn advance_scoped_radio_choice(&mut self, view: &mut SkillView<'_>) -> SkillStep {
        let Some(spec) = self.scoped_radio_choice.clone() else {
            return fail(
                FailureCode::ContextUnresolved,
                "the registered radio-group specification is missing",
            );
        };
        let desired = match self.desired.as_ref() {
            Some(ExpectedValue::Text { value }) if spec.choices.contains(&value.as_str()) => {
                value.clone()
            }
            _ => {
                return fail(
                    FailureCode::ContextUnresolved,
                    "the radio-group step has no registered Text choice",
                );
            }
        };
        match self.phase.clone() {
            Phase::Ended(result) => SkillStep::Done(result),
            Phase::AwaitingOutcome(_) => fail(
                FailureCode::ContextUnresolved,
                "the radio-group input outcome is still unresolved",
            ),
            Phase::Start => {
                if !view.state.usable(view.now_ms, view.max_age_ms) {
                    return fail(
                        FailureCode::SourceInvalid,
                        "radio-group selection requires healthy fresh Source evidence",
                    );
                }
                let env = match evidence(view.state, None) {
                    Ok(env) if env.snapshot.observed_ms <= view.now_ms => env,
                    Ok(_) => {
                        return fail(
                            FailureCode::SourceInvalid,
                            "the radio-group Source timestamp is in the future",
                        );
                    }
                    Err(step) => return step,
                };
                let controls =
                    match scoped_revisit_controls(&self.target, &spec.revisit, view.pack, env) {
                        Some(controls)
                            if env.page.as_deref() == Some(controls.home_page.as_str()) =>
                        {
                            controls
                        }
                        _ => {
                            return fail(
                                FailureCode::ContextUnresolved,
                                "the frozen radio-group page pair or home page changed",
                            );
                        }
                    };
                let Some(radio) = scoped_radio_control_from_source(view.pack, env, &spec, &desired)
                else {
                    return fail(
                        FailureCode::ContextUnresolved,
                        "the exact radio group or its unique child values are unavailable",
                    );
                };
                if radio.home_page != controls.home_page {
                    return fail(
                        FailureCode::ContextUnresolved,
                        "the radio group is not on its frozen home page",
                    );
                }
                let binding = match self.bound(view) {
                    Ok(binding) => binding,
                    Err(step) => return step,
                };
                if scoped_radio_binding_object(&self.target, env, &radio, &binding, true).is_none()
                {
                    return fail(
                        FailureCode::ContextUnresolved,
                        "the current binding is not the exact registered desired radio child",
                    );
                }
                if self.skill == Skill::ReadValue {
                    return SkillStep::Done(SkillResult::Succeeded);
                }
                if self.skill != Skill::SelectByLabel || self.menu.is_none() {
                    return fail(
                        FailureCode::EffectNotAuthorized,
                        "radio-group input requires the frozen select-by-label step",
                    );
                }
                if radio.selected_label == desired {
                    return SkillStep::Done(SkillResult::AlreadyDone);
                }
                self.radio_input_source_sequence = Some(env.snapshot.source_sequence);
                self.act(
                    binding,
                    Primitive::AxAction {
                        action: AxActionKind::Press,
                    },
                    ExecutionPath::Ax,
                    Follow::Verify,
                    format!(
                        "press the unique radio child \"{desired}\" in \"{}\"",
                        spec.group_label
                    ),
                )
            }
            Phase::Judging(Follow::Verify, known) if self.skill == Skill::SelectByLabel => {
                if known.unknown {
                    return fail(
                        FailureCode::EffectUnknown,
                        "the radio Press outcome is unknown and will not be replayed",
                    );
                }
                let after_ms = known.end_ms.saturating_add(1);
                let Some(env) = view.state.latest_since(after_ms) else {
                    return SkillStep::AwaitEvidence { after_ms };
                };
                if !view.state.usable(view.now_ms, view.max_age_ms)
                    || env.snapshot.observed_ms <= known.end_ms
                    || env.snapshot.observed_ms > view.now_ms
                    || env.snapshot.source_sequence
                        <= self.radio_input_source_sequence.unwrap_or(u64::MAX)
                {
                    return SkillStep::AwaitEvidence {
                        after_ms: after_ms.max(env.snapshot.observed_ms.saturating_add(1)),
                    };
                }
                let controls =
                    match scoped_revisit_controls(&self.target, &spec.revisit, view.pack, env) {
                        Some(controls)
                            if env.page.as_deref() == Some(controls.home_page.as_str()) =>
                        {
                            controls
                        }
                        _ => {
                            return fail(
                                FailureCode::EffectUnknown,
                                "fresh Source no longer proves the frozen radio group and page",
                            );
                        }
                    };
                let Some(radio) = scoped_radio_control_from_source(view.pack, env, &spec, &desired)
                else {
                    return fail(
                        FailureCode::EffectUnknown,
                        "fresh Source cannot prove one selected registered radio child",
                    );
                };
                if radio.home_page != controls.home_page || radio.selected_label != desired {
                    return fail(
                        FailureCode::EffectUnknown,
                        "fresh Source does not prove the requested radio choice",
                    );
                }
                let Some(binding) = view.bindings.bind_instance(
                    env,
                    self.target.clone(),
                    &radio.desired.instance_ref,
                ) else {
                    return fail(
                        FailureCode::EffectUnknown,
                        "fresh Source cannot bind the exact selected radio child",
                    );
                };
                if scoped_radio_binding_object(&self.target, env, &radio, &binding, true).is_none()
                {
                    return fail(
                        FailureCode::EffectUnknown,
                        "fresh Source binding does not prove the requested radio child",
                    );
                }
                self.binding = binding.binding_ref.clone();
                SkillStep::Done(SkillResult::Succeeded)
            }
            Phase::Judging(_, known) if known.unknown => fail(
                FailureCode::EffectUnknown,
                "the radio-group input outcome is unknown and will not be replayed",
            ),
            Phase::Judging(_, _) => fail(
                FailureCode::ContextUnresolved,
                "the radio-group skill entered an invalid phase",
            ),
        }
    }

    pub fn with_window(mut self, app: String, pid: u32, window: String) -> Self {
        self.activation = Some((app, pid, window));
        self
    }
    pub fn with_execution_domain(mut self, domain: ExecutionDomain) -> Self {
        self.execution_domain = domain;
        self
    }
    pub fn with_scoped_revisit(mut self, pair: ScopedUiRevisit) -> Self {
        self.scoped_revisit = Some(pair);
        self
    }
    /// Enabled only by Engine for a Host-frozen, registered Form text target.
    pub(crate) fn with_scoped_form_text(mut self) -> Self {
        self.scoped_form_text = true;
        self
    }
    pub(crate) fn scoped_form_text_focus_action_pending(&self) -> bool {
        self.scoped_form_text
            && self.skill == Skill::EnsureValue
            && matches!(self.phase, Phase::AwaitingOutcome(Follow::FocusText))
            && self.actions == 1
            && self.last_path == Some(ExecutionPath::Pointer)
    }
    pub(crate) fn scoped_form_text_set_value_action_pending(&self) -> bool {
        self.scoped_form_text
            && self.skill == Skill::EnsureValue
            && matches!(self.phase, Phase::AwaitingOutcome(Follow::Verify))
            && self.last_path == Some(ExecutionPath::Ax)
            && match self.actions {
                1 => self.scoped_form_focus_source_sequence.is_none(),
                2 => self.scoped_form_focus_source_sequence.is_some(),
                _ => false,
            }
    }
    pub(crate) fn scoped_form_focus_source_sequence(&self) -> Option<u64> {
        self.scoped_form_focus_source_sequence
    }
    pub(crate) fn scoped_revisit_pair(&self) -> Option<&ScopedUiRevisit> {
        self.scoped_revisit.as_ref()
    }
    /// The only semantic page target that the pending scoped Revisit input may
    /// address, derived from the skill's phase and action count.
    pub(crate) fn scoped_revisit_action_target(&self) -> Option<&TargetSpec> {
        if self.skill != Skill::Revisit {
            return None;
        }
        let pair = self.scoped_revisit.as_ref()?;
        match (&self.phase, self.actions) {
            (Phase::AwaitingOutcome(Follow::Away), 1) => Some(&pair.away),
            (Phase::AwaitingOutcome(Follow::Back), 2) => Some(&pair.home),
            _ => None,
        }
    }
    pub fn with_browser_page_profile(mut self, profile: BrowserPageProfile) -> Self {
        self.browser_page_profile = Some(profile);
        self
    }
    pub fn browser_original_binding(&self) -> Option<&BrowserCheckboxBinding> {
        self.browser_original_binding.as_ref()
    }
    pub fn with_scroll(mut self, spec: ScrollToSpec, destination: TargetSpec) -> Self {
        self.scroll = Some(scroll::ScrollRun::new(spec, destination));
        self
    }
    pub fn with_select(mut self, spec: SelectByLabelSpec) -> Self {
        self.menu = Some(menu::MenuRun::new(spec));
        self
    }
    pub(crate) fn with_scoped_radio_choice(mut self, spec: ScopedRadioChoiceSpec) -> Self {
        self.scoped_radio_choice = Some(spec);
        self
    }
    pub fn with_row_target(mut self, target: TargetSpec) -> Self {
        self.row_target = Some(target);
        self
    }
    pub fn keyboard_context(&self) -> Option<&FocusContext> {
        self.keyboard_context.as_ref()
    }

    pub fn update_keyboard_read_binding(&mut self, binding: &Binding) {
        if self.keyboard_context.is_some()
            && self.last_path == Some(ExecutionPath::Keyboard)
            && matches!(
                self.phase,
                Phase::Judging(Follow::Verify, Known { unknown: false, .. })
            )
            && binding.semantic_target == self.target
        {
            self.binding = binding.binding_ref.clone();
        }
    }

    /// Enabled by the trusted native host, rather than by planner parameters.
    pub fn with_scroll_pointer_preparation(mut self) -> Self {
        self.scroll_pointer_preparation = true;
        self
    }
    pub fn input_floor(&self) -> Option<u64> {
        self.input_floor
    }

    /// A known boolean input may relocate for verification. Only the first AX
    /// stage can then request a pointer path; the second stage can never issue
    /// a third input. The engine obtains a fresh binding and checks its effect
    /// gate. An unknown input never opens this transition.
    pub(crate) fn may_relocate_known_toggle(&self) -> bool {
        self.skill == Skill::EnsureValue
            && matches!(self.desired, Some(ExpectedValue::Bool { .. }))
            && matches!(
                (self.actions, self.last_path),
                (1, Some(ExecutionPath::Ax)) | (1 | 2, Some(ExecutionPath::Pointer))
            )
            && matches!(
                self.phase,
                Phase::Judging(Follow::Verify, Known { unknown: false, .. })
            )
    }

    pub(crate) fn relocate_known_toggle(&mut self, binding: &Binding) -> bool {
        if !self.may_relocate_known_toggle()
            || binding.semantic_target != self.target
            || binding.observed_ms < self.input_floor.unwrap_or(u64::MAX)
            || binding.binding_ref == self.binding
        {
            return false;
        }
        self.binding = binding.binding_ref.clone();
        true
    }

    fn advance_window(&mut self, view: &mut SkillView<'_>) -> SkillStep {
        let Some((app, pid, window)) = self.activation.clone() else {
            return fail(
                FailureCode::ContextUnresolved,
                "window activation has no trusted scope",
            );
        };
        let Some(env) = view.state.latest() else {
            return SkillStep::AwaitEvidence {
                after_ms: self.input_floor.unwrap_or(0),
            };
        };
        let step = match self.phase.clone() {
            Phase::Ended(result) => return SkillStep::Done(result),
            Phase::AwaitingOutcome(_) => fail(
                FailureCode::ContextUnresolved,
                "window activation outcome is unresolved",
            ),
            Phase::Start => {
                let binding = match self.bound(view) {
                    Ok(binding) if binding.rule == "window_activation" => binding,
                    _ => return fail(FailureCode::ContextUnresolved, "window binding changed"),
                };
                match binding.continuity.scope.clone() {
                    BindingProofScope::WindowRoot { table_scope } => {
                        if !matches!(
                            &binding.device_context,
                            DeviceContextWitness::WindowRootV1 { .. }
                        ) || !binding.device_context.supports_window_activation()
                        {
                            return fail(
                                FailureCode::ContextUnresolved,
                                "the scoped activation binding has no WindowRootV1 witness",
                            );
                        }
                        let Some(root) = view.state.row_window_root(
                            &table_scope,
                            &app,
                            pid,
                            &window,
                            view.now_ms,
                            view.max_age_ms.min(2_000),
                        ) else {
                            return SkillStep::AwaitEvidence {
                                after_ms: env.snapshot.observed_ms.saturating_add(1),
                            };
                        };
                        if root.instance_ref != binding.instance {
                            return fail(
                                FailureCode::ContextUnresolved,
                                "the scoped activation window root changed",
                            );
                        }
                        self.window_root_source_sequence = Some(binding.continuity.source_sequence);
                        self.phase = Phase::AwaitingOutcome(Follow::Verify);
                        self.actions += 1;
                        self.last_path = Some(ExecutionPath::Window);
                        SkillStep::Act(Box::new(ActionPlan {
                            binding,
                            primitive: Primitive::ActivateWindow { app },
                            path: ExecutionPath::Window,
                            note: "verify and activate the authorized window on Device".into(),
                        }))
                    }
                    BindingProofScope::WholeStructure => {
                        if focused_window_id(&env.snapshot) == Some(window.as_str()) {
                            SkillStep::Done(SkillResult::AlreadyDone)
                        } else if let Some(object) = view.state.window_root(
                            &app,
                            pid,
                            &window,
                            view.now_ms,
                            view.max_age_ms.min(2_000),
                        ) {
                            if binding.instance != object.instance_ref
                                || !matches!(
                                    &binding.device_context,
                                    DeviceContextWitness::WholeStructureV1 { .. }
                                )
                                || !binding.device_context.supports_window_activation()
                            {
                                return fail(
                                    FailureCode::ContextUnresolved,
                                    "the complete-tree activation binding changed",
                                );
                            }
                            self.phase = Phase::AwaitingOutcome(Follow::Verify);
                            self.actions += 1;
                            self.last_path = Some(ExecutionPath::Window);
                            SkillStep::Act(Box::new(ActionPlan {
                                binding,
                                primitive: Primitive::ActivateWindow { app },
                                path: ExecutionPath::Window,
                                note: "activate the authorized window".into(),
                            }))
                        } else {
                            SkillStep::AwaitEvidence {
                                after_ms: env.snapshot.observed_ms.saturating_add(1),
                            }
                        }
                    }
                    BindingProofScope::TableViewport { .. } => fail(
                        FailureCode::ContextUnresolved,
                        "row binding cannot activate a window",
                    ),
                    BindingProofScope::BackgroundAx { .. } => fail(
                        FailureCode::ContextUnresolved,
                        "a background AX binding cannot activate a window",
                    ),
                }
            }
            Phase::Judging(_, known) => {
                let binding = match self.bound(view) {
                    Ok(binding) if binding.rule == "window_activation" => binding,
                    _ => return fail(FailureCode::ContextUnresolved, "window binding changed"),
                };
                match binding.continuity.scope.clone() {
                    BindingProofScope::WindowRoot { table_scope } => {
                        if known.unknown {
                            return fail(
                                FailureCode::EffectUnknown,
                                "the window activation outcome is unknown; it will not be replayed",
                            );
                        }
                        let input_floor = self.input_floor.unwrap_or(known.end_ms);
                        let after_ms = known
                            .end_ms
                            .saturating_add(1)
                            .max(input_floor.saturating_add(1));
                        let Some(current) = view.state.latest_since(after_ms) else {
                            return SkillStep::AwaitEvidence { after_ms };
                        };
                        let source_floor = self
                            .window_root_source_sequence
                            .unwrap_or(binding.continuity.source_sequence);
                        if current.snapshot.observed_ms <= known.end_ms
                            || current.snapshot.observed_ms <= input_floor
                            || current.snapshot.source_sequence <= source_floor
                        {
                            return SkillStep::AwaitEvidence {
                                after_ms: current
                                    .snapshot
                                    .observed_ms
                                    .saturating_add(1)
                                    .max(after_ms),
                            };
                        }
                        let Some(root) = view.state.row_window_root(
                            &table_scope,
                            &app,
                            pid,
                            &window,
                            view.now_ms,
                            view.max_age_ms.min(2_000),
                        ) else {
                            return SkillStep::AwaitEvidence {
                                after_ms: current.snapshot.observed_ms.saturating_add(1),
                            };
                        };
                        if root.instance_ref != binding.instance {
                            fail(
                                FailureCode::ContextUnresolved,
                                "fresh source focus identifies a different window root",
                            )
                        } else {
                            SkillStep::Done(SkillResult::Succeeded)
                        }
                    }
                    BindingProofScope::WholeStructure => {
                        if env.snapshot.observed_ms >= known.end_ms
                            && crate::verification::window_active(
                                view.state,
                                &app,
                                pid,
                                &window,
                                env.snapshot.observed_ms,
                                known.end_ms,
                            )
                            .is_true()
                        {
                            SkillStep::Done(SkillResult::Succeeded)
                        } else {
                            SkillStep::AwaitEvidence {
                                after_ms: known.end_ms.max(env.snapshot.observed_ms + 1),
                            }
                        }
                    }
                    BindingProofScope::TableViewport { .. } => fail(
                        FailureCode::ContextUnresolved,
                        "row binding cannot activate a window",
                    ),
                    BindingProofScope::BackgroundAx { .. } => fail(
                        FailureCode::ContextUnresolved,
                        "a background AX binding cannot activate a window",
                    ),
                }
            }
        };
        if let SkillStep::Done(result) = &step {
            self.phase = Phase::Ended(result.clone());
        }
        step
    }

    fn advance_headless_browser(&mut self, view: &mut SkillView<'_>) -> SkillStep {
        if self.skill != Skill::EnsureValue {
            return fail(
                FailureCode::EffectNotAuthorized,
                "the browser fixture supports only its fixed checkbox ensure_value step",
            );
        }
        let Some(profile) = self.browser_page_profile else {
            return fail(
                FailureCode::ContextInsufficient,
                "the browser step has no Host-frozen page profile",
            );
        };
        if self.target.as_str() != profile.semantic_target() {
            return fail(
                FailureCode::EffectNotAuthorized,
                "the browser step does not name the frozen checkbox target",
            );
        }
        let desired = match self.desired.as_ref() {
            Some(ExpectedValue::Bool { value }) => *value,
            _ => {
                return fail(
                    FailureCode::ContextInsufficient,
                    "the browser checkbox step needs a frozen boolean value",
                );
            }
        };
        match self.phase.clone() {
            Phase::Ended(result) => SkillStep::Done(result),
            Phase::AwaitingOutcome(_) => fail(
                FailureCode::ContextUnresolved,
                "the browser input outcome is still unresolved",
            ),
            Phase::Start => {
                let Some(scope) = view.state.browser_scope() else {
                    return SkillStep::AwaitEvidence {
                        after_ms: self.input_floor.unwrap_or(view.now_ms),
                    };
                };
                let Some(latest) = view.state.browser_latest() else {
                    return SkillStep::AwaitEvidence {
                        after_ms: self.input_floor.unwrap_or(view.now_ms),
                    };
                };
                let Some(binding) = view.state.current_browser_binding(&self.binding) else {
                    return SkillStep::AwaitEvidence {
                        after_ms: latest.snapshot.observed_ms.saturating_add(1),
                    };
                };
                let proof = &binding.proof;
                let max_age_ms = view.max_age_ms.min(2_000);
                if binding.semantic_target != self.target
                    || latest.scope_id != scope.scope_id
                    || latest.generation != scope.generation
                    || latest.snapshot.snapshot_id != proof.snapshot_id
                    || latest.snapshot.source_sequence != proof.source_sequence
                    || latest.snapshot.observed_ms != proof.observed_ms
                    || proof.context != scope.context
                    || proof.clock_domain != scope.clock_domain
                    || proof.role != profile.role()
                    || proof.name != profile.label()
                    || proof.read_finished_ms > view.now_ms
                    || max_age_ms == 0
                    || view.now_ms.saturating_sub(proof.observed_ms) >= max_age_ms
                    || proof.validate().is_err()
                {
                    return SkillStep::AwaitEvidence {
                        after_ms: latest.snapshot.observed_ms.saturating_add(1),
                    };
                }
                self.browser_original_binding = Some(proof.clone());
                if proof.checked == desired {
                    return SkillStep::Done(SkillResult::AlreadyDone);
                }
                self.phase = Phase::AwaitingOutcome(Follow::Verify);
                self.actions += 1;
                SkillStep::BrowserAct(Box::new(BrowserActionPlan {
                    binding: proof.clone(),
                    action: BrowserAction::SetCheckbox { checked: desired },
                }))
            }
            Phase::Judging(_, Known { unknown: true, .. }) => fail(
                FailureCode::EffectUnknown,
                "the browser input outcome is unknown; it will not be replayed",
            ),
            Phase::Judging(_, Known { end_ms, .. }) => SkillStep::AwaitEvidence {
                after_ms: end_ms.saturating_add(1),
            },
        }
    }

    fn advance_background_ax(&mut self, view: &mut SkillView<'_>) -> SkillStep {
        if !matches!(self.skill, Skill::Activate | Skill::EnsureValue) {
            return fail(
                FailureCode::EffectNotAuthorized,
                "BackgroundAX supports only one registered AXPress value action",
            );
        }
        let desired = match self.desired.as_ref() {
            Some(ExpectedValue::Bool { value }) => *value,
            _ => {
                return fail(
                    FailureCode::ContextInsufficient,
                    "BackgroundAX needs a frozen boolean effect",
                );
            }
        };
        match self.phase.clone() {
            Phase::Ended(result) => SkillStep::Done(result),
            Phase::AwaitingOutcome(_) => fail(
                FailureCode::ContextUnresolved,
                "the BackgroundAX input outcome is still unresolved",
            ),
            Phase::Start => {
                let Some(env) = view.state.latest() else {
                    return SkillStep::AwaitEvidence {
                        after_ms: self.input_floor.unwrap_or(0),
                    };
                };
                let binding = match self.bound(view) {
                    Ok(binding) if binding.rule == "background_ax" => binding,
                    _ => return fail(FailureCode::ContextUnresolved, "background binding changed"),
                };
                let Some((object, target_context_hash)) =
                    background_object_in_scope(view, env, &binding)
                else {
                    return SkillStep::AwaitEvidence {
                        after_ms: env.snapshot.observed_ms.saturating_add(1),
                    };
                };
                let actual = match object.value.as_ref() {
                    Some(interaction_contracts::desktop::source::UiValue::Bool(value)) => *value,
                    _ => {
                        return fail(
                            FailureCode::ContextInsufficient,
                            "BackgroundAX target has no current boolean value",
                        );
                    }
                };
                if actual == desired {
                    self.phase = Phase::Ended(SkillResult::AlreadyDone);
                    return SkillStep::Done(SkillResult::AlreadyDone);
                }
                self.background_input_source_sequence = Some(env.snapshot.source_sequence);
                self.background_target_context_hash = Some(target_context_hash);
                self.background_input_binding = Some(binding.clone());
                self.phase = Phase::AwaitingOutcome(Follow::Verify);
                self.actions += 1;
                self.last_path = Some(ExecutionPath::Ax);
                SkillStep::Act(Box::new(ActionPlan {
                    binding,
                    primitive: Primitive::AxAction {
                        action: AxActionKind::Press,
                    },
                    path: ExecutionPath::Ax,
                    note: "press the registered BackgroundAX control once".into(),
                }))
            }
            Phase::Judging(_, known) => {
                if known.unknown {
                    return fail(
                        FailureCode::EffectUnknown,
                        "the BackgroundAX input outcome is unknown; it will not be replayed",
                    );
                }
                let after_ms = known
                    .end_ms
                    .saturating_add(1)
                    .max(self.input_floor.unwrap_or(known.end_ms).saturating_add(1));
                let Some(env) = view.state.latest_since(after_ms) else {
                    return SkillStep::AwaitEvidence { after_ms };
                };
                let input_sequence = self.background_input_source_sequence.unwrap_or(u64::MAX);
                if env.snapshot.observed_ms <= known.end_ms
                    || env.snapshot.observed_ms <= self.input_floor.unwrap_or(known.end_ms)
                    || env.snapshot.source_sequence <= input_sequence
                {
                    return SkillStep::AwaitEvidence {
                        after_ms: env.snapshot.observed_ms.saturating_add(1).max(after_ms),
                    };
                }
                let Some(input_binding) = self.background_input_binding.as_ref() else {
                    return fail(
                        FailureCode::ContextUnresolved,
                        "the completed BackgroundAX input proof is missing",
                    );
                };
                let Some(binding) = view.bindings.current(&self.target).cloned() else {
                    return fail(
                        FailureCode::ContextUnresolved,
                        "the BackgroundAX target is not freshly bound for readback",
                    );
                };
                if !background_readback_matches(
                    input_binding,
                    &binding,
                    &env.snapshot,
                    input_sequence,
                ) {
                    return fail(
                        FailureCode::ContextUnresolved,
                        "the fresh BackgroundAX read does not match the input's stable semantic context",
                    );
                }
                let Some((object, target_context_hash)) =
                    background_object_in_scope(view, env, &binding)
                else {
                    return fail(
                        FailureCode::ContextUnresolved,
                        "the fresh BackgroundAX target proof is invalid",
                    );
                };
                if self.background_target_context_hash.as_deref()
                    != Some(target_context_hash.as_str())
                {
                    return fail(
                        FailureCode::ContextUnresolved,
                        "the BackgroundAX target context changed after input",
                    );
                }
                match object.value.as_ref() {
                    Some(interaction_contracts::desktop::source::UiValue::Bool(value))
                        if *value == desired =>
                    {
                        SkillStep::Done(SkillResult::Succeeded)
                    }
                    Some(interaction_contracts::desktop::source::UiValue::Bool(_)) => fail(
                        FailureCode::EffectUnknown,
                        "fresh BackgroundAX readback does not match the frozen value",
                    ),
                    _ => fail(
                        FailureCode::EffectUnknown,
                        "fresh BackgroundAX readback has no boolean value",
                    ),
                }
            }
        }
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

    /// Revisit only clicks a page control that the app pack classifies as
    /// navigation. The live AXPress declaration remains source evidence; it
    /// does not select the input path for this skill.
    fn navigate(
        &mut self,
        pack: &AppPack,
        binding: Binding,
        object: &UiObject,
        follow: Follow,
    ) -> SkillStep {
        if pack.navigation_page(object).is_none() {
            return fail(
                FailureCode::ContextUnresolved,
                "the control is not a registered page-navigation target",
            );
        }
        if pack.effect_class(&object.role, label_of(object)) != EffectClass::Navigation {
            return fail(
                FailureCode::EffectNotAuthorized,
                "the registered page control is not authorized as navigation",
            );
        }
        self.act(
            binding,
            click(object),
            ExecutionPath::Pointer,
            follow,
            format!(
                "click registered navigation control \"{}\"",
                label_of(object)
            ),
        )
    }

    fn start(&mut self, view: &mut SkillView<'_>) -> SkillStep {
        if self.skill == Skill::Revisit && self.scoped_revisit.is_some() {
            return self.start_scoped_revisit(view);
        }
        if self.skill == Skill::Revisit && self.row_target.is_some() {
            return self.start_row_revisit(view);
        }
        if self.skill == Skill::EnsureValue && view.pack.keyboard_toggle(&self.target).is_some() {
            return self.start_keyboard(view);
        }
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
            Skill::EnterText => {
                if !matches!(self.desired, Some(ExpectedValue::Text { .. }))
                    || !object.focused
                    || !["textfield", "textarea", "AXTextField", "AXTextArea"]
                        .contains(&object.role.as_str())
                {
                    fail(
                        FailureCode::ContextUnresolved,
                        "text input needs a known focused text target",
                    )
                } else {
                    self.ensure(&binding, object, false)
                }
            }
            Skill::SaveDocument => unreachable!("owned document Save uses its private action path"),
            Skill::Activate => {
                if object.enabled {
                    self.press(binding, object, Follow::Verify)
                } else {
                    fail(FailureCode::ContextUnresolved, "the control is disabled")
                }
            }
            Skill::ActivateWindow => {
                unreachable!("window activation uses its dedicated evidence path")
            }
            Skill::ScrollTo => unreachable!("scrolling uses its dedicated evidence path"),
            Skill::SelectByLabel => unreachable!("menu selection uses its dedicated evidence path"),
            Skill::SelectRow => unreachable!("row selection uses its dedicated viewport path"),
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
                self.navigate(view.pack, nav, away, Follow::Away)
            }
        }
    }

    fn start_scoped_revisit(&mut self, view: &mut SkillView<'_>) -> SkillStep {
        if self.execution_domain != ExecutionDomain::ForegroundNative {
            return fail(
                FailureCode::ContextUnresolved,
                "scoped page revisit is limited to foreground native execution",
            );
        }
        let Some(pair) = self.scoped_revisit.clone() else {
            return fail(
                FailureCode::ContextUnresolved,
                "the scoped page pair is missing",
            );
        };
        let env = match evidence(view.state, None) {
            Ok(env)
                if view.state.usable(view.now_ms, view.max_age_ms)
                    && env.snapshot.observed_ms <= view.now_ms =>
            {
                env
            }
            Ok(_) => {
                return fail(
                    FailureCode::SourceInvalid,
                    "scoped page revisit requires healthy fresh Source evidence",
                );
            }
            Err(step) => return step,
        };
        if modal_in_front(env) {
            return fail(
                FailureCode::ContextUnresolved,
                "a modal layer prevents scoped page navigation",
            );
        }
        let Some(controls) = scoped_revisit_controls(&self.target, &pair, view.pack, env) else {
            return fail(
                FailureCode::ContextUnresolved,
                "the exact scoped page pair is missing, ambiguous, or outside the active window",
            );
        };
        if env.page.as_deref() != Some(controls.home_page.as_str()) {
            return fail(
                FailureCode::ContextUnresolved,
                "scoped revisit must begin on its frozen home page",
            );
        }
        let target = &pair.away;
        let Some(binding) = view.bindings.bind_instance(
            env,
            target.semantic_target.clone(),
            &controls.away.instance_ref,
        ) else {
            return fail(
                FailureCode::ContextUnresolved,
                "the frozen away page control could not be bound",
            );
        };
        if scoped_revisit_binding_object(&self.target, &pair, view.pack, env, &binding, target)
            .is_none()
            || view.bindings.current(&target.semantic_target) != Some(&binding)
        {
            view.bindings.invalidate(
                &target.semantic_target,
                "scoped away page binding failed exact Source revalidation",
            );
            return fail(
                FailureCode::ContextUnresolved,
                "the away page binding no longer matches the frozen page pair",
            );
        }
        self.last_scoped_source_sequence = Some(binding.continuity.source_sequence);
        self.home_page = Some(controls.home_page);
        self.navigate(view.pack, binding, controls.away, Follow::Away)
    }

    fn advance_row_selection(&mut self, view: &mut SkillView<'_>) -> SkillStep {
        let Some(target) = self.row_target.clone() else {
            return fail(
                FailureCode::ContextUnresolved,
                "row selection has no frozen row target",
            );
        };
        let step = match self.phase.clone() {
            Phase::Ended(result) => return SkillStep::Done(result),
            Phase::AwaitingOutcome(_) => {
                return fail(
                    FailureCode::ContextUnresolved,
                    "row selection input outcome is unresolved",
                );
            }
            Phase::Start => {
                let Some(row_spec) = target.row.as_ref() else {
                    return fail(
                        FailureCode::ContextUnresolved,
                        "row selection has no frozen table scope",
                    );
                };
                if !view
                    .state
                    .row_scope_usable(&row_spec.table_scope, view.now_ms, view.max_age_ms)
                {
                    return fail(
                        FailureCode::SourceInvalid,
                        "row selection requires healthy fresh Source evidence for its frozen table scope",
                    );
                }
                let Some(env) = view.state.latest() else {
                    return fail(
                        FailureCode::SourceInvalid,
                        "row selection requires a current Source read",
                    );
                };
                let Some(binding) = view.bindings.current_row(&self.target).cloned() else {
                    return fail(
                        FailureCode::ContextUnresolved,
                        "the frozen row has no current row binding",
                    );
                };
                if binding.table_scope != row_spec.table_scope {
                    return fail(
                        FailureCode::ContextUnresolved,
                        "the row binding does not match the frozen table scope",
                    );
                }
                if !env.snapshot.complete {
                    let candidate = env
                        .snapshot
                        .table_scope_search
                        .as_ref()
                        .filter(|search| search.requested_scope == row_spec.table_scope)
                        .and_then(|search| search.unique_candidate());
                    if row_scope_coverage(&env.snapshot, &row_spec.table_scope).is_err()
                        || candidate.is_none_or(|candidate| {
                            candidate.table_instance_ref != binding.selector.table_instance_ref
                                || candidate.table_instance_ref != binding.source.table_instance_ref
                        })
                    {
                        return fail(
                            FailureCode::ContextUnresolved,
                            "the partial Source read does not uniquely certify the frozen table scope",
                        );
                    }
                }
                if binding.binding_ref != self.binding
                    || binding.snapshot_id != env.snapshot.snapshot_id
                    || binding.source.source_sequence != env.snapshot.source_sequence
                    || binding.env_revision != env.revision
                    || binding.layer_revision != env.layer_revision
                    || binding.identity_boundary != env.snapshot.identity_boundary
                    || binding.semantic_target != target.semantic_target
                    || binding
                        .source
                        .validate_against_snapshot(
                            &binding.selector,
                            &env.snapshot,
                            view.now_ms,
                            view.max_age_ms,
                        )
                        .is_err()
                {
                    return fail(
                        FailureCode::ContextUnresolved,
                        "the row binding does not match the current Source read",
                    );
                }
                if binding.source.selected == Some(true) {
                    self.phase = Phase::Ended(SkillResult::AlreadyDone);
                    return SkillStep::Done(SkillResult::AlreadyDone);
                }
                if binding.source.selected != Some(false)
                    || binding.source.ax_selected_settable != Some(true)
                {
                    return fail(
                        FailureCode::ContextUnresolved,
                        "the row does not prove unselected, settable AXSelected",
                    );
                }
                let plan = match row::RowSelectionActionPlan::new(binding.clone()) {
                    Ok(plan) => plan,
                    Err(reason) => return fail(FailureCode::ContextUnresolved, reason),
                };
                self.row_input_sequence = Some(binding.source.source_sequence);
                self.phase = Phase::AwaitingOutcome(Follow::Verify);
                self.actions += 1;
                self.last_path = Some(ExecutionPath::Ax);
                SkillStep::SelectRow(Box::new(plan))
            }
            Phase::Judging(Follow::Verify, known) => {
                if known.unknown {
                    return fail(
                        FailureCode::EffectUnknown,
                        "unknown row selection input will not be replayed",
                    );
                }
                let floor = known.end_ms.saturating_add(1);
                let Some(row_spec) = target.row.as_ref() else {
                    return fail(
                        FailureCode::EffectUnknown,
                        "row selection has no frozen table scope for readback",
                    );
                };
                let Some(env) = view.state.latest_since(floor) else {
                    return SkillStep::AwaitEvidence { after_ms: floor };
                };
                if !view
                    .state
                    .row_scope_usable(&row_spec.table_scope, view.now_ms, view.max_age_ms)
                    || env.snapshot.observed_ms <= known.end_ms
                    || env.snapshot.source_sequence <= self.row_input_sequence.unwrap_or(u64::MAX)
                {
                    return SkillStep::AwaitEvidence {
                        after_ms: floor.max(env.snapshot.observed_ms.saturating_add(1)),
                    };
                }
                if !env.snapshot.complete {
                    let candidate = env
                        .snapshot
                        .table_scope_search
                        .as_ref()
                        .filter(|search| search.requested_scope == row_spec.table_scope)
                        .and_then(|search| search.unique_candidate());
                    if row_scope_coverage(&env.snapshot, &row_spec.table_scope).is_err()
                        || candidate.is_none()
                    {
                        return SkillStep::AwaitEvidence {
                            after_ms: env.snapshot.observed_ms.saturating_add(1),
                        };
                    }
                }
                match view
                    .bindings
                    .bind_row(env, &target, view.now_ms, view.max_age_ms)
                {
                    Ok(binding) if binding.source.selected == Some(true) => {
                        SkillStep::Done(SkillResult::Succeeded)
                    }
                    Ok(binding) if binding.source.selected == Some(false) => fail(
                        FailureCode::EffectUnknown,
                        "a fresh row read shows AXSelected is still false; the action will not be repeated",
                    ),
                    Ok(_) => fail(
                        FailureCode::EffectUnknown,
                        "a fresh row read does not prove its AXSelected state",
                    ),
                    Err(reason) => fail(
                        FailureCode::EffectUnknown,
                        format!(
                            "fresh row semantics could not be established after input: {reason}"
                        ),
                    ),
                }
            }
            Phase::Judging(_, _) => {
                return fail(
                    FailureCode::ContextUnresolved,
                    "row selection entered an invalid skill phase",
                );
            }
        };
        if let SkillStep::Done(result) = &step {
            self.phase = Phase::Ended(result.clone());
        }
        step
    }

    fn start_row_revisit(&mut self, view: &mut SkillView<'_>) -> SkillStep {
        if !view.state.usable(view.now_ms, view.max_age_ms) {
            return fail(
                FailureCode::SourceInvalid,
                "row revisit requires healthy fresh Source evidence",
            );
        }
        let env = match evidence(view.state, None) {
            Ok(env) => env,
            Err(step) => return step,
        };
        let Some(binding) = view.bindings.current_row(&self.target) else {
            return fail(
                FailureCode::ContextUnresolved,
                "the frozen row target is not currently resolved",
            );
        };
        if binding.binding_ref != self.binding
            || binding.snapshot_id != env.snapshot.snapshot_id
            || binding.source.source_sequence != env.snapshot.source_sequence
        {
            return fail(
                FailureCode::ContextUnresolved,
                "the frozen row binding changed before revisit",
            );
        }
        let Some(home) = env.page.clone() else {
            return fail(
                FailureCode::ContextUnresolved,
                "the row's current page is not known",
            );
        };
        let Some(away) = other_tab(env, view.pack, &home) else {
            return fail(
                FailureCode::CandidateMissing,
                "there is no other page to visit",
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
        self.navigate(view.pack, nav, away, Follow::Away)
    }

    fn start_keyboard(&mut self, view: &mut SkillView<'_>) -> SkillStep {
        if !view.state.usable(view.now_ms, view.max_age_ms) {
            return fail(
                FailureCode::SourceInvalid,
                "keyboard input requires healthy normal Source evidence",
            );
        }
        let env = match evidence(view.state, None) {
            Ok(env) => env,
            Err(step) => return step,
        };
        let Some(definition) = view.pack.keyboard_toggle(&self.target) else {
            return fail(
                FailureCode::EffectNotAuthorized,
                "keyboard target is not registered",
            );
        };
        let Some(context) = env.snapshot.focus_context.as_ref() else {
            return fail(
                FailureCode::ContextUnresolved,
                "Source has no actual focused receiver",
            );
        };
        let Some(object) = env
            .snapshot
            .objects
            .iter()
            .find(|object| object.instance_ref == context.target_instance)
        else {
            return fail(
                FailureCode::ContextUnresolved,
                "the actual focused receiver is missing",
            );
        };
        if env.snapshot.validate().is_err()
            || object.role != definition.role
            || normalize_label(label_of(object)) != normalize_label(&definition.label)
            || instance_fingerprint(&env.snapshot, object) != context.target_fingerprint
            || !matches!(self.desired, Some(ExpectedValue::Bool { .. }))
        {
            return fail(
                FailureCode::ContextUnresolved,
                "the actual focused control does not match the registered keyboard target",
            );
        }
        if view.bindings.current(&self.target).is_none_or(|bound| {
            bound.instance != context.target_instance
                || bound.fingerprint_digest != context.target_fingerprint.digest()
        }) {
            return fail(
                FailureCode::ContextUnresolved,
                "the original target binding does not identify the actual focused receiver",
            );
        }
        let Some(binding) =
            view.bindings
                .bind_instance(env, self.target.clone(), &context.target_instance)
        else {
            return fail(
                FailureCode::ContextUnresolved,
                "cannot bind the current actual focused receiver",
            );
        };
        self.binding = binding.binding_ref.clone();
        self.keyboard_context = Some(context.clone());
        self.ensure(&binding, object, false)
    }

    /// Decide what `ensure_value` does about what the control shows.
    fn ensure(&mut self, binding: &Binding, object: &UiObject, unknown: bool) -> SkillStep {
        if !object.enabled || !object.in_view || object.secure {
            return fail(
                FailureCode::ContextUnresolved,
                "the control is no longer available for input",
            );
        }
        let Some(desired) = self.desired.clone() else {
            return fail(FailureCode::ContextInsufficient, "there is no value to set");
        };
        if self.keyboard_context.is_some() && unknown {
            return fail(
                FailureCode::EffectUnknown,
                "a matching value cannot confirm unknown keyboard input",
            );
        }
        let current_matches = object.value.as_ref().and_then(|actual| {
            if self.scoped_form_text && matches!(desired, ExpectedValue::Number { .. }) {
                scoped_number_text_matches(actual, &desired)
            } else {
                values_equal(actual, &desired)
            }
        });
        match current_matches {
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
            Some(false) if self.keyboard_context.is_some() && self.actions == 0 => self.act(
                binding.clone(),
                Primitive::KeyPress {
                    key: LayoutIndependentSpecialKey::Space.canonical_name().into(),
                    modifiers: Modifiers::default(),
                },
                ExecutionPath::Keyboard,
                Follow::Verify,
                "press the registered Space key on the actual focused toggle",
            ),
            Some(false) if self.keyboard_context.is_some() => fail(
                FailureCode::EffectUnknown,
                "the one completed keyboard input did not establish the desired value",
            ),
            Some(false) => {
                if self.scoped_form_text {
                    let supported_form_value = match &desired {
                        ExpectedValue::Text { .. } => {
                            matches!(object.role.as_str(), "AXTextField" | "AXTextArea")
                        }
                        ExpectedValue::Number { value } => {
                            object.role == "AXTextField"
                                && (-MAX_SCOPED_FORM_SAFE_INTEGER..=MAX_SCOPED_FORM_SAFE_INTEGER)
                                    .contains(value)
                        }
                        ExpectedValue::Bool { .. } => false,
                    };
                    if !supported_form_value || !object.actions.contains(&AxActionName::SetValue) {
                        return fail(
                            FailureCode::ContextUnresolved,
                            "the scoped form text target no longer supports its frozen AX value action",
                        );
                    }
                    return match (self.actions, self.last_path, object.focused) {
                        (0, _, false) => {
                            self.scoped_form_focus_source_sequence =
                                Some(binding.continuity.source_sequence);
                            self.act(
                                binding.clone(),
                                click(object),
                                ExecutionPath::Pointer,
                                Follow::FocusText,
                                format!("focus registered form field \"{}\"", label_of(object)),
                            )
                        }
                        (0, _, true) | (1, Some(ExecutionPath::Pointer), true) => self.act(
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
                        (1, Some(ExecutionPath::Pointer), false) => fail(
                            FailureCode::ContextUnresolved,
                            "a fresh Source read did not confirm focus on the registered form field",
                        ),
                        _ => fail(
                            FailureCode::EffectUnknown,
                            "the scoped form field has no remaining authorized focus or value action",
                        ),
                    };
                }
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
        let floor = if self.keyboard_context.is_some() {
            known.end_ms.saturating_add(1)
        } else {
            known.end_ms
        };
        let env = match evidence(view.state, Some(floor)) {
            Ok(env) => env,
            Err(step) => return step,
        };
        // `latest_since` is inclusive, so a same-millisecond observation may
        // still be the pre-input Source read.
        if env.snapshot.observed_ms <= known.end_ms {
            return SkillStep::AwaitEvidence {
                after_ms: known.end_ms.saturating_add(1),
            };
        }
        match (follow, self.skill) {
            (Follow::PrepareScroll, _) => {
                unreachable!("scroll preparation uses its dedicated path")
            }
            (Follow::OpenMenu | Follow::ChooseMenuItem, _) => {
                unreachable!("menu selection uses its dedicated evidence path")
            }
            (Follow::FocusText, Skill::EnsureValue) => {
                if known.unknown {
                    return fail(
                        FailureCode::EffectUnknown,
                        "the form-field focus outcome is unknown; it will not be replayed",
                    );
                }
                if !self.scoped_form_text || !view.state.usable(view.now_ms, view.max_age_ms) {
                    return SkillStep::AwaitEvidence {
                        after_ms: floor.max(env.snapshot.observed_ms.saturating_add(1)),
                    };
                }
                let Some(focus_source_sequence) = self.scoped_form_focus_source_sequence else {
                    return fail(
                        FailureCode::ContextUnresolved,
                        "the focus click has no recorded source sequence",
                    );
                };
                if env.snapshot.source_sequence <= focus_source_sequence {
                    return SkillStep::AwaitEvidence {
                        after_ms: floor.max(env.snapshot.observed_ms.saturating_add(1)),
                    };
                }
                let binding = match self.bound(view) {
                    Ok(binding) => binding,
                    Err(step) => return step,
                };
                let Some(object) = object_for(env, &binding.fingerprint_digest) else {
                    return fail(
                        FailureCode::ContextUnresolved,
                        "the registered form field disappeared after the focus click",
                    );
                };
                if !object.focused
                    || !matches!(object.role.as_str(), "AXTextField" | "AXTextArea")
                    || !object.actions.contains(&AxActionName::SetValue)
                {
                    return fail(
                        FailureCode::ContextUnresolved,
                        "a newer Source read did not confirm focus on the registered form field",
                    );
                }
                self.ensure(&binding, object, false)
            }
            (Follow::FocusText, _) => fail(
                FailureCode::EffectNotAuthorized,
                "form-field focus is valid only inside its EnsureValue attempt",
            ),
            (Follow::Verify, Skill::EnsureValue | Skill::EnterText) => {
                if let Some(context) = &self.keyboard_context {
                    if known.unknown {
                        return fail(
                            FailureCode::EffectUnknown,
                            "unknown keyboard input cannot be confirmed by matching UI",
                        );
                    }
                    if env.snapshot.source_sequence <= context.verified_source_sequence
                        || !view.state.usable(view.now_ms, view.max_age_ms)
                    {
                        return SkillStep::AwaitEvidence {
                            after_ms: floor.max(env.snapshot.observed_ms.saturating_add(1)),
                        };
                    }
                }
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
            (Follow::Away, _) if self.scoped_revisit.is_some() => {
                if !view.state.usable(view.now_ms, view.max_age_ms)
                    || env.snapshot.observed_ms > view.now_ms
                    || self
                        .last_scoped_source_sequence
                        .is_some_and(|sequence| env.snapshot.source_sequence <= sequence)
                {
                    return fail(
                        FailureCode::SourceInvalid,
                        "scoped navigation needs a newer healthy Source read",
                    );
                }
                if modal_in_front(env) {
                    return fail(
                        FailureCode::ContextUnresolved,
                        "a modal layer interrupted scoped page navigation",
                    );
                }
                let Some(pair) = self.scoped_revisit.clone() else {
                    return fail(
                        FailureCode::ContextUnresolved,
                        "the scoped page pair is missing",
                    );
                };
                let Some(controls) = scoped_revisit_controls(&self.target, &pair, view.pack, env)
                else {
                    return fail(
                        FailureCode::ContextUnresolved,
                        "the frozen page pair changed or became ambiguous after navigation",
                    );
                };
                if env.page.as_deref() != Some(controls.away_page.as_str()) {
                    return if env.page.as_deref() == Some(controls.home_page.as_str()) {
                        fail(FailureCode::EffectUnknown, "the page did not change")
                    } else {
                        fail(
                            FailureCode::ContextUnresolved,
                            "scoped navigation reached a page outside its frozen pair",
                        )
                    };
                }
                let target = &pair.home;
                let Some(binding) = view.bindings.bind_instance(
                    env,
                    target.semantic_target.clone(),
                    &controls.home.instance_ref,
                ) else {
                    return fail(
                        FailureCode::ContextUnresolved,
                        "the frozen home page control could not be rebound",
                    );
                };
                if scoped_revisit_binding_object(
                    &self.target,
                    &pair,
                    view.pack,
                    env,
                    &binding,
                    target,
                )
                .is_none()
                    || view.bindings.current(&target.semantic_target) != Some(&binding)
                {
                    view.bindings.invalidate(
                        &target.semantic_target,
                        "scoped home page binding failed exact Source revalidation",
                    );
                    return fail(
                        FailureCode::ContextUnresolved,
                        "the home page binding no longer matches the frozen page pair",
                    );
                }
                self.last_scoped_source_sequence = Some(binding.continuity.source_sequence);
                self.navigate(view.pack, binding, controls.home, Follow::Back)
            }
            (Follow::Back, _) if self.scoped_revisit.is_some() => {
                if !view.state.usable(view.now_ms, view.max_age_ms)
                    || env.snapshot.observed_ms > view.now_ms
                    || self
                        .last_scoped_source_sequence
                        .is_some_and(|sequence| env.snapshot.source_sequence <= sequence)
                {
                    return fail(
                        FailureCode::SourceInvalid,
                        "scoped return needs a newer healthy Source read",
                    );
                }
                if modal_in_front(env) {
                    return fail(
                        FailureCode::ContextUnresolved,
                        "a modal layer is in the way back to the scoped home page",
                    );
                }
                let Some(pair) = self.scoped_revisit.as_ref() else {
                    return fail(
                        FailureCode::ContextUnresolved,
                        "the scoped page pair is missing",
                    );
                };
                let Some(controls) = scoped_revisit_controls(&self.target, pair, view.pack, env)
                else {
                    return fail(
                        FailureCode::ContextUnresolved,
                        "the frozen page pair changed or became ambiguous before return",
                    );
                };
                if env.page.as_deref() == Some(controls.home_page.as_str()) {
                    SkillStep::Done(SkillResult::Succeeded)
                } else if env.page.as_deref() == Some(controls.away_page.as_str()) {
                    fail(
                        FailureCode::EffectUnknown,
                        "did not return to the scoped home page",
                    )
                } else {
                    fail(
                        FailureCode::ContextUnresolved,
                        "scoped return reached a page outside its frozen pair",
                    )
                }
            }
            (Follow::Away, _) => {
                if modal_in_front(env) {
                    return fail(
                        FailureCode::ContextUnresolved,
                        "a dialog interrupted navigation",
                    );
                }
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
                self.navigate(view.pack, nav, back, Follow::Back)
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
    use crate::environment::binding::{BindingReducer, RowBinding};
    use crate::environment::pack::AppPack;
    use crate::environment::state_core::StateCore;
    use crate::testkit::*;
    use interaction_contracts::desktop::ids::ObjectRef;
    use interaction_contracts::desktop::ids::SemanticTargetRef;
    use interaction_contracts::desktop::source::{
        AxEnabledObservation, ObjectIdentityContinuity, ObserverCoverage, Rect,
        SourceIdentityBoundary, StructureSnapshot, TableViewport, TableViewportCell,
        TableViewportColumn, TableViewportRow, UiValue,
    };
    use interaction_contracts::desktop::table_scope::{
        TableScopeCandidate, TableScopePathElement, TableScopeSearch, TableScopeSearchState,
    };
    use interaction_contracts::desktop::task::{
        RowColumnConditionSpec, RowContainerRole, RowContainerScope, RowTableScope, RowTargetSpec,
    };
    use sim_world::app::{Applied, Modal, Page, SimApp};

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
            now_ms: rig.now_ms,
            max_age_ms: 2_000,
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

    fn row_target() -> TargetSpec {
        TargetSpec {
            semantic_target: SemanticTargetRef::new("saved-view-row").unwrap(),
            label: "Quarterly review".into(),
            group: None,
            role_hint: Some("AXRow".into()),
            row: Some(RowTargetSpec {
                table_scope: RowTableScope {
                    scroll_path: vec![RowContainerScope {
                        role: RowContainerRole::AxScrollArea,
                        label: "Saved views".into(),
                    }],
                    column_headers: vec!["Entry".into()],
                },
                conditions: vec![RowColumnConditionSpec {
                    column_name: "Entry".into(),
                    value: ExpectedValue::Text {
                        value: "Quarterly review".into(),
                    },
                }],
            }),
        }
    }

    fn partial_row_snapshot(
        sequence: u64,
        observed_ms: u64,
        selected: Option<bool>,
    ) -> StructureSnapshot {
        let mut snapshot = SimApp::new().snapshot(&meta(sequence, observed_ms));
        let window_id = snapshot
            .global_focus
            .known_window_for_app(&snapshot.app)
            .unwrap()
            .to_owned();
        let window = snapshot
            .windows
            .iter_mut()
            .find(|window| window.window_id == window_id)
            .unwrap();
        window.owner_bundle_id = "com.lingxi.computer-user.mvp-fixture".into();
        let window_frame = window.frame;
        snapshot.app.bundle_id = "com.lingxi.computer-user.mvp-fixture".into();
        crate::testkit::focus_snapshot_window(&mut snapshot, window_id.clone());
        snapshot.identity_boundary = Some(SourceIdentityBoundary {
            namespace: "00000000-0000-4000-8000-000000000001".into(),
            generation: 1,
        });
        snapshot.complete = false;
        snapshot.incomplete_reason = Some("AX child list truncated".into());

        let mut root = snapshot
            .objects
            .iter()
            .find(|object| object.window_id == window_id && object.parent.is_none())
            .unwrap()
            .clone();
        root.role = "AXWindow".into();
        root.frame = window_frame;
        root.identity_continuity = ObjectIdentityContinuity::NotificationCovered;
        let root_ref = root.instance_ref.clone();
        let scroll_ref = ObjectRef::new("row-scroll").unwrap();
        let table_ref = ObjectRef::new("row-table").unwrap();
        let column_ref = ObjectRef::new("row-column").unwrap();
        let header_ref = ObjectRef::new("row-header").unwrap();
        let row_ref = ObjectRef::new("row-target").unwrap();
        let cell_ref = ObjectRef::new("row-cell").unwrap();
        let scroll_frame = Rect {
            x: window_frame.x + 20.0,
            y: window_frame.y + 40.0,
            width: window_frame.width - 40.0,
            height: window_frame.height - 80.0,
        };
        let object = |reference: &str,
                      parent: Option<ObjectRef>,
                      role: &str,
                      frame: Rect,
                      description: Option<&str>| {
            UiObject {
                instance_ref: ObjectRef::new(reference).unwrap(),
                parent,
                window_id: window_id.clone(),
                role: role.into(),
                subrole: None,
                title: None,
                description: description.map(str::to_owned),
                value: None,
                enabled: true,
                focused: false,
                frame,
                actions: vec![],
                ax_action_names: None,
                in_view: true,
                group: None,
                secure: false,
                scroll: None,
                identity_continuity: ObjectIdentityContinuity::NotificationCovered,
            }
        };
        let scroll = object(
            scroll_ref.as_str(),
            Some(root_ref.clone()),
            "AXScrollArea",
            scroll_frame,
            Some("Saved views"),
        );
        let table = object(
            table_ref.as_str(),
            Some(scroll_ref.clone()),
            "AXTable",
            scroll_frame,
            None,
        );
        let column = object(
            column_ref.as_str(),
            Some(table_ref.clone()),
            "AXColumn",
            scroll_frame,
            None,
        );
        let header_frame = Rect {
            x: scroll_frame.x,
            y: scroll_frame.y,
            width: scroll_frame.width,
            height: 24.0,
        };
        let header = object(
            header_ref.as_str(),
            Some(column_ref.clone()),
            "AXStaticText",
            header_frame,
            Some("Entry"),
        );
        let row_frame = Rect {
            x: scroll_frame.x,
            y: scroll_frame.y + 20.0,
            width: scroll_frame.width,
            height: 30.0,
        };
        let row = object(
            row_ref.as_str(),
            Some(table_ref.clone()),
            "AXRow",
            row_frame,
            None,
        );
        let mut cell = object(
            cell_ref.as_str(),
            Some(row_ref.clone()),
            "AXCell",
            row_frame,
            None,
        );
        cell.value = Some(UiValue::Text("Quarterly review".into()));
        snapshot.objects = vec![root, scroll, table, column, header, row, cell];
        snapshot.observer_coverage = ObserverCoverage::for_projected_objects(&snapshot.objects);
        snapshot.table_viewports = vec![TableViewport {
            table_instance_ref: table_ref.clone(),
            scroll_container_instance_ref: scroll_ref.clone(),
            process_id: snapshot.app.pid,
            ax_enabled: AxEnabledObservation::Enabled,
            table_frame: scroll_frame,
            viewport_frame: scroll_frame,
            visible_rows_complete: true,
            visible_rows: vec![TableViewportRow {
                instance_ref: Some(row_ref.clone()),
                parent_instance_ref: table_ref.clone(),
                frame: row_frame,
                ax_enabled: AxEnabledObservation::Enabled,
                ax_index: Some(0),
                accessibility_identifier: None,
                selected,
                ax_selected_settable: Some(true),
                cells_complete: true,
                cells: vec![TableViewportCell {
                    instance_ref: Some(cell_ref),
                    parent_instance_ref: Some(row_ref),
                    frame: row_frame,
                    row_ax_index: Some(0),
                    column_ax_index: Some(0),
                    column_instance_ref: Some(column_ref.clone()),
                    header_instance_ref: Some(header_ref.clone()),
                    column_header: Some("Entry".into()),
                    value: Some(UiValue::Text("Quarterly review".into())),
                }],
            }],
            columns_complete: true,
            columns: vec![TableViewportColumn {
                instance_ref: Some(column_ref),
                parent_instance_ref: table_ref.clone(),
                frame: scroll_frame,
                header_instance_ref: Some(header_ref.clone()),
                header: Some("Entry".into()),
                ax_index: Some(0),
            }],
        }];
        let scope = row_target().row.unwrap().table_scope;
        snapshot.table_scope_search = Some(TableScopeSearch {
            requested_scope: scope.clone(),
            process_id: snapshot.app.pid,
            window_id,
            state: TableScopeSearchState::Complete,
            candidates: vec![TableScopeCandidate {
                table_instance_ref: table_ref,
                scroll_container_instance_ref: scroll_ref.clone(),
                scroll_path: vec![TableScopePathElement {
                    instance_ref: scroll_ref,
                    label: "Saved views".into(),
                    frame: scroll_frame,
                }],
                column_headers: scope.column_headers,
            }],
        });
        snapshot
    }

    fn partial_row_rig(
        sequence: u64,
        observed_ms: u64,
        selected: Option<bool>,
    ) -> (StateCore, AppPack, BindingReducer, RowBinding, TargetSpec) {
        let target = row_target();
        let pack = AppPack::registered_native("com.lingxi.computer-user.mvp-fixture").unwrap();
        let mut state = StateCore::new();
        state.set_session(1);
        state
            .admit(
                partial_row_snapshot(sequence, observed_ms, selected),
                observed_ms,
                &pack,
            )
            .unwrap();
        let env = state.latest().unwrap().clone();
        let mut bindings = BindingReducer::new();
        let binding = bindings
            .bind_row(&env, &target, observed_ms, 2_000)
            .unwrap();
        (state, pack, bindings, binding, target)
    }

    fn advance_partial_row(
        run: &mut SkillRun,
        state: &StateCore,
        bindings: &mut BindingReducer,
        pack: &AppPack,
        now_ms: u64,
    ) -> SkillStep {
        run.advance(&mut SkillView {
            state,
            bindings,
            pack,
            now_ms,
            max_age_ms: 2_000,
        })
    }

    #[test]
    fn partial_row_scope_accepts_only_strictly_new_readback_and_never_repeats_input() {
        for (selected, expected_success) in
            [(Some(true), true), (Some(false), false), (None, false)]
        {
            let (mut state, pack, mut bindings, binding, target) =
                partial_row_rig(1, 100, Some(false));
            let mut run = SkillRun::new(
                Skill::SelectRow,
                binding.semantic_target.clone(),
                None,
                binding.binding_ref.clone(),
            )
            .with_row_target(target);
            assert!(matches!(
                advance_partial_row(&mut run, &state, &mut bindings, &pack, 100),
                SkillStep::SelectRow(_)
            ));
            run.acted(ActionOutcome::Completed { end_ms: 150 });
            assert_eq!(run.actions(), 1);
            assert_eq!(
                advance_partial_row(&mut run, &state, &mut bindings, &pack, 150),
                SkillStep::AwaitEvidence { after_ms: 151 },
                "the partial pre-input read cannot confirm its own action"
            );

            state
                .admit(partial_row_snapshot(2, 151, selected), 151, &pack)
                .unwrap();
            let latest = state.latest().unwrap().clone();
            bindings.revalidate(&latest);
            let result = advance_partial_row(&mut run, &state, &mut bindings, &pack, 151);
            if expected_success {
                assert_eq!(result, SkillStep::Done(SkillResult::Succeeded));
            } else {
                assert!(matches!(
                    result,
                    SkillStep::Done(SkillResult::Failed {
                        code: FailureCode::EffectUnknown,
                        ..
                    })
                ));
            }
            assert_eq!(
                run.actions(),
                1,
                "fresh false or unknown never replays input"
            );
        }
    }

    #[test]
    fn unknown_partial_row_input_outcome_is_terminal_without_replay() {
        let (state, pack, mut bindings, binding, target) = partial_row_rig(1, 100, Some(false));
        let mut run = SkillRun::new(
            Skill::SelectRow,
            binding.semantic_target.clone(),
            None,
            binding.binding_ref.clone(),
        )
        .with_row_target(target);
        assert!(matches!(
            advance_partial_row(&mut run, &state, &mut bindings, &pack, 100),
            SkillStep::SelectRow(_)
        ));
        run.acted(ActionOutcome::Unknown { at_ms: 120 });
        assert!(matches!(
            advance_partial_row(&mut run, &state, &mut bindings, &pack, 120),
            SkillStep::Done(SkillResult::Failed {
                code: FailureCode::EffectUnknown,
                ..
            })
        ));
        assert_eq!(run.actions(), 1);
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
    fn same_millisecond_source_cannot_authorize_the_pointer_fallback() {
        let mut rig = EnvRig::new();
        rig.observe(0);
        let binding = rig.bind_goal_target();
        let mut run = SkillRun::new(Skill::EnsureValue, target(), off(), binding.binding_ref);

        let first = act(advance(&mut run, &mut rig));
        assert_eq!(first.path, ExecutionPath::Ax);
        assert!(matches!(
            first.primitive,
            Primitive::AxAction {
                action: AxActionKind::SetValue { .. }
            }
        ));
        apply(&mut rig, &first);
        run.acted(ActionOutcome::Completed { end_ms: 0 });

        assert_eq!(rig.state.latest().unwrap().snapshot.observed_ms, 0);
        assert_eq!(
            advance(&mut run, &mut rig),
            SkillStep::AwaitEvidence { after_ms: 1 }
        );
        assert_eq!(
            run.actions(),
            1,
            "the old Source read cannot trigger fallback"
        );

        rig.observe(50);
        assert_eq!(rig.state.latest().unwrap().snapshot.observed_ms, 50);
        assert_eq!(rig.app.switch_on("Push", "Daily picks"), Some(false));
        assert_eq!(
            advance(&mut run, &mut rig),
            SkillStep::Done(SkillResult::Succeeded)
        );
        assert_eq!(
            run.actions(),
            1,
            "the fresh Source confirms the completed input"
        );
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
    fn known_ax_completion_can_use_a_fresh_relocated_binding_for_its_pointer_path() {
        let (mut rig, mut run) = ready();
        assert_eq!(act(advance(&mut run, &mut rig)).path, ExecutionPath::Ax);
        run.acted(ActionOutcome::Completed { end_ms: 110 });
        let mut snapshot = rig.app.snapshot(&meta(2, 200));
        snapshot
            .objects
            .iter_mut()
            .find(|o| o.instance_ref.as_str() == "sw-push-daily")
            .unwrap()
            .instance_ref =
            interaction_contracts::desktop::ids::ObjectRef::new("replacement-toggle").unwrap();
        rig.state.admit(snapshot, 200, &rig.pack).unwrap();
        rig.now_ms = 200;
        rig.bindings.revalidate(rig.state.latest().unwrap());
        let binding = rig.bind_goal_target();
        let mut stale = binding.clone();
        stale.observed_ms = 109;
        assert!(!run.relocate_known_toggle(&stale));
        assert!(run.relocate_known_toggle(&binding));
        assert_eq!(run.input_floor(), Some(110));
        let pointer = act(advance(&mut run, &mut rig));
        assert_eq!(pointer.path, ExecutionPath::Pointer);
        assert_eq!(run.actions(), 2);
        apply(&mut rig, &pointer);
        run.acted(ActionOutcome::Completed { end_ms: 210 });
        let mut snapshot = rig.app.snapshot(&meta(3, 250));
        snapshot
            .objects
            .iter_mut()
            .find(|o| o.instance_ref.as_str() == "sw-push-daily")
            .unwrap()
            .instance_ref =
            interaction_contracts::desktop::ids::ObjectRef::new("after-pointer-toggle").unwrap();
        rig.state.admit(snapshot, 250, &rig.pack).unwrap();
        rig.now_ms = 250;
        rig.bindings.revalidate(rig.state.latest().unwrap());
        let binding = rig.bind_goal_target();
        assert!(run.relocate_known_toggle(&binding));
        assert_eq!(
            advance(&mut run, &mut rig),
            SkillStep::Done(SkillResult::Succeeded)
        );
        assert_eq!(run.actions(), 2);
    }

    #[test]
    fn unknown_ax_input_never_relocates_into_a_second_input() {
        let (mut rig, mut run) = ready();
        let first = act(advance(&mut run, &mut rig));
        run.acted(ActionOutcome::Unknown { at_ms: 110 });
        let mut snapshot = rig.app.snapshot(&meta(2, 200));
        snapshot
            .objects
            .iter_mut()
            .find(|o| o.instance_ref.as_str() == "sw-push-daily")
            .unwrap()
            .instance_ref =
            interaction_contracts::desktop::ids::ObjectRef::new("replacement-toggle").unwrap();
        rig.state.admit(snapshot, 200, &rig.pack).unwrap();
        rig.now_ms = 200;
        rig.bindings.revalidate(rig.state.latest().unwrap());
        let binding = rig.bind_goal_target();
        assert_ne!(first.binding.binding_ref, binding.binding_ref);
        assert!(!run.relocate_known_toggle(&binding));
        assert!(matches!(
            advance(&mut run, &mut rig),
            SkillStep::Done(SkillResult::Failed { .. })
        ));
        assert_eq!(run.actions(), 1);
    }

    #[test]
    fn ensure_value_does_not_fallback_to_input_after_a_control_becomes_ineligible() {
        for change in ["disabled", "hidden", "secure"] {
            let (mut rig, mut run) = ready();
            let first = act(advance(&mut run, &mut rig));
            assert_eq!(first.path, ExecutionPath::Ax);
            run.acted(ActionOutcome::Completed { end_ms: 110 });
            let mut snapshot = rig.app.snapshot(&meta(2, 210));
            let object = snapshot
                .objects
                .iter_mut()
                .find(|object| object.instance_ref.as_str() == "sw-push-daily")
                .unwrap();
            match change {
                "disabled" => object.enabled = false,
                "hidden" => object.in_view = false,
                "secure" => {
                    object.secure = true;
                    object.value = None;
                }
                _ => unreachable!(),
            }
            rig.state.admit(snapshot, 210, &rig.pack).unwrap();
            assert!(
                matches!(
                    advance(&mut run, &mut rig),
                    SkillStep::Done(SkillResult::Failed { .. })
                ),
                "{change}"
            );
            assert_eq!(run.actions(), 1, "{change}");
        }
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

    fn assert_navigation_source_keeps_press(env: &EnvSnapshot, pack: &AppPack) {
        let controls: Vec<_> = env
            .snapshot
            .objects
            .iter()
            .filter(|object| pack.navigation_page(object).is_some())
            .collect();
        assert!(
            !controls.is_empty(),
            "the Source has registered page controls"
        );
        assert!(
            controls
                .iter()
                .all(|object| object.actions.contains(&AxActionName::Press))
        );
    }

    fn assert_pointer_navigation(plan: &ActionPlan, semantic: &str) {
        assert_eq!(plan.binding.semantic_target.as_str(), semantic);
        assert_eq!(plan.path, ExecutionPath::Pointer);
        assert!(matches!(
            &plan.primitive,
            Primitive::Click {
                button: MouseButton::Left,
                modifiers,
                ..
            } if *modifiers == Modifiers::default()
        ));
        assert!(plan.primitive.needs_hit_test());
    }

    #[test]
    fn revisit_leaves_the_page_and_comes_back() {
        let (mut rig, mut run) = revisit();
        assert_navigation_source_keeps_press(rig.state.latest().unwrap(), &rig.pack);
        let away = act(advance(&mut run, &mut rig));
        assert_pointer_navigation(&away, "nav/away");
        apply(&mut rig, &away);
        run.acted(ActionOutcome::Completed { end_ms: 110 });
        rig.observe(200);
        assert_eq!(rig.app.page(), Page::General);
        assert_navigation_source_keeps_press(rig.state.latest().unwrap(), &rig.pack);

        let back = act(advance(&mut run, &mut rig));
        assert_pointer_navigation(&back, "nav/back");
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
    fn unknown_away_navigation_never_dispatches_back_even_when_source_shows_away() {
        let (mut rig, mut run) = revisit();
        let away = act(advance(&mut run, &mut rig));
        assert_pointer_navigation(&away, "nav/away");
        apply(&mut rig, &away);
        run.acted(ActionOutcome::Unknown { at_ms: 110 });

        rig.observe(200);
        assert_eq!(rig.app.page(), Page::General);
        let result = advance(&mut run, &mut rig);
        assert!(matches!(
            &result,
            SkillStep::Done(SkillResult::Failed {
                code: FailureCode::EffectUnknown,
                ..
            })
        ));
        assert_eq!(
            run.actions(),
            1,
            "Unknown Away must not create a Back action"
        );
        assert_eq!(advance(&mut run, &mut rig), result);
    }

    #[test]
    fn unknown_back_navigation_cannot_be_upgraded_when_source_shows_home() {
        let (mut rig, mut run) = revisit();
        let away = act(advance(&mut run, &mut rig));
        apply(&mut rig, &away);
        run.acted(ActionOutcome::Completed { end_ms: 110 });
        rig.observe(200);

        let back = act(advance(&mut run, &mut rig));
        assert_pointer_navigation(&back, "nav/back");
        apply(&mut rig, &back);
        run.acted(ActionOutcome::Unknown { at_ms: 310 });
        rig.observe(400);
        assert_eq!(rig.app.page(), Page::Notifications);

        assert!(matches!(
            advance(&mut run, &mut rig),
            SkillStep::Done(SkillResult::Failed {
                code: FailureCode::EffectUnknown,
                ..
            })
        ));
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
    fn mvp_revisit_clicks_only_registered_page_radios_when_ax_press_is_available() {
        let mut snapshot = sim_world::app::SimApp::new().snapshot(&meta(1, 10));
        let active = snapshot
            .global_focus
            .known_window_for_app(&snapshot.app)
            .unwrap()
            .to_owned();
        let template = snapshot
            .objects
            .iter()
            .find(|object| object.role == "AXWindow" && object.parent.is_none())
            .expect("synthetic Source has its actual AXWindow root")
            .clone();
        let window = snapshot
            .windows
            .iter_mut()
            .find(|window| window.window_id == active)
            .unwrap();
        window.owner_bundle_id = "com.lingxi.computer-user.mvp-fixture".into();
        let mut root = template.clone();
        root.title = window.title.clone();
        let root_ref = root.instance_ref.clone();
        snapshot.app.bundle_id = "com.lingxi.computer-user.mvp-fixture".into();
        crate::testkit::focus_snapshot_window(&mut snapshot, active.clone());
        snapshot.objects.clear();

        let radio = |id: &str, label: &str, selected: bool| {
            let mut object = template.clone();
            object.instance_ref = interaction_contracts::desktop::ids::ObjectRef::new(id).unwrap();
            object.parent = Some(root_ref.clone());
            object.window_id = active.clone();
            object.frame = interaction_contracts::desktop::source::Rect {
                x: match id {
                    "settings" => 20.0,
                    "summary" => 140.0,
                    "overview" => 260.0,
                    _ => unreachable!(),
                },
                y: 20.0,
                width: 110.0,
                height: 28.0,
            };
            object.role = "AXRadioButton".into();
            object.subrole = None;
            object.title = Some(label.into());
            object.description = None;
            object.value = Some(interaction_contracts::desktop::source::UiValue::Bool(
                selected,
            ));
            object.enabled = true;
            object.actions = vec![AxActionName::Press];
            object.in_view = true;
            object.group = None;
            object.secure = false;
            object
        };
        let mut checkbox = template.clone();
        checkbox.instance_ref =
            interaction_contracts::desktop::ids::ObjectRef::new("email-alerts").unwrap();
        checkbox.parent = Some(root_ref.clone());
        checkbox.window_id = active.clone();
        checkbox.frame = interaction_contracts::desktop::source::Rect {
            x: 20.0,
            y: 80.0,
            width: 110.0,
            height: 28.0,
        };
        checkbox.role = "AXCheckBox".into();
        checkbox.subrole = None;
        checkbox.title = Some("邮件提醒".into());
        checkbox.description = None;
        checkbox.value = Some(interaction_contracts::desktop::source::UiValue::Bool(false));
        checkbox.enabled = true;
        checkbox.actions = vec![AxActionName::Press, AxActionName::SetValue];
        checkbox.in_view = true;
        checkbox.group = None;
        checkbox.secure = false;
        snapshot.objects = vec![
            root,
            radio("settings", "设置", true),
            radio("summary", "每周", false),
            radio("overview", "概览", false),
            checkbox,
        ];
        sim_world::app::refresh_simulated_observer_coverage(&mut snapshot);
        let env = EnvSnapshot {
            revision: 1,
            layer_revision: 1,
            snapshot,
            page: Some("Settings".into()),
        };
        let pack = AppPack::registered_native("com.lingxi.computer-user.mvp-fixture").unwrap();
        let mut state = StateCore::new();
        state.set_session(1);
        state.admit(env.snapshot.clone(), 10, &pack).unwrap();
        let env = state.latest().unwrap();
        assert_navigation_source_keeps_press(env, &pack);

        let away = other_tab(env, &pack, "Settings").unwrap();
        assert_eq!(away.title.as_deref(), Some("概览"));
        assert_eq!(
            tab_for(env, &pack, "Settings").and_then(|object| object.title.as_deref()),
            Some("设置")
        );

        let mut bindings = BindingReducer::new();
        let current = tab_for(env, &pack, "Settings").unwrap();
        let goal_binding = bindings
            .bind_instance(env, target(), &current.instance_ref)
            .expect("the current registered page control binds as the revisit target");
        let mut run = SkillRun::new(Skill::Revisit, target(), None, goal_binding.binding_ref);
        let plan = act(run.advance(&mut SkillView {
            state: &state,
            bindings: &mut bindings,
            pack: &pack,
            now_ms: 10,
            max_age_ms: 2_000,
        }));
        assert_pointer_navigation(&plan, "nav/away");

        // The static page registration excludes the unrelated summary radio.
        let summary = env
            .snapshot
            .objects
            .iter()
            .find(|object| object.title.as_deref() == Some("每周"))
            .unwrap();
        assert!(pack.navigation_page(summary).is_none());

        let primary = pack
            .control(&SemanticTargetRef::new("setting-email-alerts-zh").unwrap())
            .unwrap();
        let (home_control, away_control) = pack.scoped_revisit_pages(primary).unwrap();
        let endpoint = |control: &crate::environment::pack::ControlDef| TargetSpec {
            semantic_target: SemanticTargetRef::new(&control.semantic).unwrap(),
            label: control.label.clone(),
            group: None,
            role_hint: Some(control.role.clone()),
            row: None,
        };
        let pair = ScopedUiRevisit {
            home: endpoint(home_control),
            away: endpoint(away_control),
        };
        let primary_object = env
            .snapshot
            .objects
            .iter()
            .find(|object| object.title.as_deref() == Some(primary.label.as_str()))
            .unwrap();
        let primary_binding = bindings
            .bind_instance(
                env,
                SemanticTargetRef::new(&primary.semantic).unwrap(),
                &primary_object.instance_ref,
            )
            .unwrap();
        let mut scoped = SkillRun::new(
            Skill::Revisit,
            SemanticTargetRef::new(&primary.semantic).unwrap(),
            None,
            primary_binding.binding_ref,
        )
        .with_scoped_revisit(pair.clone());
        let scoped_away = act(scoped.advance(&mut SkillView {
            state: &state,
            bindings: &mut bindings,
            pack: &pack,
            now_ms: 10,
            max_age_ms: 2_000,
        }));
        assert_pointer_navigation(&scoped_away, pair.away.semantic_target.as_str());
        assert_eq!(
            scoped.scoped_revisit_action_target(),
            Some(&pair.away),
            "the first scoped input is frozen to the registered away endpoint"
        );
        assert_eq!(
            bindings.current(&pair.away.semantic_target),
            Some(&scoped_away.binding),
            "the skill installs a fresh binding only for its frozen away endpoint"
        );

        scoped.acted(ActionOutcome::Unknown { at_ms: 20 });
        assert!(matches!(
            scoped.advance(&mut SkillView {
                state: &state,
                bindings: &mut bindings,
                pack: &pack,
                now_ms: 20,
                max_age_ms: 2_000,
            }),
            SkillStep::Done(SkillResult::Failed {
                code: FailureCode::EffectUnknown,
                ..
            })
        ));
        assert_eq!(scoped.actions(), 1, "Unknown navigation is never replayed");
    }

    #[test]
    fn only_the_implemented_skills_have_a_runtime() {
        for name in Skill::IMPLEMENTED {
            assert_eq!(Skill::from_name(name).unwrap().name(), name);
        }
        assert_eq!(Skill::from_name("ui.enter_text"), Some(Skill::EnterText));
        assert_eq!(Skill::from_name("ui.scroll_to"), Some(Skill::ScrollTo));
        assert_eq!(Skill::EnsureValue.quota().actions, 2);
    }
    fn text_ready(focused: bool, value: &str) -> (EnvRig, SkillRun) {
        use interaction_contracts::desktop::source::UiValue;
        let mut rig = EnvRig::new();
        let mut snapshot = rig.app.snapshot(&meta(1, 10));
        let object = snapshot
            .objects
            .iter_mut()
            .find(|o| o.instance_ref.as_str() == "sw-push-daily")
            .unwrap();
        object.role = "textfield".into();
        object.value = Some(UiValue::Text(value.into()));
        object.focused = focused;
        object.actions = vec![AxActionName::SetValue];
        let instance = object.instance_ref.clone();
        rig.state.admit(snapshot, 10, &rig.pack).unwrap();
        let binding = rig
            .bindings
            .bind_instance(rig.state.latest().unwrap(), target(), &instance)
            .unwrap();
        let run = SkillRun::new(
            Skill::EnterText,
            target(),
            Some(ExpectedValue::Text {
                value: "wanted".into(),
            }),
            binding.binding_ref,
        );
        (rig, run)
    }
    #[test]
    fn enter_text_requires_focus_and_skips_an_already_matching_value() {
        let (mut rig, mut run) = text_ready(false, "before");
        assert!(matches!(
            advance(&mut run, &mut rig),
            SkillStep::Done(SkillResult::Failed {
                code: FailureCode::ContextUnresolved,
                ..
            })
        ));
        assert_eq!(run.actions(), 0);
        let (mut rig, mut run) = text_ready(true, "wanted");
        assert_eq!(
            advance(&mut run, &mut rig),
            SkillStep::Done(SkillResult::AlreadyDone)
        );
        assert_eq!(run.actions(), 0);
    }
    #[test]
    fn enter_text_uses_set_value_and_does_not_replay_an_unknown_input() {
        let (mut rig, mut run) = text_ready(true, "before");
        let plan = act(advance(&mut run, &mut rig));
        assert_eq!(
            plan.primitive,
            Primitive::AxAction {
                action: AxActionKind::SetValue {
                    value: "wanted".into()
                }
            }
        );
        run.acted(ActionOutcome::Unknown { at_ms: 20 });
        assert_eq!(
            advance(&mut run, &mut rig),
            SkillStep::AwaitEvidence { after_ms: 20 }
        );
        let mut snapshot = rig.state.latest().unwrap().snapshot.clone();
        snapshot.snapshot_id =
            interaction_contracts::desktop::ids::RequestId::new("after-unknown").unwrap();
        snapshot.source_sequence += 1;
        snapshot.observed_ms = 30;
        rig.state.admit(snapshot, 30, &rig.pack).unwrap();
        assert!(matches!(
            advance(&mut run, &mut rig),
            SkillStep::Done(SkillResult::Failed {
                code: FailureCode::EffectUnknown,
                ..
            })
        ));
        assert_eq!(run.actions(), 1);
    }
}

#[cfg(test)]
pub(crate) mod background_readback_test_support {
    use super::*;
    use interaction_contracts::desktop::ids::{ObjectRef, RequestId};
    use interaction_contracts::desktop::source::{
        ObjectIdentityContinuity, ObserverCoverage, StructureSnapshot, TargetContext,
    };

    pub const APP: &str = "com.lingxi.computer-user.guard-fixture";
    const SOURCE_FIXTURE: &str = include_str!(
        "../../../../contracts/desktop-v1/native-binding/background-window-source.json"
    );

    /// Offline Source replay plus a synthetic next-read projection. The
    /// projected sequence/reference is test data, never a live Native read.
    pub fn snapshots() -> (StructureSnapshot, StructureSnapshot) {
        let mut before: StructureSnapshot =
            serde_json::from_str(SOURCE_FIXTURE).expect("captured Source replay is valid");
        let target = before
            .objects
            .iter_mut()
            .find(|object| object.role == "AXCheckBox")
            .expect("fixture has its registered checkbox");
        target.identity_continuity = ObjectIdentityContinuity::ReadScoped;
        before.observer_coverage = ObserverCoverage::for_projected_objects(&before.objects);
        before
            .validate()
            .expect("the read-scoped input projection remains a valid Source snapshot");

        let mut after = before.clone();
        after.source_sequence += 1;
        after.snapshot_id = RequestId::new("background-readback-post-input").unwrap();
        after.observed_ms += 10;
        let target = after
            .objects
            .iter_mut()
            .find(|object| object.role == "AXCheckBox")
            .expect("fixture has its registered checkbox");
        target.instance_ref = ObjectRef::new("read-scoped-checkbox-after-input").unwrap();
        target.value = Some(interaction_contracts::desktop::source::UiValue::Bool(true));
        after.observer_coverage = ObserverCoverage::for_projected_objects(&after.objects);
        if let Some(TargetContext::NativeWindow { app, source }) = after.target_context.as_mut() {
            source.window.verified_source_sequence = after.source_sequence;
            source.window.verified_observed_ms = after.observed_ms;
            source.target_context_hash = source.computed_target_context_hash(app).unwrap();
        }
        after
            .validate()
            .expect("the projected post-input read preserves exact source provenance");
        (before, after)
    }

    pub fn target() -> TargetSpec {
        TargetSpec {
            row: None,
            semantic_target: SemanticTargetRef::new("background-toggle").unwrap(),
            label: "Background toggle".into(),
            group: None,
            role_hint: Some("AXCheckBox".into()),
        }
    }

    pub fn pack() -> AppPack {
        AppPack::registered_native(APP).expect("guard fixture is registered")
    }
}

#[cfg(test)]
mod background_readback_tests {
    use super::background_readback_test_support as fixture;
    use super::*;
    use crate::environment::binding::{BindingReducer, background_readback_matches};
    use crate::environment::state_core::StateCore;
    use interaction_contracts::desktop::domain::ExecutionDomain;
    use interaction_contracts::desktop::source::{
        ObjectIdentityContinuity, StructureSnapshot, TargetContext,
    };

    fn prepare_before(
        snapshot: &StructureSnapshot,
        target: &TargetSpec,
    ) -> (StateCore, BindingReducer, AppPack, Binding) {
        let pack = fixture::pack();
        let mut state = StateCore::new();
        state.set_session(snapshot.session_epoch);
        state
            .admit(snapshot.clone(), snapshot.observed_ms, &pack)
            .unwrap();
        let env = state.latest().unwrap().clone();
        let mut bindings = BindingReducer::new();
        let binding = bindings
            .bind_background_ax(&env, target, snapshot.observed_ms, 2_000)
            .expect("the frozen background target binds in the replay");
        (state, bindings, pack, binding)
    }

    fn fresh_binding(
        state: &mut StateCore,
        bindings: &mut BindingReducer,
        pack: &AppPack,
        target: &TargetSpec,
        snapshot: &StructureSnapshot,
    ) -> Binding {
        state
            .admit(snapshot.clone(), snapshot.observed_ms, pack)
            .unwrap();
        let env = state.latest().unwrap().clone();
        bindings.revalidate(&env);
        bindings
            .bind_background_ax(&env, target, snapshot.observed_ms, 2_000)
            .expect("the current read independently derives a fresh binding")
    }

    fn start_background_press(
        run: &mut SkillRun,
        state: &StateCore,
        bindings: &mut BindingReducer,
        pack: &AppPack,
        now_ms: u64,
    ) {
        let step = run.advance(&mut SkillView {
            state,
            bindings,
            pack,
            now_ms,
            max_age_ms: 2_000,
        });
        let SkillStep::Act(plan) = step else {
            panic!("a known false boolean is changed by one AXPress: {step:?}");
        };
        assert_eq!(plan.path, ExecutionPath::Ax);
        assert_eq!(
            plan.primitive,
            Primitive::AxAction {
                action: AxActionKind::Press
            }
        );
    }

    #[test]
    fn known_completed_read_scoped_press_uses_fresh_semantics_without_replaying() {
        let (before, after) = fixture::snapshots();
        let target = fixture::target();
        let (mut state, mut bindings, pack, input_binding) = prepare_before(&before, &target);
        assert_eq!(
            input_binding.continuity.kind,
            ObjectIdentityContinuity::ReadScoped
        );
        let mut run = SkillRun::new(
            Skill::EnsureValue,
            target.semantic_target.clone(),
            Some(ExpectedValue::Bool { value: true }),
            input_binding.binding_ref.clone(),
        )
        .with_execution_domain(ExecutionDomain::BackgroundAx);

        start_background_press(&mut run, &state, &mut bindings, &pack, before.observed_ms);
        run.acted(ActionOutcome::Completed {
            end_ms: before.observed_ms + 2,
        });
        let current_binding = fresh_binding(&mut state, &mut bindings, &pack, &target, &after);
        assert_ne!(current_binding.binding_ref, input_binding.binding_ref);
        assert_ne!(current_binding.instance, input_binding.instance);
        assert!(
            !input_binding
                .continuity
                .same_closure(&current_binding.continuity)
        );
        assert!(background_readback_matches(
            &input_binding,
            &current_binding,
            &after,
            before.source_sequence,
        ));

        let result = run.advance(&mut SkillView {
            state: &state,
            bindings: &mut bindings,
            pack: &pack,
            now_ms: after.observed_ms,
            max_age_ms: 2_000,
        });
        assert!(matches!(result, SkillStep::Done(SkillResult::Succeeded)));
        assert_eq!(run.actions(), 1, "readback never submits a second input");
    }

    #[test]
    fn unknown_press_never_becomes_success_from_a_matching_new_read() {
        let (before, after) = fixture::snapshots();
        let target = fixture::target();
        let (mut state, mut bindings, pack, input_binding) = prepare_before(&before, &target);
        let mut run = SkillRun::new(
            Skill::EnsureValue,
            target.semantic_target.clone(),
            Some(ExpectedValue::Bool { value: true }),
            input_binding.binding_ref,
        )
        .with_execution_domain(ExecutionDomain::BackgroundAx);
        start_background_press(&mut run, &state, &mut bindings, &pack, before.observed_ms);
        run.acted(ActionOutcome::Unknown {
            at_ms: before.observed_ms + 2,
        });
        let _ = fresh_binding(&mut state, &mut bindings, &pack, &target, &after);
        let result = run.advance(&mut SkillView {
            state: &state,
            bindings: &mut bindings,
            pack: &pack,
            now_ms: after.observed_ms,
            max_age_ms: 2_000,
        });
        assert!(matches!(
            result,
            SkillStep::Done(SkillResult::Failed {
                code: FailureCode::EffectUnknown,
                ..
            })
        ));
        assert_eq!(run.actions(), 1);
    }

    #[test]
    fn background_readback_rejects_changed_scope_and_target_semantics() {
        let (before, after) = fixture::snapshots();
        let target = fixture::target();
        let (mut state, mut bindings, pack, input_binding) = prepare_before(&before, &target);
        let current_binding = fresh_binding(&mut state, &mut bindings, &pack, &target, &after);
        assert!(background_readback_matches(
            &input_binding,
            &current_binding,
            &after,
            before.source_sequence,
        ));

        let mut changed_watch = after.clone();
        if let Some(TargetContext::NativeWindow { app, source }) =
            changed_watch.target_context.as_mut()
        {
            source.window.source_session_id = "different-native-watch".into();
            source.target_context_hash = source.computed_target_context_hash(app).unwrap();
        }
        assert!(!background_readback_matches(
            &input_binding,
            &current_binding,
            &changed_watch,
            before.source_sequence,
        ));

        let mut changed_kernel = after.clone();
        if let Some(TargetContext::NativeWindow { app, source }) =
            changed_kernel.target_context.as_mut()
        {
            source.window.process.start_micros += 1;
            source.target_context_hash = source.computed_target_context_hash(app).unwrap();
        }
        assert!(!background_readback_matches(
            &input_binding,
            &current_binding,
            &changed_kernel,
            before.source_sequence,
        ));

        for change in [
            "label",
            "role",
            "group",
            "frame",
            "duplicate",
            "secure",
            "incomplete",
        ] {
            let mut changed = after.clone();
            let object = changed
                .objects
                .iter_mut()
                .find(|object| object.role == "AXCheckBox")
                .unwrap();
            match change {
                "label" => object.title = Some("Different control".into()),
                "role" => object.role = "AXButton".into(),
                "group" => object.group = Some("Different group".into()),
                "frame" => object.frame.x += 1.0,
                "duplicate" => {
                    let mut duplicate = object.clone();
                    duplicate.instance_ref =
                        interaction_contracts::desktop::ids::ObjectRef::new("duplicate-toggle")
                            .unwrap();
                    changed.objects.push(duplicate);
                    changed.observer_coverage =
                        interaction_contracts::desktop::source::ObserverCoverage::for_projected_objects(
                            &changed.objects,
                        );
                }
                "secure" => object.secure = true,
                "incomplete" => {
                    changed.complete = false;
                    changed.incomplete_reason = Some("truncated test read".into());
                }
                _ => unreachable!(),
            }
            assert!(
                !background_readback_matches(
                    &input_binding,
                    &current_binding,
                    &changed,
                    before.source_sequence,
                ),
                "changed readback context {change} must not stand in for the input target"
            );
        }
    }
}
