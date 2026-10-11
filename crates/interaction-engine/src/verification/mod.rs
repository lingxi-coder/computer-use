//! Verification: decides what the evidence proves (design 13.3 and 14.7).
//!
//! It evaluates the frozen success predicate over observations and nothing
//! else. A frame captured before the last input cannot confirm an effect of
//! that input, however late it arrives, and unknown never becomes success.
//! It can only say what it lacks; it has no way to act.

use std::collections::BTreeSet;

use interaction_contracts::desktop::binding::{
    BindingProofScope, derive_background_ax_binding_evidence, instance_fingerprint, normalize_label,
};
use interaction_contracts::desktop::browser::{
    BrowserCheckboxBinding, BrowserCheckboxWitness, BrowserPageProfile, BrowserPageSnapshot,
    BrowserReadScope, MAX_BROWSER_SAFE_INTEGER, browser_target_context_hash,
    derive_browser_checkbox_binding, derive_browser_checkbox_witness,
};
use interaction_contracts::desktop::ids::{AttemptId, RequestId, RootTaskId, SemanticTargetRef};
use interaction_contracts::desktop::owned_input::OwnedDocumentBindingProof;
use interaction_contracts::desktop::owned_registration::OwnedDocumentRegistration;
use interaction_contracts::desktop::source::{SourceWindowContext, TargetContext, UiValue};
use interaction_contracts::desktop::task::{
    ConfirmationScope, ExpectedValue, GoalSpec, Predicate, ScopedCommitBinding, ScopedCommitKind,
    ScopedCommitReceiptBinding,
};
use interaction_contracts::desktop::verification::{
    Causality, EffectResult, EffectStatus, EvidenceNeed, TimeSpan, TriState,
};
use rusqlite::{Connection, Transaction, params};

use crate::environment::binding::{Binding, BindingReducer, background_readback_matches};
use crate::environment::pack::{AppPack, ScopedControlContextKind, ScopedControlValueKind};
use crate::environment::read::{focused_window_id, object_for, values_equal};
use crate::environment::row::{ResolvedRowTarget, resolve_row_target, row_scope_coverage};
use crate::environment::state_core::{EnvSnapshot, StateCore};
use crate::infrastructure::storage::{Storage, StoreError, enum_to_str};
use crate::reaction::skills::{
    scoped_form_number_control, scoped_form_number_value_from_binding,
    scoped_radio_value_from_binding, scoped_revisit_controls,
};
use sha2::{Digest, Sha256};

/// Verify the exact Pack-registered Region code field and its Source-visible
/// positive validation feedback. This is read-only evidence: it grants no
/// input capability and deliberately rejects incomplete, duplicate, hidden,
/// secure, cross-window, or foreign-form feedback.
pub(crate) fn scoped_validation_feedback_valid(
    goal: &GoalSpec,
    pack: &AppPack,
    env: &EnvSnapshot,
    binding: &Binding,
) -> bool {
    if goal.validate().is_err()
        || goal.execution_domain
            != interaction_contracts::desktop::domain::ExecutionDomain::ForegroundNative
        || goal.scope.app != pack.bundle_id
        || env.snapshot.app.bundle_id != pack.bundle_id
        || !env.snapshot.complete
        || env.snapshot.menu_context.is_some()
        || env.snapshot.validate().is_err()
        || binding.semantic_target != goal.target.semantic_target
        || binding.identity_boundary != env.snapshot.identity_boundary
        || binding.continuity.validate().is_err()
        || binding.continuity.source_sequence > env.snapshot.source_sequence
    {
        return false;
    }
    let Some(scope) = goal
        .allowed_scope
        .as_ref()
        .filter(|scope| scope.targets.len() == 1 && scope.no_unrelated_controls)
    else {
        return false;
    };
    let Some(allowed) = scope.targets.first() else {
        return false;
    };
    let Some(feedback) = pack.scoped_validation_feedback(allowed) else {
        return false;
    };
    let Some(control) = pack.scoped_control(allowed) else {
        return false;
    };
    let (ExpectedValue::Text { value: desired }, ScopedControlValueKind::Text) =
        (&goal.desired, control.value_kind)
    else {
        return false;
    };
    if control.context_kind != ScopedControlContextKind::Form
        || control.context_label != Some(feedback.context_label)
        || control.control.semantic != feedback.control_semantic
        || control.control.role != "AXTextField"
        || goal.target.semantic_target.as_str() != feedback.control_semantic
        || goal.target.label != control.control.label
        || goal.target.role_hint.as_deref() != Some(control.control.role.as_str())
        || goal.target.group.as_deref() != Some(feedback.context_label)
    {
        return false;
    }
    let Some(pair) = goal.scoped_revisit.as_ref() else {
        return false;
    };
    let Some(revisit_controls) =
        scoped_revisit_controls(&goal.target.semantic_target, pair, pack, env)
    else {
        return false;
    };
    let Some(active_window) = focused_window_id(&env.snapshot) else {
        return false;
    };
    if env.page.as_deref() != Some(revisit_controls.home_page.as_str())
        || !matches!(
            revisit_controls.home.value.as_ref(),
            Some(UiValue::Bool(true))
        )
    {
        return false;
    }
    let fields: Vec<_> = env
        .snapshot
        .objects
        .iter()
        .filter(|object| {
            object.window_id == active_window
                && object.role == control.control.role
                && object.group.as_deref() == Some(feedback.context_label)
                && object.in_view
                && !object.secure
                && object
                    .title
                    .as_deref()
                    .or(object.description.as_deref())
                    .is_some_and(|label| {
                        normalize_label(label) == normalize_label(&control.control.label)
                    })
        })
        .collect();
    let [field] = fields.as_slice() else {
        return false;
    };
    if field.instance_ref != binding.instance
        || !matches!(binding.rule.as_str(), "instance" | "unique")
        || !binding.device_context.supports_native_input()
        || instance_fingerprint(&env.snapshot, field).digest() != binding.fingerprint_digest
        || binding.continuity.scope != BindingProofScope::WholeStructure
        || !field
            .value
            .as_ref()
            .is_some_and(|value| matches!(value, UiValue::Text(value) if value == desired))
    {
        return false;
    }
    let mut matching_forms = env.snapshot.objects.iter().filter(|object| {
        object.role == "AXGroup"
            && object.window_id == active_window
            && object.in_view
            && !object.secure
            && object.title.as_deref().or(object.description.as_deref())
                == Some(feedback.context_label)
    });
    let Some(form) = matching_forms.next() else {
        return false;
    };
    if matching_forms.next().is_some()
        || !has_ancestor_in_same_window(&env.snapshot, field, &form.instance_ref, active_window)
    {
        return false;
    }
    let mut matching_feedback = env
        .snapshot
        .objects
        .iter()
        .filter(|object| object.description.as_deref() == Some(feedback.description));
    let Some(message) = matching_feedback.next() else {
        return false;
    };
    if matching_feedback.next().is_some()
        || message.role != feedback.role
        || message.window_id != active_window
        || message.group.as_deref() != Some(feedback.context_label)
        || !message.in_view
        || message.secure
        || !has_ancestor_in_same_window(&env.snapshot, message, &form.instance_ref, active_window)
        || message.value != Some(UiValue::Text(feedback.valid_text.to_owned()))
    {
        return false;
    }
    true
}

const MAX_VALIDATION_FORM_ANCESTRY_DEPTH: usize = 64;

fn has_ancestor_in_same_window(
    snapshot: &interaction_contracts::desktop::source::StructureSnapshot,
    child: &interaction_contracts::desktop::source::UiObject,
    expected_ancestor: &interaction_contracts::desktop::ids::ObjectRef,
    window_id: &str,
) -> bool {
    let mut current = child.parent.as_ref();
    let mut visited = BTreeSet::new();
    for _ in 0..MAX_VALIDATION_FORM_ANCESTRY_DEPTH {
        let Some(reference) = current else {
            return false;
        };
        if !visited.insert(reference.as_str().to_owned()) {
            return false;
        }
        let mut matches = snapshot
            .objects
            .iter()
            .filter(|object| &object.instance_ref == reference);
        let Some(parent) = matches.next() else {
            return false;
        };
        if matches.next().is_some() || parent.window_id != window_id {
            return false;
        }
        if &parent.instance_ref == expected_ancestor {
            return true;
        }
        current = parent.parent.as_ref();
    }
    false
}

/// Tracks whether the page holding the target was left and entered again,
/// and the target bound afresh afterwards.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PageVisits {
    target_page: Option<String>,
    armed_after_ms: Option<u64>,
    left_at: Option<u64>,
    returned_at: Option<u64>,
    rebound: bool,
}

impl PageVisits {
    pub fn set_target_page(&mut self, page: Option<String>) {
        if self.target_page.is_none() {
            self.target_page = page;
        }
    }

    /// The change itself is done: from now on leaving the page counts.
    pub fn arm(&mut self, after_ms: u64) {
        if self.armed_after_ms.is_none() {
            self.armed_after_ms = Some(after_ms);
        }
    }

    pub fn observe(&mut self, page: Option<&str>, observed_ms: u64) {
        let Some(armed) = self.armed_after_ms else {
            return;
        };
        if observed_ms < armed {
            return;
        }
        let on_target = page.is_some() && page == self.target_page.as_deref();
        if self.left_at.is_none() {
            if page.is_some() && !on_target {
                self.left_at = Some(observed_ms);
            }
        } else if self.returned_at.is_none() && on_target {
            self.returned_at = Some(observed_ms);
        }
    }

    pub fn note_bound(&mut self, bound_at_ms: u64, page: Option<&str>) {
        if let Some(returned) = self.returned_at
            && bound_at_ms >= returned
            && page == self.target_page.as_deref()
        {
            self.rebound = true;
        }
    }

    pub fn is_armed(&self) -> bool {
        self.armed_after_ms.is_some()
    }

    pub fn left(&self) -> bool {
        self.left_at.is_some()
    }

    pub fn returned(&self) -> bool {
        self.returned_at.is_some()
    }

    pub fn satisfied(&self) -> bool {
        self.left_at.is_some() && self.returned_at.is_some() && self.rebound
    }

    pub fn target_page(&self) -> Option<&str> {
        self.target_page.as_deref()
    }
}

/// What the evaluation may read. Verification owns none of it.
pub struct EvidenceContext<'a> {
    pub state: &'a StateCore,
    pub bindings: &'a BindingReducer,
    pub pack: &'a AppPack,
    pub goal: &'a GoalSpec,
    pub now_ms: u64,
    pub max_age_ms: u64,
}

#[derive(Debug, Clone)]
struct BackgroundActionInput {
    binding: Binding,
    source_sequence: u64,
    target_context_hash: String,
    end_ms: u64,
}

/// The trusted boundary after which the readback must begin. `Submitted`
/// carries only a real input span from a matching receipt; `NoInputNeeded`
/// records the completed already-satisfied decision without inventing input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BrowserCurrentUiFence {
    Submitted { input_span: TimeSpan },
    NoInputNeeded { fence_ms: u64 },
}

/// Facts captured by Engine for one frozen browser goal and its initial read.
/// This is kept separate from native BackgroundAX action evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BrowserCurrentUiInput {
    pub root_task_id: RootTaskId,
    pub task_revision: u64,
    pub root_epoch: u64,
    pub attempt_id: AttemptId,
    pub intent_id: Option<String>,
    pub session_epoch: u64,
    pub browser_page_profile: BrowserPageProfile,
    pub goal_app: String,
    pub scope: BrowserReadScope,
    pub original_binding: BrowserCheckboxBinding,
    pub expected_checked: bool,
    pub fence: BrowserCurrentUiFence,
    pub deadline_ms: u64,
}

/// Current trusted Root/Attempt identity and the exact later Source read to
/// check against the registered input fence.
pub(crate) struct BrowserCurrentUiReadback<'a> {
    pub root_task_id: &'a RootTaskId,
    pub task_revision: u64,
    pub root_epoch: u64,
    pub attempt_id: &'a AttemptId,
    pub intent_id: Option<&'a str>,
    pub session_epoch: u64,
    pub browser_page_profile: BrowserPageProfile,
    pub goal_app: &'a str,
    pub scope: &'a BrowserReadScope,
    pub snapshot: &'a BrowserPageSnapshot,
    pub binding: &'a BrowserCheckboxBinding,
    pub now_ms: u64,
    pub max_age_ms: u64,
}

/// Closed rejection reasons for the browser CurrentUi proof path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BrowserCurrentUiRejection {
    InvalidInput,
    InvalidProfile,
    InvalidScope,
    InvalidBinding,
    InputBeforeBinding,
    InvalidFence,
    ConflictingAttempt,
    InputNotRegistered,
    AttemptMismatch,
    ScopeMismatch,
    SnapshotInvalid,
    SnapshotBeforeFence,
    SnapshotNotFresh,
    SnapshotExpired,
    BindingMismatch,
    WitnessInvalid,
    ValueMismatch,
}

impl BrowserCurrentUiRejection {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::InvalidProfile => "invalid_profile",
            Self::InvalidScope => "invalid_scope",
            Self::InvalidBinding => "invalid_binding",
            Self::InputBeforeBinding => "input_before_binding",
            Self::InvalidFence => "invalid_fence",
            Self::ConflictingAttempt => "conflicting_attempt",
            Self::InputNotRegistered => "input_not_registered",
            Self::AttemptMismatch => "attempt_mismatch",
            Self::ScopeMismatch => "scope_mismatch",
            Self::SnapshotInvalid => "snapshot_invalid",
            Self::SnapshotBeforeFence => "snapshot_before_fence",
            Self::SnapshotNotFresh => "snapshot_not_fresh",
            Self::SnapshotExpired => "snapshot_expired",
            Self::BindingMismatch => "binding_mismatch",
            Self::WitnessInvalid => "witness_invalid",
            Self::ValueMismatch => "value_mismatch",
        }
    }
}

/// A successful readback proof retains the checked witness while carrying the
/// durable effect row through the existing CurrentUi persistence path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BrowserCurrentUiProof {
    result: EffectResult,
    witness: BrowserCheckboxWitness,
}

impl BrowserCurrentUiProof {
    pub(crate) fn into_effect_result(self) -> EffectResult {
        let Self { result, witness: _ } = self;
        result
    }

    #[cfg(test)]
    pub(crate) fn witness(&self) -> &BrowserCheckboxWitness {
        &self.witness
    }
}

fn browser_fence_ms(input: &BrowserCurrentUiInput) -> u64 {
    match input.fence {
        BrowserCurrentUiFence::Submitted { input_span } => input_span.to_ms,
        BrowserCurrentUiFence::NoInputNeeded { fence_ms } => fence_ms,
    }
}

