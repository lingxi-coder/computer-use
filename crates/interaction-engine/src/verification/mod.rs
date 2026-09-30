//! Verification: decides what the evidence proves (design 13.3 and 14.7).
//!
//! It evaluates the frozen success predicate over observations and nothing
//! else. A frame captured before the last input cannot confirm an effect of
//! that input, however late it arrives, and unknown never becomes success.
//! It can only say what it lacks; it has no way to act.

use interaction_contracts::desktop::binding::instance_fingerprint;
use interaction_contracts::desktop::ids::{AttemptId, RequestId, RootTaskId, SemanticTargetRef};
use interaction_contracts::desktop::source::UiValue;
use interaction_contracts::desktop::task::{ExpectedValue, GoalSpec, Predicate};
use interaction_contracts::desktop::verification::{
    Causality, EffectResult, EffectStatus, EvidenceNeed, TimeSpan, TriState,
};
use rusqlite::{Connection, Transaction, params};

use crate::environment::binding::BindingReducer;
use crate::environment::pack::AppPack;
use crate::environment::state_core::{EnvSnapshot, StateCore};
use crate::infrastructure::storage::{Storage, StoreError, enum_to_str};

/// Tracks whether the page holding the target was left and entered again,
/// and the target bound afresh afterwards.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PageVisits {
    target_page: Option<String>,
    armed_after_ms: Option<u64>,
    left_at: Option<u64>,
    returned_at: Option<u64>,
    rebound: bool,
}

impl PageVisits {
    pub fn set_target_page(&mut self, page: Option<String>) {
        if self.target_page.is_none() {
            self.target_page = page;
        }
    }

    /// The change itself is done: from now on leaving the page counts.
    pub fn arm(&mut self, after_ms: u64) {
        if self.armed_after_ms.is_none() {
            self.armed_after_ms = Some(after_ms);
        }
    }

    pub fn observe(&mut self, page: Option<&str>, observed_ms: u64) {
        let Some(armed) = self.armed_after_ms else {
            return;
        };
        if observed_ms < armed {
            return;
        }
        let on_target = page.is_some() && page == self.target_page.as_deref();
        if self.left_at.is_none() {
            if page.is_some() && !on_target {
                self.left_at = Some(observed_ms);
            }
        } else if self.returned_at.is_none() && on_target {
            self.returned_at = Some(observed_ms);
        }
    }

    pub fn note_bound(&mut self, bound_at_ms: u64, page: Option<&str>) {
        if let Some(returned) = self.returned_at
            && bound_at_ms >= returned
            && page == self.target_page.as_deref()
        {
            self.rebound = true;
        }
    }

    pub fn is_armed(&self) -> bool {
        self.armed_after_ms.is_some()
    }

    pub fn left(&self) -> bool {
        self.left_at.is_some()
    }

    pub fn returned(&self) -> bool {
        self.returned_at.is_some()
    }

    pub fn satisfied(&self) -> bool {
        self.left_at.is_some() && self.returned_at.is_some() && self.rebound
    }

    pub fn target_page(&self) -> Option<&str> {
        self.target_page.as_deref()
    }
}

/// What the evaluation may read. Verification owns none of it.
pub struct EvidenceContext<'a> {
    pub state: &'a StateCore,
    pub bindings: &'a BindingReducer,
    pub pack: &'a AppPack,
    pub goal: &'a GoalSpec,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub status: TriState,
    /// The snapshots the conclusion rests on.
    pub evidence: Vec<RequestId>,
    /// What is missing, for the parts that are not true yet.
    pub needs: Vec<EvidenceNeed>,
}

#[derive(Debug, Default)]
pub struct Verification {
    pub visits: PageVisits,
    first_input_ms: Option<u64>,
    last_input_end_ms: Option<u64>,
    frozen_ms: u64,
    baseline: Option<TriState>,
    revision: u64,
    last: Option<(EffectStatus, Vec<RequestId>)>,
    need_seq: u64,
}

