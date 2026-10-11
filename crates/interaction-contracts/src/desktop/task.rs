//! Root tasks: the frozen goal, its states and the attempt admission messages
//! (design 11.2, 13.3 and 13.5).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::browser::BrowserPageProfile;
use super::decision::DecisionSource;
use super::domain::ExecutionDomain;
use super::effect::EffectClass;
use super::error::FailureCode;
use super::ids::{AttemptId, DecisionId, RootTaskId, SemanticTargetRef};
use super::require_nonempty;

const MAX_ROW_SCOPE_LABEL_BYTES: usize = 256;
const MAX_ROW_COLUMN_NAME_BYTES: usize = 256;
const MAX_ROW_CONDITION_TEXT_BYTES: usize = 2_048;
const MAX_ROW_CONDITIONS: usize = 32;
const MAX_ROW_TABLE_ANCESTORS: usize = 8;
const MAX_ROW_TABLE_HEADERS: usize = 64;
const MAX_ROW_CONDITION_INTEGER: i64 = 1_000_000_000_000;
/// Largest magnitude that can be represented exactly by Source's f64 number
/// value while preserving an integer field's decimal meaning.
pub const MAX_SCOPED_FORM_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExpectedValue {
    Bool { value: bool },
    Text { value: String },
    Number { value: i64 },
}

/// How much the user asked to have confirmed. The two are separate claims:
/// seeing the state again in the UI is not proof that it was committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmationScope {
    /// Navigation postconditions only; root goals still require revisit or commit.
    CurrentUi,
    UiRevisit,
    Commit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TaskScope {
    #[schemars(length(min = 1))]
    pub app: String,
    pub account: Option<String>,
}

/// One user-named control inside an allowed application scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AllowedControlTarget {
    #[schemars(length(min = 1, max = 512))]
    pub control: String,
    #[schemars(length(max = 256))]
    pub group: Option<String>,
    /// A trusted form context. `group` and `form` are alternate encodings of
    /// the one public context label, never two independent authorities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 256))]
    pub form: Option<String>,
    /// V1 only accepts the same exact group value; other disambiguators have
    /// no trusted live binding and are rejected instead of being ignored.
    #[schemars(length(max = 256))]
    pub disambiguator: Option<String>,
}

impl AllowedControlTarget {
    /// Context represented by the live target's public group field.
    pub fn context_label(&self) -> Option<&str> {
        self.group.as_deref().or(self.form.as_deref())
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_scope_text("allowed control", &self.control, 512)?;
        if let Some(group) = &self.group {
            validate_scope_text("allowed control group", group, 256)?;
        }
        if let Some(form) = &self.form {
            validate_scope_text("allowed control form", form, 256)?;
        }
        if self.group.is_some() && self.form.is_some() {
            return Err("allowed control target cannot name both a group and a form".into());
        }
        if let Some(disambiguator) = &self.disambiguator {
            validate_scope_text("allowed control disambiguator", disambiguator, 256)?;
            if self.group.as_deref() != Some(disambiguator.as_str()) {
                return Err("v1 control disambiguator must equal its bound group".into());
            }
        }
        Ok(())
    }
}

/// Immutable user authority for a single control in one trusted application.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AllowedControlScope {
    /// Exact trusted pack display name or bundle id, resolved once by Host.
    #[schemars(length(min = 1, max = 256))]
    pub application: String,
    #[schemars(length(min = 1, max = 1))]
    pub targets: Vec<AllowedControlTarget>,
    pub no_unrelated_controls: bool,
}

impl AllowedControlScope {
    pub fn validate(&self) -> Result<(), String> {
        validate_scope_text("allowed application", &self.application, 256)?;
        if self.targets.len() != 1 {
            return Err("v1 allowed control scope must name exactly one target".into());
        }
        if !self.no_unrelated_controls {
            return Err("v1 allowed control scope must forbid unrelated controls".into());
        }
        self.targets[0].validate()
    }
}

fn validate_scope_text(field: &str, value: &str, max_bytes: usize) -> Result<(), String> {
    require_nonempty(field, value)?;
    if value.len() > max_bytes {
        return Err(format!("{field} exceeds its UTF-8 byte bound"));
    }
    Ok(())
}

/// Provenance selected by Main from a pinned goal-only catalog. These fields
/// identify an admitted request; they grant no execution authority and are not
/// part of the planner's input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FrozenTaskProfile {
    pub catalog_id: String,
    pub selector: String,
    pub source_freeze_sha256: String,
    pub source_request_sha256: String,
    pub goals_sha256: String,
    pub suite: String,
    pub task_id: String,
    pub seed: u32,
}

impl FrozenTaskProfile {
    pub fn validate(&self) -> Result<(), String> {
        for hash in [
            &self.catalog_id,
            &self.selector,
            &self.source_freeze_sha256,
            &self.source_request_sha256,
            &self.goals_sha256,
        ] {
            if hash.len() != 64
                || !hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err("frozen task provenance requires lowercase SHA-256 identities".into());
            }
        }
        let valid_id = |prefix: &str, maximum: u32| {
            self.task_id.strip_prefix(prefix).is_some_and(|suffix| {
                suffix.len() == 2
                    && suffix.bytes().all(|byte| byte.is_ascii_digit())
                    && suffix
                        .parse::<u32>()
                        .is_ok_and(|number| (1..=maximum).contains(&number))
            })
        };
        let valid = match self.suite.as_str() {
            "fixture" => {
                (1..=5).contains(&self.seed)
                    && (valid_id("SET-", 6)
                        || valid_id("FORM-", 7)
                        || valid_id("LIST-", 4)
                        || valid_id("MODAL-", 3))
            }
            "textedit" => self.seed == 1 && valid_id("TEXTEDIT-", 20),
            _ => false,
        };
        if !valid {
            return Err("frozen task identity is outside the complete 100+20 denominator".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TargetSpec {
    pub semantic_target: SemanticTargetRef,
    #[schemars(length(min = 1))]
    pub label: String,
    pub group: Option<String>,
    pub role_hint: Option<String>,
    /// Stable, public row semantics. This never stores a Source object ref,
    /// AXIndex, private application id, or visible-row ordinal.
    pub row: Option<RowTargetSpec>,
}

/// Host-derived verification navigation for one scoped native control. The
/// planner cannot supply this authority. Each endpoint is re-bound from fresh
/// Source evidence, in the authorized application/window, before input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScopedUiRevisit {
    /// Original page, and the only permitted return destination.
    pub home: TargetSpec,
    /// The only permitted destination while leaving the original page.
    pub away: TargetSpec,
}

impl ScopedUiRevisit {
    pub fn validate(&self) -> Result<(), String> {
        for endpoint in [&self.home, &self.away] {
            endpoint.validate()?;
            validate_scope_text("scoped navigation label", &endpoint.label, 512)?;
            if endpoint.role_hint.as_deref() != Some("AXRadioButton")
                || endpoint.group.is_some()
                || endpoint.row.is_some()
            {
                return Err("scoped revisit requires exact native page radio targets".into());
            }
        }
        if self.home.semantic_target == self.away.semantic_target
            || super::binding::normalize_label(&self.home.label)
                == super::binding::normalize_label(&self.away.label)
        {
            return Err("scoped revisit endpoints must be distinct".into());
        }
        Ok(())
    }
}

/// A Host-frozen persistence action and its independently visible receipt for
/// one scoped native control. The action is absent for applications that save
/// automatically; receipt location and interpretation still come from the
/// trusted application pack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScopedCommitBinding {
    /// The sole exact Save control, when the app requires an explicit save.
    pub action: Option<TargetSpec>,
    pub receipt: ScopedCommitReceiptBinding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScopedCommitKind {
    Manual,
    Automatic,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScopedCommitReceiptBinding {
    /// Exact public group that contains the receipt.
    pub group: String,
    /// Receipt controls are static text, never an actionable target.
    pub role_hint: String,
    /// Stable AXDescription supplied by the trusted fixture.
    pub description: String,
    pub kind: ScopedCommitKind,
    /// Exact localized prefix; a positive decimal revision must follow it.
    pub text_prefix: String,
}

impl ScopedCommitBinding {
    pub fn validate(&self) -> Result<(), String> {
        let receipt = &self.receipt;
        validate_scope_text("commit receipt group", &receipt.group, 256)?;
        validate_scope_text("commit receipt role", &receipt.role_hint, 64)?;
        validate_scope_text("commit receipt description", &receipt.description, 256)?;
        validate_scope_text("commit receipt prefix", &receipt.text_prefix, 256)?;
        if receipt.role_hint != "AXStaticText"
            || !receipt.text_prefix.ends_with(' ')
            || receipt.text_prefix.contains('\n')
        {
            return Err(
                "scoped commit receipt requires exact static text and a revision prefix".into(),
            );
        }
        match (receipt.kind, self.action.as_ref()) {
            (ScopedCommitKind::Manual, Some(action)) => {
                action.validate()?;
                if action.role_hint.as_deref() != Some("AXButton")
                    || !matches!(action.label.as_str(), "Save" | "保存")
                    || action.group.as_deref() != Some(receipt.group.as_str())
                    || action.row.is_some()
                {
                    return Err(
                        "manual scoped commit requires its exact grouped Save button".into(),
                    );
                }
            }
            (ScopedCommitKind::Automatic, None) => {}
            _ => {
                return Err("scoped commit action does not match its persistence kind".into());
            }
        }
        Ok(())
    }
}

/// The only row-container role admitted by v1. Source refs identify the live
/// container only inside a single observation and are not part of this scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RowContainerRole {
    AxScrollArea,
}

impl RowContainerRole {
    pub fn ax_role(self) -> &'static str {
        match self {
            Self::AxScrollArea => "AXScrollArea",
        }
    }
}

/// One exact public label on the table's actual ancestor path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RowContainerScope {
    pub role: RowContainerRole,
    #[schemars(length(min = 1, max = 256))]
    pub label: String,
}

