//! Native AX identity boundary changes retire old bindings and permits without
//! manufacturing source loss or treating every new snapshot as a reset.

mod common;

use common::world::{TEXT, World, engine_config};
use execution_core::core::PermitState;
use interaction_contracts::desktop::device::{GrantOutcome, ReceiptStatus};
use interaction_contracts::desktop::{
    binding::{BindRequest, BindScope, BindingOutcome, TargetDescription},
    device::SurfaceRef,
    ids::{ObjectRef, RequestId, SemanticTargetRef},
    source::{
        LayerKind, ObjectIdentityContinuity, ObservationFact, Rect, SourceIdentityBoundary,
        SourceState, UiObject, WindowInfo,
    },
    task::RootState,
};
use interaction_engine::{
    engine::{Input, NativeWindowScope, Output},
    environment::{binding::BindingReducer, pack::AppPack, state_core::StateCore},
    execution::{DeviceCommand, DeviceReply},
};
use sim_world::app::{SimApp, SnapshotMeta, refresh_simulated_observer_coverage};
use std::{cell::Cell, rc::Rc};

const NS1: &str = "11111111-1111-4111-8111-111111111111";
const NS2: &str = "22222222-2222-4222-8222-222222222222";

fn boundary(namespace: &str, generation: u64) -> SourceIdentityBoundary {
    SourceIdentityBoundary {
        namespace: namespace.into(),
        generation,
    }
}

fn snapshot(
    app: &SimApp,
    sequence: u64,
    now_ms: u64,
    boundary: Option<SourceIdentityBoundary>,
) -> interaction_contracts::desktop::source::StructureSnapshot {
    let mut snapshot = app.snapshot(&SnapshotMeta {
        snapshot_id: format!("identity-snapshot-{sequence}"),
        session_epoch: 1,
        surface: SurfaceRef {
            id: "main".into(),
            epoch: 1,
        },
        clock_domain: "sim/1/monotonic".into(),
        source_sequence: sequence,
        observed_ms: now_ms,
    });
    snapshot.identity_boundary = boundary;
    snapshot
}

fn bind_request() -> BindRequest {
    BindRequest {
        request_id: RequestId::new("identity-bind-request").unwrap(),
        semantic_target: SemanticTargetRef::new("push/daily-picks").unwrap(),
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
        max_age_ms: 2_000,
    }
}

#[derive(Debug, PartialEq, Eq)]
struct AttemptFacts {
    id: String,
    intent: Option<String>,
    state: String,
    deadline_ms: i64,
    quota_actions: i64,
    quota_observations: i64,
}

fn latest_attempt(world: &World) -> AttemptFacts {
    world
        .engine
        .storage()
        .read()
        .query_row(
            "SELECT attempt_id, intent_id, state, deadline_ms, quota_actions, quota_observations \
             FROM attempts ORDER BY created_ms DESC LIMIT 1",
            [],
            |row| {
                Ok(AttemptFacts {
                    id: row.get(0)?,
                    intent: row.get(1)?,
                    state: row.get(2)?,
                    deadline_ms: row.get(3)?,
                    quota_actions: row.get(4)?,
                    quota_observations: row.get(5)?,
                })
            },
        )
        .unwrap()
}

fn attempt_by_id(world: &World, id: &str) -> AttemptFacts {
    world
        .engine
        .storage()
        .read()
        .query_row(
            "SELECT attempt_id, intent_id, state, deadline_ms, quota_actions, quota_observations \
             FROM attempts WHERE attempt_id = ?1",
            [id],
            |row| {
                Ok(AttemptFacts {
                    id: row.get(0)?,
                    intent: row.get(1)?,
                    state: row.get(2)?,
                    deadline_ms: row.get(3)?,
                    quota_actions: row.get(4)?,
                    quota_observations: row.get(5)?,
                })
            },
        )
        .unwrap()
}

