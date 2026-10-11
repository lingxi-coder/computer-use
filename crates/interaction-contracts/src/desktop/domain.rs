use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The authorization domain for one observed action attempt.
///
/// This is deliberately independent from `BackendKind`: a physical backend may
/// implement more than one domain, but every intent is admitted under exactly
/// one domain for its lifetime.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionDomain {
    ForegroundNative,
    BackgroundAx,
    HeadlessBrowser,
    AppIntegration,
    Simulated,
}

impl ExecutionDomain {
    pub const ALL: [Self; 5] = [
        Self::ForegroundNative,
        Self::BackgroundAx,
        Self::HeadlessBrowser,
        Self::AppIntegration,
        Self::Simulated,
    ];
}

/// Action families advertised for the current execution domain.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DomainAction {
    NativePointer,
    NativeKeyboard,
    NativeScroll,
    WindowActivation,
    ForegroundAx,
    BackgroundAxPress,
    HeadlessBrowserAction,
    AppIntegrationAction,
    SimulatedAction,
}