fn validate_browser_current_ui_input(
    input: &BrowserCurrentUiInput,
) -> Result<(), BrowserCurrentUiRejection> {
    if input.task_revision == 0
        || input.root_epoch == 0
        || input.session_epoch == 0
        || input.deadline_ms > MAX_BROWSER_SAFE_INTEGER
        || input.session_epoch > MAX_BROWSER_SAFE_INTEGER
        || input.goal_app != input.browser_page_profile.app()
        || input.intent_id.as_deref().is_some_and(|id| {
            id.trim().is_empty() || id.len() > 256 || id.chars().any(char::is_control)
        })
    {
        return Err(BrowserCurrentUiRejection::InvalidInput);
    }
    input
        .scope
        .validate()
        .map_err(|_| BrowserCurrentUiRejection::InvalidScope)?;
    if input.scope.context.session_epoch != input.session_epoch
        || input.scope.clock_domain != input.original_binding.clock_domain
    {
        return Err(BrowserCurrentUiRejection::InvalidScope);
    }
    input
        .original_binding
        .validate()
        .map_err(|_| BrowserCurrentUiRejection::InvalidBinding)?;
    let expected_hash = browser_target_context_hash(&input.scope.context)
        .map_err(|_| BrowserCurrentUiRejection::InvalidScope)?;
    if input.original_binding.context != input.scope.context
        || input.original_binding.target_context_hash != expected_hash
        || input.original_binding.role != input.browser_page_profile.role()
        || input.original_binding.name != input.browser_page_profile.label()
        || input.scope.issued_ms > input.original_binding.observed_ms
        || input.scope.issued_ms > input.original_binding.read_finished_ms
    {
        return Err(BrowserCurrentUiRejection::InvalidBinding);
    }
    let fence_ms = browser_fence_ms(input);
    if input.deadline_ms <= fence_ms || input.deadline_ms <= input.original_binding.read_finished_ms
    {
        return Err(BrowserCurrentUiRejection::InvalidFence);
    }
    match input.fence {
        BrowserCurrentUiFence::Submitted { input_span } => {
            if input.intent_id.as_deref().is_none_or(str::is_empty)
                || input.expected_checked == input.original_binding.checked
                || input_span.from_ms > MAX_BROWSER_SAFE_INTEGER
                || input_span.to_ms > MAX_BROWSER_SAFE_INTEGER
                || input_span.from_ms > input_span.to_ms
                || input_span.from_ms < input.original_binding.read_finished_ms
                || input_span.to_ms >= input.deadline_ms
            {
                return Err(BrowserCurrentUiRejection::InputBeforeBinding);
            }
        }
        BrowserCurrentUiFence::NoInputNeeded { fence_ms } => {
            if input.intent_id.is_some()
                || input.expected_checked != input.original_binding.checked
                || fence_ms > MAX_BROWSER_SAFE_INTEGER
                || fence_ms < input.original_binding.read_finished_ms
            {
                return Err(BrowserCurrentUiRejection::InvalidFence);
            }
        }
    }
    Ok(())
}

fn browser_effect_result_id(attempt_id: &AttemptId, snapshot_id: &str) -> RequestId {
    let mut digest = Sha256::new();
    digest.update(attempt_id.as_str().as_bytes());
    digest.update([0]);
    digest.update(snapshot_id.as_bytes());
    RequestId::new(format!("browser-effect-{:x}", digest.finalize()))
        .expect("the fixed hash-derived effect id is bounded and valid")
}

#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub status: TriState,
    /// The snapshots the conclusion rests on.
    pub evidence: Vec<RequestId>,
    /// What is missing, for the parts that are not true yet.
    pub needs: Vec<EvidenceNeed>,
}

/// Result of reading the exact Source-bound persistence receipt for a scoped
/// native goal. Pending is only an automatic-save state; ambiguity is Unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScopedCommitReceiptStatus {
    Confirmed,
    Pending,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScopedCommitReceiptRead {
    revision: Option<u64>,
    pending: bool,
    matched_text: bool,
}

fn scoped_commit_revision(text: &str, prefix: &str) -> Option<u64> {
    let suffix = text.strip_prefix(prefix)?;
    if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let revision = suffix.parse::<u64>().ok()?;
    (revision > 0).then_some(revision)
}

fn scoped_commit_receipt_text<'a>(
    objects: &'a [interaction_contracts::desktop::source::UiObject],
    active_window: &str,
    receipt: &ScopedCommitReceiptBinding,
) -> Option<&'a str> {
    let matching: Vec<_> = objects
        .iter()
        .filter(|object| {
            object.window_id == active_window
                && object.role == receipt.role_hint
                && object.description.as_deref() == Some(receipt.description.as_str())
                && object.group.as_deref() == Some(receipt.group.as_str())
        })
        .collect();
    let [object] = matching.as_slice() else {
        return None;
    };
    if !object.in_view || object.secure {
        return None;
    }
    match object.value.as_ref() {
        Some(UiValue::Text(text)) => Some(text),
        _ => None,
    }
}

fn scoped_commit_revision_advanced(revision: u64, baseline: Option<Option<u64>>) -> bool {
    baseline.is_some_and(|baseline| baseline.is_none_or(|previous| revision > previous))
}

fn interpret_scoped_commit_receipt(
    commit: &ScopedCommitBinding,
    text: &str,
) -> (ScopedCommitReceiptStatus, ScopedCommitReceiptRead) {
    let revision = scoped_commit_revision(text, &commit.receipt.text_prefix);
    let is_chinese = commit.receipt.text_prefix.starts_with("已");
    let pending_text = if is_chinese {
        "正在保存更改…"
    } else {
        "Saving changes…"
    };
    let pending = commit.receipt.kind == ScopedCommitKind::Automatic && text == pending_text;
    let matched_text = revision.is_some()
        || pending
        || match commit.receipt.kind {
            ScopedCommitKind::Manual if is_chinese => text == "尚无已保存更改",
            ScopedCommitKind::Manual => text == "No saved change yet",
            ScopedCommitKind::Automatic if is_chinese => text == "所有更改均已保存",
            ScopedCommitKind::Automatic => text == "All changes saved",
        };
    let status = if revision.is_some() {
        ScopedCommitReceiptStatus::Confirmed
    } else if pending {
        ScopedCommitReceiptStatus::Pending
    } else {
        ScopedCommitReceiptStatus::Unknown
    };
    (
        status,
        ScopedCommitReceiptRead {
            revision,
            pending,
            matched_text,
        },
    )
}

fn scoped_commit_receipt_read(
    ctx: &EvidenceContext<'_>,
    after_ms: Option<u64>,
) -> (
    ScopedCommitReceiptStatus,
    Option<ScopedCommitReceiptRead>,
    Option<RequestId>,
) {
    let Some(commit) = ctx.goal.scoped_commit.as_ref() else {
        return (ScopedCommitReceiptStatus::Unknown, None, None);
    };
    if ctx.goal.validate().is_err() || !ctx.state.usable(ctx.now_ms, ctx.max_age_ms) {
        return (ScopedCommitReceiptStatus::Unknown, None, None);
    }
    let env = match after_ms {
        Some(after) => after
            .checked_add(1)
            .and_then(|floor| ctx.state.latest_since(floor)),
        None => ctx.state.latest(),
    };
    let Some(env) = env else {
        return (ScopedCommitReceiptStatus::Unknown, None, None);
    };
    let snapshot = &env.snapshot;
    let evidence = Some(snapshot.snapshot_id.clone());
    let Some(active_window) = focused_window_id(snapshot) else {
        return (ScopedCommitReceiptStatus::Unknown, None, evidence);
    };
    if snapshot.validate().is_err()
        || !snapshot.complete
        || snapshot.menu_context.is_some()
        || snapshot.app.bundle_id != ctx.goal.scope.app
        || snapshot.app.bundle_id != ctx.pack.bundle_id
        || snapshot.observer_coverage.state
            == interaction_contracts::desktop::source::SourceState::Unavailable
        || snapshot.observed_ms > ctx.now_ms
        || ctx.now_ms.saturating_sub(snapshot.observed_ms) > ctx.max_age_ms
        || after_ms.is_some_and(|after| snapshot.observed_ms <= after)
        || snapshot.windows.iter().any(|window| window.modal)
    {
        return (ScopedCommitReceiptStatus::Unknown, None, evidence);
    }
    let Some(text) = scoped_commit_receipt_text(&snapshot.objects, active_window, &commit.receipt)
    else {
        return (ScopedCommitReceiptStatus::Unknown, None, evidence);
    };
    let (status, receipt_read) = interpret_scoped_commit_receipt(commit, text);
    (status, Some(receipt_read), evidence)
}

#[derive(Debug, Default)]
pub struct Verification {
    unknown_input_floor_ms: Option<u64>,
    pub visits: PageVisits,
    first_input_ms: Option<u64>,
    last_input_end_ms: Option<u64>,
    frozen_ms: u64,
    baseline: Option<TriState>,
    revision: u64,
    last: Option<(EffectStatus, Vec<RequestId>)>,
    need_seq: u64,
    background_action_input: Option<BackgroundActionInput>,
    browser_current_ui_input: Option<BrowserCurrentUiInput>,
    /// A Source-observed receipt revision before any goal input. A confirmed
    /// post-input receipt must advance beyond this baseline.
    scoped_commit_baseline_revision: Option<Option<u64>>,
}

impl Verification {
    pub fn new(frozen_ms: u64) -> Self {
        Self {
            frozen_ms,
            ..Self::default()
        }
    }

    /// After a restart the conclusions already stored stay as they are; new
    /// ones continue the numbering, because history is never rewritten.
    pub fn resume_after(&mut self, stored_revision: u64) {
        self.revision = self.revision.max(stored_revision);
    }

    pub fn last_input_end_ms(&self) -> Option<u64> {
        self.last_input_end_ms
    }

    pub fn input_span(&self) -> Option<TimeSpan> {
        self.first_input_ms
            .zip(self.last_input_end_ms)
            .map(|(from_ms, to_ms)| TimeSpan { from_ms, to_ms })
    }

    /// Input reached the application between these times.
    pub fn record_input(&mut self, from_ms: u64, to_ms: u64) {
        self.first_input_ms.get_or_insert(from_ms);
        self.last_input_end_ms = Some(self.last_input_end_ms.map_or(to_ms, |end| end.max(to_ms)));
    }
    pub fn record_unknown_input(&mut self, at_ms: u64) {
        self.unknown_input_floor_ms = Some(self.unknown_input_floor_ms.unwrap_or(0).max(at_ms));
    }

    pub fn record_background_action_input(
        &mut self,
        binding: Binding,
        source_sequence: u64,
        target_context_hash: String,
        end_ms: u64,
    ) {
        if self.background_action_input.is_none() {
            self.background_action_input = Some(BackgroundActionInput {
                binding,
                source_sequence,
                target_context_hash,
                end_ms,
            });
        }
    }

    /// Register the real input receipt or explicit no-input decision that
    /// fences a later, independent browser readback.
    pub(crate) fn register_browser_current_ui_input(
        &mut self,
        input: BrowserCurrentUiInput,
    ) -> Result<(), BrowserCurrentUiRejection> {
        validate_browser_current_ui_input(&input)?;
        if let Some(current) = &self.browser_current_ui_input {
            if current == &input {
                return Ok(());
            }
            if current.root_task_id != input.root_task_id
                || current.attempt_id == input.attempt_id
                || current.root_epoch > input.root_epoch
                || current.task_revision > input.task_revision
                || current.scope.generation > input.scope.generation
                || current.original_binding.source_sequence > input.original_binding.source_sequence
                || browser_fence_ms(current) > browser_fence_ms(&input)
                || (current.root_epoch == input.root_epoch
                    && current.task_revision == input.task_revision
                    && current.scope.generation == input.scope.generation
                    && current.original_binding.source_sequence
                        == input.original_binding.source_sequence
                    && browser_fence_ms(current) == browser_fence_ms(&input))
            {
                return Err(BrowserCurrentUiRejection::ConflictingAttempt);
            }
        }
        self.browser_current_ui_input = Some(input);
        Ok(())
    }

    /// Verify one later complete Source read against the registered browser
    /// input fence. This never consults native EnvSnapshot or native bindings.
    pub(crate) fn verify_browser_current_ui(
        &self,
        readback: BrowserCurrentUiReadback<'_>,
    ) -> Result<BrowserCurrentUiProof, BrowserCurrentUiRejection> {
        let input = self
            .browser_current_ui_input
            .as_ref()
            .ok_or(BrowserCurrentUiRejection::InputNotRegistered)?;
        validate_browser_current_ui_input(input)?;
        if readback.root_task_id != &input.root_task_id
            || readback.task_revision != input.task_revision
            || readback.root_epoch != input.root_epoch
            || readback.attempt_id != &input.attempt_id
            || readback.intent_id != input.intent_id.as_deref()
            || readback.session_epoch != input.session_epoch
            || readback.browser_page_profile != input.browser_page_profile
            || readback.goal_app != input.goal_app.as_str()
            || readback.goal_app != input.browser_page_profile.app()
        {
            return Err(BrowserCurrentUiRejection::AttemptMismatch);
        }
        if readback.scope != &input.scope {
            return Err(BrowserCurrentUiRejection::ScopeMismatch);
        }
        readback
            .scope
            .validate()
            .map_err(|_| BrowserCurrentUiRejection::InvalidScope)?;
        if readback.session_epoch != readback.scope.context.session_epoch {
            return Err(BrowserCurrentUiRejection::InvalidScope);
        }
        readback
            .snapshot
            .validate()
            .map_err(|_| BrowserCurrentUiRejection::SnapshotInvalid)?;
        if readback.now_ms > MAX_BROWSER_SAFE_INTEGER
            || readback.snapshot.observed_ms > readback.now_ms
            || readback.snapshot.read_finished_ms > readback.now_ms
        {
            return Err(BrowserCurrentUiRejection::SnapshotNotFresh);
        }
        if readback.now_ms >= input.deadline_ms
            || readback.snapshot.observed_ms >= input.deadline_ms
            || readback.snapshot.read_finished_ms >= input.deadline_ms
        {
            return Err(BrowserCurrentUiRejection::SnapshotExpired);
        }
        let max_age_ms = readback.max_age_ms.min(2_000);
        if max_age_ms == 0
            || readback
                .now_ms
                .saturating_sub(readback.snapshot.observed_ms)
                >= max_age_ms
        {
            return Err(BrowserCurrentUiRejection::SnapshotExpired);
        }
        if readback.snapshot.observed_ms <= browser_fence_ms(input)
            || readback.snapshot.source_sequence <= input.original_binding.source_sequence
        {
            return Err(BrowserCurrentUiRejection::SnapshotBeforeFence);
        }
        let expected_hash = browser_target_context_hash(&input.scope.context)
            .map_err(|_| BrowserCurrentUiRejection::InvalidScope)?;
        if readback.scope.issued_ms > readback.snapshot.observed_ms
            || readback.snapshot.context != input.scope.context
            || readback.snapshot.clock_domain != input.scope.clock_domain
            || readback.snapshot.target_context_hash != expected_hash
            || readback.snapshot.context.session_epoch != input.session_epoch
        {
            return Err(BrowserCurrentUiRejection::ScopeMismatch);
        }
        let derived_binding =
            derive_browser_checkbox_binding(readback.snapshot, input.browser_page_profile.label())
                .map_err(|_| BrowserCurrentUiRejection::BindingMismatch)?;
        if &derived_binding != readback.binding
            || readback.binding.role != input.browser_page_profile.role()
            || readback.binding.name != input.browser_page_profile.label()
            || readback.binding.context != input.scope.context
            || readback.binding.clock_domain != input.scope.clock_domain
            || readback.binding.target_context_hash != expected_hash
            || readback.binding.backend_dom_node_id != input.original_binding.backend_dom_node_id
            || readback.binding.ax_node_id != input.original_binding.ax_node_id
        {
            return Err(BrowserCurrentUiRejection::BindingMismatch);
        }
        let witness = derive_browser_checkbox_witness(&input.original_binding, readback.snapshot)
            .map_err(|_| BrowserCurrentUiRejection::WitnessInvalid)?;
        witness
            .validate()
            .map_err(|_| BrowserCurrentUiRejection::WitnessInvalid)?;
        if witness.observed_checked != input.expected_checked {
            return Err(BrowserCurrentUiRejection::ValueMismatch);
        }
        let input_span = match input.fence {
            BrowserCurrentUiFence::Submitted { input_span } => Some(input_span),
            BrowserCurrentUiFence::NoInputNeeded { .. } => None,
        };
        let result_id = browser_effect_result_id(&input.attempt_id, &readback.snapshot.snapshot_id);
        let evidence_id = RequestId::new(readback.snapshot.snapshot_id.clone())
            .map_err(|_| BrowserCurrentUiRejection::SnapshotInvalid)?;
        let target = SemanticTargetRef::new(input.browser_page_profile.semantic_target())
            .map_err(|_| BrowserCurrentUiRejection::InvalidProfile)?;
        Ok(BrowserCurrentUiProof {
            result: EffectResult {
                result_id,
                revision: 1,
                root_task_id: input.root_task_id.clone(),
                attempt_id: Some(input.attempt_id.clone()),
                intent_id: input.intent_id.clone(),
                predicate: "browser.checkbox_state@1".into(),
                target: Some(target),
                status: EffectStatus::Satisfied,
                evidence_group: vec![evidence_id],
                input_span,
                observed_ms: readback.snapshot.observed_ms,
                confirmation_scope:
                    interaction_contracts::desktop::task::ConfirmationScope::CurrentUi,
                causality: match input.fence {
                    BrowserCurrentUiFence::Submitted { .. } => Causality::Unknown,
                    BrowserCurrentUiFence::NoInputNeeded { .. } => Causality::NotApplicable,
                },
            },
            witness,
        })
    }