fn window_world(hold_grants: bool) -> (World, Rc<Cell<u64>>, Rc<Cell<bool>>) {
    let mut config = engine_config();
    config.native_window_scope = Some(NativeWindowScope {
        app: "test.settings".into(),
        pid: 4242,
        window: "w1".into(),
    });
    let mut world = World::with_config(config);
    world.hold_grants = hold_grants;
    let generation = Rc::new(Cell::new(1));
    let include_window_root = Rc::new(Cell::new(true));
    let snapshot_generation = generation.clone();
    let snapshot_include_root = include_window_root.clone();
    world.snapshot_transform = Some(Box::new(move |snapshot| {
        snapshot.identity_boundary = Some(boundary(NS1, snapshot_generation.get()));
        snapshot.global_focus.window_id = ObservationFact::Known { value: "w2".into() };
        snapshot.windows.push(WindowInfo {
            window_id: "w2".into(),
            title: Some("Background window".into()),
            frame: Rect {
                x: 700.0,
                y: 0.0,
                width: 600.0,
                height: 400.0,
            },
            z_index: 1,
            kind: LayerKind::Window,
            modal: false,
            owner_bundle_id: "test.settings".into(),
        });
        if snapshot_include_root.get() {
            let window = snapshot
                .windows
                .iter()
                .find(|window| window.window_id == "w1")
                .unwrap();
            snapshot.objects.insert(
                0,
                UiObject {
                    instance_ref: ObjectRef::new("fixture-window-root").unwrap(),
                    parent: None,
                    window_id: window.window_id.clone(),
                    role: "AXWindow".into(),
                    subrole: Some("AXStandardWindow".into()),
                    title: None,
                    description: None,
                    value: None,
                    enabled: true,
                    focused: false,
                    frame: window.frame,
                    actions: Vec::new(),
                    ax_action_names: None,
                    in_view: true,
                    group: None,
                    secure: false,
                    scroll: None,
                    identity_continuity: ObjectIdentityContinuity::NotificationCovered,
                },
            );
        } else {
            snapshot
                .objects
                .retain(|object| object.role != "AXWindow" || object.window_id != "w1");
        }
        snapshot.objects.push(UiObject {
            instance_ref: ObjectRef::new("fixture-background-window-root").unwrap(),
            parent: None,
            window_id: "w2".into(),
            role: "AXWindow".into(),
            subrole: Some("AXStandardWindow".into()),
            title: None,
            description: None,
            value: None,
            enabled: true,
            focused: false,
            frame: Rect {
                x: 700.0,
                y: 0.0,
                width: 600.0,
                height: 400.0,
            },
            actions: Vec::new(),
            ax_action_names: None,
            in_view: true,
            group: None,
            secure: false,
            scroll: None,
            identity_continuity: ObjectIdentityContinuity::NotificationCovered,
        });
        refresh_simulated_observer_coverage(snapshot);
    }));
    (world, generation, include_window_root)
}

#[test]
fn first_baseline_and_new_source_sequence_do_not_reset_but_boundary_change_allows_fresh_rebind() {
    let app = SimApp::new();
    let pack = AppPack::settings_fixture();
    let mut state = StateCore::new();
    state.set_session(1);
    let mut bindings = BindingReducer::new();

    let first = snapshot(&app, 1, 10, Some(boundary(NS1, 3)));
    assert!(!state.identity_boundary_changed(first.identity_boundary.as_ref()));
    state.admit(first, 10, &pack).unwrap();
    let target = bind_request().semantic_target;
    let request = bind_request();
    assert!(matches!(
        bindings.bind(state.latest().unwrap(), &request, 10).outcome,
        BindingOutcome::Bound { .. }
    ));
    let old = bindings.current(&target).unwrap().clone();

    let same_boundary = snapshot(&app, 2, 20, Some(boundary(NS1, 3)));
    assert!(
        !state.identity_boundary_changed(same_boundary.identity_boundary.as_ref()),
        "a newer source sequence alone is not an identity boundary"
    );
    state.admit(same_boundary, 20, &pack).unwrap();
    assert!(bindings.revalidate(state.latest().unwrap()).is_empty());

    let changed = snapshot(&app, 3, 30, Some(boundary(NS1, 4)));
    assert!(state.identity_boundary_changed(changed.identity_boundary.as_ref()));
    let dropped = bindings.invalidate_all("native identity boundary changed");
    assert_eq!(dropped.len(), 1);
    state.admit(changed, 30, &pack).unwrap();
    assert!(
        state.usable(30, 2_000),
        "identity reset does not make source unusable"
    );
    assert!(matches!(
        bindings.bind(state.latest().unwrap(), &request, 30).outcome,
        BindingOutcome::Bound { .. }
    ));
    let fresh = bindings.current(&target).unwrap();
    assert_eq!(
        fresh.instance, old.instance,
        "retained CF identity may rebind freshly"
    );
    assert_ne!(fresh.binding_ref, old.binding_ref);
    assert_eq!(fresh.env_revision, state.latest().unwrap().revision);

    let new_namespace = Some(boundary(NS2, 1));
    assert!(state.identity_boundary_changed(new_namespace.as_ref()));
}

