//! Read-only source contracts for the owned, headless browser page adapter.
//!
//! These values describe a real Chromium accessibility tree for the local
//! static fixture. They do not describe a native window and grant no root,
//! permit, or input authority.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

/// Maximum JavaScript-safe integer. These values cross a JSON boundary through
/// JavaScript numbers and therefore must never exceed `Number.MAX_SAFE_INTEGER`.
pub const MAX_BROWSER_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
/// Maximum serialized size of a browser read scope or source-control message.
pub const MAX_BROWSER_READ_SCOPE_BYTES: usize = 8_192;
/// Maximum number of real AX nodes copied into one page snapshot.
pub const MAX_BROWSER_PAGE_NODES: usize = 2_048;
/// Maximum ordered children retained for one AX node.
pub const MAX_BROWSER_PAGE_CHILDREN_PER_NODE: usize = 2_048;
/// Maximum total parent-child edges in one page snapshot.
pub const MAX_BROWSER_PAGE_EDGES: usize = 8_192;
/// Maximum tree depth, counting the root as depth one.
pub const MAX_BROWSER_PAGE_TREE_DEPTH: usize = 256;
/// Maximum serialized snapshot size, including context and bounded node text.
pub const MAX_BROWSER_PAGE_SNAPSHOT_BYTES: usize = 2 * 1024 * 1024;

const MAX_CDP_IDENTIFIER_BYTES: usize = 256;
const MAX_FIXTURE_URL_BYTES: usize = 4_096;
const MAX_ROLE_BYTES: usize = 128;
const MAX_NAME_BYTES: usize = 512;
const CHECKBOX_ROLE: &str = "checkbox";

/// The only browser page profile admitted by this protocol revision. It names
/// a local static fixture, not a model-selected origin or arbitrary website.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BrowserPageProfile {
    FixtureBackgroundOption,
}

impl BrowserPageProfile {
    pub const fn app(self) -> &'static str {
        "browser.fixture"
    }

    pub const fn semantic_target(self) -> &'static str {
        "browser.fixture.background_option"
    }

    pub const fn label(self) -> &'static str {
        "Background option"
    }

    pub const fn role(self) -> &'static str {
        CHECKBOX_ROLE
    }
}

/// Host-to-Main request for a fresh read from the already-installed page
/// scope. This carries no action or input authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BrowserRefreshRequest {
    #[schemars(length(min = 36, max = 36))]
    pub request_id: String,
    #[schemars(length(min = 36, max = 36))]
    pub scope_id: String,
    #[schemars(range(min = 1, max = 9_007_199_254_740_991u64))]
    pub generation: u64,
    pub context: BrowserTargetContext,
    #[schemars(length(min = 68, max = 68))]
    pub target_context_hash: String,
    #[schemars(length(min = 1, max = 128))]
    pub clock_domain: String,
    #[schemars(range(min = 1, max = 9_007_199_254_740_991u64))]
    pub after_source_sequence: u64,
    #[schemars(range(max = 9_007_199_254_740_991u64))]
    /// Conservative time threshold for the next page read, in `clock_domain`.
    pub after_ms: u64,
    #[schemars(range(max = 9_007_199_254_740_991u64))]
    pub deadline_ms: u64,
}

impl BrowserRefreshRequest {
    pub fn validate(&self) -> Result<(), String> {
        if !valid_scope_uuid(&self.request_id) || !valid_scope_uuid(&self.scope_id) {
            return Err("browser refresh request and scope IDs must be canonical UUIDs".into());
        }
        if self.generation == 0 || self.generation > MAX_BROWSER_SAFE_INTEGER {
            return Err("browser refresh generation must be positive and JS-safe".into());
        }
        self.context.validate()?;
        if browser_target_context_hash(&self.context)? != self.target_context_hash {
            return Err("browser refresh target-context hash does not match its context".into());
        }
        if !valid_text(&self.clock_domain, 128)
            || self.after_source_sequence == 0
            || self.after_source_sequence > MAX_BROWSER_SAFE_INTEGER
            || self.after_ms > MAX_BROWSER_SAFE_INTEGER
            || self.deadline_ms > MAX_BROWSER_SAFE_INTEGER
            || self.deadline_ms <= self.after_ms
            || self.deadline_ms - self.after_ms > 30_000
        {
            return Err("browser refresh bounds or same-domain deadline are invalid".into());
        }
        let bytes = serde_json::to_vec(self)
            .map_err(|_| "browser refresh request could not be serialized".to_string())?;
        if bytes.len() > MAX_BROWSER_READ_SCOPE_BYTES {
            return Err("browser refresh request exceeds its serialized byte bound".into());
        }
        Ok(())
    }
}

/// Exact browser target and document identity for this owned read-only source.
///
/// The field order is part of the `tc1` hash contract. Keep this a fixed-field
/// struct and do not add snapshot sequence or read-time fields here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BrowserTargetContext {
    #[schemars(length(min = 1, max = 256))]
    pub browser_session_id: String,
    #[schemars(length(min = 1, max = 256))]
    pub browser_context_id: String,
    #[schemars(length(min = 1, max = 256))]
    pub page_target_id: String,
    #[schemars(length(min = 1, max = 256))]
    pub cdp_session_id: String,
    #[schemars(length(min = 1, max = 256))]
    pub frame_id: String,
    #[schemars(length(min = 1, max = 256))]
    pub document_loader_id: String,
    #[schemars(length(min = 1, max = 4096))]
    pub fixture_file_url: String,
    #[schemars(range(min = 1, max = 9_007_199_254_740_991u64))]
    pub session_epoch: u64,
}

impl BrowserTargetContext {
    pub fn validate(&self) -> Result<(), String> {
        for (field, value) in [
            ("browser_session_id", self.browser_session_id.as_str()),
            ("browser_context_id", self.browser_context_id.as_str()),
            ("page_target_id", self.page_target_id.as_str()),
            ("cdp_session_id", self.cdp_session_id.as_str()),
            ("frame_id", self.frame_id.as_str()),
            ("document_loader_id", self.document_loader_id.as_str()),
        ] {
            if !valid_identifier(value, MAX_CDP_IDENTIFIER_BYTES) {
                return Err(format!(
                    "browser target context {field} is empty or unbounded"
                ));
            }
        }
        if self.session_epoch == 0 || self.session_epoch > MAX_BROWSER_SAFE_INTEGER {
            return Err(
                "browser target context session_epoch is not a positive JS-safe integer".into(),
            );
        }
        if !valid_fixture_file_url(&self.fixture_file_url) {
            return Err(
                "browser target context must carry the exact local static fixture file URL".into(),
            );
        }
        Ok(())
    }
}

