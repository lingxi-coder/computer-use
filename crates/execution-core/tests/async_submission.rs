//! Deterministic production-state-machine tests for an asynchronous backend.
mod common;

use common::{binding_proof, rect, snapshot_with_target};
use execution_core::backend::*;
use execution_core::core::{CoreConfig, ExecutionCore};
use interaction_contracts::desktop::{
    binding::{
        FocusedControlScope, FocusedKeyProfile, LayoutIndependentSpecialKey, instance_fingerprint,
    },
    device::*,
    error::FailureCode,
    primitive::{AxActionKind, Primitive},
    source::ObjectIdentityContinuity,
};
use std::collections::VecDeque;

#[derive(Default)]
struct DelayedBackend {
    requests: Vec<(u64, String)>,
    replies: VecDeque<SubmitCompletion>,
    revoked: Vec<PermitRef>,
    abandoned: bool,
}
impl InputBackend for DelayedBackend {
    fn submit(&mut self, _: u64, _: &Primitive, context: &SubmitContext<'_>) -> SubmitOutcome {
        self.requests.push((
            context.intent.session_epoch,
            context.intent.intent_id.clone(),
        ));
        SubmitOutcome::Pending {
            sequence: self.requests.len() as u64,
            reply_deadline_ms: context.authority.valid_until_ms + 500,
        }
    }
    fn poll_completion(&mut self) -> Option<SubmitCompletion> {
        self.replies.pop_front()
    }
    fn revoke(&mut self, permit: &PermitRef) {
        self.revoked.push(permit.clone());
    }
    fn abandon_pending(&mut self) {
        self.abandoned = true;
    }
    fn release(&mut self, _: u64, _: &Primitive) -> Result<(), ReleaseFailure> {
        Ok(())
    }
    fn held(&self, _: &str) -> HeldState {
        HeldState::NotHeld
    }
    fn hit_test(&mut self, _: &HitTestRequest<'_>) -> HitTestOutcome {
        HitTestOutcome::CheckedDuringSubmit
    }
}
type Core = ExecutionCore<DelayedBackend>;
fn press() -> Primitive {
    Primitive::AxAction {
        action: AxActionKind::Press,
    }
}
fn ready() -> (Core, ActionIntent) {
    let mut core = ExecutionCore::new(CoreConfig::production("test"), DelayedBackend::default());
    core.renew_lease(5000);
    core.register_root(&RootGrant {
        root_id: "root".into(),
        revision: 1,
        root_epoch: 1,
        deadline_ms: 5000,
    })
    .unwrap();
    let snapshot = snapshot_with_target(
        "object",
        1,
        "AXButton",
        None,
        "target",
        rect(0.0, 0.0, 100.0, 100.0),
        ObjectIdentityContinuity::NotificationCovered,
    );
    let proof = binding_proof(&snapshot, "object", "target", None, None);
    let evidence = Observation {
        observation_id: "obs".into(),
        session_epoch: 1,
        surface: core.surface(),
        target_ref: "target".into(),
        clock_domain: core.clock_domain(),
        source_sequence: 1,
        observed_ms: 0,
        valid_until_ms: 2000,
        freshness: SourceFreshness::Verified,
        execution_domain: core.config().execution_domain,
        purpose: Default::default(),
        owned_document_proof: None,
        binding_proof: Some(proof.clone()),
        dependencies: core.dependency_versions(),
    };
    core.observe(evidence.clone()).unwrap();
    let permit = core
        .grant_permit(&GrantPermit {
            acked_stop_seq: 0,
            task_id: "root".into(),
            target_ref: "target".into(),
            evidence_id: "obs".into(),
            valid_until_ms: 1000,
        })
        .unwrap();
    let intent = ActionIntent {
        intent_id: "one".into(),
        attempt_id: None,
        task_id: "root".into(),
        task_revision: 1,
        root_epoch: 1,
        session_epoch: 1,
        execution_domain: evidence.execution_domain,
        permit,
        surface: core.surface(),
        target_ref: "target".into(),
        binding_fingerprint: Some(proof.fingerprint.digest()),
        owned_document_proof: None,
        binding_proof: Some(proof),
        coordinate_space: CoordinateSpace::DisplayPoints,
        dependencies: core.dependency_versions(),
        evidence_id: "obs".into(),
        latest_start_ms: 500,
        duration_ms: 100,
        steps: vec![ScheduledPrimitive {
            offset_ms: 0,
            primitive: press(),
        }],
    };
    (core, intent)
}

