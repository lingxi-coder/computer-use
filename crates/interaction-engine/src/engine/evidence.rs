//! Evidence: snapshots in, bindings kept honest, fresh observations asked for
//! (design 6 and 20.1).

use interaction_contracts::desktop::device::DependencyUpdate;
use interaction_contracts::desktop::source::{SourceCommand, SourceHealth, StructureSnapshot};
use interaction_contracts::desktop::task::RootState;

use super::{Engine, EngineError, Output, Step, Wait};
use crate::execution::{DeviceCommand, Expect};
use crate::tasks::budget::BudgetKind;

impl Engine {
    pub(super) fn on_structure(&mut self, snapshot: StructureSnapshot) -> Result<(), EngineError> {
        let pack = self.pack_for(&snapshot.app.bundle_id);
        let had_previous = self.state.latest().is_some();
        let now = self.now_ms;
        let admitted = match self.state.admit(snapshot, now, &pack) {
            Ok(admitted) => admitted,
            Err(reject) => {
                self.note(format!("a snapshot was refused: {reject}"));
                return Ok(());
            }
        };
        let Some(env) = self.state.latest().cloned() else {
            return Ok(());
        };
        // A binding only holds for the evidence it was made from. Anything that
        // no longer fits is dropped, and the device is told so its permits for
        // that binding stop being valid.
        let dropped = self.bindings.revalidate(&env);
        for binding in &dropped {
            self.note(format!(
                "binding for {} dropped: {}",
                binding.semantic_target, binding.reason
            ));
        }
        if admitted.layer_changed && had_previous {
            self.bump_dependency("layer");
        }
        if !dropped.is_empty() {
            self.bump_dependency("binding");
        }
        if let Some(run) = self.run.as_mut() {
            run.verification
                .visits
                .observe(env.page.as_deref(), env.snapshot.observed_ms);
        }
        Ok(())
    }

    pub(super) fn on_health(&mut self, health: SourceHealth) -> Result<(), EngineError> {
        self.state.note_health(health);
        if self.state.structure_unavailable() {
            let dropped = self
                .bindings
                .invalidate_all("the structure source is unavailable");
            if !dropped.is_empty() {
                self.note("the structure source is unavailable; every binding was dropped");
                self.bump_dependency("binding");
            }
        }
        Ok(())
    }

    /// Tell the device a dependency its permits rest on changed.
    pub(super) fn bump_dependency(&mut self, name: &str) {
        if self.device.manifest().is_none() {
            return;
        }
        self.send(
            Expect::Dependency,
            DeviceCommand::UpdateDependency(DependencyUpdate {
                name: name.to_string(),
                valid: Some(true),
            }),
        );
    }

    /// Evidence new enough and complete enough to act on.
    pub(super) fn evidence_usable(&self) -> bool {
        self.state.usable(self.now_ms, self.cfg.evidence_max_age_ms)
    }

    /// Ask the source for a new observation, observed at or after `floor_ms`.
    /// Observations are budgeted like any other work; asking again while one is
    /// awaited only raises what it must satisfy.
    pub(super) fn request_refresh(&mut self, floor_ms: u64, why: &str) -> Step {
        let Some(run) = self.run.as_mut() else {
            return Ok(false);
        };
        if let Some(wait) = run.wait.as_mut() {
            wait.floor_ms = wait.floor_ms.max(floor_ms);
            return Ok(false);
        }
        if !self
            .tasks
            .spend(&mut self.storage, BudgetKind::Observation, 1)?
        {
            self.fail_root("the observation budget is exhausted")?;
            return Ok(true);
        }
        let now = self.now_ms;
        if let Some(run) = self.run.as_mut() {
            run.wait = Some(Wait {
                floor_ms,
                requested_ms: now,
            });
        }
        self.emit(Output::Source(SourceCommand::RefreshNow));
        self.note(format!("observing again: {why}"));
        if self.tasks.active_state() == Some(RootState::Running) {
            self.tasks.await_evidence(&mut self.storage, now)?;
        }
        Ok(true)
    }

    /// Back to work once the evidence that was waited for has arrived.
    pub(super) fn step_waiting_evidence(&mut self) -> Step {
        let floor = self
            .run
            .as_ref()
            .and_then(|run| run.wait)
            .map(|w| w.floor_ms);
        let arrived = match floor {
            Some(floor) => self.state.latest_since(floor).is_some(),
            // Nothing is waited for, so there is nothing to wait for any longer.
            None => true,
        };
        if !arrived {
            return Ok(false);
        }
        if let Some(run) = self.run.as_mut() {
            run.wait = None;
        }
        self.tasks
            .evidence_arrived(&mut self.storage, self.now_ms)?;
        Ok(true)
    }

    /// Ask again when an observation did not arrive in time.
    pub(super) fn retry_stale_wait(&mut self) -> Result<(), EngineError> {
        let now = self.now_ms;
        let limit = self.cfg.refresh_timeout_ms;
        let stale = self
            .run
            .as_ref()
            .and_then(|run| run.wait)
            .is_some_and(|wait| now.saturating_sub(wait.requested_ms) > limit);
        if !stale {
            return Ok(());
        }
        if !self
            .tasks
            .spend(&mut self.storage, BudgetKind::Observation, 1)?
        {
            self.fail_root("the observation budget is exhausted")?;
            return Ok(());
        }
        if let Some(wait) = self.run.as_mut().and_then(|run| run.wait.as_mut()) {
            wait.requested_ms = now;
        }
        self.emit(Output::Source(SourceCommand::RefreshNow));
        self.note("the source has not answered; asking again");
        Ok(())
    }
}
