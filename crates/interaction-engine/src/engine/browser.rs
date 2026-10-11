use interaction_contracts::desktop::browser::{
    BrowserPageProfile, BrowserSourceControl, BrowserSourceInvalidation,
};
use interaction_contracts::desktop::browser_execution::BrowserExecutionObservation;
use interaction_contracts::desktop::device::SourceFreshness;
use interaction_contracts::desktop::ids::SemanticTargetRef;
use interaction_contracts::desktop::operator::BrowserObservationView;

use super::{BrowserPageAdmissionError, Engine};

impl Engine {
    pub fn admit_browser_page(
        &mut self,
        now_ms: u64,
        snapshot: interaction_contracts::desktop::browser::BrowserPageSnapshot,
    ) -> Result<(), BrowserPageAdmissionError> {
        self.now_ms = self.now_ms.max(now_ms);
        if let Err(rejection) = self.state.admit_browser_snapshot(snapshot, self.now_ms) {
            return match rejection {
                crate::environment::browser::BrowserReject::ReadBeforeFloor => {
                    // A delayed pre-floor read cannot replace current facts or
                    // revoke a newer admitted binding.
                    Err(BrowserPageAdmissionError::ReadBeforeFloor)
                }
                crate::environment::browser::BrowserReject::ForeignScope => {
                    Err(BrowserPageAdmissionError::Rejected)
                }
                _ => {
                    self.browser_read_binding_ref = None;
                    Err(BrowserPageAdmissionError::Rejected)
                }
            };
        }
        // The fixed checkbox descriptor remains a read-only projection even
        // when Host did not enable this profile for a Root. This reference is
        // never input authority: Device evidence construction and Goal
        // compilation still require the Host-frozen profile.
        let profile = self
            .cfg
            .browser_page_profile
            .unwrap_or(BrowserPageProfile::FixtureBackgroundOption);
        let target = SemanticTargetRef::new(profile.semantic_target())
            .map_err(|_| BrowserPageAdmissionError::Rejected)?;
        self.browser_read_binding_ref = self
            .state
            .browser_checkbox_binding(target, profile.label(), self.now_ms, 2_000)
            .map(|binding| binding.binding_ref);
        Ok(())
    }

    /// Called only by the authenticated Supervisor control route. Source data
    /// and decision proposals never install observation scope.
    pub fn control_browser_source(
        &mut self,
        now_ms: u64,
        control: BrowserSourceControl,
    ) -> Result<(), String> {
        control.validate()?;
        self.now_ms = self.now_ms.max(now_ms);
        match control {
            BrowserSourceControl::Install { scope } => {
                self.browser_read_binding_ref = None;
                self.state
                    .install_browser_scope(*scope, self.now_ms)
                    .map_err(|_| "browser read scope rejected".to_string())
            }
            BrowserSourceControl::Invalidate {
                scope_id,
                generation,
                reason,
            } => {
                let matches_current = self.state.browser_scope().is_some_and(|scope| {
                    scope.scope_id == scope_id && scope.generation == generation
                });
                self.state
                    .invalidate_browser_scope(&scope_id, generation, reason, self.now_ms)
                    .map_err(|_| "browser read scope invalidation rejected".to_string())?;
                if matches_current {
                    self.browser_read_binding_ref = None;
                }
                Ok(())
            }
        }
    }

    pub(super) fn retire_browser_source_for_stop(&mut self) {
        self.browser_read_binding_ref = None;
        if let Some(scope) = self.state.browser_scope().cloned() {
            let _ = self.state.invalidate_browser_scope(
                &scope.scope_id,
                scope.generation,
                BrowserSourceInvalidation::Stopped,
                self.now_ms,
            );
        }
    }

    /// Lowering execution mode invalidates Root-bound browser evidence while
    /// keeping the read-only scope available for a fresh Source admission.
    pub(super) fn require_fresh_browser_read_after(&mut self, floor_ms: u64) {
        self.browser_read_binding_ref = None;
        self.state.require_fresh_browser_read_after(floor_ms);
    }

