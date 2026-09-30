//! An environment pack: what the system knows about one application
//! (design 9.3). It is data, not code, and it registers control semantics;
//! the model never declares an effect class for a control.

use interaction_contracts::desktop::binding::normalize_label;
use interaction_contracts::desktop::effect::EffectClass;
use interaction_contracts::desktop::source::StructureSnapshot;

#[derive(Debug, Clone)]
pub struct EffectRule {
    /// Only controls with this role, when set.
    pub role: Option<String>,
    /// Matches when the normalised label contains this text.
    pub label_contains: String,
    pub class: EffectClass,
}

#[derive(Debug, Clone)]
pub struct AppPack {
    pub bundle_id: String,
    pub name: String,
    pub rules: Vec<EffectRule>,
    /// The role of controls that switch pages.
    pub nav_role: String,
    /// `description` of the control that reports the save state.
    pub save_state_description: Option<String>,
}

/// Label words that raise the class of any control, whatever a pack says.
/// Inference only ever raises a class, it never lowers one.
const DESTRUCTIVE_WORDS: [&str; 6] = ["delete", "remove", "discard", "reset", "erase", "clear"];
const EXTERNAL_WORDS: [&str; 5] = ["send", "publish", "post", "pay", "purchase"];

impl AppPack {
    /// The pack for the simulated settings application.
    pub fn settings_fixture() -> Self {
        let rule = |role: Option<&str>, label: &str, class| EffectRule {
            role: role.map(String::from),
            label_contains: label.into(),
            class,
        };
        Self {
            bundle_id: "test.settings".into(),
            name: "Settings".into(),
            rules: vec![
                rule(Some("button"), "apply", EffectClass::Commit),
                rule(Some("button"), "save", EffectClass::Commit),
                rule(Some("button"), "reset", EffectClass::Destructive),
                rule(Some("button"), "discard", EffectClass::Destructive),
                rule(Some("button"), "cancel", EffectClass::Navigation),
            ],
            nav_role: "tab".into(),
            save_state_description: Some("save-state".into()),
        }
    }

    fn role_default(role: &str) -> EffectClass {
        match role {
            "switch" | "checkbox" | "radio" | "popup" | "textfield" | "slider" => {
                EffectClass::ReversibleEdit
            }
            "tab" | "link" | "disclosure" | "row" => EffectClass::Navigation,
            "text" | "group" | "sheet" | "window" => EffectClass::ReadOnly,
            _ => EffectClass::Unknown,
        }
    }

    fn inferred(label: &str) -> EffectClass {
        let label = normalize_label(label);
        let has = |words: &[&str]| words.iter().any(|word| label.contains(word));
        if has(&EXTERNAL_WORDS) {
            EffectClass::External
        } else if has(&DESTRUCTIVE_WORDS) {
            EffectClass::Destructive
        } else {
            EffectClass::ReadOnly
        }
    }

    fn registered(&self, role: Option<&str>, label: &str) -> Option<EffectClass> {
        let label = normalize_label(label);
        self.rules
            .iter()
            // A rule with a role only narrows the match when the caller knows the role.
            .filter(|rule| match (rule.role.as_deref(), role) {
                (Some(rule_role), Some(role)) => rule_role == role,
                _ => true,
            })
            .find(|rule| label.contains(&rule.label_contains))
            .map(|rule| rule.class)
    }

    /// The effect class of acting on one control.
    pub fn effect_class(&self, role: &str, label: &str) -> EffectClass {
        let trusted = self
            .registered(Some(role), label)
            .unwrap_or_else(|| Self::role_default(role));
        trusted.raise(Self::inferred(label))
    }

    /// The class for a control known only by a label, such as a plan step.
    pub fn effect_class_for_label(&self, label: &str) -> EffectClass {
        self.registered(None, label)
            .map(|class| class.raise(Self::inferred(label)))
            .unwrap_or(EffectClass::Unknown)
            .raise(Self::inferred(label))
    }

    /// The page the window is showing: the part of the title after the dash.
    pub fn page_of(&self, snapshot: &StructureSnapshot) -> Option<String> {
        let layer = snapshot.active_layer.as_ref()?;
        let main = snapshot
            .windows
            .iter()
            .find(|window| !window.modal && window.owner_bundle_id == self.bundle_id)
            .or_else(|| {
                snapshot
                    .windows
                    .iter()
                    .find(|window| &window.window_id == layer)
            })?;
        let title = main.title.as_deref()?;
        Some(
            title
                .rsplit_once(" — ")
                .map_or(title, |(_, page)| page)
                .to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_controls_get_their_registered_class() {
        let pack = AppPack::settings_fixture();
        assert_eq!(pack.effect_class("button", "Apply"), EffectClass::Commit);
        assert_eq!(
            pack.effect_class("button", "Cancel"),
            EffectClass::Navigation
        );
        assert_eq!(
            pack.effect_class("switch", "Daily picks"),
            EffectClass::ReversibleEdit
        );
        assert_eq!(pack.effect_class("tab", "General"), EffectClass::Navigation);
    }

    #[test]
    fn an_unregistered_button_is_unknown_and_never_guessed_harmless() {
        let pack = AppPack::settings_fixture();
        assert_eq!(
            pack.effect_class("button", "Frobnicate"),
            EffectClass::Unknown
        );
        assert_eq!(
            pack.effect_class("mystery", "Whatever"),
            EffectClass::Unknown
        );
    }

    #[test]
    fn label_words_can_only_raise_a_class() {
        let pack = AppPack::settings_fixture();
        // A switch is a reversible edit, unless its label says otherwise.
        assert_eq!(
            pack.effect_class("switch", "Delete account data"),
            EffectClass::Destructive
        );
        assert_eq!(
            pack.effect_class("switch", "Send weekly report"),
            EffectClass::External
        );
        // A registered destructive control stays destructive whatever its label.
        assert_eq!(
            pack.effect_class("button", "Discard"),
            EffectClass::Destructive
        );
        // Unknown cannot be talked down by a harmless word.
        assert_eq!(
            pack.effect_class("button", "Frobnicate"),
            EffectClass::Unknown
        );
    }

    #[test]
    fn a_plan_step_is_classified_from_its_label_alone() {
        let pack = AppPack::settings_fixture();
        assert_eq!(
            pack.effect_class_for_label("apply button"),
            EffectClass::Commit
        );
        assert_eq!(
            pack.effect_class_for_label("reset button"),
            EffectClass::Destructive
        );
        assert_eq!(
            pack.effect_class_for_label("something else"),
            EffectClass::Unknown
        );
    }
}
