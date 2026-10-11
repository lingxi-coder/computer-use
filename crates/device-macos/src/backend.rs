//! Simulated world input, readonly native sessions and scoped AX execution.

use std::path::PathBuf;

use desktop_ipc::SharedClock;
use execution_core::backend::{
    BrowserSubmitContext, HeldState, HitTestOutcome, HitTestRequest, InputBackend, ReleaseFailure,
    SubmitCompletion, SubmitContext, SubmitOutcome,
};
use interaction_contracts::desktop::browser_execution::BrowserAction;
use interaction_contracts::desktop::device::SurfaceRef;
use interaction_contracts::desktop::primitive::Primitive;
use sim_world::app::SnapshotMeta;
use sim_world::world_log::{WorldEntry, WorldFollower, WorldLog};

use crate::config::{DeviceBackendMode, DeviceConfig};
pub mod background_ax;
pub mod native;
use background_ax::BackgroundAxBackend;
pub mod browser_control;
use browser_control::BrowserControlBackend;
use native::NativeAxBackend;

/// The native observation session has no input implementation. It must never
/// route a real desktop observation through the fixture input backend.
pub enum DeviceBackend {
    Simulated(Box<SimWorldBackend>),
    MacosObserve,
    MacosAxPress(Box<NativeAxBackend>),
    MacosBackgroundAxPassive,
    MacosBackgroundAx(Box<BackgroundAxBackend>),
    MacosOwnedValue(Box<NativeAxBackend>),
    HeadlessBrowser(Box<BrowserControlBackend>),
}

impl DeviceBackend {
    pub fn new(config: &mut DeviceConfig) -> Result<Self, String> {
        let backend = match config.backend {
            DeviceBackendMode::Simulated => Self::Simulated(Box::new(SimWorldBackend::new(
                &config.world_log,
                config.clock,
            ))),
            DeviceBackendMode::MacosObserve => Self::MacosObserve,
            DeviceBackendMode::MacosAxPress => Self::MacosAxPress(Box::new(NativeAxBackend::new(
                config
                    .native_target
                    .clone()
                    .expect("validated native target"),
                config.native_target_window.clone(),
            ))),
            DeviceBackendMode::MacosBackgroundAx => {
                let Some(process) = config.native_target_process.clone() else {
                    return Ok(Self::MacosBackgroundAxPassive);
                };
                Self::MacosBackgroundAx(Box::new(BackgroundAxBackend::new(
                    config
                        .native_target
                        .clone()
                        .expect("validated background AX target"),
                    config
                        .native_target_window
                        .clone()
                        .expect("validated background AX window"),
                    process,
                )))
            }
            DeviceBackendMode::MacosOwnedValue => {
                let registration = config
                    .owned_document_registration
                    .clone()
                    .expect("validated private owned registration");
                Self::MacosOwnedValue(Box::new(NativeAxBackend::new_owned_value(
                    config
                        .native_target
                        .clone()
                        .expect("owned registration derives its target"),
                    registration,
                    config.owned_save_enabled,
                )))
            }
            DeviceBackendMode::HeadlessBrowser => {
                let session = config
                    .browser_control_session
                    .take()
                    .ok_or("headless browser mode requires authenticated private fd3")?;
                if config.browser_scope.as_deref() != Some(session.scope.as_ref()) {
                    return Err(
                        "browser Core scope differs from its authenticated fd3 session".into(),
                    );
                }
                Self::HeadlessBrowser(Box::new(BrowserControlBackend::new(session)?))
            }
        };
        Ok(backend)
    }

    pub fn mode(&self) -> DeviceBackendMode {
        match self {
            Self::Simulated(_) => DeviceBackendMode::Simulated,
            Self::MacosObserve => DeviceBackendMode::MacosObserve,
            Self::MacosAxPress(_) => DeviceBackendMode::MacosAxPress,
            Self::MacosBackgroundAxPassive | Self::MacosBackgroundAx(_) => {
                DeviceBackendMode::MacosBackgroundAx
            }
            Self::MacosOwnedValue(_) => DeviceBackendMode::MacosOwnedValue,
            Self::HeadlessBrowser(_) => DeviceBackendMode::HeadlessBrowser,
        }
    }

    pub fn catch_up(&mut self) {
        if let Self::Simulated(backend) = self {
            backend.catch_up();
        }
    }
    pub fn native_mut(&mut self) -> Option<&mut NativeAxBackend> {
        match self {
            Self::MacosAxPress(b) | Self::MacosOwnedValue(b) => Some(b),
            _ => None,
        }
    }
    pub fn background_ax_mut(&mut self) -> Option<&mut BackgroundAxBackend> {
        match self {
            Self::MacosBackgroundAx(backend) => Some(backend),
            _ => None,
        }
    }
    pub fn native_ready(&self) -> bool {
        match self {
            Self::MacosAxPress(backend) | Self::MacosOwnedValue(backend) => backend.ready(),
            Self::MacosBackgroundAx(backend) => backend.ready(),
            _ => false,
        }
    }
    pub fn browser_ready(&self) -> bool {
        match self {
            Self::HeadlessBrowser(backend) => backend.ready(),
            _ => false,
        }
    }
}

