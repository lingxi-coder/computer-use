//! Input primitives: a superset of I0's inputs (design 5.3).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::require_nonempty;
use crate::i0;

pub const MAX_TEXT_BYTES: usize = 8 * 1024;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// Modifier keys travel as event flags on an atomic primitive. They are never
/// pressed on their own, so a dead device cannot leave one held down.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Modifiers {
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub control: bool,
    #[serde(default)]
    pub option: bool,
    #[serde(default)]
    pub command: bool,
}

/// What an accessibility action does to the bound element.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AxActionKind {
    Press,
    /// Absolute row-selection effect. This requires a dedicated
    /// TableViewportV1 binding and can never be inferred from AXPress.
    SetSelectedTrue,
    /// Absolute: repeating it leaves the same value.
    SetValue {
        value: String,
    },
    /// Save an already-open owned document only if its exact expected value
    /// is still present. This is never a generic AX action.
    SaveOwnedDocument {
        expected_value: String,
    },
    ShowMenu,
    ScrollToVisible,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    content = "payload",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Primitive {
    /// Atomic: down and up are submitted in one critical section.
    Click {
        button: MouseButton,
        x: i32,
        y: i32,
        #[serde(default)]
        modifiers: Modifiers,
    },
    /// One absolute pointer event with no held buttons. Held/drag motion uses
    /// PointerMove and remains subject to the separate held-input policy.
    PointerPosition {
        x: i32,
        y: i32,
    },
    /// Atomic key press with modifier flags.
    KeyPress {
        key: String,
        #[serde(default)]
        modifiers: Modifiers,
    },
    TypeText {
        text: String,
    },
    Scroll {
        dx: i32,
        dy: i32,
    },
    AxAction {
        action: AxActionKind,
    },
    ActivateWindow {
        app: String,
    },
    // Held-class primitives below. They leave state behind that must be
    // released, so they are disabled unless the policy allows them.
    KeyDown {
        key: String,
    },
    KeyUp {
        key: String,
    },
    PointerDown {
        button: MouseButton,
    },
    PointerUp {
        button: MouseButton,
    },
    PointerMove {
        x: i32,
        y: i32,
    },
}

/// Whether held-class primitives may be submitted at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PrimitivePolicy {
    /// Phase 1 production: only atomic primitives.
    AtomicOnly,
    /// Simulator and later phases: dragging, long presses, held keys.
    AllowHeld,
}

impl Primitive {
    /// Leaves a held key or button behind until a matching release.
    pub fn is_held_class(&self) -> bool {
        matches!(
            self,
            Self::KeyDown { .. }
                | Self::KeyUp { .. }
                | Self::PointerDown { .. }
                | Self::PointerUp { .. }
                | Self::PointerMove { .. }
        )
    }

    /// Depends on screen coordinates, so it also depends on the geometry.
    pub fn is_pointer_class(&self) -> bool {
        matches!(
            self,
            Self::Click { .. }
                | Self::PointerPosition { .. }
                | Self::Scroll { .. }
                | Self::PointerDown { .. }
                | Self::PointerUp { .. }
                | Self::PointerMove { .. }
        )
    }

    /// Needs a point-of-hit check against the binding just before submission.
    pub fn needs_hit_test(&self) -> bool {
        matches!(
            self,
            Self::Click { .. }
                | Self::PointerPosition { .. }
                | Self::Scroll { .. }
                | Self::AxAction { .. }
        )
    }

    /// The ledger token and whether this is the down half, for held keys and
    /// buttons. Moves and atomic primitives own nothing.
    pub fn held_token(&self) -> Option<(String, bool)> {
        match self {
            Self::KeyDown { key } => Some((format!("key:{key}"), true)),
            Self::KeyUp { key } => Some((format!("key:{key}"), false)),
            Self::PointerDown { button } => {
                Some((format!("button:{}", button_name(*button)), true))
            }
            Self::PointerUp { button } => Some((format!("button:{}", button_name(*button)), false)),
            _ => None,
        }
    }

    /// The primitive that undoes a down, used for cleanup.
    pub fn release_of(&self) -> Option<Primitive> {
        match self {
            Self::KeyDown { key } => Some(Self::KeyUp { key: key.clone() }),
            Self::PointerDown { button } => Some(Self::PointerUp { button: *button }),
            _ => None,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::KeyPress { key, .. } | Self::KeyDown { key } | Self::KeyUp { key } => {
                require_nonempty("primitive.key", key)
            }
            Self::TypeText { text } => {
                if text.is_empty() {
                    Err("primitive.text must not be empty".into())
                } else if text.len() > MAX_TEXT_BYTES {
                    Err(format!(
                        "primitive.text is longer than {MAX_TEXT_BYTES} bytes"
                    ))
                } else {
                    Ok(())
                }
            }
            Self::ActivateWindow { app } => require_nonempty("primitive.app", app),
            Self::AxAction {
                action: AxActionKind::SaveOwnedDocument { expected_value },
            } if expected_value.len() > 2048 || expected_value.contains('\0') => {
                Err("owned document save value must be NUL-free and at most 2048 bytes".into())
            }
            _ => Ok(()),
        }
    }
}

pub fn button_name(button: MouseButton) -> &'static str {
    match button {
        MouseButton::Left => "left",
        MouseButton::Right => "right",
        MouseButton::Middle => "middle",
    }
}

impl From<i0::MouseButton> for MouseButton {
    fn from(button: i0::MouseButton) -> Self {
        match button {
            i0::MouseButton::Left => Self::Left,
            i0::MouseButton::Right => Self::Right,
        }
    }
}