fn focused_key_ready() -> (Core, ActionIntent) {
    use interaction_contracts::desktop::source::{
        CompositionState, FocusContext, FocusRelation, KernelProcessIdentity, SourceKeyboardContext,
    };
    let (mut core, mut intent) = ready();
    let mut snapshot = snapshot_with_target(
        "object",
        2,
        "AXCheckBox",
        None,
        "target",
        rect(0.0, 0.0, 100.0, 100.0),
        ObjectIdentityContinuity::NotificationCovered,
    );
    let target_index = snapshot
        .objects
        .iter()
        .position(|object| object.instance_ref.as_str() == "object")
        .unwrap();
    snapshot.objects[target_index].focused = true;
    let target = &snapshot.objects[target_index];
    let target_fingerprint = instance_fingerprint(&snapshot, target);
    let focus_context = FocusContext {
        source_session_id: "actual-source-watch".into(),
        session_epoch: 1,
        process: KernelProcessIdentity {
            pid: 42,
            start_seconds: 1,
            start_micros: 0,
        },
        origin_window_id: "window-1".into(),
        origin_window_frame: rect(0.0, 0.0, 800.0, 600.0),
        target_instance: target.instance_ref.clone(),
        target_fingerprint,
        target_frame: target.frame,
        focused_instance: target.instance_ref.clone(),
        relation: FocusRelation::ExactControl,
        composition: CompositionState::NotApplicable,
        keyboard: SourceKeyboardContext {
            selected_input_source: None,
            underlying_keyboard_layout: None,
            keyboard_type: None,
            caps_lock: Some(false),
        },
        verified_source_sequence: 2,
        verified_observed_ms: 0,
    };
    snapshot.focus_context = Some(focus_context.clone());
    let focused_scope = FocusedControlScope {
        profile: FocusedKeyProfile::MacosAtomicSpecialKeyV1,
        key: LayoutIndependentSpecialKey::Space,
        context: focus_context,
    };
    let proof = binding_proof(&snapshot, "object", "target", None, Some(focused_scope));
    intent.binding_proof = Some(proof.clone());
    intent.attempt_id = Some("focused-attempt".into());
    intent.evidence_id = "focused-observation".into();
    intent.binding_fingerprint = Some(proof.fingerprint.digest());
    intent.steps[0].primitive = Primitive::KeyPress {
        key: "Space".into(),
        modifiers: Default::default(),
    };
    core.observe(Observation {
        observation_id: intent.evidence_id.clone(),
        session_epoch: 1,
        surface: core.surface(),
        target_ref: intent.target_ref.clone(),
        clock_domain: core.clock_domain(),
        source_sequence: 2,
        observed_ms: 0,
        valid_until_ms: 2000,
        freshness: SourceFreshness::Verified,
        execution_domain: core.config().execution_domain,
        purpose: ObservationPurpose::FocusedControl,
        owned_document_proof: None,
        binding_proof: intent.binding_proof.clone(),
        dependencies: core.dependency_versions(),
    })
    .unwrap();
    intent.permit = core
        .grant_permit(&GrantPermit {
            acked_stop_seq: 0,
            task_id: "root".into(),
            target_ref: "target".into(),
            evidence_id: intent.evidence_id.clone(),
            valid_until_ms: 1000,
        })
        .unwrap();
    (core, intent)
}

#[test]
fn focused_atomic_key_requires_an_actual_return_and_unknown_never_replays() {
    for outcome in [SubmitOutcome::Applied, SubmitOutcome::Unknown] {
        let (mut core, intent) = focused_key_ready();
        submit(&mut core, &intent);
        complete(&mut core, outcome, Some(5));
        core.backend_mut().replies.back_mut().unwrap().input_end_ms = Some(7);
        core.advance_to(10).unwrap();
        assert_eq!(
            receipt(&core).status,
            if outcome == SubmitOutcome::Applied {
                ReceiptStatus::Completed
            } else {
                ReceiptStatus::Unknown
            }
        );
        assert_eq!(receipt(&core).input_end_ms, Some(7));
        assert_eq!(receipt(&core).submitted.len(), 1);
        assert_eq!(core.is_isolated(), outcome == SubmitOutcome::Unknown);
        core.advance_to(100).unwrap();
        assert!(core.dispatch_next().is_none());
        assert_eq!(core.backend().requests.len(), 1);
    }
    let (mut core, intent) = focused_key_ready();
    submit(&mut core, &intent);
    complete(&mut core, SubmitOutcome::Applied, Some(5));
    core.backend_mut().replies.back_mut().unwrap().input_end_ms = None;
    core.advance_to(10).unwrap();
    assert_ne!(
        receipt(&core).status,
        ReceiptStatus::Completed,
        "enqueue/applied without actual input end cannot use the native fast completion path"
    );
    assert_eq!(receipt(&core).input_end_ms, None);
}

#[test]
fn focused_key_permit_does_not_authorize_other_primitives_or_receiver_changes() {
    for change in 0..6 {
        let (mut core, mut intent) = focused_key_ready();
        match change {
            0 => intent.steps[0].primitive = press(),
            1 => intent.steps[0].primitive = Primitive::TypeText { text: " ".into() },
            2 => {
                intent.steps[0].primitive = Primitive::KeyPress {
                    key: "Return".into(),
                    modifiers: Default::default(),
                }
            }
            3 => {
                if let Primitive::KeyPress { modifiers, .. } = &mut intent.steps[0].primitive {
                    modifiers.shift = true;
                }
            }
            4 => intent.steps[0].offset_ms = 1,
            _ => {
                intent
                    .binding_proof
                    .as_mut()
                    .unwrap()
                    .focus_scope
                    .as_mut()
                    .unwrap()
                    .context
                    .process
                    .start_seconds += 1
            }
        }
        assert_eq!(
            core.submit(SubmitIntent {
                acked_stop_seq: 0,
                intent: intent.into(),
            })
            .status,
            ReceiptStatus::Rejected
        );
        assert!(core.dispatch_next().is_none());
        assert!(core.backend().requests.is_empty());
        assert_eq!(receipt(&core).status, ReceiptStatus::Rejected);
    }
}

#[test]
fn ordinary_target_input_without_positive_focus_cannot_submit_keypress() {
    let (mut core, mut intent) = ready();
    intent.steps[0].primitive = Primitive::KeyPress {
        key: "Space".into(),
        modifiers: Default::default(),
    };
    assert_eq!(
        core.submit(SubmitIntent {
            acked_stop_seq: 0,
            intent: intent.into(),
        })
        .status,
        ReceiptStatus::Rejected
    );
    assert!(core.dispatch_next().is_none());
    assert!(core.backend().requests.is_empty());
}

