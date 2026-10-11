use super::*;
use interaction_contracts::desktop::browser::{
    BrowserObservedBool, BrowserPageNode, BrowserTargetContext, browser_target_context_hash,
    derive_browser_checkbox_binding,
};

const FIXTURE_URL: &str = "file:///tmp/computer-use/fixtures/headless/readonly-page.html";

fn context(epoch: u64) -> BrowserTargetContext {
    BrowserTargetContext {
        browser_session_id: "browser-session".into(),
        browser_context_id: "browser-context".into(),
        page_target_id: "page-target".into(),
        cdp_session_id: "cdp-session".into(),
        frame_id: "main-frame".into(),
        document_loader_id: "document-1".into(),
        fixture_file_url: FIXTURE_URL.into(),
        session_epoch: epoch,
    }
}

fn scope() -> BrowserReadScope {
    BrowserReadScope {
        scope_id: "550e8400-e29b-41d4-a716-446655440000".into(),
        generation: 3,
        context: context(7),
        clock_domain: "headless/7/wall".into(),
        issued_ms: 10,
    }
}

fn snapshot(
    sequence: u64,
    snapshot_id: &str,
    observed_ms: u64,
    read_finished_ms: u64,
    checked: BrowserObservedBool,
) -> BrowserPageSnapshot {
    let context = context(7);
    BrowserPageSnapshot {
        snapshot_id: snapshot_id.into(),
        source_sequence: sequence,
        observed_ms,
        read_finished_ms,
        clock_domain: "headless/7/wall".into(),
        target_context_hash: browser_target_context_hash(&context).unwrap(),
        context,
        root_ax_node_id: "root".into(),
        nodes: vec![
            BrowserPageNode {
                ax_node_id: "root".into(),
                backend_dom_node_id: None,
                parent_ax_node_id: None,
                child_ax_node_ids: vec!["checkbox".into()],
                ignored: false,
                role: Some("RootWebArea".into()),
                name: Some("Fixture".into()),
                disabled: BrowserObservedBool::Unknown,
                checked: BrowserObservedBool::Unknown,
            },
            BrowserPageNode {
                ax_node_id: "checkbox".into(),
                backend_dom_node_id: Some(17),
                parent_ax_node_id: Some("root".into()),
                child_ax_node_ids: Vec::new(),
                ignored: false,
                role: Some("checkbox".into()),
                name: Some(BrowserPageProfile::FixtureBackgroundOption.label().into()),
                disabled: BrowserObservedBool::False,
                checked,
            },
        ],
    }
}

fn submitted_input() -> (Verification, BrowserPageSnapshot, BrowserCurrentUiInput) {
    let original = snapshot(1, "original-read", 20, 30, BrowserObservedBool::False);
    let original_binding = derive_browser_checkbox_binding(
        &original,
        BrowserPageProfile::FixtureBackgroundOption.label(),
    )
    .unwrap();
    let input = BrowserCurrentUiInput {
        root_task_id: RootTaskId::new("browser-root").unwrap(),
        task_revision: 4,
        root_epoch: 2,
        attempt_id: AttemptId::new("browser-attempt").unwrap(),
        intent_id: Some("browser-intent".into()),
        session_epoch: 7,
        browser_page_profile: BrowserPageProfile::FixtureBackgroundOption,
        goal_app: BrowserPageProfile::FixtureBackgroundOption.app().into(),
        scope: scope(),
        original_binding,
        expected_checked: true,
        fence: BrowserCurrentUiFence::Submitted {
            input_span: TimeSpan {
                from_ms: 31,
                to_ms: 32,
            },
        },
        deadline_ms: 500,
    };
    (Verification::default(), original, input)
}

fn readback<'a>(
    input: &'a BrowserCurrentUiInput,
    snapshot: &'a BrowserPageSnapshot,
    binding: &'a BrowserCheckboxBinding,
    now_ms: u64,
) -> BrowserCurrentUiReadback<'a> {
    BrowserCurrentUiReadback {
        root_task_id: &input.root_task_id,
        task_revision: input.task_revision,
        root_epoch: input.root_epoch,
        attempt_id: &input.attempt_id,
        intent_id: input.intent_id.as_deref(),
        session_epoch: input.session_epoch,
        browser_page_profile: input.browser_page_profile,
        goal_app: &input.goal_app,
        scope: &input.scope,
        snapshot,
        binding,
        now_ms,
        max_age_ms: 2_000,
    }
}

