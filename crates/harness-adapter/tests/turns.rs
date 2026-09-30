//! The rules of a turn (design 12.4): exactly one valid submission, counted by
//! distinct tool-use id from published results; an identity the model cannot
//! set; isolation of cancelled, expired and late turns.

use harness_adapter::adapter::output_schema;
use harness_adapter::{
    AdapterConfig, BeginError, EventKind, PlannerAdapter, Scope, ScopedEvent, ScriptedTransport,
    TOOL, TurnResult, Usage,
};
use interaction_contracts::desktop::ids::{RequestId, RootTaskId};
use interaction_contracts::desktop::planning::{
    Dependency, PlannerOutput, PlanningContext, PlanningReason, PlanningRequest,
};
use sim_world::policy::ScriptedPlanner;

const TEXT: &str = r#"Turn off "Daily picks" in "Push""#;

fn request(generation: u64, reason: PlanningReason) -> PlanningRequest {
    PlanningRequest {
        request_id: RequestId::new(format!("plan-{generation}")).unwrap(),
        generation,
        root_task_id: RootTaskId::new("root-1").unwrap(),
        task_revision: 1,
        root_epoch: 1,
        reason,
        context: PlanningContext {
            user_text: TEXT.into(),
            unknown_items: vec![],
            allowed_skills: vec!["ui.ensure_value".into()],
            recent_failures: vec![],
            notes: vec![],
        },
        deadline_ms: 10_000,
    }
}

fn adapter() -> PlannerAdapter<ScriptedTransport> {
    PlannerAdapter::new(ScriptedTransport::default(), AdapterConfig::default())
}

/// What the planner would say to a goal-compile request.
fn goal_plan() -> PlannerOutput {
    ScriptedPlanner::default().plan_text(TEXT)
}

/// A plan with no goal, as an escalation may return.
fn plain_plan() -> PlannerOutput {
    match goal_plan() {
        PlannerOutput::Plan { mut proposal } => {
            proposal.goal_draft = None;
            PlannerOutput::Plan { proposal }
        }
        other => other,
    }
}

fn json(output: &PlannerOutput) -> serde_json::Value {
    serde_json::to_value(output).unwrap()
}

fn event(scope: &Scope, kind: EventKind) -> ScopedEvent {
    ScopedEvent::new(scope, kind)
}

fn call(scope: &Scope, id: &str) -> ScopedEvent {
    event(
        scope,
        EventKind::ToolCall {
            tool_use_id: id.into(),
            tool: TOOL.into(),
        },
    )
}

fn result(scope: &Scope, id: &str, accepted: bool, slot: Option<serde_json::Value>) -> ScopedEvent {
    event(
        scope,
        EventKind::ToolResult {
            tool_use_id: id.into(),
            tool: TOOL.into(),
            accepted,
            slot,
        },
    )
}

fn finished(scope: &Scope, complete: bool) -> ScopedEvent {
    event(scope, EventKind::Finished { complete })
}

/// A turn in which the model makes one call that the execution path accepts.
fn one_good_call(
    adapter: &mut PlannerAdapter<ScriptedTransport>,
    scope: &Scope,
    output: &PlannerOutput,
) {
    adapter.on_event(call(scope, "tu-1"));
    adapter.on_event(result(scope, "tu-1", true, Some(json(output))));
    adapter.on_event(finished(scope, true));
}

fn open(reason: PlanningReason) -> (PlannerAdapter<ScriptedTransport>, Scope) {
    let mut adapter = adapter();
    let scope = adapter.begin(&request(1, reason), 0).unwrap();
    (adapter, scope)
}

