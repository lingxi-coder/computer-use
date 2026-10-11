//! Test-only actual Core submissions drive subsequent Source facts. This
//! verifies Runtime integration; it does not claim native keyboard acceptance.

mod common;

use execution_core::backend::{
    HeldState, HitTestOutcome, HitTestRequest, InputBackend, ReleaseFailure, SubmitCompletion,
    SubmitContext, SubmitOutcome,
};
use execution_core::core::{CoreConfig, CoreEvent, ExecutionCore};
use interaction_contracts::desktop::{
    binding::instance_fingerprint,
    device::*,
    domain::ExecutionDomain,
    effect::EffectClass,
    ids::{ObjectRef, SemanticTargetRef},
    operator::{OperatorCommand, RunMode, UiProjection},
    planning::*,
    primitive::{AxActionKind, Modifiers, MouseButton, Primitive, PrimitivePolicy},
    source::*,
    task::{ConfirmationScope, ExpectedValue, RootState, TargetSpec},
};
use interaction_engine::{
    engine::{Engine, EngineConfig, Input, Output},
    environment::pack::AppPack,
    execution::{DeviceCommand, DeviceNotice, DeviceReply},
    infrastructure::storage::Storage,
    tasks::TaskConfig,
};
use sim_world::{
    app::{SimApp, SnapshotMeta},
    policy::ScriptedTeacher,
};
use std::{cell::RefCell, collections::VecDeque, rc::Rc};
const BUNDLE: &str = "com.lingxi.computer-user.guard-fixture";