#[test]
fn submitted_receipt_requires_a_later_same_checkbox_read_and_persists_current_ui_effect() {
    let (mut verification, _original, input) = submitted_input();
    verification
        .register_browser_current_ui_input(input.clone())
        .unwrap();
    let fresh = snapshot(2, "fresh-read", 33, 35, BrowserObservedBool::True);
    let binding =
        derive_browser_checkbox_binding(&fresh, input.browser_page_profile.label()).unwrap();
    let readback = readback(&input, &fresh, &binding, 40);

    let proof = verification.verify_browser_current_ui(readback).unwrap();
    assert!(proof.witness().observed_checked);
    assert_eq!(proof.witness().backend_dom_node_id, 17);
    let result = proof.into_effect_result();
    assert_eq!(result.root_task_id, input.root_task_id);
    assert_eq!(result.attempt_id, Some(input.attempt_id.clone()));
    assert_eq!(result.intent_id.as_deref(), Some("browser-intent"));
    assert_eq!(result.predicate, "browser.checkbox_state@1");
    assert_eq!(
        result.target.as_ref().unwrap().as_str(),
        "browser.fixture.background_option"
    );
    assert_eq!(result.status, EffectStatus::Satisfied);
    assert_eq!(
        result.input_span,
        Some(TimeSpan {
            from_ms: 31,
            to_ms: 32
        })
    );
    assert_eq!(result.observed_ms, 33);
    assert_eq!(result.causality, Causality::Unknown);

    let mut storage = Storage::open_in_memory().unwrap();
    conclude_current_ui(&mut storage, result).unwrap();
}

#[test]
fn already_satisfied_uses_a_no_input_fence_without_synthetic_receipt_or_span() {
    let original = snapshot(1, "original-read", 20, 30, BrowserObservedBool::True);
    let input = BrowserCurrentUiInput {
        root_task_id: RootTaskId::new("browser-root").unwrap(),
        task_revision: 4,
        root_epoch: 2,
        attempt_id: AttemptId::new("already-satisfied-attempt").unwrap(),
        intent_id: None,
        session_epoch: 7,
        browser_page_profile: BrowserPageProfile::FixtureBackgroundOption,
        goal_app: BrowserPageProfile::FixtureBackgroundOption.app().into(),
        scope: scope(),
        original_binding: derive_browser_checkbox_binding(
            &original,
            BrowserPageProfile::FixtureBackgroundOption.label(),
        )
        .unwrap(),
        expected_checked: true,
        fence: BrowserCurrentUiFence::NoInputNeeded { fence_ms: 40 },
        deadline_ms: 500,
    };
    let mut verification = Verification::default();
    verification
        .register_browser_current_ui_input(input.clone())
        .unwrap();
    let fresh = snapshot(2, "fresh-after-step", 41, 43, BrowserObservedBool::True);
    let binding =
        derive_browser_checkbox_binding(&fresh, input.browser_page_profile.label()).unwrap();
    let proof = verification
        .verify_browser_current_ui(readback(&input, &fresh, &binding, 45))
        .unwrap();
    let result = proof.into_effect_result();
    assert_eq!(result.intent_id, None);
    assert_eq!(result.attempt_id, Some(input.attempt_id));
    assert_eq!(result.input_span, None);
    assert_eq!(result.causality, Causality::NotApplicable);
    assert_eq!(result.observed_ms, 41);
}

#[test]
fn read_start_must_be_strictly_after_the_real_input_end() {
    let (mut verification, _, input) = submitted_input();
    verification
        .register_browser_current_ui_input(input.clone())
        .unwrap();
    let at_input_end = snapshot(2, "overlapping-read", 32, 35, BrowserObservedBool::True);
    let binding =
        derive_browser_checkbox_binding(&at_input_end, input.browser_page_profile.label()).unwrap();
    assert_eq!(
        verification
            .verify_browser_current_ui(readback(&input, &at_input_end, &binding, 40))
            .unwrap_err(),
        BrowserCurrentUiRejection::SnapshotBeforeFence
    );
}

#[test]
fn exact_attempt_scope_and_node_identity_are_required() {
    let (mut verification, _, input) = submitted_input();
    verification
        .register_browser_current_ui_input(input.clone())
        .unwrap();
    let fresh = snapshot(2, "fresh-read", 33, 35, BrowserObservedBool::True);
    let binding =
        derive_browser_checkbox_binding(&fresh, input.browser_page_profile.label()).unwrap();

    let mut wrong_attempt = readback(&input, &fresh, &binding, 40);
    let another_attempt = AttemptId::new("other-attempt").unwrap();
    wrong_attempt.attempt_id = &another_attempt;
    assert_eq!(
        verification
            .verify_browser_current_ui(wrong_attempt)
            .unwrap_err(),
        BrowserCurrentUiRejection::AttemptMismatch
    );

    let mut other_scope = input.scope.clone();
    other_scope.generation += 1;
    let mut wrong_scope = readback(&input, &fresh, &binding, 40);
    wrong_scope.scope = &other_scope;
    assert_eq!(
        verification
            .verify_browser_current_ui(wrong_scope)
            .unwrap_err(),
        BrowserCurrentUiRejection::ScopeMismatch
    );

    let mut changed_node = fresh.clone();
    changed_node.nodes[1].backend_dom_node_id = Some(18);
    let changed_binding =
        derive_browser_checkbox_binding(&changed_node, input.browser_page_profile.label()).unwrap();
    let wrong_node = readback(&input, &changed_node, &changed_binding, 40);
    assert_eq!(
        verification
            .verify_browser_current_ui(wrong_node)
            .unwrap_err(),
        BrowserCurrentUiRejection::BindingMismatch
    );

    let mut foreign_document = fresh.clone();
    foreign_document.context.document_loader_id = "document-2".into();
    foreign_document.target_context_hash =
        browser_target_context_hash(&foreign_document.context).unwrap();
    let foreign_binding =
        derive_browser_checkbox_binding(&foreign_document, input.browser_page_profile.label())
            .unwrap();
    assert_eq!(
        verification
            .verify_browser_current_ui(readback(&input, &foreign_document, &foreign_binding, 40))
            .unwrap_err(),
        BrowserCurrentUiRejection::ScopeMismatch
    );
}

