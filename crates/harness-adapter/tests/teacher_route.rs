use std::collections::{BTreeMap, VecDeque};

use harness_adapter::teacher::{
    TeacherAdapter, TeacherCall, TeacherCallTransport, TeacherTurnResult, output_schema,
};
use harness_adapter::{EventKind, Scope, ScopedEvent, Usage};
use interaction_contracts::desktop::decision::{
    Candidate, DecisionRequest, DecisionState, Precondition, Question, QuestionKind, QuestionOption,
};
use interaction_contracts::desktop::effect::EffectClass;
use interaction_contracts::desktop::ids::{
    BindingRef, CandidateId, DecisionId, RootTaskId, SemanticTargetRef,
};
use serde_json::{Value, json};

#[derive(Default)]
struct ScriptedTeacher {
    starts: Vec<(Scope, TeacherCall)>,
    events: VecDeque<ScopedEvent>,
    cancelled: Vec<Scope>,
}

impl TeacherCallTransport for ScriptedTeacher {
    fn start(
        &mut self,
        scope: &Scope,
        call: &TeacherCall,
    ) -> Result<(), harness_adapter::TransportError> {
        self.starts.push((scope.clone(), call.clone()));
        Ok(())
    }

    fn try_event(&mut self) -> Option<ScopedEvent> {
        self.events.pop_front()
    }

    fn cancel(&mut self, scope: &Scope) {
        self.cancelled.push(scope.clone());
    }
}

fn request(generation: u64, decision: &str) -> DecisionRequest {
    let root = RootTaskId::new("root-teacher").unwrap();
    DecisionRequest {
        decision_id: DecisionId::new(decision).unwrap(),
        lane_id: "lane-teacher".into(),
        request_generation: generation,
        root_task_id: root.clone(),
        task_revision: 4,
        plan_revision: 2,
        root_epoch: 1,
        decision_program: "ui.ensure_value@1".into(),
        calibration_revision: "local-s1".into(),
        state: DecisionState {
            root_task_id: root,
            task_revision: 4,
            goal_summary: "turn off daily picks".into(),
            subgoal: "find the matching control".into(),
            snapshot_revision: 12,
            objects: vec![],
            candidates: vec![Candidate {
                candidate_id: CandidateId::new("candidate-off").unwrap(),
                skill: "ui.ensure_value".into(),
                semantic_target: Some(SemanticTargetRef::new("daily-picks").unwrap()),
                binding: Some(BindingRef::new("binding-7").unwrap()),
                parameters: BTreeMap::from([("value".into(), "off".into())]),
                effect_class: EffectClass::ReversibleEdit,
                precondition: Precondition::Executable,
                description: "set the daily picks control off".into(),
            }],
            recent_results: vec![],
            missing_fields: vec![],
            omitted_items: vec![],
            candidate_set_digest: "sha256:fixture-private-digest".into(),
        },
        questions: vec![Question {
            id: "q-choice".into(),
            kind: QuestionKind::Choice,
            prompt: "Which listed candidate advances the subgoal?".into(),
            options: vec![QuestionOption {
                key: "candidate-off".into(),
                description: "turn the control off".into(),
            }],
        }],
        created_ms: 90,
        result_deadline_ms: 1_000,
    }
}

fn output(selected: &str, candidate: &str) -> Value {
    json!({
        "answers": [{
            "question_id": "q-choice",
            "selected": selected,
            "confidence_milli": 820
        }],
        "choice": {"action": "start_skill", "candidate_id": candidate}
    })
}