fn install_boundary(world: &mut World, value: SourceIdentityBoundary) {
    world.snapshot_transform = Some(Box::new(move |snapshot| {
        snapshot.identity_boundary = Some(value.clone());
    }));
}

fn world_with_pending_input(unknown: bool) -> World {
    let mut world = World::new();
    world.hold_observes = true;
    install_boundary(&mut world, boundary(NS1, 1));
    if unknown {
        world.unknown_submission.set(true);
    }
    world.start(TEXT);
    world.until(|world| world.sent("observe") > 0, 5_000);
    world.source_up = false;
    world.deliver_delayed();
    world.until(|world| !world.applied().is_empty(), 10_000);
    assert_eq!(
        world.applied().len(),
        1,
        "the input reached the simulated application once"
    );
    world.hold_observes = false;
    world
}

#[test]
fn changed_boundary_revokes_completed_input_permit_and_does_not_replay() {
    let mut world = world_with_pending_input(false);
    let original_input = world.applied()[0].clone();
    let old_permit = original_input.permit.id.clone();
    let log_start = world.log_len();
    let dependency_start = world.dependency_updates.len();
    let original_attempt = latest_attempt(&world);
    assert!(world.engine.root_state().is_some());

    install_boundary(&mut world, boundary(NS1, 2));
    world.source_up = true;
    world.until(|world| world.sent_since(log_start, "revoke") > 0, 5_000);

    assert_eq!(
        world.core.permit_state(&old_permit),
        Some(PermitState::Revoked)
    );
    assert!(world.sent_since(log_start, "update_dependency") >= 2);
    let reset_dependencies = &world.dependency_updates[dependency_start..];
    assert!(
        reset_dependencies
            .iter()
            .any(|(name, valid)| name == "binding" && *valid == Some(false))
    );
    assert!(
        reset_dependencies
            .iter()
            .any(|(name, valid)| name == "geometry" && *valid == Some(false))
    );
    assert_eq!(
        world
            .applied()
            .iter()
            .filter(|record| record.primitive == original_input.primitive)
            .count(),
        1,
        "the exact completed input is not replayed"
    );
    let original_intent = original_attempt.intent.as_deref().unwrap();
    assert_eq!(
        world
            .core
            .query_receipt(1, original_intent)
            .map(|receipt| receipt.status),
        Some(ReceiptStatus::Completed)
    );
    for _ in 0..12 {
        world.step();
    }
    let retained_attempt = attempt_by_id(&world, &original_attempt.id);
    assert_ne!(retained_attempt.state, "cancelled");
    assert_eq!(retained_attempt.id, original_attempt.id);
    assert_eq!(retained_attempt.intent, original_attempt.intent);
    assert_eq!(retained_attempt.deadline_ms, original_attempt.deadline_ms);
    assert_eq!(
        retained_attempt.quota_actions,
        original_attempt.quota_actions
    );
    assert_eq!(
        retained_attempt.quota_observations,
        original_attempt.quota_observations
    );
}