/// A host-installed, read-only scope that authorizes observation of exactly
/// one owned browser target/document generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BrowserReadScope {
    #[schemars(length(min = 36, max = 36))]
    pub scope_id: String,
    #[schemars(range(min = 1, max = 9_007_199_254_740_991u64))]
    pub generation: u64,
    pub context: BrowserTargetContext,
    #[schemars(length(min = 1, max = 128))]
    pub clock_domain: String,
    #[schemars(range(max = 9_007_199_254_740_991u64))]
    pub issued_ms: u64,
}

impl BrowserReadScope {
    pub fn validate(&self) -> Result<(), String> {
        if !valid_scope_uuid(&self.scope_id) {
            return Err("browser read scope_id must be a canonical UUID".into());
        }
        if self.generation == 0 || self.generation > MAX_BROWSER_SAFE_INTEGER {
            return Err("browser read scope generation must be a positive JS-safe integer".into());
        }
        if self.issued_ms > MAX_BROWSER_SAFE_INTEGER {
            return Err("browser read scope issued_ms is not a JS-safe integer".into());
        }
        if !valid_text(&self.clock_domain, 128) {
            return Err("browser read scope clock_domain is empty or unbounded".into());
        }
        self.context.validate()?;
        let context_hash = browser_target_context_hash(&self.context)?;
        if !valid_target_context_hash(&context_hash) {
            return Err("browser read scope context hash is not canonical tc1".into());
        }
        let bytes = serde_json::to_vec(self)
            .map_err(|_| "browser read scope could not be serialized".to_string())?;
        if bytes.len() > MAX_BROWSER_READ_SCOPE_BYTES {
            return Err("browser read scope exceeds its serialized byte bound".into());
        }
        Ok(())
    }
}

/// Closed reasons for ending an installed browser read scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BrowserSourceInvalidation {
    SourceUnavailable,
    DocumentChanged,
    SessionClosed,
    Stopped,
}

/// Main-to-host scope lifecycle control. It carries observation scope only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BrowserSourceControl {
    Install {
        scope: Box<BrowserReadScope>,
    },
    Invalidate {
        #[schemars(length(min = 36, max = 36))]
        scope_id: String,
        #[schemars(range(min = 1, max = 9_007_199_254_740_991u64))]
        generation: u64,
        reason: BrowserSourceInvalidation,
    },
}

impl BrowserSourceControl {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Install { scope } => scope.validate()?,
            Self::Invalidate {
                scope_id,
                generation,
                ..
            } => {
                if !valid_scope_uuid(scope_id) {
                    return Err(
                        "browser source invalidation scope_id must be a canonical UUID".into(),
                    );
                }
                if *generation == 0 || *generation > MAX_BROWSER_SAFE_INTEGER {
                    return Err(
                        "browser source invalidation generation must be positive and JS-safe"
                            .into(),
                    );
                }
            }
        }
        let bytes = serde_json::to_vec(self)
            .map_err(|_| "browser source control could not be serialized".to_string())?;
        if bytes.len() > MAX_BROWSER_READ_SCOPE_BYTES {
            return Err("browser source control exceeds its serialized byte bound".into());
        }
        Ok(())
    }
}

/// A direct observation of a CDP boolean property. `unknown` means Chrome did
/// not report the property; it must never be interpreted as false.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BrowserObservedBool {
    Unknown,
    False,
    True,
}

impl BrowserObservedBool {
    const fn known(self) -> Option<bool> {
        match self {
            Self::Unknown => None,
            Self::False => Some(false),
            Self::True => Some(true),
        }
    }
}

/// One bounded node in the real CDP accessibility tree.
///
/// `role` and `name` are copied only when Chrome returned the corresponding
/// values. No value/text/action support is projected into this DTO.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BrowserPageNode {
    #[schemars(length(min = 1, max = 256))]
    pub ax_node_id: String,
    #[schemars(range(min = 1))]
    pub backend_dom_node_id: Option<i32>,
    #[schemars(length(max = 256))]
    pub parent_ax_node_id: Option<String>,
    #[schemars(length(max = 2048))]
    pub child_ax_node_ids: Vec<String>,
    pub ignored: bool,
    #[schemars(length(max = 128))]
    pub role: Option<String>,
    #[schemars(length(max = 512))]
    pub name: Option<String>,
    pub disabled: BrowserObservedBool,
    pub checked: BrowserObservedBool,
}

/// One complete, bounded read of the owned local fixture's page tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BrowserPageSnapshot {
    #[schemars(length(min = 1, max = 256))]
    pub snapshot_id: String,
    #[schemars(range(min = 1, max = 9_007_199_254_740_991u64))]
    pub source_sequence: u64,
    /// Conservative lower bound: the trusted same-domain read-start time.
    #[schemars(range(max = 9_007_199_254_740_991u64))]
    pub observed_ms: u64,
    /// Trusted same-domain read-finish time; never earlier than `observed_ms`.
    #[schemars(range(max = 9_007_199_254_740_991u64))]
    pub read_finished_ms: u64,
    /// Trusted Main-provided target clock domain for same-domain freshness.
    #[schemars(length(min = 1, max = 128))]
    pub clock_domain: String,
    pub context: BrowserTargetContext,
    #[schemars(length(min = 68, max = 68))]
    pub target_context_hash: String,
    #[schemars(length(min = 1, max = 256))]
    pub root_ax_node_id: String,
    #[schemars(length(min = 1, max = 2048))]
    pub nodes: Vec<BrowserPageNode>,
}