/// Pair each synthetic foreground Source tree with pixels from that exact
/// focused window. Native foreground manifests require screenshot evidence;
/// these workflow fixtures therefore model the normal AX + frame bundle.
fn frame_for_snapshot(snapshot: &StructureSnapshot) -> Option<FrameMeta> {
    let window_id = snapshot
        .global_focus
        .known_window_for_app(&snapshot.app)?
        .to_owned();
    let window_frame = snapshot
        .windows
        .iter()
        .find(|window| window.window_id == window_id)?
        .frame;
    let display_time_ns = snapshot
        .observed_ms
        .checked_mul(1_000_000)?
        .checked_add(snapshot.source_sequence)?;
    let width_px = window_frame.width.round();
    let height_px = window_frame.height.round();
    if display_time_ns == 0
        || !width_px.is_finite()
        || !height_px.is_finite()
        || width_px < 1.0
        || height_px < 1.0
        || width_px > 8192.0
        || height_px > 8192.0
    {
        return None;
    }
    Some(FrameMeta {
        frame_id: format!("key-image-{}", snapshot.source_sequence),
        session_epoch: snapshot.session_epoch,
        surface: snapshot.surface.clone(),
        clock_domain: snapshot.clock_domain.clone(),
        source_sequence: snapshot.source_sequence,
        source_ms: snapshot.observed_ms,
        status: FrameStatus::Complete,
        geometry_revision: 1,
        width_px: width_px as u32,
        height_px: height_px as u32,
        scale_factor: 1.0,
        byte_length: 100,
        checksum: format!("sha256:key-image-{}", snapshot.source_sequence),
        provenance: Some(FrameProvenance::Stream {
            stream: FrameStreamInfo {
                stream_id: "key-workflow-screen".into(),
                image_frame_id: Some(format!("key-image-{}", snapshot.source_sequence)),
                display_time_ticks: display_time_ns,
                display_time_ns,
                received_uptime_ns: display_time_ns,
                received_unix_ms: 100_000_u64.saturating_add(snapshot.observed_ms),
                callback_ms: snapshot.observed_ms,
                source_clock: "mach_absolute".into(),
                window_id,
                pid: snapshot.app.pid,
                window_frame,
                content_rect: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: window_frame.width,
                    height: window_frame.height,
                },
                content_scale: 1.0,
                dirty_rects: vec![],
                dropped_frames: 0,
                capture_interval_ms: 100,
            },
        }),
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Fault {
    #[default]
    None,
    Unknown,
    NoEffect,
    NoFocus,
    SecureAfter,
    SecureSubroleAfter,
    MovedAfter,
    WrongFocus,
    FocusChangedAtGrant,
    Partial,
    MissingWindowProof,
    RestartedProcess,
    TextFingerprintChangedAfter,
    MissingTextInputEnd,
    AlreadyWantedBefore,
    DelayedTextSourceProof,
    HeldTextSourceAfterFirstInvalid,
}

fn click_at(x: i32, y: i32) -> Primitive {
    Primitive::Click {
        button: MouseButton::Left,
        x,
        y,
        modifiers: Modifiers::default(),
    }
}

#[derive(Clone, Debug)]
struct Applied {
    at_ms: u64,
    intent: ActionIntent,
    primitive: Primitive,
    purpose: ObservationPurpose,
}
struct Application {
    fault: Fault,
    value: bool,
    text_value: String,
    text_focused: bool,
    text_mode: bool,
    other: bool,
    focused: bool,
    inputs: Vec<Applied>,
    snapshot: Option<StructureSnapshot>,
}
struct Backend {
    app: Rc<RefCell<Application>>,
    sequence: u64,
    completions: VecDeque<SubmitCompletion>,
}
impl InputBackend for Backend {
    fn submit(
        &mut self,
        at_ms: u64,
        primitive: &Primitive,
        context: &SubmitContext<'_>,
    ) -> SubmitOutcome {
        let mut app = self.app.borrow_mut();
        let proof = context.intent.binding_proof.as_ref().unwrap();
        let object = app
            .snapshot
            .as_ref()
            .unwrap()
            .objects
            .iter()
            .find(|o| o.instance_ref == proof.instance)
            .unwrap()
            .clone();
        let unknown = match primitive {
            Primitive::KeyPress { key, modifiers } => {
                assert_eq!(key, "Space");
                assert_eq!(*modifiers, Modifiers::default());
                assert_eq!(
                    context.authority.evidence.purpose,
                    ObservationPurpose::FocusedControl
                );
                let scope = proof
                    .focus_scope
                    .as_ref()
                    .expect("Core admitted actual focused scope");
                assert_eq!(
                    scope.context,
                    *app.snapshot
                        .as_ref()
                        .unwrap()
                        .focus_context
                        .as_ref()
                        .unwrap()
                );
                assert_eq!(scope.context.target_instance, object.instance_ref);
                assert!(proof.menu_scope.is_none());
                assert!(app.focused);
                assert!(!app.other);
                if app.fault != Fault::NoEffect {
                    app.value = !app.value;
                }
                app.fault == Fault::Unknown
            }
            Primitive::Click {
                button: MouseButton::Left,
                x,
                y,
                modifiers,
            } if object.role == "AXRadioButton" => {
                assert_eq!(*modifiers, Modifiers::default());
                let (center_x, center_y) = object.frame.center();
                assert_eq!((*x, *y), (center_x.round() as i32, center_y.round() as i32));
                app.other = object.title.as_deref() == Some("Other");
                app.focused = false;
                false
            }
            Primitive::AxAction {
                action: AxActionKind::Press,
            } if object.role == "AXRadioButton" => {
                app.other = object.title.as_deref() == Some("Other");
                app.focused = false;
                false
            }
            Primitive::AxAction {
                action: AxActionKind::SetValue { value },
            } if object.role == "AXTextField" => {
                assert_eq!(
                    context.authority.evidence.purpose,
                    ObservationPurpose::TargetInput
                );
                assert!(app.text_mode && !app.other);
                if app.fault != Fault::NoEffect {
                    app.text_value = value.clone();
                }
                app.text_focused = false;
                app.fault == Fault::Unknown
            }
            _ => panic!("keyboard profile must not use fallback input: {primitive:?}"),
        };
        app.inputs.push(Applied {
            at_ms,
            intent: context.intent.clone(),
            primitive: primitive.clone(),
            purpose: context.authority.evidence.purpose,
        });
        self.sequence += 1;
        self.completions.push_back(SubmitCompletion {
            sequence: self.sequence,
            session_epoch: context.intent.session_epoch,
            intent_id: context.intent.intent_id.clone(),
            outcome: if unknown {
                SubmitOutcome::Unknown
            } else {
                SubmitOutcome::Applied
            },
            submitted_at_ms: Some(at_ms),
            input_end_ms: (app.fault != Fault::MissingTextInputEnd).then_some(at_ms + 1),
            failure_code: None,
        });
        SubmitOutcome::Pending {
            sequence: self.sequence,
            reply_deadline_ms: context.authority.valid_until_ms + 500,
        }
    }
    fn poll_completion(&mut self) -> Option<SubmitCompletion> {
        self.completions.pop_front()
    }
    fn release(&mut self, _: u64, _: &Primitive) -> Result<(), ReleaseFailure> {
        Ok(())
    }
    fn held(&self, _: &str) -> HeldState {
        HeldState::NotHeld
    }
    fn hit_test(&mut self, request: &HitTestRequest<'_>) -> HitTestOutcome {
        let app = self.app.borrow();
        let snapshot = app.snapshot.as_ref().unwrap();
        let proof = request.context.intent.binding_proof.as_ref().unwrap();
        snapshot
            .objects
            .iter()
            .find(|o| o.instance_ref == proof.instance)
            .map_or(HitTestOutcome::Unresolved, |o| {
                HitTestOutcome::Element(instance_fingerprint(snapshot, o).digest())
            })
    }
}
fn plan() -> PlanProposal {
    let semantic = SemanticTargetRef::new("keyboard-toggle").unwrap();
    PlanProposal {
        goal_draft: Some(GoalDraft {
            app: BUNDLE.into(),
            account: None,
            target: TargetSpec {
                row: None,
                semantic_target: semantic.clone(),
                label: "Key fixture toggle".into(),
                group: None,
                role_hint: Some("AXCheckBox".into()),
            },
            desired: ExpectedValue::Bool { value: true },
            confirmation_scope: ConfirmationScope::UiRevisit,
            max_effect_class: EffectClass::ReversibleEdit,
        }),
        bundle: PlanBundle {
            subgoals: vec![
                Subgoal {
                    id: "key".into(),
                    description: "Set focused toggle".into(),
                    skill: "ui.ensure_value".into(),
                    target: Some(semantic.clone()),
                    expects: vec!["actual value true".into()],
                    scroll: None,
                    select: None,
                },
                Subgoal {
                    id: "revisit".into(),
                    description: "Revisit Document".into(),
                    skill: "ui.revisit".into(),
                    target: Some(semantic),
                    expects: vec!["value persists after return".into()],
                    scroll: None,
                    select: None,
                },
            ],
            dependencies: vec![Dependency {
                before: "key".into(),
                after: "revisit".into(),
            }],
            required_observations: vec![],
            expected_effects: vec![],
            recovery_branches: vec![],
            termination: vec!["frozen root predicates".into()],
        },
    }
}

fn text_plan() -> PlanProposal {
    let semantic = SemanticTargetRef::new("document").unwrap();
    PlanProposal {
        goal_draft: Some(GoalDraft {
            app: BUNDLE.into(),
            account: None,
            target: TargetSpec {
                row: None,
                semantic_target: semantic.clone(),
                label: "MVP document".into(),
                group: None,
                role_hint: Some("AXTextField".into()),
            },
            desired: ExpectedValue::Text {
                value: "wanted".into(),
            },
            confirmation_scope: ConfirmationScope::UiRevisit,
            max_effect_class: EffectClass::ReversibleEdit,
        }),
        bundle: PlanBundle {
            subgoals: vec![
                Subgoal {
                    id: "text".into(),
                    description: "Enter text into the document".into(),
                    skill: "ui.enter_text".into(),
                    target: Some(semantic.clone()),
                    expects: vec!["the text value is visible".into()],
                    scroll: None,
                    select: None,
                },
                Subgoal {
                    id: "revisit".into(),
                    description: "Revisit the document".into(),
                    skill: "ui.revisit".into(),
                    target: Some(semantic),
                    expects: vec!["the text remains visible after return".into()],
                    scroll: None,
                    select: None,
                },
            ],
            dependencies: vec![Dependency {
                before: "text".into(),
                after: "revisit".into(),
            }],
            required_observations: vec![],
            expected_effects: vec![],
            recovery_branches: vec![],
            termination: vec!["frozen root predicates".into()],
        },
    }
}
struct Harness {
    engine: Engine,
    core: ExecutionCore<Backend>,
    app: Rc<RefCell<Application>>,
    queue: VecDeque<Input>,
    now: u64,
    sequence: u64,
    ui: Option<UiProjection>,
    observations: Vec<Observation>,
    text_sources_after_input: u32,
    text_identity_boundary_changes: bool,
}
impl Harness {
    fn new(fault: Fault, value: bool) -> Self {
        Self::build(fault, value, false)
    }
    fn new_text(fault: Fault) -> Self {
        Self::build(fault, false, true)
    }
    fn build(fault: Fault, value: bool, text_mode: bool) -> Self {
        let app = Rc::new(RefCell::new(Application {
            fault,
            value,
            text_value: if fault == Fault::AlreadyWantedBefore {
                "wanted".into()
            } else {
                "before".into()
            },
            text_focused: true,
            text_mode,
            other: false,
            focused: true,
            inputs: vec![],
            snapshot: None,
        }));
        let mut cfg = EngineConfig::new(TaskConfig::new(vec![
            AppPack::registered_native(BUNDLE).unwrap(),
        ]));
        cfg.initial_mode = RunMode::Execute;
        let mut core = ExecutionCore::new(
            CoreConfig::production("key-workflow"),
            Backend {
                app: app.clone(),
                sequence: 0,
                completions: VecDeque::new(),
            },
        );
        core.renew_lease(60_000);
        let mut h = Self {
            engine: Engine::new(cfg, Storage::open_in_memory().unwrap()),
            core,
            app,
            queue: VecDeque::new(),
            now: 0,
            sequence: 0,
            ui: None,
            observations: vec![],
            text_sources_after_input: 0,
            text_identity_boundary_changes: false,
        };
        h.send(Input::DeviceNotice(DeviceNotice::Manifest(Box::new(
            CapabilityManifest {
                identity: h.core.identity(),
                backend: BackendKind::Macos,
                execution_domain: ExecutionDomain::ForegroundNative,
                policy: PrimitivePolicy::AtomicOnly,
                human_input: CapabilityState::Available,
                hit_test: CapabilityState::Available,
                input: CapabilityState::Available,
                domains: common::domain_capabilities_for_test_profile(
                    ExecutionDomain::ForegroundNative,
                ),
                max_uninterruptible_ms: 100,
            },
        ))));
        h
    }
    fn source(&mut self) {
        self.sequence += 1;
        let (text_mode, has_input, fault) = {
            let app = self.app.borrow();
            (app.text_mode, !app.inputs.is_empty(), app.fault)
        };
        let missing_text_window_once = text_mode
            && has_input
            && self.text_sources_after_input == 0
            && matches!(
                fault,
                Fault::DelayedTextSourceProof | Fault::HeldTextSourceAfterFirstInvalid
            );
        if text_mode && has_input {
            self.text_sources_after_input += 1;
        }
        let registry_sequence = if self.app.borrow().fault == Fault::MissingWindowProof {
            1
        } else {
            self.sequence
        };
        let identity = self.core.identity();
        let mut s = SimApp::new().snapshot(&SnapshotMeta {
            snapshot_id: format!("key-read-{}", self.sequence),
            session_epoch: identity.session_epoch,
            surface: identity.surface,
            clock_domain: identity.clock_domain,
            source_sequence: self.sequence,
            observed_ms: self.now,
        });
        if self.text_identity_boundary_changes {
            s.identity_boundary = Some(
                interaction_contracts::desktop::source::SourceIdentityBoundary {
                    namespace: "11111111-1111-4111-8111-111111111111".into(),
                    generation: if has_input { 2 } else { 1 },
                },
            );
        }
        s.app.bundle_id = BUNDLE.into();
        s.global_focus.app = ObservationFact::Known {
            value: s.app.clone(),
        };
        s.global_focus.window_id = ObservationFact::Known {
            value: s.windows[0].window_id.clone(),
        };
        s.windows[0].owner_bundle_id = BUNDLE.into();
        let app = self.app.borrow();
        s.windows[0].title = Some(format!(
            "Key Fixture — {}",
            if app.other { "Other" } else { "Document" }
        ));
        let w = s.windows[0].clone();
        let rootid = ObjectRef::new(format!("root-{}", registry_sequence)).unwrap();
        let object =
            |id: String, role: &str, title: &str, parent: Option<ObjectRef>, frame: Rect| {
                UiObject {
                    instance_ref: ObjectRef::new(id).unwrap(),
                    parent,
                    window_id: w.window_id.clone(),
                    role: role.into(),
                    subrole: None,
                    title: Some(title.into()),
                    description: None,
                    value: None,
                    enabled: true,
                    focused: false,
                    frame,
                    actions: vec![AxActionName::Press],
                    ax_action_names: None,
                    in_view: true,
                    group: None,
                    secure: false,
                    scroll: None,
                    identity_continuity: ObjectIdentityContinuity::ReadScoped,
                }
            };
        let mut root = object(rootid.as_str().into(), "AXWindow", "", None, w.frame);
        root.subrole = Some("AXStandardWindow".into());
        root.actions.clear();
        s.objects = vec![root];
        s.menu_anchors.clear();
        s.menu_context = None;
        s.focus_context = None;
        let process = KernelProcessIdentity {
            pid: s.app.pid,
            start_seconds: if !app.inputs.is_empty() && app.fault == Fault::RestartedProcess {
                2
            } else {
                1
            },
            start_micros: 0,
        };
        s.window_context = if app.fault == Fault::Partial
            || (!app.inputs.is_empty() && app.fault == Fault::MissingWindowProof)
            || missing_text_window_once
        {
            None
        } else {
            Some(SourceWindowContext {
                source_session_id: "key-source-watch".into(),
                session_epoch: s.session_epoch,
                process: process.clone(),
                origin_window_id: w.window_id.clone(),
                origin_window_frame: w.frame,
                verified_source_sequence: self.sequence,
                verified_observed_ms: self.now,
            })
        };
        for (label, y) in [("Document", 10.0), ("Other", 40.0)] {
            s.objects.push(object(
                format!("nav-{label}-{}", registry_sequence),
                "AXRadioButton",
                label,
                Some(rootid.clone()),
                Rect {
                    x: 400.0,
                    y,
                    width: 100.0,
                    height: 25.0,
                },
            ));
        }
        if !app.other && app.text_mode {
            let frame = Rect {
                x: 30.0,
                y: 80.0,
                width: 300.0,
                height: 30.0,
            };
            let mut text = object(
                "text-document".into(),
                "AXTextField",
                "MVP document",
                Some(rootid.clone()),
                frame,
            );
            text.value = Some(UiValue::Text(app.text_value.clone()));
            text.focused = app.text_focused;
            text.actions = vec![AxActionName::SetValue];
            if !app.inputs.is_empty() && app.fault == Fault::TextFingerprintChangedAfter {
                text.frame.x += 5.0;
            }
            s.objects.push(text);
        } else if !app.other {
            let frame = Rect {
                x: 30.0,
                y: 80.0,
                width: 150.0,
                height: 30.0,
            };
            // Real AppKit distinguishes a checkbox wrapper and its focused cell. The
            // title/frame alone cannot select the actual input receiver.
            let mut wrapper = object(
                format!("wrapper-{}", registry_sequence),
                "AXCheckBox",
                "Key fixture toggle",
                Some(rootid),
                frame,
            );
            wrapper.value = Some(UiValue::Bool(app.value));
            if !app.inputs.is_empty() && app.fault == Fault::SecureAfter {
                wrapper.secure = true;
            }
            if !app.inputs.is_empty() && app.fault == Fault::SecureSubroleAfter {
                wrapper.subrole = Some("AXSecureTextField".into());
            }
            let mut cell = object(
                format!("cell-{}", registry_sequence),
                "AXCheckBox",
                "Key fixture toggle",
                Some(wrapper.instance_ref.clone()),
                frame,
            );
            cell.value = Some(UiValue::Bool(app.value));
            cell.focused = app.focused;
            if !app.inputs.is_empty() && app.fault == Fault::MovedAfter {
                cell.frame.x += 5.0;
            }
            s.objects.push(wrapper);
            s.objects.push(cell.clone());
            if cell.focused
                && app.fault != Fault::NoFocus
                && app.fault != Fault::Partial
                && (app.inputs.is_empty()
                    || !matches!(app.fault, Fault::SecureAfter | Fault::SecureSubroleAfter))
            {
                if app.fault == Fault::WrongFocus {
                    s.objects[1].focused = true;
                }
                let target = if app.fault == Fault::WrongFocus {
                    &s.objects[1]
                } else {
                    &cell
                };
                s.focus_context = Some(FocusContext {
                    source_session_id: "key-source-watch".into(),
                    session_epoch: s.session_epoch,
                    process: process.clone(),
                    origin_window_id: w.window_id,
                    origin_window_frame: w.frame,
                    target_instance: target.instance_ref.clone(),
                    target_fingerprint: instance_fingerprint(&s, target),
                    target_frame: target.frame,
                    focused_instance: target.instance_ref.clone(),
                    relation: FocusRelation::ExactControl,
                    composition: CompositionState::NotApplicable,
                    keyboard: SourceKeyboardContext {
                        selected_input_source: None,
                        underlying_keyboard_layout: None,
                        keyboard_type: None,
                        caps_lock: None,
                    },
                    verified_source_sequence: self.sequence,
                    verified_observed_ms: self.now,
                });
            }
        }
        if app.fault == Fault::Partial {
            s.complete = false;
            s.incomplete_reason = Some("test actual partial tree".into());
        }
        drop(app);
        sim_world::app::refresh_simulated_observer_coverage(&mut s);
        s.validate().expect("typed test Source must be valid");
        let frame = frame_for_snapshot(&s);
        self.app.borrow_mut().snapshot = Some(s.clone());
        self.queue.push_back(Input::Structure(Box::new(s)));
        if let Some(frame) = frame {
            self.queue.push_back(Input::Frame(Box::new(frame)));
        }
    }
    fn send(&mut self, input: Input) {
        self.queue.push_back(input);
        let mut deliveries = 0;
        while let Some(input) = self.queue.pop_front() {
            deliveries += 1;
            assert!(deliveries < 10_000, "keyboard engine did not settle");
            for output in self.engine.handle(self.now, input) {
                match output {
                    Output::Ui(ui) => self.ui = Some(*ui),
                    Output::Source { generation, .. } => {
                        let hold_text_source = {
                            let app = self.app.borrow();
                            app.fault == Fault::HeldTextSourceAfterFirstInvalid
                                && !app.inputs.is_empty()
                                && self.text_sources_after_input > 0
                        };
                        // The native Source ACK follows retirement of the old
                        // watcher; only then can the replacement watcher
                        // publish a new snapshot. Keep this ordering explicit
                        // so Engine's post-ACK sequence floor is exercised.
                        self.queue.push_back(Input::SourceRefreshStarted {
                            generation,
                            accepted: true,
                        });
                        if !hold_text_source {
                            self.source();
                        }
                    }
                    Output::BrowserRefresh(_) => {}
                    Output::Plan(request) if request.reason == PlanningReason::GoalCompile => {
                        self.queue.push_back(Input::Planner {
                            request_id: request.request_id,
                            generation: request.generation,
                            output: Box::new(PlannerOutput::Plan {
                                proposal: Box::new(if self.app.borrow().text_mode {
                                    text_plan()
                                } else {
                                    plan()
                                }),
                            }),
                        })
                    }
                    Output::Plan(_) => {}
                    Output::Decision(request) => self.queue.push_back(Input::Decision(Box::new(
                        ScriptedTeacher::default().decide(&request),
                    ))),
                    Output::Device { corr, command } => self.device(corr, *command),
                    Output::CancelDecision(_) | Output::CancelPlan(_) => {}
                    Output::Computer(_) => {
                        panic!("computer proposals must remain disabled in this fixture")
                    }
                }
            }
        }
    }
    fn device(&mut self, corr: u64, command: DeviceCommand) {
        let ack = |r: Result<(), String>| {
            DeviceReply::Ack(match r {
                Ok(()) => CommandAck::ok(),
                Err(reason) => CommandAck::rejected(None, reason),
            })
        };
        let reply = match command {
            DeviceCommand::RegisterRoot(grant) => ack(self.core.register_root(&grant)),
            DeviceCommand::Observe(observation) => {
                let observation = observation
                    .native()
                    .expect("keyboard workflow uses native evidence")
                    .clone();
                self.observations.push(observation.clone());
                ack(self.core.observe(observation))
            }
            DeviceCommand::GrantPermit(request) => {
                DeviceReply::Grant(match self.core.grant_permit(&request) {
                    Ok(permit) => GrantOutcome::Granted { permit },
                    Err(denial) => GrantOutcome::Denied {
                        code: denial.code,
                        reason: denial.reason,
                    },
                })
            }
            DeviceCommand::Submit(request) => {
                let receipt = self.core.submit(request);
                self.core.dispatch_next();
                DeviceReply::Receipt(Some(Box::new(receipt)))
            }
            DeviceCommand::QueryReceipt(query) => DeviceReply::Receipt(
                self.core
                    .query_receipt(query.session_epoch, &query.intent_id)
                    .map(Box::new),
            ),
            DeviceCommand::Revoke(request) => ack(self.core.revoke(request)),
            DeviceCommand::RenewLease(lease) => {
                self.core.renew_lease(lease.valid_for_ms);
                DeviceReply::Ack(CommandAck::ok())
            }
            DeviceCommand::Stop(stop) => DeviceReply::Stopped(self.core.stop(&stop)),
            DeviceCommand::Resume(resume) => DeviceReply::Resumed(self.core.resume(&resume)),
            DeviceCommand::UpdateDependency(update) => {
                self.core.update_dependency(&update.name, update.valid);
                DeviceReply::Dependencies(DependencyVersions {
                    versions: self.core.dependency_versions(),
                })
            }
        };
        if matches!(reply, DeviceReply::Grant(GrantOutcome::Granted { .. }))
            && self.app.borrow().fault == Fault::FocusChangedAtGrant
            && self.observations.last().is_some_and(|observation| {
                observation.purpose == ObservationPurpose::FocusedControl
            })
        {
            self.app.borrow_mut().focused = false;
            self.source();
        }
        self.queue.push_back(Input::Device { corr, reply });
        self.pump();
    }
    fn pump(&mut self) {
        for event in self.core.drain_events() {
            let notice = match event {
                CoreEvent::ReceiptChanged(r) => DeviceNotice::Receipt(Box::new(r)),
                event => DeviceNotice::Event(DeviceEvent {
                    seq: 0,
                    at_ms: self.now,
                    event: match event {
                        CoreEvent::PermitRevoked { permit, reason } => {
                            DeviceEventKind::PermitRevoked { permit, reason }
                        }
                        CoreEvent::Isolated { reason } => {
                            DeviceEventKind::ChannelIsolated { reason }
                        }
                        CoreEvent::Recovered => DeviceEventKind::ChannelRecovered,
                        CoreEvent::Stopped(ack) => DeviceEventKind::Stopped { ack },
                        CoreEvent::Resumed { scope } => DeviceEventKind::Resumed { scope },
                        CoreEvent::LeaseLost => DeviceEventKind::LeaseLost,
                        CoreEvent::ReceiptChanged(_) => unreachable!(),
                    },
                }),
            };
            self.queue.push_back(Input::DeviceNotice(notice));
        }
    }
    fn step(&mut self) {
        self.now += 50;
        self.core.advance_to(self.now).unwrap();
        self.core.dispatch_next();
        self.pump();
        if self.now.is_multiple_of(100) {
            self.source();
        }
        self.send(Input::Tick);
    }
    fn until(&mut self, done: impl Fn(&Self) -> bool) {
        let deadline = self.now + 5000;
        while !done(self) {
            assert!(self.now < deadline, "{}", self.report());
            self.step();
        }
    }
    fn start(&mut self) {
        self.start_task("Set Key fixture toggle true and verify after revisit");
    }
    fn start_text(&mut self) {
        self.start_task("Enter wanted text in MVP document and verify after revisit");
    }
    fn start_task(&mut self, task: &str) {
        self.send(Input::Operator(OperatorCommand::CreateTask {
            text: task.into(),
        }));
        self.until(|h| h.engine.root_state() == Some(RootState::AwaitingConfirmation));
        let confirmation_id = self
            .ui
            .as_ref()
            .unwrap()
            .pending_confirmation
            .as_ref()
            .unwrap()
            .confirmation_id
            .clone();
        self.send(Input::Operator(OperatorCommand::ConfirmGoal {
            confirmation_id,
            approved: true,
        }));
    }

    fn count(&self, query: &str) -> i64 {
        self.engine
            .storage()
            .read()
            .query_row(query, [], |r| r.get(0))
            .unwrap()
    }
    fn inputs(&self) -> Vec<Applied> {
        self.app.borrow().inputs.clone()
    }
    fn report(&self) -> String {
        format!(
            "state {:?}, ended {:?}, fault {:?}, notes {:?}, inputs {:?}",
            self.engine.root_state(),
            self.engine.last_finished(),
            self.engine.faulted(),
            self.engine.timeline().map(|n| &n.text).collect::<Vec<_>>(),
            self.inputs()
        )
    }
}
#[test]
fn one_space_then_fresh_actual_value_and_two_navigation_inputs_complete_revisit() {
    let mut h = Harness::new(Fault::None, false);
    h.start();
    h.until(|h| h.count("SELECT COUNT(*) FROM effect_results WHERE json_extract(json,'$.predicate') = 'keyboard.value@1'") == 1);
    assert_ne!(
        h.engine.last_finished(),
        Some(RootState::Completed),
        "CurrentUi cannot complete the frozen UiRevisit goal"
    );
    h.until(|h| h.engine.last_finished().is_some());
    assert_eq!(
        h.engine.last_finished(),
        Some(RootState::Completed),
        "{}",
        h.report()
    );
    let inputs = h.inputs();
    assert_eq!(inputs.len(), 3, "{}", h.report());
    assert_eq!(inputs[0].purpose, ObservationPurpose::FocusedControl);
    assert!(matches!(inputs[0].primitive, Primitive::KeyPress { .. }));
    assert_eq!(
        inputs[0]
            .intent
            .binding_proof
            .as_ref()
            .unwrap()
            .instance
            .as_str()
            .split('-')
            .next(),
        Some("cell")
    );
    assert_eq!(
        inputs[1..]
            .iter()
            .map(|input| input.primitive.clone())
            .collect::<Vec<_>>(),
        vec![click_at(450, 53), click_at(450, 23)]
    );
    assert!(inputs[1].at_ms > inputs[0].at_ms);
    assert!(inputs[2].at_ms > inputs[1].at_ms);
    assert_eq!(
        h.count("SELECT COUNT(*) FROM attempts WHERE skill = 'ui.ensure_value'"),
        1
    );
    assert_eq!(
        h.count("SELECT quota_actions FROM attempts WHERE skill = 'ui.ensure_value'"),
        1
    );
    assert_eq!(h.count("SELECT COUNT(*) FROM effect_results WHERE json_extract(json,'$.predicate') = 'goal.required@1' AND json_extract(json,'$.status') = 'satisfied' AND json_extract(json,'$.confirmation_scope') = 'ui_revisit'"),1);
    let current_json: String = h.engine.storage().read().query_row(
        "SELECT json FROM effect_results WHERE json_extract(json,'$.predicate') = 'keyboard.value@1'", [], |r| r.get(0)).unwrap();
    let current: serde_json::Value = serde_json::from_str(&current_json).unwrap();
    assert_eq!(current["confirmation_scope"], "current_ui");
    assert_eq!(current["causality"], "unknown");
    assert_eq!(current["intent_id"], inputs[0].intent.intent_id);
    let receipt = h
        .core
        .query_receipt(1, &inputs[0].intent.intent_id)
        .unwrap();
    assert!(current["observed_ms"].as_u64().unwrap() > receipt.input_end_ms.unwrap());
    assert!(
        h.app
            .borrow()
            .snapshot
            .as_ref()
            .unwrap()
            .focus_context
            .is_none(),
        "return navigation actually cleared focus"
    );
}
#[test]
fn unknown_receipt_cannot_be_confirmed_by_the_actual_matching_value() {
    let mut h = Harness::new(Fault::Unknown, false);
    h.start();
    h.until(|h| {
        h.engine.root_state() == Some(RootState::Suspended) || h.engine.last_finished().is_some()
    });
    for _ in 0..20 {
        h.step();
    }
    assert!(h.app.borrow().value);
    assert_eq!(h.inputs().len(), 1, "{}", h.report());
    assert_ne!(h.engine.last_finished(), Some(RootState::Completed));
    assert_eq!(
        h.count("SELECT COUNT(*) FROM attempts WHERE skill = 'ui.ensure_value'"),
        1
    );
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM effect_results WHERE json_extract(json,'$.status') = 'satisfied'"
        ),
        0
    );
    assert!(
        h.engine
            .timeline()
            .any(|n| n.text.contains("EFFECT_UNKNOWN")),
        "{}",
        h.report()
    );
}

