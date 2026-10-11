//! A browser action runs through the same execution core as native input, but
//! keeps its evidence and submitted operation in a separate closed type.

use std::collections::{BTreeMap, VecDeque};

use execution_core::backend::{
    BrowserSubmitContext, HeldState, HitTestOutcome, HitTestRequest, InputBackend, ReleaseFailure,
    SubmitCompletion, SubmitContext, SubmitOutcome,
};
use execution_core::core::{CoreConfig, ExecutionCore};
use interaction_contracts::desktop::browser::{
    BrowserCheckboxBinding, BrowserObservedBool, BrowserPageNode, BrowserPageSnapshot,
    BrowserReadScope, BrowserTargetContext, browser_target_context_hash,
    derive_browser_checkbox_binding,
};
use interaction_contracts::desktop::browser_execution::{
    BrowserAction, BrowserActionIntent, BrowserExecutionObservation,
};
use interaction_contracts::desktop::device::{
    ActionIntent, CoordinateSpace, ExecutionIntent, GrantPermit, PermitRef, ReceiptStatus,
    RootGrant, ScheduledPrimitive, SourceFreshness, StopCommand, StopDisposition, StopScope,
    StopSource, SubmitIntent, SurfaceRef,
};
use interaction_contracts::desktop::domain::ExecutionDomain;
use interaction_contracts::desktop::ids::BindingRef;
use interaction_contracts::desktop::primitive::{Modifiers, Primitive};

const FIXTURE_URL: &str = "file:///tmp/computer-use/fixtures/headless/readonly-page.html";

struct BrowserBackend {
    outcome: SubmitOutcome,
    requests: Vec<(String, BrowserAction)>,
    replies: VecDeque<SubmitCompletion>,
    revoked: Vec<PermitRef>,
    native_calls: usize,
    abandoned: bool,
}

impl Default for BrowserBackend {
    fn default() -> Self {
        Self {
            outcome: SubmitOutcome::Applied,
            requests: Vec::new(),
            replies: VecDeque::new(),
            revoked: Vec::new(),
            native_calls: 0,
            abandoned: false,
        }
    }
}

impl InputBackend for BrowserBackend {
    fn submit(&mut self, _: u64, _: &Primitive, _: &SubmitContext<'_>) -> SubmitOutcome {
        self.native_calls += 1;
        SubmitOutcome::Rejected
    }

    fn submit_browser(
        &mut self,
        _: u64,
        action: &BrowserAction,
        context: &BrowserSubmitContext<'_>,
    ) -> SubmitOutcome {
        assert_eq!(context.authority.permit, context.intent.permit);
        assert_eq!(
            &context.authority.evidence,
            context.intent.evidence.as_ref(),
            "the backend receives the same evidence that entered the core"
        );
        self.requests
            .push((context.intent.intent_id.clone(), *action));
        match self.outcome {
            SubmitOutcome::Pending {
                sequence,
                reply_deadline_ms: _,
            } => SubmitOutcome::Pending {
                sequence,
                reply_deadline_ms: context.authority.valid_until_ms + 500,
            },
            other => other,
        }
    }

    fn poll_completion(&mut self) -> Option<SubmitCompletion> {
        self.replies.pop_front()
    }

    fn abandon_pending(&mut self) {
        self.abandoned = true;
    }

    fn revoke(&mut self, permit: &PermitRef) {
        self.revoked.push(permit.clone());
    }

    fn release(&mut self, _: u64, _: &Primitive) -> Result<(), ReleaseFailure> {
        panic!("browser actions are atomic and never own native held input")
    }

    fn held(&self, _: &str) -> HeldState {
        HeldState::NotHeld
    }

    fn hit_test(&mut self, _: &HitTestRequest<'_>) -> HitTestOutcome {
        panic!("browser actions use browser-scoped revalidation, not native hit testing")
    }
}

type Core = ExecutionCore<BrowserBackend>;

