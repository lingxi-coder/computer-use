//! Private, single-action browser device channel contracts.
//!
//! These DTOs are intended for the inherited private device pipe only. They
//! are not public desktop `Message` variants and carry no native window,
//! coordinate, or generic primitive representation.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

use super::browser::{
    BrowserObservedBool, BrowserPageProfile, BrowserReadScope, MAX_BROWSER_SAFE_INTEGER,
    browser_contract_golden_snapshots, derive_browser_checkbox_binding, valid_scope_uuid,
};
use super::browser_execution::{BrowserAction, BrowserActionIntent, BrowserExecutionObservation};
use super::device::{PermitRef, SourceFreshness, SurfaceRef};
use super::error::FailureCode;
use super::ids::BindingRef;
use super::require_nonempty;

pub const BROWSER_CONTROL_VERSION: u16 = 1;
pub const MAX_BROWSER_CONTROL_BOOTSTRAP_BYTES: usize = 16 * 1024;
pub const MAX_BROWSER_CONTROL_SUBMIT_BYTES: usize = 64 * 1024;
pub const MAX_BROWSER_CONTROL_REPLY_BYTES: usize = 4 * 1024;
const MAX_CONTROL_ID_BYTES: usize = 256;
const MAX_REPLY_GRACE_MS: u64 = 500;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct BrowserControlGoldenFrames {
    pub bootstrap: BrowserControlBootstrap,
    pub hello: BrowserControlHello,
    pub ready: BrowserControlReady,
    pub submit: BrowserControlSubmit,
    pub reply: BrowserControlReply,
}

/// Synthetic, public-only private-channel fixture used by schema/export and
/// cross-language conformance tests. It never contains a live channel token.
pub fn browser_control_contract_golden_frames() -> Result<BrowserControlGoldenFrames, String> {
    let mut snapshots = browser_contract_golden_snapshots()?;
    let mut authority_snapshot = snapshots.remove(0);
    authority_snapshot.snapshot_id = "control-authority-read".into();
    authority_snapshot.source_sequence = 10;
    authority_snapshot.observed_ms = 110;
    authority_snapshot.read_finished_ms = 115;
    make_fixture_checkbox_known(&mut authority_snapshot)?;

    let mut execution_snapshot = authority_snapshot.clone();
    execution_snapshot.snapshot_id = "control-execution-read".into();
    execution_snapshot.source_sequence = 11;
    execution_snapshot.observed_ms = 120;
    execution_snapshot.read_finished_ms = 125;
    execution_snapshot.validate()?;

    let scope = BrowserReadScope {
        scope_id: "550e8400-e29b-41d4-a716-446655440000".into(),
        generation: 1,
        context: authority_snapshot.context.clone(),
        clock_domain: authority_snapshot.clock_domain.clone(),
        issued_ms: 100,
    };
    scope.validate()?;
    let authority_evidence =
        control_observation(&authority_snapshot, &scope, "control-authority-binding", 1)?;
    let execution_evidence =
        control_observation(&execution_snapshot, &scope, "control-execution-binding", 2)?;
    let intent = BrowserActionIntent {
        intent_id: "control-intent-1".into(),
        attempt_id: "control-attempt-1".into(),
        task_id: "control-task-1".into(),
        task_revision: 1,
        root_epoch: 1,
        session_epoch: scope.context.session_epoch,
        permit: PermitRef {
            id: "control-permit-1".into(),
            generation: 1,
        },
        evidence: Box::new(execution_evidence),
        latest_start_ms: 300,
        duration_ms: 10,
        action: BrowserAction::SetCheckbox { checked: true },
    };
    intent.validate()?;
    let authority = BrowserControlAuthority {
        task_id: intent.task_id.clone(),
        task_revision: intent.task_revision,
        root_epoch: intent.root_epoch,
        attempt_id: intent.attempt_id.clone(),
        permit: intent.permit.clone(),
        stop_seq: 0,
        command_start_ms: 130,
        command_end_ms: 140,
        valid_until_ms: authority_evidence.valid_until_ms,
        reply_deadline_ms: 160,
    };
    let submit = BrowserControlSubmit {
        version: BROWSER_CONTROL_VERSION,
        request_id: "550e8400-e29b-41d4-a716-446655440001".into(),
        channel_generation: 1,
        session_epoch: scope.context.session_epoch,
        scope_id: scope.scope_id.clone(),
        scope_generation: scope.generation,
        sequence: 1,
        intent: Box::new(intent),
        authority: Box::new(authority),
        authority_evidence: Box::new(authority_evidence),
    };
    submit.validate()?;

    let bootstrap = BrowserControlBootstrap {
        version: BROWSER_CONTROL_VERSION,
        token: BrowserControlToken::new("11".repeat(32))?,
        channel_generation: 1,
        browser_page_profile: BrowserPageProfile::FixtureBackgroundOption,
        scope: Box::new(scope),
    };
    bootstrap.validate()?;
    let hello = BrowserControlHello {
        version: BROWSER_CONTROL_VERSION,
        token: bootstrap.token.clone(),
        channel_generation: bootstrap.channel_generation,
        session_epoch: bootstrap.scope.context.session_epoch,
        browser_page_profile: bootstrap.browser_page_profile,
        scope_id: bootstrap.scope.scope_id.clone(),
        scope_generation: bootstrap.scope.generation,
    };
    hello.validate_against(&bootstrap)?;
    let ready = BrowserControlReady {
        version: BROWSER_CONTROL_VERSION,
        channel_generation: bootstrap.channel_generation,
        session_epoch: bootstrap.scope.context.session_epoch,
        scope_id: bootstrap.scope.scope_id.clone(),
        scope_generation: bootstrap.scope.generation,
        next_sequence: 1,
    };
    ready.validate_against(&hello)?;
    let reply = BrowserControlReply {
        version: BROWSER_CONTROL_VERSION,
        request_id: submit.request_id.clone(),
        channel_generation: submit.channel_generation,
        session_epoch: submit.session_epoch,
        scope_id: submit.scope_id.clone(),
        scope_generation: submit.scope_generation,
        sequence: submit.sequence,
        intent_id: submit.intent.intent_id.clone(),
        outcome: BrowserControlOutcome::Completed,
        submitted_at_ms: Some(130),
        input_end_ms: Some(140),
        failure_code: None,
    };
    reply.validate_against(&submit)?;
    Ok(BrowserControlGoldenFrames {
        bootstrap,
        hello,
        ready,
        submit,
        reply,
    })
}

