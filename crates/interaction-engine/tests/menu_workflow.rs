//! A test-only menu application. Source facts follow real ExecutionCore
//! submissions; the Engine owns admission, scope construction and verification.

mod common;

use execution_core::backend::{
    HeldState, HitTestOutcome, HitTestRequest, InputBackend, ReleaseFailure, SubmitCompletion,
    SubmitContext, SubmitOutcome,
};
use execution_core::core::{CoreConfig, CoreEvent, ExecutionCore};
use interaction_contracts::desktop::binding::{
    DeviceContextMenuRelation, DeviceContextWitness, MenuBindingScope, instance_fingerprint,
};
use interaction_contracts::desktop::device::*;
use interaction_contracts::desktop::domain::ExecutionDomain;
use interaction_contracts::desktop::effect::EffectClass;
use interaction_contracts::desktop::ids::{ObjectRef, SemanticTargetRef};
use interaction_contracts::desktop::operator::{OperatorCommand, RunMode, UiProjection};
use interaction_contracts::desktop::planning::*;
use interaction_contracts::desktop::primitive::{
    AxActionKind, Modifiers, MouseButton, Primitive, PrimitivePolicy,
};
use interaction_contracts::desktop::source::*;
use interaction_contracts::desktop::task::{
    ConfirmationScope, ExpectedValue, RootState, TargetSpec,
};
use interaction_engine::engine::{Engine, EngineConfig, Input, Output};
use interaction_engine::environment::pack::{AppPack, EffectRule};
use interaction_engine::execution::{DeviceCommand, DeviceNotice, DeviceReply};
use interaction_engine::infrastructure::storage::Storage;
use interaction_engine::tasks::TaskConfig;
use sim_world::app::{SimApp, SnapshotMeta};
use sim_world::policy::ScriptedTeacher;
use std::{cell::RefCell, collections::VecDeque, rc::Rc};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Fault {
    #[default]
    None,
    UnknownShow,
    UnknownPress,
    Disabled,
    Duplicate,
    AnchorReplaced,
    WatchReplaced,
    NoEffect,
}

#[derive(Clone, Copy, Debug, Default)]
enum ResultChange {
    #[default]
    None,
    AnchorRetired,
    OpenerMoved,
    KernelChanged,
    PidChanged,
    AppChanged,
    WindowChanged,
    RootFrameChanged,
    EpochChanged,
}