impl BrowserPageSnapshot {
    /// Validate bounded values and the actual single-root ordered tree. The
    /// method does not infer completeness from the CDP method name.
    pub fn validate(&self) -> Result<(), String> {
        if !valid_identifier(&self.snapshot_id, 256) {
            return Err("browser page snapshot_id is empty or unbounded".into());
        }
        if self.source_sequence == 0 || self.source_sequence > MAX_BROWSER_SAFE_INTEGER {
            return Err("browser page source_sequence is not a positive JS-safe integer".into());
        }
        if self.observed_ms > MAX_BROWSER_SAFE_INTEGER
            || self.read_finished_ms > MAX_BROWSER_SAFE_INTEGER
            || self.read_finished_ms < self.observed_ms
        {
            return Err("browser page read times are invalid JS-safe same-domain bounds".into());
        }
        if !valid_text(&self.clock_domain, 128) {
            return Err("browser page clock domain is empty or unbounded".into());
        }
        self.context.validate()?;
        if browser_target_context_hash(&self.context)? != self.target_context_hash {
            return Err("browser page target-context hash does not match its fixed context".into());
        }
        if !valid_identifier(&self.root_ax_node_id, MAX_CDP_IDENTIFIER_BYTES) {
            return Err("browser page root AX node id is empty or unbounded".into());
        }
        if self.nodes.is_empty() || self.nodes.len() > MAX_BROWSER_PAGE_NODES {
            return Err("browser page node count is empty or exceeds its bound".into());
        }

        let mut ids = HashMap::with_capacity(self.nodes.len());
        let mut edge_count = 0usize;
        for (index, node) in self.nodes.iter().enumerate() {
            if !valid_identifier(&node.ax_node_id, MAX_CDP_IDENTIFIER_BYTES) {
                return Err("browser page AX node id is empty or unbounded".into());
            }
            if ids.insert(node.ax_node_id.as_str(), index).is_some() {
                return Err("browser page contains duplicate AX node ids".into());
            }
            if node.backend_dom_node_id.is_some_and(|id| id <= 0) {
                return Err("browser page backend DOM node ids must be positive i32 values".into());
            }
            if node
                .parent_ax_node_id
                .as_deref()
                .is_some_and(|id| !valid_identifier(id, MAX_CDP_IDENTIFIER_BYTES))
            {
                return Err("browser page parent AX node id is empty or unbounded".into());
            }
            if node.child_ax_node_ids.len() > MAX_BROWSER_PAGE_CHILDREN_PER_NODE {
                return Err("browser page child count exceeds its per-node bound".into());
            }
            edge_count = edge_count.saturating_add(node.child_ax_node_ids.len());
            if edge_count > MAX_BROWSER_PAGE_EDGES {
                return Err("browser page aggregate edge count exceeds its bound".into());
            }
            if node
                .role
                .as_deref()
                .is_some_and(|role| !valid_observed_text(role, MAX_ROLE_BYTES))
            {
                return Err("browser page role is empty or unbounded".into());
            }
            if node
                .name
                .as_deref()
                .is_some_and(|name| !valid_observed_text(name, MAX_NAME_BYTES))
            {
                return Err("browser page name is empty or unbounded".into());
            }
            let mut child_ids = HashMap::with_capacity(node.child_ax_node_ids.len());
            for child_id in &node.child_ax_node_ids {
                if !valid_identifier(child_id, MAX_CDP_IDENTIFIER_BYTES) {
                    return Err("browser page child AX node id is empty or unbounded".into());
                }
                if child_ids.insert(child_id.as_str(), ()).is_some() {
                    return Err("browser page parent contains a duplicate ordered child id".into());
                }
            }
        }

        let Some(&root_index) = ids.get(self.root_ax_node_id.as_str()) else {
            return Err("browser page root AX node is missing".into());
        };
        let root_count = self
            .nodes
            .iter()
            .filter(|node| node.parent_ax_node_id.is_none())
            .count();
        if root_count != 1
            || self.nodes[root_index].parent_ax_node_id.is_some()
            || self
                .nodes
                .iter()
                .enumerate()
                .any(|(index, node)| index != root_index && node.parent_ax_node_id.is_none())
        {
            return Err("browser page tree must have exactly the declared root".into());
        }

        for node in &self.nodes {
            for child_id in &node.child_ax_node_ids {
                let Some(&child_index) = ids.get(child_id.as_str()) else {
                    return Err("browser page contains a dangling child AX node id".into());
                };
                if self.nodes[child_index].parent_ax_node_id.as_deref()
                    != Some(node.ax_node_id.as_str())
                {
                    return Err("browser page child and actual parent references disagree".into());
                }
            }
            if let Some(parent_id) = node.parent_ax_node_id.as_deref() {
                let Some(&parent_index) = ids.get(parent_id) else {
                    return Err("browser page contains a dangling parent AX node id".into());
                };
                if self.nodes[parent_index]
                    .child_ax_node_ids
                    .iter()
                    .filter(|child_id| child_id.as_str() == node.ax_node_id.as_str())
                    .count()
                    != 1
                {
                    return Err("browser page actual parent omits or duplicates its child".into());
                }
            }
        }

        self.validate_cycles_and_depth(&ids)?;
        self.validate_root_connectivity(root_index, &ids)?;

        let serialized = serde_json::to_vec(self)
            .map_err(|_| "browser page snapshot could not be serialized".to_string())?;
        if serialized.len() > MAX_BROWSER_PAGE_SNAPSHOT_BYTES {
            return Err("browser page snapshot exceeds its serialized byte bound".into());
        }
        Ok(())
    }

    fn validate_cycles_and_depth(&self, ids: &HashMap<&str, usize>) -> Result<(), String> {
        // Iterative DFS keeps validation stack use bounded by the node count.
        let mut color = vec![0u8; self.nodes.len()];
        for start in 0..self.nodes.len() {
            if color[start] != 0 {
                continue;
            }
            color[start] = 1;
            let mut stack = vec![(start, 0usize, 1usize)];
            while !stack.is_empty() {
                let position = stack.len() - 1;
                let (index, next_child, depth) = stack[position];
                let node = &self.nodes[index];
                if next_child >= node.child_ax_node_ids.len() {
                    color[index] = 2;
                    stack.pop();
                    continue;
                }
                let child_id = node.child_ax_node_ids[next_child].as_str();
                stack[position].1 += 1;
                let child_index = *ids.get(child_id).ok_or_else(|| {
                    "browser page contains a dangling child AX node id".to_string()
                })?;
                match color[child_index] {
                    1 => return Err("browser page tree contains a cycle".into()),
                    2 => continue,
                    _ => {
                        let child_depth = depth.saturating_add(1);
                        if child_depth > MAX_BROWSER_PAGE_TREE_DEPTH {
                            return Err("browser page tree exceeds its depth bound".into());
                        }
                        color[child_index] = 1;
                        stack.push((child_index, 0, child_depth));
                    }
                }
            }
        }
        Ok(())
    }

    fn validate_root_connectivity(
        &self,
        root_index: usize,
        ids: &HashMap<&str, usize>,
    ) -> Result<(), String> {
        let mut visited = vec![false; self.nodes.len()];
        let mut stack = vec![root_index];
        visited[root_index] = true;
        while let Some(index) = stack.pop() {
            for child_id in &self.nodes[index].child_ax_node_ids {
                let child_index = *ids.get(child_id.as_str()).ok_or_else(|| {
                    "browser page contains a dangling child AX node id".to_string()
                })?;
                if !visited[child_index] {
                    visited[child_index] = true;
                    stack.push(child_index);
                }
            }
        }
        if visited.iter().any(|seen| !seen) {
            return Err("browser page contains nodes outside the declared root tree".into());
        }
        Ok(())
    }
}