fn make_fixture_checkbox_known(
    snapshot: &mut super::browser::BrowserPageSnapshot,
) -> Result<(), String> {
    let checkbox = snapshot
        .nodes
        .iter_mut()
        .find(|node| {
            node.role.as_deref() == Some("checkbox")
                && node.name.as_deref() == Some("Background option")
        })
        .ok_or("synthetic browser control fixture lacks its checkbox")?;
    checkbox.disabled = BrowserObservedBool::False;
    checkbox.checked = BrowserObservedBool::False;
    snapshot.validate()
}

fn control_observation(
    snapshot: &super::browser::BrowserPageSnapshot,
    scope: &BrowserReadScope,
    binding_id: &str,
    environment_revision: u64,
) -> Result<BrowserExecutionObservation, String> {
    let binding = derive_browser_checkbox_binding(snapshot, "Background option")?;
    let observation = BrowserExecutionObservation {
        observation_id: snapshot.snapshot_id.clone(),
        surface: SurfaceRef {
            id: scope.context.page_target_id.clone(),
            epoch: scope.context.session_epoch,
        },
        scope: Box::new(scope.clone()),
        environment_revision,
        binding_ref: BindingRef::new(binding_id).map_err(|error| error.to_string())?,
        binding: Box::new(binding),
        valid_until_ms: snapshot.observed_ms + 1_500,
        freshness: SourceFreshness::Verified,
        dependencies: std::collections::BTreeMap::from([("browser_scope".into(), 1)]),
    };
    observation.validate()?;
    Ok(observation)
}

/// One-use channel credential. Its wire form is a 32-byte lowercase-hex
/// string, but formatting it for logs always redacts the value.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
#[schemars(transparent)]
pub struct BrowserControlToken(#[schemars(length(min = 64, max = 64))] String);

impl BrowserControlToken {
    pub fn new(value: impl Into<String>) -> Result<Self, String> {
        let token = Self(value.into());
        token.validate()?;
        Ok(token)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.0.len() != 64
            || !self
                .0
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("browser control token must be 32 bytes encoded as lowercase hex".into());
        }
        Ok(())
    }
}

