//! One anchored ShowMenu followed by one exact-label AXPress, never replayed.
use interaction_contracts::desktop::binding::{
    BindRequest, BindScope, BindingOutcome, TargetDescription, instance_fingerprint,
};
use interaction_contracts::desktop::ids::RequestId;
use interaction_contracts::desktop::source::{MenuContext, MenuOpenerAnchor};
use sha2::{Digest, Sha256};

use super::*;
use crate::environment::read::focused_window_id;

#[derive(Debug, Clone)]
pub(super) struct MenuRun {
    spec: SelectByLabelSpec,
    anchor: Option<MenuOpenerAnchor>,
    context: Option<MenuContext>,
    initial_epoch: Option<u64>,
    last_sequence: Option<u64>,
    opening_end_ms: Option<u64>,
    read_only_result: Option<ReadOnlyResult>,
}

/// Frozen after the known final input. This authorizes only reading a newly
/// bound semantic opener; it is never an input or a Source-anchor continuation.
#[derive(Debug, Clone)]
struct ReadOnlyResult {
    anchor: MenuOpenerAnchor,
    session_epoch: u64,
    sequence_floor: u64,
    input_end_ms: u64,
}

fn same_opener_semantics(a: &MenuOpenerAnchor, b: &MenuOpenerAnchor) -> bool {
    let (a, b) = (&a.opener_fingerprint, &b.opener_fingerprint);
    a.app == b.app
        && a.app_pid == b.app_pid
        && a.role == b.role
        && a.subrole == b.subrole
        && a.group == b.group
        && a.label == b.label
        && a.ancestor_roles == b.ancestor_roles
        && a.sibling_index == b.sibling_index
        && a.scroll_container == b.scroll_container
}

impl MenuRun {
    pub(super) fn new(spec: SelectByLabelSpec) -> Self {
        Self {
            spec,
            anchor: None,
            context: None,
            initial_epoch: None,
            last_sequence: None,
            opening_end_ms: None,
            read_only_result: None,
        }
    }
}

fn current_anchor<'a>(
    env: &'a EnvSnapshot,
    retained: &MenuOpenerAnchor,
) -> Option<&'a MenuOpenerAnchor> {
    let mut matches = env.snapshot.menu_anchors.iter().filter(|anchor| {
        anchor.same_identity(retained)
            && anchor.verified_source_sequence == env.snapshot.source_sequence
            && anchor.verified_observed_ms == env.snapshot.observed_ms
    });
    let result = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    let object = env
        .snapshot
        .objects
        .iter()
        .find(|object| object.instance_ref == result.opener_instance)?;
    (result.validate().is_ok()
        && object.enabled
        && object.in_view
        && !object.secure
        && object.frame == result.opener_frame
        && instance_fingerprint(&env.snapshot, object) == result.opener_fingerprint
        && focused_window_id(&env.snapshot) == Some(result.origin_window_id.as_str()))
    .then_some(result)
}

impl SkillRun {
    pub(crate) fn menu_target(&self) -> Option<&SemanticTargetRef> {
        (self.skill == Skill::SelectByLabel).then_some(&self.target)
    }
    pub(crate) fn menu_desired(&self) -> Option<&ExpectedValue> {
        if self.skill == Skill::SelectByLabel {
            self.desired.as_ref()
        } else {
            None
        }
    }
    /// Source-owned values only. Engine adds the actual opening intent/attempt
    /// identity when constructing the canonical execution proof.
    pub(crate) fn menu_anchor(&self) -> Option<&MenuOpenerAnchor> {
        self.menu.as_ref()?.anchor.as_ref()
    }
    pub(crate) fn menu_context(&self) -> Option<&MenuContext> {
        self.menu.as_ref()?.context.as_ref()
    }
    pub(crate) fn menu_opening_end_ms(&self) -> Option<u64> {
        self.menu.as_ref()?.opening_end_ms
    }
    pub(crate) fn menu_same_watch_required(&self) -> bool {
        self.skill == Skill::SelectByLabel
            && self.actions > 0
            && !self.is_finished()
            && !self.menu_readonly_result_ready()
    }
    pub(crate) fn menu_readonly_result_ready(&self) -> bool {
        self.skill == Skill::SelectByLabel
            && self.actions == 2
            && !self.is_finished()
            && matches!(
                self.phase,
                Phase::Judging(Follow::ChooseMenuItem, Known { unknown: false, .. })
            )
    }
    /// Engine repeats this independently at result verification. It remains
    /// meaningful after Succeeded; it never grants a permit or another input.
    pub(crate) fn menu_readonly_result_scope_matches(
        &self,
        env: &EnvSnapshot,
        binding: &Binding,
    ) -> bool {
        let Some(scope) = self
            .menu
            .as_ref()
            .and_then(|menu| menu.read_only_result.as_ref())
        else {
            return false;
        };
        let snapshot = &env.snapshot;
        if self.skill != Skill::SelectByLabel
            || !snapshot.complete
            || snapshot.menu_context.is_some()
            || snapshot.validate().is_err()
            || snapshot.session_epoch != scope.session_epoch
            || snapshot.app.bundle_id != scope.anchor.opener_fingerprint.app
            || snapshot.app.pid != scope.anchor.process.pid
            || focused_window_id(snapshot) != Some(scope.anchor.origin_window_id.as_str())
            || snapshot.source_sequence <= scope.sequence_floor
            || snapshot.observed_ms <= scope.input_end_ms
            || binding.semantic_target != self.target
            || binding.env_revision != env.revision
            || binding.layer_revision != env.layer_revision
            || binding.snapshot_id != snapshot.snapshot_id
            || binding.observed_ms != snapshot.observed_ms
        {
            return false;
        }
        let Some(object) = snapshot
            .objects
            .iter()
            .find(|object| object.instance_ref == binding.instance)
        else {
            return false;
        };
        if !object.enabled
            || !object.in_view
            || object.secure
            || object.window_id != scope.anchor.origin_window_id
        {
            return false;
        }
        let fp = instance_fingerprint(snapshot, object);
        let matching: Vec<_> = snapshot
            .menu_anchors
            .iter()
            .filter(|anchor| {
                anchor.process == scope.anchor.process
                    && anchor.origin_window_id == scope.anchor.origin_window_id
                    && anchor.origin_window_frame == scope.anchor.origin_window_frame
                    && same_opener_semantics(anchor, &scope.anchor)
                    && anchor.opener_instance == object.instance_ref
                    && anchor.opener_fingerprint == fp
                    && anchor.opener_frame == object.frame
                    && anchor.verified_source_sequence == snapshot.source_sequence
                    && anchor.verified_observed_ms == snapshot.observed_ms
                    && anchor.validate().is_ok()
            })
            .collect();
        matching.len() == 1 && fp.digest() == binding.fingerprint_digest
    }