    /// Whether the goal already held before anything was done. It decides
    /// whether a later satisfied goal can be attributed to our own input.
    pub fn note_baseline(&mut self, value: TriState) {
        if self.baseline.is_none() {
            self.baseline = Some(value);
        }
    }

    pub(crate) fn capture_scoped_commit_baseline(&mut self, ctx: &EvidenceContext<'_>) {
        if self.scoped_commit_baseline_revision.is_some() || ctx.goal.scoped_commit.is_none() {
            return;
        }
        let (_, receipt, _) = scoped_commit_receipt_read(ctx, None);
        let Some(receipt) = receipt.filter(|receipt| receipt.matched_text) else {
            return;
        };
        if receipt.pending {
            return;
        }
        self.scoped_commit_baseline_revision = Some(receipt.revision);
    }

    pub(crate) fn scoped_commit_receipt_status(
        &self,
        ctx: &EvidenceContext<'_>,
    ) -> ScopedCommitReceiptStatus {
        let Some(after) = self.last_input_end_ms else {
            return ScopedCommitReceiptStatus::Unknown;
        };
        let (status, receipt, _) = scoped_commit_receipt_read(ctx, Some(self.evidence_floor_ms()));
        let Some(receipt) = receipt else {
            return ScopedCommitReceiptStatus::Unknown;
        };
        if receipt.pending {
            return ScopedCommitReceiptStatus::Pending;
        }
        let Some(revision) = receipt.revision else {
            return ScopedCommitReceiptStatus::Unknown;
        };
        if ctx
            .state
            .latest()
            .is_none_or(|env| env.snapshot.observed_ms <= after)
            || !scoped_commit_revision_advanced(revision, self.scoped_commit_baseline_revision)
        {
            return ScopedCommitReceiptStatus::Unknown;
        }
        if status == ScopedCommitReceiptStatus::Confirmed {
            ScopedCommitReceiptStatus::Confirmed
        } else {
            ScopedCommitReceiptStatus::Unknown
        }
    }

    /// Nothing observed earlier than this can confirm an effect.
    pub fn evidence_floor_ms(&self) -> u64 {
        self.last_input_end_ms
            .unwrap_or(self.frozen_ms)
            .max(self.unknown_input_floor_ms.unwrap_or(0))
    }