impl fmt::Debug for BrowserControlToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BrowserControlToken([REDACTED])")
    }
}

/// Parent-to-child bootstrap over inherited fd3. The token is never carried
/// in argv, environment variables, stdout, or the public desktop envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BrowserControlBootstrap {
    #[schemars(range(min = 1, max = 1))]
    pub version: u16,
    pub token: BrowserControlToken,
    #[schemars(range(min = 1, max = 9_007_199_254_740_991u64))]
    pub channel_generation: u64,
    pub browser_page_profile: BrowserPageProfile,
    pub scope: Box<BrowserReadScope>,
}

impl BrowserControlBootstrap {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != BROWSER_CONTROL_VERSION {
            return Err("unsupported browser control bootstrap version".into());
        }
        self.token.validate()?;
        validate_positive_safe(
            "browser control channel_generation",
            self.channel_generation,
        )?;
        self.scope.validate()?;
        ensure_profile_context(self.browser_page_profile, &self.scope)?;
        validate_json_bound(self, MAX_BROWSER_CONTROL_BOOTSTRAP_BYTES, "bootstrap")
    }
}

/// Child-to-parent acknowledgement of the exact private bootstrap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BrowserControlHello {
    #[schemars(range(min = 1, max = 1))]
    pub version: u16,
    pub token: BrowserControlToken,
    #[schemars(range(min = 1, max = 9_007_199_254_740_991u64))]
    pub channel_generation: u64,
    #[schemars(range(min = 1, max = 9_007_199_254_740_991u64))]
    pub session_epoch: u64,
    pub browser_page_profile: BrowserPageProfile,
    #[schemars(length(min = 36, max = 36))]
    pub scope_id: String,
    #[schemars(range(min = 1, max = 9_007_199_254_740_991u64))]
    pub scope_generation: u64,
}

impl BrowserControlHello {
    pub fn validate_shape(&self) -> Result<(), String> {
        if self.version != BROWSER_CONTROL_VERSION {
            return Err("unsupported browser control hello version".into());
        }
        self.token.validate()?;
        validate_positive_safe(
            "browser control channel_generation",
            self.channel_generation,
        )?;
        validate_positive_safe("browser control session_epoch", self.session_epoch)?;
        validate_positive_safe("browser control scope_generation", self.scope_generation)?;
        if !valid_scope_uuid(&self.scope_id) {
            return Err("browser control hello scope_id must be a canonical UUID".into());
        }
        validate_json_bound(self, MAX_BROWSER_CONTROL_BOOTSTRAP_BYTES, "hello")
    }

    pub fn validate_against(&self, bootstrap: &BrowserControlBootstrap) -> Result<(), String> {
        bootstrap.validate()?;
        self.validate_shape()?;
        if self.version != BROWSER_CONTROL_VERSION
            || !constant_time_eq(
                self.token.as_str().as_bytes(),
                bootstrap.token.as_str().as_bytes(),
            )
            || self.channel_generation != bootstrap.channel_generation
            || self.session_epoch != bootstrap.scope.context.session_epoch
            || self.browser_page_profile != bootstrap.browser_page_profile
            || self.scope_id != bootstrap.scope.scope_id
            || self.scope_generation != bootstrap.scope.generation
        {
            return Err("browser control hello does not match its private bootstrap".into());
        }
        Ok(())
    }
}

/// Parent-to-child acknowledgement that the private channel is ready for its
/// first strictly sequenced request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BrowserControlReady {
    #[schemars(range(min = 1, max = 1))]
    pub version: u16,
    #[schemars(range(min = 1, max = 9_007_199_254_740_991u64))]
    pub channel_generation: u64,
    #[schemars(range(min = 1, max = 9_007_199_254_740_991u64))]
    pub session_epoch: u64,
    #[schemars(length(min = 36, max = 36))]
    pub scope_id: String,
    #[schemars(range(min = 1, max = 9_007_199_254_740_991u64))]
    pub scope_generation: u64,
    #[schemars(range(min = 1, max = 9_007_199_254_740_991u64))]
    pub next_sequence: u64,
}

