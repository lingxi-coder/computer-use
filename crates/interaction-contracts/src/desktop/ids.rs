//! Validated identifiers for everything above the device layer.

use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

pub const MAX_ID_LEN: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdError {
    kind: &'static str,
    reason: &'static str,
}

impl fmt::Display for IdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid {}: {}", self.kind, self.reason)
    }
}

impl std::error::Error for IdError {}

fn check(kind: &'static str, value: &str) -> Result<(), IdError> {
    let reason = if value.trim().is_empty() {
        Some("must not be empty or whitespace-only")
    } else if value.len() > MAX_ID_LEN {
        Some("longer than 256 bytes")
    } else if value.chars().any(char::is_control) {
        Some("contains control characters")
    } else {
        None
    };
    match reason {
        Some(reason) => Err(IdError { kind, reason }),
        None => Ok(()),
    }
}

macro_rules! string_id {
    ($(#[$doc:meta])* $name:ident, $kind:literal) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, IdError> {
                let value = value.into();
                check($kind, &value)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = IdError;
            fn try_from(value: String) -> Result<Self, IdError> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> String {
                value.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl JsonSchema for $name {
            fn schema_name() -> Cow<'static, str> {
                Cow::Borrowed(stringify!($name))
            }

            fn json_schema(_: &mut SchemaGenerator) -> Schema {
                json_schema!({
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 256,
                    "description": $kind,
                })
            }
        }
    };
}

string_id!(
    /// A root task. A retry never mints a new one; only a new user task does.
    RootTaskId,
    "root task id"
);
string_id!(
    /// One attempt to run a skill inside a root task.
    AttemptId,
    "attempt id"
);
string_id!(
    /// One logical decision; consumed at most once.
    DecisionId,
    "decision id"
);
string_id!(
    /// A candidate inside one decision request.
    CandidateId,
    "candidate id"
);
string_id!(
    /// A request/response correlation id (planning, perception, binding).
    RequestId,
    "request id"
);
string_id!(
    /// The semantic object a task asks for, independent of the current screen.
    SemanticTargetRef,
    "semantic target ref"
);
string_id!(
    /// A live UI object instance in one observation. Never reused as a target.
    ObjectRef,
    "object ref"
);
string_id!(
    /// The current binding of a semantic target to one live instance.
    BindingRef,
    "binding ref"
);
string_id!(
    /// A user confirmation prompt.
    ConfirmationId,
    "confirmation id"
);
string_id!(
    /// A wire message.
    MessageId,
    "message id"
);
string_id!(
    /// The process or component that produced a message.
    ProducerId,
    "producer id"
);
string_id!(
    /// A published knowledge release.
    ReleaseId,
    "release id"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_reject_blank_oversized_and_control_text() {
        assert!(RootTaskId::new("task-1").is_ok());
        assert!(RootTaskId::new("").is_err());
        assert!(RootTaskId::new(" \t").is_err());
        assert!(RootTaskId::new("a\0b").is_err());
        assert!(RootTaskId::new("x".repeat(MAX_ID_LEN)).is_ok());
        assert!(RootTaskId::new("x".repeat(MAX_ID_LEN + 1)).is_err());
    }

    #[test]
    fn ids_validate_while_decoding_and_serialize_as_plain_strings() {
        let id: AttemptId = serde_json::from_str("\"attempt-7\"").unwrap();
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"attempt-7\"");
        assert!(serde_json::from_str::<AttemptId>("\"\"").is_err());
        assert!(serde_json::from_str::<AttemptId>("17").is_err());
    }

    #[test]
    fn schema_describes_the_length_limits() {
        let schema = schemars::schema_for!(DecisionId);
        let value = serde_json::to_value(schema).unwrap();
        assert_eq!(value["type"], "string");
        assert_eq!(value["minLength"], 1);
        assert_eq!(value["maxLength"], 256);
    }
}