impl RowContainerScope {
    pub fn validate(&self) -> Result<(), String> {
        validate_row_scope_text(
            "row container label",
            &self.label,
            MAX_ROW_SCOPE_LABEL_BYTES,
        )
    }
}

/// A stable table scope made only from its actual labelled scroll ancestry
/// and its exact public header set. `column_headers` is a set encoded as a
/// bounded vector; callers must not interpret its order as AX column order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RowTableScope {
    #[schemars(length(min = 1, max = 8))]
    pub scroll_path: Vec<RowContainerScope>,
    #[schemars(length(min = 1, max = 64))]
    pub column_headers: Vec<String>,
}

impl RowTableScope {
    pub fn validate(&self) -> Result<(), String> {
        if self.scroll_path.is_empty() || self.scroll_path.len() > MAX_ROW_TABLE_ANCESTORS {
            return Err("row table scope needs 1 to 8 labelled scroll ancestors".into());
        }
        for container in &self.scroll_path {
            container.validate()?;
        }

        if self.column_headers.is_empty() || self.column_headers.len() > MAX_ROW_TABLE_HEADERS {
            return Err("row table scope needs 1 to 64 exact public column headers".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for header in &self.column_headers {
            validate_row_scope_text("row table column header", header, MAX_ROW_COLUMN_NAME_BYTES)?;
            if !seen.insert(row_name_key(header)) {
                return Err("row table scope contains duplicate column headers".into());
            }
        }
        Ok(())
    }
}

/// A public exact-value condition for one actual AXColumn header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RowColumnConditionSpec {
    #[schemars(length(min = 1, max = 256))]
    pub column_name: String,
    pub value: ExpectedValue,
}

impl RowColumnConditionSpec {
    pub fn validate(&self) -> Result<(), String> {
        validate_row_scope_text(
            "row condition column name",
            &self.column_name,
            MAX_ROW_COLUMN_NAME_BYTES,
        )?;
        match &self.value {
            ExpectedValue::Bool { .. } => {}
            ExpectedValue::Text { value } => validate_row_scope_text(
                "row condition text value",
                value,
                MAX_ROW_CONDITION_TEXT_BYTES,
            )?,
            ExpectedValue::Number { value }
                if (-MAX_ROW_CONDITION_INTEGER..=MAX_ROW_CONDITION_INTEGER).contains(value) => {}
            ExpectedValue::Number { .. } => {
                return Err("row condition integer is outside the bounded range".into());
            }
        }
        Ok(())
    }
}

/// The public, immutable part of a row target. The live Source table and row
/// refs are resolved separately for every observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RowTargetSpec {
    pub table_scope: RowTableScope,
    #[schemars(length(min = 1, max = 32))]
    pub conditions: Vec<RowColumnConditionSpec>,
}

impl RowTargetSpec {
    pub fn validate(&self) -> Result<(), String> {
        self.table_scope.validate()?;
        if self.conditions.is_empty() || self.conditions.len() > MAX_ROW_CONDITIONS {
            return Err("row target requires 1 to 32 public column conditions".into());
        }

        let headers: std::collections::BTreeSet<_> = self
            .table_scope
            .column_headers
            .iter()
            .map(String::as_str)
            .collect();
        let mut seen = std::collections::BTreeSet::new();
        for condition in &self.conditions {
            condition.validate()?;
            let key = row_name_key(&condition.column_name);
            if !headers.contains(condition.column_name.as_str()) {
                return Err(
                    "row condition column is absent from the scoped table header set".into(),
                );
            }
            if !seen.insert(key) {
                return Err("row target contains duplicate column conditions".into());
            }
        }
        Ok(())
    }
}

impl TargetSpec {
    /// Validate generic target text and the optional frozen row selector.
    /// Effect classification remains the responsibility of a registered pack.
    pub fn validate(&self) -> Result<(), String> {
        require_nonempty("target.label", &self.label)?;
        if let Some(row) = &self.row {
            validate_row_scope_text("row target label", &self.label, MAX_ROW_SCOPE_LABEL_BYTES)?;
            if let Some(group) = &self.group {
                validate_row_scope_text("row target group", group, MAX_ROW_SCOPE_LABEL_BYTES)?;
            }
            row.validate()?;
            if self
                .role_hint
                .as_deref()
                .is_some_and(|role| role != "AXRow")
            {
                return Err("row target role hint, when present, must be AXRow".into());
            }
        }
        Ok(())
    }
}

fn row_name_key(value: &str) -> String {
    value.trim().chars().flat_map(char::to_lowercase).collect()
}

fn validate_row_scope_text(field: &str, value: &str, max_bytes: usize) -> Result<(), String> {
    require_nonempty(field, value)?;
    if value.len() > max_bytes {
        return Err(format!("{field} exceeds its UTF-8 byte bound"));
    }
    Ok(())
}

/// The root budget. It is spent by attempts, replans and recoveries alike and
/// is never reset by a retry, a restart or a model switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RootBudgetSpec {
    /// Absolute, in the host's monotonic milliseconds.
    pub deadline_ms: u64,
    pub max_attempts: u32,
    pub max_observations: u32,
    pub max_recoveries: u32,
    pub max_replans: u32,
}