    fn evidence_for<'a>(
        &self,
        ctx: &'a EvidenceContext<'_>,
        after_action: bool,
    ) -> Option<&'a EnvSnapshot> {
        if after_action {
            ctx.state.latest_since(self.evidence_floor_ms())
        } else {
            ctx.state.latest()
        }
    }

    /// The current value of the bound target in `env`, read from evidence.
    fn read_value(
        ctx: &EvidenceContext<'_>,
        env: &EnvSnapshot,
        target: &SemanticTargetRef,
    ) -> Option<UiValue> {
        let binding = ctx.bindings.current(target)?;
        let is_registered_radio_group = target == &ctx.goal.target.semantic_target
            && ctx
                .goal
                .allowed_scope
                .as_ref()
                .and_then(|scope| scope.targets.first())
                .and_then(|allowed| ctx.pack.scoped_control(allowed))
                .is_some_and(|scoped| {
                    matches!(
                        scoped.value_kind,
                        crate::environment::pack::ScopedControlValueKind::RadioChoice(_)
                    )
                });
        if is_registered_radio_group {
            return scoped_radio_value_from_binding(ctx.goal, ctx.pack, env, binding);
        }
        if target == &ctx.goal.target.semantic_target
            && scoped_form_number_control(ctx.goal, ctx.pack).is_some()
        {
            return scoped_form_number_value_from_binding(ctx.goal, ctx.pack, env, binding);
        }
        if target == &ctx.goal.target.semantic_target
            && ctx
                .goal
                .allowed_scope
                .as_ref()
                .and_then(|scope| scope.targets.first())
                .and_then(|allowed| ctx.pack.scoped_validation_feedback(allowed))
                .is_some()
        {
            let current = ctx.state.latest()?;
            if !ctx.state.usable(ctx.now_ms, ctx.max_age_ms)
                || current.snapshot.snapshot_id != env.snapshot.snapshot_id
                || !scoped_validation_feedback_valid(ctx.goal, ctx.pack, env, binding)
            {
                return None;
            }
        }
        object_for(env, &binding.fingerprint_digest).and_then(|object| object.value.clone())
    }

    fn background_target_value(
        &self,
        ctx: &EvidenceContext<'_>,
        env: &EnvSnapshot,
        target: &SemanticTargetRef,
        observed_after_action: bool,
    ) -> Option<UiValue> {
        if target.as_str() != ctx.goal.target.semantic_target.as_str()
            || ctx.goal.execution_domain
                != interaction_contracts::desktop::domain::ExecutionDomain::BackgroundAx
        {
            return None;
        }
        let binding = ctx.bindings.current(target)?;
        if binding.rule != "background_ax"
            || binding.semantic_target.as_str() != target.as_str()
            || binding.identity_boundary != env.snapshot.identity_boundary
            || !env.snapshot.complete
            || env.snapshot.menu_context.is_some()
        {
            return None;
        }
        let BindingProofScope::BackgroundAx {
            target_context_hash: binding_hash,
        } = &binding.continuity.scope
        else {
            return None;
        };
        let TargetContext::NativeWindow { app, source } = env.snapshot.target_context.as_ref()?
        else {
            return None;
        };
        if app.bundle_id != ctx.goal.scope.app
            || app != &env.snapshot.app
            || source.target_context_hash.as_str() != binding_hash.as_str()
            || !source.is_visible_standard_window()
            || !ctx.state.background_ax_usable(
                (&app.bundle_id, app.pid),
                &source.window.origin_window_id,
                Some((
                    source.window.process.start_seconds,
                    source.window.process.start_micros,
                )),
                Some(source.target_context_hash.as_str()),
                ctx.now_ms,
                ctx.max_age_ms.min(2_000),
            )
        {
            return None;
        }
        let object = env
            .snapshot
            .objects
            .iter()
            .find(|object| object.instance_ref == binding.instance)?;
        if instance_fingerprint(&env.snapshot, object).digest() != binding.fingerprint_digest {
            return None;
        }
        let current = derive_background_ax_binding_evidence(&env.snapshot, object).ok()?;
        if !current.device_context.supports_background_ax_press()
            || !binding.continuity.same_closure(&current.continuity)
            || binding.continuity.source_sequence > current.continuity.source_sequence
        {
            return None;
        }
        if observed_after_action {
            let input = self.background_action_input.as_ref()?;
            if input.binding.rule != "background_ax"
                || input.binding.semantic_target.as_str() != target.as_str()
                || input.target_context_hash.as_str() != binding_hash.as_str()
                || input.end_ms >= env.snapshot.observed_ms
                || !background_readback_matches(
                    &input.binding,
                    binding,
                    &env.snapshot,
                    input.source_sequence,
                )
            {
                return None;
            }
        }
        object.value.clone()
    }

    fn row_selected(
        &self,
        ctx: &EvidenceContext<'_>,
        target: &SemanticTargetRef,
        observed_after_action: bool,
    ) -> (TriState, Option<RequestId>) {
        if target != &ctx.goal.target.semantic_target
            || ctx.goal.target.row.is_none()
            || ctx.goal.confirmation_scope != ConfirmationScope::UiRevisit
            || !matches!(&ctx.goal.desired, ExpectedValue::Bool { value: true })
        {
            return (TriState::Unknown, None);
        }
        if ctx.goal.scope.app != ctx.pack.bundle_id {
            return (TriState::Unknown, None);
        }
        let Some(row_spec) = ctx.goal.target.row.as_ref() else {
            return (TriState::Unknown, None);
        };
        if ctx
            .pack
            .row_selection_effect_class(target, row_spec, ctx.goal.target.label.as_str())
            .is_none()
        {
            return (TriState::Unknown, None);
        }
        let latest = ctx.state.latest();
        let evidence_id = latest.map(|env| env.snapshot.snapshot_id.clone());
        if !ctx
            .state
            .row_scope_usable(&row_spec.table_scope, ctx.now_ms, ctx.max_age_ms)
        {
            return (TriState::Unknown, evidence_id);
        }

        let env = if observed_after_action {
            let Some(floor) = self.evidence_floor_ms().checked_add(1) else {
                return (TriState::Unknown, evidence_id);
            };
            ctx.state.latest_since(floor)
        } else {
            latest
        };
        let Some(env) = env else {
            return (TriState::Unknown, evidence_id);
        };
        let snapshot = &env.snapshot;
        if snapshot.app.bundle_id != ctx.goal.scope.app
            || snapshot.app.bundle_id != ctx.pack.bundle_id
            || snapshot.observed_ms > ctx.now_ms
            || ctx.now_ms.saturating_sub(snapshot.observed_ms) > ctx.max_age_ms
            || (observed_after_action && snapshot.observed_ms <= self.evidence_floor_ms())
        {
            return (TriState::Unknown, Some(snapshot.snapshot_id.clone()));
        }
        let Some(valid_until_ms) = snapshot.observed_ms.checked_add(ctx.max_age_ms) else {
            return (TriState::Unknown, Some(snapshot.snapshot_id.clone()));
        };
        let partial_scope_candidate = if !snapshot.complete {
            if row_scope_coverage(snapshot, &row_spec.table_scope).is_err() {
                return (TriState::Unknown, Some(snapshot.snapshot_id.clone()));
            }
            let Some(candidate) = snapshot
                .table_scope_search
                .as_ref()
                .filter(|search| search.requested_scope == row_spec.table_scope)
                .and_then(|search| search.unique_candidate())
            else {
                return (TriState::Unknown, Some(snapshot.snapshot_id.clone()));
            };
            Some(candidate.table_instance_ref.clone())
        } else {
            None
        };
        let result = resolve_row_target(
            snapshot,
            &ctx.goal.target,
            valid_until_ms,
            ctx.now_ms,
            ctx.max_age_ms,
        );
        let ResolvedRowTarget { selector, source } = match result {
            Ok(result) => result,
            Err(_) => return (TriState::Unknown, Some(snapshot.snapshot_id.clone())),
        };
        if partial_scope_candidate.as_ref().is_some_and(|table_ref| {
            table_ref != &selector.table_instance_ref || table_ref != &source.table_instance_ref
        }) {
            return (TriState::Unknown, Some(snapshot.snapshot_id.clone()));
        }
        if source
            .validate_against_snapshot(&selector, snapshot, ctx.now_ms, ctx.max_age_ms)
            .is_err()
        {
            return (TriState::Unknown, Some(snapshot.snapshot_id.clone()));
        }
        let selected = match source.selected {
            Some(true) => TriState::True,
            Some(false) => TriState::False,
            None => TriState::Unknown,
        };
        (selected, Some(snapshot.snapshot_id.clone()))
    }

    fn leaf(
        &self,
        predicate: &Predicate,
        ctx: &EvidenceContext<'_>,
    ) -> (TriState, Option<RequestId>) {
        match predicate {
            Predicate::ContextMatches { app } => match ctx.state.latest() {
                Some(env)
                    if ctx.goal.execution_domain
                        == interaction_contracts::desktop::domain::ExecutionDomain::BackgroundAx =>
                {
                    let context_matches = app == &ctx.goal.scope.app
                        && app == &env.snapshot.app.bundle_id
                        && self
                            .background_target_value(
                                ctx,
                                env,
                                &ctx.goal.target.semantic_target,
                                false,
                            )
                            .is_some();
                    (
                        if context_matches {
                            TriState::True
                        } else {
                            TriState::Unknown
                        },
                        Some(env.snapshot.snapshot_id.clone()),
                    )
                }
                Some(env)
                    if env.snapshot.complete && focused_window_id(&env.snapshot).is_some() =>
                {
                    (
                    if &env.snapshot.app.bundle_id == app {
                        TriState::True
                    } else {
                        TriState::False
                    },
                    Some(env.snapshot.snapshot_id.clone()),
                    )
                }
                Some(env) => (TriState::Unknown, Some(env.snapshot.snapshot_id.clone())),
                None => (TriState::Unknown, None),
            },
            Predicate::TargetBound { target } => {
                let bound = ctx.bindings.current(target).is_some_and(|binding| {
                    ctx.goal.execution_domain
                        != interaction_contracts::desktop::domain::ExecutionDomain::BackgroundAx
                        || binding.rule == "background_ax"
                            && binding.device_context.supports_background_ax_press()
                });
                (
                    if bound {
                        TriState::True
                    } else {
                        TriState::Unknown
                    },
                    None,
                )
            }
            Predicate::ReadValueEquals {
                target,
                expected,
                observed_after_action,
            } => {
                let Some(env) = self.evidence_for(ctx, *observed_after_action) else {
                    return (TriState::Unknown, None);
                };
                let id = Some(env.snapshot.snapshot_id.clone());
                if !env.snapshot.complete {
                    return (TriState::Unknown, id);
                }
                let is_registered_validation_target = target == &ctx.goal.target.semantic_target
                    && (ctx.goal.allowed_scope.as_ref()
                        .and_then(|scope| scope.targets.first())
                        .and_then(|allowed| ctx.pack.scoped_validation_feedback(allowed))
                        .is_some()
                        || scoped_form_number_control(ctx.goal, ctx.pack).is_some());
                if is_registered_validation_target {
                    let current = ctx.state.latest();
                    let returned_after = if ctx.goal.confirmation_scope == ConfirmationScope::UiRevisit {
                        self.visits.returned_at
                    } else {
                        Some(0)
                    };
                    if !ctx.state.usable(ctx.now_ms, ctx.max_age_ms)
                        || current.is_none_or(|latest| {
                            latest.snapshot.snapshot_id != env.snapshot.snapshot_id
                        })
                        || *observed_after_action
                            && env.snapshot.observed_ms <= self.evidence_floor_ms()
                        || returned_after.is_none_or(|returned_at| {
                            env.snapshot.observed_ms < returned_at
                        })
                    {
                        return (TriState::Unknown, id);
                    }
                }
                let value = if ctx.goal.execution_domain
                    == interaction_contracts::desktop::domain::ExecutionDomain::BackgroundAx
                {
                    self.background_target_value(ctx, env, target, *observed_after_action)
                } else {
                    Self::read_value(ctx, env, target)
                };
                match value.and_then(|v| values_equal(&v, expected)) {
                    Some(true) => (TriState::True, id),
                    Some(false) => (TriState::False, id),
                    None => (TriState::Unknown, id),
                }
            }
            Predicate::RowSelected {
                target,
                observed_after_action,
            } => {
                let (value, evidence) = self.row_selected(ctx, target, *observed_after_action);
                (value, evidence)
            }
            Predicate::Revisited { .. } => (
                if self.visits.satisfied() {
                    TriState::True
                } else {
                    TriState::Unknown
                },
                None,
            ),
            Predicate::CommitConfirmed { target } if ctx.goal.scoped_commit.is_some() => {
                if target != &ctx.goal.target.semantic_target {
                    return (TriState::Unknown, None);
                }
                let status = self.scoped_commit_receipt_status(ctx);
                let (_, _, evidence) = scoped_commit_receipt_read(
                    ctx,
                    Some(self.evidence_floor_ms()),
                );
                match status {
                    ScopedCommitReceiptStatus::Confirmed => (TriState::True, evidence),
                    ScopedCommitReceiptStatus::Pending => (TriState::False, evidence),
                    ScopedCommitReceiptStatus::Unknown => (TriState::Unknown, evidence),
                }
            }
            Predicate::CommitConfirmed { .. } => {
                let Some(rule) = ctx.pack.save_state_description.as_deref() else {
                    return (TriState::Unknown, None);
                };
                let Some(env) = ctx.state.latest_since(self.evidence_floor_ms()) else {
                    return (TriState::Unknown, None);
                };
                let id = Some(env.snapshot.snapshot_id.clone());
                let status = env
                    .snapshot
                    .objects
                    .iter()
                    .find(|object| object.description.as_deref() == Some(rule))
                    .and_then(|object| object.value.clone());
                match status {
                    Some(UiValue::Text(text)) if text == "saved" => (TriState::True, id),
                    Some(UiValue::Text(_)) => (TriState::False, id),
                    _ => (TriState::Unknown, id),
                }
            }
            Predicate::And { .. } | Predicate::Or { .. } | Predicate::Not { .. } => {
                (self.evaluate(predicate, ctx), None)
            }
        }
    }

    /// The three-valued value of a predicate over the current evidence.
    pub fn evaluate(&self, predicate: &Predicate, ctx: &EvidenceContext<'_>) -> TriState {
        match predicate {
            Predicate::And { of } => TriState::and(of.iter().map(|p| self.evaluate(p, ctx))),
            Predicate::Or { of } => TriState::or(of.iter().map(|p| self.evaluate(p, ctx))),
            Predicate::Not { of } => !self.evaluate(of, ctx),
            leaf => self.leaf(leaf, ctx).0,
        }
    }

    fn collect(&self, predicate: &Predicate, ctx: &EvidenceContext<'_>, verdict: &mut Verdict) {
        match predicate {
            Predicate::And { of } | Predicate::Or { of } => {
                of.iter()
                    .for_each(|child| self.collect(child, ctx, verdict));
            }
            Predicate::Not { of } => self.collect(of, ctx, verdict),
            leaf => {
                let (value, evidence) = self.leaf(leaf, ctx);
                if let Some(id) = evidence
                    && !verdict.evidence.contains(&id)
                {
                    verdict.evidence.push(id);
                }
                if value != TriState::True
                    && let Some(need) = self.need_for(leaf, value, ctx)
                {
                    verdict.needs.push(need);
                }
            }
        }
    }

    fn need_for(
        &self,
        leaf: &Predicate,
        value: TriState,
        ctx: &EvidenceContext<'_>,
    ) -> Option<EvidenceNeed> {
        let (fact, target, after, requires_input) =
            match leaf {
                Predicate::ContextMatches { app } => (format!("context:{app}"), None, None, false),
                Predicate::TargetBound { target } | Predicate::ReadValueEquals { target, .. }
                    if ctx.bindings.current(target).is_none() =>
                {
                    (
                        format!("binding:{target}"),
                        Some(target.clone()),
                        None,
                        false,
                    )
                }
                Predicate::TargetBound { .. } => return None,
                Predicate::ReadValueEquals { target, .. } => (
                    format!("value:{target}"),
                    Some(target.clone()),
                    Some(self.evidence_floor_ms()),
                    // A reading that disagrees needs the change, which is input.
                    value == TriState::False,
                ),
                Predicate::RowSelected {
                    target,
                    observed_after_action,
                } => (
                    format!("row-selected:{target}"),
                    Some(target.clone()),
                    observed_after_action.then(|| self.evidence_floor_ms()),
                    value == TriState::False,
                ),
                Predicate::Revisited { target } => (
                    format!("revisit:{target}"),
                    Some(target.clone()),
                    None,
                    true,
                ),
                Predicate::CommitConfirmed { target } => (
                    format!("commit:{target}"),
                    Some(target.clone()),
                    Some(self.evidence_floor_ms()),
                    value == TriState::False
                        && !ctx.goal.scoped_commit.as_ref().is_some_and(|commit| {
                            commit.receipt.kind == ScopedCommitKind::Automatic
                        }),
                ),
                _ => return None,
            };
        Some(EvidenceNeed {
            need_id: RequestId::new(format!("need-{}", self.need_seq)).expect("valid id"),
            fact,
            target,
            observed_after_ms: after,
            requires_input,
        })
    }

    /// Evaluate the whole goal and say what is still missing.
    pub fn verdict(&mut self, ctx: &EvidenceContext<'_>) -> Verdict {
        self.need_seq += 1;
        let mut verdict = Verdict {
            status: self.evaluate(&ctx.goal.required, ctx),
            evidence: Vec::new(),
            needs: Vec::new(),
        };
        self.collect(&ctx.goal.required, ctx, &mut verdict);
        verdict
    }

    fn causality(&self, status: EffectStatus) -> Causality {
        if self.unknown_input_floor_ms.is_some() {
            return Causality::Unknown;
        }
        if status == EffectStatus::Satisfied
            && self.background_action_input.as_ref().is_some_and(|input| {
                input.binding.continuity.kind
                    == interaction_contracts::desktop::source::ObjectIdentityContinuity::ReadScoped
            })
        {
            // The later Source read proves the current semantic target value,
            // but a read-scoped AX reference cannot prove native-entity
            // continuity or attribute that value to the earlier AXPress.
            return Causality::Unknown;
        }
        match (self.first_input_ms, status) {
            (None, _) => Causality::NotApplicable,
            (Some(_), EffectStatus::Satisfied) if self.baseline == Some(TriState::False) => {
                Causality::Confirmed
            }
            (Some(_), _) => Causality::Unknown,
        }
    }

    /// Record a conclusion as a new immutable revision, if it differs from
    /// the last one. History is never rewritten.
    pub fn conclude(
        &mut self,
        storage: &mut Storage,
        root: &RootTaskId,
        ctx: &EvidenceContext<'_>,
        verdict: &Verdict,
        attempt: Option<(&AttemptId, Option<&str>)>,
        now_ms: u64,
    ) -> Result<Option<EffectResult>, StoreError> {
        let status = EffectStatus::from(verdict.status);
        let key = (status, verdict.evidence.clone());
        if self.last.as_ref() == Some(&key) {
            return Ok(None);
        }
        self.revision += 1;
        let result = EffectResult {
            result_id: RequestId::new(format!("effect-{root}")).expect("derived from valid ids"),
            revision: self.revision,
            root_task_id: root.clone(),
            attempt_id: attempt.map(|(id, _)| id.clone()),
            intent_id: attempt.and_then(|(_, intent)| intent.map(String::from)),
            predicate: "goal.required@1".into(),
            target: Some(ctx.goal.target.semantic_target.clone()),
            status,
            evidence_group: verdict.evidence.clone(),
            input_span: self
                .first_input_ms
                .zip(self.last_input_end_ms)
                .map(|(from_ms, to_ms)| TimeSpan { from_ms, to_ms }),
            observed_ms: now_ms,
            confirmation_scope: ctx.goal.confirmation_scope,
            causality: if ctx.goal.target.row.is_some() && self.first_input_ms.is_some() {
                Causality::Unknown
            } else {
                self.causality(status)
            },
        };
        storage.transaction::<_, StoreError>(|tx| VerificationStore::insert(tx, &result))?;
        self.last = Some(key);
        Ok(Some(result))
    }
}

/// Verification's table: immutable effect results.
pub struct VerificationStore;

/// Facts that Engine has correlated to a trusted Main reopen transition and
/// an admitted private Source proof. Verification owns the allowed goal
/// recipe, the conclusion, and its durable write.
pub(crate) struct OwnedRevisitConclusion<'a> {
    pub goal: &'a GoalSpec,
    pub registration: &'a OwnedDocumentRegistration,
    pub proof: &'a OwnedDocumentBindingProof,
    pub current_ui_anchor: &'a EffectResult,
    pub transition_id: &'a str,
    pub save_attempt_id: &'a AttemptId,
    pub save_intent_id: &'a str,
    pub save_span: TimeSpan,
    pub expected_sha256: &'a str,
    pub expected_byte_length: u32,
    pub old_source_namespace: &'a str,
    /// Engine supplies these only after validating the complete private Main
    /// file lineage and its directory/process identity.
    pub main_file_sha256: &'a str,
    pub main_file_byte_length: u32,
    pub now_ms: u64,
    pub max_age_ms: u64,
}

pub(crate) fn owned_revisit_goal_supported(goal: &GoalSpec) -> bool {
    let target = &goal.target.semantic_target;
    let ExpectedValue::Text { value } = &goal.desired else {
        return false;
    };
    goal.confirmation_scope == ConfirmationScope::UiRevisit
        && goal.scope.app == "com.apple.TextEdit"
        && !value.is_empty()
        && value.len() <= 2048
        && !value.contains('\0')
        && goal.required
            == (Predicate::And {
                of: vec![
                    Predicate::ContextMatches {
                        app: goal.scope.app.clone(),
                    },
                    Predicate::ReadValueEquals {
                        target: target.clone(),
                        expected: goal.desired.clone(),
                        observed_after_action: true,
                    },
                    Predicate::Revisited {
                        target: target.clone(),
                    },
                ],
            })
}

/// Verify the supported frozen TextEdit goal recipe against the durable
/// CurrentUi setter result, Main's independently checked saved-file digest,
/// and the new registration-bound post-reopen AX value. A failure stores no
/// satisfied effect.
pub(crate) fn conclude_owned_revisit(
    storage: &mut Storage,
    evidence: OwnedRevisitConclusion<'_>,
) -> Result<EffectResult, StoreError> {
    if !owned_revisit_goal_supported(evidence.goal)
        || evidence.save_span.from_ms > evidence.save_span.to_ms
        || evidence.main_file_sha256 != evidence.expected_sha256
        || evidence.main_file_byte_length != evidence.expected_byte_length
        || evidence.main_file_byte_length > 2048
        || evidence.proof.semantic_target != evidence.goal.target.semantic_target
        || evidence
            .proof
            .receiver
            .provenance
            .identity_boundary
            .namespace
            == evidence.old_source_namespace
        || evidence
            .proof
            .validate_registration(evidence.registration)
            .is_err()
        || evidence
            .proof
            .validate_at(evidence.now_ms, evidence.max_age_ms)
            .is_err()
    {
        return Err(StoreError::Conflict(
            "owned revisit evidence does not match the frozen goal and reopen lineage".into(),
        ));
    }
    let ExpectedValue::Text { value } = &evidence.goal.desired else {
        return Err(StoreError::Conflict(
            "owned revisit goal is not text".into(),
        ));
    };
    let expected_sha256 = format!("{:x}", Sha256::digest(value.as_bytes()));
    let Some(readback) = evidence.proof.receiver.value_readback.as_ref() else {
        return Err(StoreError::Conflict(
            "reopened AX value is unavailable".into(),
        ));
    };
    if expected_sha256 != evidence.expected_sha256
        || value.len() != evidence.expected_byte_length as usize
        || readback.sha256 != evidence.expected_sha256
        || readback.utf8_byte_count != evidence.expected_byte_length
        || readback.observed_ms > evidence.now_ms
    {
        return Err(StoreError::Conflict(
            "reopened AX value does not match the saved text".into(),
        ));
    }

    let anchor = evidence.current_ui_anchor;
    let Some(anchor_span) = anchor.input_span else {
        return Err(StoreError::Conflict(
            "CurrentUi anchor has no input span".into(),
        ));
    };
    let anchor_is_durable =
        VerificationStore::history(storage.read(), evidence.goal.root_task_id.as_str())?
            .iter()
            .any(|stored| stored == anchor);
    if !anchor_is_durable
        || anchor.root_task_id != evidence.goal.root_task_id
        || anchor.predicate != "text.value@1"
        || anchor.status != EffectStatus::Satisfied
        || anchor.confirmation_scope != ConfirmationScope::CurrentUi
        || anchor.target.as_ref() != Some(&evidence.goal.target.semantic_target)
        || anchor.attempt_id.as_ref() == Some(evidence.save_attempt_id)
        || anchor.intent_id.as_deref() == Some(evidence.save_intent_id)
        || anchor_span.from_ms > anchor_span.to_ms
        || anchor_span.to_ms > anchor.observed_ms
        || anchor.observed_ms >= evidence.save_span.from_ms
        || anchor_span.to_ms >= evidence.save_span.from_ms
    {
        return Err(StoreError::Conflict(
            "owned revisit lacks its durable pre-save CurrentUi anchor".into(),
        ));
    }

    let history = VerificationStore::history(storage.read(), evidence.goal.root_task_id.as_str())?;
    let revision = history
        .iter()
        .map(|result| result.revision)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| StoreError::Corrupt("verification revision overflow".into()))?;
    let transition_ref = RequestId::new(evidence.transition_id.to_owned())
        .map_err(|error| StoreError::Corrupt(error.to_string()))?;
    let result = EffectResult {
        result_id: RequestId::new(format!("effect-{}", evidence.goal.root_task_id))
            .map_err(|error| StoreError::Corrupt(error.to_string()))?,
        revision,
        root_task_id: evidence.goal.root_task_id.clone(),
        attempt_id: Some(evidence.save_attempt_id.clone()),
        intent_id: Some(evidence.save_intent_id.to_owned()),
        predicate: "goal.required@1".into(),
        target: Some(evidence.goal.target.semantic_target.clone()),
        status: EffectStatus::Satisfied,
        evidence_group: anchor
            .evidence_group
            .iter()
            .cloned()
            .chain([
                evidence.proof.receiver.provenance.snapshot_id.clone(),
                transition_ref,
            ])
            .collect(),
        input_span: Some(evidence.save_span),
        observed_ms: readback.observed_ms,
        confirmation_scope: ConfirmationScope::UiRevisit,
        causality: Causality::Unknown,
    };
    storage.transaction::<_, StoreError>(|tx| VerificationStore::insert(tx, &result))?;
    Ok(result)
}