#[test]
fn identity_change_during_unknown_input_keeps_original_no_replay_reconciliation() {
    let mut world = world_with_pending_input(true);
    let submitted_before = world.sent("submit");
    let original_attempt = latest_attempt(&world);
    let old_permit = world.applied()[0].permit.id.clone();
    let log_start = world.log_len();

    install_boundary(&mut world, boundary(NS2, 1));
    world.source_up = true;
    for _ in 0..16 {
        world.step();
    }

    assert_eq!(world.engine.root_state(), Some(RootState::Suspended));
    assert_eq!(
        world.core.permit_state(&old_permit),
        Some(PermitState::Revoked)
    );
    assert!(world.sent_since(log_start, "revoke") >= 1);
    assert_eq!(world.sent("submit"), submitted_before);
    assert_eq!(
        world.applied().len(),
        1,
        "an unknown input is reconciled, never replayed"
    );
    let reconciled_attempt = latest_attempt(&world);
    assert_eq!(reconciled_attempt.id, original_attempt.id);
    assert_eq!(reconciled_attempt.intent, original_attempt.intent);
    assert_eq!(reconciled_attempt.state, "unknown");
    assert_eq!(reconciled_attempt.deadline_ms, original_attempt.deadline_ms);
    assert_eq!(
        reconciled_attempt.quota_actions,
        original_attempt.quota_actions
    );
    assert_eq!(
        reconciled_attempt.quota_observations,
        original_attempt.quota_observations
    );
}

#[test]
fn boundary_revokes_late_window_activation_grant_and_old_window_binding() {
    let (mut world, generation, _) = window_world(true);
    world.start(TEXT);
    world.until(|world| world.sent("grant_permit") > 0, 5_000);
    assert!(
        world
            .observations
            .iter()
            .any(|observation| observation.purpose
                == interaction_contracts::desktop::device::ObservationPurpose::WindowActivation),
        "expected a WindowActivation observation; {}",
        world.report()
    );
    assert!(world.engine.bindings().all().next().is_some());
    let old_window_binding = world.engine.bindings().all().next().unwrap().clone();

    let mut delayed_grants = world.take_delayed();
    assert_eq!(delayed_grants.len(), 1);
    let (corr, permit) = match delayed_grants.pop().unwrap() {
        Input::Device {
            corr,
            reply: DeviceReply::Grant(GrantOutcome::Granted { permit }),
        } => (corr, permit),
        other => panic!("expected the held WindowActivation grant, got {other:?}"),
    };
    let old_window_revision = world.core.dependency_versions()["window"];
    let submitted_before = world.sent("submit");

    generation.set(2);
    world.publish();
    world.send(Input::Tick);

    assert_eq!(
        world.core.dependency_versions()["window"],
        old_window_revision + 1,
        "identity reset advances the WindowActivation authority epoch"
    );
    assert_eq!(
        world.core.permit_state(&permit.id),
        Some(PermitState::Revoked)
    );
    assert!(
        world
            .engine
            .bindings()
            .all()
            .all(|binding| binding.binding_ref != old_window_binding.binding_ref)
    );
    assert!(world.engine.state().usable(world.now, 2_000));

    world.send(Input::Device {
        corr,
        reply: DeviceReply::Grant(GrantOutcome::Granted { permit }),
    });
    assert_eq!(world.sent("submit"), submitted_before);
}

#[test]
fn a_new_read_revokes_a_late_grant_for_its_read_scoped_binding() {
    let mut world = World::new();
    world
        .app
        .borrow_mut()
        .set_object_identity_continuity("group-push", ObjectIdentityContinuity::ReadScoped);
    world.hold_grants = true;
    world.start(TEXT);
    world.until(|world| world.sent("grant_permit") > 0, 5_000);
    let old = world
        .engine
        .bindings()
        .current(&SemanticTargetRef::new("push/daily-picks").unwrap())
        .unwrap()
        .clone();
    assert_eq!(old.continuity.kind, ObjectIdentityContinuity::ReadScoped);

    let delayed = world.take_delayed();
    let (corr, permit) = match delayed.into_iter().next().unwrap() {
        Input::Device {
            corr,
            reply: DeviceReply::Grant(GrantOutcome::Granted { permit }),
        } => (corr, permit),
        other => panic!("expected a delayed permit grant, got {other:?}"),
    };
    assert_eq!(
        world.core.permit_state(&permit.id),
        Some(PermitState::Issued)
    );

    world.publish();
    world.send(Input::Tick);
    world.send(Input::Device {
        corr,
        reply: DeviceReply::Grant(GrantOutcome::Granted {
            permit: permit.clone(),
        }),
    });

    assert_eq!(
        world.core.permit_state(&permit.id),
        Some(PermitState::Revoked)
    );
    assert_eq!(world.sent("submit"), 0);
    assert!(world.applied().is_empty());
    assert!(world.sent("revoke") >= 1);
}