/// A restricted, checkable success condition. There is no way to write "the
/// model says it worked".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Predicate {
    ContextMatches {
        app: String,
    },
    TargetBound {
        target: SemanticTargetRef,
    },
    ReadValueEquals {
        target: SemanticTargetRef,
        expected: ExpectedValue,
        /// The reading must come from evidence observed after the last input.
        observed_after_action: bool,
    },
    /// The unique public row condition is currently selected in a fresh table
    /// viewport. This predicate carries no live Source or Device identifier.
    RowSelected {
        target: SemanticTargetRef,
        observed_after_action: bool,
    },
    /// The page was left and entered again and the target re-bound from new evidence.
    Revisited {
        target: SemanticTargetRef,
    },
    /// Evidence that the change was committed, not merely shown.
    CommitConfirmed {
        target: SemanticTargetRef,
    },
    And {
        of: Vec<Predicate>,
    },
    Or {
        of: Vec<Predicate>,
    },
    Not {
        of: Box<Predicate>,
    },
}

const MAX_PREDICATE_DEPTH: usize = 8;
const MAX_PREDICATE_NODES: usize = 64;

impl Predicate {
    pub fn validate(&self) -> Result<(), String> {
        let mut nodes = 0;
        self.check(0, &mut nodes)
    }

    fn check(&self, depth: usize, nodes: &mut usize) -> Result<(), String> {
        if depth > MAX_PREDICATE_DEPTH {
            return Err("predicate is nested too deeply".into());
        }
        *nodes += 1;
        if *nodes > MAX_PREDICATE_NODES {
            return Err("predicate has too many nodes".into());
        }
        match self {
            Self::ContextMatches { app } => require_nonempty("predicate.app", app),
            Self::And { of } | Self::Or { of } => {
                if of.is_empty() {
                    return Err("and/or need at least one operand".into());
                }
                of.iter()
                    .try_for_each(|child| child.check(depth + 1, nodes))
            }
            Self::Not { of } => of.check(depth + 1, nodes),
            _ => Ok(()),
        }
    }

    /// Whether any leaf needs post-action evidence.
    pub fn needs_post_action_evidence(&self) -> bool {
        match self {
            Self::ReadValueEquals {
                observed_after_action,
                ..
            }
            | Self::RowSelected {
                observed_after_action,
                ..
            } => *observed_after_action,
            Self::Revisited { .. } | Self::CommitConfirmed { .. } => true,
            Self::And { of } | Self::Or { of } => of.iter().any(Self::needs_post_action_evidence),
            Self::Not { of } => of.needs_post_action_evidence(),
            _ => false,
        }
    }
}

/// The frozen goal. After the user confirms it, none of this changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GoalSpec {
    pub root_task_id: RootTaskId,
    pub task_revision: u64,
    /// The trusted device-profile domain selected when this goal was compiled.
    /// It is never supplied by `GoalDraft` and cannot follow later capability changes.
    pub execution_domain: ExecutionDomain,
    /// A trusted, closed page profile frozen by the host. GoalDraft/model
    /// inputs cannot select a browser origin or profile.
    pub browser_page_profile: Option<BrowserPageProfile>,
    /// The user's words, kept verbatim.
    #[schemars(length(min = 1))]
    pub user_text: String,
    pub scope: TaskScope,
    /// Host-frozen user authority. It is independent from the planner's draft.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_scope: Option<AllowedControlScope>,
    /// Frozen by Host from trusted application semantics, never from GoalDraft.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scoped_revisit: Option<ScopedUiRevisit>,
    /// Persistence action and receipt derived from the trusted app pack.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scoped_commit: Option<ScopedCommitBinding>,
    pub target: TargetSpec,
    pub desired: ExpectedValue,
    pub confirmation_scope: ConfirmationScope,
    pub max_effect_class: EffectClass,
    pub required: Predicate,
    pub budget: RootBudgetSpec,
}

