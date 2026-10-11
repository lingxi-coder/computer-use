//! Background authority is exercised on the normal core, without physical input.
#[allow(dead_code)]
mod common;

use execution_core::backend::{
    HeldState, HitTestOutcome, HitTestRequest, InputBackend, ReleaseFailure, SubmitContext,
    SubmitOutcome,
};
use execution_core::core::{CoreConfig, ExecutionCore, PermitState};
use interaction_contracts::desktop::binding::{
    BindingProof, derive_background_ax_binding_evidence, instance_fingerprint,
};
use interaction_contracts::desktop::device::*;
use interaction_contracts::desktop::domain::ExecutionDomain;
use interaction_contracts::desktop::ids::BindingRef;
use interaction_contracts::desktop::primitive::{AxActionKind, MouseButton, Primitive};
use interaction_contracts::desktop::source::{
    AppIdentity, AxActionName, GlobalFocusObservation, KernelProcessIdentity,
    ObjectIdentityContinuity, ObservationFact, SourceBackgroundWindowContext, SourceWindowContext,
    TargetContext,
};

#[derive(Default)]
struct BackgroundBackend {
    calls: Vec<String>,
    unknown: bool,
}

impl InputBackend for BackgroundBackend {
    fn submit(
        &mut self,
        _: u64,
        primitive: &Primitive,
        context: &SubmitContext<'_>,
    ) -> SubmitOutcome {
        assert_eq!(
            context.intent.execution_domain,
            ExecutionDomain::BackgroundAx
        );
        assert_eq!(
            context.intent.coordinate_space,
            CoordinateSpace::TargetObject
        );
        assert!(matches!(
            primitive,
            Primitive::AxAction {
                action: AxActionKind::Press
            }
        ));
        self.calls.push(context.intent.intent_id.clone());
        if self.unknown {
            SubmitOutcome::Unknown
        } else {
            SubmitOutcome::Applied
        }
    }
    fn release(&mut self, _: u64, _: &Primitive) -> Result<(), ReleaseFailure> {
        panic!("single AXPress never owns held input")
    }
    fn held(&self, _: &str) -> HeldState {
        HeldState::NotHeld
    }
    fn hit_test(&mut self, _: &HitTestRequest<'_>) -> HitTestOutcome {
        panic!("background AX must resolve an element inside submit, without a global hit test")
    }
}

type Core = ExecutionCore<BackgroundBackend>;

fn core() -> Core {
    let mut core = Core::new(
        CoreConfig::background_ax("test"),
        BackgroundBackend::default(),
    );
    core.renew_lease(5_000);
    core.register_root(&RootGrant {
        root_id: "root".into(),
        revision: 1,
        root_epoch: 1,
        deadline_ms: 5_000,
    })
    .unwrap();
    core
}

fn observation(core: &Core) -> Observation {
    observation_with(core, 1, ObjectIdentityContinuity::NotificationCovered)
}

fn observation_with(
    core: &Core,
    sequence: u64,
    continuity: ObjectIdentityContinuity,
) -> Observation {
    let mut snapshot = common::snapshot_with_target(
        "button",
        sequence,
        "AXButton",
        None,
        "Apply",
        common::rect(20.0, 30.0, 80.0, 24.0),
        continuity,
    );
    snapshot.global_focus = GlobalFocusObservation {
        app: ObservationFact::Known {
            value: AppIdentity {
                bundle_id: "another.app".into(),
                name: "Other application".into(),
                pid: 99,
            },
        },
        window_id: ObservationFact::Known {
            value: "window-99".into(),
        },
    };
    snapshot
        .objects
        .iter_mut()
        .find(|o| o.instance_ref.as_str() == "button")
        .unwrap()
        .actions = vec![AxActionName::Press];
    snapshot
        .objects
        .iter_mut()
        .find(|o| o.instance_ref.as_str() == "button")
        .unwrap()
        .ax_action_names = Some(vec!["AXPress".into()]);
    let mut source = SourceBackgroundWindowContext {
        window: SourceWindowContext {
            source_session_id: "source-session".into(),
            session_epoch: snapshot.session_epoch,
            process: KernelProcessIdentity {
                pid: snapshot.app.pid,
                start_seconds: 100,
                start_micros: 20,
            },
            origin_window_id: snapshot.windows[0].window_id.clone(),
            origin_window_frame: snapshot.windows[0].frame,
            verified_source_sequence: snapshot.source_sequence,
            verified_observed_ms: snapshot.observed_ms,
        },
        hidden: ObservationFact::Known { value: false },
        minimized: ObservationFact::Known { value: false },
        target_context_hash: String::new(),
    };
    source.target_context_hash = source.computed_target_context_hash(&snapshot.app).unwrap();
    snapshot.target_context = Some(TargetContext::NativeWindow {
        app: snapshot.app.clone(),
        source,
    });
    let target = snapshot
        .objects
        .iter()
        .find(|o| o.instance_ref.as_str() == "button")
        .unwrap();
    let derived = derive_background_ax_binding_evidence(&snapshot, target).unwrap();
    let proof = BindingProof {
        binding: BindingRef::new("background-button").unwrap(),
        instance: target.instance_ref.clone(),
        snapshot_id: snapshot.snapshot_id.clone(),
        environment_revision: 1,
        layer_revision: 1,
        fingerprint: instance_fingerprint(&snapshot, target),
        display_frame: target.frame,
        continuity: derived.continuity,
        device_context: derived.device_context,
        menu_scope: None,
        focus_scope: None,
    };
    Observation {
        observation_id: format!("background-observation-{sequence}"),
        session_epoch: core.session_epoch(),
        surface: core.surface(),
        target_ref: proof.binding.as_str().into(),
        clock_domain: core.clock_domain(),
        source_sequence: snapshot.source_sequence,
        observed_ms: snapshot.observed_ms,
        valid_until_ms: 2_000,
        freshness: SourceFreshness::Verified,
        execution_domain: ExecutionDomain::BackgroundAx,
        purpose: ObservationPurpose::BackgroundAxAction,
        binding_proof: Some(proof),
        owned_document_proof: None,
        dependencies: core.dependency_versions(),
    }
}