#[test]
fn successful_ax_enter_text_emits_a_fresh_current_ui_effect_separate_from_revisit() {
    let mut h = Harness::new_text(Fault::None);
    h.start_text();
    h.until(|h| {
        h.count("SELECT COUNT(*) FROM effect_results WHERE json_extract(json,'$.predicate') = 'text.value@1'") == 1
    });

    let text_json: String = h.engine.storage().read().query_row(
        "SELECT json FROM effect_results WHERE json_extract(json,'$.predicate') = 'text.value@1'",
        [],
        |row| row.get(0),
    ).unwrap();
    let text: serde_json::Value = serde_json::from_str(&text_json).unwrap();
    assert_eq!(text["status"], "satisfied");
    assert_eq!(text["confirmation_scope"], "current_ui");
    assert_eq!(text["causality"], "unknown");
    assert_eq!(text["target"], "document");
    let input_end = text["input_span"]["to_ms"].as_u64().unwrap();
    assert!(text["observed_ms"].as_u64().unwrap() > input_end);
    assert_eq!(h.app.borrow().text_value, "wanted");
    assert!(
        !h.app.borrow().text_focused,
        "post-input focus is not required to read the current value"
    );
    let inputs = h.inputs();
    assert!(!inputs.is_empty());
    assert!(matches!(
        inputs[0].primitive,
        Primitive::AxAction {
            action: AxActionKind::SetValue { .. }
        }
    ));
    assert_eq!(inputs[0].purpose, ObservationPurpose::TargetInput);
    assert_ne!(h.engine.last_finished(), Some(RootState::Completed));

    h.until(|h| h.engine.last_finished().is_some());
    assert_eq!(
        h.engine.last_finished(),
        Some(RootState::Completed),
        "{}",
        h.report()
    );
    assert_eq!(h.count("SELECT COUNT(*) FROM effect_results WHERE json_extract(json,'$.predicate') = 'goal.required@1' AND json_extract(json,'$.status') = 'satisfied' AND json_extract(json,'$.confirmation_scope') = 'ui_revisit'"), 1);
    let completed_inputs = h.inputs();
    assert_eq!(completed_inputs.len(), 3);
    assert_eq!(completed_inputs[0].primitive, inputs[0].primitive);
    assert_eq!(
        completed_inputs[1..]
            .iter()
            .map(|input| input.primitive.clone())
            .collect::<Vec<_>>(),
        vec![click_at(450, 53), click_at(450, 23)]
    );
}