fn submit(transport: &mut ScriptedTeacher, scope: &Scope, value: Value, usage: Usage) {
    transport.events.push_back(ScopedEvent::new(
        scope,
        EventKind::ToolCall {
            tool_use_id: "tool-call-1".into(),
            tool: "TeacherDecision".into(),
        },
    ));
    transport.events.push_back(ScopedEvent::new(
        scope,
        EventKind::ToolResult {
            tool_use_id: "tool-call-1".into(),
            tool: "TeacherDecision".into(),
            accepted: true,
            slot: Some(value),
        },
    ));
    transport
        .events
        .push_back(ScopedEvent::new(scope, EventKind::Usage(usage)));
    transport.events.push_back(ScopedEvent::new(
        scope,
        EventKind::Finished { complete: true },
    ));
}

#[test]
fn one_teacher_call_uses_independent_schema_and_adapter_owned_identity() {
    let mut adapter = TeacherAdapter::new(ScriptedTeacher::default(), "open_ai", "model-r7");
    let request = request(1, "decision-secret-1");
    let scope = adapter.begin(&request, 100).unwrap();
    assert!(adapter.begin(&request, 101).is_err());
    let started = &adapter.transport().starts;
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].0, scope);
    assert_eq!(started[0].1.schema, output_schema());
    assert!(!started[0].1.input.contains("decision-secret-1"));
    assert!(!started[0].1.input.contains("root-teacher"));
    assert!(!started[0].1.input.contains("sha256:fixture-private-digest"));
    assert!(started[0].1.input.contains("candidate-off"));

    submit(
        adapter.transport_mut(),
        &scope,
        output("candidate-off", "candidate-off"),
        Usage {
            input_tokens: Some(41),
            output_tokens: Some(9),
            cost_micros: None,
        },
    );
    let Some(TeacherTurnResult::Proposal {
        proposal, usage, ..
    }) = adapter.poll(145)
    else {
        panic!("expected one teacher proposal")
    };
    assert_eq!(proposal.decision_id.as_str(), "decision-secret-1");
    assert_eq!(proposal.request_generation, 1);
    assert_eq!(
        proposal.candidate_set_digest,
        "sha256:fixture-private-digest"
    );
    assert_eq!(proposal.backend, "open_ai");
    assert_eq!(proposal.model_revision, "model-r7");
    assert_eq!(
        proposal.source,
        interaction_contracts::desktop::decision::DecisionSource::Teacher
    );
    assert_eq!(proposal.answers[0].question_id, "q-choice");
    assert_eq!(
        proposal.answers[0].selected.as_deref(),
        Some("candidate-off")
    );
    assert_eq!(usage.input_tokens, Some(41));
    assert_eq!(usage.output_tokens, Some(9));
    assert_eq!(usage.cost_micros, None);
}

#[test]
fn invalid_option_or_model_supplied_identity_is_rejected() {
    let request = request(1, "decision-2");
    let mut adapter = TeacherAdapter::new(ScriptedTeacher::default(), "fixture", "r1");
    let scope = adapter.begin(&request, 100).unwrap();
    submit(
        adapter.transport_mut(),
        &scope,
        output("not-an-option", "candidate-off"),
        Usage::default(),
    );
    let Some(TeacherTurnResult::Proposal { proposal, .. }) = adapter.poll(120) else {
        panic!("expected a failure proposal")
    };
    assert_eq!(
        proposal.failure,
        Some(interaction_contracts::desktop::decision::DecisionFailure::UnsupportedQuestion)
    );
    assert!(proposal.choice.is_none());
    assert!(proposal.answers.is_empty());

    let mut forged = output("candidate-off", "candidate-off");
    forged["decision_id"] = json!("attacker-choice");
    assert!(serde_json::from_value::<harness_adapter::TeacherOutput>(forged).is_err());
}

#[test]
fn an_empty_choice_cannot_accept_an_invented_answer() {
    let mut request = request(1, "empty-choice");
    request.questions[0].options.clear();
    let mut adapter = TeacherAdapter::new(ScriptedTeacher::default(), "fixture", "r1");
    let scope = adapter.begin(&request, 100).unwrap();
    submit(
        adapter.transport_mut(),
        &scope,
        output("invented", "candidate-off"),
        Usage::default(),
    );
    let Some(TeacherTurnResult::Proposal { proposal, .. }) = adapter.poll(120) else {
        panic!("expected a failure proposal")
    };
    assert!(proposal.failure.is_some());
    assert!(proposal.answers.is_empty());
    assert!(proposal.choice.is_none());
}