fn context(epoch: u64) -> BrowserTargetContext {
    BrowserTargetContext {
        browser_session_id: format!("browser-session-{epoch}"),
        browser_context_id: format!("browser-context-{epoch}"),
        page_target_id: format!("page-target-{epoch}"),
        cdp_session_id: format!("cdp-session-{epoch}"),
        frame_id: format!("frame-{epoch}"),
        document_loader_id: format!("loader-{epoch}"),
        fixture_file_url: FIXTURE_URL.into(),
        session_epoch: epoch,
    }
}

fn scope(epoch: u64, generation: u64) -> BrowserReadScope {
    BrowserReadScope {
        scope_id: "550e8400-e29b-41d4-a716-446655440000".into(),
        generation,
        context: context(epoch),
        clock_domain: format!("headless/{epoch}/wall"),
        issued_ms: 100,
    }
}

fn page_snapshot(
    scope: &BrowserReadScope,
    sequence: u64,
    observed_ms: u64,
    checked: BrowserObservedBool,
    disabled: BrowserObservedBool,
) -> BrowserPageSnapshot {
    let target_context_hash = browser_target_context_hash(&scope.context).unwrap();
    BrowserPageSnapshot {
        snapshot_id: format!(
            "browser-snapshot-{}-{sequence}",
            scope.context.session_epoch
        ),
        source_sequence: sequence,
        observed_ms,
        read_finished_ms: observed_ms + 5,
        clock_domain: scope.clock_domain.clone(),
        context: scope.context.clone(),
        target_context_hash,
        root_ax_node_id: "root-ax".into(),
        nodes: vec![
            BrowserPageNode {
                ax_node_id: "root-ax".into(),
                backend_dom_node_id: None,
                parent_ax_node_id: None,
                child_ax_node_ids: vec!["checkbox-ax".into()],
                ignored: false,
                role: Some("RootWebArea".into()),
                name: Some("Owned Headless Source Fixture".into()),
                disabled: BrowserObservedBool::Unknown,
                checked: BrowserObservedBool::Unknown,
            },
            BrowserPageNode {
                ax_node_id: "checkbox-ax".into(),
                backend_dom_node_id: Some(17),
                parent_ax_node_id: Some("root-ax".into()),
                child_ax_node_ids: Vec::new(),
                ignored: false,
                role: Some("checkbox".into()),
                name: Some("Background option".into()),
                disabled,
                checked,
            },
        ],
    }
}

fn evidence(
    scope: &BrowserReadScope,
    sequence: u64,
    observed_ms: u64,
    checked: BrowserObservedBool,
    disabled: BrowserObservedBool,
    valid_until_ms: u64,
) -> Result<BrowserExecutionObservation, String> {
    let snapshot = page_snapshot(scope, sequence, observed_ms, checked, disabled);
    let binding: BrowserCheckboxBinding =
        derive_browser_checkbox_binding(&snapshot, "Background option")?;
    let observation = BrowserExecutionObservation {
        observation_id: snapshot.snapshot_id,
        surface: SurfaceRef {
            id: scope.context.page_target_id.clone(),
            epoch: scope.context.session_epoch,
        },
        scope: Box::new(scope.clone()),
        environment_revision: sequence,
        binding_ref: BindingRef::new(format!(
            "browser-binding-{}-{sequence}",
            scope.context.session_epoch
        ))
        .unwrap(),
        binding: Box::new(binding),
        valid_until_ms,
        freshness: SourceFreshness::Verified,
        dependencies: BTreeMap::from([
            ("authorization".into(), 1),
            ("binding".into(), 1),
            ("browser_scope".into(), 1),
            ("document".into(), 1),
        ]),
    };
    observation.validate()?;
    Ok(observation)
}

fn new_core(epoch: u64, lease: bool, backend: BrowserBackend) -> (Core, BrowserReadScope) {
    new_core_at(epoch, lease, backend, 105)
}

fn new_core_at(
    epoch: u64,
    lease: bool,
    backend: BrowserBackend,
    now_ms: u64,
) -> (Core, BrowserReadScope) {
    let scope = scope(epoch, 1);
    let mut core = Core::new(
        CoreConfig::headless_browser(scope.clone()).unwrap(),
        backend,
    );
    core.advance_to(now_ms).unwrap();
    if lease {
        core.renew_lease(5_000);
    }
    core.register_root(&RootGrant {
        root_id: "root-browser".into(),
        revision: 1,
        root_epoch: 1,
        deadline_ms: 5_000,
    })
    .unwrap();
    (core, scope)
}