impl GoalSpec {
    pub fn validate(&self) -> Result<(), String> {
        validate_user_text(&self.user_text)?;
        self.target.validate()?;
        if self.execution_domain == ExecutionDomain::AppIntegration {
            return Err("the selected execution domain is not implemented".into());
        }
        if self.execution_domain == ExecutionDomain::HeadlessBrowser {
            let Some(profile) = self.browser_page_profile else {
                return Err("headless browser goal requires a trusted frozen page profile".into());
            };
            let target = &self.target.semantic_target;
            let browser_predicate = Predicate::And {
                of: vec![
                    Predicate::ContextMatches {
                        app: profile.app().into(),
                    },
                    Predicate::ReadValueEquals {
                        target: target.clone(),
                        expected: self.desired.clone(),
                        observed_after_action: true,
                    },
                ],
            };
            if self.scope.app != profile.app()
                || self.scope.account.is_some()
                || target.as_str() != profile.semantic_target()
                || self.target.label != profile.label()
                || self.target.group.is_some()
                || self.target.role_hint.as_deref() != Some(profile.role())
                || self.target.row.is_some()
                || !matches!(&self.desired, ExpectedValue::Bool { .. })
                || self.confirmation_scope != ConfirmationScope::CurrentUi
                || self.max_effect_class != EffectClass::ReversibleEdit
                || self.required != browser_predicate
            {
                return Err(
                    "headless browser goal does not match its fixed checkbox profile".into(),
                );
            }
        } else if self.browser_page_profile.is_some() {
            return Err(
                "browser page profile requires the headless browser execution domain".into(),
            );
        }
        if self.confirmation_scope == ConfirmationScope::CurrentUi
            && !matches!(
                self.execution_domain,
                ExecutionDomain::BackgroundAx | ExecutionDomain::HeadlessBrowser
            )
        {
            return Err(
                "current-UI confirmation is limited to supported background domains".into(),
            );
        }
        require_nonempty("scope.app", &self.scope.app)?;
        require_nonempty("target.label", &self.target.label)?;
        if let Some(commit) = &self.scoped_commit {
            commit.validate()?;
            let allowed = self
                .allowed_scope
                .as_ref()
                .ok_or("scoped commit requires one frozen allowed control")?;
            let target = &allowed.targets[0];
            if self.execution_domain != ExecutionDomain::ForegroundNative
                || self.scope.account.is_some()
                || self.target.row.is_some()
                || self.target.label != target.control
                || self.target.group.as_deref() != target.context_label()
                || !matches!(&self.desired, ExpectedValue::Bool { value: true })
                || commit.receipt.group != target.context_label().unwrap_or_default()
                || commit
                    .action
                    .as_ref()
                    .is_some_and(|action| action.semantic_target == self.target.semantic_target)
            {
                return Err("scoped commit does not match the frozen native control".into());
            }
            match (
                commit.receipt.kind,
                self.confirmation_scope,
                self.scoped_revisit.is_some(),
            ) {
                (ScopedCommitKind::Manual, ConfirmationScope::Commit, false)
                    if self.max_effect_class == EffectClass::Commit => {}
                (ScopedCommitKind::Automatic, ConfirmationScope::UiRevisit, true) => {}
                _ => return Err("scoped commit has an unsupported confirmation path".into()),
            }
        }
        if let Some(revisit) = &self.scoped_revisit {
            revisit.validate()?;
            if self.allowed_scope.is_none()
                || self.execution_domain != ExecutionDomain::ForegroundNative
                || (self.confirmation_scope != ConfirmationScope::UiRevisit
                    && !(self.confirmation_scope == ConfirmationScope::Commit
                        && self
                            .scoped_commit
                            .as_ref()
                            .is_some_and(|commit| commit.receipt.kind == ScopedCommitKind::Manual)))
                || self.target.semantic_target == revisit.home.semantic_target
                || self.target.semantic_target == revisit.away.semantic_target
            {
                return Err("scoped revisit requires a separate scoped foreground goal".into());
            }
            let mut required = vec![
                Predicate::ContextMatches {
                    app: self.scope.app.clone(),
                },
                Predicate::ReadValueEquals {
                    target: self.target.semantic_target.clone(),
                    expected: self.desired.clone(),
                    observed_after_action: true,
                },
                Predicate::Revisited {
                    target: self.target.semantic_target.clone(),
                },
            ];
            if self.scoped_commit.is_some() {
                required.push(Predicate::CommitConfirmed {
                    target: self.target.semantic_target.clone(),
                });
            }
            if self.required != (Predicate::And { of: required }) {
                return Err("scoped revisit must verify fresh target state after returning".into());
            }
        }
        if let Some(allowed_scope) = &self.allowed_scope {
            allowed_scope.validate()?;
            let target = &allowed_scope.targets[0];
            let supported_confirmation = match self.execution_domain {
                ExecutionDomain::BackgroundAx | ExecutionDomain::HeadlessBrowser => {
                    self.confirmation_scope == ConfirmationScope::CurrentUi
                        && self.scoped_revisit.is_none()
                }
                ExecutionDomain::ForegroundNative => {
                    self.confirmation_scope == ConfirmationScope::UiRevisit
                        && self.scoped_revisit.is_some()
                        && (self.scoped_commit.is_none()
                            || self.scoped_commit.as_ref().is_some_and(|commit| {
                                commit.receipt.kind == ScopedCommitKind::Automatic
                            }))
                        || self.confirmation_scope == ConfirmationScope::Commit
                            && self.scoped_revisit.is_none()
                            && self.scoped_commit.as_ref().is_some_and(|commit| {
                                commit.receipt.kind == ScopedCommitKind::Manual
                            })
                }
                _ => false,
            };
            let exact_manual_commit = self.confirmation_scope == ConfirmationScope::Commit
                && self.scoped_commit.as_ref().is_some_and(|commit| {
                    commit.receipt.kind == ScopedCommitKind::Manual && commit.action.is_some()
                });
            let supported_max_effect = self.max_effect_class <= EffectClass::ReversibleEdit
                || (exact_manual_commit && self.max_effect_class == EffectClass::Commit);
            if self.scope.account.is_some()
                || self.target.row.is_some()
                || self.target.label != target.control
                || self.target.group.as_deref() != target.context_label()
                || (target.form.is_none() && !matches!(&self.desired, ExpectedValue::Bool { .. }))
                || (target.form.is_some()
                    && (self.execution_domain != ExecutionDomain::ForegroundNative
                        || self.confirmation_scope != ConfirmationScope::UiRevisit
                        || self.scoped_revisit.is_none()
                        || !scoped_form_value_supported(&self.desired)
                        || !scoped_form_role_supported(
                            &self.desired,
                            self.target.role_hint.as_deref(),
                        )))
                || !supported_confirmation
                || !supported_max_effect
            {
                return Err("frozen goal target does not match its allowed control scope".into());
            }
        }
        if self.scoped_commit.is_some() && self.confirmation_scope == ConfirmationScope::Commit {
            let expected = Predicate::And {
                of: vec![
                    Predicate::ContextMatches {
                        app: self.scope.app.clone(),
                    },
                    Predicate::ReadValueEquals {
                        target: self.target.semantic_target.clone(),
                        expected: self.desired.clone(),
                        observed_after_action: true,
                    },
                    Predicate::CommitConfirmed {
                        target: self.target.semantic_target.clone(),
                    },
                ],
            };
            if self.required != expected {
                return Err(
                    "manual scoped commit must verify fresh control and receipt evidence".into(),
                );
            }
        }
        if self.task_revision == 0 {
            return Err("task_revision starts at 1".into());
        }
        if self.max_effect_class == EffectClass::Unknown {
            return Err("a goal cannot allow effects of unknown class".into());
        }
        if self.max_effect_class >= EffectClass::External {
            return Err("phase 1 goals cannot include external effects".into());
        }
        if self.budget.max_attempts == 0 {
            return Err("the budget must allow at least one attempt".into());
        }
        self.required.validate()?;

        if self.target.row.is_some()
            && !matches!(&self.desired, ExpectedValue::Bool { value: true })
        {
            return Err("a row target only supports the frozen selected=true goal".into());
        }
        let mut row_predicate_targets = Vec::new();
        collect_row_selected_targets(&self.required, &mut row_predicate_targets);
        if row_predicate_targets
            .iter()
            .any(|target| *target != &self.target.semantic_target)
        {
            return Err("RowSelected predicate must name the unique frozen Goal target".into());
        }
        if !row_predicate_targets.is_empty() && self.target.row.is_none() {
            return Err("RowSelected predicate requires a frozen row target specification".into());
        }
        if self.execution_domain == ExecutionDomain::BackgroundAx {
            let target = &self.target.semantic_target;
            let supported_role = self.target.role_hint.as_deref().is_some_and(|role| {
                matches!(
                    role,
                    "AXButton" | "AXCheckBox" | "AXRadioButton" | "AXSwitch"
                )
            });
            let background_predicate = Predicate::And {
                of: vec![
                    Predicate::ContextMatches {
                        app: self.scope.app.clone(),
                    },
                    Predicate::ReadValueEquals {
                        target: target.clone(),
                        expected: self.desired.clone(),
                        observed_after_action: true,
                    },
                ],
            };
            if self.target.row.is_some()
                || !supported_role
                || !matches!(&self.desired, ExpectedValue::Bool { .. })
                || self.confirmation_scope != ConfirmationScope::CurrentUi
                || self.required != background_predicate
            {
                return Err(
                    "background AX goals are limited to one boolean form action with fresh value verification".into(),
                );
            }
        }
        Ok(())
    }
}

pub(super) fn scoped_form_value_supported(value: &ExpectedValue) -> bool {
    match value {
        ExpectedValue::Bool { .. } => true,
        ExpectedValue::Text { value } => {
            value.len() <= super::primitive::MAX_TEXT_BYTES && !value.contains('\0')
        }
        ExpectedValue::Number { value } => {
            (-MAX_SCOPED_FORM_SAFE_INTEGER..=MAX_SCOPED_FORM_SAFE_INTEGER).contains(value)
        }
    }
}

fn scoped_form_role_supported(value: &ExpectedValue, role: Option<&str>) -> bool {
    match value {
        ExpectedValue::Bool { .. } => role == Some("AXCheckBox"),
        // Choice-group text is a type-level allowance only. The Host's
        // application pack separately authorizes exact popup/radio choices.
        ExpectedValue::Text { .. } => {
            matches!(
                role,
                Some("AXTextField" | "AXTextArea" | "AXPopUpButton" | "AXRadioGroup")
            )
        }
        ExpectedValue::Number { .. } => role == Some("AXTextField"),
    }
}

fn collect_row_selected_targets<'a>(
    predicate: &'a Predicate,
    targets: &mut Vec<&'a SemanticTargetRef>,
) {
    match predicate {
        Predicate::RowSelected { target, .. } => targets.push(target),
        Predicate::And { of } | Predicate::Or { of } => {
            for child in of {
                collect_row_selected_targets(child, targets);
            }
        }
        Predicate::Not { of } => collect_row_selected_targets(of, targets),
        _ => {}
    }
}

/// User task prose may contain line breaks, unlike identifier fields.
pub fn validate_user_text(text: &str) -> Result<(), String> {
    if text.trim().is_empty()
        || text.len() > 8192
        || text
            .chars()
            .any(|c| c.is_control() && !['\n', '\r', '\t'].contains(&c))
    {
        return Err("user_text must contain bounded task text without unsupported controls".into());
    }
    Ok(())
}

/// The root task states of design 13.5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RootState {
    Draft,
    CompilingGoal,
    AwaitingConfirmation,
    NeedsInput,
    Ready,
    Running,
    WaitingEvidence,
    WaitingPlan,
    Pausing,
    Suspended,
    NeedsRevalidation,
    Cancelling,
    Completed,
    Failed,
    Unverified,
    Expired,
    Cancelled,
}

impl RootState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Unverified | Self::Expired | Self::Cancelled
        )
    }

    /// The root is active, so a still-valid sub-goal may produce input.
    pub fn may_produce_input(self) -> bool {
        matches!(
            self,
            Self::Running | Self::WaitingEvidence | Self::WaitingPlan
        )
    }
}