impl BrowserControlReady {
    pub fn validate_against(&self, hello: &BrowserControlHello) -> Result<(), String> {
        hello.validate_shape()?;
        if self.version != BROWSER_CONTROL_VERSION
            || self.channel_generation != hello.channel_generation
            || self.session_epoch != hello.session_epoch
            || self.scope_id != hello.scope_id
            || self.scope_generation != hello.scope_generation
            || self.next_sequence != 1
        {
            return Err("browser control ready does not match its hello or first sequence".into());
        }
        validate_json_bound(self, MAX_BROWSER_CONTROL_BOOTSTRAP_BYTES, "ready")
    }
}

/// Core-issued authority copied beside the exact intent for Device-side
/// revalidation. `command_start_ms` is the accepted start, while
/// `latest_start_ms` remains the intent's admission deadline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BrowserControlAuthority {
    #[schemars(length(min = 1, max = 256))]
    pub task_id: String,
    #[schemars(range(min = 1, max = 9_007_199_254_740_991u64))]
    pub task_revision: u64,
    #[schemars(range(min = 1, max = 9_007_199_254_740_991u64))]
    pub root_epoch: u64,
    #[schemars(length(min = 1, max = 256))]
    pub attempt_id: String,
    pub permit: PermitRef,
    #[schemars(range(max = 9_007_199_254_740_991u64))]
    pub stop_seq: u64,
    #[schemars(range(max = 9_007_199_254_740_991u64))]
    pub command_start_ms: u64,
    #[schemars(range(max = 9_007_199_254_740_991u64))]
    pub command_end_ms: u64,
    #[schemars(range(max = 9_007_199_254_740_991u64))]
    pub valid_until_ms: u64,
    #[schemars(range(max = 9_007_199_254_740_991u64))]
    pub reply_deadline_ms: u64,
}

/// One private, correlated browser action. `authority_evidence` preserves the
/// evidence that Core used when it issued the permit; it may predate the
/// intent's fresher execution evidence, but must identify the same target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BrowserControlSubmit {
    #[schemars(range(min = 1, max = 1))]
    pub version: u16,
    #[schemars(length(min = 36, max = 36))]
    pub request_id: String,
    #[schemars(range(min = 1, max = 9_007_199_254_740_991u64))]
    pub channel_generation: u64,
    #[schemars(range(min = 1, max = 9_007_199_254_740_991u64))]
    pub session_epoch: u64,
    #[schemars(length(min = 36, max = 36))]
    pub scope_id: String,
    #[schemars(range(min = 1, max = 9_007_199_254_740_991u64))]
    pub scope_generation: u64,
    #[schemars(range(min = 1, max = 9_007_199_254_740_991u64))]
    pub sequence: u64,
    pub intent: Box<BrowserActionIntent>,
    pub authority: Box<BrowserControlAuthority>,
    pub authority_evidence: Box<BrowserExecutionObservation>,
}

impl BrowserControlSubmit {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != BROWSER_CONTROL_VERSION || !valid_scope_uuid(&self.request_id) {
            return Err("browser control submit version or request ID is invalid".into());
        }
        validate_positive_safe(
            "browser control channel_generation",
            self.channel_generation,
        )?;
        validate_positive_safe("browser control session_epoch", self.session_epoch)?;
        validate_positive_safe("browser control scope_generation", self.scope_generation)?;
        validate_positive_safe("browser control sequence", self.sequence)?;
        if !valid_scope_uuid(&self.scope_id) {
            return Err("browser control scope_id must be a canonical UUID".into());
        }
        self.intent.validate()?;
        self.authority_evidence.validate()?;
        validate_authority(&self.intent, &self.authority, &self.authority_evidence)?;
        if self.session_epoch != self.intent.session_epoch
            || self.scope_id != self.intent.evidence.scope.scope_id
            || self.scope_generation != self.intent.evidence.scope.generation
            || self.intent.evidence.scope.context.session_epoch != self.session_epoch
        {
            return Err("browser control submit differs from its intent session or scope".into());
        }
        validate_json_bound(self, MAX_BROWSER_CONTROL_SUBMIT_BYTES, "submit")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BrowserControlOutcome {
    Completed,
    Rejected,
    Unknown,
}

/// Child-to-parent result correlated with exactly one request and sequence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BrowserControlReply {
    #[schemars(range(min = 1, max = 1))]
    pub version: u16,
    #[schemars(length(min = 36, max = 36))]
    pub request_id: String,
    #[schemars(range(min = 1, max = 9_007_199_254_740_991u64))]
    pub channel_generation: u64,
    #[schemars(range(min = 1, max = 9_007_199_254_740_991u64))]
    pub session_epoch: u64,
    #[schemars(length(min = 36, max = 36))]
    pub scope_id: String,
    #[schemars(range(min = 1, max = 9_007_199_254_740_991u64))]
    pub scope_generation: u64,
    #[schemars(range(min = 1, max = 9_007_199_254_740_991u64))]
    pub sequence: u64,
    #[schemars(length(min = 1, max = 256))]
    pub intent_id: String,
    pub outcome: BrowserControlOutcome,
    #[schemars(range(max = 9_007_199_254_740_991u64))]
    pub submitted_at_ms: Option<u64>,
    #[schemars(range(max = 9_007_199_254_740_991u64))]
    pub input_end_ms: Option<u64>,
    pub failure_code: Option<FailureCode>,
}