fn grant(core: &mut Core, observation: &BrowserExecutionObservation) -> PermitRef {
    core.grant_permit(&GrantPermit {
        acked_stop_seq: core.latch().floor(),
        task_id: "root-browser".into(),
        target_ref: observation.target_ref().into(),
        evidence_id: observation.observation_id.clone(),
        valid_until_ms: 800,
    })
    .unwrap()
}

fn browser_intent(
    core: &Core,
    observation: &BrowserExecutionObservation,
    permit: PermitRef,
    id: &str,
) -> BrowserActionIntent {
    BrowserActionIntent {
        intent_id: id.into(),
        attempt_id: format!("attempt-{id}"),
        task_id: "root-browser".into(),
        task_revision: 1,
        root_epoch: 1,
        session_epoch: core.session_epoch(),
        permit,
        evidence: Box::new(observation.clone()),
        latest_start_ms: observation.binding.read_finished_ms + 10,
        duration_ms: 100,
        action: BrowserAction::SetCheckbox { checked: true },
    }
}

fn submit(
    core: &mut Core,
    intent: BrowserActionIntent,
) -> interaction_contracts::desktop::device::Receipt {
    core.submit(SubmitIntent {
        acked_stop_seq: core.latch().floor(),
        intent: intent.into(),
    })
}

fn pending_browser_submission(epoch: u64) -> (Core, BrowserActionIntent) {
    let backend = BrowserBackend {
        outcome: SubmitOutcome::Pending {
            sequence: 1,
            reply_deadline_ms: 1_500,
        },
        ..BrowserBackend::default()
    };
    let (mut core, scope) = new_core(epoch, true, backend);
    let observation = evidence(
        &scope,
        1,
        100,
        BrowserObservedBool::False,
        BrowserObservedBool::False,
        1_000,
    )
    .unwrap();
    core.observe(observation.clone()).unwrap();
    let permit = grant(&mut core, &observation);
    let intent = browser_intent(&core, &observation, permit, "browser-late-applied");
    assert_eq!(
        submit(&mut core, intent.clone()).status,
        ReceiptStatus::Accepted
    );
    assert_eq!(core.dispatch_next().unwrap().status, ReceiptStatus::Running);
    assert_eq!(core.backend().requests.len(), 1);
    (core, intent)
}

#[test]
fn browser_checkbox_uses_the_shared_permit_queue_and_browser_receipt_variant() {
    let (mut core, scope) = new_core(7, true, BrowserBackend::default());
    let observation = evidence(
        &scope,
        1,
        100,
        BrowserObservedBool::False,
        BrowserObservedBool::False,
        1_000,
    )
    .unwrap();
    core.observe(observation.clone()).unwrap();
    let permit = grant(&mut core, &observation);
    let intent = browser_intent(&core, &observation, permit, "browser-one");

    assert_eq!(
        submit(&mut core, intent.clone()).status,
        ReceiptStatus::Accepted
    );
    let completed = core.dispatch_next().unwrap();
    assert_eq!(completed.status, ReceiptStatus::Completed);
    assert_eq!(completed.input_end_ms, Some(105));
    assert_eq!(
        completed.release_status,
        interaction_contracts::desktop::device::ReleaseStatus::NotNeeded
    );
    assert!(matches!(
        completed.submitted.as_slice(),
        [
            interaction_contracts::desktop::device::SubmittedAction::Browser {
                action: BrowserAction::SetCheckbox { checked: true },
                ..
            }
        ]
    ));
    assert_eq!(
        core.backend().requests,
        vec![(
            "browser-one".into(),
            BrowserAction::SetCheckbox { checked: true }
        )]
    );
    assert_eq!(core.backend().native_calls, 0);
    assert_eq!(core.held_input_count(), 0);

    assert_eq!(submit(&mut core, intent).status, ReceiptStatus::Completed);
    assert_eq!(
        core.backend().requests.len(),
        1,
        "duplicate intent IDs are never replayed"
    );
}