pub fn conclude_current_ui(storage: &mut Storage, result: EffectResult) -> Result<(), StoreError> {
    if ![
        "window.active@1",
        "scroll.target_visible_bound@1",
        "menu.selected_value@1",
        "radio_group.selected_value@1",
        "keyboard.value@1",
        "text.value@1",
        "browser.checkbox_state@1",
    ]
    .contains(&result.predicate.as_str())
        || result.status != EffectStatus::Satisfied
        || result.confirmation_scope
            != interaction_contracts::desktop::task::ConfirmationScope::CurrentUi
        || result.evidence_group.is_empty()
        || result
            .input_span
            .is_some_and(|span| span.to_ms > result.observed_ms)
    {
        return Err(StoreError::Conflict(
            "invalid current UI effect evidence".into(),
        ));
    }
    storage.transaction::<_, StoreError>(|tx| VerificationStore::insert(tx, &result))
}

/// Prove that an AX text setter's value is visible in a new normal-window
/// Source read of the same bound text control after the actual input end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TextValueRejection {
    InvalidContext,
    SnapshotMissing,
    SnapshotNotFresh,
    WindowMissing,
    WindowChanged,
    BindingMissing,
    BindingSemanticChanged,
    BindingInstanceChanged,
    BindingFingerprintChanged,
    BindingPageChanged,
    BindingNotFresh,
    ObjectMissing,
    ObjectIneligible,
    ValueMismatch,
    ValueUnavailable,
}

impl TextValueRejection {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::InvalidContext => "invalid_context",
            Self::SnapshotMissing => "snapshot_missing",
            Self::SnapshotNotFresh => "snapshot_not_fresh",
            Self::WindowMissing => "window_missing",
            Self::WindowChanged => "window_changed",
            Self::BindingMissing => "binding_missing",
            Self::BindingSemanticChanged => "binding_semantic_changed",
            Self::BindingInstanceChanged => "binding_instance_changed",
            Self::BindingFingerprintChanged => "binding_fingerprint_changed",
            Self::BindingPageChanged => "binding_page_changed",
            Self::BindingNotFresh => "binding_not_fresh",
            Self::ObjectMissing => "object_missing",
            Self::ObjectIneligible => "object_ineligible",
            Self::ValueMismatch => "value_mismatch",
            Self::ValueUnavailable => "value_unavailable",
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn text_value(
    state: &StateCore,
    bindings: &BindingReducer,
    target: &SemanticTargetRef,
    expected: &ExpectedValue,
    input_end_ms: u64,
    input_binding: &Binding,
    input_window: &SourceWindowContext,
    now_ms: u64,
    max_age_ms: u64,
) -> TriState {
    match text_value_diagnostic(
        state,
        bindings,
        target,
        expected,
        input_end_ms,
        input_binding,
        input_window,
        now_ms,
        max_age_ms,
    ) {
        Ok(result) => result,
        Err(TextValueRejection::ValueMismatch) => TriState::False,
        Err(_) => TriState::Unknown,
    }
}

/// The public verifier keeps its tri-state contract. The engine uses this
/// bounded reason variant only to explain why a private text proof stayed
/// unknown; it never includes app values or object identifiers.
#[allow(clippy::too_many_arguments)]
pub(crate) fn text_value_diagnostic(
    state: &StateCore,
    bindings: &BindingReducer,
    target: &SemanticTargetRef,
    expected: &ExpectedValue,
    input_end_ms: u64,
    input_binding: &Binding,
    input_window: &SourceWindowContext,
    now_ms: u64,
    max_age_ms: u64,
) -> Result<TriState, TextValueRejection> {
    if !matches!(expected, ExpectedValue::Text { .. })
        || !state.usable(now_ms, max_age_ms)
        || input_binding.semantic_target != *target
    {
        return Err(TextValueRejection::InvalidContext);
    }
    let Some(env) = input_end_ms
        .checked_add(1)
        .and_then(|floor| state.latest_since(floor))
    else {
        return Err(TextValueRejection::SnapshotMissing);
    };
    let snapshot = &env.snapshot;
    if snapshot.validate().is_err()
        || !snapshot.complete
        || snapshot.menu_context.is_some()
        || focused_window_id(snapshot).is_none()
        || snapshot.observed_ms <= input_end_ms
        || snapshot.observed_ms > now_ms
        || now_ms.saturating_sub(snapshot.observed_ms) > max_age_ms
    {
        return Err(TextValueRejection::SnapshotNotFresh);
    }
    let Some(window) = snapshot.window_context.as_ref() else {
        return Err(TextValueRejection::WindowMissing);
    };
    if window.session_epoch != input_window.session_epoch
        || window.process != input_window.process
        || window.origin_window_id != input_window.origin_window_id
        || window.origin_window_frame != input_window.origin_window_frame
        || window.session_epoch != snapshot.session_epoch
        || Some(window.origin_window_id.as_str()) != focused_window_id(snapshot)
        || window.verified_source_sequence != snapshot.source_sequence
        || window.verified_observed_ms != snapshot.observed_ms
    {
        return Err(TextValueRejection::WindowChanged);
    }
    let Some(binding) = bindings.current(target) else {
        return Err(TextValueRejection::BindingMissing);
    };
    if binding.semantic_target != input_binding.semantic_target {
        return Err(TextValueRejection::BindingSemanticChanged);
    }
    if binding.instance != input_binding.instance {
        return Err(TextValueRejection::BindingInstanceChanged);
    }
    if binding.fingerprint_digest != input_binding.fingerprint_digest {
        return Err(TextValueRejection::BindingFingerprintChanged);
    }
    if binding.page != input_binding.page {
        return Err(TextValueRejection::BindingPageChanged);
    }
    if binding.snapshot_id != snapshot.snapshot_id
        || binding.env_revision != env.revision
        || binding.observed_ms != snapshot.observed_ms
        || binding.observed_ms <= input_end_ms
    {
        return Err(TextValueRejection::BindingNotFresh);
    }
    let Some(object) = object_for(env, &input_binding.fingerprint_digest) else {
        return Err(TextValueRejection::ObjectMissing);
    };
    let label = object
        .title
        .as_deref()
        .or(object.description.as_deref())
        .unwrap_or("");
    if object.instance_ref != input_binding.instance
        || label.trim().is_empty()
        || !matches!(
            object.role.as_str(),
            "textfield" | "textarea" | "AXTextField" | "AXTextArea"
        )
        || !object
            .actions
            .contains(&interaction_contracts::desktop::source::AxActionName::SetValue)
        || !object.enabled
        || !object.in_view
        || object.secure
    {
        return Err(TextValueRejection::ObjectIneligible);
    }
    match object
        .value
        .as_ref()
        .and_then(|value| values_equal(value, expected))
    {
        Some(true) => Ok(TriState::True),
        Some(false) => Err(TextValueRejection::ValueMismatch),
        None => Err(TextValueRejection::ValueUnavailable),
    }
}

/// Diagnose a post-input owned-document value read without upgrading it into
/// an effect result. The caller must supply the exact span extracted from a
/// confirmed single SetValue receipt; this function only checks evidence.
#[allow(clippy::too_many_arguments)]
pub fn owned_text_value_diagnostic(
    original: &OwnedDocumentBindingProof,
    current: &OwnedDocumentBindingProof,
    registration: &OwnedDocumentRegistration,
    expected: &str,
    input_span: TimeSpan,
    now_ms: u64,
    max_age_ms: u64,
) -> TriState {
    if OwnedDocumentBindingProof::validate_setter_value(expected).is_err()
        || input_span.from_ms > input_span.to_ms
        || original.validate_registration(registration).is_err()
        || input_span.from_ms < original.snapshot_observed_ms
        || input_span.to_ms < original.receiver.provenance.observed_ms
        || input_span.to_ms >= original.receiver.provenance.valid_until_ms
    {
        return TriState::Unknown;
    }
    if current.validate_registration(registration).is_err()
        || current.validate_at(now_ms, max_age_ms).is_err()
    {
        return TriState::Unknown;
    }
    if !original.same_instance(current) {
        return TriState::Unknown;
    }
    if current.environment_revision < original.environment_revision
        || current.receiver.provenance.source_sequence
            <= original.receiver.provenance.source_sequence
        || current.receiver.provenance.snapshot_id == original.receiver.provenance.snapshot_id
        || current.snapshot_observed_ms <= input_span.to_ms
        || current.receiver.provenance.observed_ms <= input_span.to_ms
    {
        return TriState::Unknown;
    }
    let Some(readback) = current.receiver.value_readback.as_ref() else {
        return TriState::Unknown;
    };
    if readback.observed_ms <= input_span.to_ms {
        return TriState::Unknown;
    }

    let expected_bytes = expected.as_bytes();
    let expected_sha256 = format!("{:x}", Sha256::digest(expected_bytes));
    if readback.utf8_byte_count as usize == expected_bytes.len()
        && readback.sha256 == expected_sha256
    {
        TriState::True
    } else {
        TriState::False
    }
}

/// A navigation effect is proved by the destination's new binding in the
/// selected container, independently of offset progress and input receipts.
pub fn scroll_target_bound(
    state: &StateCore,
    bindings: &BindingReducer,
    container: &SemanticTargetRef,
    destination: &SemanticTargetRef,
    now_ms: u64,
    max_age_ms: u64,
    floor_ms: u64,
) -> TriState {
    let Some(env) = state.latest_since(floor_ms) else {
        return TriState::Unknown;
    };
    if !env.snapshot.complete
        || env.snapshot.observed_ms > now_ms
        || now_ms.saturating_sub(env.snapshot.observed_ms) > max_age_ms
    {
        return TriState::Unknown;
    }
    let (Some(container), Some(destination)) =
        (bindings.current(container), bindings.current(destination))
    else {
        return TriState::Unknown;
    };
    if [container, destination].iter().any(|binding| {
        binding.snapshot_id != env.snapshot.snapshot_id
            || binding.env_revision != env.revision
            || binding.observed_ms < floor_ms
    }) {
        return TriState::Unknown;
    }
    let (Some(area), Some(target)) = (
        object_for(env, &container.fingerprint_digest),
        object_for(env, &destination.fingerprint_digest),
    ) else {
        return TriState::Unknown;
    };
    if area.role != "AXScrollArea"
        || !area.enabled
        || area.secure
        || !area.in_view
        || !target.enabled
        || target.secure
        || !target.in_view
        || !BindingReducer::in_container(env, &target.instance_ref, &area.instance_ref)
    {
        return TriState::False;
    }
    TriState::True
}

/// Selection requires a new normal-window binding after the menu has closed.
/// A menu callback, a disappeared menu or an input receipt cannot prove it.
#[allow(clippy::too_many_arguments)]
pub fn menu_selected_value(
    state: &StateCore,
    bindings: &BindingReducer,
    opener: &SemanticTargetRef,
    desired: &interaction_contracts::desktop::task::ExpectedValue,
    now_ms: u64,
    max_age_ms: u64,
    floor_ms: u64,
) -> TriState {
    let Some(env) = state.latest_since(floor_ms) else {
        return TriState::Unknown;
    };
    if !state.usable(now_ms, max_age_ms)
        || env.snapshot.menu_context.is_some()
        || env.snapshot.observed_ms < floor_ms
    {
        return TriState::Unknown;
    }
    let Some(binding) = bindings.current(opener) else {
        return TriState::Unknown;
    };
    if binding.snapshot_id != env.snapshot.snapshot_id
        || binding.env_revision != env.revision
        || binding.observed_ms < floor_ms
    {
        return TriState::Unknown;
    }
    let Some(object) = object_for(env, &binding.fingerprint_digest) else {
        return TriState::Unknown;
    };
    if !["AXMenuButton", "AXPopUpButton"].contains(&object.role.as_str())
        || !object.enabled
        || !object.in_view
        || object.secure
    {
        return TriState::False;
    }
    match object
        .value
        .as_ref()
        .and_then(|value| values_equal(value, desired))
    {
        Some(true) => TriState::True,
        Some(false) => TriState::False,
        None => TriState::Unknown,
    }
}

/// Registered window activation needs later complete focus evidence.
pub fn window_active(
    state: &StateCore,
    app: &str,
    pid: u32,
    window: &str,
    now: u64,
    after: u64,
) -> TriState {
    let Some(env) = state.latest() else {
        return TriState::Unknown;
    };
    if env.snapshot.observed_ms < after || state.window_root(app, pid, window, now, 2000).is_none()
    {
        return TriState::Unknown;
    }
    if focused_window_id(&env.snapshot) == Some(window) {
        TriState::True
    } else {
        TriState::False
    }
}

impl VerificationStore {
    pub fn insert(tx: &Transaction<'_>, result: &EffectResult) -> Result<(), StoreError> {
        tx.execute(
            "INSERT INTO effect_results (result_id, revision, root_id, json) VALUES (?1, ?2, ?3, ?4)",
            params![
                result.result_id.as_str(),
                result.revision as i64,
                result.root_task_id.as_str(),
                serde_json::to_string(result)?,
            ],
        )?;
        Ok(())
    }

