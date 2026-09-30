//! The simulated input backend: inputs become entries in the world log.

use std::path::PathBuf;

use desktop_ipc::SharedClock;
use execution_core::backend::{
    HeldState, HitTestOutcome, HitTestRequest, InputBackend, ReleaseFailure, SubmitContext,
    SubmitOutcome,
};
use interaction_contracts::desktop::device::SurfaceRef;
use interaction_contracts::desktop::primitive::Primitive;
use sim_world::app::SnapshotMeta;
use sim_world::world_log::{WorldEntry, WorldFollower, WorldLog};

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
            binding_fingerprint: context.binding_fingerprint.map(str::to_string),
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
            request.expected_fingerprint,
        ) {
            Some(digest) => HitTestOutcome::Element(digest),
            None => HitTestOutcome::Unresolved,
        }
    }
}