/// Source-owned values for a virtual anchored menu. The opener is submitted
/// through the real core first; the item reference never invents its receipt.
fn menu_ready() -> (Core, ActionIntent) {
    use interaction_contracts::desktop::binding::{MenuBindingScope, MenuOpeningRef};
    use interaction_contracts::desktop::ids::{AttemptId, ObjectRef};
    use interaction_contracts::desktop::source::{
        AxActionName, KernelProcessIdentity, MenuAnchorContinuity, MenuContext, MenuDrawingWindow,
        MenuOpenerAnchor,
    };
    let (mut core, mut intent) = ready();
    let mut opener_snapshot = snapshot_with_target(
        "opener",
        2,
        "AXMenuButton",
        None,
        "opener",
        rect(0.0, 0.0, 100.0, 100.0),
        ObjectIdentityContinuity::NotificationCovered,
    );
    let opener_index = opener_snapshot
        .objects
        .iter()
        .position(|object| object.instance_ref.as_str() == "opener")
        .unwrap();
    opener_snapshot.objects[opener_index].actions = vec![AxActionName::ShowMenu];
    let opener_object = &opener_snapshot.objects[opener_index];
    let opener_fingerprint = instance_fingerprint(&opener_snapshot, opener_object);
    let mut anchor = MenuOpenerAnchor {
        anchor_id: "actual-source-anchor".into(),
        source_session_id: "source-watch".into(),
        anchor_revision: 1,
        process: KernelProcessIdentity {
            pid: 42,
            start_seconds: 1,
            start_micros: 0,
        },
        origin_window_id: "window-1".into(),
        origin_window_frame: rect(0.0, 0.0, 800.0, 600.0),
        opener_instance: opener_object.instance_ref.clone(),
        opener_fingerprint,
        opener_frame: opener_object.frame,
        verified_source_sequence: 2,
        verified_observed_ms: 0,
        continuity: MenuAnchorContinuity::DirectRead,
    };
    opener_snapshot.menu_anchors = vec![anchor.clone()];
    let opener_scope = MenuBindingScope::Opener {
        anchor: anchor.clone(),
        attempt_deadline_ms: 5000,
    };
    let opener_proof = binding_proof(
        &opener_snapshot,
        "opener",
        "target",
        Some(opener_scope),
        None,
    );
    intent.binding_proof = Some(opener_proof.clone());
    intent.attempt_id = Some("menu-attempt".into());
    intent.intent_id = "opening-input".into();
    intent.evidence_id = "opener-obs".into();
    intent.binding_fingerprint = Some(opener_proof.fingerprint.digest());
    intent.steps[0].primitive = Primitive::AxAction {
        action: AxActionKind::ShowMenu,
    };
    core.observe(Observation {
        observation_id: intent.evidence_id.clone(),
        session_epoch: 1,
        surface: core.surface(),
        target_ref: "target".into(),
        clock_domain: core.clock_domain(),
        source_sequence: 2,
        observed_ms: 0,
        valid_until_ms: 2000,
        freshness: SourceFreshness::Verified,
        execution_domain: core.config().execution_domain,
        purpose: ObservationPurpose::TargetInput,
        owned_document_proof: None,
        binding_proof: intent.binding_proof.clone(),
        dependencies: core.dependency_versions(),
    })
    .unwrap();
    intent.permit = core
        .grant_permit(&GrantPermit {
            acked_stop_seq: 0,
            task_id: "root".into(),
            target_ref: "target".into(),
            evidence_id: intent.evidence_id.clone(),
            valid_until_ms: 1000,
        })
        .unwrap();
    submit(&mut core, &intent);
    core.backend_mut().replies.push_back(SubmitCompletion {
        sequence: 1,
        session_epoch: 1,
        intent_id: "opening-input".into(),
        outcome: SubmitOutcome::Applied,
        submitted_at_ms: Some(5),
        input_end_ms: Some(7),
        failure_code: None,
    });
    core.advance_to(10).unwrap();
    assert_eq!(
        core.query_receipt(1, "opening-input").unwrap().status,
        ReceiptStatus::Completed
    );
    anchor.verified_source_sequence = 3;
    anchor.verified_observed_ms = 10;
    let mut menu = anchor.opener_fingerprint.clone();
    menu.role = "AXMenu".into();
    menu.label = "".into();
    menu.ancestor_roles.push("AXMenuButton".into());
    let context = MenuContext {
        context_id: "source-menu-context".into(),
        source_session_id: anchor.source_session_id.clone(),
        opener: anchor,
        menu_instance: ObjectRef::new("menu").unwrap(),
        menu_fingerprint: menu.clone(),
        menu_frame: opener_proof.display_frame,
        drawing_window: MenuDrawingWindow {
            window_id: 999,
            owner_pid: 42,
            layer: 101,
            frame: opener_proof.display_frame,
        },
    };
    let root = common::ui_object(
        "window-root",
        None,
        "AXWindow",
        Some("AXStandardWindow"),
        Some("Settings"),
        rect(0.0, 0.0, 800.0, 600.0),
        ObjectIdentityContinuity::NotificationCovered,
    );
    let mut opener = common::ui_object(
        "opener",
        Some("window-root"),
        "AXMenuButton",
        None,
        Some("opener"),
        opener_proof.display_frame,
        ObjectIdentityContinuity::NotificationCovered,
    );
    opener.actions = vec![AxActionName::ShowMenu];
    let menu_object = common::ui_object(
        "menu",
        Some("opener"),
        "AXMenu",
        None,
        None,
        opener_proof.display_frame,
        ObjectIdentityContinuity::NotificationCovered,
    );
    let menu_item = common::ui_object(
        "menu-item",
        Some("menu"),
        "AXMenuItem",
        None,
        Some("choice"),
        rect(10.0, 10.0, 80.0, 20.0),
        ObjectIdentityContinuity::NotificationCovered,
    );
    let menu_sibling = common::ui_object(
        "menu-sibling",
        Some("menu"),
        "AXMenuItem",
        None,
        Some("another choice"),
        rect(10.0, 40.0, 80.0, 20.0),
        ObjectIdentityContinuity::NotificationCovered,
    );
    let mut menu_snapshot =
        common::snapshot_from_objects(vec![root, opener, menu_object, menu_item, menu_sibling], 3);
    menu_snapshot.observed_ms = 10;
    menu_snapshot.menu_context = Some(context.clone());
    menu_snapshot.menu_anchors = vec![context.opener.clone()];
    let item_scope = MenuBindingScope::Item {
        context,
        opened_by: MenuOpeningRef {
            attempt_id: AttemptId::new("menu-attempt").unwrap(),
            intent_id: "opening-input".into(),
            attempt_deadline_ms: 5000,
        },
    };
    let proof = binding_proof(
        &menu_snapshot,
        "menu-item",
        "target",
        Some(item_scope),
        None,
    );
    intent.binding_proof = Some(proof.clone());
    intent.intent_id = "one".into();
    intent.evidence_id = "menu-obs".into();
    intent.binding_fingerprint = Some(proof.fingerprint.digest());
    intent.steps[0].primitive = press();
    core.observe(Observation {
        observation_id: intent.evidence_id.clone(),
        session_epoch: 1,
        surface: core.surface(),
        target_ref: "target".into(),
        clock_domain: core.clock_domain(),
        source_sequence: 3,
        observed_ms: 10,
        valid_until_ms: 2000,
        freshness: SourceFreshness::Verified,
        execution_domain: core.config().execution_domain,
        purpose: ObservationPurpose::MenuSelection,
        owned_document_proof: None,
        binding_proof: intent.binding_proof.clone(),
        dependencies: core.dependency_versions(),
    })
    .unwrap();
    intent.permit = core
        .grant_permit(&GrantPermit {
            acked_stop_seq: 0,
            task_id: "root".into(),
            target_ref: "target".into(),
            evidence_id: intent.evidence_id.clone(),
            valid_until_ms: 1000,
        })
        .unwrap();
    (core, intent)
}