#[test]
fn one_valid_submission_is_accepted_under_the_identity_the_host_gave() {
    let (mut adapter, scope) = open(PlanningReason::GoalCompile);
    assert_eq!(scope.request_id.as_str(), "plan-1");
    assert_eq!(scope.generation, 1);
    // The call carried the host's context and the schema of the one allowed output.
    let (started_scope, started) = &adapter.transport().started[0];
    assert_eq!(started_scope, &scope);
    assert_eq!(started.context.user_text, TEXT);
    assert_eq!(started.schema, output_schema());
    assert!(started.schema.to_string().contains("goal_draft"));

    assert!(
        adapter.poll(10).is_none(),
        "nothing is decided before the barrier"
    );
    one_good_call(&mut adapter, &scope, &goal_plan());
    match adapter.poll(20).expect("the barrier passed") {
        TurnResult::Proposal {
            scope: got, output, ..
        } => {
            assert_eq!(got, scope);
            assert_eq!(*output, goal_plan());
        }
        other => panic!("{other:?}"),
    }
    assert!(!adapter.is_busy());
    assert!(adapter.poll(30).is_none(), "a result is delivered once");
}

#[test]
fn text_and_unverified_calls_are_never_a_submission() {
    let (mut adapter, scope) = open(PlanningReason::GoalCompile);
    adapter.on_event(event(
        &scope,
        EventKind::Text {
            delta: json(&goal_plan()).to_string(),
        },
    ));
    adapter.on_event(call(&scope, "tu-1"));
    adapter.on_event(finished(&scope, true));
    assert!(matches!(adapter.poll(10), Some(TurnResult::Missing { .. })));
}

#[test]
fn a_result_the_execution_path_rejected_does_not_count_but_a_later_valid_one_does() {
    let (mut adapter, scope) = open(PlanningReason::GoalCompile);
    adapter.on_event(call(&scope, "tu-1"));
    adapter.on_event(result(&scope, "tu-1", false, None));
    adapter.on_event(call(&scope, "tu-2"));
    adapter.on_event(result(&scope, "tu-2", true, Some(json(&goal_plan()))));
    adapter.on_event(finished(&scope, true));
    assert!(adapter.poll(10).unwrap().is_proposal());
}