#[test]
fn a_completed_ax_text_setter_without_a_new_source_value_has_no_text_effect() {
    let mut h = Harness::new_text(Fault::NoEffect);
    h.start_text();
    h.until(|h| h.engine.last_finished().is_some());
    assert_eq!(h.app.borrow().text_value, "before");
    assert_eq!(
        h.count("SELECT COUNT(*) FROM effect_results WHERE json_extract(json,'$.predicate') = 'text.value@1'"),
        0,
        "a completed AX receipt cannot establish the text value"
    );
    assert_ne!(h.engine.last_finished(), Some(RootState::Completed));
}

#[test]
fn ax_text_value_from_a_restarted_window_process_has_no_current_ui_effect() {
    let mut h = Harness::new_text(Fault::RestartedProcess);
    h.start_text();
    h.until(|h| h.engine.last_finished().is_some());
    assert_eq!(h.app.borrow().text_value, "wanted");
    assert_eq!(
        h.count("SELECT COUNT(*) FROM effect_results WHERE json_extract(json,'$.predicate') = 'text.value@1'"),
        0,
        "the same value from a different process generation is not current effect evidence"
    );
    assert_ne!(h.engine.last_finished(), Some(RootState::Completed));
}

#[test]
fn same_text_instance_with_a_changed_fingerprint_has_no_current_ui_effect() {
    let mut h = Harness::new_text(Fault::TextFingerprintChangedAfter);
    h.start_text();
    h.until(|h| h.engine.last_finished().is_some());
    assert_eq!(h.app.borrow().text_value, "wanted");
    assert_eq!(
        h.inputs()
            .iter()
            .filter(|input| matches!(
                input.primitive,
                Primitive::AxAction {
                    action: AxActionKind::SetValue { .. }
                }
            ))
            .count(),
        1,
        "a changed input-instance fingerprint cannot authorize a recovery setter"
    );
    assert_eq!(
        h.count("SELECT COUNT(*) FROM effect_results WHERE json_extract(json,'$.predicate') = 'text.value@1'"),
        0,
        "the same ObjectRef after its source fingerprint changes is not the input instance's current value"
    );
    assert!(
        h.engine.timeline().any(|entry| entry
            .text
            .contains("text input CurrentUi proof ended unverified (bound_fingerprint_changed)")),
        "the bounded diagnostic identifies the rejected fingerprint comparison without exposing its value"
    );
    assert_ne!(h.engine.last_finished(), Some(RootState::Completed));
}