#[test]
fn browser_profile_rejects_native_intents_and_observations() {
    let (mut core, scope) = new_core(7, true, BrowserBackend::default());
    let observation = evidence(
        &scope,
        1,
        100,
        BrowserObservedBool::False,
        BrowserObservedBool::False,
        1_000,
    )
    .unwrap();
    let native_observation = interaction_contracts::desktop::device::Observation {
        observation_id: "native-observation".into(),
        session_epoch: core.session_epoch(),
        surface: core.surface(),
        target_ref: observation.target_ref().into(),
        clock_domain: core.clock_domain(),
        source_sequence: 1,
        observed_ms: 100,
        valid_until_ms: 1_000,
        freshness: SourceFreshness::Verified,
        execution_domain: ExecutionDomain::ForegroundNative,
        purpose: Default::default(),
        binding_proof: None,
        owned_document_proof: None,
        dependencies: core.dependency_versions(),
    };
    assert!(core.observe(native_observation.clone()).is_err());

    let native_intent = ActionIntent {
        intent_id: "native-intent".into(),
        attempt_id: Some("attempt-native".into()),
        task_id: "root-browser".into(),
        task_revision: 1,
        root_epoch: 1,
        session_epoch: core.session_epoch(),
        execution_domain: ExecutionDomain::ForegroundNative,
        permit: PermitRef {
            id: "permit-native".into(),
            generation: 1,
        },
        surface: core.surface(),
        target_ref: observation.target_ref().into(),
        binding_fingerprint: None,
        binding_proof: None,
        owned_document_proof: None,
        coordinate_space: CoordinateSpace::DisplayPoints,
        dependencies: core.dependency_versions(),
        evidence_id: native_observation.observation_id.clone(),
        latest_start_ms: 120,
        duration_ms: 100,
        steps: vec![ScheduledPrimitive {
            offset_ms: 0,
            primitive: Primitive::KeyPress {
                key: "Space".into(),
                modifiers: Modifiers::default(),
            },
        }],
    };
    let receipt = core.submit(SubmitIntent {
        acked_stop_seq: 0,
        intent: ExecutionIntent::from(native_intent),
    });
    assert_eq!(receipt.status, ReceiptStatus::Rejected);
    assert_eq!(core.backend().requests.len(), 0);
    assert_eq!(core.backend().native_calls, 0);
}

#[test]
fn browser_scope_sequence_time_disabled_and_unknown_state_fail_closed() {
    let (mut core, scope) = new_core(7, true, BrowserBackend::default());
    let valid = evidence(
        &scope,
        2,
        200,
        BrowserObservedBool::False,
        BrowserObservedBool::False,
        1_000,
    )
    .unwrap();
    core.advance_to(300).unwrap();
    core.observe(valid).unwrap();
    let older_sequence = evidence(
        &scope,
        1,
        250,
        BrowserObservedBool::False,
        BrowserObservedBool::False,
        1_000,
    )
    .unwrap();
    assert!(
        core.observe(older_sequence)
            .unwrap_err()
            .contains("out-of-order")
    );

    let mut wrong_scope = scope.clone();
    wrong_scope.generation += 1;
    let mismatched = evidence(
        &wrong_scope,
        3,
        300,
        BrowserObservedBool::False,
        BrowserObservedBool::False,
        1_000,
    )
    .unwrap();
    assert!(core.observe(mismatched).is_err());

    let (mut future_core, future_scope) = new_core(8, true, BrowserBackend::default());
    let future = evidence(
        &future_scope,
        1,
        200,
        BrowserObservedBool::False,
        BrowserObservedBool::False,
        1_000,
    )
    .unwrap();
    assert!(future_core.observe(future).unwrap_err().contains("future"));

    let (mut expired_core, expired_scope) = new_core(9, true, BrowserBackend::default());
    expired_core.advance_to(700).unwrap();
    let expired = evidence(
        &expired_scope,
        1,
        100,
        BrowserObservedBool::False,
        BrowserObservedBool::False,
        500,
    )
    .unwrap();
    assert!(expired_core.observe(expired).is_err());

    assert!(
        evidence(
            &scope,
            4,
            400,
            BrowserObservedBool::False,
            BrowserObservedBool::True,
            1_000,
        )
        .is_err()
    );
    assert!(
        evidence(
            &scope,
            4,
            400,
            BrowserObservedBool::Unknown,
            BrowserObservedBool::False,
            1_000,
        )
        .is_err()
    );
}