fn values_equal(actual: &UiValue, expected: &ExpectedValue) -> Option<bool> {
    match (actual, expected) {
        (UiValue::Bool(a), ExpectedValue::Bool { value }) => Some(a == value),
        (UiValue::Text(a), ExpectedValue::Text { value }) => Some(a == value),
        (UiValue::Number(a), ExpectedValue::Number { value }) => {
            Some((*a - *value as f64).abs() < f64::EPSILON)
        }
        // A different kind of value proves nothing either way.
        _ => None,
    }
}

impl Verification {
    pub fn new(frozen_ms: u64) -> Self {
        Self {
            frozen_ms,
            ..Self::default()
        }
    }

    pub fn last_input_end_ms(&self) -> Option<u64> {
        self.last_input_end_ms
    }

    /// Input reached the application between these times.
    pub fn record_input(&mut self, from_ms: u64, to_ms: u64) {
        self.first_input_ms.get_or_insert(from_ms);
        self.last_input_end_ms = Some(self.last_input_end_ms.map_or(to_ms, |end| end.max(to_ms)));
    }

    /// Whether the goal already held before anything was done. It decides
    /// whether a later satisfied goal can be attributed to our own input.
    pub fn note_baseline(&mut self, value: TriState) {
        if self.baseline.is_none() {
            self.baseline = Some(value);
        }
    }

    /// Nothing observed earlier than this can confirm an effect.
    pub fn evidence_floor_ms(&self) -> u64 {
        self.last_input_end_ms.unwrap_or(self.frozen_ms)
    }