impl InputBackend for DeviceBackend {
    fn submit(
        &mut self,
        at_ms: u64,
        primitive: &Primitive,
        context: &SubmitContext<'_>,
    ) -> SubmitOutcome {
        match self {
            Self::Simulated(backend) => backend.submit(at_ms, primitive, context),
            Self::MacosObserve => SubmitOutcome::Rejected,
            Self::MacosBackgroundAxPassive => SubmitOutcome::Rejected,
            Self::MacosBackgroundAx(backend) => backend.submit(primitive, context),
            Self::MacosAxPress(backend) | Self::MacosOwnedValue(backend) => {
                backend.submit(primitive, context)
            }
            Self::HeadlessBrowser(_) => SubmitOutcome::Rejected,
        }
    }
    fn submit_browser(
        &mut self,
        at_ms: u64,
        action: &BrowserAction,
        context: &BrowserSubmitContext<'_>,
    ) -> SubmitOutcome {
        match self {
            Self::HeadlessBrowser(backend) => backend.submit_browser(at_ms, action, context),
            _ => SubmitOutcome::Rejected,
        }
    }
    fn poll_completion(&mut self) -> Option<SubmitCompletion> {
        match self {
            Self::MacosAxPress(b) | Self::MacosOwnedValue(b) => b.poll_completion(),
            Self::MacosBackgroundAx(backend) => backend.poll_completion(),
            Self::HeadlessBrowser(backend) => backend.poll_completion(),
            _ => None,
        }
    }
    fn revoke(&mut self, permit: &interaction_contracts::desktop::device::PermitRef) {
        match self {
            Self::MacosAxPress(b) | Self::MacosOwnedValue(b) => b.revoke(permit),
            Self::MacosBackgroundAx(backend) => backend.revoke(permit),
            Self::HeadlessBrowser(backend) => backend.revoke(permit),
            _ => {}
        }
    }
    fn abandon_pending(&mut self) {
        match self {
            Self::MacosAxPress(b) | Self::MacosOwnedValue(b) => b.abandon_pending(),
            Self::MacosBackgroundAx(backend) => backend.abandon_pending(),
            Self::HeadlessBrowser(backend) => backend.abandon_pending(),
            _ => {}
        }
    }

    fn release(&mut self, at_ms: u64, release: &Primitive) -> Result<(), ReleaseFailure> {
        match self {
            Self::Simulated(backend) => backend.release(at_ms, release),
            Self::MacosObserve => Err(ReleaseFailure),
            Self::MacosAxPress(_) | Self::MacosOwnedValue(_) => Err(ReleaseFailure),
            Self::MacosBackgroundAxPassive | Self::MacosBackgroundAx(_) => Err(ReleaseFailure),
            Self::HeadlessBrowser(_) => Err(ReleaseFailure),
        }
    }

    fn held(&self, token: &str) -> HeldState {
        match self {
            Self::Simulated(backend) => backend.held(token),
            Self::MacosObserve => HeldState::Unknown,
            Self::MacosAxPress(_) | Self::MacosOwnedValue(_) => HeldState::Unknown,
            Self::MacosBackgroundAxPassive | Self::MacosBackgroundAx(_) => HeldState::Unknown,
            Self::HeadlessBrowser(_) => HeldState::Unknown,
        }
    }

    fn hit_test(&mut self, request: &HitTestRequest<'_>) -> HitTestOutcome {
        match self {
            Self::Simulated(backend) => backend.hit_test(request),
            Self::MacosObserve => HitTestOutcome::Unresolved,
            Self::MacosBackgroundAxPassive | Self::MacosBackgroundAx(_) => {
                HitTestOutcome::Unresolved
            }
            Self::MacosAxPress(backend) | Self::MacosOwnedValue(backend) => {
                backend.hit_test(request)
            }
            Self::HeadlessBrowser(_) => HitTestOutcome::Unresolved,
        }
    }
}

fn meta(at_ms: u64) -> SnapshotMeta {
    SnapshotMeta {
        snapshot_id: "device-view".into(),
        session_epoch: 1,
        surface: SurfaceRef {
            id: "main".into(),
            epoch: 1,
        },
        clock_domain: "sim/1/monotonic".into(),
        source_sequence: 0,
        observed_ms: at_ms,
    }
}

pub struct SimWorldBackend {
    log: WorldLog,
    follower: WorldFollower,
    clock: SharedClock,
}

impl SimWorldBackend {
    pub fn new(path: impl Into<PathBuf>, clock: SharedClock) -> Self {
        let path = path.into();
        Self {
            log: WorldLog::new(path.clone()),
            follower: WorldFollower::new(path),
            clock,
        }
    }

    /// Take in what others did to the world, such as a dialog that appeared.
    pub fn catch_up(&mut self) {
        let _ = self.follower.catch_up();
    }

    /// Whether the simulated application has a dialog in front, for tests.
    pub fn modal_in_front(&self) -> bool {
        self.follower.app.modal().is_some()
    }
}