    /// Build the typed device observation only from the current Environment
    /// owner, exact installed page scope, device identity, and live binding.
    /// No native `Observation` fields or window proof are synthesized.
    pub(super) fn browser_execution_observation(
        &mut self,
        profile: BrowserPageProfile,
    ) -> Option<BrowserExecutionObservation> {
        if self.cfg.browser_page_profile != Some(profile) {
            return None;
        }
        let identity = self.device.identity()?.clone();
        let scope = self.state.browser_scope()?.clone();
        let latest = self.state.browser_latest()?.clone();
        if latest.scope_id != scope.scope_id
            || latest.generation != scope.generation
            || identity.session_epoch != scope.context.session_epoch
            || identity.surface.id != scope.context.page_target_id
            || identity.surface.epoch != scope.context.session_epoch
            || identity.clock_domain != scope.clock_domain
            || latest.snapshot.context != scope.context
            || latest.snapshot.clock_domain != scope.clock_domain
            || latest.snapshot.observed_ms > self.now_ms
            || latest.snapshot.read_finished_ms > self.now_ms
            || self.now_ms.saturating_sub(latest.snapshot.observed_ms) >= 2_000
        {
            self.browser_read_binding_ref = None;
            return None;
        }
        let target = SemanticTargetRef::new(profile.semantic_target()).ok()?;
        let binding =
            self.state
                .browser_checkbox_binding(target, profile.label(), self.now_ms, 2_000)?;
        let valid_until_ms = latest.snapshot.observed_ms.checked_add(2_000)?;
        let observation = BrowserExecutionObservation {
            observation_id: latest.snapshot.snapshot_id.clone(),
            surface: identity.surface,
            scope: Box::new(scope),
            environment_revision: latest.revision,
            binding_ref: binding.binding_ref.clone(),
            binding: Box::new(binding.proof),
            valid_until_ms,
            freshness: SourceFreshness::Verified,
            dependencies: self.device.dependencies().clone(),
        };
        observation.validate().ok()?;
        self.browser_read_binding_ref = Some(observation.binding_ref.clone());
        Some(observation)
    }

    pub(super) fn browser_goal_evidence_usable(&self) -> bool {
        let Some(run) = self.run.as_ref() else {
            return false;
        };
        let Some(profile) = run.goal.browser_page_profile else {
            return false;
        };
        if self.cfg.browser_page_profile != Some(profile)
            || run.goal.scope.app != profile.app()
            || run.goal.target.semantic_target.as_str() != profile.semantic_target()
        {
            return false;
        }
        let Some(identity) = self.device.identity() else {
            return false;
        };
        let Some(scope) = self.state.browser_scope() else {
            return false;
        };
        let Some(latest) = self.state.browser_latest() else {
            return false;
        };
        let Some(reference) = self.browser_read_binding_ref.as_ref() else {
            return false;
        };
        let Some(binding) = self.state.current_browser_binding(reference) else {
            return false;
        };
        scope.validate().is_ok()
            && latest.scope_id == scope.scope_id
            && latest.generation == scope.generation
            && latest.snapshot.context == scope.context
            && latest.snapshot.clock_domain == scope.clock_domain
            && identity.session_epoch == scope.context.session_epoch
            && identity.surface.id == scope.context.page_target_id
            && identity.surface.epoch == scope.context.session_epoch
            && identity.clock_domain == scope.clock_domain
            && latest.snapshot.observed_ms <= self.now_ms
            && latest.snapshot.read_finished_ms <= self.now_ms
            && self.now_ms.saturating_sub(latest.snapshot.observed_ms) < 2_000
            && binding.semantic_target.as_str() == profile.semantic_target()
            && binding.proof.snapshot_id == latest.snapshot.snapshot_id
            && binding.proof.role == profile.role()
            && binding.proof.name == profile.label()
            && binding.proof.disabled
                == interaction_contracts::desktop::browser::BrowserObservedBool::False
            && binding.proof.validate().is_ok()
    }

    pub(super) fn browser_observation_view(&self) -> Option<BrowserObservationView> {
        let current = self.state.browser_latest()?;
        let snapshot = &current.snapshot;
        if snapshot.observed_ms > self.now_ms
            || self.now_ms.saturating_sub(snapshot.observed_ms) >= 2_000
        {
            return None;
        }
        let scope = self.state.browser_scope()?.clone();
        let binding = self
            .browser_read_binding_ref
            .as_ref()
            .and_then(|reference| self.state.current_browser_binding(reference));
        let view = BrowserObservationView {
            scope,
            snapshot_id: snapshot.snapshot_id.clone(),
            revision: current.revision,
            source_sequence: snapshot.source_sequence,
            observed_ms: snapshot.observed_ms,
            read_finished_ms: snapshot.read_finished_ms,
            target_context_hash: snapshot.target_context_hash.clone(),
            node_count: snapshot.nodes.len() as u64,
            readonly_binding_ref: binding.as_ref().map(|binding| binding.binding_ref.clone()),
            readonly_checked: binding.as_ref().map(|binding| binding.proof.checked),
        };
        view.validate().ok()?;
        Some(view)
    }
}