    /// Every revision for a root, oldest first.
    pub fn history(conn: &Connection, root: &str) -> Result<Vec<EffectResult>, StoreError> {
        let mut statement =
            conn.prepare("SELECT json FROM effect_results WHERE root_id = ?1 ORDER BY revision")?;
        let mut rows = statement.query([root])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let text: String = row.get(0)?;
            out.push(serde_json::from_str(&text)?);
        }
        Ok(out)
    }

    pub fn latest(conn: &Connection, root: &str) -> Result<Option<EffectResult>, StoreError> {
        Ok(Self::history(conn, root)?.pop())
    }
}

/// How the status is spelled in logs.
pub fn status_name(status: EffectStatus) -> String {
    enum_to_str(&status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::state_core::StateCore;
    use interaction_contracts::desktop::binding::{BindRequest, BindScope, TargetDescription};
    use interaction_contracts::desktop::device::SurfaceRef;
    use interaction_contracts::desktop::effect::EffectClass;
    use interaction_contracts::desktop::ids::ObjectRef;
    use interaction_contracts::desktop::primitive::{AxActionKind, Primitive};
    use interaction_contracts::desktop::source::{
        ObjectIdentityContinuity, ObserverCoverage, Rect, SourceIdentityBoundary,
        StructureSnapshot, TableViewport, TableViewportCell, TableViewportColumn, TableViewportRow,
        UiObject,
    };
    use interaction_contracts::desktop::table_scope::{
        TableScopeCandidate, TableScopePathElement, TableScopeSearch, TableScopeSearchState,
    };
    use interaction_contracts::desktop::task::{
        ConfirmationScope, ExpectedValue, RootBudgetSpec, RowColumnConditionSpec, RowContainerRole,
        RowContainerScope, RowTableScope, RowTargetSpec, TargetSpec, TaskScope,
    };
    use sim_world::app::{Applied, SimApp, SnapshotMeta};

    const TARGET: &str = "push/daily-picks";

    fn meta(sequence: u64, observed_ms: u64) -> SnapshotMeta {
        SnapshotMeta {
            snapshot_id: format!("snap-{sequence}"),
            session_epoch: 1,
            surface: SurfaceRef {
                id: "main".into(),
                epoch: 1,
            },
            clock_domain: "sim/1/monotonic".into(),
            source_sequence: sequence,
            observed_ms,
        }
    }

    fn saved_view_row_target() -> TargetSpec {
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

    fn row_goal() -> GoalSpec {
        let target = saved_view_row_target();
        let semantic = target.semantic_target.clone();
        GoalSpec {
            root_task_id: RootTaskId::new("row-root").unwrap(),
            task_revision: 1,
            execution_domain: interaction_contracts::desktop::domain::ExecutionDomain::Simulated,
            browser_page_profile: None,
            user_text: "select the quarterly review row".into(),
            scope: TaskScope {
                app: "com.lingxi.computer-user.mvp-fixture".into(),
                account: None,
            },
            allowed_scope: None,
            scoped_revisit: None,
            scoped_commit: None,
            target,
            desired: ExpectedValue::Bool { value: true },
            confirmation_scope: ConfirmationScope::UiRevisit,
            max_effect_class: EffectClass::Navigation,
            required: Predicate::And {
                of: vec![
                    Predicate::ContextMatches {
                        app: "com.lingxi.computer-user.mvp-fixture".into(),
                    },
                    Predicate::RowSelected {
                        target: semantic.clone(),
                        observed_after_action: true,
                    },
                    Predicate::Revisited { target: semantic },
                ],
            },
            budget: RootBudgetSpec {
                deadline_ms: 10_000,
                max_attempts: 4,
                max_observations: 8,
                max_recoveries: 1,
                max_replans: 1,
            },
        }
    }

    fn row_snapshot(
        sequence: u64,
        observed_ms: u64,
        selected: Option<bool>,
        row_refs: &[(&str, ObjectIdentityContinuity, &str, f64)],
        visible_rows_complete: bool,
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
        snapshot.complete = true;
        snapshot.incomplete_reason = None;

        let root = snapshot
            .objects
            .iter()
            .find(|object| object.window_id == window_id && object.parent.is_none())
            .unwrap()
            .clone();
        let root_ref = root.instance_ref.clone();
        let scroll_frame = Rect {
            x: 20.0,
            y: 100.0,
            width: 560.0,
            height: 300.0,
        };
        let table_frame = scroll_frame;
        let scroll_ref = interaction_contracts::desktop::ids::ObjectRef::new("row-scroll").unwrap();
        let table_ref = interaction_contracts::desktop::ids::ObjectRef::new("row-table").unwrap();
        let column_ref = interaction_contracts::desktop::ids::ObjectRef::new("row-column").unwrap();
        let header_ref = interaction_contracts::desktop::ids::ObjectRef::new("row-header").unwrap();

        let object = |reference: &str,
                      parent: Option<interaction_contracts::desktop::ids::ObjectRef>,
                      role: &str,
                      frame: Rect,
                      continuity: ObjectIdentityContinuity| {
            UiObject {
                instance_ref: interaction_contracts::desktop::ids::ObjectRef::new(reference)
                    .unwrap(),
                parent,
                window_id: window_id.clone(),
                role: role.into(),
                subrole: None,
                title: None,
                description: (role == "AXScrollArea").then(|| "Saved views".into()),
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
                identity_continuity: continuity,
            }
        };

        let header_frame = Rect {
            x: table_frame.x,
            y: table_frame.y,
            width: table_frame.width,
            height: 24.0,
        };
        let mut objects = vec![
            root,
            object(
                scroll_ref.as_str(),
                Some(root_ref),
                "AXScrollArea",
                scroll_frame,
                ObjectIdentityContinuity::NotificationCovered,
            ),
            object(
                table_ref.as_str(),
                Some(scroll_ref.clone()),
                "AXTable",
                table_frame,
                ObjectIdentityContinuity::NotificationCovered,
            ),
            object(
                column_ref.as_str(),
                Some(table_ref.clone()),
                "AXColumn",
                table_frame,
                ObjectIdentityContinuity::NotificationCovered,
            ),
            object(
                header_ref.as_str(),
                Some(column_ref.clone()),
                "AXStaticText",
                header_frame,
                ObjectIdentityContinuity::ReadScoped,
            ),
        ];
        let mut viewport_rows = Vec::new();
        for (index, (row_ref, continuity, value, row_y)) in row_refs.iter().enumerate() {
            let cell_ref = interaction_contracts::desktop::ids::ObjectRef::new(format!(
                "{row_ref}-entry-cell"
            ))
            .unwrap();
            let row_ref = interaction_contracts::desktop::ids::ObjectRef::new(*row_ref).unwrap();
            let row_frame = Rect {
                x: scroll_frame.x,
                y: scroll_frame.y + *row_y,
                width: scroll_frame.width,
                height: 30.0,
            };
            objects.push(object(
                row_ref.as_str(),
                Some(table_ref.clone()),
                "AXRow",
                row_frame,
                *continuity,
            ));
            let mut cell_object = object(
                cell_ref.as_str(),
                Some(row_ref.clone()),
                "AXCell",
                row_frame,
                ObjectIdentityContinuity::ReadScoped,
            );
            cell_object.value = Some(UiValue::Text((*value).into()));
            objects.push(cell_object);
            viewport_rows.push(TableViewportRow {
                ax_enabled: interaction_contracts::desktop::source::AxEnabledObservation::Enabled,
                instance_ref: Some(row_ref.clone()),
                parent_instance_ref: table_ref.clone(),
                frame: row_frame,
                accessibility_identifier: None,
                ax_index: Some(index as u32),
                selected,
                ax_selected_settable: Some(true),
                cells_complete: true,
                cells: vec![TableViewportCell {
                    instance_ref: Some(cell_ref),
                    parent_instance_ref: Some(row_ref),
                    frame: row_frame,
                    row_ax_index: Some(index as u32),
                    column_ax_index: Some(0),
                    column_instance_ref: Some(column_ref.clone()),
                    header_instance_ref: Some(header_ref.clone()),
                    column_header: Some("Entry".into()),
                    value: Some(UiValue::Text((*value).into())),
                }],
            });
        }
        snapshot.objects = objects;
        snapshot.observer_coverage = ObserverCoverage::for_projected_objects(&snapshot.objects);
        snapshot.table_viewports = vec![TableViewport {
            ax_enabled: interaction_contracts::desktop::source::AxEnabledObservation::Enabled,
            table_instance_ref: table_ref.clone(),
            scroll_container_instance_ref: scroll_ref.clone(),
            process_id: snapshot.app.pid,
            table_frame,
            viewport_frame: scroll_frame,
            visible_rows_complete,
            visible_rows: viewport_rows,
            columns_complete: true,
            columns: vec![TableViewportColumn {
                instance_ref: Some(column_ref.clone()),
                parent_instance_ref: table_ref,
                frame: table_frame,
                header_instance_ref: Some(header_ref.clone()),
                header: Some("Entry".into()),
                ax_index: Some(0),
            }],
        }];
        assert!(window_frame.contains_rect(&scroll_frame));
        snapshot
    }

    fn partial_row_snapshot(
        sequence: u64,
        observed_ms: u64,
        selected: Option<bool>,
    ) -> StructureSnapshot {
        let mut snapshot = row_snapshot(
            sequence,
            observed_ms,
            selected,
            &[(
                "row-read-partial",
                ObjectIdentityContinuity::ReadScoped,
                "Quarterly review",
                0.0,
            )],
            true,
        );
        snapshot.complete = false;
        snapshot.incomplete_reason = Some("AX child list truncated".into());
        let goal = row_goal();
        let scope = goal.target.row.unwrap().table_scope;
        let table_instance_ref =
            interaction_contracts::desktop::ids::ObjectRef::new("row-table").unwrap();
        let scroll_container_instance_ref =
            interaction_contracts::desktop::ids::ObjectRef::new("row-scroll").unwrap();
        let scroll = snapshot
            .objects
            .iter()
            .find(|object| object.instance_ref == scroll_container_instance_ref)
            .unwrap();
        let candidate = TableScopeCandidate {
            table_instance_ref,
            scroll_container_instance_ref: scroll_container_instance_ref.clone(),
            scroll_path: vec![TableScopePathElement {
                instance_ref: scroll_container_instance_ref,
                label: "Saved views".into(),
                frame: scroll.frame,
            }],
            column_headers: scope.column_headers.clone(),
        };
        snapshot.table_scope_search = Some(TableScopeSearch {
            requested_scope: scope,
            process_id: snapshot.app.pid,
            window_id: snapshot
                .global_focus
                .known_window_for_app(&snapshot.app)
                .unwrap()
                .to_owned(),
            state: TableScopeSearchState::Complete,
            candidates: vec![candidate],
        });
        snapshot
    }

    fn goal(scope: ConfirmationScope) -> GoalSpec {
        let target = SemanticTargetRef::new(TARGET).unwrap();
        let mut checks = vec![
            Predicate::ContextMatches {
                app: "test.settings".into(),
            },
            Predicate::ReadValueEquals {
                target: target.clone(),
                expected: ExpectedValue::Bool { value: false },
                observed_after_action: true,
            },
        ];
        checks.push(match scope {
            ConfirmationScope::CurrentUi => unreachable!("root fixture requires revisit or commit"),
            ConfirmationScope::UiRevisit => Predicate::Revisited {
                target: target.clone(),
            },
            ConfirmationScope::Commit => Predicate::CommitConfirmed {
                target: target.clone(),
            },
        });
        GoalSpec {
            root_task_id: RootTaskId::new("root-1").unwrap(),
            task_revision: 1,
            execution_domain: interaction_contracts::desktop::domain::ExecutionDomain::Simulated,
            browser_page_profile: None,
            user_text: "turn it off".into(),
            scope: TaskScope {
                app: "test.settings".into(),
                account: None,
            },
            allowed_scope: None,
            scoped_revisit: None,
            scoped_commit: None,
            target: TargetSpec {
                semantic_target: target,
                label: "Daily picks".into(),
                group: Some("Push".into()),
                role_hint: Some("switch".into()),
                row: None,
            },
            desired: ExpectedValue::Bool { value: false },
            confirmation_scope: scope,
            max_effect_class: EffectClass::ReversibleEdit,
            required: Predicate::And { of: checks },
            budget: RootBudgetSpec {
                deadline_ms: 60_000,
                max_attempts: 8,
                max_observations: 16,
                max_recoveries: 2,
                max_replans: 1,
            },
        }
    }

    struct Rig {
        state: StateCore,
        bindings: BindingReducer,
        pack: AppPack,
        app: SimApp,
        goal: GoalSpec,
        verification: Verification,
        sequence: u64,
        now_ms: u64,
    }

    impl Rig {
        fn new(scope: ConfirmationScope) -> Self {
            let mut state = StateCore::new();
            state.set_session(1);
            Self {
                state,
                bindings: BindingReducer::new(),
                pack: AppPack::settings_fixture(),
                app: SimApp::new(),
                goal: goal(scope),
                verification: Verification::new(0),
                sequence: 0,
                now_ms: 0,
            }
        }

        fn observe_at(&mut self, observed_ms: u64) {
            self.now_ms = observed_ms;
            self.sequence += 1;
            let snapshot = self.app.snapshot(&meta(self.sequence, observed_ms));
            self.state.admit(snapshot, observed_ms, &self.pack).unwrap();
            let env = self.state.latest().unwrap().clone();
            self.bindings.revalidate(&env);
            self.verification
                .visits
                .observe(env.page.as_deref(), observed_ms);
        }

        fn bind(&mut self) {
            let env = self.state.latest().unwrap().clone();
            let request = BindRequest {
                request_id: RequestId::new("bind").unwrap(),
                semantic_target: SemanticTargetRef::new(TARGET).unwrap(),
                description: TargetDescription {
                    label: "Daily picks".into(),
                    group: Some("Push".into()),
                    role_hint: Some("switch".into()),
                },
                scope: BindScope {
                    app: "test.settings".into(),
                    account: None,
                },
                choice: None,
                max_age_ms: 10_000,
            };
            self.bindings.bind(&env, &request, env.snapshot.observed_ms);
            let bound_at = env.snapshot.observed_ms;
            self.verification.visits.set_target_page(env.page.clone());
            self.verification
                .visits
                .note_bound(bound_at, env.page.as_deref());
        }

        fn set_value(&mut self, value: &str, at_ms: u64) {
            let proof = self
                .bindings
                .current(&SemanticTargetRef::new(TARGET).unwrap())
                .unwrap()
                .fingerprint_digest
                .clone();
            self.app.apply(
                &meta(900 + at_ms, at_ms),
                &Applied {
                    primitive: Primitive::AxAction {
                        action: AxActionKind::SetValue {
                            value: value.into(),
                        },
                    },
                    binding_fingerprint: Some(proof),
                },
            );
            self.verification.record_input(at_ms, at_ms);
        }

        fn verdict(&mut self) -> Verdict {
            let ctx = EvidenceContext {
                state: &self.state,
                bindings: &self.bindings,
                pack: &self.pack,
                goal: &self.goal,
                now_ms: self.now_ms,
                max_age_ms: 10_000,
            };
            self.verification.verdict(&ctx)
        }

        fn press_tab(&mut self, instance: &str, at_ms: u64) {
            let proof = self
                .app
                .fingerprint_of(&meta(800 + at_ms, at_ms), instance)
                .unwrap();
            self.app.apply(
                &meta(800 + at_ms, at_ms),
                &Applied {
                    primitive: Primitive::AxAction {
                        action: AxActionKind::Press,
                    },
                    binding_fingerprint: Some(proof),
                },
            );
        }
    }

    #[test]
    fn conclusions_after_a_restart_continue_the_numbering_and_never_overwrite() {
        let mut storage = Storage::open_in_memory().unwrap();
        let mut first = Rig::new(ConfirmationScope::UiRevisit);
        first.observe_at(10);
        first.bind();
        let root = RootTaskId::new("root-1").unwrap();
        for _ in 0..2 {
            let ctx = EvidenceContext {
                state: &first.state,
                bindings: &first.bindings,
                pack: &first.pack,
                goal: &first.goal,
                now_ms: first.now_ms,
                max_age_ms: 10_000,
            };
            let verdict = first.verification.verdict(&ctx);
            first
                .verification
                .conclude(&mut storage, &root, &ctx, &verdict, None, 10)
                .unwrap();
            first.observe_at(first.sequence * 20 + 50);
            first.observe_at(first.sequence * 20 + 50);
        }
        let stored = VerificationStore::latest(storage.read(), "root-1")
            .unwrap()
            .expect("something was concluded")
            .revision;

        // A new process has a fresh Verification; left alone it would collide.
        let mut after = Verification::new(0);
        after.resume_after(stored);
        first.observe_at(500);
        let ctx = EvidenceContext {
            state: &first.state,
            bindings: &first.bindings,
            pack: &first.pack,
            goal: &first.goal,
            now_ms: first.now_ms,
            max_age_ms: 10_000,
        };
        let verdict = after.verdict(&ctx);
        let written = after
            .conclude(&mut storage, &root, &ctx, &verdict, None, 500)
            .unwrap()
            .expect("a new conclusion is written");
        assert_eq!(written.revision, stored + 1);
        assert_eq!(
            VerificationStore::history(storage.read(), "root-1")
                .unwrap()
                .len() as u64,
            stored + 1
        );
    }

    #[test]
    fn before_anything_is_observed_everything_is_unknown() {
        let mut rig = Rig::new(ConfirmationScope::UiRevisit);
        let verdict = rig.verdict();
        assert_eq!(verdict.status, TriState::Unknown);
        assert!(verdict.needs.iter().any(|n| n.fact.starts_with("context:")));
    }

    #[test]
    fn a_value_that_differs_is_false_and_the_need_is_input() {
        let mut rig = Rig::new(ConfirmationScope::UiRevisit);
        rig.observe_at(10);
        rig.bind();
        let verdict = rig.verdict();
        assert_eq!(verdict.status, TriState::False, "the switch is still on");
        let need = verdict
            .needs
            .iter()
            .find(|n| n.fact.starts_with("value:"))
            .unwrap();
        assert!(
            need.requires_input,
            "a disagreeing reading needs the change, not just another look"
        );
    }

    #[test]
    fn row_selected_uses_the_fresh_table_selection_not_the_row_object_value() {
        let snapshot = row_snapshot(
            1,
            100,
            Some(true),
            &[(
                "row-read-1",
                ObjectIdentityContinuity::ReadScoped,
                "Quarterly review",
                0.0,
            )],
            true,
        );
        assert!(
            snapshot
                .objects
                .iter()
                .find(|object| object.instance_ref.as_str() == "row-read-1")
                .unwrap()
                .value
                .is_none()
        );

        let pack = AppPack::registered_native("com.lingxi.computer-user.mvp-fixture").unwrap();
        let mut state = StateCore::new();
        state.set_session(1);
        state.admit(snapshot, 101, &pack).unwrap();
        let bindings = BindingReducer::new();
        let goal = row_goal();
        let ctx = EvidenceContext {
            state: &state,
            bindings: &bindings,
            pack: &pack,
            goal: &goal,
            now_ms: 101,
            max_age_ms: 1_000,
        };
        let predicate = Predicate::RowSelected {
            target: goal.target.semantic_target.clone(),
            observed_after_action: true,
        };
        assert_eq!(
            Verification::new(0).evaluate(&predicate, &ctx),
            TriState::True
        );

        let false_snapshot = row_snapshot(
            2,
            100,
            Some(false),
            &[(
                "row-unselected",
                ObjectIdentityContinuity::ReadScoped,
                "Quarterly review",
                0.0,
            )],
            true,
        );
        let mut false_state = StateCore::new();
        false_state.set_session(1);
        false_state.admit(false_snapshot, 101, &pack).unwrap();
        let false_ctx = EvidenceContext {
            state: &false_state,
            bindings: &bindings,
            pack: &pack,
            goal: &goal,
            now_ms: 101,
            max_age_ms: 1_000,
        };
        let mut false_verification = Verification::new(0);
        assert_eq!(
            false_verification.evaluate(&predicate, &false_ctx),
            TriState::False
        );
        assert!(
            false_verification
                .verdict(&false_ctx)
                .needs
                .iter()
                .any(|need| need.fact.starts_with("row-selected:") && need.requires_input)
        );
    }

    #[test]
    fn partial_scope_row_selected_uses_only_the_same_read_scoped_selection() {
        let pack = AppPack::registered_native("com.lingxi.computer-user.mvp-fixture").unwrap();
        let bindings = BindingReducer::new();
        let goal = row_goal();
        let predicate = Predicate::RowSelected {
            target: goal.target.semantic_target.clone(),
            observed_after_action: true,
        };

        for (selected, expected) in [
            (Some(true), TriState::True),
            (Some(false), TriState::False),
            (None, TriState::Unknown),
        ] {
            let mut state = StateCore::new();
            state.set_session(1);
            state
                .admit(partial_row_snapshot(1, 100, selected), 101, &pack)
                .unwrap();
            let ctx = EvidenceContext {
                state: &state,
                bindings: &bindings,
                pack: &pack,
                goal: &goal,
                now_ms: 101,
                max_age_ms: 1_000,
            };
            assert_eq!(
                Verification::new(0).evaluate(&predicate, &ctx),
                expected,
                "a complete scoped census can read the viewport selection without completing the whole tree"
            );
        }

        let mut missing_census = partial_row_snapshot(1, 100, Some(true));
        missing_census.table_scope_search = None;
        let mut state = StateCore::new();
        state.set_session(1);
        state.admit(missing_census, 101, &pack).unwrap();
        let missing_ctx = EvidenceContext {
            state: &state,
            bindings: &bindings,
            pack: &pack,
            goal: &goal,
            now_ms: 101,
            max_age_ms: 1_000,
        };
        assert_eq!(
            Verification::new(0).evaluate(&predicate, &missing_ctx),
            TriState::Unknown,
            "a partial tree without the exact scoped certificate cannot verify the row"
        );

        let stale = partial_row_snapshot(1, 100, Some(true));
        let mut stale_state = StateCore::new();
        stale_state.set_session(1);
        stale_state.admit(stale, 100, &pack).unwrap();
        let stale_ctx = EvidenceContext {
            state: &stale_state,
            bindings: &bindings,
            pack: &pack,
            goal: &goal,
            now_ms: 1_100,
            max_age_ms: 1_000,
        };
        assert_eq!(
            Verification::new(0).evaluate(&predicate, &stale_ctx),
            TriState::Unknown,
            "a scoped certificate is bounded by the normal Source freshness window"
        );

        let same_ms = partial_row_snapshot(1, 100, Some(true));
        let mut same_ms_state = StateCore::new();
        same_ms_state.set_session(1);
        same_ms_state.admit(same_ms, 100, &pack).unwrap();
        let mut after_action = Verification::new(0);
        after_action.note_baseline(TriState::False);
        after_action.record_input(100, 100);
        let same_ms_ctx = EvidenceContext {
            state: &same_ms_state,
            bindings: &bindings,
            pack: &pack,
            goal: &goal,
            now_ms: 101,
            max_age_ms: 1_000,
        };
        assert_eq!(
            after_action.evaluate(&predicate, &same_ms_ctx),
            TriState::Unknown,
            "even a locally complete row census must be observed strictly after the input floor"
        );
    }

    #[test]
    fn row_selected_requires_strictly_new_evidence_and_does_not_claim_row_causality() {
        let pack = AppPack::registered_native("com.lingxi.computer-user.mvp-fixture").unwrap();
        let mut state = StateCore::new();
        state.set_session(1);
        let before = row_snapshot(
            1,
            100,
            Some(false),
            &[(
                "row-read-old",
                ObjectIdentityContinuity::ReadScoped,
                "Quarterly review",
                0.0,
            )],
            true,
        );
        state.admit(before, 100, &pack).unwrap();

        let bindings = BindingReducer::new();
        let goal = row_goal();
        let predicate = Predicate::RowSelected {
            target: goal.target.semantic_target.clone(),
            observed_after_action: true,
        };
        let mut verification = Verification::new(0);
        verification.note_baseline(TriState::False);
        verification.record_input(101, 102);
        let same_floor_ctx = EvidenceContext {
            state: &state,
            bindings: &bindings,
            pack: &pack,
            goal: &goal,
            now_ms: 102,
            max_age_ms: 1_000,
        };
        assert_eq!(
            verification.evaluate(&predicate, &same_floor_ctx),
            TriState::Unknown,
            "a read at the input floor cannot confirm an after-action row state"
        );

        let after = row_snapshot(
            2,
            103,
            Some(true),
            &[(
                "row-read-new",
                ObjectIdentityContinuity::ReadScoped,
                "Quarterly review",
                0.0,
            )],
            true,
        );
        state.admit(after, 103, &pack).unwrap();
        verification.visits.set_target_page(Some("row-page".into()));
        verification.visits.arm(102);
        verification.visits.observe(Some("other-page"), 102);
        verification.visits.observe(Some("row-page"), 103);
        verification.visits.note_bound(103, Some("row-page"));

        let ctx = EvidenceContext {
            state: &state,
            bindings: &bindings,
            pack: &pack,
            goal: &goal,
            now_ms: 104,
            max_age_ms: 1_000,
        };
        assert_eq!(verification.evaluate(&predicate, &ctx), TriState::True);
        let verdict = verification.verdict(&ctx);
        assert_eq!(verdict.status, TriState::True);
        let mut storage = Storage::open_in_memory().unwrap();
        let result = verification
            .conclude(&mut storage, &goal.root_task_id, &ctx, &verdict, None, 104)
            .unwrap()
            .unwrap();
        assert_eq!(
            result.causality,
            Causality::Unknown,
            "a new read-scoped row ref proves current selection, not same-entity causality"
        );
    }

    #[test]
    fn stale_missing_ambiguous_and_locally_incomplete_rows_remain_unknown() {
        let pack = AppPack::registered_native("com.lingxi.computer-user.mvp-fixture").unwrap();
        let bindings = BindingReducer::new();
        let goal = row_goal();
        let predicate = Predicate::RowSelected {
            target: goal.target.semantic_target.clone(),
            observed_after_action: true,
        };
        let evaluate = |snapshot: StructureSnapshot, now_ms: u64, max_age_ms: u64| {
            let mut state = StateCore::new();
            state.set_session(1);
            state.admit(snapshot, now_ms, &pack).unwrap();
            let ctx = EvidenceContext {
                state: &state,
                bindings: &bindings,
                pack: &pack,
                goal: &goal,
                now_ms,
                max_age_ms,
            };
            Verification::new(0).evaluate(&predicate, &ctx)
        };

        let stale = row_snapshot(
            1,
            100,
            Some(true),
            &[(
                "row-stale",
                ObjectIdentityContinuity::NotificationCovered,
                "Quarterly review",
                0.0,
            )],
            true,
        );
        assert_eq!(evaluate(stale, 1_101, 1_000), TriState::Unknown);

        let missing = row_snapshot(
            1,
            100,
            Some(true),
            &[(
                "row-missing",
                ObjectIdentityContinuity::NotificationCovered,
                "Different row",
                0.0,
            )],
            true,
        );
        assert_eq!(evaluate(missing, 101, 1_000), TriState::Unknown);

        let ambiguous = row_snapshot(
            1,
            100,
            Some(true),
            &[
                (
                    "row-ambiguous-a",
                    ObjectIdentityContinuity::NotificationCovered,
                    "Quarterly review",
                    0.0,
                ),
                (
                    "row-ambiguous-b",
                    ObjectIdentityContinuity::NotificationCovered,
                    "Quarterly review",
                    30.0,
                ),
            ],
            true,
        );
        assert_eq!(evaluate(ambiguous, 101, 1_000), TriState::Unknown);

        let incomplete = row_snapshot(
            1,
            100,
            Some(true),
            &[(
                "row-incomplete",
                ObjectIdentityContinuity::NotificationCovered,
                "Quarterly review",
                0.0,
            )],
            false,
        );
        assert_eq!(evaluate(incomplete, 101, 1_000), TriState::Unknown);
    }

    #[test]
    fn a_frame_from_before_the_input_cannot_confirm_the_effect() {
        let mut rig = Rig::new(ConfirmationScope::UiRevisit);
        rig.observe_at(10);
        rig.bind();
        rig.set_value("off", 100);
        // A frame observed at 50 arrives after the input at 100: the floor is 100.
        let ctx_value = |rig: &mut Rig| {
            let value_leaf = Predicate::ReadValueEquals {
                target: SemanticTargetRef::new(TARGET).unwrap(),
                expected: ExpectedValue::Bool { value: false },
                observed_after_action: true,
            };
            let ctx = EvidenceContext {
                state: &rig.state,
                bindings: &rig.bindings,
                pack: &rig.pack,
                goal: &rig.goal,
                now_ms: rig.now_ms,
                max_age_ms: 10_000,
            };
            rig.verification.evaluate(&value_leaf, &ctx)
        };
        assert_eq!(
            ctx_value(&mut rig),
            TriState::Unknown,
            "the only evidence predates the input"
        );
        rig.observe_at(150);
        assert_eq!(ctx_value(&mut rig), TriState::True);
    }

    #[test]
    fn a_satisfied_goal_needs_the_revisit_too_and_the_revisit_needs_a_fresh_binding() {
        let mut rig = Rig::new(ConfirmationScope::UiRevisit);
        rig.observe_at(10);
        rig.bind();
        rig.set_value("off", 100);
        rig.verification.visits.arm(100);
        rig.observe_at(150);
        let verdict = rig.verdict();
        assert_eq!(
            verdict.status,
            TriState::Unknown,
            "value is right, revisit is pending"
        );
        assert!(
            verdict
                .needs
                .iter()
                .any(|n| n.fact.starts_with("revisit:") && n.requires_input)
        );

        rig.press_tab("tab-general", 200);
        rig.observe_at(250);
        assert!(rig.verification.visits.left());
        rig.press_tab("tab-notifications", 300);
        rig.observe_at(350);
        assert!(rig.verification.visits.returned());
        assert!(
            !rig.verification.visits.satisfied(),
            "coming back is not enough without rebinding"
        );
        assert_eq!(rig.verdict().status, TriState::Unknown);

        rig.bind();
        let verdict = rig.verdict();
        assert!(rig.verification.visits.satisfied());
        assert_eq!(verdict.status, TriState::True);
        assert!(verdict.needs.is_empty());
    }

    #[test]
    fn leaving_before_the_change_is_done_does_not_count_as_a_revisit() {
        let mut visits = PageVisits::default();
        visits.set_target_page(Some("Notifications".into()));
        visits.observe(Some("General"), 10);
        assert!(!visits.left(), "not armed yet");
        visits.arm(100);
        visits.observe(Some("General"), 50);
        assert!(!visits.left(), "that frame predates arming");
        visits.observe(Some("General"), 120);
        assert!(visits.left());
    }

    #[test]
    fn an_incomplete_read_proves_nothing() {
        let mut rig = Rig::new(ConfirmationScope::UiRevisit);
        rig.observe_at(10);
        rig.bind();
        rig.sequence += 1;
        let partial = rig.app.partial_snapshot(&meta(rig.sequence, 50), 3);
        rig.state.admit(partial, 50, &rig.pack).unwrap();
        let verdict = rig.verdict();
        assert_eq!(verdict.status, TriState::Unknown);
    }

    #[test]
    fn a_commit_goal_reads_the_save_state_after_the_last_input() {
        let mut rig = Rig::new(ConfirmationScope::Commit);
        rig.observe_at(10);
        rig.bind();
        rig.set_value("off", 100);
        rig.observe_at(150);
        let verdict = rig.verdict();
        assert_eq!(
            verdict.status,
            TriState::False,
            "unsaved changes do not count as committed"
        );
        assert!(
            verdict
                .needs
                .iter()
                .any(|n| n.fact.starts_with("commit:") && n.requires_input)
        );

        let proof = rig.app.fingerprint_of(&meta(0, 0), "btn-apply").unwrap();
        rig.app.apply(
            &meta(700, 200),
            &Applied {
                primitive: Primitive::AxAction {
                    action: AxActionKind::Press,
                },
                binding_fingerprint: Some(proof),
            },
        );
        rig.verification.record_input(200, 200);
        rig.observe_at(250);
        assert_eq!(rig.verdict().status, TriState::True);
    }

    #[test]
    fn conclusions_are_immutable_revisions_and_unchanged_ones_are_not_repeated() {
        let mut rig = Rig::new(ConfirmationScope::UiRevisit);
        let mut storage = Storage::open_in_memory().unwrap();
        let root = RootTaskId::new("root-1").unwrap();
        rig.observe_at(10);
        rig.bind();
        let conclude = |rig: &mut Rig, storage: &mut Storage, now: u64| {
            let verdict = rig.verdict();
            let ctx = EvidenceContext {
                state: &rig.state,
                bindings: &rig.bindings,
                pack: &rig.pack,
                goal: &rig.goal,
                now_ms: 10_000,
                max_age_ms: 10_000,
            };
            rig.verification
                .conclude(storage, &root, &ctx, &verdict, None, now)
                .unwrap()
        };
        let first = conclude(&mut rig, &mut storage, 20).unwrap();
        assert_eq!(
            (first.revision, first.status),
            (1, EffectStatus::Unsatisfied)
        );
        assert!(
            conclude(&mut rig, &mut storage, 21).is_none(),
            "same conclusion, same evidence"
        );

        rig.set_value("off", 100);
        rig.observe_at(150);
        let second = conclude(&mut rig, &mut storage, 160).unwrap();
        assert_eq!(second.revision, 2);
        assert_eq!(second.status, EffectStatus::Unknown);
        assert_eq!(
            second.input_span,
            Some(TimeSpan {
                from_ms: 100,
                to_ms: 100
            })
        );

        let history = VerificationStore::history(storage.read(), "root-1").unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0], first, "the first revision was not rewritten");
    }

    #[test]
    fn a_goal_that_was_already_met_is_not_credited_to_our_input() {
        let mut already = Verification::new(0);
        already.note_baseline(TriState::True);
        already.record_input(10, 20);
        assert_eq!(
            already.causality(EffectStatus::Satisfied),
            Causality::Unknown
        );

        let mut changed = Verification::new(0);
        changed.note_baseline(TriState::False);
        changed.record_input(10, 20);
        assert_eq!(
            changed.causality(EffectStatus::Satisfied),
            Causality::Confirmed
        );
        assert_eq!(changed.causality(EffectStatus::Unknown), Causality::Unknown);

        let untouched = Verification::new(0);
        assert_eq!(
            untouched.causality(EffectStatus::Satisfied),
            Causality::NotApplicable
        );
    }

    fn scoped_receipt_binding(kind: ScopedCommitKind, prefix: &str) -> ScopedCommitBinding {
        ScopedCommitBinding {
            action: (kind == ScopedCommitKind::Manual).then(|| TargetSpec {
                semantic_target: SemanticTargetRef::new("save-search").unwrap(),
                label: "Save".into(),
                group: Some("Search".into()),
                role_hint: Some("AXButton".into()),
                row: None,
            }),
            receipt: ScopedCommitReceiptBinding {
                group: "Search".into(),
                role_hint: "AXStaticText".into(),
                description: "fixture-persistence-receipt".into(),
                kind,
                text_prefix: prefix.into(),
            },
        }
    }

    fn scoped_receipt_object(
        id: &str,
        group: &str,
        role: &str,
        description: &str,
        text: &str,
        in_view: bool,
    ) -> UiObject {
        UiObject {
            instance_ref: ObjectRef::new(id).unwrap(),
            parent: None,
            window_id: "window-1".into(),
            role: role.into(),
            subrole: None,
            title: None,
            description: Some(description.into()),
            value: Some(UiValue::Text(text.into())),
            enabled: true,
            focused: false,
            frame: Rect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 20.0,
            },
            actions: Vec::new(),
            ax_action_names: None,
            in_view,
            group: Some(group.into()),
            secure: false,
            scroll: None,
            identity_continuity: ObjectIdentityContinuity::NotificationCovered,
        }
    }

    #[test]
    fn scoped_commit_receipt_requires_exact_unique_visible_source_identity() {
        let binding =
            scoped_receipt_binding(ScopedCommitKind::Manual, "Saved manually · revision ");
        let receipt = &binding.receipt;
        let exact = scoped_receipt_object(
            "receipt",
            "Search",
            "AXStaticText",
            "fixture-persistence-receipt",
            "Saved manually · revision 5",
            true,
        );
        assert_eq!(
            scoped_commit_receipt_text(std::slice::from_ref(&exact), "window-1", receipt),
            Some("Saved manually · revision 5")
        );
        assert_eq!(
            scoped_commit_receipt_text(
                &[scoped_receipt_object(
                    "wrong-group",
                    "Notifications",
                    "AXStaticText",
                    "fixture-persistence-receipt",
                    "Saved manually · revision 6",
                    true,
                )],
                "window-1",
                receipt,
            ),
            None,
            "a same-looking receipt in another group is not evidence"
        );
        let offscreen_duplicate = scoped_receipt_object(
            "offscreen-duplicate",
            "Search",
            "AXStaticText",
            "fixture-persistence-receipt",
            "Saved manually · revision 5",
            false,
        );
        assert_eq!(
            scoped_commit_receipt_text(&[exact, offscreen_duplicate], "window-1", receipt),
            None,
            "an offscreen duplicate makes the receipt ambiguous"
        );
    }

    #[test]
    fn scoped_commit_receipt_parsing_rejects_wrong_kind_malformed_and_stale_revisions() {
        let manual = scoped_receipt_binding(ScopedCommitKind::Manual, "Saved manually · revision ");
        let (status, read) =
            interpret_scoped_commit_receipt(&manual, "Saved manually · revision 5");
        assert_eq!(status, ScopedCommitReceiptStatus::Confirmed);
        assert_eq!(read.revision, Some(5));
        assert!(scoped_commit_revision_advanced(5, Some(Some(4))));
        assert!(!scoped_commit_revision_advanced(4, Some(Some(4))));
        assert!(!scoped_commit_revision_advanced(3, Some(Some(4))));
        assert!(scoped_commit_revision_advanced(1, Some(None)));
        assert!(!scoped_commit_revision_advanced(5, None));

        let automatic = scoped_receipt_binding(
            ScopedCommitKind::Automatic,
            "Saved automatically · revision ",
        );
        assert_eq!(
            interpret_scoped_commit_receipt(&automatic, "Saving changes…").0,
            ScopedCommitReceiptStatus::Pending
        );
        let (_, wrong_kind) =
            interpret_scoped_commit_receipt(&manual, "Saved automatically · revision 6");
        assert_eq!(wrong_kind.revision, None);
        assert!(!wrong_kind.matched_text);
        assert_eq!(
            scoped_commit_revision("Saved manually · revision 0", &manual.receipt.text_prefix),
            None
        );
        assert_eq!(
            scoped_commit_revision(
                "Saved manually · revision 6 unexpected",
                &manual.receipt.text_prefix
            ),
            None
        );
    }
}