#[test]
fn newer_read_of_changed_checkbox_state_cancels_queued_browser_action() {
    let (mut core, scope) = new_core(14, true, BrowserBackend::default());
    let observation = evidence(
        &scope,
        1,
        100,
        BrowserObservedBool::False,
        BrowserObservedBool::False,
        1_000,
    )
    .unwrap();
    core.observe(observation.clone()).unwrap();
    let permit = grant(&mut core, &observation);
    let intent = browser_intent(&core, &observation, permit, "changed-state");
    assert_eq!(submit(&mut core, intent).status, ReceiptStatus::Accepted);

    core.advance_to(115).unwrap();
    let changed = evidence(
        &scope,
        2,
        110,
        BrowserObservedBool::True,
        BrowserObservedBool::False,
        1_000,
    )
    .unwrap();
    core.observe(changed).unwrap();

    assert_eq!(
        core.query_receipt(core.session_epoch(), "changed-state")
            .unwrap()
            .status,
        ReceiptStatus::Cancelled
    );
    assert!(core.backend().requests.is_empty());
}

#[test]
fn browser_read_must_finish_before_observation_and_exact_boundary_is_accepted() {
    let (mut early, early_scope) = new_core_at(17, true, BrowserBackend::default(), 104);
    let unfinished = evidence(
        &early_scope,
        1,
        100,
        BrowserObservedBool::False,
        BrowserObservedBool::False,
        1_000,
    )
    .unwrap();
    assert!(
        early
            .observe(unfinished)
            .unwrap_err()
            .contains("future finish")
    );

    let (mut boundary, boundary_scope) = new_core(18, true, BrowserBackend::default());
    let finished = evidence(
        &boundary_scope,
        1,
        100,
        BrowserObservedBool::False,
        BrowserObservedBool::False,
        1_000,
    )
    .unwrap();
    assert_eq!(boundary.now_ms(), finished.binding.read_finished_ms);
    assert!(boundary.observe(finished).is_ok());
}