#[test]
fn anchored_menu_press_completes_or_isolates_without_replaying() {
    for outcome in [SubmitOutcome::Applied, SubmitOutcome::Unknown] {
        let (mut core, intent) = menu_ready();
        submit(&mut core, &intent);
        core.backend_mut().replies.push_back(SubmitCompletion {
            sequence: 2,
            session_epoch: 1,
            intent_id: "one".into(),
            outcome,
            submitted_at_ms: Some(15),
            input_end_ms: Some(17),
            failure_code: None,
        });
        core.advance_to(20).unwrap();
        assert_eq!(
            receipt(&core).status,
            if outcome == SubmitOutcome::Applied {
                ReceiptStatus::Completed
            } else {
                ReceiptStatus::Unknown
            }
        );
        assert_eq!(receipt(&core).input_end_ms, Some(17));
        core.submit(SubmitIntent {
            acked_stop_seq: 0,
            intent: intent.into(),
        });
        core.advance_to(50).unwrap();
        assert_eq!(
            core.backend().requests.len(),
            2,
            "one opener and one selection"
        );
        assert_eq!(core.is_isolated(), outcome == SubmitOutcome::Unknown);
    }
}

#[test]
fn menu_selection_cannot_mix_primitive_attempt_or_root_authority() {
    for change in 0..5 {
        let (mut core, mut intent) = menu_ready();
        match change {
            0 => intent.steps[0].primitive = Primitive::Scroll { dx: 0, dy: -40 },
            1 => intent.attempt_id = Some("another-attempt".into()),
            2 => intent.binding_proof = None,
            3 => intent.session_epoch += 1,
            _ => {
                core.register_root(&RootGrant {
                    root_id: "another-root".into(),
                    revision: 1,
                    root_epoch: 1,
                    deadline_ms: 5000,
                })
                .unwrap();
                intent.task_id = "another-root".into();
                intent.permit = core
                    .grant_permit(&GrantPermit {
                        acked_stop_seq: 0,
                        task_id: intent.task_id.clone(),
                        target_ref: "target".into(),
                        evidence_id: intent.evidence_id.clone(),
                        valid_until_ms: 1000,
                    })
                    .unwrap();
            }
        }
        assert_eq!(
            core.submit(SubmitIntent {
                acked_stop_seq: 0,
                intent: intent.into(),
            })
            .status,
            ReceiptStatus::Rejected,
            "accepted mutation {change}"
        );
        assert_eq!(
            core.backend().requests.len(),
            1,
            "only the opener reached input"
        );
    }
}

#[test]
fn menu_evidence_cannot_use_ordinary_purpose_or_missing_menu_dependency() {
    for change in 0..3 {
        let (mut core, intent) = menu_ready();
        let mut evidence = Observation {
            observation_id: "bad-menu".into(),
            session_epoch: 1,
            surface: core.surface(),
            target_ref: "target".into(),
            clock_domain: core.clock_domain(),
            source_sequence: 4,
            observed_ms: 10,
            valid_until_ms: 2000,
            freshness: SourceFreshness::Verified,
            execution_domain: core.config().execution_domain,
            purpose: ObservationPurpose::MenuSelection,
            owned_document_proof: None,
            binding_proof: intent.binding_proof.clone(),
            dependencies: core.dependency_versions(),
        };
        if let Some(interaction_contracts::desktop::binding::MenuBindingScope::Item {
            context,
            ..
        }) = evidence.binding_proof.as_mut().unwrap().menu_scope.as_mut()
        {
            context.opener.verified_source_sequence = 4;
        }
        match change {
            0 => evidence.purpose = ObservationPurpose::TargetInput,
            1 => evidence.binding_proof = None,
            _ => {
                evidence.dependencies.remove("menu");
            }
        }
        let observed = core.observe(evidence);
        if change < 2 {
            assert!(observed.is_err());
        } else {
            observed.unwrap();
            assert!(
                core.grant_permit(&GrantPermit {
                    acked_stop_seq: 0,
                    task_id: "root".into(),
                    target_ref: "target".into(),
                    evidence_id: "bad-menu".into(),
                    valid_until_ms: 1000
                })
                .is_err()
            );
        }
        assert_eq!(core.backend().requests.len(), 1);
    }
}