#[test]
fn a_read_between_grant_ack_and_device_submit_revokes_the_exact_permit() {
    let mut world = World::new();
    world
        .app
        .borrow_mut()
        .set_object_identity_continuity("group-push", ObjectIdentityContinuity::ReadScoped);
    world.hold_grants = true;
    world.start(TEXT);
    world.until(|world| world.sent("grant_permit") > 0, 5_000);
    let delayed = world.take_delayed();
    let (corr, permit) = match delayed.into_iter().next().unwrap() {
        Input::Device {
            corr,
            reply: DeviceReply::Grant(GrantOutcome::Granted { permit }),
        } => (corr, permit),
        other => panic!("expected a delayed permit grant, got {other:?}"),
    };

    // Let Engine accept the grant and produce Submit, but hold that output
    // before it reaches Device. A Source read races into that gap.
    let submit_outputs = world.engine.handle(
        world.now,
        Input::Device {
            corr,
            reply: DeviceReply::Grant(GrantOutcome::Granted {
                permit: permit.clone(),
            }),
        },
    );
    let (submit_corr, submit_request) = submit_outputs
        .iter()
        .find_map(|output| match output {
            Output::Device { corr, command } => match command.as_ref() {
                DeviceCommand::Submit(request) => Some((*corr, request.clone())),
                _ => None,
            },
            _ => None,
        })
        .expect("accepted grant produces one held Submit output");

    world.publish();
    world.send(Input::Tick);
    assert_eq!(
        world.core.permit_state(&permit.id),
        Some(PermitState::Revoked)
    );
    let receipt = world.core.submit(submit_request);
    assert_eq!(receipt.status, ReceiptStatus::Rejected);
    assert!(world.applied().is_empty());
    world.send(Input::Device {
        corr: submit_corr,
        reply: DeviceReply::Receipt(Some(Box::new(receipt))),
    });
    assert_eq!(world.sent("submit"), 0);
    assert!(world.applied().is_empty());
}