/// Stable, fixed-field SHA-256 identity for one browser target/document.
///
/// The bytes are compact UTF-8 JSON from `serde_json::to_vec` over
/// `BrowserTargetContext`; struct field order above is the canonical key order.
/// Snapshot ids, sequences, and read times are deliberately excluded.
pub fn browser_target_context_hash(context: &BrowserTargetContext) -> Result<String, String> {
    context.validate()?;
    let bytes = serde_json::to_vec(context)
        .map_err(|_| "browser target context could not be serialized".to_string())?;
    let digest = Sha256::digest(bytes);
    Ok(format!("tc1:{digest:x}"))
}

/// Read-only semantic binding derived from one unique observed checkbox.
///
/// This is a source descriptor only. It cannot be converted into a root,
/// permit, capability, or input request by this module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BrowserCheckboxBinding {
    #[schemars(length(min = 1, max = 256))]
    pub snapshot_id: String,
    #[schemars(range(min = 1, max = 9_007_199_254_740_991u64))]
    pub source_sequence: u64,
    #[schemars(range(max = 9_007_199_254_740_991u64))]
    pub observed_ms: u64,
    #[schemars(range(max = 9_007_199_254_740_991u64))]
    pub read_finished_ms: u64,
    #[schemars(length(min = 1, max = 128))]
    pub clock_domain: String,
    pub context: BrowserTargetContext,
    #[schemars(length(min = 68, max = 68))]
    pub target_context_hash: String,
    #[schemars(length(min = 1, max = 256))]
    pub ax_node_id: String,
    #[schemars(range(min = 1))]
    pub backend_dom_node_id: i32,
    pub role: String,
    #[schemars(length(min = 1, max = 512))]
    pub name: String,
    pub disabled: BrowserObservedBool,
    pub checked: bool,
}

impl BrowserCheckboxBinding {
    pub fn validate(&self) -> Result<(), String> {
        if !valid_identifier(&self.snapshot_id, 256)
            || self.source_sequence == 0
            || self.source_sequence > MAX_BROWSER_SAFE_INTEGER
            || self.observed_ms > MAX_BROWSER_SAFE_INTEGER
            || self.read_finished_ms > MAX_BROWSER_SAFE_INTEGER
            || self.read_finished_ms < self.observed_ms
            || !valid_text(&self.clock_domain, 128)
            || !valid_identifier(&self.ax_node_id, MAX_CDP_IDENTIFIER_BYTES)
            || self.backend_dom_node_id <= 0
            || self.role != CHECKBOX_ROLE
            || !valid_text(&self.name, MAX_NAME_BYTES)
            || self.disabled != BrowserObservedBool::False
        {
            return Err(
                "browser checkbox binding is invalid or lacks observed enabled state".into(),
            );
        }
        self.context.validate()?;
        if browser_target_context_hash(&self.context)? != self.target_context_hash {
            return Err("browser checkbox binding context hash does not match".into());
        }
        Ok(())
    }
}

/// Read-only proof that a later real page read still observes the same
/// checkbox in the same browser/document/node context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BrowserCheckboxWitness {
    pub binding: BrowserCheckboxBinding,
    #[schemars(length(min = 1, max = 256))]
    pub snapshot_id: String,
    #[schemars(range(min = 1, max = 9_007_199_254_740_991u64))]
    pub source_sequence: u64,
    #[schemars(range(max = 9_007_199_254_740_991u64))]
    pub observed_ms: u64,
    #[schemars(range(max = 9_007_199_254_740_991u64))]
    pub read_finished_ms: u64,
    #[schemars(length(min = 1, max = 128))]
    pub clock_domain: String,
    #[schemars(length(min = 68, max = 68))]
    pub target_context_hash: String,
    #[schemars(length(min = 1, max = 256))]
    pub ax_node_id: String,
    #[schemars(range(min = 1))]
    pub backend_dom_node_id: i32,
    pub role: String,
    #[schemars(length(min = 1, max = 512))]
    pub name: String,
    pub ignored: bool,
    pub disabled: BrowserObservedBool,
    pub observed_checked: bool,
}

impl BrowserCheckboxWitness {
    pub fn validate(&self) -> Result<(), String> {
        self.binding.validate()?;
        if !valid_identifier(&self.snapshot_id, 256)
            || self.snapshot_id == self.binding.snapshot_id
            || self.source_sequence <= self.binding.source_sequence
            || self.source_sequence > MAX_BROWSER_SAFE_INTEGER
            || self.observed_ms > MAX_BROWSER_SAFE_INTEGER
            || self.read_finished_ms > MAX_BROWSER_SAFE_INTEGER
            || self.read_finished_ms < self.observed_ms
            || self.observed_ms < self.binding.read_finished_ms
            || self.clock_domain != self.binding.clock_domain
            || self.target_context_hash != self.binding.target_context_hash
            || !valid_identifier(&self.ax_node_id, MAX_CDP_IDENTIFIER_BYTES)
            || self.ax_node_id != self.binding.ax_node_id
            || self.backend_dom_node_id <= 0
            || self.backend_dom_node_id != self.binding.backend_dom_node_id
            || self.role != CHECKBOX_ROLE
            || !valid_text(&self.name, MAX_NAME_BYTES)
            || self.name != self.binding.name
            || self.ignored
            || self.disabled != BrowserObservedBool::False
        {
            return Err("browser checkbox witness does not preserve its fresh bound target".into());
        }
        Ok(())
    }
}

/// Derive a source-only checkbox binding from exactly one matching observed
/// role/name. Unknown disabled or checked state never qualifies.
pub fn derive_browser_checkbox_binding(
    snapshot: &BrowserPageSnapshot,
    expected_name: &str,
) -> Result<BrowserCheckboxBinding, String> {
    snapshot.validate()?;
    if !valid_text(expected_name, MAX_NAME_BYTES) {
        return Err("browser checkbox name selector is empty or unbounded".into());
    }
    let mut candidates = snapshot.nodes.iter().filter(|node| {
        node.role.as_deref() == Some(CHECKBOX_ROLE) && node.name.as_deref() == Some(expected_name)
    });
    let Some(node) = candidates.next() else {
        return Err("browser checkbox role/name selector has no observed match".into());
    };
    if candidates.next().is_some() {
        return Err("browser checkbox role/name selector is ambiguous".into());
    }
    let Some(backend_dom_node_id) = node.backend_dom_node_id.filter(|id| *id > 0) else {
        return Err("browser checkbox has no actual backend DOM node id".into());
    };
    let Some(checked) = node.checked.known() else {
        return Err("browser checkbox checked state was not observed".into());
    };
    if node.ignored || node.disabled != BrowserObservedBool::False {
        return Err("browser checkbox is ignored or lacks observed enabled state".into());
    }
    let binding = BrowserCheckboxBinding {
        snapshot_id: snapshot.snapshot_id.clone(),
        source_sequence: snapshot.source_sequence,
        observed_ms: snapshot.observed_ms,
        read_finished_ms: snapshot.read_finished_ms,
        clock_domain: snapshot.clock_domain.clone(),
        context: snapshot.context.clone(),
        target_context_hash: snapshot.target_context_hash.clone(),
        ax_node_id: node.ax_node_id.clone(),
        backend_dom_node_id,
        role: CHECKBOX_ROLE.into(),
        name: expected_name.into(),
        disabled: node.disabled,
        checked,
    };
    binding.validate()?;
    Ok(binding)
}