    pub(super) fn advance_menu(&mut self, view: &mut SkillView<'_>) -> SkillStep {
        if let Phase::Ended(result) = &self.phase {
            return SkillStep::Done(result.clone());
        }
        if self.menu_readonly_result_ready() {
            let Phase::Judging(Follow::ChooseMenuItem, known) = self.phase else {
                unreachable!()
            };
            if let Some(menu) = self.menu.as_mut()
                && menu.read_only_result.is_none()
                && let (Some(anchor), Some(session_epoch), Some(sequence_floor)) =
                    (&menu.anchor, menu.initial_epoch, menu.last_sequence)
            {
                menu.read_only_result = Some(ReadOnlyResult {
                    anchor: anchor.clone(),
                    session_epoch,
                    sequence_floor,
                    input_end_ms: known.end_ms,
                });
            }
        }
        let step = self.menu_step(view);
        if let SkillStep::Done(result) = &step {
            self.phase = Phase::Ended(result.clone());
        }
        step
    }

    fn menu_step(&mut self, view: &mut SkillView<'_>) -> SkillStep {
        let Some(menu) = self.menu.as_ref() else {
            return fail(
                FailureCode::ContextUnresolved,
                "menu selection has no typed choice",
            );
        };
        if menu.spec.validate().is_err()
            || !matches!(&self.desired, Some(ExpectedValue::Text { value }) if value == &menu.spec.choice_label)
        {
            return fail(
                FailureCode::ContextUnresolved,
                "menu choice must equal the frozen text result",
            );
        }
        let known = match self.phase {
            Phase::Start => None,
            Phase::Judging(Follow::OpenMenu | Follow::ChooseMenuItem, known) => Some(known),
            _ => {
                return fail(
                    FailureCode::ContextUnresolved,
                    "the previous menu input has no reported outcome",
                );
            }
        };
        if matches!(
            self.phase,
            Phase::Judging(Follow::OpenMenu, Known { unknown: true, .. })
        ) {
            return fail(
                FailureCode::EffectUnknown,
                "ShowMenu is unknown; no choice input may follow",
            );
        }
        let Some(env) = view.state.latest() else {
            return SkillStep::AwaitEvidence {
                after_ms: self.input_floor.unwrap_or(0),
            };
        };
        // Ordinary registry generations may change, but the retained Source
        // AX anchor and the watch session must not be replaced or resurrected.
        if menu
            .initial_epoch
            .is_some_and(|epoch| epoch != env.snapshot.session_epoch)
        {
            return fail(
                if known.is_some_and(|known| known.unknown) {
                    FailureCode::EffectUnknown
                } else {
                    FailureCode::ContextUnresolved
                },
                "the menu source epoch changed",
            );
        }
        if !env.snapshot.complete
            || focused_window_id(&env.snapshot).is_none()
            || env.snapshot.observed_ms > view.now_ms
            || view.now_ms.saturating_sub(env.snapshot.observed_ms) > view.max_age_ms
            || known.is_some_and(|known| env.snapshot.observed_ms <= known.end_ms)
            || menu
                .last_sequence
                .is_some_and(|last| env.snapshot.source_sequence <= last)
        {
            return SkillStep::AwaitEvidence {
                after_ms: self
                    .input_floor
                    .unwrap_or(0)
                    .max(env.snapshot.observed_ms.saturating_add(1)),
            };
        }
        match self.phase {
            Phase::Start => self.menu_start(view, env),
            Phase::Judging(Follow::OpenMenu, known) => self.menu_choose(view, env, known),
            Phase::Judging(Follow::ChooseMenuItem, known) => self.menu_verify(view, env, known),
            _ => unreachable!("menu phase was screened above"),
        }
    }