#[test]
fn browser_actions_require_lease_root_stop_and_current_dependencies() {
    let (mut no_lease, no_lease_scope) = new_core(7, false, BrowserBackend::default());
    let no_lease_observation = evidence(
        &no_lease_scope,
        1,
        100,
        BrowserObservedBool::False,
        BrowserObservedBool::False,
        1_000,
    )
    .unwrap();
    no_lease.observe(no_lease_observation.clone()).unwrap();
    assert!(
        no_lease
            .grant_permit(&GrantPermit {
                acked_stop_seq: 0,
                task_id: "root-browser".into(),
                target_ref: no_lease_observation.target_ref().into(),
                evidence_id: no_lease_observation.observation_id.clone(),
                valid_until_ms: 800,
            })
            .is_err()
    );

    let (mut wrong_root, wrong_root_scope) = new_core(8, true, BrowserBackend::default());
    let wrong_root_observation = evidence(
        &wrong_root_scope,
        1,
        100,
        BrowserObservedBool::False,
        BrowserObservedBool::False,
        1_000,
    )
    .unwrap();
    wrong_root.observe(wrong_root_observation.clone()).unwrap();
    let permit = grant(&mut wrong_root, &wrong_root_observation);
    let mut wrong_revision =
        browser_intent(&wrong_root, &wrong_root_observation, permit, "wrong-root");
    wrong_revision.task_revision += 1;
    assert_eq!(
        submit(&mut wrong_root, wrong_revision).status,
        ReceiptStatus::Rejected
    );
    assert_eq!(wrong_root.backend().requests.len(), 0);

    let (mut paused, paused_scope) = new_core(10, true, BrowserBackend::default());
    let paused_observation = evidence(
        &paused_scope,
        1,
        100,
        BrowserObservedBool::False,
        BrowserObservedBool::False,
        1_000,
    )
    .unwrap();
    paused.observe(paused_observation.clone()).unwrap();
    let paused_permit = grant(&mut paused, &paused_observation);
    let paused_intent = browser_intent(&paused, &paused_observation, paused_permit, "paused");
    paused.stop(&StopCommand {
        scope: StopScope::Root {
            root_id: "root-browser".into(),
        },
        disposition: StopDisposition::Pause,
        source: StopSource::OperatorButton,
        reason: "test stop".into(),
    });
    assert_eq!(
        submit(&mut paused, paused_intent).status,
        ReceiptStatus::Rejected
    );
    assert_eq!(paused.backend().requests.len(), 0);

    let (mut cancelled, cancelled_scope) = new_core(11, true, BrowserBackend::default());
    let cancelled_observation = evidence(
        &cancelled_scope,
        1,
        100,
        BrowserObservedBool::False,
        BrowserObservedBool::False,
        1_000,
    )
    .unwrap();
    cancelled.observe(cancelled_observation.clone()).unwrap();
    let cancelled_permit = grant(&mut cancelled, &cancelled_observation);
    let cancelled_intent = browser_intent(
        &cancelled,
        &cancelled_observation,
        cancelled_permit,
        "cancelled",
    );
    cancelled.stop(&StopCommand {
        scope: StopScope::Root {
            root_id: "root-browser".into(),
        },
        disposition: StopDisposition::Cancel,
        source: StopSource::OperatorButton,
        reason: "test cancel".into(),
    });
    assert_eq!(
        submit(&mut cancelled, cancelled_intent).status,
        ReceiptStatus::Rejected
    );
    assert_eq!(cancelled.backend().requests.len(), 0);

    let (mut changed_dependency, dependency_scope) = new_core(12, true, BrowserBackend::default());
    let dependency_observation = evidence(
        &dependency_scope,
        1,
        100,
        BrowserObservedBool::False,
        BrowserObservedBool::False,
        1_000,
    )
    .unwrap();
    changed_dependency
        .observe(dependency_observation.clone())
        .unwrap();
    let dependency_permit = grant(&mut changed_dependency, &dependency_observation);
    let dependency_intent = browser_intent(
        &changed_dependency,
        &dependency_observation,
        dependency_permit,
        "dependency-change",
    );
    changed_dependency.update_dependency("document", None);
    assert_eq!(
        submit(&mut changed_dependency, dependency_intent).status,
        ReceiptStatus::Rejected
    );
    assert_eq!(changed_dependency.backend().requests.len(), 0);
}

#[test]
fn browser_pending_unknown_isolated_and_duplicate_intent_is_not_replayed() {
    let backend = BrowserBackend {
        outcome: SubmitOutcome::Pending {
            sequence: 1,
            reply_deadline_ms: 1_500,
        },
        ..BrowserBackend::default()
    };
    let (mut core, scope) = new_core(13, true, backend);
    let observation = evidence(
        &scope,
        1,
        100,
        BrowserObservedBool::False,
        BrowserObservedBool::False,
        1_000,
    )
    .unwrap();
    core.observe(observation.clone()).unwrap();
    let permit = grant(&mut core, &observation);
    let intent = browser_intent(&core, &observation, permit, "browser-pending");
    let accepted = submit(&mut core, intent.clone());
    assert_eq!(accepted.status, ReceiptStatus::Accepted);
    assert_eq!(core.dispatch_next().unwrap().status, ReceiptStatus::Running);
    assert_eq!(core.backend().requests.len(), 1);

    let session_epoch = core.session_epoch();
    core.backend_mut().replies.push_back(SubmitCompletion {
        sequence: 1,
        session_epoch,
        intent_id: intent.intent_id.clone(),
        outcome: SubmitOutcome::Unknown,
        submitted_at_ms: None,
        input_end_ms: None,
        failure_code: Some(interaction_contracts::desktop::error::FailureCode::EffectUnknown),
    });
    core.advance_to(106).unwrap();
    assert!(core.is_isolated());
    assert_eq!(
        core.query_receipt(core.session_epoch(), "browser-pending")
            .unwrap()
            .status,
        ReceiptStatus::Unknown
    );
    assert_eq!(submit(&mut core, intent).status, ReceiptStatus::Unknown);
    assert_eq!(core.backend().requests.len(), 1);
}