/// Derive a fresh, read-only checkbox witness. It requires a strictly newer
/// read and the exact same session, document, semantic label, AX id, and DOM id.
pub fn derive_browser_checkbox_witness(
    binding: &BrowserCheckboxBinding,
    snapshot: &BrowserPageSnapshot,
) -> Result<BrowserCheckboxWitness, String> {
    binding.validate()?;
    snapshot.validate()?;
    if snapshot.source_sequence <= binding.source_sequence
        || snapshot.observed_ms < binding.read_finished_ms
        || snapshot.clock_domain != binding.clock_domain
        || snapshot.snapshot_id == binding.snapshot_id
        || snapshot.context != binding.context
        || snapshot.target_context_hash != binding.target_context_hash
    {
        return Err(
            "browser checkbox witness requires a fresh read in the same target context".into(),
        );
    }

    if snapshot
        .nodes
        .iter()
        .filter(|node| {
            node.role.as_deref() == Some(CHECKBOX_ROLE)
                && node.name.as_deref() == Some(binding.name.as_str())
        })
        .count()
        != 1
    {
        return Err(
            "browser checkbox witness role/name selector is not unique in the fresh read".into(),
        );
    }

    let mut matches = snapshot.nodes.iter().filter(|node| {
        node.ax_node_id == binding.ax_node_id
            && node.backend_dom_node_id == Some(binding.backend_dom_node_id)
    });
    let Some(node) = matches.next() else {
        return Err("browser checkbox witness cannot find the exact bound AX and DOM nodes".into());
    };
    if matches.next().is_some()
        || node.role.as_deref() != Some(CHECKBOX_ROLE)
        || node.name.as_deref() != Some(binding.name.as_str())
    {
        return Err("browser checkbox witness target node changed or became ambiguous".into());
    }
    let Some(observed_checked) = node.checked.known() else {
        return Err("browser checkbox witness checked state was not observed".into());
    };
    let witness = BrowserCheckboxWitness {
        binding: binding.clone(),
        snapshot_id: snapshot.snapshot_id.clone(),
        source_sequence: snapshot.source_sequence,
        observed_ms: snapshot.observed_ms,
        read_finished_ms: snapshot.read_finished_ms,
        clock_domain: snapshot.clock_domain.clone(),
        target_context_hash: snapshot.target_context_hash.clone(),
        ax_node_id: node.ax_node_id.clone(),
        backend_dom_node_id: binding.backend_dom_node_id,
        role: CHECKBOX_ROLE.into(),
        name: binding.name.clone(),
        ignored: node.ignored,
        disabled: node.disabled,
        observed_checked,
    };
    witness.validate()?;
    Ok(witness)
}

/// Small native-free page snapshots for the private browser contract exporter.
/// They cover fixed ASCII identity, UTF-8 identity text, and an escaped local
/// file URL while keeping the page content limited to this public fixture.
pub fn browser_contract_golden_snapshots() -> Result<Vec<BrowserPageSnapshot>, String> {
    let contexts = [
        BrowserTargetContext {
            browser_session_id: "session-1".into(),
            browser_context_id: "context-1".into(),
            page_target_id: "target-1".into(),
            cdp_session_id: "cdp-1".into(),
            frame_id: "frame-1".into(),
            document_loader_id: "loader-1".into(),
            fixture_file_url: "file:///tmp/computer-use/fixtures/headless/readonly-page.html"
                .into(),
            session_epoch: 7,
        },
        BrowserTargetContext {
            browser_session_id: "session-雪".into(),
            browser_context_id: "context-雪".into(),
            page_target_id: "target-2".into(),
            cdp_session_id: "cdp-2".into(),
            frame_id: "frame-2".into(),
            document_loader_id: "loader-2".into(),
            fixture_file_url: "file:///tmp/computer-use/fixtures/headless/readonly-page.html"
                .into(),
            session_epoch: 8,
        },
        BrowserTargetContext {
            browser_session_id: "session-3".into(),
            browser_context_id: "context-3".into(),
            page_target_id: "target-3".into(),
            cdp_session_id: "cdp-3".into(),
            frame_id: "frame-3".into(),
            document_loader_id: "loader-3".into(),
            fixture_file_url:
                "file:///tmp/with%20space%20%23%25/fixtures/headless/readonly-page.html".into(),
            session_epoch: 9,
        },
    ];

    contexts
        .into_iter()
        .enumerate()
        .map(|(index, context)| {
            let sequence = index as u64 + 1;
            let snapshot = BrowserPageSnapshot {
                snapshot_id: format!("golden-{sequence}"),
                source_sequence: sequence,
                observed_ms: 100 + sequence * 10,
                read_finished_ms: 105 + sequence * 10,
                clock_domain: "headless-performance-monotonic-ms".into(),
                target_context_hash: browser_target_context_hash(&context)?,
                context,
                root_ax_node_id: "root-ax".into(),
                nodes: vec![
                    BrowserPageNode {
                        ax_node_id: "root-ax".into(),
                        backend_dom_node_id: None,
                        parent_ax_node_id: None,
                        child_ax_node_ids: vec!["checkbox-ax".into()],
                        ignored: false,
                        role: Some("RootWebArea".into()),
                        name: Some("Owned Headless Source Fixture".into()),
                        disabled: BrowserObservedBool::Unknown,
                        checked: BrowserObservedBool::Unknown,
                    },
                    BrowserPageNode {
                        ax_node_id: "checkbox-ax".into(),
                        backend_dom_node_id: Some(17),
                        parent_ax_node_id: Some("root-ax".into()),
                        child_ax_node_ids: Vec::new(),
                        ignored: false,
                        role: Some(CHECKBOX_ROLE.into()),
                        name: Some("Background option".into()),
                        disabled: BrowserObservedBool::Unknown,
                        checked: BrowserObservedBool::False,
                    },
                ],
            };
            snapshot.validate()?;
            Ok(snapshot)
        })
        .collect()
}

fn valid_identifier(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && !value.chars().any(char::is_control)
        && !value.trim().is_empty()
}