    fn evidence_for<'a>(
        &self,
        ctx: &'a EvidenceContext<'_>,
        after_action: bool,
    ) -> Option<&'a EnvSnapshot> {
        if after_action {
            ctx.state.latest_since(self.evidence_floor_ms())
        } else {
            ctx.state.latest()
        }
    }

    /// The current value of the bound target in `env`, read from evidence.
    fn read_value(
        ctx: &EvidenceContext<'_>,
        env: &EnvSnapshot,
        target: &SemanticTargetRef,
    ) -> Option<UiValue> {
        let binding = ctx.bindings.current(target)?;
        let active = env.snapshot.active_layer.as_deref()?;
        env.snapshot
            .objects
            .iter()
            .find(|object| {
                object.window_id == active
                    && !object.secure
                    && instance_fingerprint(&env.snapshot, object).digest()
                        == binding.fingerprint_digest
            })
            .and_then(|object| object.value.clone())
    }

    fn leaf(
        &self,
        predicate: &Predicate,
        ctx: &EvidenceContext<'_>,
    ) -> (TriState, Option<RequestId>) {
        match predicate {
            Predicate::ContextMatches { app } => match ctx.state.latest() {
                Some(env) if env.snapshot.complete && env.snapshot.active_layer.is_some() => (
                    if &env.snapshot.app.bundle_id == app {
                        TriState::True
                    } else {
                        TriState::False
                    },
                    Some(env.snapshot.snapshot_id.clone()),
                ),
                Some(env) => (TriState::Unknown, Some(env.snapshot.snapshot_id.clone())),
                None => (TriState::Unknown, None),
            },
            Predicate::TargetBound { target } => (
                if ctx.bindings.current(target).is_some() {
                    TriState::True
                } else {
                    TriState::Unknown
                },
                None,
            ),
            Predicate::ReadValueEquals {
                target,
                expected,
                observed_after_action,
            } => {
                let Some(env) = self.evidence_for(ctx, *observed_after_action) else {
                    return (TriState::Unknown, None);
                };
                let id = Some(env.snapshot.snapshot_id.clone());
                if !env.snapshot.complete {
                    return (TriState::Unknown, id);
                }
                match Self::read_value(ctx, env, target).and_then(|v| values_equal(&v, expected)) {
                    Some(true) => (TriState::True, id),
                    Some(false) => (TriState::False, id),
                    None => (TriState::Unknown, id),
                }
            }
            Predicate::Revisited { .. } => (
                if self.visits.satisfied() {
                    TriState::True
                } else {
                    TriState::Unknown
                },
                None,
            ),
            Predicate::CommitConfirmed { .. } => {
                let Some(rule) = ctx.pack.save_state_description.as_deref() else {
                    return (TriState::Unknown, None);
                };
                let Some(env) = ctx.state.latest_since(self.evidence_floor_ms()) else {
                    return (TriState::Unknown, None);
                };
                let id = Some(env.snapshot.snapshot_id.clone());
                let status = env
                    .snapshot
                    .objects
                    .iter()
                    .find(|object| object.description.as_deref() == Some(rule))
                    .and_then(|object| object.value.clone());
                match status {
                    Some(UiValue::Text(text)) if text == "saved" => (TriState::True, id),
                    Some(UiValue::Text(_)) => (TriState::False, id),
                    _ => (TriState::Unknown, id),
                }
            }
            Predicate::And { .. } | Predicate::Or { .. } | Predicate::Not { .. } => {
                (self.evaluate(predicate, ctx), None)
            }
        }
    }

    /// The three-valued value of a predicate over the current evidence.
    pub fn evaluate(&self, predicate: &Predicate, ctx: &EvidenceContext<'_>) -> TriState {
        match predicate {
            Predicate::And { of } => TriState::and(of.iter().map(|p| self.evaluate(p, ctx))),
            Predicate::Or { of } => TriState::or(of.iter().map(|p| self.evaluate(p, ctx))),
            Predicate::Not { of } => !self.evaluate(of, ctx),
            leaf => self.leaf(leaf, ctx).0,
        }
    }

    fn collect(&self, predicate: &Predicate, ctx: &EvidenceContext<'_>, verdict: &mut Verdict) {
        match predicate {
            Predicate::And { of } | Predicate::Or { of } => {
                of.iter()
                    .for_each(|child| self.collect(child, ctx, verdict));
            }
            Predicate::Not { of } => self.collect(of, ctx, verdict),
            leaf => {
                let (value, evidence) = self.leaf(leaf, ctx);
                if let Some(id) = evidence
                    && !verdict.evidence.contains(&id)
                {
                    verdict.evidence.push(id);
                }
                if value != TriState::True
                    && let Some(need) = self.need_for(leaf, value, ctx)
                {
                    verdict.needs.push(need);
                }
            }
        }
    }

    fn need_for(
        &self,
        leaf: &Predicate,
        value: TriState,
        ctx: &EvidenceContext<'_>,
    ) -> Option<EvidenceNeed> {
        let (fact, target, after, requires_input) = match leaf {
            Predicate::ContextMatches { app } => (format!("context:{app}"), None, None, false),
            Predicate::TargetBound { target } | Predicate::ReadValueEquals { target, .. }
                if ctx.bindings.current(target).is_none() =>
            {
                (
                    format!("binding:{target}"),
                    Some(target.clone()),
                    None,
                    false,
                )
            }
            Predicate::TargetBound { .. } => return None,
            Predicate::ReadValueEquals { target, .. } => (
                format!("value:{target}"),
                Some(target.clone()),
                Some(self.evidence_floor_ms()),
                // A reading that disagrees needs the change, which is input.
                value == TriState::False,
            ),
            Predicate::Revisited { target } => (
                format!("revisit:{target}"),
                Some(target.clone()),
                None,
                true,
            ),
            Predicate::CommitConfirmed { target } => (
                format!("commit:{target}"),
                Some(target.clone()),
                Some(self.evidence_floor_ms()),
                value == TriState::False,
            ),
            _ => return None,
        };
        Some(EvidenceNeed {
            need_id: RequestId::new(format!("need-{}", self.need_seq)).expect("valid id"),
            fact,
            target,
            observed_after_ms: after,
            requires_input,
        })
    }

    /// Evaluate the whole goal and say what is still missing.
    pub fn verdict(&mut self, ctx: &EvidenceContext<'_>) -> Verdict {
        self.need_seq += 1;
        let mut verdict = Verdict {
            status: self.evaluate(&ctx.goal.required, ctx),
            evidence: Vec::new(),
            needs: Vec::new(),
        };
        self.collect(&ctx.goal.required, ctx, &mut verdict);
        verdict
    }

    fn causality(&self, status: EffectStatus) -> Causality {
        match (self.first_input_ms, status) {
            (None, _) => Causality::NotApplicable,
            (Some(_), EffectStatus::Satisfied) if self.baseline == Some(TriState::False) => {
                Causality::Confirmed
            }
            (Some(_), _) => Causality::Unknown,
        }
    }

    /// Record a conclusion as a new immutable revision, if it differs from
    /// the last one. History is never rewritten.
    pub fn conclude(
        &mut self,
        storage: &mut Storage,
        root: &RootTaskId,
        ctx: &EvidenceContext<'_>,
        verdict: &Verdict,
        attempt: Option<(&AttemptId, Option<&str>)>,
        now_ms: u64,
    ) -> Result<Option<EffectResult>, StoreError> {
        let status = EffectStatus::from(verdict.status);
        let key = (status, verdict.evidence.clone());
        if self.last.as_ref() == Some(&key) {
            return Ok(None);
        }
        self.revision += 1;
        let result = EffectResult {
            result_id: RequestId::new(format!("effect-{root}")).expect("derived from valid ids"),
            revision: self.revision,
            root_task_id: root.clone(),
            attempt_id: attempt.map(|(id, _)| id.clone()),
            intent_id: attempt.and_then(|(_, intent)| intent.map(String::from)),
            predicate: "goal.required@1".into(),
            target: Some(ctx.goal.target.semantic_target.clone()),
            status,
            evidence_group: verdict.evidence.clone(),
            input_span: self
                .first_input_ms
                .zip(self.last_input_end_ms)
                .map(|(from_ms, to_ms)| TimeSpan { from_ms, to_ms }),
            observed_ms: now_ms,
            confirmation_scope: ctx.goal.confirmation_scope,
            causality: self.causality(status),
        };
        storage.transaction::<_, StoreError>(|tx| VerificationStore::insert(tx, &result))?;
        self.last = Some(key);
        Ok(Some(result))
    }
}

