//! DecisionStateCompiler: the bounded projection a decision backend is shown
//! (design 9.1 and 9.5).
//!
//! It selects and serialises; it never infers a fact that is not in the
//! evidence. What does not fit is recorded as omitted, and when something the
//! decision needs cannot fit the compilation fails instead of silently cutting
//! it, so the caller decomposes the task or observes again.

use std::collections::BTreeSet;

use interaction_contracts::desktop::binding::normalize_label;
use interaction_contracts::desktop::decision::{Candidate, DecisionState, StateObject};
use interaction_contracts::desktop::source::UiObject;
use interaction_contracts::desktop::task::GoalSpec;
use sha2::{Digest, Sha256};

use crate::environment::read::display;
use crate::environment::state_core::EnvSnapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompilerLimits {
    pub max_objects: usize,
    pub max_recent: usize,
}

impl Default for CompilerLimits {
    fn default() -> Self {
        Self {
            max_objects: 48,
            max_recent: 8,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileFailure {
    /// Something the decision needs does not fit in the budget.
    TooLarge(String),
}

impl std::fmt::Display for CompileFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge(what) => write!(f, "the decision input does not fit: {what}"),
        }
    }
}

impl std::error::Error for CompileFailure {}

pub struct CompileInput<'a> {
    pub goal: &'a GoalSpec,
    pub subgoal: &'a str,
    pub env: &'a EnvSnapshot,
    pub now_ms: u64,
    pub candidates: Vec<Candidate>,
    pub recent_results: &'a [String],
    pub missing_fields: Vec<String>,
}