impl BrowserControlReply {
    pub fn validate_against(&self, submit: &BrowserControlSubmit) -> Result<(), String> {
        submit.validate()?;
        if self.version != BROWSER_CONTROL_VERSION
            || self.request_id != submit.request_id
            || self.channel_generation != submit.channel_generation
            || self.session_epoch != submit.session_epoch
            || self.scope_id != submit.scope_id
            || self.scope_generation != submit.scope_generation
            || self.sequence != submit.sequence
            || self.intent_id != submit.intent.intent_id
        {
            return Err("browser control reply does not correlate to its submit".into());
        }
        for value in [self.submitted_at_ms, self.input_end_ms]
            .into_iter()
            .flatten()
        {
            if value > MAX_BROWSER_SAFE_INTEGER {
                return Err("browser control reply time is not JS-safe".into());
            }
        }
        match self.outcome {
            BrowserControlOutcome::Completed => {
                let submitted = self
                    .submitted_at_ms
                    .ok_or("completed reply lacks submit time")?;
                let ended = self
                    .input_end_ms
                    .ok_or("completed reply lacks input end time")?;
                if self.failure_code.is_some()
                    || submitted < submit.authority.command_start_ms
                    || ended < submitted
                    || ended > submit.authority.command_end_ms
                    || ended > submit.authority.reply_deadline_ms
                {
                    return Err("completed browser reply times or failure code are invalid".into());
                }
            }
            BrowserControlOutcome::Rejected => {
                if self.submitted_at_ms.is_some()
                    || self.input_end_ms.is_some()
                    || self.failure_code.is_none()
                {
                    return Err("rejected browser reply must carry only a failure code".into());
                }
            }
            BrowserControlOutcome::Unknown => {
                if self.submitted_at_ms.is_none() && self.input_end_ms.is_none() {
                    return Err("unknown browser reply must preserve an observed boundary".into());
                }
                if self
                    .submitted_at_ms
                    .zip(self.input_end_ms)
                    .is_some_and(|(submitted, ended)| ended < submitted)
                {
                    return Err("unknown browser reply has reversed input times".into());
                }
            }
        }
        validate_json_bound(self, MAX_BROWSER_CONTROL_REPLY_BYTES, "reply")
    }
}