#[test]
fn malformed_browser_completion_keeps_the_expected_submission_fenced() {
    let (mut core, intent) = pending_browser_submission(19);
    let session_epoch = core.session_epoch();
    core.backend_mut().replies.push_back(SubmitCompletion {
        sequence: 2,
        session_epoch,
        intent_id: intent.intent_id.clone(),
        outcome: SubmitOutcome::Applied,
        submitted_at_ms: Some(105),
        input_end_ms: Some(106),
        failure_code: None,
    });
    core.advance_to(106).unwrap();
    assert_eq!(
        core.query_receipt(core.session_epoch(), &intent.intent_id)
            .unwrap()
            .status,
        ReceiptStatus::Unknown
    );
    assert!(core.recover_channel().is_err());
    assert_eq!(core.backend().requests.len(), 1);

    core.backend_mut().replies.push_back(SubmitCompletion {
        sequence: 1,
        session_epoch,
        intent_id: intent.intent_id.clone(),
        outcome: SubmitOutcome::Applied,
        submitted_at_ms: Some(105),
        input_end_ms: Some(106),
        failure_code: None,
    });
    core.advance_to(107).unwrap();
    let receipt = core
        .query_receipt(core.session_epoch(), &intent.intent_id)
        .unwrap();
    assert_eq!(receipt.status, ReceiptStatus::Unknown);
    assert_eq!(receipt.submitted.len(), 1);
    assert_eq!(submit(&mut core, intent).status, ReceiptStatus::Unknown);
    assert_eq!(core.backend().requests.len(), 1);

    core.recover_channel().unwrap();
    assert!(!core.is_isolated());
}

#[test]
fn malformed_browser_completion_stays_fenced_until_mock_abandon_deadline() {
    let (mut core, intent) = pending_browser_submission(20);
    let session_epoch = core.session_epoch();
    core.backend_mut().replies.push_back(SubmitCompletion {
        sequence: 2,
        session_epoch,
        intent_id: intent.intent_id.clone(),
        outcome: SubmitOutcome::Applied,
        submitted_at_ms: Some(105),
        input_end_ms: Some(106),
        failure_code: None,
    });
    core.advance_to(106).unwrap();
    assert!(core.recover_channel().is_err());
    assert_eq!(
        core.query_receipt(core.session_epoch(), &intent.intent_id)
            .unwrap()
            .status,
        ReceiptStatus::Unknown
    );

    let reply_deadline = 105 + intent.duration_ms + 500;
    core.advance_to(reply_deadline - 1).unwrap();
    assert!(core.recover_channel().is_err());
    assert!(!core.backend().abandoned);

    // BrowserBackend only records the hook call; this does not verify a real
    // CDP pipe was closed.
    core.advance_to(reply_deadline).unwrap();
    assert!(core.backend().abandoned);
    assert_eq!(
        core.query_receipt(core.session_epoch(), &intent.intent_id)
            .unwrap()
            .status,
        ReceiptStatus::Unknown
    );
    assert_eq!(core.backend().requests.len(), 1);

    core.recover_channel().unwrap();
    assert!(!core.is_isolated());
    assert_eq!(submit(&mut core, intent).status, ReceiptStatus::Unknown);
    assert_eq!(core.backend().requests.len(), 1);
}