/// Everything that can move a root task from one state to another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RootEvent {
    SubmitGoal,
    DraftPassed,
    AmbiguityFound,
    CompileFailed,
    UserConfirmed,
    UserEdited,
    AdmissionPassed,
    AwaitEvidence,
    AwaitPlan,
    EvidenceArrived,
    PlanInstalled,
    NeedUserInfo,
    UserAnswered,
    /// A pause, human input, or device or source failure.
    PauseRequested,
    InputsReleased,
    ResumeRequested,
    ProcessRestarted,
    RevalidationPassed,
    PredicatesSatisfied,
    Unrecoverable,
    InputEndedUnverified,
    DeadlineExpired,
    StopRequested,
    CancelFinished,
}

/// How much of a root's budget one attempt may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AttemptQuota {
    pub actions: u32,
    pub observations: u32,
}

/// Reaction's request to start a skill. It is checked and recorded in one
/// transaction by TaskRuntime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AdmitAttempt {
    pub attempt_id: AttemptId,
    pub decision_id: DecisionId,
    pub generation: u64,
    pub root_task_id: RootTaskId,
    pub root_epoch: u64,
    pub task_revision: u64,
    pub plan_revision: u64,
    pub subgoal: String,
    pub skill: String,
    pub quota: AttemptQuota,
    pub deadline_ms: u64,
    pub effect_class: EffectClass,
    pub source: DecisionSource,
}

/// Only an activation allows normal input for the attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Activation {
    pub attempt_id: AttemptId,
    pub root_task_id: RootTaskId,
    pub root_epoch: u64,
    pub activation_seq: u64,
    pub granted: AttemptQuota,
    pub deadline_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AdmitRejection {
    RootNotRunning,
    RootEpochStale,
    VersionMismatch,
    DeadlineExpired,
    BudgetExhausted,
    DecisionNotPending,
    DecisionAlreadyConsumed,
    AttemptIdConflict,
    EffectNotAuthorized,
}