/// Model the normal foreground AX + screenshot pair using the same focused
/// application window. A deliberate foreign-focus mutation has no matching
/// target window and therefore cannot produce target execution pixels.
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
        frame_id: format!("menu-image-{}", snapshot.source_sequence),
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
        checksum: format!("sha256:menu-image-{}", snapshot.source_sequence),
        provenance: Some(FrameProvenance::Stream {
            stream: FrameStreamInfo {
                stream_id: "menu-workflow-screen".into(),
                image_frame_id: Some(format!("menu-image-{}", snapshot.source_sequence)),
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

#[derive(Debug, Clone)]
struct Applied {
    at_ms: u64,
    source_sequence_at_submit: u64,
    intent: ActionIntent,
    primitive: Primitive,
    purpose: ObservationPurpose,
}

struct Application {
    fault: Fault,
    open: bool,
    other_page: bool,
    value: String,
    menu_generation: u32,
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
        let source_sequence_at_submit = app
            .snapshot
            .as_ref()
            .expect("the test Source read is present before input")
            .source_sequence;
        let object = app
            .snapshot
            .as_ref()
            .unwrap()
            .objects
            .iter()
            .find(|object| {
                object.instance_ref == context.intent.binding_proof.as_ref().unwrap().instance
            })
            .unwrap()
            .clone();
        let unknown = match primitive {
            Primitive::AxAction {
                action: AxActionKind::ShowMenu,
            } => {
                assert!(!app.open);
                assert_eq!(object.role, "AXMenuButton");
                assert_eq!(
                    context.authority.evidence.purpose,
                    ObservationPurpose::TargetInput
                );
                assert!(matches!(
                    context.intent.binding_proof.as_ref().unwrap().menu_scope,
                    Some(MenuBindingScope::Opener { .. })
                ));
                assert!(matches!(
                    &context
                        .intent
                        .binding_proof
                        .as_ref()
                        .unwrap()
                        .device_context,
                    DeviceContextWitness::WholeStructureV1 { menu: Some(_), .. }
                ));
                app.open = true;
                app.fault == Fault::UnknownShow
            }
            Primitive::AxAction {
                action: AxActionKind::Press,
            } if object.role == "AXMenuItem" => {
                assert!(app.open);
                assert_eq!(object.title.as_deref(), Some("Second"));
                assert_eq!(
                    context.authority.evidence.purpose,
                    ObservationPurpose::MenuSelection
                );
                assert!(matches!(
                    context.intent.binding_proof.as_ref().unwrap().menu_scope,
                    Some(MenuBindingScope::Item { .. })
                ));
                assert!(matches!(
                    &context
                        .intent
                        .binding_proof
                        .as_ref()
                        .unwrap()
                        .device_context,
                    DeviceContextWitness::WholeStructureV1 {
                        menu: Some(menu),
                        ..
                    } if matches!(menu.as_ref(), DeviceContextMenuRelation::AnchoredMenu { .. })
                ));
                app.open = false;
                if app.fault != Fault::NoEffect {
                    app.value = "Second".into();
                }
                app.fault == Fault::UnknownPress
            }
            Primitive::Click {
                button: MouseButton::Left,
                x,
                y,
                modifiers,
            } if object.role == "AXRadioButton" => {
                assert!(
                    !app.open,
                    "background navigation is forbidden while the menu is open"
                );
                assert_eq!(*modifiers, Modifiers::default());
                let (center_x, center_y) = object.frame.center();
                assert_eq!((*x, *y), (center_x.round() as i32, center_y.round() as i32));
                app.other_page = object.title.as_deref() == Some("Other");
                false
            }
            _ => panic!("unexpected menu fixture input: {primitive:?}"),
        };
        app.inputs.push(Applied {
            at_ms,
            source_sequence_at_submit,
            intent: context.intent.clone(),
            primitive: primitive.clone(),
            purpose: context.authority.evidence.purpose,
        });
        // Exercise the native asynchronous receipt path with a fixture call
        // end, preserving the real Core's input_end_ms and opener audit rules.
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
            input_end_ms: Some(at_ms + 1),
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
            .map_or(HitTestOutcome::Unresolved, |object| {
                HitTestOutcome::Element(instance_fingerprint(snapshot, object).digest())
            })
    }
}

fn pack(choice_class: EffectClass) -> AppPack {
    let mut pack = AppPack::settings_fixture();
    pack.nav_role = "AXRadioButton".into();
    for (role, label, class) in [
        ("AXMenuButton", "locale", EffectClass::ReversibleEdit),
        ("AXMenuItem", "second", choice_class),
        ("AXRadioButton", "", EffectClass::Navigation),
    ] {
        pack.rules.push(EffectRule {
            role: Some(role.into()),
            label_contains: label.into(),
            class,
        });
    }
    pack
}

fn navigation_click_at(x: i32, y: i32) -> Primitive {
    Primitive::Click {
        button: MouseButton::Left,
        x,
        y,
        modifiers: Modifiers::default(),
    }
}

fn plan() -> PlanProposal {
    let semantic = SemanticTargetRef::new("locale").unwrap();
    PlanProposal {
        goal_draft: Some(GoalDraft {
            app: "test.settings".into(),
            account: None,
            target: TargetSpec {
                row: None,
                semantic_target: semantic.clone(),
                label: "Locale".into(),
                group: None,
                role_hint: Some("AXMenuButton".into()),
            },
            desired: ExpectedValue::Text {
                value: "Second".into(),
            },
            confirmation_scope: ConfirmationScope::UiRevisit,
            max_effect_class: EffectClass::ReversibleEdit,
        }),
        bundle: PlanBundle {
            subgoals: vec![
                Subgoal {
                    id: "select".into(),
                    description: "Select Second".into(),
                    skill: "ui.select_by_label".into(),
                    target: Some(semantic.clone()),
                    expects: vec!["actual closed-menu Locale value is Second".into()],
                    scroll: None,
                    select: Some(SelectByLabelSpec {
                        choice_label: "Second".into(),
                    }),
                },
                Subgoal {
                    id: "revisit".into(),
                    description: "Revisit Doc".into(),
                    skill: "ui.revisit".into(),
                    target: Some(semantic),
                    expects: vec!["value persists after return".into()],
                    scroll: None,
                    select: None,
                },
            ],
            dependencies: vec![Dependency {
                before: "select".into(),
                after: "revisit".into(),
            }],
            required_observations: vec![],
            expected_effects: vec![],
            recovery_branches: vec![],
            termination: vec!["all frozen root predicates hold".into()],
        },
    }
}

struct Harness {
    engine: Engine,
    core: ExecutionCore<Backend>,
    app: Rc<RefCell<Application>>,
    pack: AppPack,
    queue: VecDeque<Input>,
    now: u64,
    sequence: u64,
    ui: Option<UiProjection>,
    observations: Vec<Observation>,
    snapshots: Vec<StructureSnapshot>,
    grants: usize,
    /// All Source `RefreshNow` commands, including lifecycle scope clears.
    refreshes: usize,
    /// Clear-only RefreshNow commands emitted while handling unavailable health.
    scope_clear_refreshes: usize,
    /// Business RefreshNow commands that await a Source observation.
    business_refreshes: usize,
    /// Closed-menu Source publications after a known completed menu selection.
    readonly_result_reads: usize,
    ax_health_sequence: u64,
    watch_generation: usize,
    source_up: bool,
    result_change: ResultChange,
    hold_item_grant: bool,
    held_grant: Option<(u64, DeviceReply)>,
}

impl Harness {
    fn new(fault: Fault) -> Self {
        Self::with_class(fault, EffectClass::ReversibleEdit)
    }
    fn with_class(fault: Fault, choice_class: EffectClass) -> Self {
        let app = Rc::new(RefCell::new(Application {
            fault,
            open: false,
            other_page: false,
            value: "First".into(),
            menu_generation: 1,
            inputs: vec![],
            snapshot: None,
        }));
        let pack = pack(choice_class);
        let mut config = EngineConfig::new(TaskConfig::new(vec![pack.clone()]));
        config.initial_mode = RunMode::Execute;
        let mut core = ExecutionCore::new(
            CoreConfig::production("menu-test"),
            Backend {
                app: app.clone(),
                sequence: 0,
                completions: VecDeque::new(),
            },
        );
        core.renew_lease(60_000);
        let mut h = Self {
            engine: Engine::new(config, Storage::open_in_memory().unwrap()),
            core,
            app,
            pack,
            queue: VecDeque::new(),
            now: 0,
            sequence: 0,
            ui: None,
            observations: vec![],
            snapshots: vec![],
            grants: 0,
            refreshes: 0,
            scope_clear_refreshes: 0,
            business_refreshes: 0,
            readonly_result_reads: 0,
            ax_health_sequence: 0,
            watch_generation: 0,
            source_up: true,
            result_change: ResultChange::None,
            hold_item_grant: false,
            held_grant: None,
        };
        h.send(Input::DeviceNotice(DeviceNotice::Manifest(Box::new(
            h.manifest(),
        ))));
        h
    }
    fn manifest(&self) -> CapabilityManifest {
        CapabilityManifest {
            identity: self.core.identity(),
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
        }
    }

    fn ax_health(&mut self, state: SourceState, reason: Option<String>) -> SourceHealth {
        self.ax_health_sequence += 1;
        SourceHealth {
            source: SourceKind::Ax,
            state,
            reason,
            at_ms: self.now,
            provenance: Some(SourceStamp {
                session_epoch: self.core.identity().session_epoch,
                stream_id: format!("watch-{}", self.watch_generation),
                source_sequence: self.ax_health_sequence,
            }),
        }
    }

    fn source(&mut self) {
        if !self.source_up {
            return;
        }
        self.sequence += 1;
        let identity = self.core.identity();
        let mut snapshot = SimApp::new().snapshot(&SnapshotMeta {
            snapshot_id: format!("menu-read-{}", self.sequence),
            session_epoch: identity.session_epoch,
            surface: identity.surface,
            clock_domain: identity.clock_domain,
            source_sequence: self.sequence,
            observed_ms: self.now,
        });
        let app = self.app.borrow();
        // Mutations are fresh test Source facts after the actual final input;
        // they never alter Engine verdicts or the Core's Completed receipt.
        let result_change = if app.inputs.len() >= 2 && !app.open {
            self.result_change
        } else {
            ResultChange::None
        };
        match result_change {
            ResultChange::PidChanged => {
                snapshot.app.pid += 1;
                snapshot.global_focus.app = ObservationFact::Known {
                    value: snapshot.app.clone(),
                };
            }
            ResultChange::AppChanged => {
                snapshot.app = AppIdentity {
                    bundle_id: "test.other".into(),
                    name: "Other application".into(),
                    pid: 9000,
                };
                snapshot.windows[0].window_id = "other-window".into();
                snapshot.windows[0].owner_bundle_id = snapshot.app.bundle_id.clone();
            }
            ResultChange::WindowChanged => {
                snapshot.windows[0].window_id = "replacement-window".into();
                snapshot.global_focus.window_id = ObservationFact::Known {
                    value: "replacement-window".into(),
                };
            }
            ResultChange::RootFrameChanged => snapshot.windows[0].frame.width += 10.0,
            ResultChange::EpochChanged => snapshot.session_epoch += 1,
            _ => {}
        }
        // Every published Source read names its actual foreground app and
        // window. AppChanged models a valid read from a different app/window,
        // which remains outside the frozen authorization scope.
        snapshot.global_focus.app = ObservationFact::Known {
            value: snapshot.app.clone(),
        };
        snapshot.global_focus.window_id = ObservationFact::Known {
            value: snapshot.windows[0].window_id.clone(),
        };
        snapshot.windows[0].title = Some(format!(
            "Settings — {}",
            if app.other_page { "Other" } else { "Doc" }
        ));
        let window = snapshot.windows[0].clone();
        let root_id = ObjectRef::new(format!("root-registry-{}", self.sequence)).unwrap();
        let object = |id: String,
                      role: &str,
                      title: &str,
                      parent: Option<ObjectRef>,
                      frame: Rect,
                      actions: Vec<AxActionName>| UiObject {
            instance_ref: ObjectRef::new(id).unwrap(),
            parent,
            window_id: window.window_id.clone(),
            role: role.into(),
            subrole: None,
            title: Some(title.into()),
            description: None,
            value: None,
            enabled: true,
            focused: false,
            frame,
            actions,
            ax_action_names: None,
            in_view: true,
            group: None,
            secure: false,
            scroll: None,
            identity_continuity: ObjectIdentityContinuity::ReadScoped,
        };
        let mut root = object(
            root_id.as_str().into(),
            "AXWindow",
            "",
            None,
            window.frame,
            vec![],
        );
        root.subrole = Some("AXStandardWindow".into());
        snapshot.objects = vec![root];
        for (label, y) in [("Doc", 10.0), ("Other", 40.0)] {
            snapshot.objects.push(object(
                format!("nav-{label}-{}", self.sequence),
                "AXRadioButton",
                label,
                Some(root_id.clone()),
                Rect {
                    x: 400.0,
                    y,
                    width: 120.0,
                    height: 25.0,
                },
                vec![AxActionName::Press],
            ));
        }
        snapshot.menu_anchors.clear();
        snapshot.menu_context = None;
        if !app.other_page {
            let opener_id = ObjectRef::new(format!("opener-registry-{}", self.sequence)).unwrap();
            let mut opener = object(
                opener_id.as_str().into(),
                "AXMenuButton",
                "Locale",
                Some(root_id),
                Rect {
                    x: if matches!(result_change, ResultChange::OpenerMoved) {
                        40.0
                    } else {
                        30.0
                    },
                    y: 70.0,
                    width: 180.0,
                    height: 30.0,
                },
                vec![AxActionName::ShowMenu],
            );
            opener.value = Some(UiValue::Text(app.value.clone()));
            snapshot.objects.push(opener.clone());
            let anchor_id = if app.open && app.fault == Fault::AnchorReplaced
                || matches!(result_change, ResultChange::AnchorRetired)
            {
                "new-anchor"
            } else {
                "retained-anchor"
            };
            let session = if app.open && app.fault == Fault::WatchReplaced {
                "replacement-watch".to_owned()
            } else {
                // RefreshNow starts a new Source watch; periodic reads alone
                // preserve it across ordinary registry generations.
                format!("watch-{}", self.watch_generation)
            };
            let anchor = MenuOpenerAnchor {
                anchor_id: anchor_id.into(),
                source_session_id: session.clone(),
                anchor_revision: if matches!(result_change, ResultChange::AnchorRetired) {
                    2
                } else {
                    1
                },
                process: KernelProcessIdentity {
                    pid: snapshot.app.pid,
                    start_seconds: if matches!(result_change, ResultChange::KernelChanged) {
                        2
                    } else {
                        1
                    },
                    start_micros: 0,
                },
                origin_window_id: window.window_id.clone(),
                origin_window_frame: window.frame,
                opener_instance: opener_id.clone(),
                opener_fingerprint: instance_fingerprint(&snapshot, &opener),
                opener_frame: opener.frame,
                verified_source_sequence: self.sequence,
                verified_observed_ms: self.now,
                continuity: MenuAnchorContinuity::DirectRead,
            };
            snapshot.menu_anchors.push(anchor.clone());
            if app.open {
                let menu_id = ObjectRef::new(format!("menu-{}", app.menu_generation)).unwrap();
                let menu_frame = Rect {
                    x: 30.0,
                    y: 105.0,
                    width: 180.0,
                    height: 90.0,
                };
                let menu = object(
                    menu_id.as_str().into(),
                    "AXMenu",
                    "",
                    Some(opener_id),
                    menu_frame,
                    vec![],
                );
                snapshot.objects.push(menu.clone());
                for (label, y) in [("First", 105.0), ("Second", 135.0)] {
                    let mut item = object(
                        format!("choice-{label}-{}", app.menu_generation),
                        "AXMenuItem",
                        label,
                        Some(menu_id.clone()),
                        Rect {
                            x: 30.0,
                            y,
                            width: 180.0,
                            height: 30.0,
                        },
                        vec![AxActionName::Press],
                    );
                    item.enabled = !(label == "Second" && app.fault == Fault::Disabled);
                    snapshot.objects.push(item);
                }
                if app.fault == Fault::Duplicate {
                    snapshot.objects.push(object(
                        "duplicate-choice".into(),
                        "AXMenuItem",
                        "Second",
                        Some(menu_id.clone()),
                        Rect {
                            x: 30.0,
                            y: 165.0,
                            width: 180.0,
                            height: 30.0,
                        },
                        vec![AxActionName::Press],
                    ));
                }
                snapshot.menu_context = Some(MenuContext {
                    context_id: format!("context-{}", app.menu_generation),
                    source_session_id: session,
                    opener: anchor,
                    menu_instance: menu_id,
                    menu_fingerprint: instance_fingerprint(&snapshot, &menu),
                    menu_frame,
                    drawing_window: MenuDrawingWindow {
                        window_id: 999,
                        owner_pid: snapshot.app.pid,
                        layer: 101,
                        frame: menu_frame,
                    },
                });
            }
        }
        drop(app);
        sim_world::app::refresh_simulated_observer_coverage(&mut snapshot);
        snapshot
            .validate()
            .expect("test source must publish consistent actual tree and provenance");
        let frame = frame_for_snapshot(&snapshot);
        let completed_selection = self
            .app
            .borrow()
            .inputs
            .iter()
            .rev()
            .find(|input| input.purpose == ObservationPurpose::MenuSelection)
            .cloned();
        let is_readonly_result_read = self.readonly_result_reads == 0
            && completed_selection
                .and_then(|input| {
                    self.core
                        .query_receipt(input.intent.session_epoch, &input.intent.intent_id)
                        .filter(|receipt| receipt.status == ReceiptStatus::Completed)
                        .and_then(|receipt| receipt.input_end_ms)
                        .map(|input_end_ms| {
                            snapshot.complete
                                && snapshot.menu_context.is_none()
                                && snapshot.source_sequence > input.source_sequence_at_submit
                                && snapshot.observed_ms > input_end_ms
                        })
                })
                .unwrap_or(false);
        self.app.borrow_mut().snapshot = Some(snapshot.clone());
        self.snapshots.push(snapshot.clone());
        if is_readonly_result_read {
            // Foreground native runs wait on their existing watch for this
            // proof; the periodic Source publication is not a RefreshNow.
            self.readonly_result_reads += 1;
        }
        self.queue.push_back(Input::Structure(Box::new(snapshot)));
        if let Some(frame) = frame {
            self.queue.push_back(Input::Frame(Box::new(frame)));
        }
    }

    fn send(&mut self, input: Input) {
        self.queue.push_back(input);
        let mut deliveries = 0;
        while let Some(input) = self.queue.pop_front() {
            deliveries += 1;
            assert!(deliveries < 10_000, "menu engine did not settle");
            let unavailable_health = matches!(
                &input,
                Input::SourceHealth(health) if health.state == SourceState::Unavailable
            );
            let mut classified_scope_clear = false;
            for output in self.engine.handle(self.now, input) {
                match output {
                    Output::Ui(ui) => self.ui = Some(*ui),
                    Output::Source {
                        generation,
                        command,
                    } => {
                        let is_refresh = matches!(&command, SourceCommand::RefreshNow { .. });
                        let is_scope_clear = unavailable_health
                            && matches!(&command, SourceCommand::RefreshNow { table_scope: None })
                            && !classified_scope_clear;
                        if is_refresh {
                            self.refreshes += 1;
                            if is_scope_clear {
                                self.scope_clear_refreshes += 1;
                                classified_scope_clear = true;
                            } else {
                                self.business_refreshes += 1;
                            }
                        }
                        self.watch_generation += 1;
                        self.source();
                        self.queue.push_back(Input::SourceRefreshStarted {
                            generation,
                            accepted: true,
                        });
                    }
                    Output::BrowserRefresh(_) => {}
                    Output::Plan(request) if request.reason == PlanningReason::GoalCompile => {
                        self.queue.push_back(Input::Planner {
                            request_id: request.request_id,
                            generation: request.generation,
                            output: Box::new(PlannerOutput::Plan {
                                proposal: Box::new(plan()),
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
        let is_item_grant = matches!(command, DeviceCommand::GrantPermit(_))
            && self
                .observations
                .last()
                .is_some_and(|o| o.purpose == ObservationPurpose::MenuSelection);
        let reply = match command {
            DeviceCommand::RegisterRoot(grant) => ack(self.core.register_root(&grant)),
            DeviceCommand::Observe(observation) => {
                let observation = observation
                    .native()
                    .expect("menu workflow uses native evidence")
                    .clone();
                self.observations.push(observation.clone());
                ack(self.core.observe(observation))
            }
            DeviceCommand::GrantPermit(request) => {
                self.grants += 1;
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
        if self.hold_item_grant && is_item_grant {
            assert!(self.held_grant.is_none());
            self.held_grant = Some((corr, reply));
        } else {
            self.queue.push_back(Input::Device { corr, reply });
        }
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
        self.send(Input::Operator(OperatorCommand::CreateTask {
            text: "Set Locale to Second and verify after revisit".into(),
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
    fn effects(&self, predicate: &str) -> Vec<serde_json::Value> {
        self.engine.storage().read().prepare("SELECT json FROM effect_results WHERE json_extract(json, '$.predicate') = ?1 AND json_extract(json, '$.status') = 'satisfied'").unwrap().query_map([predicate], |r| r.get::<_, String>(0)).unwrap().map(|r| serde_json::from_str(&r.unwrap()).unwrap()).collect()
    }
    fn inputs(&self) -> Vec<Applied> {
        self.app.borrow().inputs.clone()
    }
    fn report(&self) -> String {
        format!(
            "state {:?}, ended {:?}, fault {:?}, notes {:?}, inputs {:?}, observations {:?}",
            self.engine.root_state(),
            self.engine.last_finished(),
            self.engine.faulted(),
            self.engine.timeline().map(|n| &n.text).collect::<Vec<_>>(),
            self.app.borrow().inputs,
            self.observations
        )
    }
}

fn assert_known_selection_source_accounting(h: &Harness) {
    assert_eq!(
        h.refreshes, 2,
        "the flow has one initial RefreshNow and one lifecycle scope clear"
    );
    assert_eq!(
        h.scope_clear_refreshes, 1,
        "the Source gap gets one out-of-band scope clear"
    );
    assert_eq!(
        h.business_refreshes, 1,
        "the initial watch refresh is the only business RefreshNow"
    );
    assert_eq!(
        h.readonly_result_reads, 1,
        "a known final input permits one post-input closed-menu Source read"
    );
}

fn assert_known_selection_wait_accounting(h: &Harness) {
    assert_eq!(
        h.refreshes, 2,
        "the flow has one initial RefreshNow and one lifecycle scope clear"
    );
    assert_eq!(
        h.scope_clear_refreshes, 1,
        "the Source gap gets one out-of-band scope clear"
    );
    assert_eq!(
        h.business_refreshes, 1,
        "the initial watch refresh is the only business RefreshNow"
    );
    assert_eq!(
        h.readonly_result_reads, 0,
        "the Source gap has not supplied the post-input read yet"
    );
    assert!(
        h.engine
            .timeline()
            .any(|entry| entry.text == "observing again: evidence from after the input"),
        "the known terminal input remains in its read-only Source wait"
    );
}

#[test]
fn actual_menu_selection_then_revisit_preserves_the_stronger_frozen_goal() {
    let mut h = Harness::new(Fault::None);
    h.start();
    let frozen: String = h
        .engine
        .storage()
        .read()
        .query_row("SELECT goal_json FROM root_tasks", [], |r| r.get(0))
        .unwrap();
    h.until(|h| !h.effects("menu.selected_value@1").is_empty());
    let selected = h.effects("menu.selected_value@1");
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0]["confirmation_scope"], "current_ui");
    assert_eq!(
        h.refreshes, 1,
        "the live foreground watch supplies result proof"
    );
    assert_eq!(h.business_refreshes, 1);
    assert_eq!(h.scope_clear_refreshes, 0);
    assert_eq!(
        h.readonly_result_reads, 1,
        "one fresh closed-menu Source read proves the known result"
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
    assert_eq!(inputs.len(), 4, "ShowMenu, selection, away, return");
    assert_eq!(
        inputs[2].primitive,
        navigation_click_at(460, 53),
        "Away click"
    );
    assert_eq!(
        inputs[3].primitive,
        navigation_click_at(460, 23),
        "Back click"
    );
    assert_eq!(
        inputs
            .iter()
            .filter(|input| matches!(input.primitive, Primitive::Click { .. }))
            .count(),
        2,
        "only the registered page navigation uses pointer clicks"
    );
    assert_eq!(inputs[0].intent.attempt_id, inputs[1].intent.attempt_id);
    assert_ne!(inputs[0].intent.intent_id, inputs[1].intent.intent_id);
    assert_ne!(inputs[0].intent.permit, inputs[1].intent.permit);
    assert_eq!(inputs[0].purpose, ObservationPurpose::TargetInput);
    assert_eq!(inputs[1].purpose, ObservationPurpose::MenuSelection);
    let opening = h
        .core
        .query_receipt(1, &inputs[0].intent.intent_id)
        .unwrap();
    let choosing = h
        .core
        .query_receipt(1, &inputs[1].intent.intent_id)
        .unwrap();
    assert_eq!(opening.status, ReceiptStatus::Completed);
    assert_eq!(choosing.status, ReceiptStatus::Completed);
    assert!(inputs[1].at_ms > inputs[0].at_ms);
    let item_observation = h
        .observations
        .iter()
        .find(|o| o.purpose == ObservationPurpose::MenuSelection)
        .unwrap();
    assert!(item_observation.observed_ms > opening.input_end_ms.unwrap());
    assert!(selected[0]["observed_ms"].as_u64().unwrap() > choosing.input_end_ms.unwrap());
    let opener_instance = &inputs[0].intent.binding_proof.as_ref().unwrap().instance;
    let Some(MenuBindingScope::Item { context, opened_by }) = inputs[1]
        .intent
        .binding_proof
        .as_ref()
        .unwrap()
        .menu_scope
        .as_ref()
    else {
        unreachable!()
    };
    assert_ne!(
        opener_instance, &context.opener.opener_instance,
        "ordinary registry changed while the actual Source anchor remained"
    );
    assert_eq!(opened_by.intent_id, inputs[0].intent.intent_id);
    let Some(MenuBindingScope::Opener {
        attempt_deadline_ms,
        ..
    }) = inputs[0]
        .intent
        .binding_proof
        .as_ref()
        .unwrap()
        .menu_scope
        .as_ref()
    else {
        unreachable!()
    };
    assert_eq!(opened_by.attempt_deadline_ms, *attempt_deadline_ms);
    assert!(opening.command_end_ms.unwrap() <= *attempt_deadline_ms);
    assert!(choosing.command_end_ms.unwrap() <= *attempt_deadline_ms);
    assert_eq!(
        h.count("SELECT COUNT(*) FROM attempts WHERE skill = 'ui.select_by_label'"),
        1
    );
    assert_eq!(h.grants, 4);
    assert_eq!(
        h.count("SELECT quota_actions FROM attempts WHERE skill = 'ui.select_by_label'"),
        2
    );
    assert_eq!(
        h.count("SELECT quota_observations FROM attempts WHERE skill = 'ui.select_by_label'"),
        3
    );
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM event_log WHERE kind = 'root_state' AND detail LIKE '%Completed'"
        ),
        1
    );
    let after: String = h
        .engine
        .storage()
        .read()
        .query_row("SELECT goal_json FROM root_tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(frozen, after);
    assert!(after.contains("revisited"));
    let root = h.effects("goal.required@1");
    assert_eq!(root[0]["confirmation_scope"], "ui_revisit");
    assert!(
        root[0]["observed_ms"].as_u64().unwrap() > selected[0]["observed_ms"].as_u64().unwrap()
    );
    let returned = h
        .app
        .borrow()
        .snapshot
        .as_ref()
        .unwrap()
        .snapshot_id
        .as_str()
        .to_owned();
    assert!(
        root[0]["evidence_group"]
            .as_array()
            .unwrap()
            .iter()
            .any(|id| id == &returned)
    );
}

/// Engine/Source simulation only: no native frame stream, AX handles or helper
/// are used. Synthetic FrameMeta accompanies each fixture snapshot so the
/// foreground gate remains enabled. The Core reports call-end before the
/// Source gap closes and later restores the read-only evidence path.
fn known_selection_then_source_gap() -> (Harness, Applied, Receipt) {
    let mut h = Harness::new(Fault::None);
    h.start();
    h.until(|h| h.inputs().len() == 2);
    let choosing = h.inputs()[1].clone();
    // Prevent the first post-selection Source reading from reaching Engine.
    h.source_up = false;
    h.until(|h| {
        h.core
            .query_receipt(1, &choosing.intent.intent_id)
            .is_some_and(|r| r.status == ReceiptStatus::Completed)
    });
    let receipt = h.core.query_receipt(1, &choosing.intent.intent_id).unwrap();
    assert!(receipt.input_end_ms.is_some());
    assert!(h.effects("menu.selected_value@1").is_empty());
    let unavailable = h.ax_health(
        SourceState::Unavailable,
        Some("simulated Source publication gap after a known returned selection".into()),
    );
    h.send(Input::SourceHealth(unavailable));
    for _ in 0..4 {
        h.step();
    }
    assert_eq!(h.inputs().len(), 2, "{}", h.report());
    assert!(h.effects("menu.selected_value@1").is_empty());
    assert!(h.effects("goal.required@1").is_empty());
    assert_known_selection_wait_accounting(&h);
    let available = h.ax_health(SourceState::Available, None);
    h.send(Input::SourceHealth(available));
    assert!(
        h.effects("menu.selected_value@1").is_empty(),
        "a recovery status does not make the old snapshot fresh"
    );
    (h, choosing, receipt)
}

#[test]
fn known_completed_selection_survives_same_watch_gap_as_readonly_verification() {
    let (mut h, choosing, receipt) = known_selection_then_source_gap();
    let frozen: String = h
        .engine
        .storage()
        .read()
        .query_row("SELECT goal_json FROM root_tasks", [], |r| r.get(0))
        .unwrap();
    let Some(MenuBindingScope::Item { context, opened_by }) = choosing
        .intent
        .binding_proof
        .as_ref()
        .unwrap()
        .menu_scope
        .as_ref()
    else {
        unreachable!()
    };
    let original_watch = format!("watch-{}", h.watch_generation);
    assert_ne!(
        original_watch, context.source_session_id,
        "the known result read is separate from input authority"
    );
    let original_deadline = opened_by.attempt_deadline_ms;
    let restored_at = h.now;
    h.source_up = true;
    h.until(|h| !h.effects("menu.selected_value@1").is_empty());
    let selection = h.effects("menu.selected_value@1");
    assert_eq!(selection.len(), 1);
    assert_eq!(selection[0]["confirmation_scope"], "current_ui");
    assert_eq!(selection[0]["intent_id"], choosing.intent.intent_id);
    assert_eq!(
        selection[0]["attempt_id"],
        choosing.intent.attempt_id.clone().unwrap()
    );
    assert_eq!(
        selection[0]["input_span"]["to_ms"],
        receipt.input_end_ms.unwrap()
    );
    let observed = selection[0]["observed_ms"].as_u64().unwrap();
    assert!(observed > restored_at && observed > receipt.input_end_ms.unwrap());
    assert!(observed < original_deadline);
    let result_source = h
        .snapshots
        .iter()
        .find(|snapshot| snapshot.observed_ms == observed)
        .unwrap();
    assert!(result_source.menu_context.is_none());
    assert_eq!(
        result_source.menu_anchors[0].source_session_id,
        original_watch
    );
    assert_known_selection_source_accounting(&h);
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
        4,
        "one opening, one selection and two revisit inputs"
    );
    assert_eq!(
        inputs[1].intent, choosing.intent,
        "recovery keeps the actual original selection intent"
    );
    assert_eq!(
        inputs
            .iter()
            .filter(|input| input.purpose == ObservationPurpose::MenuSelection)
            .count(),
        1
    );
    assert_eq!(
        h.count("SELECT COUNT(*) FROM attempts WHERE skill = 'ui.select_by_label'"),
        1
    );
    assert_eq!(
        h.count("SELECT deadline_ms FROM attempts WHERE skill = 'ui.select_by_label'"),
        original_deadline as i64
    );
    let root = h.effects("goal.required@1");
    assert_eq!(root[0]["confirmation_scope"], "ui_revisit");
    assert!(root[0]["observed_ms"].as_u64().unwrap() > observed);
    let still_frozen: String = h
        .engine
        .storage()
        .read()
        .query_row("SELECT goal_json FROM root_tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(frozen, still_frozen);
    assert!(still_frozen.contains("revisited"));
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM event_log WHERE kind = 'root_state' AND detail LIKE '%Completed'"
        ),
        1
    );
}

#[test]
fn known_selection_accepts_retired_anchor_or_moved_opener_after_watch_restart() {
    for change in [ResultChange::AnchorRetired, ResultChange::OpenerMoved] {
        let (mut h, choosing, receipt) = known_selection_then_source_gap();
        let frozen: String = h
            .engine
            .storage()
            .read()
            .query_row("SELECT goal_json FROM root_tasks", [], |r| r.get(0))
            .unwrap();
        let Some(MenuBindingScope::Item { context, opened_by }) = choosing
            .intent
            .binding_proof
            .as_ref()
            .unwrap()
            .menu_scope
            .as_ref()
        else {
            unreachable!()
        };
        let original_anchor = context.opener.clone();
        let original_deadline = opened_by.attempt_deadline_ms;
        h.watch_generation += 1;
        h.result_change = change;
        h.source_up = true;
        h.until(|h| !h.effects("menu.selected_value@1").is_empty());
        let selected = h.effects("menu.selected_value@1");
        let observed = selected[0]["observed_ms"].as_u64().unwrap();
        assert_known_selection_source_accounting(&h);
        let snapshot = h
            .snapshots
            .iter()
            .find(|s| s.observed_ms == observed)
            .unwrap();
        let anchor = &snapshot.menu_anchors[0];
        assert_ne!(anchor.source_session_id, original_anchor.source_session_id);
        assert_eq!(anchor.process, original_anchor.process);
        assert_eq!(anchor.origin_window_id, original_anchor.origin_window_id);
        assert_eq!(
            anchor.origin_window_frame,
            original_anchor.origin_window_frame
        );
        match change {
            ResultChange::AnchorRetired => {
                assert_ne!(anchor.anchor_id, original_anchor.anchor_id);
                assert_ne!(anchor.anchor_revision, original_anchor.anchor_revision);
            }
            ResultChange::OpenerMoved => {
                assert_ne!(anchor.opener_frame, original_anchor.opener_frame)
            }
            _ => unreachable!(),
        }
        assert!(observed > receipt.input_end_ms.unwrap() && observed < original_deadline);
        assert_eq!(selected[0]["intent_id"], choosing.intent.intent_id);
        assert_eq!(
            h.inputs()
                .iter()
                .filter(|input| input.purpose == ObservationPurpose::MenuSelection)
                .count(),
            1,
            "readonly result must not repeat selection"
        );
        assert!(h.effects("goal.required@1").is_empty());
        h.until(|h| h.engine.last_finished().is_some());
        assert_eq!(
            h.engine.last_finished(),
            Some(RootState::Completed),
            "{}",
            h.report()
        );
        assert_eq!(h.inputs().len(), 4);
        assert_eq!(h.inputs()[1].intent, choosing.intent);
        assert_eq!(
            h.core.query_receipt(1, &choosing.intent.intent_id).unwrap(),
            receipt
        );
        assert_eq!(
            h.count("SELECT COUNT(*) FROM attempts WHERE skill = 'ui.select_by_label'"),
            1
        );
        assert_eq!(
            h.count("SELECT deadline_ms FROM attempts WHERE skill = 'ui.select_by_label'"),
            original_deadline as i64
        );
        assert_eq!(h.count("SELECT COUNT(*) FROM event_log WHERE kind = 'root_state' AND detail LIKE '%Completed'"), 1);
        assert_eq!(
            h.effects("goal.required@1")[0]["confirmation_scope"],
            "ui_revisit"
        );
        let after: String = h
            .engine
            .storage()
            .read()
            .query_row("SELECT goal_json FROM root_tasks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(frozen, after);
    }
}

#[test]
fn known_selection_rejects_new_source_with_changed_authorized_result_scope() {
    for change in [
        ResultChange::KernelChanged,
        ResultChange::PidChanged,
        ResultChange::AppChanged,
        ResultChange::WindowChanged,
        ResultChange::RootFrameChanged,
        ResultChange::EpochChanged,
    ] {
        let (mut h, choosing, receipt) = known_selection_then_source_gap();
        h.watch_generation += 1;
        h.result_change = change;
        h.source_up = true;
        // Wrong epoch can remain unavailable until the immutable deadline;
        // the other scope mismatches fail as soon as fresh Source is read.
        for _ in 0..400 {
            h.step();
            if h.engine.last_finished().is_some()
                || h.engine.root_state() == Some(RootState::Suspended)
            {
                break;
            }
        }
        for _ in 0..8 {
            h.step();
        }
        assert_eq!(h.inputs().len(), 2, "{change:?}: {}", h.report());
        assert_eq!(h.inputs()[1].intent, choosing.intent);
        assert_eq!(
            h.core.query_receipt(1, &choosing.intent.intent_id).unwrap(),
            receipt
        );
        assert!(h.effects("menu.selected_value@1").is_empty(), "{change:?}");
        assert!(h.effects("goal.required@1").is_empty(), "{change:?}");
        assert_eq!(
            h.readonly_result_reads, 1,
            "{change:?} was rejected from one post-input closed-menu Source read"
        );
        assert_ne!(h.engine.last_finished(), Some(RootState::Completed));
        assert_eq!(
            h.count("SELECT COUNT(*) FROM attempts WHERE skill = 'ui.select_by_label'"),
            1
        );
    }
}

#[test]
fn unknown_show_never_selects_even_when_the_actual_menu_is_visible() {
    let mut h = Harness::new(Fault::UnknownShow);
    h.start();
    h.until(|h| h.engine.root_state() == Some(RootState::Suspended));
    for _ in 0..20 {
        h.step();
    }
    assert!(h.app.borrow().open);
    assert!(
        h.app
            .borrow()
            .snapshot
            .as_ref()
            .unwrap()
            .menu_context
            .is_some()
    );
    assert_eq!(h.inputs().len(), 1, "{}", h.report());
    assert!(h.core.is_isolated());
    assert!(h.effects("menu.selected_value@1").is_empty());
}

#[test]
fn unknown_selection_never_replays_or_automatically_completes_the_root() {
    let mut h = Harness::new(Fault::UnknownPress);
    h.start();
    h.until(|h| h.inputs().len() >= 2);
    let choosing = h.inputs()[1].clone();
    h.until(|h| h.core.is_isolated());
    let actual = h.core.query_receipt(1, &choosing.intent.intent_id).unwrap();
    assert_eq!(actual.status, ReceiptStatus::Unknown);
    // A conflicting late device notification cannot downgrade the already
    // consumed Unknown. The real Core ledger remains Unknown and isolated.
    let mut late = actual.clone();
    late.status = ReceiptStatus::Completed;
    h.send(Input::DeviceNotice(DeviceNotice::Receipt(Box::new(late))));
    for _ in 0..40 {
        h.step();
    }
    assert_eq!(
        h.core.query_receipt(1, &choosing.intent.intent_id).unwrap(),
        actual
    );
    assert_eq!(h.inputs().len(), 2, "{}", h.report());
    assert!(h.core.is_isolated());
    assert_eq!(
        h.app.borrow().value,
        "Second",
        "the actual effect remains Unknown mechanically"
    );
    assert!(h.effects("menu.selected_value@1").is_empty());
    assert!(h.effects("goal.required@1").is_empty());
    assert_eq!(
        h.count("SELECT COUNT(*) FROM attempts WHERE skill = 'ui.revisit'"),
        0
    );
    assert_ne!(h.engine.last_finished(), Some(RootState::Completed));
    assert_eq!(
        h.count("SELECT COUNT(*) FROM attempts WHERE skill = 'ui.select_by_label'"),
        1
    );
}

#[test]
fn disabled_duplicate_or_replaced_anchor_watch_cannot_receive_choice_input() {
    for fault in [
        Fault::Disabled,
        Fault::Duplicate,
        Fault::AnchorReplaced,
        Fault::WatchReplaced,
    ] {
        let mut h = Harness::new(fault);
        h.start();
        h.until(|h| {
            h.engine.last_finished().is_some()
                || h.engine.root_state() == Some(RootState::Suspended)
        });
        for _ in 0..10 {
            h.step();
        }
        assert_eq!(h.inputs().len(), 1, "{fault:?}: {}", h.report());
        assert_eq!(
            h.core
                .query_receipt(1, &h.inputs()[0].intent.intent_id)
                .unwrap()
                .status,
            ReceiptStatus::Completed
        );
        assert!(h.effects("menu.selected_value@1").is_empty());
        assert_ne!(h.engine.last_finished(), Some(RootState::Completed));
    }
}

#[test]
fn replaced_menu_context_invalidates_an_already_issued_item_permit() {
    let mut h = Harness::new(Fault::None);
    h.hold_item_grant = true;
    h.start();
    h.until(|h| h.held_grant.is_some());
    assert_eq!(h.inputs().len(), 1);
    assert!(matches!(
        h.held_grant.as_ref().unwrap().1,
        DeviceReply::Grant(GrantOutcome::Granted { .. })
    ));
    h.app.borrow_mut().menu_generation += 1;
    h.until(|h| {
        h.engine.last_finished().is_some() || h.engine.root_state() == Some(RootState::Suspended)
    });
    let (corr, reply) = h.held_grant.take().unwrap();
    h.send(Input::Device { corr, reply });
    for _ in 0..20 {
        h.step();
    }
    assert_eq!(h.inputs().len(), 1, "{}", h.report());
    assert!(h.effects("menu.selected_value@1").is_empty());
    assert_eq!(
        h.count("SELECT COUNT(*) FROM attempts WHERE skill = 'ui.select_by_label'"),
        1
    );
}

#[test]
fn unregistered_or_destructive_choice_semantics_never_authorize_input() {
    for class in [EffectClass::Unknown, EffectClass::Destructive] {
        let mut h = Harness::with_class(Fault::None, class);
        h.send(Input::Operator(OperatorCommand::CreateTask {
            text: "Set Locale to Second".into(),
        }));
        h.until(|h| h.engine.last_finished().is_some());
        assert_eq!(h.engine.last_finished(), Some(RootState::Failed));
        assert!(h.inputs().is_empty());
        assert_eq!(h.grants, 0);
    }
}

#[test]
fn a_closed_menu_with_an_unchanged_value_is_never_selection_success() {
    let mut h = Harness::new(Fault::NoEffect);
    h.start();
    h.until(|h| h.inputs().len() >= 2);
    let choosing = h.inputs()[1].clone();
    for _ in 0..40 {
        h.step();
    }
    assert_eq!(
        h.core
            .query_receipt(1, &choosing.intent.intent_id)
            .unwrap()
            .status,
        ReceiptStatus::Completed
    );
    assert_eq!(h.inputs().len(), 2, "{}", h.report());
    assert_eq!(h.app.borrow().value, "First");
    assert!(!h.app.borrow().open);
    assert!(h.effects("menu.selected_value@1").is_empty());
    assert_ne!(h.engine.last_finished(), Some(RootState::Completed));
}

#[test]
fn ordinary_background_binding_cannot_build_input_from_actual_open_menu_source() {
    use interaction_engine::environment::{binding::BindingReducer, state_core::StateCore};
    use interaction_engine::execution::DeviceView;
    let mut h = Harness::new(Fault::None);
    h.hold_item_grant = true;
    h.start();
    h.until(|h| h.held_grant.is_some());
    let snapshot = h.app.borrow().snapshot.clone().unwrap();
    assert!(snapshot.menu_context.is_some());
    let initial = h.snapshots[0].clone();
    let initial_opener = initial.menu_anchors[0].opener_instance.clone();
    let mut state = StateCore::new();
    state.admit(initial, 0, &h.pack).unwrap();
    let mut bindings = BindingReducer::new();
    let ordinary_binding = bindings
        .bind_instance(
            state.latest().unwrap(),
            SemanticTargetRef::new("locale").unwrap(),
            &initial_opener,
        )
        .unwrap();
    let opener = snapshot.menu_anchors[0].opener_instance.clone();
    state.admit(snapshot, h.now, &h.pack).unwrap();
    assert!(!state.usable(h.now, 1000));
    assert!(state.menu_usable(h.now, 1000));
    let env = state.latest().unwrap();
    assert!(
        BindingReducer::new()
            .bind_instance(env, SemanticTargetRef::new("locale").unwrap(), &opener)
            .is_none()
    );
    let mut device = DeviceView::new();
    device.install_manifest(h.manifest());
    assert_eq!(
        device
            .bound_observation(
                env,
                &ordinary_binding,
                SourceFreshness::Verified,
                500,
                h.now + 1000,
                None
            )
            .unwrap_err(),
        "binding proof does not belong to the current menu input domain"
    );
    assert_eq!(h.inputs().len(), 1);
}