#[test]
fn malformed_browser_reply_at_deadline_abandons_without_accepting_or_replaying() {
    let (mut core, intent) = pending_browser_submission(21);
    let session_epoch = core.session_epoch();
    core.backend_mut().replies.push_back(SubmitCompletion {
        sequence: 2,
        session_epoch,
        intent_id: intent.intent_id.clone(),
        outcome: SubmitOutcome::Applied,
        submitted_at_ms: Some(105),
        input_end_ms: Some(106),
        failure_code: None,
    });

    let reply_deadline = 105 + intent.duration_ms + 500;
    // The fake backend records only the abandon hook; it does not prove real
    // transport or CDP pipe closure.
    core.advance_to(reply_deadline).unwrap();

    assert!(core.backend().abandoned);
    let receipt = core
        .query_receipt(core.session_epoch(), &intent.intent_id)
        .unwrap();
    assert_eq!(receipt.status, ReceiptStatus::Unknown);
    assert!(receipt.submitted.is_empty());
    assert_eq!(receipt.input_end_ms, None);
    assert_eq!(core.backend().requests.len(), 1);

    core.recover_channel().unwrap();
    assert_eq!(submit(&mut core, intent).status, ReceiptStatus::Unknown);
    assert_eq!(core.backend().requests.len(), 1);
}

#[test]
fn browser_pending_stop_keeps_late_applied_result_unknown_without_replay() {
    let (mut core, intent) = pending_browser_submission(15);
    core.stop(&StopCommand {
        scope: StopScope::Root {
            root_id: "root-browser".into(),
        },
        disposition: StopDisposition::Pause,
        source: StopSource::OperatorButton,
        reason: "stop during pending browser action".into(),
    });
    assert_eq!(
        core.query_receipt(core.session_epoch(), &intent.intent_id)
            .unwrap()
            .status,
        ReceiptStatus::Unknown
    );

    let session_epoch = core.session_epoch();
    core.backend_mut().replies.push_back(SubmitCompletion {
        sequence: 1,
        session_epoch,
        intent_id: intent.intent_id.clone(),
        outcome: SubmitOutcome::Applied,
        submitted_at_ms: Some(105),
        input_end_ms: Some(106),
        failure_code: None,
    });
    core.advance_to(106).unwrap();

    assert_eq!(
        core.query_receipt(core.session_epoch(), &intent.intent_id)
            .unwrap()
            .status,
        ReceiptStatus::Unknown
    );
    assert_eq!(submit(&mut core, intent).status, ReceiptStatus::Unknown);
    assert_eq!(core.backend().requests.len(), 1);
}

#[test]
fn browser_pending_lease_expiry_keeps_late_applied_result_unknown_without_replay() {
    let (mut core, intent) = pending_browser_submission(16);
    core.renew_lease(50);
    core.advance_to(155).unwrap();
    assert_eq!(
        core.query_receipt(core.session_epoch(), &intent.intent_id)
            .unwrap()
            .status,
        ReceiptStatus::Unknown
    );

    let session_epoch = core.session_epoch();
    core.backend_mut().replies.push_back(SubmitCompletion {
        sequence: 1,
        session_epoch,
        intent_id: intent.intent_id.clone(),
        outcome: SubmitOutcome::Applied,
        submitted_at_ms: Some(105),
        input_end_ms: Some(106),
        failure_code: None,
    });
    core.advance_to(156).unwrap();

    assert_eq!(
        core.query_receipt(core.session_epoch(), &intent.intent_id)
            .unwrap()
            .status,
        ReceiptStatus::Unknown
    );
    assert_eq!(submit(&mut core, intent).status, ReceiptStatus::Unknown);
    assert_eq!(core.backend().requests.len(), 1);
}

#[test]
fn noninitial_browser_epoch_is_trusted_at_startup_but_old_scope_dies_on_reconnect() {
    let (mut core, scope) = new_core(27, true, BrowserBackend::default());
    assert_eq!(core.session_epoch(), 27);
    assert_eq!(core.surface().epoch, 27);
    let observation = evidence(
        &scope,
        1,
        100,
        BrowserObservedBool::False,
        BrowserObservedBool::False,
        1_000,
    )
    .unwrap();
    core.observe(observation.clone()).unwrap();

    assert_eq!(core.reconnect(), 28);
    assert_eq!(core.surface().epoch, 28);
    assert!(core.observe(observation).is_err());
}
