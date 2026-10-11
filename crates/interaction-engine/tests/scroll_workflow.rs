//! Test-only virtual list. Its source and input backend model scrolling while
//! the real Engine, TaskRuntime and ExecutionCore own admission and receipts.

mod common;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use execution_core::backend::{
    HeldState, HitTestOutcome, HitTestRequest, InputBackend, ReleaseFailure, SubmitContext,
    SubmitOutcome,
};
use execution_core::core::{CoreConfig, CoreEvent, ExecutionCore};
use interaction_contracts::desktop::binding::instance_fingerprint;
use interaction_contracts::desktop::device::{
    BackendKind, CapabilityManifest, CapabilityState, CommandAck, GrantOutcome, PermitRef,
};
use interaction_contracts::desktop::domain::ExecutionDomain;
use interaction_contracts::desktop::effect::EffectClass;
use interaction_contracts::desktop::ids::{ObjectRef, SemanticTargetRef};
use interaction_contracts::desktop::operator::{OperatorCommand, RunMode, UiProjection};
use interaction_contracts::desktop::planning::{
    Dependency, GoalDraft, PlanBundle, PlanProposal, PlannerOutput, PlanningReason,
    ScrollDirection, ScrollToSpec, Subgoal,
};
use interaction_contracts::desktop::primitive::{
    Modifiers, MouseButton, Primitive, PrimitivePolicy,
};
use interaction_contracts::desktop::source::{
    AxActionName, FrameMeta, FrameProvenance, FrameStatus, FrameStreamInfo, GlobalFocusObservation,
    ObjectIdentityContinuity, ObservationFact, Rect, ScrollAxisState, ScrollState,
    StructureSnapshot, UiObject, UiValue,
};
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

