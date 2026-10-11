//! Registered reference workflow: real source/device, generic skills, frozen
//! UI-revisit goal and persisted Verification. No cloud/model success signals.
#[path = "support/native_session.rs"]
mod support;
use interaction_contracts::desktop::{
    binding::BindingProof,
    device::*,
    effect::EffectClass,
    ids::*,
    message::Message,
    planning::{Dependency, GoalDraft, PlanBundle, Subgoal},
    task::*,
    verification::{EffectStatus, TriState},
};
use interaction_engine::{
    environment::{
        binding::{Binding, BindingReducer},
        pack::{AppPack, EffectRule},
        state_core::StateCore,
    },
    execution::DeviceView,
    infrastructure::storage::Storage,
    reaction::skills::{
        ActionOutcome, ActionPlan, Skill, SkillResult, SkillRun, SkillStep, SkillView,
    },
    tasks::{
        budget::BudgetKind,
        runtime::{ConfirmOutcome, TaskConfig, TaskRuntime},
    },
    verification::{EvidenceContext, Verification, VerificationStore},
};
use source_macos::{
    config::{NativeCaptureMode, NativeConfig, TextReadPolicy},
    native::watch::NativeWatch,
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use support::Session;

fn read_source(
    session: &mut Session,
    watch: &mut NativeWatch,
    config: &NativeConfig,
    state: &mut StateCore,
    pack: &AppPack,
    floor: u64,
    page: Option<&str>,
) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut restarts = 0;
    loop {
        session.pump()?;
        let now = session.clock.now_ms();
        for report in watch.take_reports() {
            for health in report.health {
                let _ = state.note_health(health, now);
            }
            if let Some(mut snapshot) = report.snapshot {
                // Match cu-source's session sequence: each helper has its own
                // sequence, but StateCore's snapshot ordering spans restarts.
                // Keep observed_ms, snapshot identity and frame provenance.
                snapshot.source_sequence = state
                    .latest()
                    .map_or(0, |env| env.snapshot.source_sequence)
                    .checked_add(1)
                    .ok_or("source snapshot sequence exhausted")?;
                state
                    .admit(snapshot, now, pack)
                    .map_err(|error| format!("source snapshot rejected: {error:?}"))?;
            }
            if let Some(frame) = report.frame_meta {
                let _ = state.admit_frame(frame, now, 1500);
            }
        }
        if state.usable(now, 1500)
            && state
                .frame_deadline(now, 1500)
                .is_some_and(|end| end >= now.saturating_add(400))
            && state.latest().is_some_and(|e| {
                e.snapshot.observed_ms >= floor
                    && e.snapshot.observed_ms.saturating_add(1500) >= now.saturating_add(400)
                    && page.is_none_or(|p| e.page.as_deref() == Some(p))
            })
        {
            return Ok(());
        }
        // A capture rejected across a UI change cannot be reused. Start a
        // bounded new source session, retaining the published failure floor.
        // This never retries an input or promotes the rejected pixels.
        if watch.ended() && restarts < 3 {
            restarts += 1;
            *watch =
                NativeWatch::new(config.clone(), 1, "macos/1/wall".into(), session.clock, 250)?;
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "source evidence unavailable: {:?}, AX {:?}, screenshot {:?}",
                state
                    .latest()
                    .map(|e| (&e.page, e.snapshot.complete, e.snapshot.observed_ms)),
                state.health(interaction_contracts::desktop::source::SourceKind::Ax),
                state.health(interaction_contracts::desktop::source::SourceKind::Screenshot)
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn bind_document(
    state: &StateCore,
    bindings: &mut BindingReducer,
    target: &SemanticTargetRef,
) -> Result<Binding, String> {
    let env = state.latest().ok_or("no current source")?;
    let object = env
        .snapshot
        .objects
        .iter()
        .find(|o| {
            o.role == "AXTextField" && o.title.as_deref() == Some("MVP document") && !o.secure
        })
        .ok_or("registered document field not observed")?;
    bindings
        .bind_instance(env, target.clone(), &object.instance_ref)
        .ok_or("cannot bind document".into())
}
fn action(
    session: &mut Session,
    view: &DeviceView,
    state: &StateCore,
    plan: &ActionPlan,
    goal: &GoalSpec,
    index: u64,
) -> Result<Receipt, String> {
    let now = session.clock.now_ms();
    let env = state.latest().ok_or("no action evidence")?;
    let source_end = state
        .frame_deadline(now, 1500)
        .ok_or("fresh captured frame required")?;
    let evidence = view
        .bound_observation(
            env,
            &plan.binding,
            SourceFreshness::Verified,
            1500,
            goal.budget.deadline_ms,
            Some(source_end),
        )
        .map_err(str::to_string)?;
    session.send_ack(Message::Observation(ExecutionObservation::Native {
        observation: Box::new(evidence.clone()),
    }))?;
    session
        .host
        .send(Message::GrantPermit(GrantPermit {
            acked_stop_seq: 0,
            task_id: goal.root_task_id.as_str().into(),
            target_ref: evidence.target_ref.clone(),
            evidence_id: evidence.observation_id.clone(),
            valid_until_ms: evidence.valid_until_ms,
        }))
        .map_err(|e| e.to_string())?;
    let Message::GrantOutcome(GrantOutcome::Granted { permit }) =
        session.wait(|m| matches!(m, Message::GrantOutcome(_)))?
    else {
        return Err("workflow permit rejected".into());
    };
    let intent = ActionIntent {
        intent_id: format!("workflow-{index}"),
        attempt_id: Some(format!("native-attempt-{index}")),
        task_id: goal.root_task_id.as_str().into(),
        task_revision: goal.task_revision,
        root_epoch: 1,
        session_epoch: 1,
        execution_domain: evidence.execution_domain,
        permit,
        surface: evidence.surface.clone(),
        target_ref: evidence.target_ref.clone(),
        binding_fingerprint: Some(plan.binding.fingerprint_digest.clone()),
        owned_document_proof: None,
        binding_proof: evidence.binding_proof.clone(),
        coordinate_space: CoordinateSpace::DisplayPoints,
        dependencies: evidence.dependencies,
        evidence_id: evidence.observation_id,
        latest_start_ms: now + 150,
        duration_ms: 250,
        steps: vec![ScheduledPrimitive {
            offset_ms: 0,
            primitive: plan.primitive.clone(),
        }],
    };
    session
        .host
        .send(Message::SubmitIntent(SubmitIntent {
            acked_stop_seq: 0,
            intent: ExecutionIntent::Native {
                intent: Box::new(intent.clone()),
            },
        }))
        .map_err(|e| e.to_string())?;
    let Message::Receipt(receipt)=session.wait(|m|matches!(m,Message::Receipt(r) if r.intent_id==intent.intent_id && matches!(r.status,ReceiptStatus::Completed|ReceiptStatus::Unknown|ReceiptStatus::Rejected|ReceiptStatus::Cancelled|ReceiptStatus::Partial)))? else{unreachable!()};
    if receipt.status != ReceiptStatus::Completed
        || receipt.submitted.len() != 1
        || receipt.submitted[0].native_primitive() != Some(&intent.steps[0].primitive)
        || receipt.submitted[0].cleanup()
        || receipt.input_end_ms.is_none()
    {
        return Err(format!(
            "workflow input not established: {:?} {:?}",
            receipt.status, receipt.reason
        ));
    }
    if std::fs::metadata(session.dir.join("world.log")).is_ok_and(|m| m.len() > 0) {
        return Err("workflow routed through simulation".into());
    }
    Ok(receipt)
}
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 5 {
        return Err("expected device/guard/source/initial proof".into());
    }
    let proof: BindingProof =
        serde_json::from_slice(&std::fs::read(&args[4]).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let (mut session, identity) = Session::start(&args[1], &args[2], &proof)?;
    let Message::DeviceEvent(DeviceEvent{event:DeviceEventKind::CapabilityChanged{manifest},..})=session.wait(|m|matches!(m,Message::DeviceEvent(DeviceEvent{event:DeviceEventKind::CapabilityChanged{manifest},..}) if manifest.input==CapabilityState::Available))? else{unreachable!()};
    let mut view = DeviceView::new();
    let execution_domain = manifest.execution_domain;
    view.install_manifest(manifest);
    view.note_lease(true);
    let config = NativeConfig {
        table_scope: None,
        background_target: None,
        owned_document_bootstrap: None,
        helper_path: PathBuf::from(&args[3]),
        bundle_id: proof.fingerprint.app.clone(),
        pid: Some(proof.fingerprint.app_pid),
        assets_dir: session.dir.join("source-assets"),
        timeout_ms: 3000,
        mode: NativeCaptureMode::Watch,
        text_policy: Some(TextReadPolicy::RegisteredEditor {
            window_id: proof.fingerprint.window_id.clone(),
        }),
    };
    let mut watch = NativeWatch::new(
        config.clone(),
        1,
        identity.clock_domain.clone(),
        session.clock,
        250,
    )?;
    let mut state = StateCore::new();
    state.set_identity(&identity, true);
    let mut bindings = BindingReducer::new();
    let mut pack = AppPack::observe_only(&proof.fingerprint.app);
    pack.name = "Computer Use Guard Fixture".into();
    pack.nav_role = "AXRadioButton".into();
    pack.rules = vec![
        EffectRule {
            role: Some("AXTextField".into()),
            label_contains: "mvp document".into(),
            class: EffectClass::ReversibleEdit,
        },
        EffectRule {
            role: Some("AXRadioButton".into()),
            label_contains: "".into(),
            class: EffectClass::Navigation,
        },
    ];
    read_source(
        &mut session,
        &mut watch,
        &config,
        &mut state,
        &pack,
        0,
        Some("Document"),
    )?;
    let target = SemanticTargetRef::new("document").map_err(|e| e.to_string())?;
    let desired = ExpectedValue::Text {
        value: "MVP 中文\nHello 👋".into(),
    };
    let mut storage =
        Storage::open(&session.dir.join("workflow.sqlite")).map_err(|e| e.to_string())?;
    let mut tasks = TaskRuntime::new(TaskConfig::new(vec![pack.clone()]));
    tasks
        .create_root(
            &mut storage,
            session.clock.now_ms(),
            "Fill document and return to verify the UI value",
        )
        .map_err(|e| e.to_string())?;
    let draft = GoalDraft {
        app: pack.bundle_id.clone(),
        account: None,
        target: TargetSpec {
            row: None,
            semantic_target: target.clone(),
            label: "MVP document".into(),
            group: Some("Fixture group".into()),
            role_hint: Some("AXTextField".into()),
        },
        desired: desired.clone(),
        confirmation_scope: ConfirmationScope::UiRevisit,
        max_effect_class: EffectClass::ReversibleEdit,
    };
    let plan = PlanBundle {
        subgoals: vec![
            Subgoal {
                select: None,
                id: "fill".into(),
                description: "enter the specified text".into(),
                skill: "ui.enter_text".into(),
                target: Some(target.clone()),
                expects: vec!["value".into()],
                scroll: None,
            },
            Subgoal {
                select: None,
                id: "revisit".into(),
                description: "leave and revisit document".into(),
                skill: "ui.revisit".into(),
                target: Some(target.clone()),
                expects: vec!["revisited".into()],
                scroll: None,
            },
        ],
        dependencies: vec![Dependency {
            before: "fill".into(),
            after: "revisit".into(),
        }],
        required_observations: vec![],
        expected_effects: vec!["goal".into()],
        recovery_branches: vec![],
        termination: vec!["goal.required".into()],
    };
    let confirmation = tasks
        .accept_draft(
            &mut storage,
            session.clock.now_ms(),
            &draft,
            execution_domain,
            &plan,
        )
        .map_err(|e| format!("{e:?}"))?;
    let ConfirmOutcome::GoalFrozen { goal, grant } = tasks
        .answer_confirmation(
            &mut storage,
            session.clock.now_ms(),
            &confirmation.confirmation_id,
            true,
        )
        .map_err(|e| e.to_string())?
    else {
        return Err("goal did not freeze".into());
    };
    tasks
        .admission_passed(&mut storage, session.clock.now_ms())
        .map_err(|e| e.to_string())?;
    session.send_ack(Message::RegisterRoot(grant))?;
    let binding = bind_document(&state, &mut bindings, &target)?;
    let mut verification = Verification::new(session.clock.now_ms());
    verification
        .visits
        .set_target_page(state.latest().unwrap().page.clone());
    verification.note_baseline(verification.evaluate(
        &goal.required,
        &EvidenceContext {
            state: &state,
            bindings: &bindings,
            pack: &pack,
            goal: &goal,
            now_ms: session.clock.now_ms(),
            max_age_ms: 2000,
        },
    ));
    let mut fill = SkillRun::new(
        Skill::EnterText,
        target.clone(),
        Some(desired),
        binding.binding_ref,
    );
    let SkillStep::Act(plan) = fill.advance(&mut SkillView {
        state: &state,
        bindings: &mut bindings,
        pack: &pack,
        now_ms: session.clock.now_ms(),
        max_age_ms: 2000,
    }) else {
        return Err("text skill did not request registered input".into());
    };
    tasks
        .spend(&mut storage, BudgetKind::Attempt, 1)
        .map_err(|e| e.to_string())?;
    let receipt = action(&mut session, &view, &state, &plan, &goal, 1)?;
    let end = receipt.input_end_ms.unwrap();
    verification.record_input(receipt.submitted[0].at_ms(), end);
    verification.visits.arm(end);
    fill.acted(ActionOutcome::Completed { end_ms: end });
    let mut receipts = vec![receipt];
    // Receipt success is insufficient: the frozen predicate still lacks revisit.
    let before = verification.evaluate(
        &goal.required,
        &EvidenceContext {
            state: &state,
            bindings: &bindings,
            pack: &pack,
            goal: &goal,
            now_ms: session.clock.now_ms(),
            max_age_ms: 2000,
        },
    );
    if before == TriState::True {
        return Err("old evidence proved a new effect".into());
    }
    read_source(
        &mut session,
        &mut watch,
        &config,
        &mut state,
        &pack,
        end,
        Some("Document"),
    )?;
    let binding = bind_document(&state, &mut bindings, &target)?;
    // Missing observer lifetime coverage requires a new binding. A new read
    // skill consumes it; the old attempt is never silently rebound or replayed.
    let mut readback = SkillRun::new(
        Skill::ReadValue,
        target.clone(),
        None,
        binding.binding_ref.clone(),
    );
    if readback.advance(&mut SkillView {
        state: &state,
        bindings: &mut bindings,
        pack: &pack,
        now_ms: session.clock.now_ms(),
        max_age_ms: 2000,
    }) != SkillStep::Done(SkillResult::Succeeded)
    {
        return Err("fresh bound value is unreadable".into());
    }
    let mut revisit = SkillRun::new(Skill::Revisit, target.clone(), None, binding.binding_ref);
    for (index, page) in [(2, "Other"), (3, "Document")] {
        let SkillStep::Act(plan) = revisit.advance(&mut SkillView {
            state: &state,
            bindings: &mut bindings,
            pack: &pack,
            now_ms: session.clock.now_ms(),
            max_age_ms: 2000,
        }) else {
            return Err("revisit skill did not request registered navigation".into());
        };
        tasks
            .spend(&mut storage, BudgetKind::Attempt, 1)
            .map_err(|e| e.to_string())?;
        let receipt = action(&mut session, &view, &state, &plan, &goal, index)?;
        let end = receipt.input_end_ms.unwrap();
        verification.record_input(receipt.submitted[0].at_ms(), end);
        revisit.acted(ActionOutcome::Completed { end_ms: end });
        receipts.push(receipt);
        read_source(
            &mut session,
            &mut watch,
            &config,
            &mut state,
            &pack,
            end,
            Some(page),
        )?;
        if page == "Other"
            && state
                .latest()
                .unwrap()
                .snapshot
                .objects
                .iter()
                .any(|o| o.role == "AXTextField" && o.title.as_deref() == Some("MVP document"))
        {
            return Err("document field remained exposed on the other page".into());
        }
        verification.visits.observe(
            state.latest().unwrap().page.as_deref(),
            state.latest().unwrap().snapshot.observed_ms,
        );
    }
    let rebound = bind_document(&state, &mut bindings, &target)?;
    verification
        .visits
        .note_bound(rebound.observed_ms, state.latest().unwrap().page.as_deref());
    if revisit.advance(&mut SkillView {
        state: &state,
        bindings: &mut bindings,
        pack: &pack,
        now_ms: session.clock.now_ms(),
        max_age_ms: 2000,
    }) != SkillStep::Done(SkillResult::Succeeded)
    {
        return Err("revisit failed".into());
    }
    let context = EvidenceContext {
        state: &state,
        bindings: &bindings,
        pack: &pack,
        goal: &goal,
        now_ms: session.clock.now_ms(),
        max_age_ms: 2000,
    };
    let verdict = verification.verdict(&context);
    if verdict.status != TriState::True {
        return Err(format!("goal predicates unverified: {:?}", verdict.needs));
    }
    let effect = verification
        .conclude(
            &mut storage,
            &goal.root_task_id,
            &context,
            &verdict,
            None,
            session.clock.now_ms(),
        )
        .map_err(|e| e.to_string())?
        .ok_or("no immutable effect result")?;
    if effect.status != EffectStatus::Satisfied {
        return Err("effect not satisfied".into());
    }
    if VerificationStore::latest(storage.read(), goal.root_task_id.as_str())
        .map_err(|e| e.to_string())?
        != Some(effect.clone())
    {
        return Err("immutable effect result was not persisted".into());
    }
    let status = tasks
        .predicates_satisfied(&mut storage, session.clock.now_ms())
        .map_err(|e| e.to_string())?;
    if status != RootState::Completed {
        return Err("root did not complete".into());
    }
    println!(
        "{}",
        serde_json::json!({"kind":"workflow_result","root_state":status,"effect_status":effect.status,"causality":effect.causality,"effect_result":effect,"immutable_effect_persisted":true,"fresh_bound_read_skill_verified":true,"receipts":receipts,"source_health":state.observation_view(session.clock.now_ms()).map(|v|v.sources),"old_evidence_rejected":true,"ui_revisit_verified":verification.visits.satisfied(),"source_frames_required":state.requires_frames(),"simulated_input_used":false,"native_actions":3,"live_models_used":false,"effect_evidence_count":effect.evidence_group.len()})
    );
    drop(watch);
    Ok(())
}