#[test]
fn unavailable_observer_revokes_grants_before_and_after_the_engine_ack() {
    for acknowledge_before_read in [false, true] {
        let mut world = World::new();
        world
            .app
            .borrow_mut()
            .set_object_identity_continuity("group-push", ObjectIdentityContinuity::ReadScoped);
        world.hold_grants = true;
        let mark_unavailable = Rc::new(Cell::new(false));
        let source_state = mark_unavailable.clone();
        world.snapshot_transform = Some(Box::new(move |snapshot| {
            if !source_state.get() {
                return;
            }
            for object in &mut snapshot.objects {
                object.identity_continuity = ObjectIdentityContinuity::ReadScoped;
            }
            snapshot.observer_coverage.state = SourceState::Unavailable;
            snapshot.observer_coverage.covered_objects = 0;
            snapshot.observer_coverage.read_scoped_objects = snapshot.objects.len() as u32;
        }));
        world.start(TEXT);
        world.until(|world| world.sent("grant_permit") > 0, 5_000);
        let delayed = world.take_delayed();
        let (grant_corr, permit) = match delayed.into_iter().next().unwrap() {
            Input::Device {
                corr,
                reply: DeviceReply::Grant(GrantOutcome::Granted { permit }),
            } => (corr, permit),
            other => panic!("expected a delayed permit grant, got {other:?}"),
        };
        assert_eq!(
            world.core.permit_state(&permit.id),
            Some(PermitState::Issued)
        );

        let held_submit = if acknowledge_before_read {
            let outputs = world.engine.handle(
                world.now,
                Input::Device {
                    corr: grant_corr,
                    reply: DeviceReply::Grant(GrantOutcome::Granted {
                        permit: permit.clone(),
                    }),
                },
            );
            Some(
                outputs
                    .iter()
                    .find_map(|output| match output {
                        Output::Device { corr, command } => match command.as_ref() {
                            DeviceCommand::Submit(request) => Some((*corr, request.clone())),
                            _ => None,
                        },
                        _ => None,
                    })
                    .expect("the acknowledged grant produces a held Submit output"),
            )
        } else {
            None
        };

        mark_unavailable.set(true);
        world.publish();
        world.send(Input::Tick);
        assert_eq!(
            world.core.permit_state(&permit.id),
            Some(PermitState::Revoked)
        );

        if let Some((submit_corr, request)) = held_submit {
            let receipt = world.core.submit(request);
            assert_eq!(receipt.status, ReceiptStatus::Rejected);
            world.send(Input::Device {
                corr: submit_corr,
                reply: DeviceReply::Receipt(Some(Box::new(receipt))),
            });
        } else {
            world.send(Input::Device {
                corr: grant_corr,
                reply: DeviceReply::Grant(GrantOutcome::Granted { permit }),
            });
        }

        assert_eq!(world.sent("submit"), 0);
        assert!(world.applied().is_empty());
        assert!(world.sent("revoke") >= 1);
    }
}

#[test]
fn boundary_revokes_an_already_installed_window_activation_permit() {
    let (mut world, generation, _) = window_world(true);
    world.start(TEXT);
    world.until(|world| world.sent("grant_permit") > 0, 5_000);
    let mut delayed_grants = world.take_delayed();
    let (corr, permit) = match delayed_grants.pop().unwrap() {
        Input::Device {
            corr,
            reply: DeviceReply::Grant(GrantOutcome::Granted { permit }),
        } => (corr, permit),
        other => panic!("expected a held WindowActivation grant, got {other:?}"),
    };
    world.send(Input::Device {
        corr,
        reply: DeviceReply::Grant(GrantOutcome::Granted {
            permit: permit.clone(),
        }),
    });
    world.until(|world| !world.applied().is_empty(), 1_000);
    assert_eq!(world.applied()[0].permit.id, permit.id);

    let old_window_revision = world.core.dependency_versions()["window"];
    generation.set(2);
    world.publish();
    world.send(Input::Tick);

    assert_eq!(
        world.core.dependency_versions()["window"],
        old_window_revision + 1
    );
    assert_eq!(
        world.core.permit_state(&permit.id),
        Some(PermitState::Revoked)
    );
}

#[test]
fn missing_window_root_keeps_window_dependency_invalid_during_identity_reset() {
    let (mut world, generation, include_window_root) = window_world(true);
    world.start(TEXT);
    world.until(|world| world.sent("grant_permit") > 0, 5_000);
    let mut delayed_grants = world.take_delayed();
    let (corr, permit) = match delayed_grants.pop().unwrap() {
        Input::Device {
            corr,
            reply: DeviceReply::Grant(GrantOutcome::Granted { permit }),
        } => (corr, permit),
        other => panic!("expected a held WindowActivation grant, got {other:?}"),
    };
    generation.set(2);
    include_window_root.set(false);
    world.publish();
    world.send(Input::Tick);

    assert_eq!(
        world.core.permit_state(&permit.id),
        Some(PermitState::Revoked)
    );
    assert!(
        world
            .dependency_updates
            .iter()
            .rev()
            .find(|(name, _)| name == "window")
            .is_some_and(|(_, valid)| *valid == Some(false))
    );
    world.send(Input::Device {
        corr,
        reply: DeviceReply::Grant(GrantOutcome::Granted { permit }),
    });
    assert_eq!(world.sent("submit"), 0);
}