#[test]
fn changed_menu_context_anchor_or_source_session_revokes_the_old_permit() {
    use interaction_contracts::desktop::binding::MenuBindingScope;
    for change in 0..3 {
        let (mut core, intent) = menu_ready();
        let mut proof = intent.binding_proof.clone().unwrap();
        let Some(MenuBindingScope::Item { context, .. }) = proof.menu_scope.as_mut() else {
            unreachable!()
        };
        context.opener.verified_source_sequence = 4;
        match change {
            0 => context.context_id = "replacement-context".into(),
            1 => context.opener.anchor_revision += 1,
            _ => {
                context.source_session_id = "replacement-watch".into();
                context.opener.source_session_id = "replacement-watch".into();
            }
        }
        core.observe(Observation {
            observation_id: "replacement-menu".into(),
            session_epoch: 1,
            surface: core.surface(),
            target_ref: "target".into(),
            clock_domain: core.clock_domain(),
            source_sequence: 4,
            observed_ms: 10,
            valid_until_ms: 2000,
            freshness: SourceFreshness::Verified,
            execution_domain: core.config().execution_domain,
            purpose: ObservationPurpose::MenuSelection,
            owned_document_proof: None,
            binding_proof: Some(proof),
            dependencies: core.dependency_versions(),
        })
        .unwrap();
        assert_eq!(
            core.submit(SubmitIntent {
                acked_stop_seq: 0,
                intent: intent.into(),
            })
            .status,
            ReceiptStatus::Rejected
        );
        assert_eq!(core.backend().requests.len(), 1);
    }
}

#[test]
fn fresh_menu_proof_cannot_invent_an_opening_or_extend_the_attempt_deadline() {
    use interaction_contracts::desktop::binding::MenuBindingScope;
    for extend_deadline in [false, true] {
        let (mut core, mut intent) = menu_ready();
        let Some(MenuBindingScope::Item { context, opened_by }) =
            intent.binding_proof.as_mut().unwrap().menu_scope.as_mut()
        else {
            unreachable!()
        };
        context.opener.verified_source_sequence = 4;
        if extend_deadline {
            opened_by.attempt_deadline_ms += 1;
        } else {
            opened_by.intent_id = "fabricated-opening".into();
        }
        intent.evidence_id = "fresh-menu-proof".into();
        core.observe(Observation {
            observation_id: intent.evidence_id.clone(),
            session_epoch: 1,
            surface: core.surface(),
            target_ref: "target".into(),
            clock_domain: core.clock_domain(),
            source_sequence: 4,
            observed_ms: 10,
            valid_until_ms: 2000,
            freshness: SourceFreshness::Verified,
            execution_domain: core.config().execution_domain,
            purpose: ObservationPurpose::MenuSelection,
            owned_document_proof: None,
            binding_proof: intent.binding_proof.clone(),
            dependencies: core.dependency_versions(),
        })
        .unwrap();
        intent.permit = core
            .grant_permit(&GrantPermit {
                acked_stop_seq: 0,
                task_id: "root".into(),
                target_ref: "target".into(),
                evidence_id: intent.evidence_id.clone(),
                valid_until_ms: 1000,
            })
            .unwrap();
        assert_eq!(
            core.submit(SubmitIntent {
                acked_stop_seq: 0,
                intent: intent.into(),
            })
            .status,
            ReceiptStatus::Rejected
        );
        assert_eq!(core.backend().requests.len(), 1);
    }
}
fn submit(core: &mut Core, intent: &ActionIntent) {
    assert_eq!(
        core.submit(SubmitIntent {
            acked_stop_seq: 0,
            intent: intent.clone().into()
        })
        .status,
        ReceiptStatus::Accepted
    );
    assert_eq!(core.dispatch_next().unwrap().status, ReceiptStatus::Running);
}
fn receipt(core: &Core) -> Receipt {
    core.query_receipt(1, "one").unwrap()
}
fn complete(core: &mut Core, outcome: SubmitOutcome, at: Option<u64>) {
    core.backend_mut().replies.push_back(SubmitCompletion {
        sequence: 1,
        session_epoch: 1,
        intent_id: "one".into(),
        outcome,
        submitted_at_ms: at,
        input_end_ms: at,
        failure_code: None,
    });
}
#[test]
fn atomic_pointer_completion_uses_returned_input_end_and_never_replays_after_unknown() {
    use interaction_contracts::desktop::primitive::{Modifiers, MouseButton};
    for outcome in [SubmitOutcome::Applied, SubmitOutcome::Unknown] {
        let (mut core, mut intent) = ready();
        intent.steps[0].primitive = Primitive::Click {
            button: MouseButton::Left,
            x: 50,
            y: 50,
            modifiers: Modifiers::default(),
        };
        submit(&mut core, &intent);
        complete(&mut core, outcome, Some(5));
        core.advance_to(10).unwrap();
        let r = receipt(&core);
        assert_eq!(
            r.status,
            if outcome == SubmitOutcome::Applied {
                ReceiptStatus::Completed
            } else {
                ReceiptStatus::Unknown
            }
        );
        assert_eq!(r.input_end_ms, Some(5));
        assert_eq!(core.backend().requests.len(), 1);
        core.submit(SubmitIntent {
            acked_stop_seq: 0,
            intent: intent.into(),
        });
        core.advance_to(50).unwrap();
        assert_eq!(core.backend().requests.len(), 1);
        if outcome == SubmitOutcome::Unknown {
            assert!(core.is_isolated());
        }
    }
}
#[test]
fn held_pointer_move_stays_rejected_by_atomic_only_policy() {
    let (mut core, mut intent) = ready();
    intent.steps[0].primitive = Primitive::PointerMove { x: 50, y: 50 };
    let receipt = core.submit(SubmitIntent {
        acked_stop_seq: 0,
        intent: intent.into(),
    });
    assert_eq!(receipt.status, ReceiptStatus::Rejected);
    assert!(receipt.submitted.is_empty() && core.backend().requests.is_empty());
    assert_eq!(core.held_input_count(), 0);
}

