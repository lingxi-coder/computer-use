//! Effect classes and the admission table (design 9.3).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What acting on a control may change. Declared order is the order of
/// severity, and `Unknown` is deliberately the most restrictive value.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum EffectClass {
    ReadOnly,
    Navigation,
    ReversibleEdit,
    Commit,
    Destructive,
    /// Sends, publishes, pays or changes account security. Not executed in phase 1.
    External,
    /// No trusted source could classify it.
    Unknown,
}

impl EffectClass {
    /// Combine a class from a trusted source with one inferred from label text.
    ///
    /// Inference may only raise the class: it can never turn a declared
    /// destructive control into a harmless one.
    pub fn raise(self, inferred: EffectClass) -> EffectClass {
        self.max(inferred)
    }
}

/// The facts the admission check needs; everything is decided by code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EffectContext {
    /// The application, account and object are inside the task scope.
    pub in_scope: bool,
    /// The target is one the frozen goal actually asks about.
    pub target_covered: bool,
    /// The goal names this exact target for a destructive change.
    pub target_named: bool,
    /// Highest class the frozen goal includes.
    pub max_effect_class: EffectClass,
    /// The user confirmed this one action.
    pub confirmed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DenyReason {
    OutOfScope,
    TargetNotCovered,
    DestructiveNotNamed,
    ExternalNotSupported,
    UnknownEffect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "decision", content = "reason", rename_all = "snake_case")]
pub enum Admission {
    Allow,
    /// Release the input and ask the user; the answer covers this action only.
    NeedsConfirmation,
    Deny(DenyReason),
}

/// The 9.3 table. Confirmation never widens the goal.
pub fn admit(class: EffectClass, context: &EffectContext) -> Admission {
    if !context.in_scope {
        return Admission::Deny(DenyReason::OutOfScope);
    }
    match class {
        EffectClass::ReadOnly | EffectClass::Navigation => Admission::Allow,
        EffectClass::ReversibleEdit => {
            if context.target_covered {
                Admission::Allow
            } else {
                Admission::Deny(DenyReason::TargetNotCovered)
            }
        }
        EffectClass::Commit => {
            let included =
                context.target_covered && context.max_effect_class >= EffectClass::Commit;
            if included || context.confirmed {
                Admission::Allow
            } else {
                Admission::NeedsConfirmation
            }
        }
        EffectClass::Destructive => {
            if !context.target_named {
                Admission::Deny(DenyReason::DestructiveNotNamed)
            } else if context.confirmed {
                Admission::Allow
            } else {
                Admission::NeedsConfirmation
            }
        }
        EffectClass::External => Admission::Deny(DenyReason::ExternalNotSupported),
        EffectClass::Unknown => Admission::Deny(DenyReason::UnknownEffect),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> EffectContext {
        EffectContext {
            in_scope: true,
            target_covered: true,
            target_named: false,
            max_effect_class: EffectClass::ReversibleEdit,
            confirmed: false,
        }
    }

    #[test]
    fn read_only_and_navigation_run_directly_inside_the_scope() {
        for class in [EffectClass::ReadOnly, EffectClass::Navigation] {
            assert_eq!(admit(class, &context()), Admission::Allow);
        }
    }

    #[test]
    fn nothing_runs_outside_the_task_scope() {
        let outside = EffectContext {
            in_scope: false,
            ..context()
        };
        for class in [
            EffectClass::ReadOnly,
            EffectClass::Navigation,
            EffectClass::ReversibleEdit,
            EffectClass::Commit,
            EffectClass::Destructive,
        ] {
            assert_eq!(
                admit(class, &outside),
                Admission::Deny(DenyReason::OutOfScope)
            );
        }
    }

    #[test]
    fn reversible_edits_only_run_on_targets_the_goal_covers() {
        assert_eq!(
            admit(EffectClass::ReversibleEdit, &context()),
            Admission::Allow
        );
        let other = EffectContext {
            target_covered: false,
            ..context()
        };
        assert_eq!(
            admit(EffectClass::ReversibleEdit, &other),
            Admission::Deny(DenyReason::TargetNotCovered)
        );
    }

    #[test]
    fn commits_need_the_goal_to_include_them_or_a_confirmation() {
        assert_eq!(
            admit(EffectClass::Commit, &context()),
            Admission::NeedsConfirmation
        );
        let included = EffectContext {
            max_effect_class: EffectClass::Commit,
            ..context()
        };
        assert_eq!(admit(EffectClass::Commit, &included), Admission::Allow);
        let confirmed = EffectContext {
            confirmed: true,
            ..context()
        };
        assert_eq!(admit(EffectClass::Commit, &confirmed), Admission::Allow);
        // The ceiling covers the class but not this target: still ask.
        let other_target = EffectContext {
            max_effect_class: EffectClass::Commit,
            target_covered: false,
            ..context()
        };
        assert_eq!(
            admit(EffectClass::Commit, &other_target),
            Admission::NeedsConfirmation
        );
    }

    #[test]
    fn destructive_changes_must_be_named_and_confirmed_every_time() {
        assert_eq!(
            admit(EffectClass::Destructive, &context()),
            Admission::Deny(DenyReason::DestructiveNotNamed)
        );
        let named = EffectContext {
            target_named: true,
            ..context()
        };
        assert_eq!(
            admit(EffectClass::Destructive, &named),
            Admission::NeedsConfirmation
        );
        let confirmed = EffectContext {
            confirmed: true,
            ..named
        };
        assert_eq!(
            admit(EffectClass::Destructive, &confirmed),
            Admission::Allow
        );
        // A ceiling that includes destructive changes does not remove the confirmation.
        let ceiling = EffectContext {
            max_effect_class: EffectClass::Destructive,
            ..named
        };
        assert_eq!(
            admit(EffectClass::Destructive, &ceiling),
            Admission::NeedsConfirmation
        );
    }

    #[test]
    fn external_and_unknown_effects_are_never_executed() {
        let everything = EffectContext {
            target_named: true,
            confirmed: true,
            max_effect_class: EffectClass::Unknown,
            ..context()
        };
        assert_eq!(
            admit(EffectClass::External, &everything),
            Admission::Deny(DenyReason::ExternalNotSupported)
        );
        assert_eq!(
            admit(EffectClass::Unknown, &everything),
            Admission::Deny(DenyReason::UnknownEffect)
        );
    }

    #[test]
    fn inference_can_only_raise_a_declared_class() {
        assert_eq!(
            EffectClass::ReadOnly.raise(EffectClass::Destructive),
            EffectClass::Destructive
        );
        assert_eq!(
            EffectClass::Destructive.raise(EffectClass::ReadOnly),
            EffectClass::Destructive
        );
        assert_eq!(
            EffectClass::Unknown.raise(EffectClass::Commit),
            EffectClass::Unknown
        );
        assert!(EffectClass::Unknown > EffectClass::External);
    }
}