#[test]
fn an_empty_choice_can_report_no_selection_and_request_observation() {
    let mut request = request(1, "empty-observe");
    request.questions[0].options.clear();
    let mut adapter = TeacherAdapter::new(ScriptedTeacher::default(), "fixture", "r1");
    let scope = adapter.begin(&request, 100).unwrap();
    submit(
        adapter.transport_mut(),
        &scope,
        json!({
            "answers": [{"question_id": "q-choice", "selected": null, "confidence_milli": null}],
            "choice": {"action": "observe_more", "facts": ["new evidence"]}
        }),
        Usage::default(),
    );
    let Some(TeacherTurnResult::Proposal { proposal, .. }) = adapter.poll(120) else {
        panic!("expected an observation proposal")
    };
    assert!(proposal.failure.is_none());
    assert_eq!(proposal.answers[0].selected, None);
    assert!(matches!(
        proposal.choice,
        Some(interaction_contracts::desktop::decision::PolicyChoice::ObserveMore { .. })
    ));
}

#[test]
fn a_general_choice_can_use_business_options_independently_of_candidates() {
    let mut request = request(1, "classification-choice");
    request.questions[0].prompt = "Does this page mention daily picks?".into();
    request.questions[0].options = vec![
        QuestionOption {
            key: "yes".into(),
            description: "mentioned".into(),
        },
        QuestionOption {
            key: "no".into(),
            description: "absent".into(),
        },
    ];
    let mut adapter = TeacherAdapter::new(ScriptedTeacher::default(), "fixture", "r1");
    let scope = adapter.begin(&request, 100).unwrap();
    submit(
        adapter.transport_mut(),
        &scope,
        output("yes", "candidate-off"),
        Usage::default(),
    );
    let Some(TeacherTurnResult::Proposal { proposal, .. }) = adapter.poll(120) else {
        panic!("expected a classification proposal")
    };
    assert!(proposal.failure.is_none());
    assert_eq!(proposal.answers[0].selected.as_deref(), Some("yes"));
}

#[test]
fn cancellation_seals_scope_late_result_is_ignored_and_unknown_usage_stays_unknown() {
    let mut adapter = TeacherAdapter::new(ScriptedTeacher::default(), "fixture", "r1");
    let first_scope = adapter.begin(&request(1, "decision-3"), 100).unwrap();
    adapter.cancel("decision-3", 1, 110);
    assert_eq!(adapter.transport().cancelled, vec![first_scope.clone()]);
    adapter.transport_mut().events.push_back(ScopedEvent::new(
        &first_scope,
        EventKind::Finished { complete: false },
    ));
    assert!(matches!(
        adapter.poll(111),
        Some(TeacherTurnResult::Cancelled { .. })
    ));

    let second_scope = adapter.begin(&request(2, "decision-4"), 200).unwrap();
    adapter.transport_mut().events.push_back(ScopedEvent::new(
        &first_scope,
        EventKind::ToolCall {
            tool_use_id: "late".into(),
            tool: "TeacherDecision".into(),
        },
    ));
    submit(
        adapter.transport_mut(),
        &second_scope,
        output("candidate-off", "candidate-off"),
        Usage::default(),
    );
    let Some(TeacherTurnResult::Proposal {
        proposal, usage, ..
    }) = adapter.poll(230)
    else {
        panic!("late event from sealed scope must not poison the next generation")
    };
    assert_eq!(proposal.request_generation, 2);
    assert_eq!(usage.input_tokens, None);
    assert_eq!(usage.output_tokens, None);
    assert_eq!(usage.cost_micros, None);
}