pub(super) fn valid_scope_uuid(value: &str) -> bool {
    if value.len() != 36 || !value.is_ascii() {
        return false;
    }
    let bytes = value.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(index, 8 | 13 | 18 | 23) {
            if *byte != b'-' {
                return false;
            }
        } else if !(byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)) {
            return false;
        }
    }
    matches!(bytes[14], b'1'..=b'8') && matches!(bytes[19], b'8' | b'9' | b'a' | b'b')
}

fn valid_target_context_hash(hash: &str) -> bool {
    hash.strip_prefix("tc1:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn valid_text(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && !value.chars().any(char::is_control)
        && !value.trim().is_empty()
}

fn valid_observed_text(value: &str, max_bytes: usize) -> bool {
    value.len() <= max_bytes && !value.chars().any(char::is_control)
}

fn valid_fixture_file_url(value: &str) -> bool {
    if value.len() > MAX_FIXTURE_URL_BYTES
        || !value.starts_with("file:///")
        || value
            .chars()
            .any(|character| matches!(character, '?' | '#' | '\\' | ' '))
        || value.chars().any(char::is_control)
        || !value.ends_with("/fixtures/headless/readonly-page.html")
    {
        return false;
    }
    let Some(path) = value.strip_prefix("file:///") else {
        return false;
    };
    if path.is_empty() || path.starts_with('/') {
        return false;
    }
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len()
                || !bytes[index + 1].is_ascii_hexdigit()
                || !bytes[index + 2].is_ascii_hexdigit()
            {
                return false;
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE_URL: &str = "file:///tmp/computer-use/fixtures/headless/readonly-page.html";

    fn context() -> BrowserTargetContext {
        BrowserTargetContext {
            browser_session_id: "session-1".into(),
            browser_context_id: "context-1".into(),
            page_target_id: "target-1".into(),
            cdp_session_id: "cdp-1".into(),
            frame_id: "frame-1".into(),
            document_loader_id: "loader-1".into(),
            fixture_file_url: FIXTURE_URL.into(),
            session_epoch: 7,
        }
    }

    fn browser_read_scope() -> BrowserReadScope {
        BrowserReadScope {
            scope_id: "550e8400-e29b-41d4-a716-446655440000".into(),
            generation: 1,
            context: context(),
            clock_domain: "headless-performance-monotonic-ms".into(),
            issued_ms: 100,
        }
    }

    fn snapshot(sequence: u64, context: BrowserTargetContext) -> BrowserPageSnapshot {
        let target_context_hash = browser_target_context_hash(&context).unwrap();
        BrowserPageSnapshot {
            snapshot_id: format!("snapshot-{sequence}"),
            source_sequence: sequence,
            observed_ms: sequence * 100,
            read_finished_ms: sequence * 100 + 10,
            clock_domain: "headless-performance-monotonic-ms".into(),
            context,
            target_context_hash,
            root_ax_node_id: "ax-root".into(),
            nodes: vec![
                BrowserPageNode {
                    ax_node_id: "ax-root".into(),
                    backend_dom_node_id: None,
                    parent_ax_node_id: None,
                    child_ax_node_ids: vec!["ax-checkbox".into()],
                    ignored: false,
                    role: Some("RootWebArea".into()),
                    name: None,
                    disabled: BrowserObservedBool::Unknown,
                    checked: BrowserObservedBool::Unknown,
                },
                BrowserPageNode {
                    ax_node_id: "ax-checkbox".into(),
                    backend_dom_node_id: Some(17),
                    parent_ax_node_id: Some("ax-root".into()),
                    child_ax_node_ids: Vec::new(),
                    ignored: false,
                    role: Some(CHECKBOX_ROLE.into()),
                    name: Some("Background option".into()),
                    disabled: BrowserObservedBool::False,
                    checked: BrowserObservedBool::False,
                },
            ],
        }
    }

    #[test]
    fn browser_read_scope_binds_canonical_context_and_has_strict_js_safe_bounds() {
        let scope = browser_read_scope();
        scope.validate().unwrap();
        let context_hash = browser_target_context_hash(&scope.context).unwrap();
        assert!(valid_target_context_hash(&context_hash));
        assert!(serde_json::to_vec(&scope).unwrap().len() <= MAX_BROWSER_READ_SCOPE_BYTES);

        let mut bad_uuid = scope.clone();
        bad_uuid.scope_id = "550E8400-e29b-41d4-a716-446655440000".into();
        assert!(bad_uuid.validate().is_err());

        let mut zero_generation = scope.clone();
        zero_generation.generation = 0;
        assert!(zero_generation.validate().is_err());

        let mut unsafe_generation = scope.clone();
        unsafe_generation.generation = MAX_BROWSER_SAFE_INTEGER + 1;
        assert!(unsafe_generation.validate().is_err());

        let mut unsafe_issued_time = scope.clone();
        unsafe_issued_time.issued_ms = MAX_BROWSER_SAFE_INTEGER + 1;
        assert!(unsafe_issued_time.validate().is_err());

        let mut empty_clock_domain = scope.clone();
        empty_clock_domain.clock_domain.clear();
        assert!(empty_clock_domain.validate().is_err());

        let mut controlled_clock_domain = scope.clone();
        controlled_clock_domain.clock_domain.push('\n');
        assert!(controlled_clock_domain.validate().is_err());

        let mut extra_field = serde_json::to_value(scope).unwrap();
        extra_field["input_authorized"] = serde_json::json!(true);
        assert!(serde_json::from_value::<BrowserReadScope>(extra_field).is_err());
    }

    #[test]
    fn browser_scope_maximum_component_values_stay_within_the_8192_byte_wire_bound() {
        let mut scope = browser_read_scope();
        scope.generation = MAX_BROWSER_SAFE_INTEGER;
        scope.issued_ms = MAX_BROWSER_SAFE_INTEGER;
        scope.clock_domain = "c".repeat(128);
        scope.context.browser_session_id = "s".repeat(256);
        scope.context.browser_context_id = "c".repeat(256);
        scope.context.page_target_id = "p".repeat(256);
        scope.context.cdp_session_id = "d".repeat(256);
        scope.context.frame_id = "f".repeat(256);
        scope.context.document_loader_id = "l".repeat(256);
        let suffix = "/fixtures/headless/readonly-page.html";
        let prefix_length = MAX_FIXTURE_URL_BYTES - "file:///".len() - suffix.len();
        scope.context.fixture_file_url = format!("file:///{}{suffix}", "a".repeat(prefix_length));
        assert_eq!(scope.context.fixture_file_url.len(), MAX_FIXTURE_URL_BYTES);
        scope.context.session_epoch = MAX_BROWSER_SAFE_INTEGER;

        scope.validate().unwrap();
        assert!(serde_json::to_vec(&scope).unwrap().len() <= MAX_BROWSER_READ_SCOPE_BYTES);
    }

    #[test]
    fn browser_source_control_is_closed_and_validates_scope_generation() {
        let install = BrowserSourceControl::Install {
            scope: Box::new(browser_read_scope()),
        };
        install.validate().unwrap();

        let invalidate = BrowserSourceControl::Invalidate {
            scope_id: "550e8400-e29b-41d4-a716-446655440000".into(),
            generation: 2,
            reason: BrowserSourceInvalidation::SessionClosed,
        };
        invalidate.validate().unwrap();

        let mut unknown_reason = serde_json::to_value(invalidate).unwrap();
        unknown_reason["reason"] = serde_json::json!("resume_input");
        assert!(serde_json::from_value::<BrowserSourceControl>(unknown_reason).is_err());

        let mut invalidation = BrowserSourceControl::Invalidate {
            scope_id: "550e8400-e29b-41d4-a716-446655440000".into(),
            generation: 0,
            reason: BrowserSourceInvalidation::Stopped,
        };
        assert!(invalidation.validate().is_err());
        if let BrowserSourceControl::Invalidate { generation, .. } = &mut invalidation {
            *generation = MAX_BROWSER_SAFE_INTEGER + 1;
        }
        assert!(invalidation.validate().is_err());
    }

    #[test]
    fn fixed_context_hash_has_stable_tc1_golden_and_excludes_read_metadata() {
        let context = context();
        assert_eq!(
            browser_target_context_hash(&context).unwrap(),
            "tc1:450a130de5f22e3324cecd731df368361f9752d80c456bd869becba691ab3b72"
        );
        let first = snapshot(1, context.clone());
        let second = snapshot(2, context);
        assert_eq!(first.target_context_hash, second.target_context_hash);
        assert_ne!(first.source_sequence, second.source_sequence);
        assert_ne!(first.observed_ms, second.observed_ms);

        let mut alternate_clock = first.clone();
        alternate_clock.clock_domain = "other-monotonic-clock".into();
        alternate_clock.validate().unwrap();
        assert_eq!(
            alternate_clock.target_context_hash,
            first.target_context_hash
        );

        let mut alternate_epoch = first.context.clone();
        alternate_epoch.session_epoch += 1;
        assert_ne!(
            browser_target_context_hash(&alternate_epoch).unwrap(),
            first.target_context_hash
        );
    }

    #[test]
    fn exported_golden_snapshots_cover_ascii_unicode_and_escaped_file_url_without_native_fields() {
        let snapshots = browser_contract_golden_snapshots().unwrap();
        assert_eq!(snapshots.len(), 3);
        assert_eq!(
            snapshots[0].target_context_hash,
            "tc1:450a130de5f22e3324cecd731df368361f9752d80c456bd869becba691ab3b72"
        );
        assert_eq!(
            snapshots[1].target_context_hash,
            "tc1:eba6ae18239e56a65cc3bf989dcbfd442535b8b8ae20fca3e657cdc6734d3716"
        );
        assert_eq!(
            snapshots[2].target_context_hash,
            "tc1:7ba8483edcd13ba010615b69ca879f94fc340239337397801412483ad6aa2200"
        );
        assert!(snapshots[1].context.browser_session_id.contains('雪'));
        assert!(
            snapshots[2]
                .context
                .fixture_file_url
                .contains("%20space%20%23%25")
        );
        for snapshot in snapshots {
            snapshot.validate().unwrap();
            let json = serde_json::to_value(snapshot).unwrap();
            assert!(json.get("window_id").is_none());
            assert!(json.get("native_window_id").is_none());
            assert!(json.get("global_focus").is_none());
        }
    }

    #[test]
    fn snapshot_requires_exact_context_hash_and_safe_same_domain_times() {
        let mut valid = snapshot(1, context());
        valid.validate().unwrap();

        let mut empty_observed_name = snapshot(1, context());
        empty_observed_name.nodes[0].name = Some(String::new());
        empty_observed_name.nodes[0].role = Some(String::new());
        empty_observed_name.validate().unwrap();

        valid.target_context_hash.push('0');
        assert!(valid.validate().is_err());

        let mut invalid_time = snapshot(1, context());
        invalid_time.read_finished_ms = invalid_time.observed_ms - 1;
        assert!(invalid_time.validate().is_err());

        let mut unsafe_sequence = snapshot(1, context());
        unsafe_sequence.source_sequence = MAX_BROWSER_SAFE_INTEGER + 1;
        assert!(unsafe_sequence.validate().is_err());
    }

    #[test]
    fn tree_validation_rejects_duplicate_dangling_mismatched_disconnected_and_cyclic_nodes() {
        let mut duplicate = snapshot(1, context());
        duplicate.nodes[1].ax_node_id = "ax-root".into();
        assert!(duplicate.validate().is_err());

        let mut dangling_child = snapshot(1, context());
        dangling_child.nodes[0].child_ax_node_ids = vec!["missing".into()];
        assert!(dangling_child.validate().is_err());

        let mut dangling_parent = snapshot(1, context());
        dangling_parent.nodes[1].parent_ax_node_id = Some("missing".into());
        assert!(dangling_parent.validate().is_err());

        let mut mismatch = snapshot(1, context());
        mismatch.nodes[1].parent_ax_node_id = Some("ax-checkbox".into());
        assert!(mismatch.validate().is_err());

        let mut disconnected = snapshot(1, context());
        disconnected.nodes[0].child_ax_node_ids.clear();
        assert!(disconnected.validate().is_err());

        let mut cycle = snapshot(1, context());
        cycle.nodes.push(BrowserPageNode {
            ax_node_id: "ax-cycle-a".into(),
            backend_dom_node_id: None,
            parent_ax_node_id: Some("ax-cycle-b".into()),
            child_ax_node_ids: vec!["ax-cycle-b".into()],
            ignored: false,
            role: None,
            name: None,
            disabled: BrowserObservedBool::Unknown,
            checked: BrowserObservedBool::Unknown,
        });
        cycle.nodes.push(BrowserPageNode {
            ax_node_id: "ax-cycle-b".into(),
            backend_dom_node_id: None,
            parent_ax_node_id: Some("ax-cycle-a".into()),
            child_ax_node_ids: vec!["ax-cycle-a".into()],
            ignored: false,
            role: None,
            name: None,
            disabled: BrowserObservedBool::Unknown,
            checked: BrowserObservedBool::Unknown,
        });
        assert!(cycle.validate().is_err());
    }

    #[test]
    fn binding_requires_unique_enabled_known_checkbox_and_witness_requires_fresh_same_node_read() {
        let first = snapshot(1, context());
        let binding = derive_browser_checkbox_binding(&first, "Background option").unwrap();
        binding.validate().unwrap();

        let mut fresh = snapshot(2, context());
        fresh.nodes[1].checked = BrowserObservedBool::True;
        let witness = derive_browser_checkbox_witness(&binding, &fresh).unwrap();
        witness.validate().unwrap();
        assert!(witness.observed_checked);
        assert_eq!(witness.source_sequence, 2);
        assert_eq!(witness.read_finished_ms, 210);

        assert!(derive_browser_checkbox_witness(&binding, &first).is_err());

        let mut overlapping_read = snapshot(2, context());
        overlapping_read.observed_ms = binding.read_finished_ms - 1;
        overlapping_read.read_finished_ms = binding.read_finished_ms;
        assert!(derive_browser_checkbox_witness(&binding, &overlapping_read).is_err());

        let mut changed_clock_domain = snapshot(2, context());
        changed_clock_domain.clock_domain = "other-monotonic-clock".into();
        assert!(derive_browser_checkbox_witness(&binding, &changed_clock_domain).is_err());

        let mut unknown_disabled = first.clone();
        unknown_disabled.nodes[1].disabled = BrowserObservedBool::Unknown;
        assert!(derive_browser_checkbox_binding(&unknown_disabled, "Background option").is_err());

        let mut unknown_checked = first.clone();
        unknown_checked.nodes[1].checked = BrowserObservedBool::Unknown;
        assert!(derive_browser_checkbox_binding(&unknown_checked, "Background option").is_err());

        let mut missing_backend_node = first.clone();
        missing_backend_node.nodes[1].backend_dom_node_id = None;
        assert!(
            derive_browser_checkbox_binding(&missing_backend_node, "Background option").is_err()
        );

        let mut ambiguous = first.clone();
        let mut duplicate = ambiguous.nodes[1].clone();
        duplicate.ax_node_id = "ax-checkbox-2".into();
        duplicate.backend_dom_node_id = Some(18);
        ambiguous.nodes[0]
            .child_ax_node_ids
            .push(duplicate.ax_node_id.clone());
        duplicate.parent_ax_node_id = Some("ax-root".into());
        ambiguous.nodes.push(duplicate);
        assert!(derive_browser_checkbox_binding(&ambiguous, "Background option").is_err());
    }

    #[test]
    fn binding_and_witness_reject_changed_session_document_and_node() {
        let first = snapshot(1, context());
        let binding = derive_browser_checkbox_binding(&first, "Background option").unwrap();

        let mut changed_session_context = context();
        changed_session_context.browser_session_id = "session-2".into();
        let changed_session = snapshot(2, changed_session_context);
        assert!(derive_browser_checkbox_witness(&binding, &changed_session).is_err());

        let mut changed_document_context = context();
        changed_document_context.document_loader_id = "loader-2".into();
        let changed_document = snapshot(2, changed_document_context);
        assert!(derive_browser_checkbox_witness(&binding, &changed_document).is_err());

        let mut changed_node = snapshot(2, context());
        changed_node.nodes[1].ax_node_id = "ax-checkbox-replaced".into();
        changed_node.nodes[0].child_ax_node_ids[0] = "ax-checkbox-replaced".into();
        assert!(changed_node.validate().is_ok());
        assert!(derive_browser_checkbox_witness(&binding, &changed_node).is_err());
    }

    #[test]
    fn context_is_limited_to_the_exact_local_static_fixture_file_url() {
        let mut nonlocal = context();
        nonlocal.fixture_file_url =
            "https://example.invalid/fixtures/headless/readonly-page.html".into();
        assert!(nonlocal.validate().is_err());

        let mut other_file = context();
        other_file.fixture_file_url = "file:///tmp/other.html".into();
        assert!(other_file.validate().is_err());

        let mut query = context();
        query.fixture_file_url = format!("{FIXTURE_URL}?query");
        assert!(query.validate().is_err());
    }

    #[test]
    fn serde_rejects_unknown_fields_and_snapshot_validates_encoded_size_and_depth() {
        let mut value = serde_json::to_value(snapshot(1, context())).unwrap();
        value["native_window_id"] = serde_json::json!("forbidden");
        assert!(serde_json::from_value::<BrowserPageSnapshot>(value).is_err());

        let mut too_deep = snapshot(1, context());
        for index in 0..MAX_BROWSER_PAGE_TREE_DEPTH {
            let parent_id = format!("depth-{index}");
            let child_id = format!("depth-{}", index + 1);
            if index == 0 {
                too_deep.nodes[0].ax_node_id = parent_id.clone();
                too_deep.root_ax_node_id = parent_id.clone();
                too_deep.nodes[0].child_ax_node_ids = vec![child_id.clone()];
                too_deep.nodes[1].ax_node_id = child_id.clone();
                too_deep.nodes[1].parent_ax_node_id = Some(parent_id);
            } else {
                let last = too_deep.nodes.len() - 1;
                too_deep.nodes[last].child_ax_node_ids = vec![child_id.clone()];
                too_deep.nodes.push(BrowserPageNode {
                    ax_node_id: child_id,
                    backend_dom_node_id: None,
                    parent_ax_node_id: Some(parent_id),
                    child_ax_node_ids: Vec::new(),
                    ignored: false,
                    role: None,
                    name: None,
                    disabled: BrowserObservedBool::Unknown,
                    checked: BrowserObservedBool::Unknown,
                });
            }
        }
        assert!(too_deep.validate().is_err());

        let mut too_large = snapshot(1, context());
        too_large.nodes.clear();
        too_large.root_ax_node_id = "large-root".into();
        let mut children = Vec::new();
        for index in 0..2_000 {
            let id = format!("node-{index:04}-{}", "x".repeat(220));
            children.push(id.clone());
            too_large.nodes.push(BrowserPageNode {
                ax_node_id: id,
                backend_dom_node_id: None,
                parent_ax_node_id: Some("large-root".into()),
                child_ax_node_ids: Vec::new(),
                ignored: false,
                role: Some("r".repeat(MAX_ROLE_BYTES)),
                name: Some("n".repeat(MAX_NAME_BYTES)),
                disabled: BrowserObservedBool::Unknown,
                checked: BrowserObservedBool::Unknown,
            });
        }
        too_large.nodes.insert(
            0,
            BrowserPageNode {
                ax_node_id: "large-root".into(),
                backend_dom_node_id: None,
                parent_ax_node_id: None,
                child_ax_node_ids: children,
                ignored: false,
                role: None,
                name: None,
                disabled: BrowserObservedBool::Unknown,
                checked: BrowserObservedBool::Unknown,
            },
        );
        assert!(too_large.validate().is_err());
    }
}