#[cfg(test)]
mod browser_current_ui_verification;

#[cfg(test)]
mod uncertainty_tests {
    use super::*;
    use interaction_contracts::desktop::effect::EffectClass;
    use interaction_contracts::desktop::task::{RootBudgetSpec, TaskScope};
    #[test]
    fn unknown_input_advances_evidence_floor_without_inventing_a_known_span() {
        let mut verify = Verification::new(10);
        verify.note_baseline(TriState::False);
        verify.record_unknown_input(40);
        verify.record_unknown_input(30);
        assert_eq!(verify.evidence_floor_ms(), 40);
        assert_eq!(verify.first_input_ms, None);
        assert_eq!(verify.last_input_end_ms(), None);
        assert_eq!(
            verify.causality(EffectStatus::Satisfied),
            Causality::Unknown
        );
    }
    #[test]
    fn known_span_never_erases_uncertainty_from_another_input() {
        let mut verify = Verification::new(10);
        verify.note_baseline(TriState::False);
        verify.record_input(20, 25);
        assert_eq!(
            verify.causality(EffectStatus::Satisfied),
            Causality::Confirmed
        );
        verify.record_unknown_input(35);
        verify.record_input(40, 45);
        assert_eq!(verify.evidence_floor_ms(), 45);
        assert_eq!(
            verify.causality(EffectStatus::Satisfied),
            Causality::Unknown
        );
    }