#[test]
fn transient_missing_window_facts_wait_for_same_attempt_text_current_ui_proof() {
    let mut h = Harness::new_text(Fault::DelayedTextSourceProof);
    h.start_text();
    h.until(|h| {
        h.count("SELECT COUNT(*) FROM effect_results WHERE json_extract(json,'$.predicate') = 'text.value@1'")
            == 1
    });
    assert!(h.engine.timeline().any(|entry| {
        entry
            .text
            .contains("awaits read-only Source proof on its original binding")
    }));
    let text: serde_json::Value = serde_json::from_str(
        &h.engine
            .storage()
            .read()
            .query_row(
                "SELECT json FROM effect_results WHERE json_extract(json,'$.predicate') = 'text.value@1'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(text["causality"], "unknown");
    assert_eq!(text["confirmation_scope"], "current_ui");
    assert_eq!(text["target"], "document");
    assert!(text["observed_ms"].as_u64().unwrap() > text["input_span"]["to_ms"].as_u64().unwrap());
    let setters: Vec<_> = h
        .inputs()
        .into_iter()
        .filter(|input| {
            matches!(
                input.primitive,
                Primitive::AxAction {
                    action: AxActionKind::SetValue { .. }
                }
            )
        })
        .collect();
    assert_eq!(
        setters.len(),
        1,
        "read-only proof remains attached to the original single setter"
    );
    assert_eq!(
        text["attempt_id"],
        setters[0].intent.attempt_id.as_deref().unwrap()
    );
    assert_eq!(text["intent_id"], setters[0].intent.intent_id);
    h.until(|h| h.engine.last_finished().is_some());
    assert_eq!(
        h.engine.last_finished(),
        Some(RootState::Completed),
        "{}",
        h.report()
    );
    assert_eq!(
        h.count("SELECT COUNT(*) FROM attempts WHERE skill = 'ui.enter_text'"),
        1
    );
    assert_eq!(
        h.count("SELECT COUNT(*) FROM attempts WHERE skill = 'ui.revisit'"),
        1
    );
}

#[test]
fn identity_boundary_after_completed_text_preserves_current_ui_and_separate_revisit() {
    let mut h = Harness::new_text(Fault::DelayedTextSourceProof);
    h.text_identity_boundary_changes = true;
    h.start_text();
    h.until(|h| {
        h.count("SELECT COUNT(*) FROM effect_results WHERE json_extract(json,'$.predicate') = 'text.value@1'") == 1
    });
    let text_json: String = h.engine.storage().read().query_row(
        "SELECT json FROM effect_results WHERE json_extract(json,'$.predicate') = 'text.value@1'",
        [], |row| row.get(0),
    ).unwrap();
    let effect: serde_json::Value = serde_json::from_str(&text_json).unwrap();
    let setter = h
        .inputs()
        .into_iter()
        .find(|input| {
            matches!(
                input.primitive,
                Primitive::AxAction {
                    action: AxActionKind::SetValue { .. }
                }
            )
        })
        .unwrap();
    assert_eq!(
        effect["attempt_id"],
        setter.intent.attempt_id.as_deref().unwrap()
    );
    assert_eq!(effect["intent_id"], setter.intent.intent_id);
    assert_eq!(effect["confirmation_scope"], "current_ui");
    assert_eq!(effect["causality"], "unknown");
    let receipt = h.core.query_receipt(1, &setter.intent.intent_id).unwrap();
    assert!(effect["observed_ms"].as_u64().unwrap() > receipt.input_end_ms.unwrap());
    assert!(
        h.engine
            .timeline()
            .any(|entry| entry.text.contains("identity boundary changed"))
    );
    assert_eq!(
        h.app
            .borrow()
            .snapshot
            .as_ref()
            .unwrap()
            .identity_boundary
            .as_ref()
            .unwrap()
            .generation,
        2
    );
    assert_ne!(h.engine.last_finished(), Some(RootState::Completed));
    h.until(|h| h.engine.last_finished().is_some());
    assert_eq!(
        h.engine.last_finished(),
        Some(RootState::Completed),
        "{}",
        h.report()
    );
    let inputs = h.inputs();
    assert_eq!(
        inputs.len(),
        3,
        "one setter and two separate revisit inputs"
    );
    assert_eq!(
        inputs
            .iter()
            .filter(|input| matches!(
                input.primitive,
                Primitive::AxAction {
                    action: AxActionKind::SetValue { .. }
                }
            ))
            .count(),
        1
    );
    assert_eq!(
        h.count("SELECT COUNT(*) FROM attempts WHERE skill = 'ui.enter_text'"),
        1
    );
    assert_eq!(
        h.count("SELECT COUNT(*) FROM attempts WHERE skill = 'ui.revisit'"),
        1
    );
}

#[test]
fn already_matching_text_without_input_does_not_create_text_current_ui_effect() {
    let mut h = Harness::new_text(Fault::AlreadyWantedBefore);
    h.start_text();
    h.until(|h| h.engine.last_finished().is_some());
    assert_eq!(h.app.borrow().text_value, "wanted");
    assert_eq!(
        h.inputs()
            .iter()
            .filter(|input| matches!(
                input.primitive,
                Primitive::AxAction {
                    action: AxActionKind::SetValue { .. }
                }
            ))
            .count(),
        0,
        "AlreadyDone navigates for revisit without submitting a setter: {}",
        h.report()
    );
    assert_eq!(
        h.count("SELECT COUNT(*) FROM effect_results WHERE json_extract(json,'$.predicate') = 'text.value@1'"),
        0,
        "zero-input AlreadyDone is not attributed to an old or fabricated input"
    );
}

#[test]
fn stopping_during_pending_text_proof_clears_it_without_effect_or_retry() {
    let mut h = Harness::new_text(Fault::HeldTextSourceAfterFirstInvalid);
    h.start_text();
    h.until(|h| {
        h.engine.timeline().any(|entry| {
            entry
                .text
                .contains("awaits read-only Source proof on its original binding")
        })
    });
    h.send(Input::Operator(OperatorCommand::StopTask {
        root_task_id: None,
    }));
    assert_eq!(
        h.engine.last_finished(),
        Some(RootState::Cancelled),
        "{}",
        h.report()
    );
    for _ in 0..4 {
        h.send(Input::Tick);
    }
    assert_eq!(
        h.inputs()
            .iter()
            .filter(|input| matches!(
                input.primitive,
                Primitive::AxAction {
                    action: AxActionKind::SetValue { .. }
                }
            ))
            .count(),
        1,
        "stop discards the pending verifier and never repeats the setter"
    );
    assert_eq!(
        h.count("SELECT COUNT(*) FROM effect_results WHERE json_extract(json,'$.predicate') = 'text.value@1'"),
        0
    );
}

#[test]
fn device_restart_during_pending_text_proof_does_not_resume_or_replay_input() {
    let mut h = Harness::new_text(Fault::HeldTextSourceAfterFirstInvalid);
    h.start_text();
    h.until(|h| {
        h.engine.timeline().any(|entry| {
            entry
                .text
                .contains("awaits read-only Source proof on its original binding")
        })
    });
    h.core.disconnect();
    h.send(Input::DeviceNotice(DeviceNotice::Manifest(Box::new(
        CapabilityManifest {
            identity: h.core.identity(),
            backend: BackendKind::Macos,
            execution_domain: ExecutionDomain::ForegroundNative,
            policy: PrimitivePolicy::AtomicOnly,
            human_input: CapabilityState::Available,
            hit_test: CapabilityState::Available,
            input: CapabilityState::Available,
            domains: common::domain_capabilities_for_test_profile(
                ExecutionDomain::ForegroundNative,
            ),
            max_uninterruptible_ms: 100,
        },
    ))));
    for _ in 0..4 {
        h.send(Input::Tick);
    }
    assert_eq!(
        h.inputs()
            .iter()
            .filter(|input| matches!(
                input.primitive,
                Primitive::AxAction {
                    action: AxActionKind::SetValue { .. }
                }
            ))
            .count(),
        1,
        "the restarted device has no ephemeral proof that permits another setter"
    );
    assert_eq!(
        h.count("SELECT COUNT(*) FROM effect_results WHERE json_extract(json,'$.predicate') = 'text.value@1'"),
        0
    );
    assert_ne!(h.engine.root_state(), Some(RootState::Completed));
}

#[test]
fn completed_text_input_without_an_actual_end_time_is_never_retried() {
    let mut h = Harness::new_text(Fault::MissingTextInputEnd);
    h.start_text();
    h.until(|h| h.engine.last_finished().is_some());
    let inputs = h.inputs();
    assert_eq!(h.app.borrow().text_value, "wanted");
    assert_eq!(
        inputs
            .iter()
            .filter(|input| matches!(
                input.primitive,
                Primitive::AxAction {
                    action: AxActionKind::SetValue { .. }
                }
            ))
            .count(),
        1,
        "missing CurrentUi proof after a completed setter allows no recovery setter"
    );
    assert_eq!(
        h.count("SELECT COUNT(*) FROM effect_results WHERE json_extract(json,'$.predicate') = 'text.value@1'"),
        0,
        "the receipt's command end must not substitute for the actual input end"
    );
    assert_ne!(h.engine.last_finished(), Some(RootState::Completed));
}

#[test]
fn completed_no_effect_has_no_key_retry_or_ax_pointer_fallback() {
    let mut h = Harness::new(Fault::NoEffect, false);
    h.start();
    h.until(|h| h.engine.last_finished().is_some());
    for _ in 0..20 {
        h.step();
    }
    assert_eq!(h.inputs().len(), 1, "{}", h.report());
    assert!(!h.app.borrow().value);
    assert_ne!(h.engine.last_finished(), Some(RootState::Completed));
}
#[test]
fn already_matching_value_revisits_without_a_key() {
    let mut h = Harness::new(Fault::None, true);
    h.start();
    h.until(|h| h.engine.last_finished().is_some());
    assert_eq!(
        h.engine.last_finished(),
        Some(RootState::Completed),
        "{}",
        h.report()
    );
    assert_eq!(h.inputs().len(), 2);
    assert_eq!(
        h.inputs()
            .iter()
            .map(|input| input.primitive.clone())
            .collect::<Vec<_>>(),
        vec![click_at(450, 53), click_at(450, 23)]
    );
}
#[test]
fn focus_absent_or_on_another_control_never_uses_displayed_focused_bool() {
    for fault in [Fault::NoFocus, Fault::WrongFocus] {
        let mut h = Harness::new(fault, false);
        h.start();
        for _ in 0..80 {
            h.step();
        }
        assert!(h.inputs().is_empty(), "{fault:?}: {}", h.report());
        assert_ne!(h.engine.last_finished(), Some(RootState::Completed));
    }
}
#[test]
fn result_read_cannot_accept_changed_frame_or_secure_ancestor() {
    for fault in [
        Fault::MovedAfter,
        Fault::SecureAfter,
        Fault::SecureSubroleAfter,
    ] {
        let mut h = Harness::new(fault, false);
        h.start();
        for _ in 0..200 {
            h.step();
            if h.engine.last_finished().is_some() {
                break;
            }
        }
        assert_eq!(h.inputs().len(), 1, "{fault:?}: {}", h.report());
        assert_ne!(h.engine.last_finished(), Some(RootState::Completed));
    }
}

#[test]
fn focus_changes_after_grant_revoke_input_instead_of_using_old_scope() {
    let mut h = Harness::new(Fault::FocusChangedAtGrant, false);
    h.start();
    for _ in 0..80 {
        h.step();
    }
    assert!(h.inputs().is_empty(), "{}", h.report());
    assert_ne!(h.engine.last_finished(), Some(RootState::Completed));
}
#[test]
fn partial_source_never_authorizes_the_registered_key() {
    let mut h = Harness::new(Fault::Partial, false);
    h.start();
    for _ in 0..80 {
        h.step();
    }
    assert!(h.inputs().is_empty(), "{}", h.report());
}

#[test]
fn result_read_requires_new_kernel_window_facts_after_known_input() {
    for fault in [Fault::MissingWindowProof, Fault::RestartedProcess] {
        let mut h = Harness::new(fault, false);
        h.start();
        for _ in 0..200 {
            h.step();
            if h.engine.last_finished().is_some() {
                break;
            }
        }
        assert!(h.app.borrow().value);
        assert_eq!(h.inputs().len(), 1, "{fault:?}: {}", h.report());
        assert_ne!(h.engine.last_finished(), Some(RootState::Completed));
        assert_eq!(h.count("SELECT COUNT(*) FROM effect_results WHERE json_extract(json,'$.status') = 'satisfied'"), 0);
    }
}
