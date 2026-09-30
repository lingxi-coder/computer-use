//! Reading values out of evidence. Verification and the skills both need to
//! find the bound control in a snapshot and compare what it shows with what
//! is wanted; they must agree on how, so the rules live here once.

use interaction_contracts::desktop::binding::instance_fingerprint;
use interaction_contracts::desktop::source::{UiObject, UiValue};
use interaction_contracts::desktop::task::ExpectedValue;

use super::state_core::EnvSnapshot;

/// The control whose fingerprint is `digest`, looked up only in the layer that
/// receives input. A secure field is never returned: its value is not read.
pub fn object_for<'a>(env: &'a EnvSnapshot, digest: &str) -> Option<&'a UiObject> {
    let active = env.snapshot.active_layer.as_deref()?;
    env.snapshot.objects.iter().find(|object| {
        object.window_id == active
            && !object.secure
            && instance_fingerprint(&env.snapshot, object).digest() == digest
    })
}

/// Whether `actual` is the wanted value. A value of another kind proves
/// nothing either way, so it is `None` rather than `false`.
pub fn values_equal(actual: &UiValue, expected: &ExpectedValue) -> Option<bool> {
    match (actual, expected) {
        (UiValue::Bool(a), ExpectedValue::Bool { value }) => Some(a == value),
        (UiValue::Text(a), ExpectedValue::Text { value }) => Some(a == value),
        (UiValue::Number(a), ExpectedValue::Number { value }) => {
            Some((*a - *value as f64).abs() < f64::EPSILON)
        }
        _ => None,
    }
}

/// The wanted value as the string an accessibility set-value action carries.
pub fn value_text(value: &ExpectedValue) -> String {
    match value {
        ExpectedValue::Bool { value: true } => "on".into(),
        ExpectedValue::Bool { value: false } => "off".into(),
        ExpectedValue::Text { value } => value.clone(),
        ExpectedValue::Number { value } => value.to_string(),
    }
}

/// A value as a short plain string, for what a decision backend is shown.
pub fn display(value: &UiValue) -> String {
    match value {
        UiValue::Bool(value) => value.to_string(),
        UiValue::Text(text) => text.clone(),
        UiValue::Number(number) => number.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_of_a_different_kind_prove_nothing() {
        assert_eq!(
            values_equal(&UiValue::Bool(true), &ExpectedValue::Bool { value: true }),
            Some(true)
        );
        assert_eq!(
            values_equal(&UiValue::Bool(true), &ExpectedValue::Bool { value: false }),
            Some(false)
        );
        assert_eq!(
            values_equal(
                &UiValue::Text("x".into()),
                &ExpectedValue::Bool { value: true }
            ),
            None
        );
        assert_eq!(
            values_equal(&UiValue::Number(3.0), &ExpectedValue::Number { value: 3 }),
            Some(true)
        );
    }

    #[test]
    fn set_value_text_is_plain_and_absolute() {
        assert_eq!(value_text(&ExpectedValue::Bool { value: false }), "off");
        assert_eq!(value_text(&ExpectedValue::Bool { value: true }), "on");
        assert_eq!(value_text(&ExpectedValue::Number { value: 7 }), "7");
        assert_eq!(
            value_text(&ExpectedValue::Text {
                value: "English".into()
            }),
            "English"
        );
    }
}
