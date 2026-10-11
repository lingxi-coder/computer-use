//! The fixed keyboard profile's result is an independent current UI read.
//! Neither a Completed receipt nor a Skill verdict establishes this effect.
use super::Engine;
use crate::reaction::skills::SkillRun;
use interaction_contracts::desktop::{
    binding::instance_fingerprint, source::UiValue, task::ExpectedValue,
};

impl Engine {
    pub(super) fn keyboard_value_verified(
        &self,
        skill: &SkillRun,
        mechanical_unknown: bool,
        input: bool,
    ) -> bool {
        if mechanical_unknown || !self.evidence_usable() {
            return false;
        }
        let (Some(run), Some(env), Some(original)) = (
            self.run.as_ref(),
            self.state.latest(),
            skill.keyboard_context(),
        ) else {
            return false;
        };
        if run
            .pack
            .keyboard_toggle(&run.goal.target.semantic_target)
            .is_none()
            || env.snapshot.validate().is_err()
            || env.snapshot.menu_context.is_some()
        {
            return false;
        }
        let Some(window) = &env.snapshot.window_context else {
            return false;
        };
        if window.session_epoch != original.session_epoch
            || window.process != original.process
            || window.origin_window_id != original.origin_window_id
            || window.origin_window_frame != original.origin_window_frame
            || window.verified_source_sequence != env.snapshot.source_sequence
            || window.verified_observed_ms != env.snapshot.observed_ms
            || input
                && (env.snapshot.source_sequence <= original.verified_source_sequence
                    || skill
                        .input_floor()
                        .is_none_or(|end| env.snapshot.observed_ms <= end))
        {
            return false;
        }
        let Some(binding) = self.bindings.current(&run.goal.target.semantic_target) else {
            return false;
        };
        // A descriptor match without a new binding at this actual read is not
        // fresh result evidence, even when ordinary IDs happen to stay stable.
        if binding.env_revision != env.revision
            || binding.snapshot_id != env.snapshot.snapshot_id
            || binding.observed_ms != env.snapshot.observed_ms
            || binding.page != env.page
        {
            return false;
        }
        let Some(object) = env
            .snapshot
            .objects
            .iter()
            .find(|object| object.instance_ref == binding.instance)
        else {
            return false;
        };
        object.enabled
            && object.in_view
            && !object.secure
            && object.frame == original.target_frame
            && instance_fingerprint(&env.snapshot, object) == original.target_fingerprint
            && super::running::keyboard_read_path_safe(&env.snapshot, object, original)
            && matches!((&object.value, &run.goal.desired),
                (Some(UiValue::Bool(actual)), ExpectedValue::Bool { value }) if actual == value)
    }
}