impl InputBackend for SimWorldBackend {
    fn submit(
        &mut self,
        at_ms: u64,
        primitive: &Primitive,
        context: &SubmitContext<'_>,
    ) -> SubmitOutcome {
        let entry = WorldEntry::Input {
            at_ms,
            primitive: primitive.clone(),
            binding_fingerprint: context.intent.binding_fingerprint.clone(),
        };
        // The log is the application's input, so an input that could not be
        // written did not happen: that is a definite refusal, not an unknown.
        if self.log.append(&entry).is_err() {
            return SubmitOutcome::Rejected;
        }
        let _ = self.follower.catch_up();
        SubmitOutcome::Applied
    }

    fn release(&mut self, _at_ms: u64, _release: &Primitive) -> Result<(), ReleaseFailure> {
        // Phase 1 submits only atomic primitives, so nothing is ever held.
        Ok(())
    }

    fn held(&self, _token: &str) -> HeldState {
        HeldState::NotHeld
    }

    fn hit_test(&mut self, request: &HitTestRequest<'_>) -> HitTestOutcome {
        let _ = self.follower.catch_up();
        match self.follower.app.hit_test(
            &meta(self.clock.now_ms()),
            request.primitive,
            request
                .context
                .intent
                .binding_fingerprint
                .as_deref()
                .unwrap_or(""),
        ) {
            Some(digest) => HitTestOutcome::Element(digest),
            None => HitTestOutcome::Unresolved,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use interaction_contracts::desktop::primitive::{Modifiers, MouseButton};

    #[test]
    fn native_observation_never_submits_or_releases_physical_input() {
        let mut backend = DeviceBackend::MacosObserve;
        let primitive = Primitive::Click {
            button: MouseButton::Left,
            x: 100,
            y: 100,
            modifiers: Modifiers::default(),
        };
        use interaction_contracts::desktop::{
            device::{ExecutionIntent, ExecutionObservation},
            message::Message,
            samples,
        };
        let messages = samples::all();
        let intent = messages
            .iter()
            .find_map(|(_, envelope)| match &envelope.message {
                Message::SubmitIntent(request) => match &request.intent {
                    ExecutionIntent::Native { intent } => Some(intent.as_ref().clone()),
                    ExecutionIntent::Browser { .. } => None,
                },
                _ => None,
            })
            .unwrap();
        let evidence = messages
            .iter()
            .find_map(|(_, envelope)| match &envelope.message {
                Message::Observation(ExecutionObservation::Native { observation }) => {
                    Some(observation.as_ref().clone())
                }
                _ => None,
            })
            .unwrap();
        let context = SubmitContext {
            intent: &intent,
            authority: execution_core::backend::SubmissionAuthority {
                permit: intent.permit.clone(),
                stop_seq: 0,
                valid_until_ms: 100,
                command_end_ms: 100,
                evidence,
            },
        };
        assert_eq!(
            backend.submit(1, &primitive, &context),
            SubmitOutcome::Rejected
        );
        assert_eq!(
            backend.hit_test(&HitTestRequest {
                context: &context,
                primitive: &primitive
            }),
            HitTestOutcome::Unresolved
        );
        assert_eq!(backend.held("mouse:left"), HeldState::Unknown);
        assert!(backend.release(1, &primitive).is_err());
    }

    #[test]
    fn passive_background_profile_never_submits_or_uses_point_hit_testing() {
        let mut backend = DeviceBackend::MacosBackgroundAxPassive;
        let primitive = Primitive::Click {
            button: MouseButton::Left,
            x: 10,
            y: 10,
            modifiers: Modifiers::default(),
        };
        use interaction_contracts::desktop::{
            device::{ExecutionIntent, ExecutionObservation},
            message::Message,
            samples,
        };
        let messages = samples::all();
        let intent = messages
            .iter()
            .find_map(|(_, envelope)| match &envelope.message {
                Message::SubmitIntent(request) => match &request.intent {
                    ExecutionIntent::Native { intent } => Some(intent.as_ref().clone()),
                    ExecutionIntent::Browser { .. } => None,
                },
                _ => None,
            })
            .unwrap();
        let evidence = messages
            .iter()
            .find_map(|(_, envelope)| match &envelope.message {
                Message::Observation(ExecutionObservation::Native { observation }) => {
                    Some(observation.as_ref().clone())
                }
                _ => None,
            })
            .unwrap();
        let context = SubmitContext {
            intent: &intent,
            authority: execution_core::backend::SubmissionAuthority {
                permit: intent.permit.clone(),
                stop_seq: 0,
                valid_until_ms: 100,
                command_end_ms: 100,
                evidence,
            },
        };
        assert_eq!(
            backend.submit(1, &primitive, &context),
            SubmitOutcome::Rejected
        );
        assert_eq!(
            backend.hit_test(&HitTestRequest {
                context: &context,
                primitive: &primitive,
            }),
            HitTestOutcome::Unresolved
        );
        assert_eq!(backend.held("mouse:left"), HeldState::Unknown);
        assert!(backend.release(1, &primitive).is_err());
    }
}