    fn menu_start(&mut self, view: &mut SkillView<'_>, env: &EnvSnapshot) -> SkillStep {
        if env.snapshot.menu_context.is_some() {
            return fail(
                FailureCode::ContextUnresolved,
                "a menu is already open outside this attempt",
            );
        }
        if !view.state.usable(view.now_ms, view.max_age_ms) {
            return SkillStep::AwaitEvidence {
                after_ms: env.snapshot.observed_ms.saturating_add(1),
            };
        }
        let binding = match self.bound(view) {
            Ok(binding) => binding,
            Err(step) => return step,
        };
        let Some(object) = object_for(env, &binding.fingerprint_digest) else {
            return fail(
                FailureCode::ContextUnresolved,
                "the menu opener binding changed",
            );
        };
        if !object.enabled
            || !object.in_view
            || object.secure
            || !matches!(object.role.as_str(), "AXMenuButton" | "AXPopUpButton")
            || !object.actions.contains(&AxActionName::ShowMenu)
        {
            return fail(
                FailureCode::ContextUnresolved,
                "the opener cannot perform native ShowMenu",
            );
        }
        let anchors: Vec<_> = env
            .snapshot
            .menu_anchors
            .iter()
            .filter(|anchor| {
                anchor.opener_instance == binding.instance
                    && anchor.opener_fingerprint.digest() == binding.fingerprint_digest
            })
            .collect();
        if anchors.len() != 1 || current_anchor(env, anchors[0]).is_none() {
            return fail(
                FailureCode::ContextUnresolved,
                "the opener lacks a unique current Source anchor",
            );
        }
        let menu = self.menu.as_mut().unwrap();
        menu.anchor = Some(anchors[0].clone());
        menu.initial_epoch = Some(env.snapshot.session_epoch);
        menu.last_sequence = Some(env.snapshot.source_sequence);
        if object
            .value
            .as_ref()
            .and_then(|value| values_equal(value, self.desired.as_ref().unwrap()))
            == Some(true)
        {
            return SkillStep::Done(SkillResult::AlreadyDone);
        }
        self.act(
            binding,
            Primitive::AxAction {
                action: AxActionKind::ShowMenu,
            },
            ExecutionPath::Ax,
            Follow::OpenMenu,
            "open this anchored control's menu once",
        )
    }

    fn menu_choose(
        &mut self,
        view: &mut SkillView<'_>,
        env: &EnvSnapshot,
        known: Known,
    ) -> SkillStep {
        let retained = self.menu.as_ref().unwrap().anchor.as_ref().unwrap();
        let Some(anchor) = current_anchor(env, retained) else {
            return fail(
                FailureCode::ContextUnresolved,
                "the menu opener anchor or source session changed",
            );
        };
        let Some(context) = &env.snapshot.menu_context else {
            return fail(
                FailureCode::EffectUnknown,
                "ShowMenu completed without a current proven menu",
            );
        };
        if context.validate().is_err()
            || !context.opener.same_identity(anchor)
            || context.opener != *anchor
            || !view.state.menu_usable(view.now_ms, view.max_age_ms)
        {
            return fail(
                FailureCode::ContextUnresolved,
                "the current menu is not the opened anchor's input context",
            );
        }
        let menu = self.menu.as_mut().unwrap();
        menu.anchor = Some(anchor.clone());
        menu.context = Some(context.clone());
        menu.opening_end_ms = Some(known.end_ms);
        menu.last_sequence = Some(env.snapshot.source_sequence);
        let wanted = normalize_label(&menu.spec.choice_label);
        let exact_choices = env
            .snapshot
            .objects
            .iter()
            .filter(|object| {
                object.role == "AXMenuItem"
                    && object.parent.as_ref() == Some(&context.menu_instance)
                    && normalize_label(label_of(object)) == wanted
            })
            .count();
        if exact_choices != 1 {
            return fail(
                if exact_choices > 1 {
                    FailureCode::TargetAmbiguous
                } else {
                    FailureCode::CandidateMissing
                },
                "the current menu does not contain one exact choice label",
            );
        }
        // Owner-generated identity is bounded even for maximum-length binding
        // IDs. Planner parameters contain only the literal choice label.
        let digest = Sha256::digest(self.binding.as_str().as_bytes());
        let semantic = SemanticTargetRef::new(format!("menu-choice-{digest:x}")).unwrap();
        let request = BindRequest {
            request_id: RequestId::new(format!("menu-choice-{}", env.revision)).unwrap(),
            semantic_target: semantic.clone(),
            description: TargetDescription {
                label: menu.spec.choice_label.clone(),
                group: None,
                role_hint: Some("AXMenuItem".into()),
            },
            scope: BindScope {
                app: env.snapshot.app.bundle_id.clone(),
                account: None,
            },
            choice: None,
            max_age_ms: view.max_age_ms,
        };
        let result = view.bindings.bind_in_menu(env, &request, context);
        if !matches!(result.outcome, BindingOutcome::Bound { .. }) {
            let code = match result.outcome {
                BindingOutcome::Rejected { code, .. } => code,
                BindingOutcome::Ambiguous { .. } => FailureCode::TargetAmbiguous,
                _ => FailureCode::ContextUnresolved,
            };
            return fail(
                code,
                "the exact menu choice cannot be bound uniquely inside this menu",
            );
        }
        let Some(binding) = view.bindings.current(&semantic).cloned() else {
            return fail(
                FailureCode::ContextUnresolved,
                "the chosen menu binding is unavailable",
            );
        };
        let Some(object) = env
            .snapshot
            .objects
            .iter()
            .find(|object| object.instance_ref == binding.instance)
        else {
            return fail(
                FailureCode::ContextUnresolved,
                "the bound menu choice disappeared",
            );
        };
        if object.role != "AXMenuItem"
            || object.parent.as_ref() != Some(&context.menu_instance)
            || !object.enabled
            || !object.in_view
            || object.secure
            || !object.actions.contains(&AxActionName::Press)
            || normalize_label(label_of(object)) != normalize_label(&menu.spec.choice_label)
            || instance_fingerprint(&env.snapshot, object).digest() != binding.fingerprint_digest
        {
            return fail(
                FailureCode::ContextUnresolved,
                "the choice is not a visible enabled direct AXPress item",
            );
        }
        self.act(
            binding,
            Primitive::AxAction {
                action: AxActionKind::Press,
            },
            ExecutionPath::Ax,
            Follow::ChooseMenuItem,
            "press the exact label in the current anchored menu once",
        )
    }