    #[test]
    fn background_read_scoped_value_is_readable_but_causality_stays_unknown() {
        let (before, after) =
            crate::reaction::skills::background_readback_test_support::snapshots();
        let target = crate::reaction::skills::background_readback_test_support::target();
        let pack = crate::reaction::skills::background_readback_test_support::pack();
        let mut state = StateCore::new();
        state.set_session(before.session_epoch);
        state
            .admit(before.clone(), before.observed_ms, &pack)
            .expect("captured Source projection is admissible");

        let mut bindings = BindingReducer::new();
        let input_binding = bindings
            .bind_background_ax(
                &state.latest().unwrap().clone(),
                &target,
                before.observed_ms,
                2_000,
            )
            .expect("the input read binds the registered target");
        let mut verification = Verification::new(before.observed_ms);
        verification.note_baseline(TriState::False);
        verification.record_input(before.observed_ms, before.observed_ms + 2);
        verification.record_background_action_input(
            input_binding.clone(),
            before.source_sequence,
            match &input_binding.device_context {
                interaction_contracts::desktop::binding::DeviceContextWitness::BackgroundAxV1 {
                    target_context_hash,
                    ..
                } => target_context_hash.clone(),
                _ => panic!("background binding carries its typed witness"),
            },
            before.observed_ms + 2,
        );

        state
            .admit(after.clone(), after.observed_ms, &pack)
            .expect("the later Source projection is admissible");
        let env = state.latest().unwrap().clone();
        bindings.revalidate(&env);
        bindings
            .bind_background_ax(&env, &target, after.observed_ms, 2_000)
            .expect("the later read independently binds the same frozen semantics");

        let semantic = target.semantic_target.clone();
        let goal = GoalSpec {
            root_task_id: RootTaskId::new("background-readback-root").unwrap(),
            task_revision: 1,
            execution_domain: interaction_contracts::desktop::domain::ExecutionDomain::BackgroundAx,
            browser_page_profile: None,
            user_text: "turn on Background toggle".into(),
            scope: TaskScope {
                app: crate::reaction::skills::background_readback_test_support::APP.into(),
                account: None,
            },
            allowed_scope: None,
            scoped_revisit: None,
            scoped_commit: None,
            target,
            desired: ExpectedValue::Bool { value: true },
            confirmation_scope: ConfirmationScope::CurrentUi,
            max_effect_class: EffectClass::ReversibleEdit,
            required: Predicate::And {
                of: vec![
                    Predicate::ContextMatches {
                        app: crate::reaction::skills::background_readback_test_support::APP.into(),
                    },
                    Predicate::ReadValueEquals {
                        target: semantic,
                        expected: ExpectedValue::Bool { value: true },
                        observed_after_action: true,
                    },
                ],
            },
            budget: RootBudgetSpec {
                deadline_ms: 10_000,
                max_attempts: 1,
                max_observations: 3,
                max_recoveries: 0,
                max_replans: 0,
            },
        };
        let ctx = EvidenceContext {
            state: &state,
            bindings: &bindings,
            pack: &pack,
            goal: &goal,
            now_ms: after.observed_ms,
            max_age_ms: 2_000,
        };
        assert_eq!(
            verification.background_target_value(
                &ctx,
                state.latest().unwrap(),
                &goal.target.semantic_target,
                true,
            ),
            Some(UiValue::Bool(true)),
            "a fresh independent Source read may prove the frozen target's current value"
        );
        assert_eq!(
            verification.causality(EffectStatus::Satisfied),
            Causality::Unknown,
            "read-scoped semantics do not claim native identity or input causality"
        );
    }
}