#[test]
fn two_valid_submissions_conflict_even_when_identical_or_when_the_slot_holds_one_value() {
    let (mut adapter, scope) = open(PlanningReason::GoalCompile);
    let same = json(&goal_plan());
    for id in ["tu-1", "tu-2"] {
        adapter.on_event(call(&scope, id));
        adapter.on_event(result(&scope, id, true, Some(same.clone())));
    }
    // The slot ends up with a single value, the same one both times.
    adapter.on_event(finished(&scope, true));
    match adapter.poll(10).unwrap() {
        TurnResult::Conflict { count, .. } => assert_eq!(count, 2),
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_same_tool_use_id_delivered_twice_counts_once() {
    let (mut adapter, scope) = open(PlanningReason::GoalCompile);
    adapter.on_event(call(&scope, "tu-1"));
    let good = result(&scope, "tu-1", true, Some(json(&goal_plan())));
    adapter.on_event(good.clone());
    adapter.on_event(good);
    adapter.on_event(finished(&scope, true));
    assert!(adapter.poll(10).unwrap().is_proposal());
    assert!(adapter.trace().iter().any(|t| t.kind == "duplicate_result"));

    // A repeat does not hide a second, different id.
    let (mut adapter, scope) = open(PlanningReason::GoalCompile);
    adapter.on_event(call(&scope, "tu-1"));
    let first = result(&scope, "tu-1", true, Some(json(&goal_plan())));
    adapter.on_event(first.clone());
    adapter.on_event(first);
    adapter.on_event(call(&scope, "tu-2"));
    adapter.on_event(result(&scope, "tu-2", true, Some(json(&goal_plan()))));
    adapter.on_event(finished(&scope, true));
    assert!(matches!(
        adapter.poll(10).unwrap(),
        TurnResult::Conflict { count: 2, .. }
    ));
}

#[test]
fn an_id_that_got_two_different_results_cannot_be_attributed() {
    let (mut adapter, scope) = open(PlanningReason::GoalCompile);
    adapter.on_event(call(&scope, "tu-1"));
    adapter.on_event(result(&scope, "tu-1", false, None));
    adapter.on_event(result(&scope, "tu-1", true, Some(json(&goal_plan()))));
    adapter.on_event(finished(&scope, true));
    assert!(matches!(
        adapter.poll(10).unwrap(),
        TurnResult::Quarantined { .. }
    ));
}

#[test]
fn a_result_for_a_call_nobody_saw_isolates_the_turn() {
    let (mut adapter, scope) = open(PlanningReason::GoalCompile);
    adapter.on_event(result(&scope, "ghost", true, Some(json(&goal_plan()))));
    adapter.on_event(finished(&scope, true));
    match adapter.poll(10).unwrap() {
        TurnResult::Quarantined { reason, .. } => assert!(reason.contains("no matching call")),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_tool_outside_the_whitelist_is_denied_and_an_accepted_result_from_one_isolates_the_turn() {
    // A denied call is only traced.
    let (mut adapter, scope) = open(PlanningReason::GoalCompile);
    adapter.on_event(event(
        &scope,
        EventKind::ToolCall {
            tool_use_id: "x".into(),
            tool: "Bash".into(),
        },
    ));
    one_good_call(&mut adapter, &scope, &goal_plan());
    assert!(adapter.poll(10).unwrap().is_proposal());
    assert!(adapter.trace().iter().any(|t| t.kind == "denied_call"));

    // A successful result from an unlisted tool means the gate failed.
    let (mut adapter, scope) = open(PlanningReason::GoalCompile);
    adapter.on_event(event(
        &scope,
        EventKind::ToolCall {
            tool_use_id: "x".into(),
            tool: "Bash".into(),
        },
    ));
    adapter.on_event(event(
        &scope,
        EventKind::ToolResult {
            tool_use_id: "x".into(),
            tool: "Bash".into(),
            accepted: true,
            slot: None,
        },
    ));
    one_good_call(&mut adapter, &scope, &goal_plan());
    assert!(matches!(
        adapter.poll(10).unwrap(),
        TurnResult::Quarantined { .. }
    ));
}

#[test]
fn a_slot_that_changes_is_not_a_submission_and_one_that_changes_afterwards_isolates_the_turn() {
    let (mut adapter, scope) = open(PlanningReason::GoalCompile);
    adapter.on_event(event(
        &scope,
        EventKind::SlotChanged {
            slot: Some(json(&goal_plan())),
        },
    ));
    adapter.on_event(finished(&scope, true));
    assert!(
        matches!(adapter.poll(10).unwrap(), TurnResult::Missing { .. }),
        "a value in the slot is not a valid submission"
    );

    let (mut adapter, scope) = open(PlanningReason::GoalCompile);
    adapter.on_event(call(&scope, "tu-1"));
    adapter.on_event(result(&scope, "tu-1", true, Some(json(&goal_plan()))));
    adapter.on_event(event(
        &scope,
        EventKind::SlotChanged {
            slot: Some(json(&plain_plan())),
        },
    ));
    adapter.on_event(finished(&scope, true));
    match adapter.poll(10).unwrap() {
        TurnResult::Quarantined { reason, .. } => assert!(reason.contains("slot changed")),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_value_that_does_not_fit_the_schema_is_invalid() {
    let (mut adapter, scope) = open(PlanningReason::GoalCompile);
    let mut value = json(&goal_plan());
    value["proposal"]["also"] = serde_json::json!("an extra field");
    adapter.on_event(call(&scope, "tu-1"));
    adapter.on_event(result(&scope, "tu-1", true, Some(value)));
    adapter.on_event(finished(&scope, true));
    assert!(matches!(
        adapter.poll(10).unwrap(),
        TurnResult::Invalid { .. }
    ));

    let (mut adapter, scope) = open(PlanningReason::GoalCompile);
    adapter.on_event(call(&scope, "tu-1"));
    adapter.on_event(result(&scope, "tu-1", true, None));
    adapter.on_event(finished(&scope, true));
    assert!(matches!(
        adapter.poll(10).unwrap(),
        TurnResult::Invalid { .. }
    ));
}

#[test]
fn a_goal_belongs_to_goal_compilation_and_compilation_takes_nothing_else() {
    // A goal draft in an escalation is refused.
    let (mut adapter, scope) = open(PlanningReason::Escalation);
    one_good_call(&mut adapter, &scope, &goal_plan());
    match adapter.poll(10).unwrap() {
        TurnResult::Invalid { reason, .. } => assert!(reason.contains("not allowed in this phase")),
        other => panic!("{other:?}"),
    }
    // A plan without a goal is fine there.
    let (mut adapter, scope) = open(PlanningReason::Escalation);
    one_good_call(&mut adapter, &scope, &plain_plan());
    assert!(adapter.poll(10).unwrap().is_proposal());

    // Compilation without a goal, or with a review, is refused.
    let (mut adapter, scope) = open(PlanningReason::GoalCompile);
    one_good_call(&mut adapter, &scope, &plain_plan());
    assert!(matches!(
        adapter.poll(10).unwrap(),
        TurnResult::Invalid { .. }
    ));
    let review = ScriptedPlanner::default().plan(&request(1, PlanningReason::Review));
    let (mut adapter, scope) = open(PlanningReason::GoalCompile);
    one_good_call(&mut adapter, &scope, &review);
    assert!(matches!(
        adapter.poll(10).unwrap(),
        TurnResult::Invalid { .. }
    ));
}

#[test]
fn a_plan_that_is_not_well_formed_is_invalid() {
    let PlannerOutput::Plan { mut proposal } = plain_plan() else {
        panic!("a plan");
    };
    proposal.bundle.dependencies.push(Dependency {
        before: "ensure".into(),
        after: "ensure".into(),
    });
    let (mut adapter, scope) = open(PlanningReason::Escalation);
    one_good_call(&mut adapter, &scope, &PlannerOutput::Plan { proposal });
    match adapter.poll(10).unwrap() {
        TurnResult::Invalid { reason, .. } => assert!(reason.contains("malformed"), "{reason}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_recorder_that_cannot_account_for_the_turn_isolates_it() {
    let (mut adapter, scope) = open(PlanningReason::GoalCompile);
    adapter.on_event(call(&scope, "tu-1"));
    adapter.on_event(result(&scope, "tu-1", true, Some(json(&goal_plan()))));
    adapter.on_event(finished(&scope, false));
    assert!(matches!(
        adapter.poll(10).unwrap(),
        TurnResult::Quarantined { .. }
    ));
}

#[test]
fn a_cancelled_turn_finishes_its_cleanup_and_never_installs_what_it_produced() {
    let (mut adapter, scope) = open(PlanningReason::GoalCompile);
    adapter.on_event(call(&scope, "tu-1"));
    adapter.on_event(result(&scope, "tu-1", true, Some(json(&goal_plan()))));
    adapter.cancel(100);
    assert_eq!(adapter.transport().cancelled, vec![scope.clone()]);
    assert!(
        adapter.poll(150).is_none(),
        "cancellation is not completion: the barrier is awaited"
    );
    assert!(adapter.is_busy());

    adapter.on_event(finished(&scope, true));
    assert!(
        matches!(adapter.poll(200).unwrap(), TurnResult::Cancelled { .. }),
        "a valid submission in a cancelled turn is not installed"
    );
    assert!(!adapter.is_busy());
}

#[test]
fn what_arrives_after_a_turn_closed_is_only_traced_and_cannot_count_for_the_next_turn() {
    let (mut adapter, old) = open(PlanningReason::GoalCompile);
    adapter.cancel(100);
    adapter.on_event(finished(&old, true));
    assert!(matches!(
        adapter.poll(150).unwrap(),
        TurnResult::Cancelled { .. }
    ));

    // The next turn opens at a newer generation.
    let fresh = adapter
        .begin(&request(2, PlanningReason::GoalCompile), 200)
        .unwrap();
    assert_eq!(fresh.generation, 2);

    // The cancelled turn's stragglers arrive now, valid-looking and all.
    adapter.on_event(call(&old, "tu-old"));
    adapter.on_event(result(&old, "tu-old", true, Some(json(&goal_plan()))));
    adapter.on_event(finished(&old, true));
    assert_eq!(
        adapter.trace().iter().filter(|t| t.kind == "late").count(),
        3,
        "{:?}",
        adapter.trace()
    );

    // The new turn made no submission of its own, and the old one is not credited to it.
    adapter.on_event(finished(&fresh, true));
    assert!(matches!(
        adapter.poll(300).unwrap(),
        TurnResult::Missing { .. }
    ));
}

#[test]
fn a_cancelled_turn_whose_cleanup_never_arrives_is_isolated_after_the_grace_period() {
    let (mut adapter, _) = open(PlanningReason::GoalCompile);
    adapter.cancel(1_000);
    assert!(
        adapter.poll(2_500).is_none(),
        "still within the grace period"
    );
    match adapter.poll(3_100).unwrap() {
        TurnResult::Quarantined { reason, .. } => assert!(reason.contains("cleanup")),
        other => panic!("{other:?}"),
    }
    assert!(
        !adapter.is_busy(),
        "the lock is released so the root can go on"
    );
}

#[test]
fn a_turn_past_its_deadline_is_cancelled_and_nothing_it_says_afterwards_is_used() {
    let (mut adapter, scope) = open(PlanningReason::GoalCompile);
    assert!(adapter.poll(9_000).is_none());
    assert!(
        adapter.poll(10_500).is_none(),
        "the deadline cancels the call, then the barrier is awaited"
    );
    assert_eq!(adapter.transport().cancelled.len(), 1);

    // The answer comes in after all.
    one_good_call(&mut adapter, &scope, &goal_plan());
    assert!(matches!(
        adapter.poll(10_600).unwrap(),
        TurnResult::Expired { .. }
    ));
}

#[test]
fn a_turn_needs_its_own_lock_and_a_retry_needs_a_newer_generation() {
    let (mut adapter, scope) = open(PlanningReason::GoalCompile);
    assert_eq!(
        adapter.begin(&request(2, PlanningReason::GoalCompile), 0),
        Err(BeginError::Busy)
    );
    one_good_call(&mut adapter, &scope, &goal_plan());
    adapter.poll(10).unwrap();

    assert_eq!(
        adapter.begin(&request(1, PlanningReason::Escalation), 20),
        Err(BeginError::StaleGeneration { last: 1 }),
        "the same generation is not a retry"
    );
    assert!(
        adapter
            .begin(&request(2, PlanningReason::Escalation), 20)
            .is_ok()
    );

    // Requests that cannot start.
    let mut late = request(3, PlanningReason::Escalation);
    late.deadline_ms = 5;
    let mut other = PlannerAdapter::new(ScriptedTransport::default(), AdapterConfig::default());
    assert_eq!(other.begin(&late, 10), Err(BeginError::AlreadyExpired));
    let mut bad = request(3, PlanningReason::Escalation);
    bad.generation = 0;
    assert!(matches!(other.begin(&bad, 0), Err(BeginError::Invalid(_))));
    other.transport_mut().refuse = Some("no route".into());
    assert!(matches!(
        other.begin(&request(1, PlanningReason::Escalation), 0),
        Err(BeginError::Transport(_))
    ));
    assert!(!other.is_busy(), "a call that never started holds no lock");
}

#[test]
fn events_without_a_scope_or_for_strangers_are_only_traced() {
    let (mut adapter, scope) = open(PlanningReason::GoalCompile);
    adapter.on_event(ScopedEvent {
        scope: None,
        kind: EventKind::ToolCall {
            tool_use_id: "tu-1".into(),
            tool: TOOL.into(),
        },
    });
    let stranger = Scope {
        root: RootTaskId::new("root-9").unwrap(),
        request_id: RequestId::new("plan-9").unwrap(),
        generation: 9,
    };
    adapter.on_event(call(&stranger, "tu-9"));
    adapter.on_event(result(&stranger, "tu-9", true, Some(json(&goal_plan()))));
    adapter.on_event(finished(&scope, true));
    assert!(matches!(
        adapter.poll(10).unwrap(),
        TurnResult::Missing { .. }
    ));
    let kinds: Vec<&str> = adapter.trace().iter().map(|t| t.kind).collect();
    assert_eq!(kinds.iter().filter(|k| **k == "unattributed").count(), 1);
    assert_eq!(kinds.iter().filter(|k| **k == "foreign").count(), 2);
}

#[test]
fn usage_adds_up_and_what_is_unknown_stays_unknown() {
    let (mut adapter, scope) = open(PlanningReason::GoalCompile);
    adapter.on_event(event(
        &scope,
        EventKind::Usage(Usage {
            input_tokens: Some(100),
            output_tokens: Some(20),
            cost_micros: None,
        }),
    ));
    adapter.on_event(event(
        &scope,
        EventKind::Usage(Usage {
            input_tokens: Some(50),
            output_tokens: None,
            cost_micros: None,
        }),
    ));
    one_good_call(&mut adapter, &scope, &goal_plan());
    let done = adapter.poll(10).unwrap();
    let usage = done.usage();
    assert_eq!(usage.input_tokens, Some(150));
    assert_eq!(usage.output_tokens, Some(20));
    assert_eq!(
        usage.cost_micros, None,
        "no figure was given, so none is invented"
    );
    assert!(!usage.is_complete());
}

#[test]
fn the_proposal_is_accepted_by_the_hosts_coordinator_and_a_replaced_request_is_not() {
    use interaction_engine::deliberation::{Arrival, PlanningCoordinator, StaleReason};

    let root = RootTaskId::new("root-1").unwrap();
    let context = request(1, PlanningReason::GoalCompile).context;
    let mut coordinator = PlanningCoordinator::new();
    let mut adapter = adapter();

    // The host's request is what the turn runs under.
    let first = coordinator.request(
        &root,
        1,
        1,
        PlanningReason::GoalCompile,
        context.clone(),
        0,
        5_000,
    );
    let scope = adapter.begin(&first, 0).unwrap();
    one_good_call(&mut adapter, &scope, &goal_plan());
    let TurnResult::Proposal {
        scope: got, output, ..
    } = adapter.poll(10).unwrap()
    else {
        panic!("a proposal");
    };
    assert!(matches!(
        coordinator.arrived(&got.request_id, got.generation, *output.clone(), 20),
        Arrival::Accepted(_)
    ));

    // A newer request replaces the one the next turn ran for. The turn for the
    // replaced request is cancelled, and what it produced cannot be installed.
    let second = coordinator.request(
        &root,
        1,
        1,
        PlanningReason::Escalation,
        context.clone(),
        100,
        5_000,
    );
    let scope = adapter.begin(&second, 100).unwrap();
    let replacement =
        coordinator.request(&root, 1, 1, PlanningReason::Escalation, context, 110, 5_000);
    assert!(replacement.generation > second.generation);
    adapter.cancel(110);
    adapter.on_event(call(&scope, "tu-1"));
    adapter.on_event(result(&scope, "tu-1", true, Some(json(&plain_plan()))));
    adapter.on_event(finished(&scope, true));
    assert!(matches!(
        adapter.poll(120).unwrap(),
        TurnResult::Cancelled { .. }
    ));
    // Even if an answer for the replaced request reached the coordinator, it
    // would not install.
    assert_eq!(
        coordinator.arrived(&second.request_id, second.generation, plain_plan(), 130),
        Arrival::Stale(StaleReason::OldGeneration)
    );
}