fn ready(core: &mut Core) -> ActionIntent {
    ready_with(core, ObjectIdentityContinuity::NotificationCovered)
}

fn ready_with(core: &mut Core, continuity: ObjectIdentityContinuity) -> ActionIntent {
    let evidence = observation_with(core, 1, continuity);
    core.observe(evidence.clone()).unwrap();
    let permit = core
        .grant_permit(&GrantPermit {
            acked_stop_seq: 0,
            task_id: "root".into(),
            target_ref: evidence.target_ref.clone(),
            evidence_id: evidence.observation_id.clone(),
            valid_until_ms: 1_000,
        })
        .unwrap();
    ActionIntent {
        intent_id: "background-intent".into(),
        attempt_id: Some("attempt-1".into()),
        task_id: "root".into(),
        task_revision: 1,
        root_epoch: 1,
        session_epoch: core.session_epoch(),
        execution_domain: ExecutionDomain::BackgroundAx,
        permit,
        surface: core.surface(),
        target_ref: evidence.target_ref,
        binding_fingerprint: evidence
            .binding_proof
            .as_ref()
            .map(|p| p.fingerprint.digest()),
        binding_proof: evidence.binding_proof,
        owned_document_proof: None,
        coordinate_space: CoordinateSpace::TargetObject,
        dependencies: evidence.dependencies,
        evidence_id: evidence.observation_id,
        latest_start_ms: 500,
        duration_ms: 100,
        steps: vec![ScheduledPrimitive {
            offset_ms: 0,
            primitive: Primitive::AxAction {
                action: AxActionKind::Press,
            },
        }],
    }
}

fn queue(core: &mut Core, intent: ActionIntent) -> Receipt {
    core.submit(SubmitIntent {
        acked_stop_seq: 0,
        intent: intent.into(),
    })
}

#[test]
fn background_press_uses_normal_permit_with_another_app_focused_and_no_hit_test() {
    let mut core = core();
    assert!(!core.dependency_versions().contains_key("focus"));
    assert!(!core.dependency_versions().contains_key("layer"));
    let intent = ready(&mut core);
    assert_eq!(queue(&mut core, intent).status, ReceiptStatus::Accepted);
    core.dispatch_next();
    let receipt = core.query_receipt(1, "background-intent").unwrap();
    assert_eq!(receipt.status, ReceiptStatus::Completed);
    assert_eq!(
        receipt.reason.as_deref(),
        Some("atomic input returned; effect not verified")
    );
    assert_eq!(core.backend().calls.len(), 1);
    assert_eq!(core.held_input_count(), 0);
}

#[test]
fn foreground_profile_rejects_background_evidence_before_recording_it() {
    let mut foreground = Core::new(CoreConfig::production("test"), BackgroundBackend::default());
    let evidence = observation(&core());
    assert!(
        foreground
            .observe(evidence)
            .unwrap_err()
            .contains("execution domain")
    );
    assert!(foreground.backend().calls.is_empty());
}