fn validate_authority(
    intent: &BrowserActionIntent,
    authority: &BrowserControlAuthority,
    authority_evidence: &BrowserExecutionObservation,
) -> Result<(), String> {
    for (field, value) in [
        ("browser authority task_id", authority.task_id.as_str()),
        (
            "browser authority attempt_id",
            authority.attempt_id.as_str(),
        ),
        ("browser authority permit id", authority.permit.id.as_str()),
    ] {
        require_bounded_text(field, value, MAX_CONTROL_ID_BYTES)?;
    }
    for (field, value) in [
        ("browser authority task_revision", authority.task_revision),
        ("browser authority root_epoch", authority.root_epoch),
        ("browser authority stop_seq", authority.stop_seq),
        (
            "browser authority command_start_ms",
            authority.command_start_ms,
        ),
        ("browser authority command_end_ms", authority.command_end_ms),
        ("browser authority valid_until_ms", authority.valid_until_ms),
        (
            "browser authority reply_deadline_ms",
            authority.reply_deadline_ms,
        ),
    ] {
        if value > MAX_BROWSER_SAFE_INTEGER {
            return Err(format!("{field} is not JS-safe"));
        }
    }
    if authority.task_revision == 0
        || authority.root_epoch == 0
        || authority.permit.generation == 0
        || authority.permit.generation > MAX_BROWSER_SAFE_INTEGER
        || authority.task_id != intent.task_id
        || authority.task_revision != intent.task_revision
        || authority.root_epoch != intent.root_epoch
        || authority.attempt_id != intent.attempt_id
        || authority.permit != intent.permit
    {
        return Err("browser control authority does not match its intent identity".into());
    }
    authority_evidence.validate()?;
    let permit_observation = intent.evidence.as_ref();
    let permit_binding = &authority_evidence.binding;
    let current_binding = &permit_observation.binding;
    let permit_sequence = authority_evidence.source_sequence();
    let current_sequence = permit_observation.source_sequence();
    let same_read = permit_sequence == current_sequence
        && authority_evidence.observation_id == permit_observation.observation_id
        && authority_evidence.observed_ms() == permit_observation.observed_ms()
        && permit_binding.read_finished_ms == current_binding.read_finished_ms;
    let read_order_invalid = permit_sequence > current_sequence
        || (permit_sequence == current_sequence && !same_read)
        || (permit_sequence < current_sequence
            && (permit_observation.observed_ms() < permit_binding.read_finished_ms
                || authority_evidence.observation_id == permit_observation.observation_id));
    if authority_evidence.scope.as_ref() != permit_observation.scope.as_ref()
        || authority_evidence.clock_domain() != permit_observation.clock_domain()
        || permit_binding.context != current_binding.context
        || permit_binding.target_context_hash != current_binding.target_context_hash
        || permit_binding.ax_node_id != current_binding.ax_node_id
        || permit_binding.backend_dom_node_id != current_binding.backend_dom_node_id
        || permit_binding.role != current_binding.role
        || permit_binding.name != current_binding.name
        || permit_binding.disabled != current_binding.disabled
        || permit_binding.checked != current_binding.checked
        || read_order_invalid
    {
        return Err(
            "browser permit evidence and execution evidence identify different targets".into(),
        );
    }
    let action_end = authority
        .command_start_ms
        .checked_add(intent.duration_ms)
        .filter(|time| *time <= MAX_BROWSER_SAFE_INTEGER)
        .ok_or("browser authority command window overflows its time range")?;
    // The reply grace bounds acknowledgement waiting only. It never extends
    // the Core authority's input window, which ends at `command_end_ms`.
    let maximum_reply_deadline = authority
        .valid_until_ms
        .checked_add(MAX_REPLY_GRACE_MS)
        .filter(|time| *time <= MAX_BROWSER_SAFE_INTEGER)
        .unwrap_or(MAX_BROWSER_SAFE_INTEGER);
    if authority.command_start_ms < permit_observation.binding.read_finished_ms
        || authority.command_start_ms > intent.latest_start_ms
        || authority.command_end_ms != action_end
        || authority.valid_until_ms < authority.command_end_ms
        || authority.valid_until_ms > permit_observation.valid_until_ms
        || authority.valid_until_ms > authority_evidence.valid_until_ms
        || authority.reply_deadline_ms < authority.command_end_ms
        || authority.reply_deadline_ms > maximum_reply_deadline
    {
        return Err("browser control authority has an invalid bounded command window".into());
    }
    if matches!(intent.action, BrowserAction::SetCheckbox { checked } if checked == permit_binding.checked)
    {
        return Err("browser action does not change the Core-authorized checkbox value".into());
    }
    Ok(())
}

fn ensure_profile_context(
    profile: BrowserPageProfile,
    scope: &BrowserReadScope,
) -> Result<(), String> {
    scope.validate()?;
    if !scope
        .context
        .fixture_file_url
        .ends_with("/fixtures/headless/readonly-page.html")
        || profile != BrowserPageProfile::FixtureBackgroundOption
    {
        return Err("browser control profile is not the owned static fixture".into());
    }
    Ok(())
}

fn validate_positive_safe(field: &str, value: u64) -> Result<(), String> {
    if value == 0 || value > MAX_BROWSER_SAFE_INTEGER {
        return Err(format!("{field} must be positive and JS-safe"));
    }
    Ok(())
}

fn require_bounded_text(field: &str, value: &str, max_bytes: usize) -> Result<(), String> {
    require_nonempty(field, value)?;
    if value.len() > max_bytes {
        return Err(format!("{field} exceeds its UTF-8 byte bound"));
    }
    Ok(())
}