    fn menu_verify(
        &mut self,
        view: &mut SkillView<'_>,
        env: &EnvSnapshot,
        known: Known,
    ) -> SkillStep {
        if !known.unknown {
            return self.menu_readonly_result(view, env);
        }
        let menu = self.menu.as_ref().unwrap();
        let Some(anchor) = current_anchor(env, menu.anchor.as_ref().unwrap()) else {
            return fail(
                FailureCode::EffectUnknown,
                "the result opener's protected Source identity changed",
            );
        };
        if let Some(context) = &env.snapshot.menu_context {
            return fail(
                FailureCode::EffectUnknown,
                if menu
                    .context
                    .as_ref()
                    .is_some_and(|opened| opened.same_identity(context))
                {
                    "the unknown selection left the menu open; no input is repeated"
                } else {
                    "the unknown selection's menu context changed; no input is repeated"
                },
            );
        }
        if !view.state.usable(view.now_ms, view.max_age_ms) {
            return SkillStep::AwaitEvidence {
                after_ms: env.snapshot.observed_ms.saturating_add(1),
            };
        }
        let opener = env
            .snapshot
            .objects
            .iter()
            .find(|object| object.instance_ref == anchor.opener_instance)
            .unwrap();
        if opener
            .value
            .as_ref()
            .and_then(|value| values_equal(value, self.desired.as_ref().unwrap()))
            == Some(true)
        {
            fail(
                FailureCode::EffectUnknown,
                "the actual value now matches, but the selection input remains unknown and isolated",
            )
        } else {
            fail(
                FailureCode::EffectUnknown,
                if known.unknown {
                    "the unknown choice was not proved by the closed menu and actual opener value"
                } else {
                    "the closed menu's actual opener value does not equal the selected label"
                },
            )
        }
    }