#[test]
fn atomic_move_scroll_and_menu_complete_at_actual_return_and_unknown_is_not_replayed() {
    for primitive in [
        Primitive::PointerPosition { x: 50, y: 50 },
        Primitive::Scroll { dx: 0, dy: -120 },
        Primitive::AxAction {
            action: AxActionKind::ShowMenu,
        },
    ] {
        for outcome in [SubmitOutcome::Applied, SubmitOutcome::Unknown] {
            let (mut core, mut intent) = ready();
            intent.steps[0].primitive = primitive.clone();
            submit(&mut core, &intent);
            complete(&mut core, outcome, Some(5));
            core.backend_mut().replies.back_mut().unwrap().input_end_ms = Some(7);
            core.advance_to(10).unwrap();
            let r = receipt(&core);
            assert_eq!(
                r.status,
                if outcome == SubmitOutcome::Applied {
                    ReceiptStatus::Completed
                } else {
                    ReceiptStatus::Unknown
                }
            );
            assert_eq!(r.input_end_ms, Some(7));
            assert!(r.command_end_ms.unwrap() > 10);
            core.submit(SubmitIntent {
                acked_stop_seq: 0,
                intent: intent.into(),
            });
            core.advance_to(50).unwrap();
            assert_eq!(core.backend().requests.len(), 1);
            assert_eq!(core.is_isolated(), outcome == SubmitOutcome::Unknown);
        }
    }
}

fn stop(core: &mut Core) {
    core.stop(&StopCommand {
        scope: StopScope::All,
        disposition: StopDisposition::Cancel,
        source: StopSource::OperatorButton,
        reason: "user stop".into(),
    });
}

#[test]
fn pending_input_is_not_replayed_or_reported_as_submitted_before_ack() {
    let (mut core, intent) = ready();
    submit(&mut core, &intent);
    assert!(receipt(&core).submitted.is_empty());
    assert!(core.dispatch_next().is_none());
    assert_eq!(
        core.submit(SubmitIntent {
            acked_stop_seq: 0,
            intent: intent.into(),
        })
        .status,
        ReceiptStatus::Running
    );
    core.advance_to(20).unwrap();
    assert_eq!(core.backend().requests.len(), 1);
    complete(&mut core, SubmitOutcome::Applied, Some(7));
    core.advance_to(21).unwrap();
    assert_eq!(receipt(&core).submitted[0].at_ms(), 7);
    assert_eq!(receipt(&core).status, ReceiptStatus::Completed);
    assert_eq!(receipt(&core).command_end_ms, Some(100));
    core.advance_to(100).unwrap();
    assert_eq!(receipt(&core).status, ReceiptStatus::Completed);
}
#[test]
fn stop_preserves_uncertainty_until_native_rejection_arrives() {
    let (mut core, intent) = ready();
    submit(&mut core, &intent);
    stop(&mut core);
    assert_eq!(receipt(&core).status, ReceiptStatus::Unknown);
    assert!(
        core.is_isolated(),
        "stop fences the whole channel immediately"
    );
    assert_eq!(core.backend().revoked, vec![intent.permit]);
    assert!(core.recover_channel().is_err());
    complete(&mut core, SubmitOutcome::Rejected, None);
    core.advance_to(10).unwrap();
    assert_eq!(receipt(&core).status, ReceiptStatus::Unknown);
    assert!(receipt(&core).submitted.is_empty());
    assert!(
        core.is_isolated(),
        "a late rejection is not channel recovery"
    );
    assert!(core.dispatch_next().is_none());
    assert_eq!(core.backend().requests.len(), 1);
}
#[test]
fn an_applied_ack_after_stop_preserves_actual_input_without_remaining_steps() {
    let (mut core, mut intent) = ready();
    intent.steps.push(ScheduledPrimitive {
        offset_ms: 10,
        primitive: press(),
    });
    submit(&mut core, &intent);
    core.advance_to(5).unwrap();
    stop(&mut core);
    assert_eq!(receipt(&core).status, ReceiptStatus::Unknown);
    assert!(core.is_isolated());
    complete(&mut core, SubmitOutcome::Applied, Some(4));
    core.advance_to(20).unwrap();
    assert_eq!(receipt(&core).status, ReceiptStatus::Unknown);
    assert_eq!(receipt(&core).submitted[0].at_ms(), 4);
    assert!(
        core.is_isolated(),
        "a late applied reply cannot settle the stop"
    );
    assert_eq!(core.backend().requests.len(), 1);
}
#[test]
fn an_unknown_ack_after_stop_records_call_and_isolates_channel() {
    let (mut core, intent) = ready();
    submit(&mut core, &intent);
    stop(&mut core);
    assert!(core.is_isolated());
    assert!(core.recover_channel().is_err());
    complete(&mut core, SubmitOutcome::Unknown, Some(3));
    core.advance_to(10).unwrap();
    assert_eq!(receipt(&core).status, ReceiptStatus::Unknown);
    assert_eq!(receipt(&core).submitted[0].at_ms(), 3);
    assert_eq!(receipt(&core).reason_code, Some(FailureCode::EffectUnknown));
    assert!(core.is_isolated());
}