/// Pair each synthetic foreground Source tree with a complete frame from its
/// exact globally focused target window.
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
        frame_id: format!("scroll-image-{}", snapshot.source_sequence),
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
        checksum: format!("sha256:scroll-image-{}", snapshot.source_sequence),
        provenance: Some(FrameProvenance::Stream {
            stream: FrameStreamInfo {
                stream_id: "scroll-workflow-screen".into(),
                image_frame_id: Some(format!("scroll-image-{}", snapshot.source_sequence)),
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

fn target(id: &str, label: &str, role: &str) -> TargetSpec {
    TargetSpec {
        row: None,
        semantic_target: SemanticTargetRef::new(id).unwrap(),
        label: label.into(),
        group: None,
        role_hint: Some(role.into()),
    }
}

fn navigation_click_at(x: i32, y: i32) -> Primitive {
    Primitive::Click {
        button: MouseButton::Left,
        x,
        y,
        modifiers: Modifiers::default(),
    }
}

fn proposal() -> PlanProposal {
    let destination = target("destination", "Wanted row", "switch");
    PlanProposal {
        goal_draft: Some(GoalDraft {
            app: "test.settings".into(),
            account: None,
            target: destination.clone(),
            desired: ExpectedValue::Bool { value: true },
            confirmation_scope: ConfirmationScope::UiRevisit,
            max_effect_class: EffectClass::Navigation,
        }),
        bundle: PlanBundle {
            subgoals: vec![Subgoal {
                select: None,
                id: "find-row".into(),
                description: "Find the wanted row".into(),
                skill: "ui.scroll_to".into(),
                target: Some(destination.semantic_target),
                expects: vec!["visible and freshly bound inside List".into()],
                scroll: Some(ScrollToSpec {
                    container: target("list", "List", "AXScrollArea"),
                    direction: ScrollDirection::Down,
                    pixels: 40,
                    max_actions: 3,
                }),
            }],
            dependencies: vec![],
            required_observations: vec![],
            expected_effects: vec![],
            recovery_branches: vec![],
            termination: vec!["the frozen root predicates hold".into()],
        },
    }
}

#[derive(Clone, Debug)]
struct Submission {
    permit: PermitRef,
    instance: ObjectRef,
    attempt: String,
    intent: String,
    primitive: Primitive,
    at_ms: u64,
}

#[derive(Default)]
struct List {
    submissions: Vec<Submission>,
    snapshot: Option<StructureSnapshot>,
    reveal_after: Option<usize>,
    outside_duplicate: bool,
    unknown: bool,
    range_unknown: bool,
    navigation: bool,
    other_page: bool,
}

struct ListBackend(Rc<RefCell<List>>);

impl InputBackend for ListBackend {
    fn submit(
        &mut self,
        at_ms: u64,
        primitive: &Primitive,
        context: &SubmitContext<'_>,
    ) -> SubmitOutcome {
        let mut list = self.0.borrow_mut();
        match primitive {
            Primitive::Scroll { dx: 0, dy: -40 } => {}
            Primitive::Click {
                button: MouseButton::Left,
                x,
                y,
                modifiers,
            } if list.navigation => {
                let object = list
                    .snapshot
                    .as_ref()
                    .unwrap()
                    .objects
                    .iter()
                    .find(|object| {
                        object.instance_ref
                            == context.intent.binding_proof.as_ref().unwrap().instance
                    })
                    .unwrap();
                assert_eq!(object.role, "AXRadioButton");
                assert_eq!(*modifiers, Modifiers::default());
                let (center_x, center_y) = object.frame.center();
                assert_eq!((*x, *y), (center_x.round() as i32, center_y.round() as i32));
                list.other_page = object.title.as_deref() == Some("Other");
            }
            _ => panic!(
                "unexpected fixture input: {primitive:?}; semantic Down must use native negative Y"
            ),
        }
        list.submissions.push(Submission {
            permit: context.intent.permit.clone(),
            instance: context
                .intent
                .binding_proof
                .as_ref()
                .unwrap()
                .instance
                .clone(),
            attempt: context.intent.attempt_id.clone().unwrap(),
            intent: context.intent.intent_id.clone(),
            primitive: primitive.clone(),
            at_ms,
        });
        if list.unknown {
            SubmitOutcome::Unknown
        } else {
            SubmitOutcome::Applied
        }
    }

    fn release(&mut self, _: u64, _: &Primitive) -> Result<(), ReleaseFailure> {
        Ok(())
    }
    fn held(&self, _: &str) -> HeldState {
        HeldState::NotHeld
    }

    fn hit_test(&mut self, request: &HitTestRequest<'_>) -> HitTestOutcome {
        let list = self.0.borrow();
        let snapshot = list.snapshot.as_ref().unwrap();
        let proof = request.context.intent.binding_proof.as_ref().unwrap();
        match snapshot.objects.iter().find(|o| {
            o.instance_ref == proof.instance
                && (o.role == "AXScrollArea" || list.navigation && o.role == "AXRadioButton")
        }) {
            Some(area) => HitTestOutcome::Element(instance_fingerprint(snapshot, area).digest()),
            None => HitTestOutcome::Unresolved,
        }
    }
}

struct Harness {
    engine: Engine,
    core: ExecutionCore<ListBackend>,
    list: Rc<RefCell<List>>,
    queue: VecDeque<Input>,
    now: u64,
    sequence: u64,
    ui: Option<UiProjection>,
    grants: usize,
    plan: PlanProposal,
}

impl Harness {
    fn new(reveal_after: Option<usize>, outside_duplicate: bool, unknown: bool) -> Self {
        let list = Rc::new(RefCell::new(List {
            reveal_after,
            outside_duplicate,
            unknown,
            ..List::default()
        }));
        let mut pack = AppPack::settings_fixture();
        pack.nav_role = "AXRadioButton".into();
        pack.rules.push(EffectRule {
            role: Some("AXRadioButton".into()),
            label_contains: String::new(),
            class: EffectClass::Navigation,
        });
        pack.rules.push(EffectRule {
            role: Some("AXScrollArea".into()),
            label_contains: "list".into(),
            class: EffectClass::Navigation,
        });
        let mut config = EngineConfig::new(TaskConfig::new(vec![pack]));
        config.initial_mode = RunMode::Execute;
        let mut core = ExecutionCore::new(
            CoreConfig::production("scroll-test"),
            ListBackend(list.clone()),
        );
        core.renew_lease(60_000);
        let mut h = Self {
            engine: Engine::new(config, Storage::open_in_memory().unwrap()),
            core,
            list,
            queue: VecDeque::new(),
            now: 0,
            sequence: 0,
            ui: None,
            grants: 0,
            plan: proposal(),
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
        let identity = self.core.identity();
        let mut snapshot = SimApp::new().snapshot(&SnapshotMeta {
            snapshot_id: format!("list-read-{}", self.sequence),
            session_epoch: identity.session_epoch,
            surface: identity.surface,
            clock_domain: identity.clock_domain,
            source_sequence: self.sequence,
            observed_ms: self.now,
        });
        let source_window_id = snapshot
            .global_focus
            .known_window_for_app(&snapshot.app)
            .expect("the simulated app reports its focused window")
            .to_owned();
        snapshot.global_focus = GlobalFocusObservation {
            app: ObservationFact::Known {
                value: snapshot.app.clone(),
            },
            window_id: ObservationFact::Known {
                value: source_window_id.clone(),
            },
        };
        let object =
            |id: String, role: &str, title: &str, parent: Option<ObjectRef>, x: f64| UiObject {
                instance_ref: ObjectRef::new(id).unwrap(),
                parent,
                window_id: source_window_id.clone(),
                role: role.into(),
                subrole: None,
                title: Some(title.into()),
                description: None,
                value: Some(UiValue::Bool(true)),
                enabled: true,
                focused: false,
                frame: Rect {
                    x,
                    y: 40.0,
                    width: 180.0,
                    height: 220.0,
                },
                actions: vec![],
                ax_action_names: None,
                in_view: true,
                group: None,
                secure: false,
                scroll: None,
                identity_continuity: ObjectIdentityContinuity::ReadScoped,
            };
        let window_root = snapshot
            .objects
            .iter()
            .find(|object| object.role == "AXWindow" && object.parent.is_none())
            .unwrap()
            .clone();
        // IDs change every read; the same virtual row slot changes its label.
        let container_id = ObjectRef::new(format!("list-instance-{}", self.sequence)).unwrap();
        let mut area = object(
            container_id.as_str().into(),
            "AXScrollArea",
            "List",
            Some(window_root.instance_ref.clone()),
            10.0,
        );
        let list = self.list.borrow();
        let count = list
            .submissions
            .iter()
            .filter(|input| matches!(input.primitive, Primitive::Scroll { .. }))
            .count();
        area.scroll = Some(ScrollState {
            horizontal: None,
            vertical: Some(ScrollAxisState {
                value: (count * 40) as f64,
                min: if list.range_unknown { None } else { Some(0.0) },
                max: if list.range_unknown {
                    None
                } else {
                    Some(120.0)
                },
            }),
        });
        let title = if list.reveal_after.is_some_and(|after| count >= after) {
            "Wanted row"
        } else {
            "Other row"
        };
        let row = object(
            format!("recycled-row-{}", self.sequence),
            "switch",
            title,
            Some(container_id),
            10.0,
        );
        snapshot.objects = vec![window_root.clone(), area, row];
        if list.outside_duplicate {
            snapshot.objects.push(object(
                "outside-row".into(),
                "switch",
                "Wanted row",
                Some(window_root.instance_ref.clone()),
                300.0,
            ));
        }
        if list.navigation {
            if list.other_page {
                // Other has no destination or container. Offset/value remain
                // owned by the model and reappear unchanged on return to Doc.
                snapshot.objects.clear();
                snapshot.objects.push(window_root.clone());
            }
            for (label, selected, x) in [
                ("Doc", !list.other_page, 210.0),
                ("Other", list.other_page, 410.0),
            ] {
                let mut tab = object(
                    format!("nav-{label}-{}", self.sequence),
                    "AXRadioButton",
                    label,
                    Some(window_root.instance_ref.clone()),
                    x,
                );
                tab.actions = vec![AxActionName::Press];
                tab.value = Some(UiValue::Bool(selected));
                snapshot.objects.push(tab);
            }
            snapshot.windows[0].title = Some(format!(
                "Settings — {}",
                if list.other_page { "Other" } else { "Doc" }
            ));
        }
        drop(list);
        sim_world::app::refresh_simulated_observer_coverage(&mut snapshot);
        self.list.borrow_mut().snapshot = Some(snapshot.clone());
        let frame = frame_for_snapshot(&snapshot);
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
            assert!(deliveries < 10_000, "engine did not settle");
            for output in self.engine.handle(self.now, input) {
                match output {
                    Output::Ui(ui) => self.ui = Some(*ui),
                    Output::Source { generation, .. } => {
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
                                proposal: Box::new(self.plan.clone()),
                            }),
                        });
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
        let ack = |result: Result<(), String>| {
            DeviceReply::Ack(match result {
                Ok(()) => CommandAck::ok(),
                Err(reason) => CommandAck::rejected(None, reason),
            })
        };
        let reply = match command {
            DeviceCommand::RegisterRoot(grant) => ack(self.core.register_root(&grant)),
            DeviceCommand::Observe(observation) => ack(self.core.observe(observation)),
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
                DeviceReply::Dependencies(
                    interaction_contracts::desktop::device::DependencyVersions {
                        versions: self.core.dependency_versions(),
                    },
                )
            }
        };
        self.queue.push_back(Input::Device { corr, reply });
        self.pump();
    }

    fn pump(&mut self) {
        for event in self.core.drain_events() {
            if let CoreEvent::ReceiptChanged(receipt) = event {
                self.queue
                    .push_back(Input::DeviceNotice(DeviceNotice::Receipt(Box::new(
                        receipt,
                    ))));
            }
        }
    }

    fn step(&mut self) {
        self.now += 50;
        self.core.advance_to(self.now).unwrap();
        self.core.dispatch_next();
        self.pump();
        // A complete AX read arrives after the atomic command's bounded
        // receipt interval, avoiding an unrelated mid-command invalidation.
        if self.now.is_multiple_of(100) {
            self.source();
        }
        self.send(Input::Tick);
    }

    fn until(&mut self, condition: impl Fn(&Self) -> bool) {
        let limit = self.now + 5_000;
        while !condition(self) {
            assert!(self.now < limit, "timed out: {}", self.report());
            self.step();
        }
    }

    fn start(&mut self) {
        self.send(Input::Operator(OperatorCommand::CreateTask {
            text: "Find Wanted row inside List".into(),
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

    fn navigation_results(&self) -> Vec<serde_json::Value> {
        self.engine.storage().read().prepare("SELECT json FROM effect_results WHERE json_extract(json, '$.predicate') = 'scroll.target_visible_bound@1'").unwrap()
            .query_map([], |r| r.get::<_, String>(0)).unwrap().map(|r| serde_json::from_str(&r.unwrap()).unwrap()).collect()
    }

    fn report(&self) -> String {
        format!(
            "state {:?}; finished {:?}; fault {:?}; timeline {:?}; inputs {:?}",
            self.engine.root_state(),
            self.engine.last_finished(),
            self.engine.faulted(),
            self.engine.timeline().map(|n| &n.text).collect::<Vec<_>>(),
            self.list.borrow().submissions
        )
    }
}

#[test]
fn destination_is_bound_after_each_fresh_scroll_without_weakening_the_root() {
    let mut h = Harness::new(Some(2), true, false);
    h.start();
    let frozen: String = h
        .engine
        .storage()
        .read()
        .query_row("SELECT goal_json FROM root_tasks", [], |r| r.get(0))
        .unwrap();
    h.until(|h| !h.navigation_results().is_empty());
    let list = h.list.borrow();
    assert_eq!(list.submissions.len(), 2, "{}", h.report());
    assert_eq!(h.grants, 2);
    let [first, second] = list.submissions.as_slice() else {
        unreachable!()
    };
    assert_ne!(first.permit, second.permit);
    assert_ne!(first.instance, second.instance);
    assert_ne!(first.intent, second.intent);
    assert_eq!(first.attempt, second.attempt);
    assert_eq!(second.primitive, Primitive::Scroll { dx: 0, dy: -40 });
    drop(list);
    assert_eq!(
        h.count("SELECT COUNT(*) FROM attempts WHERE skill = 'ui.scroll_to'"),
        1
    );
    let result = &h.navigation_results()[0];
    assert_eq!(result["target"], "destination");
    assert_eq!(result["confirmation_scope"], "current_ui");
    assert_eq!(result["status"], "satisfied");
    assert!(!result["evidence_group"].as_array().unwrap().is_empty());
    assert!(
        result["input_span"]["to_ms"].as_u64().unwrap() <= result["observed_ms"].as_u64().unwrap()
    );
    let still_frozen: String = h
        .engine
        .storage()
        .read()
        .query_row("SELECT goal_json FROM root_tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(frozen, still_frozen);
    assert!(frozen.contains("revisited"));
    assert_ne!(h.engine.last_finished(), Some(RootState::Completed));
    assert_eq!(
        h.count("SELECT COUNT(*) FROM root_tasks WHERE state = 'completed'"),
        0
    );
}

#[test]
fn initially_hidden_root_target_completes_only_after_fresh_binding_on_return() {
    let mut h = Harness::new(Some(2), false, false);
    h.list.borrow_mut().navigation = true;
    h.plan.goal_draft.as_mut().unwrap().max_effect_class = EffectClass::ReversibleEdit;
    for (id, skill, before) in [
        ("ensure", "ui.ensure_value", "find-row"),
        ("revisit", "ui.revisit", "ensure"),
    ] {
        h.plan.bundle.subgoals.push(Subgoal {
            select: None,
            id: id.into(),
            description: id.into(),
            skill: skill.into(),
            target: Some(SemanticTargetRef::new("destination").unwrap()),
            expects: vec!["the frozen destination predicate is verified".into()],
            scroll: None,
        });
        h.plan.bundle.dependencies.push(Dependency {
            before: before.into(),
            after: id.into(),
        });
    }
    h.start();
    let frozen: String = h
        .engine
        .storage()
        .read()
        .query_row("SELECT goal_json FROM root_tasks", [], |r| r.get(0))
        .unwrap();
    h.until(|h| h.list.borrow().submissions.len() >= 3);
    let scroll_evidence = h.navigation_results();
    assert_eq!(scroll_evidence.len(), 1, "{}", h.report());
    assert!(h.list.borrow().other_page);
    assert_ne!(h.engine.last_finished(), Some(RootState::Completed));
    h.until(|h| h.list.borrow().submissions.len() >= 4);
    let returned_at = h.list.borrow().submissions[3].at_ms;
    assert!(!h.list.borrow().other_page);
    // A known navigation receipt has not yet yielded fresh return evidence.
    assert_ne!(h.engine.last_finished(), Some(RootState::Completed));
    h.until(|h| h.engine.last_finished().is_some());
    assert_eq!(
        h.engine.last_finished(),
        Some(RootState::Completed),
        "{}",
        h.report()
    );
    let list = h.list.borrow();
    assert_eq!(list.submissions.len(), 4, "{}", h.report());
    assert!(
        list.submissions[..2]
            .iter()
            .all(|input| matches!(input.primitive, Primitive::Scroll { .. }))
    );
    assert_eq!(list.submissions[2].primitive, navigation_click_at(500, 150));
    assert_eq!(list.submissions[3].primitive, navigation_click_at(300, 150));
    assert_eq!(
        list.submissions[2..]
            .iter()
            .filter(|input| matches!(input.primitive, Primitive::Click { .. }))
            .count(),
        2,
        "only registered page navigation uses pointer clicks"
    );
    let snapshot = list.snapshot.as_ref().unwrap();
    assert!(snapshot.observed_ms > returned_at);
    assert!(snapshot.observed_ms > scroll_evidence[0]["observed_ms"].as_u64().unwrap());
    assert!(
        snapshot
            .objects
            .iter()
            .any(|object| object.title.as_deref() == Some("Wanted row") && object.parent.is_some())
    );
    let returned_snapshot = snapshot.snapshot_id.as_str().to_owned();
    drop(list);
    let proof_json: String = h.engine.storage().read().query_row(
        "SELECT json FROM effect_results WHERE json_extract(json, '$.predicate') = 'goal.required@1' AND json_extract(json, '$.status') = 'satisfied'", [], |r| r.get(0)
    ).unwrap();
    let proof: serde_json::Value = serde_json::from_str(&proof_json).unwrap();
    assert_eq!(proof["confirmation_scope"], "ui_revisit");
    assert!(
        proof["evidence_group"]
            .as_array()
            .unwrap()
            .iter()
            .any(|id| id == &returned_snapshot)
    );
    assert_eq!(h.grants, 4);
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM attempts WHERE skill = 'ui.ensure_value' AND intent_id IS NULL"
        ),
        1
    );
    assert_eq!(
        h.count("SELECT COUNT(*) FROM attempts WHERE skill = 'ui.revisit'"),
        1
    );
    assert!(
        h.report()
            .contains("ui.ensure_value found the value already right")
    );
    let after: String = h
        .engine
        .storage()
        .read()
        .query_row("SELECT goal_json FROM root_tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(frozen, after);
    assert!(after.contains("revisited"));
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM event_log WHERE kind = 'root_state' AND detail LIKE '%Completed'"
        ),
        1
    );
}

#[test]
fn offset_progress_and_an_outside_duplicate_end_absent_once_without_navigation_proof() {
    let mut h = Harness::new(None, true, false);
    h.start();
    h.until(|h| h.engine.last_finished().is_some());
    for _ in 0..20 {
        h.step();
    }
    assert_eq!(h.list.borrow().submissions.len(), 3, "{}", h.report());
    assert_eq!(h.grants, 3);
    assert!(h.navigation_results().is_empty());
    assert_eq!(h.engine.last_finished(), Some(RootState::Failed));
    assert_eq!(
        h.count("SELECT COUNT(*) FROM attempts WHERE skill = 'ui.scroll_to'"),
        1
    );
    assert!(
        h.report().contains("search_complete=false"),
        "{}",
        h.report()
    );
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM event_log WHERE kind = 'root_state' AND detail LIKE '%Failed'"
        ),
        1
    );
}

#[test]
fn unknown_receipt_cannot_authorize_another_scroll_or_replay() {
    let mut h = Harness::new(None, false, true);
    h.start();
    h.until(|h| !h.list.borrow().submissions.is_empty());
    for _ in 0..80 {
        h.step();
    }
    assert_eq!(h.list.borrow().submissions.len(), 1, "{}", h.report());
    assert_eq!(h.grants, 1);
    assert!(h.navigation_results().is_empty());
    assert_eq!(h.engine.root_state(), Some(RootState::Suspended));
    assert_ne!(h.engine.last_finished(), Some(RootState::Completed));
    assert_eq!(
        h.count("SELECT COUNT(*) FROM attempts WHERE skill = 'ui.scroll_to'"),
        1
    );
}

#[test]
fn offsets_without_source_range_cannot_prove_a_boundary_or_complete_the_root() {
    let mut h = Harness::new(None, true, false);
    // Native scrollers can expose only AXValue. The test source must preserve
    // unknown bounds instead of manufacturing a normalized range.
    h.list.borrow_mut().range_unknown = true;
    h.start();
    h.until(|h| {
        h.report()
            .contains("the scroll search exhausted its action quota")
    });
    for _ in 0..20 {
        h.step();
    }
    assert_eq!(h.list.borrow().submissions.len(), 3, "{}", h.report());
    assert_eq!(h.grants, 3);
    assert!(h.navigation_results().is_empty());
    assert!(!h.report().contains("destination not found"));
    assert!(!h.report().contains("reached its boundary"));
    assert_ne!(h.engine.last_finished(), Some(RootState::Completed));
    assert_eq!(
        h.count("SELECT COUNT(*) FROM root_tasks WHERE state = 'completed'"),
        0
    );
    assert_eq!(
        h.count("SELECT COUNT(*) FROM attempts WHERE skill = 'ui.scroll_to'"),
        1
    );
}

#[test]
fn typed_scroll_profiles_refuse_unbounded_or_misdirected_plans() {
    let good = proposal().bundle;
    assert!(good.validate().is_ok());
    for (pixels, max_actions) in [(1, 1), (120, 16)] {
        let mut bounded = good.clone();
        let scroll = bounded.subgoals[0].scroll.as_mut().unwrap();
        scroll.pixels = pixels;
        scroll.max_actions = max_actions;
        assert!(bounded.validate().is_ok());
    }
    for (pixels, max_actions) in [(0, 1), (121, 1), (40, 0), (40, 17)] {
        let mut bad = good.clone();
        let scroll = bad.subgoals[0].scroll.as_mut().unwrap();
        scroll.pixels = pixels;
        scroll.max_actions = max_actions;
        assert!(
            bad.validate().is_err(),
            "accepted pixels={pixels}, actions={max_actions}"
        );
    }
    let mut bad = good.clone();
    bad.subgoals[0].target = Some(SemanticTargetRef::new("list").unwrap());
    assert!(bad.validate().is_err());
    bad = good.clone();
    bad.subgoals[0].scroll = None;
    assert!(bad.validate().is_err());
    bad = good.clone();
    bad.subgoals[0].skill = "ui.read_value".into();
    assert!(bad.validate().is_err());
    bad = good;
    bad.subgoals[0].scroll.as_mut().unwrap().container.role_hint = Some("switch".into());
    assert!(bad.validate().is_err());
}

#[test]
fn installer_cannot_scroll_to_an_unregistered_destination_outside_the_frozen_goal() {
    use interaction_contracts::desktop::ids::RootTaskId;
    use interaction_engine::tasks::goal::{compile_goal, validate_plan};

    let proposal = proposal();
    let config = TaskConfig::new(vec![AppPack::settings_fixture()]);
    let (goal, pack) = compile_goal(
        &RootTaskId::new("scope-test").unwrap(),
        "Find Wanted row inside List",
        proposal.goal_draft.as_ref().unwrap(),
        ExecutionDomain::ForegroundNative,
        config.default_budget,
        &config.packs,
    )
    .unwrap();
    assert!(validate_plan(&goal, &proposal.bundle, &pack, &config.registered_skills).is_ok());
    let mut outside = proposal.bundle;
    outside.subgoals[0].target = Some(SemanticTargetRef::new("unregistered-row").unwrap());
    assert!(validate_plan(&goal, &outside, &pack, &config.registered_skills).is_err());
}