#[test]
fn background_permit_cannot_authorize_a_foreground_or_simulated_intent() {
    for domain in [
        ExecutionDomain::ForegroundNative,
        ExecutionDomain::Simulated,
    ] {
        let mut core = core();
        let mut intent = ready(&mut core);
        intent.execution_domain = domain;
        assert_eq!(queue(&mut core, intent).status, ReceiptStatus::Rejected);
        core.dispatch_next();
        assert!(core.backend().calls.is_empty());
    }
}

#[test]
fn background_intent_rejects_coordinate_click_keyboard_scroll_and_activation() {
    for primitive in [
        Primitive::Click {
            button: MouseButton::Left,
            x: 20,
            y: 30,
            modifiers: Default::default(),
        },
        Primitive::KeyPress {
            key: "Return".into(),
            modifiers: Default::default(),
        },
        Primitive::Scroll { dx: 0, dy: 1 },
        Primitive::ActivateWindow {
            app: "test.app".into(),
        },
        Primitive::AxAction {
            action: AxActionKind::SetValue {
                value: "changed".into(),
            },
        },
    ] {
        let mut core = core();
        let mut intent = ready(&mut core);
        intent.steps[0].primitive = primitive;
        assert_eq!(queue(&mut core, intent).status, ReceiptStatus::Rejected);
        assert!(core.backend().calls.is_empty());
    }
}

#[test]
fn target_dependency_change_revokes_queued_background_input() {
    for dependency in [
        "authorization",
        "binding",
        "window",
        "geometry",
        "background_scope",
    ] {
        let mut core = core();
        let intent = ready(&mut core);
        let permit_id = intent.permit.id.clone();
        assert_eq!(queue(&mut core, intent).status, ReceiptStatus::Accepted);
        core.update_dependency(dependency, Some(false));
        core.dispatch_next();
        assert_eq!(core.permit_state(&permit_id), Some(PermitState::Revoked));
        assert!(core.backend().calls.is_empty());
    }
}

#[test]
fn a_new_read_revokes_read_scoped_background_authority_even_for_the_same_target() {
    let mut core = core();
    let intent = ready_with(&mut core, ObjectIdentityContinuity::ReadScoped);
    let permit_id = intent.permit.id.clone();
    assert_eq!(queue(&mut core, intent).status, ReceiptStatus::Accepted);
    let next = observation_with(&core, 2, ObjectIdentityContinuity::ReadScoped);
    core.observe(next).unwrap();
    core.dispatch_next();
    assert_eq!(core.permit_state(&permit_id), Some(PermitState::Revoked));
    assert!(core.backend().calls.is_empty());
}

#[test]
fn stop_and_human_input_cancel_queued_background_input() {
    for human in [false, true] {
        let mut core = core();
        let intent = ready(&mut core);
        assert_eq!(queue(&mut core, intent).status, ReceiptStatus::Accepted);
        if human {
            core.human_input_detected();
        } else {
            core.stop(&StopCommand {
                scope: StopScope::All,
                disposition: StopDisposition::Cancel,
                source: StopSource::OperatorButton,
                reason: "stop test".into(),
            });
        }
        core.dispatch_next();
        assert!(core.backend().calls.is_empty());
        assert_eq!(
            core.query_receipt(1, "background-intent").unwrap().status,
            ReceiptStatus::Cancelled
        );
    }
}

#[test]
fn expired_lease_and_source_deadline_prevent_background_submission() {
    for lease in [false, true] {
        let mut core = core();
        if lease {
            core.renew_lease(50);
        }
        let intent = ready(&mut core);
        assert_eq!(queue(&mut core, intent).status, ReceiptStatus::Accepted);
        core.advance_to(if lease { 51 } else { 1_001 }).unwrap();
        core.dispatch_next();
        assert!(core.backend().calls.is_empty());
    }
}

#[test]
fn unknown_background_submission_is_not_replayed() {
    let mut core = core();
    core.backend_mut().unknown = true;
    let intent = ready(&mut core);
    assert_eq!(
        queue(&mut core, intent.clone()).status,
        ReceiptStatus::Accepted
    );
    core.dispatch_next();
    assert_eq!(
        core.query_receipt(1, "background-intent").unwrap().status,
        ReceiptStatus::Unknown
    );
    assert!(core.is_isolated());
    assert!(
        core.grant_permit(&GrantPermit {
            acked_stop_seq: 0,
            task_id: "root".into(),
            target_ref: intent.target_ref.clone(),
            evidence_id: intent.evidence_id.clone(),
            valid_until_ms: 1_000,
        })
        .is_err(),
        "unknown input cannot gain a new permit and intent from the same observation"
    );
    assert_eq!(queue(&mut core, intent).status, ReceiptStatus::Unknown);
    core.dispatch_next();
    assert_eq!(core.backend().calls.len(), 1);
}