#[test]
fn stopped_native_submission_fences_other_roots_until_explicit_recovery() {
    for (outcome, at, expected_submitted) in [
        (SubmitOutcome::Applied, Some(3), true),
        (SubmitOutcome::Rejected, None, false),
        (SubmitOutcome::Unknown, Some(3), true),
    ] {
        let (mut core, intent) = ready();
        core.register_root(&RootGrant {
            root_id: "other-root".into(),
            revision: 1,
            root_epoch: 1,
            deadline_ms: 5000,
        })
        .unwrap();
        let other_permit = core
            .grant_permit(&GrantPermit {
                acked_stop_seq: 0,
                task_id: "other-root".into(),
                target_ref: "target".into(),
                evidence_id: "obs".into(),
                valid_until_ms: 1000,
            })
            .unwrap();

        submit(&mut core, &intent);
        let mut queued = intent.clone();
        queued.intent_id = "queued-other-root".into();
        queued.task_id = "other-root".into();
        queued.permit = other_permit.clone();
        assert_eq!(
            core.submit(SubmitIntent {
                acked_stop_seq: 0,
                intent: queued.clone().into()
            })
            .status,
            ReceiptStatus::Accepted
        );

        let stop_ack = core.stop(&StopCommand {
            scope: StopScope::All,
            disposition: StopDisposition::Pause,
            source: StopSource::HumanInput,
            reason: "human input during native submission".into(),
        });
        assert_eq!(receipt(&core).status, ReceiptStatus::Unknown);
        let stopped_reason = receipt(&core).reason.clone();
        assert!(core.is_isolated());
        assert_eq!(
            core.query_receipt(1, "queued-other-root").unwrap().status,
            ReceiptStatus::Cancelled,
            "queued but unsubmitted work stays cancelled, not unknown"
        );
        assert!(core.recover_channel().is_err(), "in-flight blocks recovery");

        assert!(
            core.resume(&ResumeCommand {
                scope: StopScope::All,
            })
            .resumed
        );
        assert!(
            core.grant_permit(&GrantPermit {
                acked_stop_seq: stop_ack.stop_floor,
                task_id: "other-root".into(),
                target_ref: "target".into(),
                evidence_id: "obs".into(),
                valid_until_ms: 1000,
            })
            .is_err()
        );
        let mut after_stop = queued.clone();
        after_stop.intent_id = "after-stop".into();
        let rejected = core.submit(SubmitIntent {
            acked_stop_seq: stop_ack.stop_floor,
            intent: after_stop.into(),
        });
        assert_eq!(rejected.status, ReceiptStatus::Rejected);
        assert!(rejected.reason.unwrap().contains("input channel isolated"));

        complete(&mut core, outcome, at);
        core.advance_to(10).unwrap();
        let original = receipt(&core);
        assert_eq!(original.status, ReceiptStatus::Unknown);
        assert_eq!(original.reason, stopped_reason);
        assert_eq!(!original.submitted.is_empty(), expected_submitted);
        if expected_submitted {
            assert_eq!(original.submitted[0].at_ms(), 3);
        }
        assert_eq!(
            core.query_receipt(1, "queued-other-root").unwrap().status,
            ReceiptStatus::Cancelled
        );
        assert!(
            core.is_isolated(),
            "late {outcome:?} cannot clear isolation"
        );
        assert!(core.dispatch_next().is_none());
        assert_eq!(core.backend().requests.len(), 1);

        // The only clearing path is an explicit trusted recovery after the
        // pending native outcome has been accounted and any owed input released.
        core.recover_channel().unwrap();
        assert!(!core.is_isolated());
    }
}
#[test]
fn command_end_cannot_invent_completion_while_ack_is_missing() {
    let (mut core, intent) = ready();
    submit(&mut core, &intent);
    core.advance_to(120).unwrap();
    assert_eq!(receipt(&core).status, ReceiptStatus::Running);
    assert!(receipt(&core).submitted.is_empty());
    complete(&mut core, SubmitOutcome::Applied, Some(99));
    core.advance_to(121).unwrap();
    assert_eq!(receipt(&core).status, ReceiptStatus::Completed);
    assert_eq!(core.backend().requests.len(), 1);
}
#[test]
fn late_ack_cannot_complete_or_submit_remaining_steps_past_fixed_end() {
    let (mut core, mut intent) = ready();
    intent.steps.push(ScheduledPrimitive {
        offset_ms: 10,
        primitive: press(),
    });
    submit(&mut core, &intent);
    complete(&mut core, SubmitOutcome::Applied, Some(50));
    core.advance_to(120).unwrap();
    assert_eq!(receipt(&core).status, ReceiptStatus::Partial);
    assert_eq!(core.backend().requests.len(), 1);
}
#[test]
fn unknown_transport_reply_never_replays_and_requires_explicit_recovery() {
    let (mut core, intent) = ready();
    submit(&mut core, &intent);
    complete(&mut core, SubmitOutcome::Unknown, None);
    core.advance_to(20).unwrap();
    assert_eq!(receipt(&core).status, ReceiptStatus::Unknown);
    assert!(core.is_isolated());
    assert!(receipt(&core).submitted.is_empty());
    assert!(core.dispatch_next().is_none());
    assert_eq!(
        core.submit(SubmitIntent {
            acked_stop_seq: 0,
            intent: intent.into(),
        })
        .status,
        ReceiptStatus::Unknown
    );
    core.recover_channel().unwrap();
    assert_eq!(core.backend().requests.len(), 1);
}
#[test]
fn wrong_epoch_intent_sequence_or_commit_window_cannot_settle_submission() {
    for bad in 0..5 {
        let (mut core, intent) = ready();
        submit(&mut core, &intent);
        let mut reply = SubmitCompletion {
            sequence: 1,
            session_epoch: 1,
            intent_id: "one".into(),
            outcome: SubmitOutcome::Applied,
            submitted_at_ms: Some(5),
            input_end_ms: None,
            failure_code: None,
        };
        match bad {
            0 => reply.session_epoch = 2,
            1 => reply.intent_id = "other".into(),
            2 => reply.sequence = 2,
            3 => reply.submitted_at_ms = Some(100),
            _ => reply.submitted_at_ms = None,
        }
        core.backend_mut().replies.push_back(reply);
        core.advance_to(20).unwrap();
        assert_eq!(receipt(&core).status, ReceiptStatus::Unknown);
        assert!(core.is_isolated());
        assert!(receipt(&core).submitted.is_empty());
        assert!(core.recover_channel().is_err());
    }
}
#[test]
fn future_timestamp_waits_for_the_shell_clock_without_extending_authority() {
    let (mut core, intent) = ready();
    submit(&mut core, &intent);
    complete(&mut core, SubmitOutcome::Applied, Some(21));
    core.advance_to(20).unwrap();
    assert!(receipt(&core).submitted.is_empty());
    assert!(!core.is_isolated());
    assert_eq!(core.next_wakeup(), Some(21));
    core.advance_to(21).unwrap();
    assert_eq!(receipt(&core).submitted[0].at_ms(), 21);
}
#[test]
fn reconnect_keeps_old_epoch_result_accountable_but_never_replays() {
    let (mut core, intent) = ready();
    submit(&mut core, &intent);
    assert_eq!(core.reconnect(), 2);
    complete(&mut core, SubmitOutcome::Applied, Some(2));
    core.advance_to(10).unwrap();
    assert_eq!(receipt(&core).status, ReceiptStatus::Unknown);
    assert_eq!(receipt(&core).submitted[0].at_ms(), 2);
    assert!(core.is_isolated());
    assert_eq!(core.backend().requests.len(), 1);
}