impl From<i0::Input> for Primitive {
    fn from(input: i0::Input) -> Self {
        match input {
            i0::Input::KeyDown { key } => Self::KeyDown { key },
            i0::Input::KeyUp { key } => Self::KeyUp { key },
            i0::Input::PointerDown { button } => Self::PointerDown {
                button: button.into(),
            },
            i0::Input::PointerUp { button } => Self::PointerUp {
                button: button.into(),
            },
            i0::Input::PointerMove { x, y } => Self::PointerMove { x, y },
        }
    }
}

/// The primitive has no I0 counterpart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotAnI0Input;

impl TryFrom<Primitive> for i0::Input {
    type Error = NotAnI0Input;

    fn try_from(primitive: Primitive) -> Result<Self, NotAnI0Input> {
        let button = |button: MouseButton| match button {
            MouseButton::Left => Ok(i0::MouseButton::Left),
            MouseButton::Right => Ok(i0::MouseButton::Right),
            MouseButton::Middle => Err(NotAnI0Input),
        };
        match primitive {
            Primitive::KeyDown { key } => Ok(Self::KeyDown { key }),
            Primitive::KeyUp { key } => Ok(Self::KeyUp { key }),
            Primitive::PointerDown { button: b } => Ok(Self::PointerDown { button: button(b)? }),
            Primitive::PointerUp { button: b } => Ok(Self::PointerUp { button: button(b)? }),
            Primitive::PointerMove { x, y } => Ok(Self::PointerMove { x, y }),
            _ => Err(NotAnI0Input),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_separates_atomic_from_held_primitives() {
        let click = Primitive::Click {
            button: MouseButton::Left,
            x: 1,
            y: 2,
            modifiers: Modifiers::default(),
        };
        assert!(!click.is_held_class());
        assert!(click.is_pointer_class());
        assert!(click.needs_hit_test());
        assert!(click.held_token().is_none());

        let position = Primitive::PointerPosition { x: 1, y: 2 };
        assert!(
            !position.is_held_class() && position.is_pointer_class() && position.needs_hit_test()
        );
        assert!(position.held_token().is_none());
        assert!(Primitive::PointerMove { x: 1, y: 2 }.is_held_class());
        let ax = Primitive::AxAction {
            action: AxActionKind::Press,
        };
        assert!(!ax.is_held_class());
        assert!(
            !ax.is_pointer_class(),
            "AX actions do not depend on coordinates"
        );
        assert!(ax.needs_hit_test());

        let down = Primitive::KeyDown { key: "W".into() };
        assert!(down.is_held_class());
        assert_eq!(down.held_token(), Some(("key:W".into(), true)));
        assert_eq!(
            down.release_of(),
            Some(Primitive::KeyUp { key: "W".into() })
        );
        assert!(Primitive::PointerMove { x: 0, y: 0 }.held_token().is_none());
    }

    #[test]
    fn validation_rejects_blank_keys_and_oversized_text() {
        assert!(
            Primitive::KeyPress {
                key: " ".into(),
                modifiers: Modifiers::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            Primitive::TypeText {
                text: String::new()
            }
            .validate()
            .is_err()
        );
        assert!(
            Primitive::TypeText {
                text: "x".repeat(MAX_TEXT_BYTES + 1)
            }
            .validate()
            .is_err()
        );
        assert!(
            Primitive::TypeText {
                text: "你好".into()
            }
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn owned_document_save_value_is_bounded_and_nul_free() {
        let valid = Primitive::AxAction {
            action: AxActionKind::SaveOwnedDocument {
                expected_value: String::new(),
            },
        };
        assert!(valid.validate().is_ok());
        for value in ["x\0y".to_owned(), "x".repeat(2049)] {
            assert!(
                Primitive::AxAction {
                    action: AxActionKind::SaveOwnedDocument {
                        expected_value: value,
                    },
                }
                .validate()
                .is_err()
            );
        }
    }

    #[test]
    fn i0_inputs_round_trip_and_new_primitives_do_not_map_back() {
        let inputs = [
            i0::Input::KeyDown { key: "W".into() },
            i0::Input::KeyUp { key: "W".into() },
            i0::Input::PointerDown {
                button: i0::MouseButton::Right,
            },
            i0::Input::PointerUp {
                button: i0::MouseButton::Left,
            },
            i0::Input::PointerMove { x: 3, y: 4 },
        ];
        for input in inputs {
            let primitive = Primitive::from(input.clone());
            assert_eq!(i0::Input::try_from(primitive).unwrap(), input);
        }
        assert!(i0::Input::try_from(Primitive::Scroll { dx: 0, dy: 1 }).is_err());
        assert!(
            i0::Input::try_from(Primitive::PointerDown {
                button: MouseButton::Middle
            })
            .is_err()
        );
    }

    #[test]
    fn strict_decoding_rejects_unknown_fields_in_nested_payloads() {
        let ok = r#"{"type":"click","payload":{"button":"left","x":1,"y":2}}"#;
        assert!(serde_json::from_str::<Primitive>(ok).is_ok());
        let extra = r#"{"type":"click","payload":{"button":"left","x":1,"y":2,"z":3}}"#;
        assert!(serde_json::from_str::<Primitive>(extra).is_err());
        let modifier = r#"{"type":"click","payload":{"button":"left","x":1,"y":2,"modifiers":{"shift":true,"hyper":true}}}"#;
        assert!(serde_json::from_str::<Primitive>(modifier).is_err());
    }
}