/// Identifies exactly this set of candidates over exactly this evidence. A
/// proposal for another set cannot be admitted.
pub fn candidate_set_digest(snapshot_revision: u64, candidates: &[Candidate]) -> String {
    let canonical =
        serde_json::to_vec(&(snapshot_revision, candidates)).expect("candidates always serialise");
    let hash = Sha256::digest(&canonical);
    let mut out = String::from("cs1:");
    for byte in hash {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn label_of(object: &UiObject) -> &str {
    object
        .title
        .as_deref()
        .or(object.description.as_deref())
        .unwrap_or("")
}

pub fn compile(
    input: CompileInput<'_>,
    limits: &CompilerLimits,
) -> Result<DecisionState, CompileFailure> {
    let CompileInput {
        goal,
        subgoal,
        env,
        now_ms,
        candidates,
        recent_results,
        missing_fields,
    } = input;

    let named: BTreeSet<&str> = candidates
        .iter()
        .filter_map(|candidate| candidate.parameters.get("instance"))
        .map(String::as_str)
        .collect();
    let wanted = normalize_label(&goal.target.label);
    let active = env.snapshot.active_layer.as_deref();

    // Relevance: what a candidate acts on, then what the goal is about, then
    // the rest of the layer that receives input, then everything else.
    let mut secure = 0usize;
    let mut ranked: Vec<(u8, usize, &UiObject)> = Vec::new();
    for (index, object) in env.snapshot.objects.iter().enumerate() {
        if object.secure {
            secure += 1;
            continue;
        }
        let rank = if named.contains(object.instance_ref.as_str()) {
            0
        } else if !wanted.is_empty() && normalize_label(label_of(object)).contains(&wanted) {
            1
        } else if Some(object.window_id.as_str()) == active {
            2
        } else {
            3
        };
        ranked.push((rank, index, object));
    }
    ranked.sort_by_key(|(rank, index, _)| (*rank, *index));
    let total = ranked.len();
    let kept: Vec<&UiObject> = ranked
        .into_iter()
        .take(limits.max_objects)
        .map(|(_, _, object)| object)
        .collect();

    // A candidate must never point at something the backend cannot see.
    for instance in &named {
        if !kept
            .iter()
            .any(|object| object.instance_ref.as_str() == *instance)
        {
            return Err(CompileFailure::TooLarge(format!(
                "control {instance} would be cut from the state"
            )));
        }
    }

    let mut omitted_items = Vec::new();
    if total > kept.len() {
        omitted_items.push(format!(
            "objects: {} less relevant objects left out",
            total - kept.len()
        ));
    }
    if secure > 0 {
        omitted_items.push(format!("secure fields: {secure} withheld"));
    }

    let objects = kept
        .into_iter()
        .map(|object| StateObject {
            instance: object.instance_ref.clone(),
            role: object.role.clone(),
            label: label_of(object).to_string(),
            value: object.value.as_ref().map(display),
            group: object.group.clone(),
            enabled: object.enabled,
            in_view: object.in_view,
            age_ms: now_ms.saturating_sub(env.snapshot.observed_ms),
        })
        .collect();

    let skip = recent_results.len().saturating_sub(limits.max_recent);
    let digest = candidate_set_digest(env.revision, &candidates);
    Ok(DecisionState {
        root_task_id: goal.root_task_id.clone(),
        task_revision: goal.task_revision,
        goal_summary: goal.user_text.clone(),
        subgoal: subgoal.to_string(),
        snapshot_revision: env.revision,
        objects,
        candidates,
        recent_results: recent_results[skip..].to_vec(),
        missing_fields,
        omitted_items,
        candidate_set_digest: digest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reaction::candidates::{Basis, Resolution, Scope, build};
    use crate::testkit::*;
    use interaction_contracts::desktop::effect::EffectClass;
    use interaction_contracts::desktop::task::ConfirmationScope;

    const TEXT: &str = r#"Turn off "Daily picks" in "Push""#;

    fn state_with(
        rig: &mut EnvRig,
        limits: &CompilerLimits,
        now: u64,
    ) -> Result<DecisionState, CompileFailure> {
        let env = rig.state.latest().unwrap().clone();
        let target = interaction_contracts::desktop::ids::SemanticTargetRef::new(TARGET).unwrap();
        let binding = match rig.bindings.current(&target) {
            Some(binding) => binding.clone(),
            None => rig.bind_goal_target(),
        };
        let goal = goal(ConfirmationScope::UiRevisit, EffectClass::ReversibleEdit);
        let bundle = bundle(TEXT);
        let plan_targets = BTreeSet::from([TARGET.to_string()]);
        let scope = Scope {
            goal: &goal,
            plan_targets: &plan_targets,
            pack: &rig.pack,
            env: &env,
        };
        let resolution = Resolution::Bound(binding);
        let candidates = build(
            &scope,
            &Basis::Step {
                subgoal: &bundle.subgoals[0],
                resolution: &resolution,
            },
        );
        compile(
            CompileInput {
                goal: &goal,
                subgoal: "ensure the switch is off",
                env: &env,
                now_ms: now,
                candidates,
                recent_results: &[],
                missing_fields: vec![],
            },
            limits,
        )
    }

    #[test]
    fn the_acted_on_control_comes_first_and_ages_count_from_the_observation() {
        let mut rig = EnvRig::new();
        rig.observe(100);
        let state = state_with(&mut rig, &CompilerLimits::default(), 160).unwrap();
        assert_eq!(state.objects[0].instance.as_str(), "sw-push-daily");
        assert_eq!(state.objects[0].value.as_deref(), Some("true"));
        assert!(state.objects.iter().all(|object| object.age_ms == 60));
        assert!(state.omitted_items.is_empty(), "{:?}", state.omitted_items);
        assert_eq!(state.candidates.len(), 4);
        assert_eq!(state.goal_summary, "turn off the daily picks in Push");
    }

    #[test]
    fn what_does_not_fit_is_recorded_and_the_candidates_never_lose_their_objects() {
        let mut rig = EnvRig::new();
        rig.observe(100);
        let full = state_with(&mut rig, &CompilerLimits::default(), 100).unwrap();
        let tight = CompilerLimits {
            max_objects: 3,
            max_recent: 8,
        };
        let state = state_with(&mut rig, &tight, 100).unwrap();
        assert_eq!(state.objects.len(), 3);
        assert_eq!(state.objects[0].instance.as_str(), "sw-push-daily");
        assert!(
            state
                .omitted_items
                .iter()
                .any(|item| item.contains("left out")),
            "{:?}",
            state.omitted_items
        );
        assert!(full.objects.len() > 3);

        let none = CompilerLimits {
            max_objects: 0,
            max_recent: 8,
        };
        let failure = state_with(&mut rig, &none, 100).unwrap_err();
        assert!(
            matches!(failure, CompileFailure::TooLarge(_)),
            "{failure:?}"
        );
    }

    #[test]
    fn secure_fields_are_withheld_and_only_counted() {
        let mut rig = EnvRig::new();
        rig.observe(100);
        // Put a secure field in the next observation.
        let mut snapshot = rig.app.snapshot(&meta(99, 150));
        let mut field = snapshot
            .objects
            .iter()
            .find(|o| o.role == "switch")
            .unwrap()
            .clone();
        field.instance_ref = interaction_contracts::desktop::ids::ObjectRef::new("pw").unwrap();
        field.secure = true;
        field.value = None;
        field.title = Some("Password".into());
        snapshot.objects.push(field);
        snapshot.source_sequence = 99;
        rig.state.admit(snapshot, 150, &rig.pack).unwrap();
        let state = state_with(&mut rig, &CompilerLimits::default(), 150).unwrap();
        assert!(state.objects.iter().all(|o| o.label != "Password"));
        assert!(
            state
                .omitted_items
                .iter()
                .any(|item| item.contains("secure fields: 1")),
            "{:?}",
            state.omitted_items
        );
    }

    #[test]
    fn the_digest_names_this_candidate_set_over_this_evidence() {
        let mut rig = EnvRig::new();
        rig.observe(100);
        let first = state_with(&mut rig, &CompilerLimits::default(), 100).unwrap();
        let again = state_with(&mut rig, &CompilerLimits::default(), 100).unwrap();
        assert_eq!(first.candidate_set_digest, again.candidate_set_digest);
        assert!(first.candidate_set_digest.starts_with("cs1:"));

        rig.observe(100);
        let later = state_with(&mut rig, &CompilerLimits::default(), 200).unwrap();
        assert_ne!(
            first.candidate_set_digest, later.candidate_set_digest,
            "new evidence is a new candidate set"
        );
    }

    #[test]
    fn only_the_most_recent_results_are_shown() {
        let mut rig = EnvRig::new();
        let env = rig.observe(100);
        let goal = goal(ConfirmationScope::UiRevisit, EffectClass::ReversibleEdit);
        let recent: Vec<String> = (1..=12).map(|n| format!("result {n}")).collect();
        let state = compile(
            CompileInput {
                goal: &goal,
                subgoal: "x",
                env: &env,
                now_ms: 100,
                candidates: vec![],
                recent_results: &recent,
                missing_fields: vec![],
            },
            &CompilerLimits::default(),
        )
        .unwrap();
        assert_eq!(state.recent_results.len(), 8);
        assert_eq!(state.recent_results[0], "result 5");
        assert_eq!(state.recent_results[7], "result 12");
    }
}