    fn menu_readonly_result(&mut self, view: &mut SkillView<'_>, env: &EnvSnapshot) -> SkillStep {
        let Some(scope) = self
            .menu
            .as_ref()
            .and_then(|menu| menu.read_only_result.as_ref())
        else {
            return fail(
                FailureCode::ContextUnresolved,
                "the final menu input lacks a frozen readonly result scope",
            );
        };
        if env.snapshot.menu_context.is_some() {
            return fail(
                FailureCode::EffectUnknown,
                "the selected menu is still open; no result input is allowed",
            );
        }
        if !view.state.usable(view.now_ms, view.max_age_ms) {
            return SkillStep::AwaitEvidence {
                after_ms: env.snapshot.observed_ms.saturating_add(1),
            };
        }
        let descriptor = &scope.anchor.opener_fingerprint;
        let exact: Vec<_> = env
            .snapshot
            .objects
            .iter()
            .filter(|object| {
                object.window_id == scope.anchor.origin_window_id
                    && object.enabled
                    && object.in_view
                    && !object.secure
                    && object.role == descriptor.role
                    && object.subrole == descriptor.subrole
                    && normalize_label(label_of(object)) == descriptor.label
                    && object.group.as_deref().map(normalize_label) == descriptor.group
            })
            .collect();
        if exact.len() != 1 {
            return fail(
                if exact.len() > 1 {
                    FailureCode::TargetAmbiguous
                } else {
                    FailureCode::ContextUnresolved
                },
                "the readonly result opener is not semantically unique",
            );
        }
        let request = BindRequest {
            request_id: RequestId::new(format!("menu-result-{}", env.revision)).unwrap(),
            semantic_target: self.target.clone(),
            description: TargetDescription {
                label: descriptor.label.clone(),
                group: descriptor.group.clone(),
                role_hint: Some(descriptor.role.clone()),
            },
            scope: BindScope {
                app: descriptor.app.clone(),
                account: None,
            },
            choice: None,
            max_age_ms: view.max_age_ms,
        };
        if !matches!(
            view.bindings.bind(env, &request, view.now_ms).outcome,
            BindingOutcome::Bound { .. }
        ) {
            return fail(
                FailureCode::ContextUnresolved,
                "the readonly result opener could not be freshly bound",
            );
        }
        let binding = view.bindings.current(&self.target).unwrap().clone();
        if !self.menu_readonly_result_scope_matches(env, &binding) {
            return fail(
                FailureCode::ContextUnresolved,
                "the readonly result lacks same-kernel-process and original-window Source proof",
            );
        }
        self.binding = binding.binding_ref.clone();
        let opener = env
            .snapshot
            .objects
            .iter()
            .find(|object| object.instance_ref == binding.instance)
            .unwrap();
        if opener
            .value
            .as_ref()
            .and_then(|value| values_equal(value, self.desired.as_ref().unwrap()))
            == Some(true)
        {
            SkillStep::Done(SkillResult::Succeeded)
        } else {
            fail(
                FailureCode::EffectUnknown,
                "the closed menu's actual fresh opener value does not equal the selected label",
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{EnvRig, meta};
    use interaction_contracts::desktop::ids::ObjectRef;
    use interaction_contracts::desktop::source::{
        GlobalFocusObservation, KernelProcessIdentity, MenuAnchorContinuity, MenuDrawingWindow,
        ObservationFact, Rect, StructureSnapshot, UiValue,
    };

    struct Rig {
        env: EnvRig,
        template: StructureSnapshot,
        run: SkillRun,
    }
    impl Rig {
        fn new(value: &str) -> Self {
            let mut env = EnvRig::new();
            let mut template = env.app.snapshot(&meta(1, 100));
            let window = template.windows[0].clone();
            let mut root = template.objects[0].clone();
            root.instance_ref = ObjectRef::new("menu-root").unwrap();
            root.parent = None;
            root.role = "AXWindow".into();
            root.subrole = Some("AXStandardWindow".into());
            root.title = window.title.clone();
            root.description = None;
            root.group = None;
            root.window_id = window.window_id.clone();
            root.frame = window.frame;
            root.enabled = true;
            root.in_view = true;
            root.secure = false;
            root.scroll = None;
            root.actions.clear();
            root.value = None;
            let mut opener = root.clone();
            opener.instance_ref = ObjectRef::new("menu-opener").unwrap();
            opener.parent = Some(root.instance_ref.clone());
            opener.role = "AXMenuButton".into();
            opener.subrole = None;
            opener.title = Some("Locale".into());
            opener.value = Some(UiValue::Text(value.into()));
            opener.frame = Rect {
                x: window.frame.x + 20.0,
                y: window.frame.y + 40.0,
                width: 160.0,
                height: 30.0,
            };
            opener.actions = vec![AxActionName::ShowMenu];
            template.objects = vec![root, opener];
            // A menu opener is only valid in a complete read that proves the
            // same app/window is globally focused. The fixture models that
            // foreground state explicitly rather than relying on an old
            // active-layer shortcut.
            template.global_focus = GlobalFocusObservation {
                app: ObservationFact::Known {
                    value: template.app.clone(),
                },
                window_id: ObservationFact::Known {
                    value: window.window_id.clone(),
                },
            };
            sim_world::app::refresh_simulated_observer_coverage(&mut template);
            template.menu_anchors = vec![Self::anchor(&template, "watch-one", "anchor-one")];
            template.menu_context = None;
            env.state.admit(template.clone(), 100, &env.pack).unwrap();
            env.now_ms = 100;
            let target = SemanticTargetRef::new("locale").unwrap();
            let binding = env
                .bindings
                .bind_instance(
                    env.state.latest().unwrap(),
                    target.clone(),
                    &template.objects[1].instance_ref,
                )
                .unwrap();
            let run = SkillRun::new(
                Skill::SelectByLabel,
                target,
                Some(ExpectedValue::Text {
                    value: "Choice B".into(),
                }),
                binding.binding_ref,
            )
            .with_select(SelectByLabelSpec {
                choice_label: "Choice B".into(),
            });
            Self { env, template, run }
        }
        fn anchor(snapshot: &StructureSnapshot, session: &str, id: &str) -> MenuOpenerAnchor {
            let opener = &snapshot.objects[1];
            MenuOpenerAnchor {
                anchor_id: id.into(),
                source_session_id: session.into(),
                anchor_revision: 1,
                process: KernelProcessIdentity {
                    pid: snapshot.app.pid,
                    start_seconds: 1,
                    start_micros: 0,
                },
                origin_window_id: opener.window_id.clone(),
                origin_window_frame: snapshot.windows[0].frame,
                opener_instance: opener.instance_ref.clone(),
                opener_fingerprint: instance_fingerprint(snapshot, opener),
                opener_frame: opener.frame,
                verified_source_sequence: snapshot.source_sequence,
                verified_observed_ms: snapshot.observed_ms,
                continuity: MenuAnchorContinuity::DirectRead,
            }
        }
        fn snapshot(
            &self,
            open: bool,
            value: &str,
            sequence: u64,
            observed_ms: u64,
        ) -> StructureSnapshot {
            let mut snapshot = self.template.clone();
            snapshot.source_sequence = sequence;
            snapshot.observed_ms = observed_ms;
            snapshot.snapshot_id = RequestId::new(format!("menu-snapshot-{sequence}")).unwrap();
            snapshot.objects[1].value = Some(UiValue::Text(value.into()));
            snapshot.menu_anchors = vec![Self::anchor(&snapshot, "watch-one", "anchor-one")];
            if open {
                let mut menu = snapshot.objects[1].clone();
                menu.instance_ref = ObjectRef::new("visible-menu").unwrap();
                menu.parent = Some(snapshot.objects[1].instance_ref.clone());
                menu.role = "AXMenu".into();
                menu.title = Some("Locale menu".into());
                menu.value = None;
                menu.actions.clear();
                menu.identity_continuity =
                    interaction_contracts::desktop::source::ObjectIdentityContinuity::ReadScoped;
                menu.frame.y += menu.frame.height + 2.0;
                menu.frame.width = 180.0;
                menu.frame.height = 60.0;
                snapshot.objects.push(menu.clone());
                for (index, label) in ["Choice A", "Choice B"].into_iter().enumerate() {
                    let mut item = menu.clone();
                    item.instance_ref = ObjectRef::new(format!("menu-choice-{index}")).unwrap();
                    item.parent = Some(menu.instance_ref.clone());
                    item.role = "AXMenuItem".into();
                    item.title = Some(label.into());
                    item.frame.y += 5.0 + index as f64 * 25.0;
                    item.frame.height = 25.0;
                    item.actions = vec![AxActionName::Press];
                    item.identity_continuity =
                        interaction_contracts::desktop::source::ObjectIdentityContinuity::ReadScoped;
                    snapshot.objects.push(item);
                }
                snapshot.menu_context = Some(MenuContext {
                    context_id: "context-one".into(),
                    source_session_id: "watch-one".into(),
                    opener: snapshot.menu_anchors[0].clone(),
                    menu_instance: menu.instance_ref.clone(),
                    menu_fingerprint: instance_fingerprint(&snapshot, &menu),
                    menu_frame: menu.frame,
                    drawing_window: MenuDrawingWindow {
                        window_id: 101,
                        owner_pid: snapshot.app.pid,
                        layer: 101,
                        frame: menu.frame,
                    },
                });
            }
            sim_world::app::refresh_simulated_observer_coverage(&mut snapshot);
            snapshot
        }
        fn admit(&mut self, snapshot: StructureSnapshot) {
            self.env.now_ms = snapshot.observed_ms;
            self.env
                .state
                .admit(snapshot, self.env.now_ms, &self.env.pack)
                .unwrap();
            self.env
                .bindings
                .revalidate(self.env.state.latest().unwrap());
        }
        fn step(&mut self) -> SkillStep {
            self.run.advance(&mut SkillView {
                state: &self.env.state,
                bindings: &mut self.env.bindings,
                pack: &self.env.pack,
                now_ms: self.env.now_ms,
                max_age_ms: 2000,
            })
        }
        fn open(&mut self) {
            assert_eq!(
                action(self.step()).primitive,
                Primitive::AxAction {
                    action: AxActionKind::ShowMenu
                }
            );
            self.run.acted(ActionOutcome::Completed { end_ms: 110 });
            self.admit(self.snapshot(true, "before", 2, 200));
        }
    }
    fn action(step: SkillStep) -> ActionPlan {
        match step {
            SkillStep::Act(plan) => *plan,
            other => panic!("{other:?}"),
        }
    }
    fn failed(step: SkillStep) -> FailureCode {
        match step {
            SkillStep::Done(SkillResult::Failed { code, .. }) => code,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn selects_with_two_distinct_bindings_and_verifies_closed_menu_and_actual_value() {
        let mut rig = Rig::new("before");
        let opener = rig.run.binding.clone();
        rig.open();
        let choice = action(rig.step());
        assert_eq!(
            choice.primitive,
            Primitive::AxAction {
                action: AxActionKind::Press
            }
        );
        assert_eq!(choice.path, ExecutionPath::Ax);
        assert_ne!(choice.binding.binding_ref, opener);
        assert_ne!(choice.binding.semantic_target, rig.run.target);
        assert_eq!(rig.run.binding, opener);
        assert!(rig.run.menu_anchor().is_some());
        assert!(rig.run.menu_context().is_some());
        assert_eq!(rig.run.menu_opening_end_ms(), Some(110));
        assert!(rig.run.menu_same_watch_required());
        rig.run.acted(ActionOutcome::Completed { end_ms: 210 });
        assert!(rig.run.menu_readonly_result_ready());
        assert!(!rig.run.menu_same_watch_required());
        rig.admit(rig.snapshot(false, "Choice B", 3, 300));
        assert_eq!(rig.step(), SkillStep::Done(SkillResult::Succeeded));
        assert_eq!(rig.run.actions(), 2);
        assert!(!rig.run.menu_same_watch_required());
        let binding = rig.env.bindings.current(&rig.run.target).unwrap();
        assert!(
            rig.run
                .menu_readonly_result_scope_matches(rig.env.state.latest().unwrap(), binding)
        );
    }
    #[test]
    fn already_done_and_mismatching_frozen_parameters_never_input() {
        let mut rig = Rig::new("Choice B");
        assert_eq!(rig.step(), SkillStep::Done(SkillResult::AlreadyDone));
        assert_eq!(rig.run.actions(), 0);
        let mut rig = Rig::new("before");
        rig.run.desired = Some(ExpectedValue::Text {
            value: "Choice A".into(),
        });
        assert_eq!(failed(rig.step()), FailureCode::ContextUnresolved);
        assert_eq!(rig.run.actions(), 0);
        assert_eq!(
            Skill::SelectByLabel.quota(),
            AttemptQuota {
                actions: 2,
                observations: 3
            }
        );
    }
    #[test]
    fn source_sequence_and_actual_input_end_both_must_advance_before_choice() {
        let mut rig = Rig::new("before");
        action(rig.step());
        rig.run.acted(ActionOutcome::Completed { end_ms: 110 });
        assert!(matches!(rig.step(), SkillStep::AwaitEvidence { .. }));
        rig.admit(rig.snapshot(true, "before", 2, 110));
        assert!(matches!(rig.step(), SkillStep::AwaitEvidence { .. }));
        assert_eq!(rig.run.actions(), 1);
        rig.admit(rig.snapshot(true, "before", 3, 111));
        action(rig.step());
        assert_eq!(rig.run.actions(), 2);
    }
    #[test]
    fn unknown_show_menu_stops_even_when_the_menu_becomes_visible() {
        let mut rig = Rig::new("before");
        action(rig.step());
        rig.run.acted(ActionOutcome::Unknown { at_ms: 110 });
        rig.admit(rig.snapshot(true, "before", 2, 200));
        assert_eq!(failed(rig.step()), FailureCode::EffectUnknown);
        assert_eq!(rig.run.actions(), 1);
    }
    #[test]
    fn unknown_choice_only_verifies_never_replays() {
        for value in ["Choice B", "before"] {
            let mut rig = Rig::new("before");
            rig.open();
            action(rig.step());
            rig.run.acted(ActionOutcome::Unknown { at_ms: 210 });
            rig.admit(rig.snapshot(false, value, 3, 300));
            assert!(!rig.run.menu_readonly_result_ready());
            assert!(rig.run.menu_same_watch_required());
            assert_eq!(failed(rig.step()), FailureCode::EffectUnknown);
            assert_eq!(rig.run.actions(), 2);
            assert!(matches!(rig.step(), SkillStep::Done(_)));
        }
    }
    #[test]
    fn duplicate_disabled_missing_press_and_substring_choices_cannot_dispatch() {
        for mutation in [
            "duplicate",
            "disabled-duplicate",
            "disabled",
            "hidden",
            "missing-press",
            "substring",
        ] {
            let mut rig = Rig::new("before");
            rig.open();
            let mut snapshot = rig.snapshot(true, "before", 3, 201);
            match mutation {
                "duplicate" => snapshot.objects[3].title = Some("Choice B".into()),
                "disabled-duplicate" => {
                    snapshot.objects[3].title = Some("Choice B".into());
                    snapshot.objects[3].enabled = false;
                }
                "disabled" => snapshot.objects[4].enabled = false,
                "hidden" => snapshot.objects[4].in_view = false,
                "missing-press" => snapshot.objects[4].actions.clear(),
                "substring" => snapshot.objects[4].title = Some("Choice B extra".into()),
                _ => unreachable!(),
            }
            rig.admit(snapshot);
            assert!(
                matches!(rig.step(), SkillStep::Done(SkillResult::Failed { .. })),
                "{mutation}"
            );
            assert_eq!(rig.run.actions(), 1, "{mutation}");
        }
    }
    #[test]
    fn closed_without_effect_replaced_anchor_and_source_restart_stop() {
        for mutation in ["closed", "anchor", "session", "revision", "frame"] {
            let mut rig = Rig::new("before");
            rig.open();
            let mut snapshot = rig.snapshot(mutation != "closed", "before", 3, 201);
            match mutation {
                "anchor" => snapshot.menu_anchors[0].anchor_id = "replacement".into(),
                "session" => snapshot.menu_anchors[0].source_session_id = "watch-two".into(),
                "revision" => snapshot.menu_anchors[0].anchor_revision += 1,
                "frame" => {
                    snapshot.objects[1].frame.x += 1.0;
                    snapshot.menu_anchors[0] = Rig::anchor(&snapshot, "watch-one", "anchor-one");
                }
                _ => {}
            }
            if let Some(context) = &mut snapshot.menu_context {
                context.opener = snapshot.menu_anchors[0].clone();
                context.source_session_id = context.opener.source_session_id.clone();
            }
            rig.admit(snapshot);
            assert!(
                matches!(rig.step(), SkillStep::Done(SkillResult::Failed { .. })),
                "{mutation}"
            );
            assert_eq!(rig.run.actions(), 1);
        }
    }
    #[test]
    fn result_cannot_succeed_while_menu_is_open_or_replaced() {
        for replaced in [false, true] {
            let mut rig = Rig::new("before");
            rig.open();
            action(rig.step());
            rig.run.acted(ActionOutcome::Completed { end_ms: 210 });
            let mut snapshot = rig.snapshot(true, "Choice B", 3, 300);
            if replaced {
                snapshot.menu_context.as_mut().unwrap().context_id = "context-two".into();
            }
            rig.admit(snapshot);
            assert!(matches!(
                rig.step(),
                SkillStep::Done(SkillResult::Failed { .. })
            ));
            assert_eq!(rig.run.actions(), 2);
        }
    }
    #[test]
    fn source_anchor_can_survive_an_ordinary_opener_instance_generation_change() {
        let mut rig = Rig::new("before");
        rig.open();
        let mut snapshot = rig.snapshot(true, "before", 3, 201);
        snapshot.objects[1].instance_ref = ObjectRef::new("opener-new-generation").unwrap();
        snapshot.objects[2].parent = Some(snapshot.objects[1].instance_ref.clone());
        snapshot.menu_anchors[0] = Rig::anchor(&snapshot, "watch-one", "anchor-one");
        snapshot.menu_context.as_mut().unwrap().opener = snapshot.menu_anchors[0].clone();
        rig.admit(snapshot);
        action(rig.step());
        assert_eq!(rig.run.actions(), 2);
    }

    #[test]
    fn known_choice_can_read_a_new_watch_anchor_and_relocated_opener_without_new_input() {
        let mut rig = Rig::new("before");
        rig.open();
        action(rig.step());
        rig.run.acted(ActionOutcome::Completed { end_ms: 210 });
        assert!(rig.run.menu_readonly_result_ready());
        assert!(!rig.run.menu_same_watch_required());
        let mut snapshot = rig.snapshot(false, "Choice B", 3, 300);
        snapshot.objects[1].instance_ref = ObjectRef::new("fresh-watch-opener").unwrap();
        snapshot.objects[1].frame.x += 10.0;
        snapshot.menu_anchors[0] = Rig::anchor(&snapshot, "watch-two", "new-readonly-anchor");
        snapshot.menu_anchors[0].anchor_revision = 7;
        rig.admit(snapshot);
        assert_eq!(rig.step(), SkillStep::Done(SkillResult::Succeeded));
        assert_eq!(rig.run.actions(), 2);
        assert!(rig.run.menu_readonly_result_scope_matches(
            rig.env.state.latest().unwrap(),
            rig.env.bindings.current(&rig.run.target).unwrap()
        ));
        assert_eq!(rig.step(), SkillStep::Done(SkillResult::Succeeded));
        assert_eq!(rig.run.actions(), 2);
    }

    #[test]
    fn readonly_result_requires_same_kernel_scope_and_unchanged_semantic_description() {
        for mutation in [
            "stamp",
            "pid",
            "window-frame",
            "window-id",
            "role",
            "group",
            "label",
            "ancestor",
            "sibling",
            "missing-anchor",
        ] {
            let mut rig = Rig::new("before");
            rig.open();
            action(rig.step());
            rig.run.acted(ActionOutcome::Completed { end_ms: 210 });
            let mut snapshot = rig.snapshot(false, "Choice B", 3, 300);
            match mutation {
                "stamp" => {}
                "pid" => {
                    snapshot.app.pid += 1;
                    snapshot.global_focus.app = ObservationFact::Known {
                        value: snapshot.app.clone(),
                    };
                }
                "window-frame" => {
                    snapshot.windows[0].frame.x -= 1.0;
                    snapshot.windows[0].frame.width += 1.0;
                    snapshot.objects[0].frame = snapshot.windows[0].frame;
                }
                "window-id" => {
                    snapshot.windows[0].window_id = "different-window".into();
                    crate::testkit::focus_snapshot_window(&mut snapshot, "different-window");
                    for object in &mut snapshot.objects {
                        object.window_id = "different-window".into();
                    }
                }
                "role" => snapshot.objects[1].role = "AXPopUpButton".into(),
                "group" => snapshot.objects[1].group = Some("Other group".into()),
                "label" => snapshot.objects[1].title = Some("Other locale".into()),
                "ancestor" => {
                    let mut group = snapshot.objects[0].clone();
                    group.role = "AXGroup".into();
                    group.subrole = None;
                    group.instance_ref = ObjectRef::new("new-ancestor").unwrap();
                    group.parent = Some(snapshot.objects[0].instance_ref.clone());
                    group.identity_continuity =
                        interaction_contracts::desktop::source::ObjectIdentityContinuity::ReadScoped;
                    snapshot.objects[1].parent = Some(group.instance_ref.clone());
                    snapshot.objects.push(group);
                }
                "sibling" => {
                    let mut sibling = snapshot.objects[1].clone();
                    sibling.instance_ref = ObjectRef::new("earlier-sibling").unwrap();
                    sibling.title = Some("Other control".into());
                    sibling.identity_continuity =
                        interaction_contracts::desktop::source::ObjectIdentityContinuity::ReadScoped;
                    snapshot.objects.insert(1, sibling);
                }
                _ => {}
            }
            let opener = snapshot
                .objects
                .iter()
                .find(|object| {
                    object.title.as_deref()
                        == Some(if mutation == "label" {
                            "Other locale"
                        } else {
                            "Locale"
                        })
                })
                .unwrap()
                .clone();
            let mut anchor = Rig::anchor(&snapshot, "watch-two", "new-result-anchor");
            anchor.opener_instance = opener.instance_ref.clone();
            anchor.opener_fingerprint = instance_fingerprint(&snapshot, &opener);
            anchor.opener_frame = opener.frame;
            if mutation == "stamp" {
                anchor.process.start_seconds += 1;
            }
            sim_world::app::refresh_simulated_observer_coverage(&mut snapshot);
            snapshot.menu_anchors = if mutation == "missing-anchor" {
                vec![]
            } else {
                vec![anchor]
            };
            rig.admit(snapshot);
            assert!(
                matches!(rig.step(), SkillStep::Done(SkillResult::Failed { .. })),
                "{mutation}"
            );
            assert_eq!(rig.run.actions(), 2, "{mutation}");
        }
    }

    #[test]
    fn readonly_result_rejects_stale_sequence_and_input_floor_then_accepts_a_complete_new_read() {
        let mut rig = Rig::new("before");
        rig.open();
        action(rig.step());
        rig.run.acted(ActionOutcome::Completed { end_ms: 210 });
        assert!(matches!(rig.step(), SkillStep::AwaitEvidence { .. }));
        rig.admit(rig.snapshot(false, "Choice B", 3, 210));
        assert!(matches!(rig.step(), SkillStep::AwaitEvidence { .. }));
        assert_eq!(rig.run.actions(), 2);
        rig.admit(rig.snapshot(false, "Choice B", 4, 211));
        assert_eq!(rig.step(), SkillStep::Done(SkillResult::Succeeded));
    }
}