fn validate_json_bound<T: Serialize>(
    value: &T,
    max_bytes: usize,
    name: &str,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| format!("browser control {name} could not be serialized"))?;
    if bytes.len() > max_bytes {
        return Err(format!(
            "browser control {name} exceeds its serialized byte bound"
        ));
    }
    Ok(())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_channel_golden_frames_validate_and_keep_authority_evidence_separate() {
        let frames = browser_control_contract_golden_frames().unwrap();
        frames.bootstrap.validate().unwrap();
        frames.hello.validate_against(&frames.bootstrap).unwrap();
        frames.ready.validate_against(&frames.hello).unwrap();
        frames.submit.validate().unwrap();
        frames.reply.validate_against(&frames.submit).unwrap();

        assert_ne!(
            frames.submit.authority_evidence.observation_id,
            frames.submit.intent.evidence.observation_id
        );
        assert_eq!(
            frames.submit.authority_evidence.binding.ax_node_id,
            frames.submit.intent.evidence.binding.ax_node_id
        );
        assert_eq!(
            frames.submit.authority_evidence.binding.checked,
            frames.submit.intent.evidence.binding.checked
        );
    }

    #[test]
    fn private_channel_rejects_cross_scope_sequence_and_reply_correlation() {
        let frames = browser_control_contract_golden_frames().unwrap();

        let mut changed_session = frames.submit.clone();
        changed_session.session_epoch += 1;
        assert!(changed_session.validate().is_err());

        let mut unsafe_session = frames.submit.clone();
        unsafe_session.session_epoch = MAX_BROWSER_SAFE_INTEGER + 1;
        assert!(unsafe_session.validate().is_err());

        let mut malformed_request_id = frames.submit.clone();
        malformed_request_id.request_id = "not-a-uuid".into();
        assert!(malformed_request_id.validate().is_err());

        let mut changed_document = frames.submit.clone();
        changed_document
            .authority_evidence
            .scope
            .context
            .document_loader_id = "replacement-document".into();
        assert!(changed_document.validate().is_err());

        let mut changed_node = frames.submit.clone();
        changed_node.authority_evidence.binding.backend_dom_node_id += 1;
        assert!(changed_node.validate().is_err());

        let mut unchanged_action_value = frames.submit.clone();
        unchanged_action_value.authority_evidence.binding.checked = true;
        unchanged_action_value.intent.evidence.binding.checked = true;
        assert!(unchanged_action_value.validate().is_err());

        let mut newer_authority_evidence = frames.submit.clone();
        newer_authority_evidence
            .authority_evidence
            .binding
            .source_sequence += 2;
        assert!(newer_authority_evidence.validate().is_err());

        let mut crossed_reply = frames.reply;
        crossed_reply.sequence += 1;
        assert!(crossed_reply.validate_against(&frames.submit).is_err());

        let mut maximum_grace = frames.submit;
        maximum_grace.authority.reply_deadline_ms =
            maximum_grace.authority.valid_until_ms + MAX_REPLY_GRACE_MS;
        assert!(maximum_grace.validate().is_ok());

        let mut too_long_reply = maximum_grace;
        too_long_reply.authority.reply_deadline_ms =
            too_long_reply.authority.valid_until_ms + MAX_REPLY_GRACE_MS + 1;
        assert!(too_long_reply.validate().is_err());
    }

    #[test]
    fn token_debug_and_validation_never_expose_or_accept_a_secret() {
        let token_text = "a7".repeat(32);
        let token = BrowserControlToken::new(token_text.clone()).unwrap();
        assert!(!format!("{token:?}").contains(&token_text));
        assert!(BrowserControlToken::new("secret-token-not-hex".to_owned()).is_err());

        let frames = browser_control_contract_golden_frames().unwrap();
        let token_debug = format!("{:?}", frames.bootstrap);
        assert!(!token_debug.contains(frames.bootstrap.token.as_str()));
    }

    #[test]
    fn private_channel_wire_dtos_reject_unknown_fields() {
        let frames = browser_control_contract_golden_frames().unwrap();
        let mut encoded = serde_json::to_value(frames.submit).unwrap();
        encoded["native_window_id"] = serde_json::json!("forbidden");
        assert!(serde_json::from_value::<BrowserControlSubmit>(encoded).is_err());
    }
}