#[test]
fn unknown_disabled_checked_and_unmatched_state_never_become_success() {
    let (mut verification, _, input) = submitted_input();
    verification
        .register_browser_current_ui_input(input.clone())
        .unwrap();

    let unknown_checked = snapshot(2, "unknown-checked", 33, 35, BrowserObservedBool::Unknown);
    assert_eq!(
        verification
            .verify_browser_current_ui(readback(
                &input,
                &unknown_checked,
                &input.original_binding,
                40,
            ))
            .unwrap_err(),
        BrowserCurrentUiRejection::BindingMismatch
    );

    let mut disabled = snapshot(2, "disabled", 33, 35, BrowserObservedBool::True);
    disabled.nodes[1].disabled = BrowserObservedBool::True;
    assert_eq!(
        verification
            .verify_browser_current_ui(readback(&input, &disabled, &input.original_binding, 40,))
            .unwrap_err(),
        BrowserCurrentUiRejection::BindingMismatch
    );

    let mut unknown_disabled = snapshot(2, "unknown-disabled", 33, 35, BrowserObservedBool::True);
    unknown_disabled.nodes[1].disabled = BrowserObservedBool::Unknown;
    assert_eq!(
        verification
            .verify_browser_current_ui(readback(
                &input,
                &unknown_disabled,
                &input.original_binding,
                40,
            ))
            .unwrap_err(),
        BrowserCurrentUiRejection::BindingMismatch
    );

    let unchanged = snapshot(2, "unchanged", 33, 35, BrowserObservedBool::False);
    let unchanged_binding =
        derive_browser_checkbox_binding(&unchanged, input.browser_page_profile.label()).unwrap();
    assert_eq!(
        verification
            .verify_browser_current_ui(readback(&input, &unchanged, &unchanged_binding, 40))
            .unwrap_err(),
        BrowserCurrentUiRejection::ValueMismatch
    );
}

#[test]
fn complete_read_must_be_fresh_and_inside_its_deadline() {
    let (mut verification, _, mut input) = submitted_input();
    input.deadline_ms = 5_000;
    verification
        .register_browser_current_ui_input(input.clone())
        .unwrap();
    let fresh = snapshot(2, "fresh-read", 33, 35, BrowserObservedBool::True);
    let binding =
        derive_browser_checkbox_binding(&fresh, input.browser_page_profile.label()).unwrap();
    let future_finish = readback(&input, &fresh, &binding, 34);
    assert_eq!(
        verification
            .verify_browser_current_ui(future_finish)
            .unwrap_err(),
        BrowserCurrentUiRejection::SnapshotNotFresh
    );
    assert!(
        verification
            .verify_browser_current_ui(readback(&input, &fresh, &binding, 500))
            .is_ok()
    );
    let stale_read = readback(&input, &fresh, &binding, 2_034);
    assert_eq!(
        verification
            .verify_browser_current_ui(stale_read)
            .unwrap_err(),
        BrowserCurrentUiRejection::SnapshotExpired
    );
    let at_deadline = snapshot(
        3,
        "read-at-deadline",
        4_990,
        4_995,
        BrowserObservedBool::True,
    );
    let at_deadline_binding =
        derive_browser_checkbox_binding(&at_deadline, input.browser_page_profile.label()).unwrap();
    assert_eq!(
        verification
            .verify_browser_current_ui(readback(&input, &at_deadline, &at_deadline_binding, 5_000))
            .unwrap_err(),
        BrowserCurrentUiRejection::SnapshotExpired
    );
}

#[test]
fn register_rejects_synthetic_submitted_input_and_conflicting_same_attempt() {
    let (mut verification, _, mut input) = submitted_input();
    input.intent_id = None;
    assert_eq!(
        verification
            .register_browser_current_ui_input(input)
            .unwrap_err(),
        BrowserCurrentUiRejection::InputBeforeBinding
    );

    let (mut verification, _, input) = submitted_input();
    verification
        .register_browser_current_ui_input(input.clone())
        .unwrap();
    let mut conflict = input.clone();
    conflict.deadline_ms += 1;
    assert_eq!(
        verification
            .register_browser_current_ui_input(conflict)
            .unwrap_err(),
        BrowserCurrentUiRejection::ConflictingAttempt
    );
}