#[test]
fn missing_ack_has_a_fixed_core_bound_and_closes_the_transport() {
    let (mut core, intent) = ready();
    submit(&mut core, &intent);
    core.advance_to(599).unwrap();
    assert!(!core.is_isolated());
    core.advance_to(600).unwrap();
    assert_eq!(receipt(&core).status, ReceiptStatus::Unknown);
    assert!(core.is_isolated());
    assert!(core.backend().abandoned);
    assert!(core.dispatch_next().is_none());
    assert_eq!(core.backend().requests.len(), 1);
}

#[test]
fn native_rejection_keeps_the_specific_recovery_reason() {
    let (mut core, intent) = ready();
    submit(&mut core, &intent);
    core.backend_mut().replies.push_back(SubmitCompletion {
        sequence: 1,
        session_epoch: 1,
        intent_id: "one".into(),
        outcome: SubmitOutcome::Rejected,
        submitted_at_ms: None,
        input_end_ms: None,
        failure_code: Some(FailureCode::TargetOccluded),
    });
    core.advance_to(20).unwrap();
    assert_eq!(receipt(&core).status, ReceiptStatus::Rejected);
    assert_eq!(
        receipt(&core).reason_code,
        Some(FailureCode::TargetOccluded)
    );
    assert!(receipt(&core).submitted.is_empty());
}

#[test]
fn actual_input_end_waits_for_clock_and_is_distinct_from_authority_end() {
    let (mut core, intent) = ready();
    submit(&mut core, &intent);
    complete(&mut core, SubmitOutcome::Applied, Some(7));
    core.backend_mut().replies.back_mut().unwrap().input_end_ms = Some(23);
    core.advance_to(20).unwrap();
    assert!(receipt(&core).submitted.is_empty());
    assert_eq!(core.next_wakeup(), Some(23));
    core.advance_to(23).unwrap();
    assert_eq!(receipt(&core).input_end_ms, Some(23));
    assert_eq!(receipt(&core).command_end_ms, Some(100));
    core.advance_to(100).unwrap();
    assert_eq!(receipt(&core).status, ReceiptStatus::Completed);
    assert!(receipt(&core).validate().is_ok());
}

#[test]
fn input_end_before_commit_cannot_settle_the_channel() {
    let (mut core, intent) = ready();
    submit(&mut core, &intent);
    complete(&mut core, SubmitOutcome::Applied, Some(7));
    core.backend_mut().replies.back_mut().unwrap().input_end_ms = Some(6);
    core.advance_to(20).unwrap();
    assert_eq!(receipt(&core).status, ReceiptStatus::Unknown);
    assert!(receipt(&core).input_end_ms.is_none());
    assert!(core.is_isolated());
}

#[test]
fn future_input_end_cannot_extend_the_fixed_reply_bound() {
    let (mut core, intent) = ready();
    submit(&mut core, &intent);
    complete(&mut core, SubmitOutcome::Applied, Some(7));
    core.backend_mut().replies.back_mut().unwrap().input_end_ms = Some(u64::MAX);
    core.advance_to(20).unwrap();
    assert_eq!(receipt(&core).status, ReceiptStatus::Unknown);
    assert!(core.is_isolated());
    assert_eq!(core.backend().requests.len(), 1);
}