/// Verification's table: immutable effect results.
pub struct VerificationStore;

impl VerificationStore {
    pub fn insert(tx: &Transaction<'_>, result: &EffectResult) -> Result<(), StoreError> {
        tx.execute(
            "INSERT INTO effect_results (result_id, revision, root_id, json) VALUES (?1, ?2, ?3, ?4)",
            params![
                result.result_id.as_str(),
                result.revision as i64,
                result.root_task_id.as_str(),
                serde_json::to_string(result)?,
            ],
        )?;
        Ok(())
    }

    /// Every revision for a root, oldest first.
    pub fn history(conn: &Connection, root: &str) -> Result<Vec<EffectResult>, StoreError> {
        let mut statement =
            conn.prepare("SELECT json FROM effect_results WHERE root_id = ?1 ORDER BY revision")?;
        let mut rows = statement.query([root])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let text: String = row.get(0)?;
            out.push(serde_json::from_str(&text)?);
        }
        Ok(out)
    }

    pub fn latest(conn: &Connection, root: &str) -> Result<Option<EffectResult>, StoreError> {
        Ok(Self::history(conn, root)?.pop())
    }
}

/// How the status is spelled in logs.
pub fn status_name(status: EffectStatus) -> String {
    enum_to_str(&status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::state_core::StateCore;
    use interaction_contracts::desktop::binding::{BindRequest, BindScope, TargetDescription};
    use interaction_contracts::desktop::device::SurfaceRef;
    use interaction_contracts::desktop::effect::EffectClass;
    use interaction_contracts::desktop::primitive::{AxActionKind, Primitive};
    use interaction_contracts::desktop::task::{
        ConfirmationScope, RootBudgetSpec, TargetSpec, TaskScope,
    };
    use sim_world::app::{Applied, SimApp, SnapshotMeta};

    const TARGET: &str = "push/daily-picks";

    fn meta(sequence: u64, observed_ms: u64) -> SnapshotMeta {
        SnapshotMeta {
            snapshot_id: format!("snap-{sequence}"),
            session_epoch: 1,
            surface: SurfaceRef {
                id: "main".into(),
                epoch: 1,
            },
            clock_domain: "sim/1/monotonic".into(),
            source_sequence: sequence,
            observed_ms,
        }
    }

    fn goal(scope: ConfirmationScope) -> GoalSpec {
        let target = SemanticTargetRef::new(TARGET).unwrap();
        let mut checks = vec![
            Predicate::ContextMatches {
                app: "test.settings".into(),
            },
            Predicate::ReadValueEquals {
                target: target.clone(),
                expected: ExpectedValue::Bool { value: false },
                observed_after_action: true,
            },
        ];
        checks.push(match scope {
            ConfirmationScope::UiRevisit => Predicate::Revisited {
                target: target.clone(),
            },
            ConfirmationScope::Commit => Predicate::CommitConfirmed {
                target: target.clone(),
            },
        });
        GoalSpec {
            root_task_id: RootTaskId::new("root-1").unwrap(),
            task_revision: 1,
            user_text: "turn it off".into(),
            scope: TaskScope {
                app: "test.settings".into(),
                account: None,
            },
            target: TargetSpec {
                semantic_target: target,
                label: "Daily picks".into(),
                group: Some("Push".into()),
                role_hint: Some("switch".into()),
            },
            desired: ExpectedValue::Bool { value: false },
            confirmation_scope: scope,
            max_effect_class: EffectClass::ReversibleEdit,
            required: Predicate::And { of: checks },
            budget: RootBudgetSpec {
                deadline_ms: 60_000,
                max_attempts: 8,
                max_observations: 16,
                max_recoveries: 2,
                max_replans: 1,
            },
        }
    }

    struct Rig {
        state: StateCore,
        bindings: BindingReducer,
        pack: AppPack,
        app: SimApp,
        goal: GoalSpec,
        verification: Verification,
        sequence: u64,
    }

    impl Rig {
        fn new(scope: ConfirmationScope) -> Self {
            let mut state = StateCore::new();
            state.set_session(1);
            Self {
                state,
                bindings: BindingReducer::new(),
                pack: AppPack::settings_fixture(),
                app: SimApp::new(),
                goal: goal(scope),
                verification: Verification::new(0),
                sequence: 0,
            }
        }

        fn observe_at(&mut self, observed_ms: u64) {
            self.sequence += 1;
            let snapshot = self.app.snapshot(&meta(self.sequence, observed_ms));
            self.state.admit(snapshot, observed_ms, &self.pack).unwrap();
            let env = self.state.latest().unwrap().clone();
            self.bindings.revalidate(&env);
            self.verification
                .visits
                .observe(env.page.as_deref(), observed_ms);
        }

        fn bind(&mut self) {
            let env = self.state.latest().unwrap().clone();
            let request = BindRequest {
                request_id: RequestId::new("bind").unwrap(),
                semantic_target: SemanticTargetRef::new(TARGET).unwrap(),
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
                max_age_ms: 10_000,
            };
            self.bindings.bind(&env, &request, env.snapshot.observed_ms);
            let bound_at = env.snapshot.observed_ms;
            self.verification.visits.set_target_page(env.page.clone());
            self.verification
                .visits
                .note_bound(bound_at, env.page.as_deref());
        }

        fn set_value(&mut self, value: &str, at_ms: u64) {
            let proof = self
                .bindings
                .current(&SemanticTargetRef::new(TARGET).unwrap())
                .unwrap()
                .fingerprint_digest
                .clone();
            self.app.apply(
                &meta(900 + at_ms, at_ms),
                &Applied {
                    primitive: Primitive::AxAction {
                        action: AxActionKind::SetValue {
                            value: value.into(),
                        },
                    },
                    binding_fingerprint: Some(proof),
                },
            );
            self.verification.record_input(at_ms, at_ms);
        }

        fn verdict(&mut self) -> Verdict {
            let ctx = EvidenceContext {
                state: &self.state,
                bindings: &self.bindings,
                pack: &self.pack,
                goal: &self.goal,
            };
            self.verification.verdict(&ctx)
        }

        fn press_tab(&mut self, instance: &str, at_ms: u64) {
            let proof = self
                .app
                .fingerprint_of(&meta(800 + at_ms, at_ms), instance)
                .unwrap();
            self.app.apply(
                &meta(800 + at_ms, at_ms),
                &Applied {
                    primitive: Primitive::AxAction {
                        action: AxActionKind::Press,
                    },
                    binding_fingerprint: Some(proof),
                },
            );
        }
    }

    #[test]
    fn before_anything_is_observed_everything_is_unknown() {
        let mut rig = Rig::new(ConfirmationScope::UiRevisit);
        let verdict = rig.verdict();
        assert_eq!(verdict.status, TriState::Unknown);
        assert!(verdict.needs.iter().any(|n| n.fact.starts_with("context:")));
    }

    #[test]
    fn a_value_that_differs_is_false_and_the_need_is_input() {
        let mut rig = Rig::new(ConfirmationScope::UiRevisit);
        rig.observe_at(10);
        rig.bind();
        let verdict = rig.verdict();
        assert_eq!(verdict.status, TriState::False, "the switch is still on");
        let need = verdict
            .needs
            .iter()
            .find(|n| n.fact.starts_with("value:"))
            .unwrap();
        assert!(
            need.requires_input,
            "a disagreeing reading needs the change, not just another look"
        );
    }

    #[test]
    fn a_frame_from_before_the_input_cannot_confirm_the_effect() {
        let mut rig = Rig::new(ConfirmationScope::UiRevisit);
        rig.observe_at(10);
        rig.bind();
        rig.set_value("off", 100);
        // A frame observed at 50 arrives after the input at 100: the floor is 100.
        let ctx_value = |rig: &mut Rig| {
            let value_leaf = Predicate::ReadValueEquals {
                target: SemanticTargetRef::new(TARGET).unwrap(),
                expected: ExpectedValue::Bool { value: false },
                observed_after_action: true,
            };
            let ctx = EvidenceContext {
                state: &rig.state,
                bindings: &rig.bindings,
                pack: &rig.pack,
                goal: &rig.goal,
            };
            rig.verification.evaluate(&value_leaf, &ctx)
        };
        assert_eq!(
            ctx_value(&mut rig),
            TriState::Unknown,
            "the only evidence predates the input"
        );
        rig.observe_at(150);
        assert_eq!(ctx_value(&mut rig), TriState::True);
    }

    #[test]
    fn a_satisfied_goal_needs_the_revisit_too_and_the_revisit_needs_a_fresh_binding() {
        let mut rig = Rig::new(ConfirmationScope::UiRevisit);
        rig.observe_at(10);
        rig.bind();
        rig.set_value("off", 100);
        rig.verification.visits.arm(100);
        rig.observe_at(150);
        let verdict = rig.verdict();
        assert_eq!(
            verdict.status,
            TriState::Unknown,
            "value is right, revisit is pending"
        );
        assert!(
            verdict
                .needs
                .iter()
                .any(|n| n.fact.starts_with("revisit:") && n.requires_input)
        );

        rig.press_tab("tab-general", 200);
        rig.observe_at(250);
        assert!(rig.verification.visits.left());
        rig.press_tab("tab-notifications", 300);
        rig.observe_at(350);
        assert!(rig.verification.visits.returned());
        assert!(
            !rig.verification.visits.satisfied(),
            "coming back is not enough without rebinding"
        );
        assert_eq!(rig.verdict().status, TriState::Unknown);

        rig.bind();
        let verdict = rig.verdict();
        assert!(rig.verification.visits.satisfied());
        assert_eq!(verdict.status, TriState::True);
        assert!(verdict.needs.is_empty());
    }

    #[test]
    fn leaving_before_the_change_is_done_does_not_count_as_a_revisit() {
        let mut visits = PageVisits::default();
        visits.set_target_page(Some("Notifications".into()));
        visits.observe(Some("General"), 10);
        assert!(!visits.left(), "not armed yet");
        visits.arm(100);
        visits.observe(Some("General"), 50);
        assert!(!visits.left(), "that frame predates arming");
        visits.observe(Some("General"), 120);
        assert!(visits.left());
    }

    #[test]
    fn an_incomplete_read_proves_nothing() {
        let mut rig = Rig::new(ConfirmationScope::UiRevisit);
        rig.observe_at(10);
        rig.bind();
        rig.sequence += 1;
        let partial = rig.app.partial_snapshot(&meta(rig.sequence, 50), 3);
        rig.state.admit(partial, 50, &rig.pack).unwrap();
        let verdict = rig.verdict();
        assert_eq!(verdict.status, TriState::Unknown);
    }

    #[test]
    fn a_commit_goal_reads_the_save_state_after_the_last_input() {
        let mut rig = Rig::new(ConfirmationScope::Commit);
        rig.observe_at(10);
        rig.bind();
        rig.set_value("off", 100);
        rig.observe_at(150);
        let verdict = rig.verdict();
        assert_eq!(
            verdict.status,
            TriState::False,
            "unsaved changes do not count as committed"
        );
        assert!(
            verdict
                .needs
                .iter()
                .any(|n| n.fact.starts_with("commit:") && n.requires_input)
        );

        let proof = rig.app.fingerprint_of(&meta(0, 0), "btn-apply").unwrap();
        rig.app.apply(
            &meta(700, 200),
            &Applied {
                primitive: Primitive::AxAction {
                    action: AxActionKind::Press,
                },
                binding_fingerprint: Some(proof),
            },
        );
        rig.verification.record_input(200, 200);
        rig.observe_at(250);
        assert_eq!(rig.verdict().status, TriState::True);
    }

    #[test]
    fn conclusions_are_immutable_revisions_and_unchanged_ones_are_not_repeated() {
        let mut rig = Rig::new(ConfirmationScope::UiRevisit);
        let mut storage = Storage::open_in_memory().unwrap();
        let root = RootTaskId::new("root-1").unwrap();
        rig.observe_at(10);
        rig.bind();
        let conclude = |rig: &mut Rig, storage: &mut Storage, now: u64| {
            let verdict = rig.verdict();
            let ctx = EvidenceContext {
                state: &rig.state,
                bindings: &rig.bindings,
                pack: &rig.pack,
                goal: &rig.goal,
            };
            rig.verification
                .conclude(storage, &root, &ctx, &verdict, None, now)
                .unwrap()
        };
        let first = conclude(&mut rig, &mut storage, 20).unwrap();
        assert_eq!(
            (first.revision, first.status),
            (1, EffectStatus::Unsatisfied)
        );
        assert!(
            conclude(&mut rig, &mut storage, 21).is_none(),
            "same conclusion, same evidence"
        );

        rig.set_value("off", 100);
        rig.observe_at(150);
        let second = conclude(&mut rig, &mut storage, 160).unwrap();
        assert_eq!(second.revision, 2);
        assert_eq!(second.status, EffectStatus::Unknown);
        assert_eq!(
            second.input_span,
            Some(TimeSpan {
                from_ms: 100,
                to_ms: 100
            })
        );

        let history = VerificationStore::history(storage.read(), "root-1").unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0], first, "the first revision was not rewritten");
    }

    #[test]
    fn a_goal_that_was_already_met_is_not_credited_to_our_input() {
        let mut already = Verification::new(0);
        already.note_baseline(TriState::True);
        already.record_input(10, 20);
        assert_eq!(
            already.causality(EffectStatus::Satisfied),
            Causality::Unknown
        );

        let mut changed = Verification::new(0);
        changed.note_baseline(TriState::False);
        changed.record_input(10, 20);
        assert_eq!(
            changed.causality(EffectStatus::Satisfied),
            Causality::Confirmed
        );
        assert_eq!(changed.causality(EffectStatus::Unknown), Causality::Unknown);

        let untouched = Verification::new(0);
        assert_eq!(
            untouched.causality(EffectStatus::Satisfied),
            Causality::NotApplicable
        );
    }
}