impl AdmitRejection {
    pub fn failure_code(self) -> FailureCode {
        match self {
            Self::BudgetExhausted => FailureCode::BudgetExhausted,
            Self::RootNotRunning | Self::RootEpochStale => FailureCode::Cancelled,
            Self::DeadlineExpired => FailureCode::EvidenceExpired,
            Self::EffectNotAuthorized => FailureCode::EffectNotAuthorized,
            Self::VersionMismatch
            | Self::DecisionNotPending
            | Self::DecisionAlreadyConsumed
            | Self::AttemptIdConflict => FailureCode::ContextUnresolved,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum AdmitOutcome {
    /// Also the answer to a repeated request: the earlier activation again.
    Activated {
        activation: Activation,
    },
    Rejected {
        reason: AdmitRejection,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AttemptState {
    Active,
    Dispatched,
    Settled,
    Unknown,
    Cancelled,
}

impl AttemptState {
    pub fn is_final(self) -> bool {
        matches!(self, Self::Settled | Self::Cancelled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::browser::BrowserPageProfile;

    fn target() -> SemanticTargetRef {
        SemanticTargetRef::new("daily-picks").unwrap()
    }

    fn browser_scope() -> AllowedControlScope {
        AllowedControlScope {
            application: "Browser fixture".into(),
            targets: vec![AllowedControlTarget {
                control: "Background option".into(),
                group: None,
                form: None,
                disambiguator: None,
            }],
            no_unrelated_controls: true,
        }
    }

    #[test]
    fn allowed_control_scope_is_exactly_one_supported_target() {
        assert!(browser_scope().validate().is_ok());

        let mut no_target = browser_scope();
        no_target.targets.clear();
        assert!(no_target.validate().is_err());

        let mut multiple_targets = browser_scope();
        multiple_targets
            .targets
            .push(multiple_targets.targets[0].clone());
        assert!(multiple_targets.validate().is_err());

        let mut unrelated_controls_allowed = browser_scope();
        unrelated_controls_allowed.no_unrelated_controls = false;
        assert!(unrelated_controls_allowed.validate().is_err());

        let mut unbound_disambiguator = browser_scope();
        unbound_disambiguator.targets[0].disambiguator = Some("different control".into());
        assert!(unbound_disambiguator.validate().is_err());
    }

    #[test]
    fn allowed_control_target_form_is_legacy_optional_and_context_exclusive() {
        let legacy = serde_json::json!({
            "application": "Browser fixture",
            "targets": [{
                "control": "Background option",
                "group": null,
                "disambiguator": null
            }],
            "no_unrelated_controls": true
        });
        let parsed: AllowedControlScope = serde_json::from_value(legacy).unwrap();
        assert_eq!(parsed.targets[0].form, None);
        assert_eq!(parsed.targets[0].context_label(), None);
        let serialized = serde_json::to_value(&parsed).unwrap();
        assert!(serialized["targets"][0].get("form").is_none());

        let form = AllowedControlTarget {
            control: "Email".into(),
            group: None,
            form: Some("Profile".into()),
            disambiguator: None,
        };
        assert!(form.validate().is_ok());
        assert_eq!(form.context_label(), Some("Profile"));

        let mut group_and_form = form.clone();
        group_and_form.group = Some("Notifications".into());
        assert!(group_and_form.validate().is_err());

        let mut form_disambiguator = form.clone();
        form_disambiguator.disambiguator = Some("Profile".into());
        assert!(form_disambiguator.validate().is_err());

        let mut blank_form = form.clone();
        blank_form.form = Some("  ".into());
        assert!(blank_form.validate().is_err());
        let mut long_form = form;
        long_form.form = Some("f".repeat(257));
        assert!(long_form.validate().is_err());
    }

    #[test]
    fn frozen_scoped_goal_cannot_widen_target_or_confirmation() {
        let mut scoped = headless_browser_goal();
        scoped.allowed_scope = Some(browser_scope());
        assert!(scoped.validate().is_ok());

        let mut changed_group = scoped.clone();
        changed_group.allowed_scope.as_mut().unwrap().targets[0].group = Some("Other".into());
        assert!(changed_group.validate().is_err());

        let mut revisit = scoped.clone();
        revisit.confirmation_scope = ConfirmationScope::UiRevisit;
        assert!(revisit.validate().is_err());

        let mut scoped_row = scoped;
        scoped_row.target.row = Some(row_target().row.unwrap());
        assert!(scoped_row.validate().is_err());
    }

    fn goal() -> GoalSpec {
        GoalSpec {
            root_task_id: RootTaskId::new("root-1").unwrap(),
            task_revision: 1,
            execution_domain: ExecutionDomain::Simulated,
            browser_page_profile: None,
            user_text: "turn off daily picks notifications".into(),
            scope: TaskScope {
                app: "test.app".into(),
                account: None,
            },
            allowed_scope: None,
            scoped_revisit: None,
            scoped_commit: None,
            target: TargetSpec {
                semantic_target: target(),
                label: "Daily picks".into(),
                group: Some("Push".into()),
                role_hint: Some("switch".into()),
                row: None,
            },
            desired: ExpectedValue::Bool { value: false },
            confirmation_scope: ConfirmationScope::UiRevisit,
            max_effect_class: EffectClass::ReversibleEdit,
            required: Predicate::And {
                of: vec![
                    Predicate::ContextMatches {
                        app: "test.app".into(),
                    },
                    Predicate::ReadValueEquals {
                        target: target(),
                        expected: ExpectedValue::Bool { value: false },
                        observed_after_action: true,
                    },
                ],
            },
            budget: RootBudgetSpec {
                deadline_ms: 60_000,
                max_attempts: 8,
                max_observations: 16,
                max_recoveries: 3,
                max_replans: 2,
            },
        }
    }

    #[test]
    fn scoped_native_revisit_requires_closed_endpoints_and_fresh_return_verification() {
        let mut scoped = goal();
        scoped.execution_domain = ExecutionDomain::ForegroundNative;
        scoped.allowed_scope = Some(AllowedControlScope {
            application: scoped.scope.app.clone(),
            targets: vec![AllowedControlTarget {
                control: scoped.target.label.clone(),
                group: scoped.target.group.clone(),
                form: None,
                disambiguator: None,
            }],
            no_unrelated_controls: true,
        });
        let endpoint = |id: &str, label: &str| TargetSpec {
            semantic_target: SemanticTargetRef::new(id).unwrap(),
            label: label.into(),
            group: None,
            role_hint: Some("AXRadioButton".into()),
            row: None,
        };
        scoped.scoped_revisit = Some(ScopedUiRevisit {
            home: endpoint("page-settings", "Settings"),
            away: endpoint("page-overview", "Overview"),
        });
        assert!(
            scoped.validate().is_err(),
            "readback alone is not a revisit"
        );
        if let Predicate::And { of } = &mut scoped.required {
            of.push(Predicate::Revisited {
                target: scoped.target.semantic_target.clone(),
            });
        }
        assert!(scoped.validate().is_ok());

        let mut missing = scoped.clone();
        missing.scoped_revisit = None;
        assert!(missing.validate().is_err());
        let mut background = scoped.clone();
        background.execution_domain = ExecutionDomain::BackgroundAx;
        assert!(background.validate().is_err());
        let mut unscoped = scoped.clone();
        unscoped.allowed_scope = None;
        assert!(unscoped.validate().is_err());
        let mut duplicate = scoped.clone();
        let revisit = duplicate.scoped_revisit.as_mut().unwrap();
        revisit.away = revisit.home.clone();
        assert!(duplicate.validate().is_err());
        let mut edit_control = scoped.clone();
        edit_control.scoped_revisit.as_mut().unwrap().away.role_hint = Some("AXCheckBox".into());
        assert!(edit_control.validate().is_err());
        let mut target_as_navigation = scoped.clone();
        target_as_navigation
            .scoped_revisit
            .as_mut()
            .unwrap()
            .away
            .semantic_target = target();
        assert!(target_as_navigation.validate().is_err());

        let draft = super::super::planning::GoalDraft {
            app: scoped.scope.app.clone(),
            account: None,
            target: scoped.target.clone(),
            desired: scoped.desired.clone(),
            confirmation_scope: scoped.confirmation_scope,
            max_effect_class: scoped.max_effect_class,
        };
        let mut forged = serde_json::to_value(draft).unwrap();
        forged["scoped_revisit"] = serde_json::to_value(&scoped.scoped_revisit).unwrap();
        assert!(serde_json::from_value::<super::super::planning::GoalDraft>(forged).is_err());
    }

    fn form_goal(desired: ExpectedValue, role: &str) -> GoalSpec {
        let mut scoped = goal();
        scoped.execution_domain = ExecutionDomain::ForegroundNative;
        scoped.target.label = "Email".into();
        scoped.target.group = Some("Profile".into());
        scoped.target.role_hint = Some(role.into());
        scoped.desired = desired.clone();
        scoped.confirmation_scope = ConfirmationScope::UiRevisit;
        scoped.allowed_scope = Some(AllowedControlScope {
            application: scoped.scope.app.clone(),
            targets: vec![AllowedControlTarget {
                control: "Email".into(),
                group: None,
                form: Some("Profile".into()),
                disambiguator: None,
            }],
            no_unrelated_controls: true,
        });
        scoped.scoped_revisit = Some(ScopedUiRevisit {
            home: TargetSpec {
                semantic_target: SemanticTargetRef::new("page-settings").unwrap(),
                label: "Settings".into(),
                group: None,
                role_hint: Some("AXRadioButton".into()),
                row: None,
            },
            away: TargetSpec {
                semantic_target: SemanticTargetRef::new("page-overview").unwrap(),
                label: "Overview".into(),
                group: None,
                role_hint: Some("AXRadioButton".into()),
                row: None,
            },
        });
        scoped.required = Predicate::And {
            of: vec![
                Predicate::ContextMatches {
                    app: scoped.scope.app.clone(),
                },
                Predicate::ReadValueEquals {
                    target: scoped.target.semantic_target.clone(),
                    expected: desired,
                    observed_after_action: true,
                },
                Predicate::Revisited {
                    target: scoped.target.semantic_target.clone(),
                },
            ],
        };
        scoped
    }

    #[test]
    fn form_scoped_goal_is_native_revisit_and_limits_typed_values_to_supported_roles() {
        let checkbox = form_goal(ExpectedValue::Bool { value: true }, "AXCheckBox");
        assert!(checkbox.validate().is_ok());
        let text_field = form_goal(
            ExpectedValue::Text {
                value: "first line\nsecond line".into(),
            },
            "AXTextField",
        );
        assert!(text_field.validate().is_ok());
        let empty_text_field = form_goal(
            ExpectedValue::Text {
                value: String::new(),
            },
            "AXTextField",
        );
        assert!(empty_text_field.validate().is_ok());
        let text_area = form_goal(
            ExpectedValue::Text {
                value: "x".repeat(super::super::primitive::MAX_TEXT_BYTES),
            },
            "AXTextArea",
        );
        assert!(text_area.validate().is_ok());
        for value in [
            -MAX_SCOPED_FORM_SAFE_INTEGER,
            0,
            MAX_SCOPED_FORM_SAFE_INTEGER,
        ] {
            assert!(
                form_goal(ExpectedValue::Number { value }, "AXTextField")
                    .validate()
                    .is_ok()
            );
        }
        let mut radio_group = form_goal(
            ExpectedValue::Text {
                value: "By week".into(),
            },
            "AXRadioGroup",
        );
        radio_group.target.label = "Summary grouping".into();
        radio_group.target.group = Some("Preferences".into());
        let allowed_radio_group = &mut radio_group.allowed_scope.as_mut().unwrap().targets[0];
        allowed_radio_group.control = "Summary grouping".into();
        allowed_radio_group.form = Some("Preferences".into());
        assert!(radio_group.validate().is_ok());
        assert_eq!(
            radio_group.desired,
            ExpectedValue::Text {
                value: "By week".into()
            }
        );
        assert!(
            !serde_json::to_string(&radio_group)
                .unwrap()
                .contains("choice_values")
        );

        for rejected in [
            form_goal(
                ExpectedValue::Text {
                    value: "x".repeat(super::super::primitive::MAX_TEXT_BYTES + 1),
                },
                "AXTextField",
            ),
            form_goal(
                ExpectedValue::Text {
                    value: "a\0b".into(),
                },
                "AXTextField",
            ),
            form_goal(ExpectedValue::Number { value: 4 }, "AXTextArea"),
            form_goal(
                ExpectedValue::Number {
                    value: MAX_SCOPED_FORM_SAFE_INTEGER + 1,
                },
                "AXTextField",
            ),
            form_goal(
                ExpectedValue::Number {
                    value: -MAX_SCOPED_FORM_SAFE_INTEGER - 1,
                },
                "AXTextField",
            ),
            form_goal(ExpectedValue::Bool { value: true }, "AXRadioButton"),
            form_goal(ExpectedValue::Bool { value: true }, "AXRadioGroup"),
            form_goal(
                ExpectedValue::Text {
                    value: "By week".into(),
                },
                "AXRadioButton",
            ),
            form_goal(ExpectedValue::Bool { value: true }, "AXMenuButton"),
        ] {
            assert!(rejected.validate().is_err());
        }

        let mut current_ui_form = checkbox.clone();
        current_ui_form.execution_domain = ExecutionDomain::BackgroundAx;
        current_ui_form.confirmation_scope = ConfirmationScope::CurrentUi;
        current_ui_form.scoped_revisit = None;
        current_ui_form.required = Predicate::And {
            of: vec![
                Predicate::ContextMatches {
                    app: current_ui_form.scope.app.clone(),
                },
                Predicate::ReadValueEquals {
                    target: current_ui_form.target.semantic_target.clone(),
                    expected: current_ui_form.desired.clone(),
                    observed_after_action: true,
                },
            ],
        };
        assert!(current_ui_form.validate().is_err());

        let mut group_text = checkbox;
        let scope_target = group_text
            .allowed_scope
            .as_mut()
            .unwrap()
            .targets
            .first_mut()
            .unwrap();
        scope_target.form = None;
        scope_target.group = Some("Profile".into());
        group_text.desired = ExpectedValue::Text {
            value: "text".into(),
        };
        if let Predicate::And { of } = &mut group_text.required {
            of[1] = Predicate::ReadValueEquals {
                target: group_text.target.semantic_target.clone(),
                expected: group_text.desired.clone(),
                observed_after_action: true,
            };
        }
        assert!(group_text.validate().is_err());
    }

    fn row_target() -> TargetSpec {
        TargetSpec {
            semantic_target: target(),
            label: "Weekly report".into(),
            group: None,
            role_hint: Some("AXRow".into()),
            row: Some(RowTargetSpec {
                table_scope: RowTableScope {
                    scroll_path: vec![
                        RowContainerScope {
                            role: RowContainerRole::AxScrollArea,
                            label: "Preferences content".into(),
                        },
                        RowContainerScope {
                            role: RowContainerRole::AxScrollArea,
                            label: "Projects".into(),
                        },
                    ],
                    column_headers: vec!["Name".into(), "Owner".into(), "Status".into()],
                },
                conditions: vec![
                    RowColumnConditionSpec {
                        column_name: "Owner".into(),
                        value: ExpectedValue::Text {
                            value: "Mina".into(),
                        },
                    },
                    RowColumnConditionSpec {
                        column_name: "Status".into(),
                        value: ExpectedValue::Text {
                            value: "Review".into(),
                        },
                    },
                ],
            }),
        }
    }

    fn row_goal() -> GoalSpec {
        let mut goal = goal();
        goal.target = row_target();
        goal.desired = ExpectedValue::Bool { value: true };
        goal.required = Predicate::And {
            of: vec![
                Predicate::ContextMatches {
                    app: "test.app".into(),
                },
                Predicate::RowSelected {
                    target: target(),
                    observed_after_action: true,
                },
            ],
        };
        goal
    }

    fn headless_browser_goal() -> GoalSpec {
        let profile = BrowserPageProfile::FixtureBackgroundOption;
        let mut goal = goal();
        goal.execution_domain = ExecutionDomain::HeadlessBrowser;
        goal.browser_page_profile = Some(profile);
        goal.scope.app = profile.app().into();
        goal.target.semantic_target = SemanticTargetRef::new(profile.semantic_target()).unwrap();
        goal.target.label = profile.label().into();
        goal.target.group = None;
        goal.target.role_hint = Some(profile.role().into());
        goal.target.row = None;
        goal.confirmation_scope = ConfirmationScope::CurrentUi;
        goal.required = Predicate::And {
            of: vec![
                Predicate::ContextMatches {
                    app: profile.app().into(),
                },
                Predicate::ReadValueEquals {
                    target: goal.target.semantic_target.clone(),
                    expected: goal.desired.clone(),
                    observed_after_action: true,
                },
            ],
        };
        goal
    }

    #[test]
    fn headless_browser_goal_is_closed_to_the_frozen_checkbox_profile() {
        let valid = headless_browser_goal();
        assert!(valid.validate().is_ok());

        let mut model_selected_domain_without_profile = valid.clone();
        model_selected_domain_without_profile.browser_page_profile = None;
        assert!(model_selected_domain_without_profile.validate().is_err());

        let mut mismatched_target = valid.clone();
        mismatched_target.target.semantic_target =
            SemanticTargetRef::new("other-checkbox").unwrap();
        assert!(mismatched_target.validate().is_err());

        let mut native_goal_with_profile = valid;
        native_goal_with_profile.execution_domain = ExecutionDomain::BackgroundAx;
        assert!(native_goal_with_profile.validate().is_err());
    }

    #[test]
    fn row_table_scope_is_public_exact_and_independent_of_ax_column_order() {
        let scope = row_target().row.unwrap().table_scope;
        assert!(scope.validate().is_ok());

        let mut reordered_set = scope.clone();
        reordered_set.column_headers.reverse();
        assert!(reordered_set.validate().is_ok());

        for invalid in [
            RowTableScope {
                scroll_path: vec![],
                ..scope.clone()
            },
            RowTableScope {
                scroll_path: (0..=MAX_ROW_TABLE_ANCESTORS)
                    .map(|index| RowContainerScope {
                        role: RowContainerRole::AxScrollArea,
                        label: format!("container-{index}"),
                    })
                    .collect(),
                ..scope.clone()
            },
            RowTableScope {
                column_headers: vec![],
                ..scope.clone()
            },
            RowTableScope {
                column_headers: vec!["Name".into(), " name ".into()],
                ..scope.clone()
            },
            RowTableScope {
                column_headers: (0..=MAX_ROW_TABLE_HEADERS)
                    .map(|index| format!("Column {index}"))
                    .collect(),
                ..scope.clone()
            },
        ] {
            assert!(invalid.validate().is_err());
        }

        let mut bad_header = scope.clone();
        bad_header.column_headers[0] = "\n".into();
        assert!(bad_header.validate().is_err());
        let mut long_header = scope.clone();
        long_header.column_headers[0] = "n".repeat(MAX_ROW_COLUMN_NAME_BYTES + 1);
        assert!(long_header.validate().is_err());
        let mut bad_container = scope;
        bad_container.scroll_path[0].label = "  ".into();
        assert!(bad_container.validate().is_err());
    }

    #[test]
    fn row_target_conditions_are_bounded_unique_and_present_in_the_scoped_header_set() {
        let row = row_target().row.unwrap();
        assert!(row.validate().is_ok());

        let mut duplicate = row.clone();
        duplicate.conditions[1].column_name = " owner ".into();
        assert!(duplicate.validate().is_err());

        let mut absent_header = row.clone();
        absent_header.conditions[0].column_name = "Modified".into();
        assert!(absent_header.validate().is_err());
        let mut different_spelling = row.clone();
        different_spelling.conditions[0].column_name =
            different_spelling.conditions[0].column_name.to_lowercase();
        assert!(different_spelling.validate().is_err());

        let mut no_conditions = row.clone();
        no_conditions.conditions.clear();
        assert!(no_conditions.validate().is_err());

        let mut too_many = row.clone();
        too_many.table_scope.column_headers = (0..MAX_ROW_CONDITIONS + 1)
            .map(|index| format!("Column {index}"))
            .collect();
        too_many.conditions = (0..MAX_ROW_CONDITIONS + 1)
            .map(|index| RowColumnConditionSpec {
                column_name: format!("Column {index}"),
                value: ExpectedValue::Bool { value: true },
            })
            .collect();
        assert!(too_many.validate().is_err());

        for invalid_value in [
            ExpectedValue::Text {
                value: String::new(),
            },
            ExpectedValue::Text { value: "  ".into() },
            ExpectedValue::Text {
                value: "line\nfeed".into(),
            },
            ExpectedValue::Text {
                value: "🙂".repeat(MAX_ROW_CONDITION_TEXT_BYTES / 4 + 1),
            },
            ExpectedValue::Number {
                value: MAX_ROW_CONDITION_INTEGER + 1,
            },
            ExpectedValue::Number { value: i64::MIN },
        ] {
            let condition = RowColumnConditionSpec {
                column_name: "Owner".into(),
                value: invalid_value,
            };
            assert!(condition.validate().is_err());
        }

        for accepted in [
            ExpectedValue::Bool { value: false },
            ExpectedValue::Text {
                value: "x".repeat(MAX_ROW_CONDITION_TEXT_BYTES),
            },
            ExpectedValue::Number {
                value: -MAX_ROW_CONDITION_INTEGER,
            },
            ExpectedValue::Number {
                value: MAX_ROW_CONDITION_INTEGER,
            },
        ] {
            assert!(
                RowColumnConditionSpec {
                    column_name: "Owner".into(),
                    value: accepted,
                }
                .validate()
                .is_ok()
            );
        }
    }

    #[test]
    fn row_goal_is_selected_true_and_row_selected_predicate_is_target_scoped() {
        assert!(row_target().validate().is_ok());
        let mut no_role_hint = row_target();
        no_role_hint.role_hint = None;
        assert!(no_role_hint.validate().is_ok());
        let valid = row_goal();
        assert!(valid.validate().is_ok());
        assert!(valid.required.needs_post_action_evidence());
        assert!(
            !Predicate::RowSelected {
                target: target(),
                observed_after_action: false,
            }
            .needs_post_action_evidence()
        );

        let mut wrong_desired = valid.clone();
        wrong_desired.desired = ExpectedValue::Bool { value: false };
        assert!(wrong_desired.validate().is_err());

        let mut wrong_role = valid.clone();
        wrong_role.target.role_hint = Some("AXButton".into());
        assert!(wrong_role.validate().is_err());

        let mut wrong_predicate_target = valid.clone();
        wrong_predicate_target.required = Predicate::RowSelected {
            target: SemanticTargetRef::new("another-target").unwrap(),
            observed_after_action: true,
        };
        assert!(wrong_predicate_target.validate().is_err());

        let mut missing_row_scope = goal();
        missing_row_scope.desired = ExpectedValue::Bool { value: true };
        missing_row_scope.required = Predicate::RowSelected {
            target: target(),
            observed_after_action: true,
        };
        assert!(missing_row_scope.validate().is_err());

        let mut current_ui = valid;
        current_ui.confirmation_scope = ConfirmationScope::CurrentUi;
        assert!(current_ui.validate().is_err());
    }

    #[test]
    fn row_target_wire_contains_only_public_semantics_and_rejects_unknown_fields() {
        let target = row_target();
        let mut wire = serde_json::to_value(&target).unwrap();
        let serialized = serde_json::to_string(&target).unwrap();
        for forbidden in [
            "instance_ref",
            "table_instance_ref",
            "row_instance_ref",
            "ax_index",
            "private_id",
            "ordinal",
        ] {
            assert!(!serialized.contains(forbidden), "serialized {forbidden}");
        }

        wire["row"]["table_scope"]["table_instance_ref"] = serde_json::json!("opaque");
        assert!(serde_json::from_value::<TargetSpec>(wire).is_err());
    }

    #[test]
    fn multiline_task_prose_is_preserved_but_unsupported_controls_and_byte_overflow_are_refused() {
        let mut g = goal();
        g.user_text = "Fill document: 中文\r\nHello 👋\t".into();
        assert!(g.validate().is_ok());
        g.user_text.push('\0');
        assert!(g.validate().is_err());
        g.user_text = "中".repeat(2731);
        assert!(g.validate().is_err());
    }

    #[test]
    fn a_reasonable_goal_validates() {
        assert!(goal().validate().is_ok());
    }

    #[test]
    fn goals_cannot_reach_beyond_phase_one() {
        let mut bad = goal();
        bad.max_effect_class = EffectClass::External;
        assert!(bad.validate().is_err());
        bad.max_effect_class = EffectClass::Unknown;
        assert!(bad.validate().is_err());
        bad = goal();
        bad.budget.max_attempts = 0;
        assert!(bad.validate().is_err());
        bad = goal();
        bad.task_revision = 0;
        assert!(bad.validate().is_err());
    }

    #[test]
    fn predicates_are_bounded() {
        let mut nested = Predicate::ContextMatches { app: "a".into() };
        for _ in 0..(MAX_PREDICATE_DEPTH + 2) {
            nested = Predicate::Not {
                of: Box::new(nested),
            };
        }
        assert!(nested.validate().is_err());
        assert!(Predicate::And { of: vec![] }.validate().is_err());
        let wide = Predicate::And {
            of: (0..=MAX_PREDICATE_NODES)
                .map(|_| Predicate::ContextMatches { app: "a".into() })
                .collect(),
        };
        assert!(wide.validate().is_err());
    }

    #[test]
    fn post_action_evidence_is_detected_through_nesting() {
        assert!(goal().required.needs_post_action_evidence());
        assert!(!Predicate::ContextMatches { app: "a".into() }.needs_post_action_evidence());
        assert!(
            Predicate::Not {
                of: Box::new(Predicate::Revisited { target: target() })
            }
            .needs_post_action_evidence()
        );
    }

    #[test]
    fn terminal_and_input_states_are_classified() {
        for state in [
            RootState::Completed,
            RootState::Failed,
            RootState::Unverified,
            RootState::Expired,
            RootState::Cancelled,
        ] {
            assert!(state.is_terminal());
            assert!(!state.may_produce_input());
        }
        assert!(RootState::Running.may_produce_input());
        assert!(RootState::WaitingPlan.may_produce_input());
        for state in [
            RootState::CompilingGoal,
            RootState::AwaitingConfirmation,
            RootState::NeedsInput,
            RootState::Suspended,
            RootState::NeedsRevalidation,
            RootState::Cancelling,
        ] {
            assert!(!state.may_produce_input());
        }
    }

    #[test]
    fn rejections_map_to_the_shared_failure_vocabulary() {
        assert_eq!(
            AdmitRejection::BudgetExhausted.failure_code(),
            FailureCode::BudgetExhausted
        );
        assert_eq!(
            AdmitRejection::RootEpochStale.failure_code(),
            FailureCode::Cancelled
        );
    }
}

#[cfg(test)]
mod frozen_profile_tests {
    use super::*;
    fn profile() -> FrozenTaskProfile {
        FrozenTaskProfile {
            catalog_id: "a".repeat(64),
            selector: "b".repeat(64),
            source_freeze_sha256: "c".repeat(64),
            source_request_sha256: "d".repeat(64),
            goals_sha256: "e".repeat(64),
            suite: "fixture".into(),
            task_id: "SET-01".into(),
            seed: 1,
        }
    }
    #[test]
    fn every_frozen_identity_is_admitted_and_outside_identities_are_refused() {
        let mut value = profile();
        let mut count = 0;
        for (prefix, maximum) in [("SET", 6), ("FORM", 7), ("LIST", 4), ("MODAL", 3)] {
            for id in 1..=maximum {
                for seed in 1..=5 {
                    value.task_id = format!("{prefix}-{id:02}");
                    value.seed = seed;
                    value.validate().unwrap();
                    count += 1;
                }
            }
        }
        value.suite = "textedit".into();
        value.seed = 1;
        for id in 1..=20 {
            value.task_id = format!("TEXTEDIT-{id:02}");
            value.validate().unwrap();
            count += 1;
        }
        assert_eq!(count, 120);
        for id in [
            "TEXTEDIT-00",
            "TEXTEDIT-21",
            "TEXTEDIT-1",
            "SET-01",
            "../TEXTEDIT-01",
        ] {
            value.task_id = id.into();
            assert!(value.validate().is_err());
        }
        value = profile();
        value.catalog_id = "A".repeat(64);
        assert!(value.validate().is_err());
        value = profile();
        value.selector = "b".repeat(63);
        assert!(value.validate().is_err());
        let mut json = serde_json::to_value(profile()).unwrap();
        json["initial_state"] = serde_json::json!({});
        assert!(serde_json::from_value::<FrozenTaskProfile>(json).is_err());
    }
}
