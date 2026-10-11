//! Bounded native guard process. Default sessions are read-only; a trusted
//! launcher may separately fix an AX execution scope for one process/bundle.
//! Output is a latest-value mailbox with monotonic counters, so a busy tap
//! cannot queue raw events or delay the device's stop loop.

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use desktop_ipc::SharedClock;
use desktop_ipc::link::QueuedWriter;
use execution_core::backend::SubmitContext;
use interaction_contracts::desktop::binding::{
    BindingProof, BindingProofScope, DeviceContextApp, DeviceContextObject, DeviceContextWitness,
};
use interaction_contracts::desktop::device::{DeviceIdentity, PermitRef};
use interaction_contracts::desktop::ids::MAX_ID_LEN;
use interaction_contracts::desktop::owned_registration::{
    MAX_OWNED_REGISTRATION_BYTES, OwnedDocumentRegistration,
};
use interaction_contracts::desktop::primitive::Primitive;
use interaction_contracts::desktop::source::{
    KernelProcessIdentity, ObjectIdentityContinuity, Rect,
};
use interaction_contracts::desktop::task::RowTableScope;
use serde::{Deserialize, Serialize};
use serde_json::json;
use zeroize::Zeroizing;

pub mod authority;
mod owned_journal;
pub mod owned_value;
pub mod submission;
use authority::{NativeProbeRequest, NativeProbeResponse};
use owned_journal::OwnedJournalBootstrap;
use owned_value::{NativeOwnedValueRequest, NativeOwnedValueResponse, OwnedValueBuildOptions};
use submission::NativeSubmitResponse;

const RECORD_LIMIT: u64 = 8192;
const FRESHNESS: Duration = Duration::from_millis(750);
const STARTUP: Duration = Duration::from_millis(1500);
const DIAGNOSTIC_BYTES: usize = 16 * 1024;
const DIAGNOSTIC_RECORDS: usize = 8;
const STDERR_PREFIX_BYTES: usize = 4 * 1024;
const OWNED_VALUE_REQUEST_LIMIT: u64 = 32 * 1024;
const OWNED_VALUE_PROOF_LIMIT: usize = 8 * 1024;
const MAX_CANONICAL_PROCESS_START_SECONDS: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GuardHealthLimits {
    startup: Duration,
    freshness: Duration,
}

impl GuardHealthLimits {
    const PRODUCTION: Self = Self {
        startup: STARTUP,
        freshness: FRESHNESS,
    };

    #[cfg(test)]
    const MOCK_IPC: Self = Self {
        startup: Duration::from_secs(10),
        freshness: Duration::from_secs(5),
    };
}

/// Keep startup and heartbeat expiry in one comparison so protocol tests can
/// vary only helper-process liveness without changing any input deadlines.
fn guard_health_expired(
    elapsed_since_start: Duration,
    latest_snapshot_age: Option<Duration>,
    limits: GuardHealthLimits,
) -> bool {
    latest_snapshot_age.map_or(elapsed_since_start >= limits.startup, |age| {
        age >= limits.freshness
    })
}

fn bounded_native_probe_command(
    request: &NativeProbeRequest,
    epoch: u64,
    command: &str,
) -> Result<serde_json::Value, String> {
    let value = json!({
        "protocol_version": 1,
        "session_epoch": epoch,
        "command": command,
        "probe": request,
    });
    bounded_guard_command(value)
}

fn bounded_guard_command(value: serde_json::Value) -> Result<serde_json::Value, String> {
    let mut record =
        serde_json::to_vec(&value).map_err(|_| "native guard command cannot be serialized")?;
    record.push(b'\n');
    if record.len() as u64 > RECORD_LIMIT {
        return Err("native guard command exceeds the 8192-byte record limit".into());
    }
    Ok(value)
}

/// Owned acceptance diagnostics only; never used by health or authority.
/// Producers only reserve a bounded budget and try_send. Filesystem operations
/// and the directory identity checks belong exclusively to the writer thread.
struct OwnedKeyDiagnostics {
    sender: mpsc::SyncSender<DiagnosticRecord>,
    budget: AtomicUsize,
    mode: OwnedDiagnosticMode,
    record_outbound: bool,
    #[cfg(test)]
    completed: Arc<AtomicUsize>,
    stopping: Arc<AtomicBool>,
    writer: Option<JoinHandle<()>>,
}
struct DiagnosticRecord {
    index: usize,
    category: &'static str,
    bytes: Vec<u8>,
}
struct DiagnosticDirectory {
    directory: PathBuf,
    identity: (u64, u64),
    handle: File,
}
type KeyDiagnostics = Option<Arc<OwnedKeyDiagnostics>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OwnedDiagnosticMode {
    Key,
    Row,
    BackgroundAx,
}

impl OwnedDiagnosticMode {
    fn for_scope(path: &Path, scope: Option<&(u32, String)>, window: Option<&str>) -> Option<Self> {
        let file_name = path.file_name()?.to_str()?;
        let (pid, bundle) = scope?;
        let owned_key = file_name.starts_with("owned-key-");
        let owned_row = file_name
            .strip_prefix("owned-row-")
            .is_some_and(|suffix| !suffix.is_empty());
        match (owned_key, owned_row) {
            (true, false)
                if bundle == "com.lingxi.computer-user.guard-fixture" && window.is_some() =>
            {
                Some(Self::Key)
            }
            (false, true)
                if *pid > 0
                    && bundle == "com.lingxi.computer-user.mvp-fixture"
                    && window.is_some_and(|value| !value.is_empty()) =>
            {
                Some(Self::Row)
            }
            _ => None,
        }
    }

    fn for_background_scope(path: &Path, scope: &NativeBackgroundAxScope) -> Option<Self> {
        let file_name = path.file_name()?.to_str()?;
        let suffix = file_name.strip_prefix("owned-background-")?;
        let mut suffix_chars = suffix.chars();
        if !suffix_chars
            .next()
            .is_some_and(|character| character.is_ascii_alphanumeric())
            || !suffix_chars.all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
            })
            || scope.pid == 0
            || scope.bundle_id != "com.lingxi.computer-user.guard-fixture"
            || !canonical_native_window_id(&scope.window_id)
            || scope.start_seconds == 0
            || scope.start_seconds > MAX_CANONICAL_PROCESS_START_SECONDS
            || scope.start_micros >= 1_000_000
        {
            return None;
        }
        Some(Self::BackgroundAx)
    }

    fn directory_env(self) -> &'static str {
        match self {
            Self::Key => "CU_OWNED_KEY_DIAGNOSTICS_DIR",
            Self::Row => "CU_OWNED_ROW_DIAGNOSTICS_DIR",
            Self::BackgroundAx => "CU_OWNED_BACKGROUND_DIAGNOSTICS_DIR",
        }
    }

    fn allows_category(self, category: &str) -> bool {
        match self {
            Self::Key => true,
            Self::Row => matches!(
                category,
                "row-submit-receipt"
                    | "ax-press-submit-receipt"
                    | "click-submit-receipt"
                    | "window-activation-submit-receipt"
            ),
            Self::BackgroundAx => matches!(
                category,
                "readiness" | "background-authority-probe" | "ax-press-submit-receipt"
            ),
        }
    }
}

fn canonical_native_window_id(value: &str) -> bool {
    value
        .strip_prefix("window-")
        .and_then(|number| number.parse::<u32>().ok())
        .is_some_and(|number| number > 0 && value == format!("window-{number}"))
}

impl DiagnosticDirectory {
    fn write(&self, record: DiagnosticRecord) {
        let Ok(metadata) = fs::symlink_metadata(&self.directory) else {
            return;
        };
        if !metadata.is_dir() || (metadata.dev(), metadata.ino()) != self.identity {
            return;
        }
        let stem = format!("{:02}-{}", record.index, record.category);
        let temporary = format!(".{stem}.pending");
        let published = format!("{stem}.json");
        // Pin creation and publication to the original directory handle.
        // linkat publishes only a complete file and refuses existing targets,
        // including symlinks; diagnostics never overwrite another file.
        let Ok(fd) = rustix::fs::openat(
            &self.handle,
            &temporary,
            rustix::fs::OFlags::WRONLY
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::EXCL
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        ) else {
            return;
        };
        let mut output = File::from(fd);
        if output.write_all(&record.bytes).is_ok()
            && let Ok(identity) = rustix::fs::fstat(&output)
        {
            let matches_output = |name: &str| {
                rustix::fs::statat(&self.handle, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
                    .is_ok_and(|stat| {
                        stat.st_dev == identity.st_dev && stat.st_ino == identity.st_ino
                    })
            };
            if matches_output(&temporary)
                && rustix::fs::linkat(
                    &self.handle,
                    &temporary,
                    &self.handle,
                    &published,
                    rustix::fs::AtFlags::empty(),
                )
                .is_ok()
                && !matches_output(&published)
            {
                let _ =
                    rustix::fs::unlinkat(&self.handle, &published, rustix::fs::AtFlags::empty());
            }
        }
        let _ = rustix::fs::unlinkat(&self.handle, &temporary, rustix::fs::AtFlags::empty());
    }
}
impl OwnedKeyDiagnostics {
    fn open(path: &Path, scope: Option<&(u32, String)>, window: Option<&str>) -> KeyDiagnostics {
        let mode = OwnedDiagnosticMode::for_scope(path, scope, window)?;
        let directory = PathBuf::from(std::env::var_os(mode.directory_env())?);
        let record_outbound = mode == OwnedDiagnosticMode::Key
            && !std::env::var("CU_OWNED_KEY_DIAGNOSTICS_MODE")
                .is_ok_and(|setting| setting == "receipts");
        Self::in_directory_with_profile(directory, mode, record_outbound)
    }
    fn open_background_ax(path: &Path, scope: &NativeBackgroundAxScope) -> KeyDiagnostics {
        let mode = OwnedDiagnosticMode::for_background_scope(path, scope)?;
        let directory = PathBuf::from(std::env::var_os(mode.directory_env())?);
        Self::in_directory_with_profile(directory, mode, false)
    }
    #[cfg(test)]
    fn in_directory(directory: PathBuf) -> KeyDiagnostics {
        Self::in_directory_with_mode(directory, true)
    }
    #[cfg(test)]
    fn in_row_directory(directory: PathBuf) -> KeyDiagnostics {
        Self::in_directory_with_profile(directory, OwnedDiagnosticMode::Row, false)
    }
    #[cfg(test)]
    fn in_directory_with_mode(directory: PathBuf, record_outbound: bool) -> KeyDiagnostics {
        Self::in_directory_with_profile(directory, OwnedDiagnosticMode::Key, record_outbound)
    }
    fn in_directory_with_profile(
        directory: PathBuf,
        mode: OwnedDiagnosticMode,
        record_outbound: bool,
    ) -> KeyDiagnostics {
        let metadata = fs::symlink_metadata(&directory).ok()?;
        if !metadata.is_dir() || metadata.permissions().mode() & 0o777 != 0o700 {
            return None;
        }
        let handle = File::from(
            rustix::fs::open(
                &directory,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::DIRECTORY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .ok()?,
        );
        let pinned = handle.metadata().ok()?;
        if (pinned.dev(), pinned.ino()) != (metadata.dev(), metadata.ino()) {
            return None;
        }
        let directory = DiagnosticDirectory {
            directory,
            identity: (metadata.dev(), metadata.ino()),
            handle,
        };
        Self::spawn_writer_with_profile(DIAGNOSTIC_RECORDS, mode, record_outbound, move |record| {
            directory.write(record)
        })
    }
    #[cfg(test)]
    fn spawn_writer(
        capacity: usize,
        write: impl FnMut(DiagnosticRecord) + Send + 'static,
    ) -> KeyDiagnostics {
        Self::spawn_writer_with_profile(capacity, OwnedDiagnosticMode::Key, true, write)
    }
    fn spawn_writer_with_profile(
        capacity: usize,
        mode: OwnedDiagnosticMode,
        record_outbound: bool,
        mut write: impl FnMut(DiagnosticRecord) + Send + 'static,
    ) -> KeyDiagnostics {
        let (sender, records) = mpsc::sync_channel(capacity);
        let stopping = Arc::new(AtomicBool::new(false));
        #[cfg(test)]
        let completed = Arc::new(AtomicUsize::new(0));
        let worker_stopping = stopping.clone();
        #[cfg(test)]
        let worker_completed = completed.clone();
        let writer = thread::Builder::new()
            .name("owned-key-diagnostics".into())
            .spawn(move || {
                while let Ok(record) = records.recv() {
                    if worker_stopping.load(Ordering::Acquire) {
                        break;
                    }
                    write(record);
                    #[cfg(test)]
                    worker_completed.fetch_add(1, Ordering::Release);
                }
            })
            .ok()?;
        Some(Arc::new(Self {
            sender,
            budget: AtomicUsize::new(0),
            mode,
            record_outbound,
            #[cfg(test)]
            completed,
            stopping,
            writer: Some(writer),
        }))
    }
    fn append(&self, category: &'static str, bytes: &[u8]) -> bool {
        if self.stopping.load(Ordering::Acquire) || !self.mode.allows_category(category) {
            return false;
        }
        // Count and byte reservations are one atomic value. Dropped records
        // still consume their reservation, so repeated full-queue attempts
        // cannot create unbounded allocations or retry the diagnostic write.
        const BYTE_MASK: usize = 0xffff;
        let reserved = self
            .budget
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                let records = used >> 16;
                let byte_count = used & BYTE_MASK;
                if records >= DIAGNOSTIC_RECORDS
                    || bytes.len() > DIAGNOSTIC_BYTES.saturating_sub(byte_count)
                {
                    None
                } else {
                    Some(((records + 1) << 16) | (byte_count + bytes.len()))
                }
            });
        let Ok(used) = reserved else {
            return false;
        };
        self.sender
            .try_send(DiagnosticRecord {
                index: used >> 16,
                category,
                bytes: bytes.to_vec(),
            })
            .is_ok()
    }
    fn record_outbound(&self) -> bool {
        self.mode == OwnedDiagnosticMode::Key && self.record_outbound
    }

    fn record_row_submit_receipt(
        &self,
        response: &NativeSubmitResponse,
        correlation: &OwnedSubmitCorrelation,
    ) -> bool {
        if self.mode != OwnedDiagnosticMode::Row {
            return false;
        }
        let value = row_submit_receipt_value(response, correlation);
        let Ok(bytes) = serde_json::to_vec(&value) else {
            return false;
        };
        bytes.len() <= 4096 && self.append("row-submit-receipt", &bytes)
    }

    fn record_ax_press_submit_receipt(
        &self,
        response: &NativeSubmitResponse,
        correlation: &OwnedSubmitCorrelation,
    ) -> bool {
        if self.mode != OwnedDiagnosticMode::Row || response.ax_trace.is_none() {
            return false;
        }
        let value = ax_press_submit_receipt_value(response, correlation);
        let Ok(bytes) = serde_json::to_vec(&value) else {
            return false;
        };
        bytes.len() <= 4096 && self.append("ax-press-submit-receipt", &bytes)
    }

    fn record_background_ax_authority_probe(
        &self,
        request: &NativeProbeRequest,
        response: &NativeProbeResponse,
        scope: &NativeBackgroundAxScope,
        session_epoch: u64,
    ) -> bool {
        if self.mode != OwnedDiagnosticMode::BackgroundAx
            || !background_ax_request_matches_scope(request, scope)
            || !response.valid_for(request, session_epoch)
        {
            return false;
        }
        let value = background_ax_authority_probe_value(
            response,
            BackgroundAxDiagnosticCorrelation::from_request(request),
        );
        let Ok(bytes) = serde_json::to_vec(&value) else {
            return false;
        };
        bytes.len() <= 1024 && self.append("background-authority-probe", &bytes)
    }

    fn record_background_ax_submit_receipt(
        &self,
        request: &NativeProbeRequest,
        response: &NativeSubmitResponse,
        scope: &NativeBackgroundAxScope,
        session_epoch: u64,
    ) -> bool {
        if self.mode != OwnedDiagnosticMode::BackgroundAx
            || !background_ax_request_matches_scope(request, scope)
            || !response.valid_for(request, session_epoch)
        {
            return false;
        }
        // Keep the validated native AX call trace; a Completed engine receipt
        // alone does not establish whether AXPress ran or how many calls ran.
        let value = background_ax_submit_receipt_value(
            response,
            BackgroundAxDiagnosticCorrelation::from_request(request),
        );
        let Ok(bytes) = serde_json::to_vec(&value) else {
            return false;
        };
        bytes.len() <= 2048 && self.append("ax-press-submit-receipt", &bytes)
    }

    fn record_background_ax_readiness(
        &self,
        sample_index: usize,
        snapshot: &GuardSnapshot,
        backend_native_ready: bool,
        core_isolated: bool,
    ) -> bool {
        if self.mode != OwnedDiagnosticMode::BackgroundAx
            || sample_index >= 6
            || snapshot.session_epoch == 0
            || snapshot.sequence == 0
        {
            return false;
        }
        let value = background_ax_readiness_value(
            sample_index,
            snapshot,
            backend_native_ready,
            core_isolated,
        );
        let Ok(bytes) = serde_json::to_vec(&value) else {
            return false;
        };
        bytes.len() <= 1024 && self.append("readiness", &bytes)
    }

    fn record_click_submit_receipt(
        &self,
        response: &NativeSubmitResponse,
        correlation: &OwnedSubmitCorrelation,
    ) -> bool {
        if self.mode != OwnedDiagnosticMode::Row {
            return false;
        }
        let value = click_submit_receipt_value(response, correlation);
        let Ok(bytes) = serde_json::to_vec(&value) else {
            return false;
        };
        bytes.len() <= 4096 && self.append("click-submit-receipt", &bytes)
    }

    fn record_window_activation_submit_receipt(
        &self,
        request: &NativeProbeRequest,
        response: &NativeSubmitResponse,
        correlation: &OwnedSubmitCorrelation,
        session_epoch: u64,
    ) -> bool {
        if self.mode != OwnedDiagnosticMode::Row
            || !response.valid_for(request, session_epoch)
            || !correlation.matches(request, response, session_epoch)
        {
            return false;
        }
        let Some(context) = correlation.window_activation.as_ref() else {
            return false;
        };
        let value = window_activation_submit_receipt_value(response, correlation, context);
        let Ok(bytes) = serde_json::to_vec(&value) else {
            return false;
        };
        bytes.len() <= 4096 && self.append("window-activation-submit-receipt", &bytes)
    }
}
impl Drop for OwnedKeyDiagnostics {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        // A filesystem write can stall indefinitely. Detach that bounded
        // worker instead of waiting on a stop, helper kill or GuardClient drop.
        if let Some(writer) = self.writer.take()
            && writer.is_finished()
        {
            let _ = writer.join();
        }
    }
}
fn key_diagnostic(diag: &KeyDiagnostics, category: &'static str, value: serde_json::Value) {
    if let Some(diag) = diag
        && let Ok(bytes) = serde_json::to_vec(&value)
    {
        diag.append(category, &bytes);
    }
}
fn key_diagnostic_limited(
    diag: &KeyDiagnostics,
    category: &'static str,
    value: serde_json::Value,
    limit: usize,
) {
    if let Some(diag) = diag
        && let Ok(bytes) = serde_json::to_vec(&value)
        && bytes.len() <= limit
    {
        diag.append(category, &bytes);
    }
}
fn diagnostic_reason(reason: &str) -> &str {
    let mut end = reason.len().min(512);
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    &reason[..end]
}
fn row_submit_receipt_value(
    response: &NativeSubmitResponse,
    correlation: &OwnedSubmitCorrelation,
) -> serde_json::Value {
    json!({
        "kind":"owned_row_guard_diagnostic",
        "category":"row-submit-receipt",
        "protocol_version":response.protocol_version,
        "response_kind":&response.kind,
        "session_epoch":response.session_epoch,
        "request_id":&response.request_id,
        "outcome":&response.outcome,
        "reason":diagnostic_reason(&response.reason),
        "submitted":response.submitted,
        "committed_ms":response.committed_ms,
        "returned_ms":response.returned_ms,
        "binding_digest":&response.binding_digest,
        "row_trace":&response.row_trace,
        "correlation":{
            "intent_id":&correlation.intent_id,
            "attempt_id":&correlation.attempt_id,
            "task_id":&correlation.task_id,
            "task_revision":correlation.task_revision,
            "root_epoch":correlation.root_epoch,
            "session_epoch":correlation.session_epoch,
            "request_id":&correlation.request_id,
        },
    })
}
fn ax_press_submit_receipt_value(
    response: &NativeSubmitResponse,
    correlation: &OwnedSubmitCorrelation,
) -> serde_json::Value {
    let mut value = row_submit_receipt_value(response, correlation);
    let object = value.as_object_mut().expect("receipt has an object shape");
    object.remove("row_trace");
    object.insert("category".into(), json!("ax-press-submit-receipt"));
    object.insert("ax_trace".into(), json!(&response.ax_trace));
    value
}

fn background_ax_authority_probe_value(
    response: &NativeProbeResponse,
    correlation: BackgroundAxDiagnosticCorrelation<'_>,
) -> serde_json::Value {
    json!({
        "kind":"owned_background_ax_diagnostic",
        "category":"background-authority-probe",
        "protocol_version":response.protocol_version,
        "response_kind":&response.kind,
        "session_epoch":response.session_epoch,
        "request_id":correlation.request_id,
        "intent_id":correlation.intent_id,
        "attempt_id":correlation.attempt_id,
        "root_id":correlation.root_id,
        "task_revision":correlation.task_revision,
        "root_epoch":correlation.root_epoch,
        "execution_domain":"background_ax",
        "purpose":"background_ax_action",
        "coordinate_space":"target_object",
        "action":"ax_press",
        "outcome":&response.outcome,
        "input_authorized":response.input_authorized,
    })
}

fn background_ax_submit_receipt_value(
    response: &NativeSubmitResponse,
    correlation: BackgroundAxDiagnosticCorrelation<'_>,
) -> serde_json::Value {
    json!({
        "kind":"owned_background_ax_diagnostic",
        "category":"ax-press-submit-receipt",
        "protocol_version":response.protocol_version,
        "response_kind":&response.kind,
        "session_epoch":response.session_epoch,
        "request_id":correlation.request_id,
        "intent_id":correlation.intent_id,
        "attempt_id":correlation.attempt_id,
        "root_id":correlation.root_id,
        "task_revision":correlation.task_revision,
        "root_epoch":correlation.root_epoch,
        "execution_domain":"background_ax",
        "purpose":"background_ax_action",
        "coordinate_space":"target_object",
        "action":"ax_press",
        "outcome":&response.outcome,
        "submitted":response.submitted,
        "committed_ms":response.committed_ms,
        "returned_ms":response.returned_ms,
        "ax_trace":&response.ax_trace,
        "background_ax_trace":&response.background_ax_trace,
    })
}

fn background_ax_readiness_value(
    sample_index: usize,
    snapshot: &GuardSnapshot,
    backend_native_ready: bool,
    core_isolated: bool,
) -> serde_json::Value {
    json!({
        "kind":"owned_background_ax_diagnostic",
        "category":"readiness",
        "session_epoch":snapshot.session_epoch,
        "sample_index":sample_index,
        "guard_sequence":snapshot.sequence,
        "accessibility":snapshot.accessibility,
        "human_input":snapshot.human_input,
        "startup_mouse_buttons_clear":snapshot.startup_mouse_buttons_clear,
        "authority_clock_ready":snapshot.authority_clock_ready,
        "host_alive":snapshot.host_alive,
        "supervisor_alive":snapshot.supervisor_alive,
        "backend_native_ready":backend_native_ready,
        "core_isolated":core_isolated,
        "ready":backend_native_ready && !core_isolated,
    })
}
fn click_submit_receipt_value(
    response: &NativeSubmitResponse,
    correlation: &OwnedSubmitCorrelation,
) -> serde_json::Value {
    let mut value = row_submit_receipt_value(response, correlation);
    let object = value.as_object_mut().expect("receipt has an object shape");
    object.remove("row_trace");
    object.insert("category".into(), json!("click-submit-receipt"));
    // This is the validated request primitive, not an OS event counter.
    object.insert("primitive".into(), json!("click"));
    value
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct OwnedWindowActivationSourceRead {
    continuity: ObjectIdentityContinuity,
    verified_source_sequence: u64,
    verified_observed_ms: u64,
    context_digest: String,
}

/// Bounded, value-only context copied from the actual WindowRootV1 request.
/// Source object refs and native AX handles never enter the diagnostic record.
#[derive(Debug, Clone, PartialEq, Serialize)]
struct OwnedWindowActivationDiagnostic {
    table_scope: RowTableScope,
    app: DeviceContextApp,
    window: interaction_contracts::desktop::binding::DeviceContextWindow,
    root: DeviceContextObject,
    source_session_id: String,
    session_epoch: u64,
    process: KernelProcessIdentity,
    origin_window_id: String,
    origin_window_frame: Rect,
    original: OwnedWindowActivationSourceRead,
    current: OwnedWindowActivationSourceRead,
}

impl OwnedWindowActivationDiagnostic {
    fn from_request(request: &NativeProbeRequest) -> Option<Self> {
        if request.action != "activate_window" {
            return None;
        }
        let BindingProofScope::WindowRoot {
            table_scope: original_scope,
        } = &request.original_proof.continuity.scope
        else {
            return None;
        };
        let BindingProofScope::WindowRoot {
            table_scope: current_scope,
        } = &request.current_proof.continuity.scope
        else {
            return None;
        };
        if original_scope != current_scope || original_scope.validate().is_err() {
            return None;
        }

        let DeviceContextWitness::WindowRootV1 {
            app: original_app,
            window: original_window,
            root: original_root,
            source: original_source,
        } = &request.original_proof.device_context
        else {
            return None;
        };
        let DeviceContextWitness::WindowRootV1 {
            app,
            window,
            root,
            source,
        } = &request.current_proof.device_context
        else {
            return None;
        };

        if original_app != app
            || original_window != window
            || original_root != root
            || original_source.source_session_id != source.source_session_id
            || original_source.session_epoch != source.session_epoch
            || original_source.process != source.process
            || original_source.origin_window_id != source.origin_window_id
            || original_source.origin_window_frame != source.origin_window_frame
            || original_source.verified_source_sequence
                != request.original_proof.continuity.source_sequence
            || source.verified_source_sequence != request.source_sequence
            || request.current_proof.continuity.source_sequence != request.source_sequence
            || source.verified_observed_ms != request.observed_ms
            || request.original_proof.continuity.context_digest
                != request.current_proof.continuity.context_digest
            || request.original_proof.continuity.kind != request.current_proof.continuity.kind
            || source.verified_source_sequence < original_source.verified_source_sequence
            || source.verified_observed_ms < original_source.verified_observed_ms
        {
            return None;
        }

        Some(Self {
            table_scope: current_scope.clone(),
            app: app.as_ref().clone(),
            window: window.as_ref().clone(),
            root: root.clone(),
            source_session_id: source.source_session_id.clone(),
            session_epoch: source.session_epoch,
            process: source.process.clone(),
            origin_window_id: source.origin_window_id.clone(),
            origin_window_frame: source.origin_window_frame,
            original: OwnedWindowActivationSourceRead {
                continuity: request.original_proof.continuity.kind,
                verified_source_sequence: original_source.verified_source_sequence,
                verified_observed_ms: original_source.verified_observed_ms,
                context_digest: request.original_proof.continuity.context_digest.clone(),
            },
            current: OwnedWindowActivationSourceRead {
                continuity: request.current_proof.continuity.kind,
                verified_source_sequence: source.verified_source_sequence,
                verified_observed_ms: source.verified_observed_ms,
                context_digest: request.current_proof.continuity.context_digest.clone(),
            },
        })
    }
}

fn window_activation_submit_receipt_value(
    response: &NativeSubmitResponse,
    correlation: &OwnedSubmitCorrelation,
    context: &OwnedWindowActivationDiagnostic,
) -> serde_json::Value {
    json!({
        "kind":"owned_row_guard_diagnostic",
        "category":"window-activation-submit-receipt",
        "protocol_version":response.protocol_version,
        "response_kind":&response.kind,
        "session_epoch":response.session_epoch,
        "request_id":&response.request_id,
        "outcome":&response.outcome,
        "reason":diagnostic_reason(&response.reason),
        "submitted":response.submitted,
        "committed_ms":response.committed_ms,
        "returned_ms":response.returned_ms,
        "binding_digest":&response.binding_digest,
        "action":"activate_window",
        "correlation":{
            "intent_id":&correlation.intent_id,
            "attempt_id":&correlation.attempt_id,
            "task_id":&correlation.task_id,
            "task_revision":correlation.task_revision,
            "root_epoch":correlation.root_epoch,
            "session_epoch":correlation.session_epoch,
            "request_id":&correlation.request_id,
        },
        "window_activation":{
            "table_scope":&context.table_scope,
            "stable_root":{
                "app":&context.app,
                "window":&context.window,
                "root":&context.root,
                "source_identity":{
                    "source_session_id":&context.source_session_id,
                    "session_epoch":context.session_epoch,
                    "process":&context.process,
                    "origin_window_id":&context.origin_window_id,
                    "origin_window_frame":context.origin_window_frame,
                },
            },
            "source_reads":{
                "original":&context.original,
                "current":&context.current,
            },
        },
    })
}
fn key_reader_failure(diag: &KeyDiagnostics, incoming: &Mutex<Mailbox>, category: &'static str) {
    let phase = incoming.lock().ok().and_then(|inbox| inbox.phase);
    key_diagnostic(
        diag,
        "reader-failure",
        json!({"kind":"owned_key_guard_reader_failure", "category":category,
        "phase":format!("{phase:?}")}),
    );
}

fn diagnostic_base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        output.push(ALPHABET[(a >> 2) as usize] as char);
        output.push(ALPHABET[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        output.push(if chunk.len() > 1 {
            ALPHABET[(((b & 15) << 2) | (c >> 6)) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            ALPHABET[(c & 63) as usize] as char
        } else {
            '='
        });
    }
    output
}
fn drain_key_stderr(mut input: impl Read, diagnostics: KeyDiagnostics) {
    let mut prefix = Vec::new();
    let mut drained = 0u64;
    let mut published_prefix = false;
    let mut buffer = [0u8; 4096];
    let failed = loop {
        match input.read(&mut buffer) {
            Ok(0) => break false,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break true,
            Ok(count) => {
                drained = drained.saturating_add(count as u64);
                let keep = count.min(STDERR_PREFIX_BYTES.saturating_sub(prefix.len()));
                prefix.extend_from_slice(&buffer[..keep]);
                if !published_prefix
                    && let Some(end) = prefix.iter().position(|byte| *byte == b'\n')
                {
                    let complete_line = &prefix[..=end];
                    key_diagnostic(
                        &diagnostics,
                        "stderr-prefix",
                        json!({"kind":"owned_key_guard_stderr_prefix",
                        "encoding":"base64","raw_stderr_base64":diagnostic_base64(complete_line),
                        "retained_bytes":complete_line.len(),"drained_bytes":drained,
                        "discarded_bytes":drained.saturating_sub(complete_line.len() as u64),
                        "truncated":drained>complete_line.len() as u64,"read_failed":false}),
                    );
                    published_prefix = true;
                }
            }
        }
    };
    key_diagnostic(
        &diagnostics,
        "stderr",
        json!({"kind":"owned_key_guard_stderr",
        "encoding":"base64","raw_stderr_base64":diagnostic_base64(&prefix),
        "retained_bytes":prefix.len(),"drained_bytes":drained,
        "discarded_bytes":drained.saturating_sub(prefix.len() as u64),
        "truncated":drained>prefix.len() as u64,"read_failed":failed}),
    );
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GuardSnapshot {
    protocol_version: u32,
    kind: String,
    session_epoch: u64,
    sequence: u64,
    uptime_ms: u64,
    pub accessibility: bool,
    pub human_input: bool,
    pub host_alive: bool,
    pub supervisor_alive: bool,
    stop_revision: u64,
    pub human_sequence: u64,
    pub hotkey_sequence: u64,
    pub lease_loss_sequence: u64,
    pub fault_sequence: u64,
    reason: String,
    input_available: bool,
    pub authority_clock_ready: bool,
    pub startup_mouse_buttons_clear: bool,
    #[serde(default)]
    pub keyboard_input_available: bool,
    #[serde(default)]
    pub startup_keyboard_clear: bool,
}

impl GuardSnapshot {
    pub fn revision(&self) -> u64 {
        self.stop_revision
    }
    pub fn reason(&self) -> &str {
        &self.reason
    }
    fn valid_after(&self, previous: Option<&Self>, epoch: u64) -> bool {
        self.protocol_version == 1
            && self.kind == "guard"
            && self.session_epoch == epoch
            && self.sequence > 0
            && !self.input_available
            && (!self.keyboard_input_available || self.startup_keyboard_clear)
            && self.reason.len() <= 256
            && self
                .human_sequence
                .checked_add(self.hotkey_sequence)
                .and_then(|v| v.checked_add(self.lease_loss_sequence))
                .and_then(|v| v.checked_add(self.fault_sequence))
                .is_some_and(|minimum| self.stop_revision >= minimum)
            && previous.is_none_or(|p| {
                self.sequence > p.sequence
                    && self.uptime_ms >= p.uptime_ms
                    && self.stop_revision >= p.stop_revision
                    && self.human_sequence >= p.human_sequence
                    && self.hotkey_sequence >= p.hotkey_sequence
                    && self.lease_loss_sequence >= p.lease_loss_sequence
                    && self.fault_sequence >= p.fault_sequence
                    && self.startup_mouse_buttons_clear == p.startup_mouse_buttons_clear
                    && self.startup_keyboard_clear == p.startup_keyboard_clear
                    && (!self.keyboard_input_available || p.keyboard_input_available)
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingPhase {
    ProbeOnly,
    KeyPreflight,
    AtomicSent,
}

#[derive(Default)]
struct Mailbox {
    latest: Option<(GuardSnapshot, Instant)>,
    failed: bool,
    pending_probe: Option<NativeProbeRequest>,
    probe_response: Option<NativeProbeResponse>,
    pending_submit: Option<NativeProbeRequest>,
    owned_submit_correlation: Option<OwnedSubmitCorrelation>,
    submit_response: Option<NativeSubmitResponse>,
    pending_owned_submit: Option<NativeOwnedValueRequest>,
    owned_submit_response: Option<NativeOwnedValueResponse>,
    phase: Option<PendingPhase>,
    key_preflight_cancelled: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq)]
struct OwnedSubmitCorrelation {
    intent_id: String,
    attempt_id: String,
    task_id: String,
    task_revision: u64,
    root_epoch: u64,
    session_epoch: u64,
    request_id: String,
    window_activation: Option<OwnedWindowActivationDiagnostic>,
}

impl OwnedSubmitCorrelation {
    fn from_action_intent(
        intent: &interaction_contracts::desktop::device::ActionIntent,
        request_id: &str,
        session_epoch: u64,
    ) -> Option<Self> {
        let attempt_id = intent.attempt_id.as_deref()?;
        let ids: [&str; 4] = [&intent.intent_id, attempt_id, &intent.task_id, request_id];
        if ids.iter().any(|value| {
            value.trim().is_empty()
                || value.len() > MAX_ID_LEN
                || value.chars().any(char::is_control)
        }) || intent.task_revision == 0
            || intent.root_epoch == 0
            || session_epoch == 0
            || intent.session_epoch != session_epoch
        {
            return None;
        }
        Some(Self {
            intent_id: intent.intent_id.clone(),
            attempt_id: attempt_id.to_owned(),
            task_id: intent.task_id.clone(),
            task_revision: intent.task_revision,
            root_epoch: intent.root_epoch,
            session_epoch,
            request_id: request_id.to_owned(),
            window_activation: None,
        })
    }

    fn attach_window_activation(&mut self, request: &NativeProbeRequest) -> bool {
        let Some(context) = OwnedWindowActivationDiagnostic::from_request(request) else {
            return false;
        };
        self.window_activation = Some(context);
        true
    }

    fn matches(
        &self,
        request: &NativeProbeRequest,
        response: &NativeSubmitResponse,
        session_epoch: u64,
    ) -> bool {
        self.matches_identity(
            request.action,
            &request.request_id,
            session_epoch,
            &response.request_id,
            response.session_epoch,
        ) && match request.action {
            "activate_window" => self.window_activation.as_ref().is_some_and(|saved| {
                OwnedWindowActivationDiagnostic::from_request(request).as_ref() == Some(saved)
            }),
            "ax_set_selected_true" | "ax_press" | "click" => self.window_activation.is_none(),
            _ => false,
        }
    }

    fn matches_identity(
        &self,
        action: &str,
        request_id: &str,
        request_session_epoch: u64,
        response_request_id: &str,
        response_session_epoch: u64,
    ) -> bool {
        matches!(
            action,
            "ax_set_selected_true" | "ax_press" | "click" | "activate_window"
        ) && self.request_id == request_id
            && self.request_id == response_request_id
            && self.session_epoch == request_session_epoch
            && self.session_epoch == response_session_epoch
    }
}

struct OwnedValueBootstrap<'a> {
    registration: &'a OwnedDocumentRegistration,
    journal: OwnedJournalBootstrap,
    dependency_validity: std::collections::BTreeMap<String, Option<bool>>,
    allow_save: bool,
}

pub struct OwnedValueStartOptions {
    pub dependency_validity: std::collections::BTreeMap<String, Option<bool>>,
    pub allow_save: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeBackgroundAxScope {
    pub pid: u32,
    pub bundle_id: String,
    pub window_id: String,
    pub start_seconds: u64,
    pub start_micros: u64,
}

#[derive(Debug, Clone, Copy)]
struct BackgroundAxDiagnosticCorrelation<'a> {
    request_id: &'a str,
    intent_id: &'a str,
    attempt_id: Option<&'a str>,
    root_id: &'a str,
    task_revision: u64,
    root_epoch: u64,
}

impl<'a> BackgroundAxDiagnosticCorrelation<'a> {
    fn from_request(request: &'a NativeProbeRequest) -> Self {
        Self {
            request_id: &request.request_id,
            intent_id: &request.intent_id,
            attempt_id: request.attempt_id.as_deref(),
            root_id: &request.task_id,
            task_revision: request.task_revision,
            root_epoch: request.root_epoch,
        }
    }
}

fn background_proof_matches_scope(proof: &BindingProof, scope: &NativeBackgroundAxScope) -> bool {
    if !proof.supports_background_ax_press() {
        return false;
    }
    let DeviceContextWitness::BackgroundAxV1 {
        app,
        window,
        source,
        ..
    } = &proof.device_context
    else {
        return false;
    };
    app.bundle_id == scope.bundle_id
        && app.pid == scope.pid
        && window.window_id == scope.window_id
        && source.window.process.pid == scope.pid
        && source.window.process.start_seconds == scope.start_seconds
        && source.window.process.start_micros == scope.start_micros
        && source.window.origin_window_id == scope.window_id
        && proof.fingerprint.app == scope.bundle_id
        && proof.fingerprint.app_pid == scope.pid
        && proof.fingerprint.window_id == scope.window_id
}

fn background_ax_request_matches_scope(
    request: &NativeProbeRequest,
    scope: &NativeBackgroundAxScope,
) -> bool {
    request.action == "ax_press"
        && request.execution_domain
            == Some(interaction_contracts::desktop::domain::ExecutionDomain::BackgroundAx)
        && request.purpose
            == Some(interaction_contracts::desktop::device::ObservationPurpose::BackgroundAxAction)
        && request.coordinate_space
            == Some(interaction_contracts::desktop::device::CoordinateSpace::TargetObject)
        && request.x == 0.0
        && request.y == 0.0
        && request.value.is_none()
        && request.mouse.is_none()
        && request.scroll.is_none()
        && request.key.is_none()
        && background_proof_matches_scope(&request.original_proof, scope)
        && background_proof_matches_scope(&request.current_proof, scope)
        && request.original_proof.same_instance(&request.current_proof)
        && request.context_witness == request.current_proof.device_context
}

pub struct GuardClient {
    child: Child,
    writer: QueuedWriter,
    mailbox: Arc<Mutex<Mailbox>>,
    reader: Option<JoinHandle<()>>,
    stderr_reader: Option<JoinHandle<()>>,
    started: Instant,
    epoch: u64,
    seen_sequence: u64,
    terminal: bool,
    clock: SharedClock,
    next_probe: u64,
    probe_deadline: Option<Instant>,
    submit_deadline: Option<Instant>,
    execution_scope: Option<(u32, String)>,
    execution_window: Option<String>,
    background_ax_scope: Option<NativeBackgroundAxScope>,
    owned_value_mode: bool,
    owned_save_mode: bool,
    diagnostics: KeyDiagnostics,
    #[cfg(test)]
    health_limits: GuardHealthLimits,
}

impl GuardClient {
    pub fn start(
        path: &Path,
        identity: &DeviceIdentity,
        clock: SharedClock,
        marker: i64,
    ) -> Result<Self, String> {
        Self::start_scoped(path, identity, clock, marker, None, None, None)
    }

    /// The trusted launcher fixes one process/bundle and may also pin its
    /// kernel start identity for a configured window. Production observation
    /// sessions never call this entry point or advertise native input.
    #[allow(clippy::too_many_arguments)]
    pub fn start_execution(
        path: &Path,
        identity: &DeviceIdentity,
        clock: SharedClock,
        marker: i64,
        pid: u32,
        bundle: &str,
        window: Option<&str>,
        expected_process: Option<&KernelProcessIdentity>,
    ) -> Result<Self, String> {
        if pid == 0 || pid > i32::MAX as u32 || bundle.is_empty() || bundle.len() > 256 {
            return Err("invalid native execution scope".into());
        }
        if window.is_some_and(|w| {
            w.strip_prefix("window-")
                .and_then(|s| s.parse::<u32>().ok())
                .is_none_or(|n| n == 0)
        }) {
            return Err("invalid native execution window".into());
        }
        if let Some(process) = expected_process {
            process.validate()?;
            if process.pid != pid
                || process.start_seconds > MAX_CANONICAL_PROCESS_START_SECONDS
                || window.is_none()
            {
                return Err(
                    "native process identity requires a matching PID and fixed window scope".into(),
                );
            }
        }
        Self::start_scoped(
            path,
            identity,
            clock,
            marker,
            Some((pid, bundle.into())),
            window.map(str::to_string),
            expected_process.cloned(),
        )
    }

    /// Fixed background AX execution requires the process birth supplied by
    /// Main at consent time. The helper receives a distinct startup marker;
    /// ordinary fixed-window launch arguments can never enable this lane.
    #[allow(clippy::too_many_arguments)]
    pub fn start_background_ax_execution(
        path: &Path,
        identity: &DeviceIdentity,
        clock: SharedClock,
        marker: i64,
        pid: u32,
        bundle: &str,
        window: &str,
        expected_process: &KernelProcessIdentity,
    ) -> Result<Self, String> {
        expected_process.validate()?;
        if pid == 0
            || pid > i32::MAX as u32
            || bundle.is_empty()
            || bundle.len() > 256
            || ![
                "com.lingxi.computer-user.guard-fixture",
                "com.lingxi.computer-user.mvp-fixture",
                "com.apple.TextEdit",
            ]
            .contains(&bundle)
            || window
                .strip_prefix("window-")
                .and_then(|value| value.parse::<u32>().ok())
                .is_none_or(|value| value == 0 || window != format!("window-{value}"))
            || expected_process.pid != pid
            || expected_process.start_seconds > MAX_CANONICAL_PROCESS_START_SECONDS
        {
            return Err("invalid immutable BackgroundAx process/window grant".into());
        }
        let background_scope = NativeBackgroundAxScope {
            pid,
            bundle_id: bundle.into(),
            window_id: window.into(),
            start_seconds: expected_process.start_seconds,
            start_micros: expected_process.start_micros,
        };
        let diagnostics = OwnedKeyDiagnostics::open_background_ax(path, &background_scope);
        Self::start_scoped_with_mode(
            path,
            identity,
            clock,
            marker,
            Some((pid, bundle.into())),
            Some(window.into()),
            Some(expected_process.clone()),
            diagnostics,
            None,
            Some(background_scope),
        )
    }

    /// Dedicated Main-authorized owned-value mode. The helper receives the
    /// bounded registration prefix before normal control messages on stdin.
    pub fn start_owned_execution(
        path: &Path,
        socket_dir: &Path,
        identity: &DeviceIdentity,
        clock: SharedClock,
        marker: i64,
        registration: &OwnedDocumentRegistration,
        options: OwnedValueStartOptions,
    ) -> Result<Self, String> {
        let now_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "owned registration startup time unavailable")?
            .as_millis()
            .try_into()
            .map_err(|_| "owned registration startup time out of range")?;
        registration
            .validate_at(now_unix_ms)
            .map_err(|_| "invalid private owned document registration")?;
        if registration.executable_path
            != "/System/Applications/TextEdit.app/Contents/MacOS/TextEdit"
            || registration.bundle_id != "com.apple.TextEdit"
        {
            return Err("owned value process is outside the private TextEdit scope".into());
        }
        let journal =
            OwnedJournalBootstrap::create(socket_dir, identity.session_epoch, registration)?;
        Self::start_scoped_with_registration(
            path,
            identity,
            clock,
            marker,
            Some((registration.pid, registration.bundle_id.clone())),
            None,
            None,
            None,
            Some(OwnedValueBootstrap {
                registration,
                journal,
                dependency_validity: options.dependency_validity,
                allow_save: options.allow_save,
            }),
        )
    }

    fn start_scoped(
        path: &Path,
        identity: &DeviceIdentity,
        clock: SharedClock,
        marker: i64,
        execution_scope: Option<(u32, String)>,
        execution_window: Option<String>,
        expected_process: Option<KernelProcessIdentity>,
    ) -> Result<Self, String> {
        let diagnostics =
            OwnedKeyDiagnostics::open(path, execution_scope.as_ref(), execution_window.as_deref());
        Self::start_scoped_with_diagnostics(
            path,
            identity,
            clock,
            marker,
            execution_scope,
            execution_window,
            expected_process,
            diagnostics,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn start_scoped_with_diagnostics(
        path: &Path,
        identity: &DeviceIdentity,
        clock: SharedClock,
        marker: i64,
        execution_scope: Option<(u32, String)>,
        execution_window: Option<String>,
        expected_process: Option<KernelProcessIdentity>,
        diagnostics: KeyDiagnostics,
    ) -> Result<Self, String> {
        Self::start_scoped_with_registration(
            path,
            identity,
            clock,
            marker,
            execution_scope,
            execution_window,
            expected_process,
            diagnostics,
            None,
        )
    }

    // The inputs are independently validated process scope, output diagnostics,
    // and optional private owned bootstrap; keeping them explicit avoids a
    // broader startup configuration object shared across unrelated modes.
    #[allow(clippy::too_many_arguments)]
    fn start_scoped_with_registration(
        path: &Path,
        identity: &DeviceIdentity,
        clock: SharedClock,
        marker: i64,
        execution_scope: Option<(u32, String)>,
        execution_window: Option<String>,
        expected_process: Option<KernelProcessIdentity>,
        diagnostics: KeyDiagnostics,
        owned_value: Option<OwnedValueBootstrap<'_>>,
    ) -> Result<Self, String> {
        Self::start_scoped_with_mode(
            path,
            identity,
            clock,
            marker,
            execution_scope,
            execution_window,
            expected_process,
            diagnostics,
            owned_value,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn start_scoped_with_mode(
        path: &Path,
        identity: &DeviceIdentity,
        clock: SharedClock,
        marker: i64,
        execution_scope: Option<(u32, String)>,
        execution_window: Option<String>,
        expected_process: Option<KernelProcessIdentity>,
        diagnostics: KeyDiagnostics,
        owned_value: Option<OwnedValueBootstrap<'_>>,
        background_ax_scope: Option<NativeBackgroundAxScope>,
    ) -> Result<Self, String> {
        if let Some(owned_value) = &owned_value
            && execution_scope.as_ref().is_none_or(|(pid, bundle)| {
                Some(*pid) != Some(owned_value.registration.pid) || bundle != "com.apple.TextEdit"
            })
        {
            return Err("owned helper execution scope does not match private registration".into());
        }
        if owned_value.is_some() && execution_window.is_some() {
            return Err("owned helper does not accept a configured window id".into());
        }
        if let Some(process) = &expected_process {
            process.validate()?;
            if process.start_seconds > MAX_CANONICAL_PROCESS_START_SECONDS
                || execution_scope
                    .as_ref()
                    .is_none_or(|(pid, _)| *pid != process.pid)
                || execution_window.is_none()
                || owned_value.is_some()
            {
                return Err(
                    "native process identity requires an AX process and fixed window scope".into(),
                );
            }
        }
        if let Some(background) = &background_ax_scope
            && (owned_value.is_some()
                || execution_scope.as_ref().is_none_or(|(pid, bundle)| {
                    *pid != background.pid || bundle != &background.bundle_id
                })
                || execution_window.as_deref() != Some(background.window_id.as_str())
                || expected_process.as_ref().is_none_or(|process| {
                    process.pid != background.pid
                        || process.start_seconds != background.start_seconds
                        || process.start_micros != background.start_micros
                }))
        {
            return Err(
                "BackgroundAx startup scope does not match its immutable process birth".into(),
            );
        }
        let epoch = identity.session_epoch;
        let mut command = Command::new(path);
        command.args([
            "--epoch",
            &epoch.to_string(),
            "--marker",
            &marker.to_string(),
        ]);
        if let Some((pid, bundle)) = &execution_scope {
            command.args([
                "--execute-pid",
                &pid.to_string(),
                "--execute-bundle",
                bundle,
            ]);
        }
        if let Some(window) = &execution_window {
            command.args(["--execute-window", window]);
        }
        if let Some(process) = &expected_process {
            command.args([
                "--execute-start-seconds",
                &process.start_seconds.to_string(),
                "--execute-start-micros",
                &process.start_micros.to_string(),
            ]);
        }
        if background_ax_scope.is_some() {
            command.arg("--execute-background-ax");
        }
        if owned_value.is_some() {
            command.arg("--execute-owned-value");
            if owned_value.as_ref().is_some_and(|value| value.allow_save) {
                command.arg("--allow-owned-save");
            }
        }
        let owned_journal_file = owned_value
            .as_ref()
            .map(|owned_value| owned_value.journal.clone_file())
            .transpose()?;
        let stderr = if let Some(journal_file) = owned_journal_file {
            Stdio::from(journal_file)
        } else if diagnostics.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        };
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .spawn()
            .map_err(|e| format!("native guard could not start: {e}"))?;
        if let Some(owned_value) = &owned_value
            && let Err(error) = owned_value.journal.verify_empty_path()
        {
            let _ = child.kill();
            return Err(error);
        }
        if let Some(owned_value) = &owned_value {
            let Ok(payload) = serde_json::to_vec(owned_value.registration) else {
                let _ = child.kill();
                return Err("private owned registration could not be encoded".into());
            };
            let payload = Zeroizing::new(payload);
            if payload.is_empty() || payload.len() > MAX_OWNED_REGISTRATION_BYTES {
                let _ = child.kill();
                return Err("private owned registration exceeds its bound".into());
            }
            let length = (payload.len() as u32).to_be_bytes();
            {
                let startup = child.stdin.as_mut().unwrap();
                if startup.write_all(&length).is_err()
                    || startup.write_all(&payload).is_err()
                    || startup.flush().is_err()
                {
                    let _ = child.kill();
                    return Err("private owned registration could not reach native helper".into());
                }
            }
        }
        let writer = QueuedWriter::new("native-guard-control", child.stdin.take().unwrap(), 16);
        let stdout = child.stdout.take().unwrap();
        let stderr_reader = child.stderr.take().map(|stderr| {
            let diagnostics = diagnostics.clone();
            thread::spawn(move || drain_key_stderr(stderr, diagnostics))
        });
        let mailbox = Arc::new(Mutex::new(Mailbox::default()));
        let incoming = mailbox.clone();
        let reader_diagnostics = diagnostics.clone();
        let reader_background_ax_scope = background_ax_scope.clone();
        let reader = thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut previous = None;
            loop {
                let mut line = Vec::new();
                let read = (&mut reader)
                    .take(RECORD_LIMIT + 1)
                    .read_until(b'\n', &mut line);
                if read.is_err()
                    || line.is_empty()
                    || line.len() as u64 > RECORD_LIMIT
                    || line.last() != Some(&b'\n')
                {
                    key_reader_failure(&reader_diagnostics, &incoming, "framing_read_or_eof");
                    break;
                }
                let Ok(value) = serde_json::from_slice::<serde_json::Value>(&line) else {
                    key_reader_failure(&reader_diagnostics, &incoming, "invalid_json");
                    break;
                };
                if value.get("kind").and_then(|v| v.as_str()) == Some("atomic_submit") {
                    let Ok(response) = serde_json::from_value::<NativeSubmitResponse>(value) else {
                        key_reader_failure(&reader_diagnostics, &incoming, "atomic_shape_decode");
                        break;
                    };
                    let Ok(mut inbox) = incoming.lock() else {
                        break;
                    };
                    if inbox.submit_response.is_some()
                        || inbox.phase != Some(PendingPhase::AtomicSent)
                        || !inbox
                            .pending_submit
                            .as_ref()
                            .is_some_and(|p| response.valid_for(p, epoch))
                    {
                        let phase = inbox.phase;
                        drop(inbox);
                        key_diagnostic_limited(
                            &reader_diagnostics,
                            "reader-failure",
                            json!({"kind":"owned_key_guard_reader_failure",
                            "category":"atomic_phase_identity_or_receipt", "phase":format!("{phase:?}"),
                            "request_id":response.request_id,"outcome":response.outcome,
                            "reason":diagnostic_reason(&response.reason),
                            "submitted":response.submitted,
                            "committed_ms":response.committed_ms,"returned_ms":response.returned_ms}),
                            1024,
                        );
                        break;
                    }
                    let owned_receipt = if reader_diagnostics
                        .as_ref()
                        .is_some_and(|diagnostics| diagnostics.mode == OwnedDiagnosticMode::Row)
                    {
                        match (&inbox.pending_submit, &inbox.owned_submit_correlation) {
                            (Some(request), Some(correlation))
                                if correlation.matches(request, &response, epoch) =>
                            {
                                Some((request.clone(), response.clone(), correlation.clone()))
                            }
                            _ => None,
                        }
                    } else {
                        None
                    };
                    let background_ax_receipt =
                        if reader_diagnostics.as_ref().is_some_and(|diagnostics| {
                            diagnostics.mode == OwnedDiagnosticMode::BackgroundAx
                        }) {
                            inbox
                                .pending_submit
                                .as_ref()
                                .zip(reader_background_ax_scope.as_ref())
                                .filter(|(request, scope)| {
                                    background_ax_request_matches_scope(request, scope)
                                })
                                .map(|(request, _)| (request.clone(), response.clone()))
                        } else {
                            None
                        };
                    key_diagnostic_limited(
                        &reader_diagnostics,
                        "atomic-response",
                        json!({"kind":"owned_key_guard_atomic_response",
                        "request_id":response.request_id,"outcome":response.outcome,
                        "reason":diagnostic_reason(&response.reason),
                        "submitted":response.submitted,"committed_ms":response.committed_ms,"returned_ms":response.returned_ms}),
                        1024,
                    );
                    inbox.submit_response = Some(response);
                    drop(inbox);
                    if let Some((request, response, correlation)) = owned_receipt
                        && let Some(diagnostics) = &reader_diagnostics
                    {
                        match request.action {
                            "ax_set_selected_true" => {
                                diagnostics.record_row_submit_receipt(&response, &correlation);
                            }
                            "ax_press" => {
                                diagnostics.record_ax_press_submit_receipt(&response, &correlation);
                            }
                            "click" => {
                                diagnostics.record_click_submit_receipt(&response, &correlation);
                            }
                            "activate_window" => {
                                diagnostics.record_window_activation_submit_receipt(
                                    &request,
                                    &response,
                                    &correlation,
                                    epoch,
                                );
                            }
                            _ => {}
                        }
                    }
                    if let Some((request, response)) = background_ax_receipt
                        && let (Some(diagnostics), Some(scope)) =
                            (&reader_diagnostics, &reader_background_ax_scope)
                    {
                        diagnostics
                            .record_background_ax_submit_receipt(&request, &response, scope, epoch);
                    }
                    continue;
                }
                if value.get("kind").and_then(|v| v.as_str()) == Some("owned_value_submit") {
                    let Ok(response) = serde_json::from_value::<NativeOwnedValueResponse>(value)
                    else {
                        key_reader_failure(
                            &reader_diagnostics,
                            &incoming,
                            "owned_value_shape_decode",
                        );
                        break;
                    };
                    let Ok(mut inbox) = incoming.lock() else {
                        break;
                    };
                    if inbox.owned_submit_response.is_some()
                        || inbox.phase != Some(PendingPhase::AtomicSent)
                        || !inbox
                            .pending_owned_submit
                            .as_ref()
                            .is_some_and(|request| response.valid_for(request, epoch))
                    {
                        drop(inbox);
                        key_reader_failure(
                            &reader_diagnostics,
                            &incoming,
                            "owned_value_phase_identity_or_receipt",
                        );
                        break;
                    }
                    inbox.owned_submit_response = Some(response);
                    continue;
                }
                if value.get("kind").and_then(|v| v.as_str()) == Some("authority_probe") {
                    let Ok(response) = serde_json::from_value::<NativeProbeResponse>(value) else {
                        key_reader_failure(&reader_diagnostics, &incoming, "probe_shape_decode");
                        break;
                    };
                    let Ok(mut inbox) = incoming.lock() else {
                        break;
                    };
                    if inbox.probe_response.is_some()
                        || !matches!(
                            inbox.phase,
                            Some(PendingPhase::ProbeOnly | PendingPhase::KeyPreflight)
                        )
                        || !inbox
                            .pending_probe
                            .as_ref()
                            .is_some_and(|p| response.valid_for(p, epoch))
                    {
                        let phase = inbox.phase;
                        drop(inbox);
                        key_diagnostic(
                            &reader_diagnostics,
                            "reader-failure",
                            json!({"kind":"owned_key_guard_reader_failure",
                            "category":"probe_phase_identity_or_receipt", "phase":format!("{phase:?}"),
                            "request_id":response.request_id,"outcome":response.outcome,"input_authorized":response.input_authorized}),
                        );
                        break;
                    }
                    key_diagnostic(
                        &reader_diagnostics,
                        "probe-response",
                        json!({"kind":"owned_key_guard_probe_response",
                        "request_id":response.request_id,"outcome":response.outcome,"input_authorized":response.input_authorized}),
                    );
                    let background_ax_probe =
                        if reader_diagnostics.as_ref().is_some_and(|diagnostics| {
                            diagnostics.mode == OwnedDiagnosticMode::BackgroundAx
                        }) {
                            inbox
                                .pending_probe
                                .as_ref()
                                .zip(reader_background_ax_scope.as_ref())
                                .filter(|(request, scope)| {
                                    background_ax_request_matches_scope(request, scope)
                                })
                                .map(|(request, _)| (request.clone(), response.clone()))
                        } else {
                            None
                        };
                    inbox.probe_response = Some(response);
                    drop(inbox);
                    if let Some((request, response)) = background_ax_probe
                        && let (Some(diagnostics), Some(scope)) =
                            (&reader_diagnostics, &reader_background_ax_scope)
                    {
                        diagnostics.record_background_ax_authority_probe(
                            &request, &response, scope, epoch,
                        );
                    }
                    continue;
                }
                let Ok(snapshot) = serde_json::from_value::<GuardSnapshot>(value) else {
                    key_reader_failure(&reader_diagnostics, &incoming, "guard_shape_decode");
                    break;
                };
                if !snapshot.valid_after(previous.as_ref(), epoch) {
                    key_reader_failure(
                        &reader_diagnostics,
                        &incoming,
                        "guard_identity_or_monotonicity",
                    );
                    break;
                }
                previous = Some(snapshot.clone());
                let Ok(mut inbox) = incoming.lock() else {
                    break;
                };
                inbox.latest = Some((snapshot, Instant::now()));
            }
            if let Ok(mut inbox) = incoming.lock() {
                inbox.failed = true;
                inbox.owned_submit_correlation = None;
            }
        });
        let mut client = Self {
            child,
            writer,
            mailbox,
            reader: Some(reader),
            stderr_reader,
            started: Instant::now(),
            epoch,
            seen_sequence: 0,
            terminal: false,
            clock,
            next_probe: 0,
            probe_deadline: None,
            submit_deadline: None,
            execution_scope,
            execution_window,
            background_ax_scope,
            owned_value_mode: owned_value.is_some(),
            owned_save_mode: owned_value.as_ref().is_some_and(|owned| owned.allow_save),
            diagnostics,
            #[cfg(test)]
            health_limits: GuardHealthLimits::PRODUCTION,
        };
        let mut authority = json!({"protocol_version":1,"session_epoch":epoch,"command":"configure_authority",
            "clock_origin_ms":clock.origin_unix_ms(),"clock_domain":identity.clock_domain,"surface":identity.surface});
        if client.owned_value_mode {
            authority["dependencies"] = json!(&identity.dependencies);
            authority["dependency_validity"] = json!(
                &owned_value
                    .as_ref()
                    .expect("owned bootstrap exists")
                    .dependency_validity
            );
        }
        client.command(authority);
        Ok(client)
    }

    fn command(&mut self, command: serde_json::Value) {
        if self.terminal {
            return;
        }
        let mut data = serde_json::to_vec(&command).expect("guard command serializes");
        data.push(b'\n');
        if command
            .get("probe")
            .and_then(|probe| probe.get("action"))
            .and_then(|action| action.as_str())
            == Some("key_press")
            && let Some(diag) = &self.diagnostics
            && diag.record_outbound()
        {
            let category = if command["command"] == "authority_probe" {
                "authority-probe"
            } else {
                "atomic-submit"
            };
            diag.append(category, &data);
        }
        let limit = if command.get("command").and_then(|value| value.as_str())
            == Some("owned_value_submit")
        {
            OWNED_VALUE_REQUEST_LIMIT
        } else {
            RECORD_LIMIT
        };
        if data.len() as u64 > limit || self.writer.write_all(&data).is_err() {
            self.terminal = true;
            self.clear_owned_submit_correlation();
        }
    }
    pub fn renew(&mut self, lane: &str, duration_ms: u64) {
        self.command(
            json!({"protocol_version":1,"session_epoch":self.epoch,"command":"lease",
            "lane":lane,"duration_ms":duration_ms.clamp(1, 1500)}),
        );
    }
    pub fn stop(&mut self) {
        if let Ok(mut inbox) = self.mailbox.lock()
            && inbox.phase == Some(PendingPhase::KeyPreflight)
        {
            inbox.key_preflight_cancelled = Some("stop_latched");
        }
        self.command(json!({"protocol_version":1,"session_epoch":self.epoch,"command":"stop"}));
    }
    pub fn revoke(&mut self, permit: &PermitRef) {
        if let Ok(mut inbox) = self.mailbox.lock()
            && inbox.phase == Some(PendingPhase::KeyPreflight)
            && inbox
                .pending_submit
                .as_ref()
                .is_some_and(|request| &request.permit == permit)
        {
            inbox.key_preflight_cancelled = Some("revoked");
        }
        self.command(json!({"protocol_version":1,"session_epoch":self.epoch,"command":"revoke_authority","permit":permit}));
    }
    pub fn update_dependency(&mut self, name: &str, revision: u64, valid: Option<bool>) {
        if self.owned_value_mode
            && let Some(command) =
                owned_value::dependency_update_command(self.epoch, name, revision, valid)
        {
            self.command(command);
        }
    }
    /// Nonblocking read-only admission. The caller keeps servicing stop and
    /// revocation commands; even an eligible response cannot authorize input.
    pub fn probe(
        &mut self,
        context: &SubmitContext<'_>,
        primitive: &Primitive,
    ) -> Result<String, String> {
        let captured = Instant::now();
        let record = self.latest_checked()?.ok_or("native guard not ready")?;
        if !record.authority_clock_ready
            || !record.startup_mouse_buttons_clear
            || !record.accessibility
            || !record.human_input
            || !record.host_alive
            || !record.supervisor_alive
            || matches!(primitive, Primitive::KeyPress { .. })
                && (!record.keyboard_input_available || !record.startup_keyboard_clear)
        {
            return Err("native authority prerequisites unavailable".into());
        }
        self.next_probe = self
            .next_probe
            .checked_add(1)
            .ok_or("native probe sequence exhausted")?;
        let id = format!("probe-{}", self.next_probe);
        let now = self.clock.now_ms();
        let request = NativeProbeRequest::build(
            id.clone(),
            context,
            primitive,
            record.stop_revision,
            now,
            self.epoch,
        )?;
        {
            let mut inbox = self
                .mailbox
                .lock()
                .map_err(|_| "native guard mailbox unavailable")?;
            if inbox.pending_probe.is_some()
                || inbox.probe_response.is_some()
                || inbox.pending_submit.is_some()
                || inbox.owned_submit_correlation.is_some()
                || inbox.submit_response.is_some()
            {
                return Err("native probe already pending".into());
            }
            inbox.pending_probe = Some(request.clone());
            inbox.phase = Some(PendingPhase::ProbeOnly);
        }
        self.probe_deadline = Some(captured + Duration::from_millis(request.valid_until_ms - now));
        self.command(json!({"protocol_version":1,"session_epoch":self.epoch,"command":"authority_probe","probe":request}));
        if self.terminal {
            return Err("native authority command failed".into());
        }
        Ok(id)
    }
    pub fn poll_probe(&mut self) -> Result<Option<NativeProbeResponse>, String> {
        if self
            .mailbox
            .lock()
            .map_err(|_| "native guard mailbox unavailable")?
            .phase
            != Some(PendingPhase::ProbeOnly)
        {
            return Err("no standalone authority probe pending".into());
        }
        self.latest_checked()?;
        let shared_expired = self
            .mailbox
            .lock()
            .map_err(|_| "native guard mailbox unavailable")?
            .pending_probe
            .as_ref()
            .is_some_and(|p| self.clock.now_ms() >= p.valid_until_ms);
        if shared_expired || self.probe_deadline.is_some_and(|v| Instant::now() >= v) {
            self.terminal = true;
            let _ = self.child.kill();
            return Err("native authority reply expired; guard restart required".into());
        }
        let mut inbox = self
            .mailbox
            .lock()
            .map_err(|_| "native guard mailbox unavailable")?;
        let Some(response) = inbox.probe_response.take() else {
            return Ok(None);
        };
        if response.outcome == "eligible"
            && inbox
                .latest
                .as_ref()
                .zip(inbox.pending_probe.as_ref())
                .is_none_or(|((snapshot, _), request)| {
                    snapshot.stop_revision != request.guard_revision
                        || !snapshot.host_alive
                        || !snapshot.supervisor_alive
                        || !snapshot.accessibility
                        || !snapshot.human_input
                        || !snapshot.authority_clock_ready
                        || !snapshot.startup_mouse_buttons_clear
                        || request.action == "key_press"
                            && (!snapshot.keyboard_input_available
                                || !snapshot.startup_keyboard_clear)
                })
        {
            self.terminal = true;
            let _ = self.child.kill();
            return Err("native authority response raced a guard loss".into());
        }
        inbox.pending_probe = None;
        inbox.phase = None;
        self.probe_deadline = None;
        Ok(Some(response))
    }

    /// Nonblocking enqueue; the caller must continue servicing stop/revoke and
    /// leases. Once enqueued, every transport error means unknown outcome.
    pub fn submit_press(
        &mut self,
        context: &SubmitContext<'_>,
        primitive: &Primitive,
    ) -> Result<String, String> {
        self.enqueue_press(context, primitive, None, false)
    }

    /// Production callers pin the revision already reconciled with the core.
    /// A newly arrived human/stop notice cannot silently refresh old authority.
    pub fn submit_press_at_revision(
        &mut self,
        context: &SubmitContext<'_>,
        primitive: &Primitive,
        expected_revision: u64,
    ) -> Result<String, String> {
        self.enqueue_press(context, primitive, Some(expected_revision), false)
    }

    /// Background AX uses the same bounded receipt/stop lifecycle as native
    /// atomic submissions, but a dedicated proof DTO and startup grant.
    pub fn submit_background_ax_at_revision(
        &mut self,
        context: &SubmitContext<'_>,
        primitive: &Primitive,
        expected_revision: u64,
    ) -> Result<String, String> {
        self.enqueue_press(context, primitive, Some(expected_revision), true)
    }

    /// Dedicated one-shot owned text submission. It never constructs a
    /// generic NativeProbe or enters the point-hit/AX fingerprint pipeline.
    pub fn submit_owned_value_at_revision(
        &mut self,
        context: &SubmitContext<'_>,
        primitive: &Primitive,
        expected_revision: u64,
        registration: &OwnedDocumentRegistration,
    ) -> Result<String, String> {
        if !self.owned_value_mode
            || self.execution_scope.as_ref().is_none_or(|(pid, bundle)| {
                *pid != registration.pid || bundle != "com.apple.TextEdit"
            })
        {
            return Err("native helper has no separate owned value startup authorization".into());
        }
        let captured = Instant::now();
        let record = self.latest_checked()?.ok_or("native guard not ready")?;
        if expected_revision != record.revision() {
            return Err("native guard changed since core reconciliation".into());
        }
        if !record.accessibility
            || !record.human_input
            || !record.startup_mouse_buttons_clear
            || !record.authority_clock_ready
            || !record.host_alive
            || !record.supervisor_alive
        {
            return Err("native owned value guards unavailable".into());
        }
        let next = self
            .next_probe
            .checked_add(1)
            .ok_or("native request sequence exhausted")?;
        let now_ms = self.clock.now_ms();
        let request = NativeOwnedValueRequest::build(
            format!("owned-submit-{next}"),
            context,
            primitive,
            record.revision(),
            now_ms,
            self.epoch,
            OwnedValueBuildOptions {
                registration,
                allow_save: self.owned_save_mode,
            },
        )?;
        if !owned_value::request_wire_within_bounds(&request, self.epoch) {
            return Err("owned value request exceeds its complete wire bounds".into());
        }
        let remaining_ms = request
            .valid_until_ms
            .checked_sub(now_ms)
            .ok_or("owned value authority expired before enqueue")?;
        {
            let mut inbox = self
                .mailbox
                .lock()
                .map_err(|_| "native guard mailbox unavailable")?;
            if inbox.pending_probe.is_some()
                || inbox.probe_response.is_some()
                || inbox.pending_submit.is_some()
                || inbox.owned_submit_correlation.is_some()
                || inbox.submit_response.is_some()
                || inbox.pending_owned_submit.is_some()
                || inbox.owned_submit_response.is_some()
            {
                return Err("native worker already pending".into());
            }
            inbox.pending_owned_submit = Some(request.clone());
            inbox.phase = Some(PendingPhase::AtomicSent);
        }
        self.next_probe = next;
        self.submit_deadline = Some(captured + Duration::from_millis(remaining_ms + 500));
        self.command(json!({
            "protocol_version":1,
            "session_epoch":self.epoch,
            "command":"owned_value_submit",
            "owned_value":request,
        }));
        Ok(format!("owned-submit-{next}"))
    }

    fn enqueue_press(
        &mut self,
        context: &SubmitContext<'_>,
        primitive: &Primitive,
        expected_revision: Option<u64>,
        background_ax: bool,
    ) -> Result<String, String> {
        if self.background_ax_scope.is_some() != background_ax {
            return Err(
                "native helper startup mode does not authorize this submission lane".into(),
            );
        }
        if background_ax {
            let scope = self
                .background_ax_scope
                .as_ref()
                .ok_or("native helper has no BackgroundAx startup grant")?;
            let original = context
                .intent
                .binding_proof
                .as_ref()
                .ok_or("missing original BackgroundAx proof")?;
            let current = context
                .authority
                .evidence
                .binding_proof
                .as_ref()
                .ok_or("missing current BackgroundAx proof")?;
            if context.intent.execution_domain
                != interaction_contracts::desktop::domain::ExecutionDomain::BackgroundAx
                || context.authority.evidence.execution_domain
                    != interaction_contracts::desktop::domain::ExecutionDomain::BackgroundAx
                || context.authority.evidence.purpose
                    != interaction_contracts::desktop::device::ObservationPurpose::BackgroundAxAction
                || context.intent.coordinate_space
                    != interaction_contracts::desktop::device::CoordinateSpace::TargetObject
                || !matches!(
                    primitive,
                    Primitive::AxAction {
                        action: interaction_contracts::desktop::primitive::AxActionKind::Press
                    }
                )
                || !background_proof_matches_scope(original, scope)
                || !background_proof_matches_scope(current, scope)
            {
                return Err("BackgroundAx proof or action differs from its immutable startup grant".into());
            }
        } else if context.intent.execution_domain
            != interaction_contracts::desktop::domain::ExecutionDomain::ForegroundNative
            || context.authority.evidence.execution_domain
                != interaction_contracts::desktop::domain::ExecutionDomain::ForegroundNative
        {
            return Err("foreground native helper rejects cross-domain submission".into());
        }
        let captured = Instant::now();
        let record = self.latest_checked()?.ok_or("native guard not ready")?;
        if expected_revision.is_some_and(|revision| revision != record.stop_revision) {
            return Err("native guard changed since core reconciliation".into());
        }
        let fp = &context
            .intent
            .binding_proof
            .as_ref()
            .ok_or("missing native proof")?
            .fingerprint;
        if self
            .execution_scope
            .as_ref()
            .is_none_or(|(pid, bundle)| *pid != fp.app_pid || bundle != &fp.app)
            || self
                .execution_window
                .as_ref()
                .is_some_and(|window| window != &fp.window_id)
            || context.authority.evidence.purpose
                == interaction_contracts::desktop::device::ObservationPurpose::MenuSelection
                && self.execution_window.is_none()
            || matches!(primitive, Primitive::KeyPress { .. })
                && (self.execution_window.is_none()
                    || !record.keyboard_input_available
                    || !record.startup_keyboard_clear)
            || !record.authority_clock_ready
            || !record.startup_mouse_buttons_clear
            || !record.accessibility
            || !record.human_input
            || !record.host_alive
            || !record.supervisor_alive
        {
            return Err("native execution scope or guards unavailable".into());
        }
        self.next_probe = self
            .next_probe
            .checked_add(1)
            .ok_or("native request sequence exhausted")?;
        let id = format!("submit-{}", self.next_probe);
        let now = self.clock.now_ms();
        let request = NativeProbeRequest::build(
            id.clone(),
            context,
            primitive,
            record.stop_revision,
            now,
            self.epoch,
        )?;
        let submit_correlation = if matches!(
            request.action,
            "ax_set_selected_true" | "ax_press" | "click" | "activate_window"
        ) && self
            .diagnostics
            .as_ref()
            .is_some_and(|diagnostics| diagnostics.mode == OwnedDiagnosticMode::Row)
        {
            OwnedSubmitCorrelation::from_action_intent(
                context.intent,
                &request.request_id,
                self.epoch,
            )
            .and_then(|mut correlation| {
                if request.action == "activate_window"
                    && !correlation.attach_window_activation(&request)
                {
                    return None;
                }
                Some(correlation)
            })
        } else {
            None
        };
        if ![
            "ax_press",
            "ax_menu_press",
            "ax_set_value",
            "ax_set_selected_true",
            "ax_show_menu",
            "activate_window",
            "click",
            "scroll",
            "pointer_move",
            "key_press",
        ]
        .contains(&request.action)
        {
            return Err("native atomic submission requires a registered atomic primitive".into());
        }
        let command_name = if request.action == "key_press" {
            "authority_probe"
        } else {
            "atomic_submit"
        };
        let command = bounded_native_probe_command(&request, self.epoch, command_name)?;
        {
            let mut inbox = self
                .mailbox
                .lock()
                .map_err(|_| "native guard mailbox unavailable")?;
            if inbox.pending_probe.is_some()
                || inbox.probe_response.is_some()
                || inbox.pending_submit.is_some()
                || inbox.owned_submit_correlation.is_some()
                || inbox.submit_response.is_some()
                || inbox.pending_owned_submit.is_some()
                || inbox.owned_submit_response.is_some()
            {
                return Err("native worker already pending".into());
            }
            inbox.pending_submit = Some(request.clone());
            inbox.owned_submit_correlation = submit_correlation;
            if request.action == "key_press" {
                inbox.pending_probe = Some(request.clone());
                inbox.phase = Some(PendingPhase::KeyPreflight);
            } else {
                inbox.phase = Some(PendingPhase::AtomicSent);
            }
        }
        // A started AX call may return after authority expiration. Its receipt
        // is still authoritative; never relabel it as a pre-submit rejection.
        self.submit_deadline =
            Some(captured + Duration::from_millis(request.valid_until_ms - now + 500));
        if request.action == "key_press" {
            self.probe_deadline =
                Some(captured + Duration::from_millis(request.valid_until_ms - now));
        }
        self.command(command);
        // Command failure could race a successful pipe write; leave the pending
        // submission queryable and report unknown through poll_submission.
        Ok(id)
    }

    pub fn poll_submission(&mut self) -> Result<Option<NativeSubmitResponse>, String> {
        let phase = self
            .mailbox
            .lock()
            .map_err(|_| "native submission outcome unknown")?
            .phase;
        if phase == Some(PendingPhase::KeyPreflight) {
            return self.poll_key_preflight();
        }
        {
            let mut inbox = self
                .mailbox
                .lock()
                .map_err(|_| "native submission outcome unknown")?;
            if let Some(response) = inbox.owned_submit_response.take() {
                inbox.pending_owned_submit = None;
                inbox.owned_submit_correlation = None;
                inbox.phase = None;
                self.submit_deadline = None;
                return Ok(Some(NativeSubmitResponse {
                    protocol_version: response.protocol_version,
                    kind: "atomic_submit".into(),
                    session_epoch: response.session_epoch,
                    request_id: response.request_id,
                    outcome: response.outcome,
                    reason: response.reason,
                    submitted: response.submitted,
                    committed_ms: response.committed_ms,
                    returned_ms: response.returned_ms,
                    binding_digest: None,
                    row_trace: None,
                    ax_trace: None,
                    background_ax_trace: None,
                }));
            }
            if let Some(response) = inbox.submit_response.take() {
                inbox.pending_submit = None;
                inbox.owned_submit_correlation = None;
                inbox.phase = None;
                self.submit_deadline = None;
                return Ok(Some(response));
            }
        }
        self.latest_checked()
            .map_err(|_| "native submission outcome unknown; do not replay")?;
        if self.submit_deadline.is_some_and(|v| Instant::now() >= v) {
            self.terminal = true;
            let _ = self.child.kill();
            self.clear_owned_submit_correlation();
            return Err(
                "native submission outcome unknown; reply deadline exceeded; do not replay".into(),
            );
        }
        Ok(None)
    }

    /// A failed preflight has never queued atomic input. Preserve that fact
    /// even when the shell drops a lost guard before polling the backend.
    pub(crate) fn reject_key_preflight(&mut self, reason: &str) -> Option<NativeSubmitResponse> {
        self.finish_key_preflight("context_unresolved", reason)
    }

    fn finish_key_preflight(
        &mut self,
        outcome: &str,
        reason: &str,
    ) -> Option<NativeSubmitResponse> {
        let request = {
            let mut inbox = self.mailbox.lock().ok()?;
            if inbox.phase != Some(PendingPhase::KeyPreflight) {
                return None;
            }
            let request = inbox.pending_submit.take()?;
            inbox.owned_submit_correlation = None;
            inbox.pending_probe = None;
            inbox.probe_response = None;
            inbox.key_preflight_cancelled = None;
            inbox.phase = None;
            request
        };
        self.probe_deadline = None;
        self.submit_deadline = None;
        self.terminate();
        Some(NativeSubmitResponse {
            protocol_version: 1,
            kind: "atomic_submit".into(),
            session_epoch: self.epoch,
            request_id: request.request_id,
            outcome: outcome.into(),
            reason: reason.chars().take(256).collect(),
            submitted: false,
            committed_ms: None,
            returned_ms: None,
            binding_digest: None,
            row_trace: None,
            ax_trace: None,
            background_ax_trace: None,
        })
    }

    fn poll_key_preflight(&mut self) -> Result<Option<NativeSubmitResponse>, String> {
        let (request, cancelled) = {
            let inbox = self
                .mailbox
                .lock()
                .map_err(|_| "native guard mailbox unavailable")?;
            (
                inbox
                    .pending_submit
                    .clone()
                    .ok_or("key preflight request missing")?,
                inbox.key_preflight_cancelled,
            )
        };
        if let Some(outcome) = cancelled {
            return Ok(self.finish_key_preflight(
                outcome,
                "key preflight cancelled before atomic submission",
            ));
        }
        if self.clock.now_ms() >= request.valid_until_ms
            || self.probe_deadline.is_some_and(|end| Instant::now() >= end)
        {
            return Ok(self.finish_key_preflight(
                "expired",
                "key preflight exceeded original authority deadline",
            ));
        }
        let snapshot = match self.latest_checked() {
            Ok(Some(snapshot)) => snapshot,
            _ => {
                return Ok(self.finish_key_preflight(
                    "context_unresolved",
                    "key preflight lost native guard before atomic submission",
                ));
            }
        };
        if snapshot.stop_revision != request.guard_revision
            || !snapshot.host_alive
            || !snapshot.supervisor_alive
            || !snapshot.accessibility
            || !snapshot.human_input
            || !snapshot.authority_clock_ready
            || !snapshot.startup_mouse_buttons_clear
            || !snapshot.startup_keyboard_clear
            || !snapshot.keyboard_input_available
        {
            return Ok(self.finish_key_preflight(
                "context_unresolved",
                "key preflight guard identity or prerequisites changed",
            ));
        }
        let response = self
            .mailbox
            .lock()
            .map_err(|_| "native guard mailbox unavailable")?
            .probe_response
            .take();
        let Some(response) = response else {
            return Ok(None);
        };
        if response.outcome != "eligible" {
            return Ok(self.finish_key_preflight(&response.outcome, &response.reason));
        }
        {
            let mut inbox = self
                .mailbox
                .lock()
                .map_err(|_| "native guard mailbox unavailable")?;
            if inbox.failed
                || inbox.phase != Some(PendingPhase::KeyPreflight)
                || inbox.key_preflight_cancelled.is_some()
                || self.clock.now_ms() >= request.valid_until_ms
                || inbox.latest.as_ref().is_none_or(|(latest, at)| {
                    latest.stop_revision != request.guard_revision
                        || at.elapsed() >= FRESHNESS
                        || !latest.host_alive
                        || !latest.supervisor_alive
                        || !latest.accessibility
                        || !latest.human_input
                        || !latest.authority_clock_ready
                        || !latest.startup_keyboard_clear
                        || !latest.keyboard_input_available
                })
            {
                drop(inbox);
                return Ok(self.finish_key_preflight(
                    "context_unresolved",
                    "key preflight changed before atomic promotion",
                ));
            }
            inbox.pending_probe = None;
            inbox.phase = Some(PendingPhase::AtomicSent);
        }
        self.probe_deadline = None;
        // Do not build a fresh request or renew either deadline. The native
        // worker consumes its one-shot CF witness for this exact packet.
        self.command(json!({"protocol_version":1,"session_epoch":self.epoch,"command":"atomic_submit","probe":request}));
        Ok(None)
    }
    fn latest_checked(&mut self) -> Result<Option<GuardSnapshot>, String> {
        let state = self
            .mailbox
            .lock()
            .map(|v| (v.latest.clone(), v.failed))
            .ok();
        let latest_snapshot_age = state
            .as_ref()
            .and_then(|(record, _)| record.as_ref().map(|(_, at)| at.elapsed()));
        let failed = self.terminal
            || self.writer.failed()
            || state.as_ref().is_none_or(|(_, failed)| *failed)
            || guard_health_expired(
                self.started.elapsed(),
                latest_snapshot_age,
                self.health_limits(),
            );
        if failed {
            let phase = self.mailbox.lock().ok().and_then(|inbox| inbox.phase);
            let status = self.child.try_wait();
            let (category, code, signal) = match status {
                Ok(Some(status)) => (
                    "helper_exited_before_guard_kill",
                    status.code(),
                    status.signal(),
                ),
                Ok(None) => ("helper_running_before_guard_kill", None, None),
                Err(_) => ("helper_status_unavailable_before_guard_kill", None, None),
            };
            key_diagnostic(
                &self.diagnostics,
                "helper-exit",
                json!({"kind":"owned_key_guard_helper_exit",
                "category":category,"phase":format!("{phase:?}"),"exit_code":code,"signal":signal,
                "shared_now_ms":self.clock.now_ms()}),
            );
            self.terminal = true;
            let _ = self.child.kill();
            self.clear_owned_submit_correlation();
            return Err("native guard unavailable, stale or invalid; restart required".into());
        }
        let Some((Some((snapshot, _)), _)) = state else {
            return Ok(None);
        };
        Ok(Some(snapshot))
    }
    fn health_limits(&self) -> GuardHealthLimits {
        #[cfg(test)]
        {
            self.health_limits
        }
        #[cfg(not(test))]
        {
            GuardHealthLimits::PRODUCTION
        }
    }
    pub fn poll(&mut self) -> Result<Option<GuardSnapshot>, String> {
        let Some(snapshot) = self.latest_checked()? else {
            return Ok(None);
        };
        if snapshot.sequence == self.seen_sequence {
            return Ok(None);
        }
        self.seen_sequence = snapshot.sequence;
        Ok(Some(snapshot))
    }
    pub fn pid(&self) -> u32 {
        self.child.id()
    }
    pub fn execution_target(&self) -> Option<(u32, &str)> {
        self.execution_scope
            .as_ref()
            .map(|(pid, bundle)| (*pid, bundle.as_str()))
    }
    pub fn execution_window(&self) -> Option<&str> {
        self.execution_window.as_deref()
    }
    pub fn is_background_ax_execution(&self) -> bool {
        self.background_ax_scope.is_some()
    }
    pub fn background_ax_scope(&self) -> Option<NativeBackgroundAxScope> {
        self.background_ax_scope.clone()
    }
    pub fn background_ax_readiness_diagnostics_enabled(&self) -> bool {
        self.background_ax_scope.is_some()
            && self
                .diagnostics
                .as_ref()
                .is_some_and(|diagnostics| diagnostics.mode == OwnedDiagnosticMode::BackgroundAx)
    }
    pub fn record_background_ax_readiness(
        &self,
        sample_index: usize,
        snapshot: &GuardSnapshot,
        backend_native_ready: bool,
        core_isolated: bool,
    ) -> bool {
        if self.background_ax_scope.is_none() || snapshot.session_epoch != self.epoch {
            return false;
        }
        self.diagnostics.as_ref().is_some_and(|diagnostics| {
            diagnostics.record_background_ax_readiness(
                sample_index,
                snapshot,
                backend_native_ready,
                core_isolated,
            )
        })
    }
    pub fn is_owned_value_execution(&self) -> bool {
        self.owned_value_mode
    }
    pub fn is_owned_save_execution(&self) -> bool {
        self.owned_save_mode
    }
    fn clear_owned_submit_correlation(&self) {
        if let Ok(mut inbox) = self.mailbox.lock() {
            inbox.owned_submit_correlation = None;
        }
    }
    pub fn terminate(&mut self) {
        self.terminal = true;
        let _ = self.child.kill();
        self.clear_owned_submit_correlation();
    }
}

impl Drop for GuardClient {
    fn drop(&mut self) {
        // Killing our child interrupts both a blocked stdin writer and stdout
        // reader. Never join either worker while the child can still block it.
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.writer.finish(Duration::from_millis(100));
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        if let Some(reader) = self.stderr_reader.take() {
            let _ = reader.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(super) fn snapshot() -> GuardSnapshot {
        serde_json::from_value(json!({"protocol_version":1,"kind":"guard","session_epoch":7,"sequence":1,
            "uptime_ms":100,"accessibility":true,"human_input":true,"host_alive":true,"supervisor_alive":true,
            "stop_revision":0,"human_sequence":0,"hotkey_sequence":0,"lease_loss_sequence":0,"fault_sequence":0,
            "reason":"native input unavailable","input_available":false,"authority_clock_ready":false,"startup_mouse_buttons_clear":true})).unwrap()
    }
    #[test]
    fn a_native_guard_can_never_advertise_execution() {
        let mut s = snapshot();
        assert!(s.valid_after(None, 7));
        assert!(!s.valid_after(None, 8));
        s.input_available = true;
        assert!(!s.valid_after(None, 7));
    }
    #[test]
    fn an_unclear_startup_mouse_state_cannot_be_cleared_by_later_snapshots() {
        let mut old = snapshot();
        old.startup_mouse_buttons_clear = false;
        assert!(old.valid_after(None, 7));
        let mut next = old.clone();
        next.sequence += 1;
        assert!(next.valid_after(Some(&old), 7));
        next.startup_mouse_buttons_clear = true;
        assert!(!next.valid_after(Some(&old), 7));
    }
    #[test]
    fn keyboard_capability_defaults_closed_and_cannot_grow_after_startup() {
        let old = snapshot();
        assert!(!old.keyboard_input_available);
        assert!(!old.startup_keyboard_clear);
        let mut next = old.clone();
        next.sequence += 1;
        next.keyboard_input_available = true;
        assert!(
            !next.valid_after(None, 7),
            "an unclear keyboard startup cannot advertise key input"
        );
        next.startup_keyboard_clear = true;
        assert!(
            !next.valid_after(Some(&old), 7),
            "neither startup evidence nor capability can be upgraded in this generation"
        );
        assert!(next.valid_after(None, 7));
        let capable = next.clone();
        next.sequence += 1;
        next.keyboard_input_available = false;
        assert!(
            next.valid_after(Some(&capable), 7),
            "capability loss is allowed"
        );
        let lost = next.clone();
        next.sequence += 1;
        next.keyboard_input_available = true;
        assert!(
            !next.valid_after(Some(&lost), 7),
            "capability recovery needs a new guard generation"
        );
    }
    #[test]
    fn coalescing_preserves_monotonic_safety_counters() {
        let old = snapshot();
        let mut s = old.clone();
        assert!(!s.valid_after(Some(&old), 7));
        s.sequence = 100;
        s.human_sequence = 20;
        s.lease_loss_sequence = 2;
        s.stop_revision = 22;
        assert!(s.valid_after(Some(&old), 7));
        let next = s.clone();
        s.sequence += 1;
        s.lease_loss_sequence = 1;
        assert!(!s.valid_after(Some(&next), 7));
    }
    #[test]
    fn stale_time_unknown_fields_and_oversized_reasons_fail_closed() {
        let old = snapshot();
        let mut s = old.clone();
        s.sequence += 1;
        s.uptime_ms -= 1;
        assert!(!s.valid_after(Some(&old), 7));
        s.uptime_ms += 1;
        s.reason = "x".repeat(257);
        assert!(!s.valid_after(Some(&old), 7));
        let mut value = serde_json::to_value(snapshot()).unwrap();
        value["unexpected"] = json!(1);
        assert!(serde_json::from_value::<GuardSnapshot>(value).is_err());
    }

    #[test]
    fn production_guard_health_limits_expire_at_the_startup_and_heartbeat_boundaries() {
        let production = GuardHealthLimits::PRODUCTION;
        assert_eq!(production.startup, Duration::from_millis(1500));
        assert_eq!(production.freshness, Duration::from_millis(750));
        assert!(!guard_health_expired(
            Duration::from_millis(1499),
            None,
            production,
        ));
        assert!(guard_health_expired(
            Duration::from_millis(1500),
            None,
            production,
        ));
        assert!(!guard_health_expired(
            Duration::from_secs(3),
            Some(Duration::from_millis(749)),
            production,
        ));
        assert!(guard_health_expired(
            Duration::from_millis(1),
            Some(Duration::from_millis(750)),
            production,
        ));

        #[cfg(test)]
        {
            assert_eq!(GuardHealthLimits::MOCK_IPC.startup, Duration::from_secs(10));
            assert_eq!(
                GuardHealthLimits::MOCK_IPC.freshness,
                Duration::from_secs(5)
            );
        }
    }

    #[test]
    fn oversized_native_command_is_rejected_before_it_can_be_queued() {
        let valid = bounded_guard_command(json!({"command":"atomic_submit","probe":{}}));
        assert!(valid.is_ok());
        let oversized = bounded_guard_command(json!({
            "command":"atomic_submit",
            "probe":{"padding":"x".repeat(RECORD_LIMIT as usize)}
        }));
        assert!(oversized.is_err());
    }
}

#[cfg(test)]
mod key_preflight_ipc_tests {
    use super::*;
    use crate::backend::native::NativeAxBackend;
    use execution_core::{
        backend::{SubmissionAuthority, SubmitOutcome},
        core::{CoreConfig, ExecutionCore},
        sim_backend::SimBackend,
    };
    use interaction_contracts::desktop::{
        binding::{
            BindingContinuityProof, BindingProof, BindingProofScope, DeviceContextApp,
            DeviceContextComposition, DeviceContextFingerprint, DeviceContextFocusRelation,
            DeviceContextObject, DeviceContextTarget, DeviceContextWindow, DeviceContextWitness,
            FocusedControlScope, FocusedKeyProfile, InstanceFingerprint,
            LayoutIndependentSpecialKey,
        },
        device::{
            ActionIntent, CoordinateSpace, Observation, ObservationPurpose, ScheduledPrimitive,
            SourceFreshness,
        },
        ids::{BindingRef, ObjectRef, RequestId},
        primitive::Modifiers,
        source::{
            CompositionState, FocusContext, FocusRelation, KernelProcessIdentity,
            ObjectIdentityContinuity, Rect, SourceKeyboardContext, SourceWindowContext,
        },
        task::{RowContainerRole, RowContainerScope, RowTableScope},
    };
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    // These tests exercise guard IPC behavior, not product liveness. Serialize
    // mock workers and use the test-only health budget set by `wait_ready`.
    static WORKER_LANE: Mutex<()> = Mutex::new(());

    struct FakeWorker {
        dir: PathBuf,
        script: PathBuf,
        log: PathBuf,
        _lane: std::sync::MutexGuard<'static, ()>,
    }
    impl FakeWorker {
        fn new(mode: &str) -> Self {
            let lane = WORKER_LANE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let dir = std::env::temp_dir().join(format!(
                "cu-key-preflight-{}",
                desktop_ipc::socket::new_token().unwrap()
            ));
            desktop_ipc::socket::private_dir(&dir).unwrap();
            let script = dir.join("worker.py");
            let log = dir.join("commands.jsonl");
            let source = format!(
                "#!/usr/bin/env python3\nMODE={}\nLOG={}\n",
                serde_json::to_string(mode).unwrap(),
                serde_json::to_string(&log).unwrap()
            );
            let body = r#"import json,sys,time,threading
args=sys.argv[1:]
if MODE=='argv_capture':
 with open(LOG,'a') as f: f.write(json.dumps({'startup_argv':args})+'\n')
epoch=int(args[args.index('--epoch')+1])
origin=0
saved=None
seq=0
revision_now=0
output_lock=threading.RLock()

def write(value):
 with output_lock: print(json.dumps(value),flush=True)

def snapshot(revision=None,authority_ready=True):
 global seq,revision_now
 with output_lock:
  if revision is not None: revision_now=revision
  seq+=1
  write(dict(protocol_version=1,kind='guard',session_epoch=epoch,sequence=seq,uptime_ms=seq,
 accessibility=True,human_input=True,host_alive=True,supervisor_alive=True,
 stop_revision=revision_now,human_sequence=revision_now,hotkey_sequence=0,lease_loss_sequence=0,fault_sequence=0,
 reason='fake worker IPC only',input_available=(MODE=='background_ready'),authority_clock_ready=authority_ready,
 startup_mouse_buttons_clear=True,startup_keyboard_clear=True,keyboard_input_available=True))

def heartbeat():
 while True:
  time.sleep(.05)
  snapshot()

# Publish an unready heartbeat immediately so the parent reader can initialize
# before the writer thread finishes delivering configure_authority.
snapshot(authority_ready=False)

def probe(p,eligible=True):
 write(dict(protocol_version=1,kind='authority_probe',session_epoch=epoch,request_id=p['request_id'],
 outcome='eligible' if eligible else 'context_unresolved',reason='fake bounded preflight',
 binding_digest=p['binding_digest'] if eligible else None,input_authorized=False))

def applied(p):
 at=max(p['started_ms'],int(time.time()*1000)-origin)
 write(dict(protocol_version=1,kind='atomic_submit',session_epoch=epoch,request_id=p['request_id'],
 outcome='applied',reason='fake receipt; no OS input',submitted=True,
 committed_ms=at,returned_ms=at,binding_digest=p['binding_digest']))

def unknown(p):
 at=max(p['started_ms'],int(time.time()*1000)-origin)
 write(dict(protocol_version=1,kind='atomic_submit',session_epoch=epoch,request_id=p['request_id'],
 outcome='unknown',reason='fake unknown; do not replay',submitted=True,
 committed_ms=at,returned_ms=None,binding_digest=p['binding_digest']))

def rejected(p):
 write(dict(protocol_version=1,kind='atomic_submit',session_epoch=epoch,request_id=p['request_id'],
 outcome='context_unresolved',reason='fake preflight rejection',submitted=False,
 committed_ms=None,returned_ms=None,binding_digest=None))

for line in sys.stdin:
 c=json.loads(line)
 command=c['command']
 if command=='configure_authority':
  origin=c['clock_origin_ms']
  snapshot(authority_ready=True)
  threading.Thread(target=heartbeat,daemon=True).start()
 elif command in ('authority_probe','atomic_submit'):
  with open(LOG,'a') as f: f.write(json.dumps(c)+'\n')
  p=c['probe']
  if command=='authority_probe':
   saved=p
   if MODE=='exit_probe': sys.exit(0)
   if MODE=='hold_probe': continue
   if MODE=='wrong_atomic': applied(p); continue
   if MODE=='cross_id':
    p=dict(p,request_id='cross-request')
   if MODE=='guard_change': snapshot(1)
   probe(p,MODE!='reject')
  else:
   if p.get('action')=='key_press' and p!=saved: sys.exit(2)
   if MODE=='exit_atomic': sys.exit(0)
   if MODE=='late_probe': probe(p); continue
   if MODE=='window_unknown': unknown(p)
   elif MODE=='window_rejected': rejected(p)
   else: applied(p)
"#;
            std::fs::write(&script, source + body).unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self {
                dir,
                script,
                log,
                _lane: lane,
            }
        }
        fn commands(&self) -> Vec<serde_json::Value> {
            std::fs::read_to_string(&self.log)
                .unwrap_or_default()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect()
        }
    }
    impl Drop for FakeWorker {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn wait_diagnostics(diagnostics: &OwnedKeyDiagnostics, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while diagnostics.completed.load(Ordering::Acquire) < count {
            assert!(
                Instant::now() < deadline,
                "diagnostic writer did not finish"
            );
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn diagnostic_object_keys(value: &serde_json::Value) -> Vec<String> {
        let mut keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        keys
    }

    #[test]
    fn owned_row_diagnostics_require_the_row_binary_mvp_app_and_window_scope() {
        let row_binary = Path::new("/private/owned-row-mvp-fixture");
        let row_scope = (42, "com.lingxi.computer-user.mvp-fixture".to_string());
        assert_eq!(
            OwnedDiagnosticMode::for_scope(row_binary, Some(&row_scope), Some("window-17")),
            Some(OwnedDiagnosticMode::Row)
        );
        assert_eq!(
            OwnedDiagnosticMode::for_scope(
                Path::new("owned-key-mvp-fixture"),
                Some(&row_scope),
                Some("window-17")
            ),
            None
        );
        assert_eq!(
            OwnedDiagnosticMode::for_scope(row_binary, Some(&row_scope), None),
            None
        );
        assert_eq!(
            OwnedDiagnosticMode::for_scope(row_binary, None, Some("window-17")),
            None
        );
        assert_eq!(
            OwnedDiagnosticMode::for_scope(
                row_binary,
                Some(&(42, "com.lingxi.computer-user.guard-fixture".to_string())),
                Some("window-17")
            ),
            None
        );
        assert_eq!(
            OwnedDiagnosticMode::for_scope(
                row_binary,
                Some(&(0, "com.lingxi.computer-user.mvp-fixture".to_string())),
                Some("window-17")
            ),
            None
        );
        assert_eq!(
            OwnedDiagnosticMode::for_scope(
                Path::new("owned-row-textedit"),
                Some(&(42, "com.apple.TextEdit".to_string())),
                Some("window-17")
            ),
            None
        );
        assert_eq!(
            OwnedDiagnosticMode::for_scope(
                Path::new("device-macos-guard"),
                Some(&row_scope),
                Some("window-17")
            ),
            None
        );
        assert_eq!(
            OwnedDiagnosticMode::for_scope(
                Path::new("owned-row-"),
                Some(&row_scope),
                Some("window-17")
            ),
            None
        );

        assert!(OwnedDiagnosticMode::Row.allows_category("row-submit-receipt"));
        assert!(OwnedDiagnosticMode::Row.allows_category("ax-press-submit-receipt"));
        assert!(OwnedDiagnosticMode::Row.allows_category("click-submit-receipt"));
        assert!(OwnedDiagnosticMode::Row.allows_category("window-activation-submit-receipt"));
        assert!(!OwnedDiagnosticMode::Row.allows_category("atomic-response"));
        assert!(!OwnedDiagnosticMode::Row.allows_category("atomic-submit"));
        assert!(!OwnedDiagnosticMode::Row.allows_category("stderr"));
        assert!(OwnedDiagnosticMode::Key.allows_category("atomic-response"));
    }

    #[test]
    fn background_ax_diagnostics_require_owned_binary_fixture_window_and_birth_scope() {
        let scope = NativeBackgroundAxScope {
            pid: 42,
            bundle_id: "com.lingxi.computer-user.guard-fixture".into(),
            window_id: "window-17".into(),
            start_seconds: 1_700_000_000,
            start_micros: 123_456,
        };
        assert_eq!(
            OwnedDiagnosticMode::for_background_scope(
                Path::new("/private/helpers/owned-background-acceptance"),
                &scope,
            ),
            Some(OwnedDiagnosticMode::BackgroundAx)
        );
        assert_eq!(
            OwnedDiagnosticMode::for_scope(
                Path::new("owned-background-acceptance"),
                Some(&(scope.pid, scope.bundle_id.clone())),
                Some(scope.window_id.as_str()),
            ),
            None,
            "foreground startup must not open BackgroundAx diagnostics by path name alone"
        );
        for path in [
            "owned-key-background",
            "owned-row-background",
            "owned-background-",
            "owned-background-bad$name",
            "device-macos-guard",
        ] {
            assert_eq!(
                OwnedDiagnosticMode::for_background_scope(Path::new(path), &scope),
                None,
                "unexpected diagnostic mode for {path}"
            );
        }
        let mut invalid = scope.clone();
        invalid.bundle_id = "com.lingxi.computer-user.mvp-fixture".into();
        assert_eq!(
            OwnedDiagnosticMode::for_background_scope(
                Path::new("owned-background-acceptance"),
                &invalid
            ),
            None
        );
        let mut invalid = scope.clone();
        invalid.window_id = "window-017".into();
        assert_eq!(
            OwnedDiagnosticMode::for_background_scope(
                Path::new("owned-background-acceptance"),
                &invalid
            ),
            None
        );
        let mut invalid = scope.clone();
        invalid.start_micros = 1_000_000;
        assert_eq!(
            OwnedDiagnosticMode::for_background_scope(
                Path::new("owned-background-acceptance"),
                &invalid
            ),
            None
        );
        let mut invalid = scope.clone();
        invalid.pid = 0;
        assert_eq!(
            OwnedDiagnosticMode::for_background_scope(
                Path::new("owned-background-acceptance"),
                &invalid
            ),
            None
        );

        assert!(OwnedDiagnosticMode::BackgroundAx.allows_category("readiness"));
        assert!(OwnedDiagnosticMode::BackgroundAx.allows_category("background-authority-probe"));
        assert!(OwnedDiagnosticMode::BackgroundAx.allows_category("ax-press-submit-receipt"));
        for category in [
            "atomic-response",
            "atomic-submit",
            "reader-failure",
            "stderr",
            "row-submit-receipt",
            "click-submit-receipt",
            "window-activation-submit-receipt",
        ] {
            assert!(
                !OwnedDiagnosticMode::BackgroundAx.allows_category(category),
                "background diagnostic mode must reject {category}"
            );
        }
        assert!(OwnedDiagnosticMode::Row.allows_category("ax-press-submit-receipt"));
        assert!(OwnedDiagnosticMode::Key.allows_category("atomic-response"));
    }

    #[test]
    fn background_ax_diagnostics_record_only_probe_result_and_native_ax_call_trace() {
        use crate::guard::submission::{
            NativeAxTrace, NativeAxTraceClockDomain, NativeAxTracePrimitive,
            NativeBackgroundAxTrace, NativeBackgroundAxTraceStage,
        };

        let probe = NativeProbeResponse {
            protocol_version: 1,
            kind: "authority_probe".into(),
            session_epoch: 9,
            request_id: "probe-1".into(),
            outcome: "eligible".into(),
            reason: "read-only authority is eligible".into(),
            binding_digest: Some("private-digest".into()),
            input_authorized: false,
        };
        let correlation = BackgroundAxDiagnosticCorrelation {
            request_id: "probe-1",
            intent_id: "intent-1",
            attempt_id: Some("attempt-1"),
            root_id: "root-1",
            task_revision: 3,
            root_epoch: 4,
        };
        let mut readiness_snapshot = super::tests::snapshot();
        readiness_snapshot.session_epoch = 9;
        readiness_snapshot.sequence = 42;
        readiness_snapshot.human_input = false;
        readiness_snapshot.supervisor_alive = false;
        let readiness = background_ax_readiness_value(2, &readiness_snapshot, false, true);
        assert_eq!(readiness["category"], "readiness");
        assert_eq!(readiness["session_epoch"], 9);
        assert_eq!(readiness["sample_index"], 2);
        assert_eq!(readiness["guard_sequence"], 42);
        assert_eq!(readiness["human_input"], false);
        assert_eq!(readiness["supervisor_alive"], false);
        assert_eq!(readiness["backend_native_ready"], false);
        assert_eq!(readiness["core_isolated"], true);
        assert_eq!(readiness["ready"], false);
        for key in [
            "reason",
            "binding_digest",
            "source",
            "context_witness",
            "root_id",
            "window_id",
        ] {
            assert!(!readiness.as_object().unwrap().contains_key(key));
        }

        let probe_value = background_ax_authority_probe_value(&probe, correlation);
        assert_eq!(probe_value["category"], "background-authority-probe");
        assert_eq!(probe_value["outcome"], "eligible");
        assert_eq!(probe_value["input_authorized"], false);
        assert_eq!(probe_value["intent_id"], "intent-1");
        assert_eq!(probe_value["attempt_id"], "attempt-1");
        assert_eq!(probe_value["root_id"], "root-1");
        assert_eq!(probe_value["task_revision"], 3);
        assert_eq!(probe_value["root_epoch"], 4);
        for key in ["reason", "binding_digest", "source", "context_witness"] {
            assert!(!probe_value.as_object().unwrap().contains_key(key));
        }

        let response = NativeSubmitResponse {
            protocol_version: 1,
            kind: "atomic_submit".into(),
            session_epoch: 9,
            request_id: "submit-1".into(),
            outcome: "applied".into(),
            reason: "atomic input call returned; effect unverified".into(),
            submitted: true,
            committed_ms: Some(120),
            returned_ms: Some(121),
            binding_digest: Some("private-digest".into()),
            row_trace: None,
            ax_trace: Some(NativeAxTrace {
                primitive: NativeAxTracePrimitive::AxPress,
                ax_call_count: 1,
                ax_error_code: Some(0),
                clock_domain: NativeAxTraceClockDomain::DispatchTimeUptime,
                call_started_uptime_ns: Some(10_000),
                call_finished_uptime_ns: Some(10_025),
            }),
            background_ax_trace: Some(NativeBackgroundAxTrace {
                stage: NativeBackgroundAxTraceStage::Submitted,
                process_identity_matches: true,
                application_boundary_matches: true,
                window_boundary_matches: true,
                target_traversal_complete: true,
                target_matches: true,
                initial_stability_passed: true,
                final_revalidation_passed: true,
                commit_ready: true,
                submitted: true,
            }),
        };
        let receipt = background_ax_submit_receipt_value(&response, correlation);
        assert_eq!(receipt["category"], "ax-press-submit-receipt");
        assert_eq!(receipt["outcome"], "applied");
        assert_eq!(receipt["submitted"], true);
        assert_eq!(receipt["ax_trace"]["primitive"], "ax_press");
        assert_eq!(receipt["ax_trace"]["ax_call_count"], 1);
        assert_eq!(receipt["ax_trace"]["ax_error_code"], 0);
        assert_eq!(receipt["background_ax_trace"]["stage"], "submitted");
        assert_eq!(receipt["background_ax_trace"]["commit_ready"], true);
        assert_eq!(receipt["background_ax_trace"]["submitted"], true);
        assert_eq!(receipt["intent_id"], "intent-1");
        assert_eq!(receipt["attempt_id"], "attempt-1");
        assert_eq!(receipt["root_id"], "root-1");
        assert_eq!(receipt["task_revision"], 3);
        assert_eq!(receipt["root_epoch"], 4);
        for key in [
            "reason",
            "binding_digest",
            "source",
            "context_witness",
            "root",
            "target",
            "label",
            "ancestry",
        ] {
            assert!(!receipt.as_object().unwrap().contains_key(key));
        }
        assert_eq!(
            receipt["background_ax_trace"].as_object().unwrap().len(),
            10,
            "owned projection keeps only the fixed enum and boolean fields"
        );
    }

    #[test]
    fn owned_row_receipts_are_complete_bounded_and_exclude_other_diagnostic_records() {
        use crate::guard::submission::{
            NativeRowTrace, NativeRowTraceClockDomain, NativeRowTracePrimitive,
        };

        let worker = FakeWorker::new("hold_probe");
        let directory = worker.dir.join("row-diagnostics");
        desktop_ipc::socket::private_dir(&directory).unwrap();
        let diagnostics = OwnedKeyDiagnostics::in_row_directory(directory.clone()).unwrap();
        assert!(!diagnostics.record_outbound());

        let response = NativeSubmitResponse {
            protocol_version: 1,
            kind: "atomic_submit".into(),
            session_epoch: 9,
            request_id: "row-request-1".into(),
            outcome: "applied".into(),
            reason: "atomic input call returned; effect unverified".into(),
            submitted: true,
            committed_ms: Some(120),
            returned_ms: Some(121),
            binding_digest: Some("fp1:stable-digest".into()),
            row_trace: Some(NativeRowTrace {
                primitive: NativeRowTracePrimitive::AxSetSelectedTrue,
                ax_call_count: 1,
                ax_error_code: Some(0),
                clock_domain: NativeRowTraceClockDomain::DispatchTimeUptime,
                call_started_uptime_ns: Some(10_000),
                call_finished_uptime_ns: Some(10_025),
            }),
            ax_trace: None,
            background_ax_trace: None,
        };
        let correlation = OwnedSubmitCorrelation {
            intent_id: "intent-1".into(),
            attempt_id: "attempt-1".into(),
            task_id: "task-1".into(),
            task_revision: 2,
            root_epoch: 3,
            session_epoch: 9,
            request_id: response.request_id.clone(),
            window_activation: None,
        };
        let value = row_submit_receipt_value(&response, &correlation);
        assert_eq!(value["category"], "row-submit-receipt");
        assert_eq!(value["session_epoch"], 9);
        assert_eq!(value["request_id"], "row-request-1");
        assert_eq!(value["row_trace"]["ax_call_count"], 1);
        assert_eq!(value["row_trace"]["ax_error_code"], 0);
        assert_eq!(value["row_trace"]["clock_domain"], "dispatch_time_uptime");
        assert_eq!(value["correlation"]["intent_id"], "intent-1");
        assert_eq!(value["correlation"]["attempt_id"], "attempt-1");
        assert_eq!(value["correlation"]["task_id"], "task-1");
        assert_eq!(value["correlation"]["task_revision"], 2);
        assert_eq!(value["correlation"]["root_epoch"], 3);
        assert_eq!(value["correlation"]["session_epoch"], 9);
        assert_eq!(value["correlation"]["request_id"], "row-request-1");
        assert!(!value.as_object().unwrap().contains_key("probe"));
        assert!(!value.as_object().unwrap().contains_key("command"));
        let unknown = NativeSubmitResponse {
            outcome: "unknown".into(),
            reason: "native return clock untrusted; do not replay".into(),
            ..response.clone()
        };
        assert_eq!(
            row_submit_receipt_value(&unknown, &correlation)["outcome"],
            "unknown"
        );

        assert!(!diagnostics.append("atomic-submit", b"outbound request"));
        assert!(diagnostics.record_row_submit_receipt(&response, &correlation));
        wait_diagnostics(&diagnostics, 1);
        let receipt_path = directory.join("00-row-submit-receipt.json");
        let saved: serde_json::Value =
            serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
        assert_eq!(saved, value);
        assert_eq!(
            fs::metadata(receipt_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);

        let no_call = NativeSubmitResponse {
            submitted: false,
            outcome: "stop_latched".into(),
            committed_ms: None,
            returned_ms: None,
            binding_digest: None,
            row_trace: None,
            ..response
        };
        assert!(row_submit_receipt_value(&no_call, &correlation)["row_trace"].is_null());
    }

    #[test]
    fn owned_submit_correlation_is_bounded_and_rejects_mismatched_or_stale_identity() {
        let (identity, intent, _, _) = fixture(SharedClock::starting_now(), 1000);
        let correlation = OwnedSubmitCorrelation::from_action_intent(
            &intent,
            "submit-17",
            identity.session_epoch,
        )
        .unwrap();
        assert_eq!(correlation.intent_id, "key-one");
        assert_eq!(correlation.attempt_id, "attempt");
        assert_eq!(correlation.task_id, "root");
        assert_eq!(correlation.task_revision, 1);
        assert_eq!(correlation.root_epoch, 1);
        assert_eq!(correlation.session_epoch, identity.session_epoch);
        assert_eq!(correlation.request_id, "submit-17");

        assert!(correlation.matches_identity(
            "ax_press",
            "submit-17",
            identity.session_epoch,
            "submit-17",
            identity.session_epoch
        ));

        assert!(correlation.matches_identity(
            "ax_set_selected_true",
            "submit-17",
            identity.session_epoch,
            "submit-17",
            identity.session_epoch
        ));
        assert!(!correlation.matches_identity(
            "ax_set_selected_true",
            "submit-16",
            identity.session_epoch,
            "submit-17",
            identity.session_epoch
        ));
        assert!(!correlation.matches_identity(
            "ax_set_selected_true",
            "submit-17",
            identity.session_epoch,
            "submit-16",
            identity.session_epoch
        ));
        assert!(!correlation.matches_identity(
            "ax_set_selected_true",
            "submit-17",
            identity.session_epoch.saturating_sub(1),
            "submit-17",
            identity.session_epoch
        ));
        assert!(!correlation.matches_identity(
            "scroll",
            "submit-17",
            identity.session_epoch,
            "submit-17",
            identity.session_epoch
        ));

        let mut oversized = intent.clone();
        oversized.intent_id = "i".repeat(MAX_ID_LEN + 1);
        assert!(
            OwnedSubmitCorrelation::from_action_intent(
                &oversized,
                "submit-18",
                identity.session_epoch
            )
            .is_none()
        );
        let mut no_attempt = intent.clone();
        no_attempt.attempt_id = None;
        assert!(
            OwnedSubmitCorrelation::from_action_intent(
                &no_attempt,
                "submit-19",
                identity.session_epoch
            )
            .is_none()
        );
    }

    #[test]
    fn window_activation_receipts_capture_only_matching_mocked_replies_and_keep_outcomes() {
        let clock = SharedClock::starting_now();
        let (identity, intent, _, _) = fixture(clock, 1000);
        let (request, correlation) = window_activation_test_request(&identity, &intent);
        assert_eq!(request.action, "activate_window");
        let source_context = correlation.window_activation.as_ref().unwrap();
        assert_eq!(
            source_context.table_scope.column_headers[0],
            "Archive Items"
        );
        assert_eq!(source_context.original.verified_source_sequence, 1);
        assert_eq!(source_context.original.verified_observed_ms, 90);
        assert_eq!(source_context.current.verified_source_sequence, 2);
        assert_eq!(source_context.current.verified_observed_ms, 100);

        let mut crossed_response = NativeSubmitResponse {
            protocol_version: 1,
            kind: "atomic_submit".into(),
            session_epoch: identity.session_epoch,
            request_id: "another-request".into(),
            outcome: "applied".into(),
            reason: "fake transport response".into(),
            submitted: true,
            committed_ms: Some(120),
            returned_ms: Some(121),
            binding_digest: Some(request.binding_digest.clone()),
            row_trace: None,
            ax_trace: None,
            background_ax_trace: None,
        };
        assert!(!crossed_response.valid_for(&request, identity.session_epoch));
        assert!(!correlation.matches(&request, &crossed_response, identity.session_epoch));
        crossed_response.request_id = request.request_id.clone();
        let mut changed_scope = request.clone();
        let BindingProofScope::WindowRoot { table_scope } =
            &mut changed_scope.current_proof.continuity.scope
        else {
            unreachable!()
        };
        table_scope.column_headers[0] = "Different Header".into();
        assert!(!correlation.matches(&changed_scope, &crossed_response, identity.session_epoch));

        for (mode, expected_outcome, expected_submitted) in [
            ("hold_probe", "applied", true),
            ("window_unknown", "unknown", true),
            ("window_rejected", "context_unresolved", false),
        ] {
            let worker = FakeWorker::new(mode);
            let directory = worker.dir.join("window-activation-diagnostics");
            desktop_ipc::socket::private_dir(&directory).unwrap();
            let diagnostics = OwnedKeyDiagnostics::in_row_directory(directory.clone()).unwrap();
            let (mut guard, _) =
                start_with_diagnostics(&worker, clock, &identity, Some(diagnostics.clone()));
            {
                let mut mailbox = guard.mailbox.lock().unwrap();
                mailbox.pending_submit = Some(request.clone());
                mailbox.owned_submit_correlation = Some(correlation.clone());
                mailbox.phase = Some(PendingPhase::AtomicSent);
            }
            guard.submit_deadline = Some(Instant::now() + Duration::from_secs(2));
            guard.command(
                bounded_native_probe_command(&request, identity.session_epoch, "atomic_submit")
                    .unwrap(),
            );
            let response = await_reply(&mut guard).unwrap();
            assert_eq!(response.outcome, expected_outcome);
            assert_eq!(response.submitted, expected_submitted);
            wait_diagnostics(&diagnostics, 1);

            let record_path = directory.join("00-window-activation-submit-receipt.json");
            let saved: serde_json::Value =
                serde_json::from_slice(&fs::read(record_path).unwrap()).unwrap();
            assert_eq!(saved["kind"], "owned_row_guard_diagnostic");
            assert_eq!(saved["category"], "window-activation-submit-receipt");
            assert_eq!(saved["protocol_version"], 1);
            assert_eq!(saved["response_kind"], "atomic_submit");
            assert_eq!(saved["outcome"], expected_outcome);
            assert_eq!(saved["submitted"], expected_submitted);
            assert_eq!(saved["request_id"], request.request_id);
            assert_eq!(saved["correlation"]["intent_id"], request.intent_id);
            assert_eq!(
                saved["window_activation"]["table_scope"]["column_headers"][0],
                "Archive Items"
            );
            assert_eq!(
                saved["window_activation"]["stable_root"]["root"]["role"],
                "AXWindow"
            );
            assert_eq!(
                saved["window_activation"]["source_reads"]["original"]["verified_source_sequence"],
                1
            );
            assert_eq!(
                saved["window_activation"]["source_reads"]["original"]["verified_observed_ms"],
                90
            );
            assert_eq!(
                saved["window_activation"]["source_reads"]["current"]["verified_source_sequence"],
                2
            );
            assert_eq!(
                saved["window_activation"]["source_reads"]["current"]["verified_observed_ms"],
                100
            );
            assert_eq!(
                saved["window_activation"]["source_reads"]["current"]["context_digest"],
                saved["window_activation"]["source_reads"]["original"]["context_digest"]
            );
            assert_eq!(
                saved["window_activation"]["stable_root"]["source_identity"]["process"]["pid"],
                42
            );
            assert_eq!(
                diagnostic_object_keys(&saved),
                [
                    "action",
                    "binding_digest",
                    "category",
                    "committed_ms",
                    "correlation",
                    "kind",
                    "outcome",
                    "protocol_version",
                    "reason",
                    "request_id",
                    "response_kind",
                    "returned_ms",
                    "session_epoch",
                    "submitted",
                    "window_activation",
                ]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
            );
            assert_eq!(
                diagnostic_object_keys(&saved["correlation"]),
                [
                    "attempt_id",
                    "intent_id",
                    "request_id",
                    "root_epoch",
                    "session_epoch",
                    "task_id",
                    "task_revision",
                ]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
            );
            assert_eq!(
                diagnostic_object_keys(&saved["window_activation"]),
                ["source_reads", "stable_root", "table_scope"]
                    .into_iter()
                    .map(String::from)
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                diagnostic_object_keys(&saved["window_activation"]["stable_root"]),
                ["app", "root", "source_identity", "window"]
                    .into_iter()
                    .map(String::from)
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                diagnostic_object_keys(&saved["window_activation"]["source_reads"]["current"]),
                [
                    "context_digest",
                    "continuity",
                    "verified_observed_ms",
                    "verified_source_sequence",
                ]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
            );
            assert!(!saved.as_object().unwrap().contains_key("ax_call_count"));
            assert!(!saved.as_object().unwrap().contains_key("ax_trace"));
            assert!(!saved.as_object().unwrap().contains_key("row_trace"));
            assert!(!saved.as_object().unwrap().contains_key("probe"));
            assert!(!saved.as_object().unwrap().contains_key("command"));
            assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        }
    }

    #[test]
    fn owned_ax_press_receipts_preserve_unknown_error_without_outbound_data() {
        use crate::guard::submission::{
            NativeAxTrace, NativeAxTraceClockDomain, NativeAxTracePrimitive,
        };
        let worker = FakeWorker::new("hold_probe");
        let directory = worker.dir.join("row-press-diagnostics");
        desktop_ipc::socket::private_dir(&directory).unwrap();
        let diagnostics = OwnedKeyDiagnostics::in_row_directory(directory.clone()).unwrap();
        let response = NativeSubmitResponse {
            protocol_version: 1,
            kind: "atomic_submit".into(),
            session_epoch: 9,
            request_id: "press-request-1".into(),
            outcome: "unknown".into(),
            reason: "atomic input result unknown; do not replay".into(),
            submitted: true,
            committed_ms: Some(120),
            returned_ms: Some(121),
            binding_digest: Some("fp1:press-digest".into()),
            row_trace: None,
            ax_trace: Some(NativeAxTrace {
                primitive: NativeAxTracePrimitive::AxPress,
                ax_call_count: 1,
                ax_error_code: Some(-25200),
                clock_domain: NativeAxTraceClockDomain::DispatchTimeUptime,
                call_started_uptime_ns: Some(10_000),
                call_finished_uptime_ns: Some(10_025),
            }),
            background_ax_trace: None,
        };
        let correlation = OwnedSubmitCorrelation {
            intent_id: "intent-1".into(),
            attempt_id: "attempt-1".into(),
            task_id: "task-1".into(),
            task_revision: 2,
            root_epoch: 3,
            session_epoch: 9,
            request_id: response.request_id.clone(),
            window_activation: None,
        };
        let value = ax_press_submit_receipt_value(&response, &correlation);
        assert_eq!(value["category"], "ax-press-submit-receipt");
        assert_eq!(value["outcome"], "unknown");
        assert_eq!(value["ax_trace"]["ax_call_count"], 1);
        assert_eq!(value["ax_trace"]["ax_error_code"], -25200);
        assert_eq!(value["correlation"]["intent_id"], "intent-1");
        assert!(!value.as_object().unwrap().contains_key("row_trace"));
        assert!(!value.as_object().unwrap().contains_key("probe"));
        assert!(!value.as_object().unwrap().contains_key("command"));
        assert!(diagnostics.record_ax_press_submit_receipt(&response, &correlation));
        wait_diagnostics(&diagnostics, 1);
        let saved = fs::read(directory.join("00-ax-press-submit-receipt.json")).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&saved).unwrap(),
            value
        );
        let missing = NativeSubmitResponse {
            ax_trace: None,
            ..response.clone()
        };
        assert!(!diagnostics.record_ax_press_submit_receipt(&missing, &correlation));
        let click = NativeSubmitResponse {
            outcome: "applied".into(),
            ax_trace: None,
            ..response
        };
        let click_value = click_submit_receipt_value(&click, &correlation);
        assert_eq!(click_value["primitive"], "click");
        assert!(!click_value.as_object().unwrap().contains_key("row_trace"));
        assert!(!click_value.as_object().unwrap().contains_key("ax_trace"));
        assert!(diagnostics.record_click_submit_receipt(&click, &correlation));
        wait_diagnostics(&diagnostics, 2);
        let saved = fs::read(directory.join("01-click-submit-receipt.json")).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&saved).unwrap(),
            click_value
        );
        assert_eq!(fs::read_dir(directory).unwrap().count(), 2);
    }

    #[test]
    fn owned_diagnostics_are_bounded_and_refuse_symlink_files_without_authority_effects() {
        let worker = FakeWorker::new("hold_probe");
        let diagnostics = OwnedKeyDiagnostics::in_directory(worker.dir.clone()).unwrap();
        let sink = &diagnostics;
        let victim = worker.dir.join("victim");
        fs::write(&victim, b"unchanged").unwrap();
        std::os::unix::fs::symlink(&victim, worker.dir.join("00-packet.json")).unwrap();
        sink.append("packet", b"{}");
        assert_eq!(fs::read(&victim).unwrap(), b"unchanged");
        for _ in 0..20 {
            sink.append("metadata", b"{}");
        }
        wait_diagnostics(&diagnostics, DIAGNOSTIC_RECORDS);
        let files: Vec<_> = fs::read_dir(&worker.dir)
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .ends_with("-metadata.json")
            })
            .collect();
        assert_eq!(files.len(), DIAGNOSTIC_RECORDS - 1);
        assert!(files.iter().all(
            |entry| fs::metadata(entry.path()).unwrap().permissions().mode() & 0o777 == 0o600
        ));
        assert_eq!(
            sink.budget.load(Ordering::Relaxed) >> 16,
            DIAGNOSTIC_RECORDS
        );
        let budget = worker.dir.join("budget");
        desktop_ipc::socket::private_dir(&budget).unwrap();
        let bounded = OwnedKeyDiagnostics::in_directory(budget.clone()).unwrap();

        bounded.append("packet", &vec![b'x'; DIAGNOSTIC_BYTES]);
        bounded.append("metadata", b"{}");
        assert_eq!(
            bounded.budget.load(Ordering::Relaxed) & 0xffff,
            DIAGNOSTIC_BYTES
        );
        assert_eq!(bounded.budget.load(Ordering::Relaxed) >> 16, 1);
        wait_diagnostics(&bounded, 1);
        assert_eq!(fs::read_dir(budget).unwrap().count(), 1);
        assert!(
            OwnedKeyDiagnostics::open(
                Path::new("cu-macos-device-guard"),
                Some(&(42, "com.lingxi.computer-user.guard-fixture".into())),
                Some("window-1")
            )
            .is_none()
        );
        assert!(
            OwnedKeyDiagnostics::open(Path::new("owned-key-test"), None, Some("window-1"))
                .is_none()
        );
    }

    #[test]
    fn diagnostic_writer_refuses_pending_symlinks_and_changed_directory_identity() {
        let worker = FakeWorker::new("hold_probe");
        let directory = worker.dir.join("diagnostics");
        desktop_ipc::socket::private_dir(&directory).unwrap();
        let diagnostics = OwnedKeyDiagnostics::in_directory(directory.clone()).unwrap();
        let victim = worker.dir.join("victim");
        fs::write(&victim, b"unchanged").unwrap();
        std::os::unix::fs::symlink(&victim, directory.join(".00-packet.pending")).unwrap();
        assert!(diagnostics.append("packet", b"{}"));
        wait_diagnostics(&diagnostics, 1);
        assert_eq!(fs::read(&victim).unwrap(), b"unchanged");
        assert!(!directory.join("00-packet.json").exists());
        let moved = worker.dir.join("original");
        fs::rename(&directory, &moved).unwrap();
        desktop_ipc::socket::private_dir(&directory).unwrap();
        assert!(diagnostics.append("metadata", b"{}"));
        wait_diagnostics(&diagnostics, 2);
        assert!(!directory.join("01-metadata.json").exists());
        assert!(!moved.join("01-metadata.json").exists());
    }

    #[test]
    fn owned_stderr_publishes_a_complete_line_before_eof_and_keeps_draining() {
        struct AcknowledgedReader {
            stream: std::os::unix::net::UnixStream,
            reads: mpsc::Sender<usize>,
        }
        impl Read for AcknowledgedReader {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                let count = self.stream.read(buffer)?;
                if count > 0 {
                    let _ = self.reads.send(count);
                }
                Ok(count)
            }
        }

        let worker = FakeWorker::new("hold_probe");
        let diagnostics = OwnedKeyDiagnostics::in_directory(worker.dir.clone()).unwrap();
        let (reader, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let (read_sender, read_acks) = mpsc::channel();
        let drain_diagnostics = diagnostics.clone();
        let drainer = thread::spawn(move || {
            drain_key_stderr(
                AcknowledgedReader {
                    stream: reader,
                    reads: read_sender,
                },
                Some(drain_diagnostics),
            )
        });
        let read_ack = || {
            read_acks
                .recv_timeout(Duration::from_secs(2))
                .expect("stderr reader did not consume the written bytes")
        };

        let partial = b"{\"outcome\":\"applied\"";
        writer.write_all(partial).unwrap();
        assert_eq!(read_ack(), partial.len());
        assert!(!worker.dir.join("00-stderr-prefix.json").exists());
        assert_eq!(diagnostics.budget.load(Ordering::Relaxed) >> 16, 0);

        let complete_line = b"{\"outcome\":\"applied\"}\n";
        writer.write_all(b"}\n").unwrap();
        assert_eq!(read_ack(), 2);
        wait_diagnostics(&diagnostics, 1);
        let early: serde_json::Value =
            serde_json::from_slice(&fs::read(worker.dir.join("00-stderr-prefix.json")).unwrap())
                .unwrap();
        assert_eq!(early["raw_stderr_base64"], diagnostic_base64(complete_line));
        assert_eq!(early["retained_bytes"], complete_line.len());
        assert!(
            !drainer.is_finished(),
            "stderr must be published before EOF"
        );

        let tail = vec![b'x'; STDERR_PREFIX_BYTES * 2];
        writer.write_all(&tail).unwrap();
        let mut tail_read = 0;
        while tail_read < tail.len() {
            tail_read += read_ack();
        }
        writer
            .shutdown(std::net::Shutdown::Write)
            .expect("close stderr input");
        drainer.join().unwrap();

        wait_diagnostics(&diagnostics, 2);
        let final_record: serde_json::Value =
            serde_json::from_slice(&fs::read(worker.dir.join("01-stderr.json")).unwrap()).unwrap();
        let total = complete_line.len() + tail.len();
        assert_eq!(final_record["retained_bytes"], STDERR_PREFIX_BYTES);
        assert_eq!(final_record["drained_bytes"], total);
        assert_eq!(final_record["discarded_bytes"], total - STDERR_PREFIX_BYTES);
        assert_eq!(final_record["truncated"], true);
        assert_eq!(diagnostics.budget.load(Ordering::Relaxed) >> 16, 2);
    }

    #[test]
    fn owned_stderr_keeps_a_lossless_bounded_prefix_and_drains_every_byte() {
        let worker = FakeWorker::new("hold_probe");
        let diagnostics = OwnedKeyDiagnostics::in_directory(worker.dir.clone());
        let raw = vec![0xff; STDERR_PREFIX_BYTES * 9];
        let mut input = std::io::Cursor::new(raw);
        drain_key_stderr(&mut input, diagnostics.clone());
        wait_diagnostics(diagnostics.as_ref().unwrap(), 1);
        assert_eq!(input.position(), input.get_ref().len() as u64);
        let saved: serde_json::Value =
            serde_json::from_slice(&fs::read(worker.dir.join("00-stderr.json")).unwrap()).unwrap();
        assert_eq!(saved["retained_bytes"], STDERR_PREFIX_BYTES);
        assert_eq!(saved["drained_bytes"], input.get_ref().len());
        assert_eq!(saved["discarded_bytes"], STDERR_PREFIX_BYTES * 8);
        assert_eq!(saved["truncated"], true);
        assert_eq!(saved["read_failed"], false);
        assert_eq!(
            saved["raw_stderr_base64"].as_str().unwrap().len(),
            STDERR_PREFIX_BYTES.div_ceil(3) * 4
        );
        assert_eq!(diagnostic_base64(b"f"), "Zg==");
        assert_eq!(diagnostic_base64(b"fo"), "Zm8=");
        assert_eq!(diagnostic_base64(b"foo"), "Zm9v");
        assert_eq!(diagnostic_base64(&[255]), "/w==");
    }

    fn fixture(
        clock: SharedClock,
        duration: u64,
    ) -> (DeviceIdentity, ActionIntent, SubmissionAuthority, Primitive) {
        let mut config = CoreConfig::production("macos");
        config.clock_domain_kind = "wall".into();
        let core = ExecutionCore::new(config, SimBackend::new());
        let identity = core.identity();
        let now = clock.now_ms();
        let frame = Rect {
            x: 0.,
            y: 0.,
            width: 100.,
            height: 100.,
        };
        let fp = InstanceFingerprint {
            app: "com.lingxi.computer-user.guard-fixture".into(),
            app_pid: 42,
            window_id: "window-1".into(),
            ancestor_roles: vec!["AXWindow".into()],
            group: None,
            role: "AXCheckBox".into(),
            subrole: None,
            label: "key fixture toggle".into(),
            sibling_index: 0,
            frame: [0, 0, 100, 100],
            scroll_container: None,
        };
        let instance = ObjectRef::new("actual-unit-receiver").unwrap();
        let context_target = DeviceContextObject {
            role: fp.role.clone(),
            subrole: fp.subrole.clone(),
            label: fp.label.clone(),
            frame,
        };
        let proof = BindingProof {
            binding: BindingRef::new("target").unwrap(),
            instance: instance.clone(),
            snapshot_id: RequestId::new("source").unwrap(),
            environment_revision: 1,
            layer_revision: 1,
            fingerprint: fp.clone(),
            display_frame: frame,
            continuity: BindingContinuityProof {
                kind: ObjectIdentityContinuity::NotificationCovered,
                source_sequence: 1,
                context_digest: format!("bc1:{}", "a".repeat(64)),
                scope: BindingProofScope::WholeStructure,
            },
            device_context: DeviceContextWitness::WholeStructureV1 {
                table_column: None,
                app: Box::new(DeviceContextApp {
                    bundle_id: fp.app.clone(),
                    pid: fp.app_pid,
                }),
                active_layer: Some("window-1".into()),
                window: Box::new(DeviceContextWindow {
                    window_id: "window-1".into(),
                    title: None,
                    frame,
                    kind: interaction_contracts::desktop::source::LayerKind::Window,
                    modal: false,
                    owner_bundle_id: fp.app.clone(),
                }),
                target: Box::new(DeviceContextTarget {
                    object: context_target.clone(),
                    group: None,
                    scroll: None,
                }),
                ancestry: vec![DeviceContextObject {
                    role: "AXWindow".into(),
                    subrole: Some("AXStandardWindow".into()),
                    label: String::new(),
                    frame,
                }],
                sibling_parent_ancestry_index: Some(0),
                siblings: vec![context_target],
                target_sibling_index: 0,
                nearest_labelled_group_ancestry_index: None,
                scroll_ancestors: vec![],
                menu: None,
                focus: Some(Box::new(DeviceContextFocusRelation {
                    origin_window_id: "window-1".into(),
                    origin_window_frame: frame,
                    target_fingerprint: DeviceContextFingerprint {
                        app: fp.app.clone(),
                        app_pid: fp.app_pid,
                        window_id: fp.window_id.clone(),
                        ancestor_roles: fp.ancestor_roles.clone(),
                        group: fp.group.clone(),
                        role: fp.role.clone(),
                        subrole: fp.subrole.clone(),
                        label: fp.label.clone(),
                        sibling_index: fp.sibling_index,
                        frame: fp.frame,
                    },
                    target_frame: frame,
                    relation: FocusRelation::ExactControl,
                    composition: DeviceContextComposition::NotApplicable,
                })),
            },
            menu_scope: None,
            focus_scope: Some(FocusedControlScope {
                profile: FocusedKeyProfile::MacosAtomicSpecialKeyV1,
                key: LayoutIndependentSpecialKey::Space,
                context: FocusContext {
                    source_session_id: "unit-source".into(),
                    session_epoch: identity.session_epoch,
                    process: KernelProcessIdentity {
                        pid: 42,
                        start_seconds: 1,
                        start_micros: 0,
                    },
                    origin_window_id: "window-1".into(),
                    origin_window_frame: frame,
                    target_instance: instance.clone(),
                    target_fingerprint: fp.clone(),
                    target_frame: frame,
                    focused_instance: instance,
                    relation: FocusRelation::ExactControl,
                    composition: CompositionState::NotApplicable,
                    keyboard: SourceKeyboardContext {
                        selected_input_source: None,
                        underlying_keyboard_layout: None,
                        keyboard_type: None,
                        caps_lock: Some(false),
                    },
                    verified_source_sequence: 1,
                    verified_observed_ms: now,
                },
            }),
        };
        let permit = PermitRef {
            id: "permit".into(),
            generation: 1,
        };
        let primitive = Primitive::KeyPress {
            key: "Space".into(),
            modifiers: Modifiers::default(),
        };
        let evidence = Observation {
            observation_id: "evidence".into(),
            session_epoch: identity.session_epoch,
            surface: identity.surface.clone(),
            target_ref: "target".into(),
            clock_domain: identity.clock_domain.clone(),
            source_sequence: 1,
            observed_ms: now,
            valid_until_ms: now + duration,
            freshness: SourceFreshness::Verified,
            execution_domain:
                interaction_contracts::desktop::domain::ExecutionDomain::ForegroundNative,
            purpose: ObservationPurpose::FocusedControl,
            owned_document_proof: None,
            binding_proof: Some(proof.clone()),
            dependencies: identity.dependencies.clone(),
        };
        let intent = ActionIntent {
            intent_id: "key-one".into(),
            attempt_id: Some("attempt".into()),
            task_id: "root".into(),
            task_revision: 1,
            root_epoch: 1,
            session_epoch: identity.session_epoch,
            execution_domain:
                interaction_contracts::desktop::domain::ExecutionDomain::ForegroundNative,
            permit: permit.clone(),
            surface: identity.surface.clone(),
            target_ref: "target".into(),
            binding_fingerprint: Some(fp.digest()),
            owned_document_proof: None,
            binding_proof: Some(proof),
            coordinate_space: CoordinateSpace::DisplayPoints,
            dependencies: identity.dependencies.clone(),
            evidence_id: "evidence".into(),
            latest_start_ms: now + 100,
            duration_ms: duration,
            steps: vec![ScheduledPrimitive {
                offset_ms: 0,
                primitive: primitive.clone(),
            }],
        };
        let authority = SubmissionAuthority {
            permit,
            stop_seq: 0,
            valid_until_ms: now + duration,
            command_end_ms: now + duration,
            evidence,
        };
        (identity, intent, authority, primitive)
    }

    fn window_activation_test_request(
        identity: &DeviceIdentity,
        intent: &ActionIntent,
    ) -> (NativeProbeRequest, OwnedSubmitCorrelation) {
        let frame = Rect {
            x: 176.0,
            y: 82.0,
            width: 1160.0,
            height: 768.0,
        };
        let label = "Interaction Fixture";
        let table_scope = RowTableScope {
            scroll_path: vec![RowContainerScope {
                role: RowContainerRole::AxScrollArea,
                label: "Archive".into(),
            }],
            column_headers: vec!["Archive Items".into()],
        };
        let fingerprint = InstanceFingerprint {
            app: "com.lingxi.computer-user.mvp-fixture".into(),
            app_pid: 42,
            window_id: "window-1".into(),
            ancestor_roles: vec![],
            group: None,
            role: "AXWindow".into(),
            subrole: Some("AXStandardWindow".into()),
            label: label.into(),
            sibling_index: 0,
            frame: [176, 82, 1160, 768],
            scroll_container: None,
        };
        let proof = |sequence: u64, observed_ms: u64| {
            let source = SourceWindowContext {
                source_session_id: "window-source-session".into(),
                session_epoch: identity.session_epoch,
                process: KernelProcessIdentity {
                    pid: 42,
                    start_seconds: 1_700_000_000,
                    start_micros: 123_456,
                },
                origin_window_id: "window-1".into(),
                origin_window_frame: frame,
                verified_source_sequence: sequence,
                verified_observed_ms: observed_ms,
            };
            BindingProof {
                binding: BindingRef::new("window-root-binding").unwrap(),
                instance: ObjectRef::new("window-root-instance").unwrap(),
                snapshot_id: RequestId::new(format!("window-source-{sequence}")).unwrap(),
                environment_revision: 1,
                layer_revision: 1,
                fingerprint: fingerprint.clone(),
                display_frame: frame,
                continuity: BindingContinuityProof {
                    kind: ObjectIdentityContinuity::NotificationCovered,
                    source_sequence: sequence,
                    context_digest: format!("bc1:{}", "a".repeat(64)),
                    scope: BindingProofScope::WindowRoot {
                        table_scope: table_scope.clone(),
                    },
                },
                device_context: DeviceContextWitness::WindowRootV1 {
                    app: Box::new(DeviceContextApp {
                        bundle_id: fingerprint.app.clone(),
                        pid: fingerprint.app_pid,
                    }),
                    window: Box::new(DeviceContextWindow {
                        window_id: fingerprint.window_id.clone(),
                        title: Some(label.into()),
                        frame,
                        kind: interaction_contracts::desktop::source::LayerKind::Window,
                        modal: false,
                        owner_bundle_id: fingerprint.app.clone(),
                    }),
                    root: DeviceContextObject {
                        role: fingerprint.role.clone(),
                        subrole: fingerprint.subrole.clone(),
                        label: fingerprint.label.clone(),
                        frame,
                    },
                    source: Box::new(source),
                },
                menu_scope: None,
                focus_scope: None,
            }
        };
        let original_proof = proof(1, 90);
        let current_proof = proof(2, 100);
        let request = NativeProbeRequest {
            request_id: "window-submit-1".into(),
            intent_id: intent.intent_id.clone(),
            attempt_id: intent.attempt_id.clone(),
            task_id: intent.task_id.clone(),
            task_revision: intent.task_revision,
            root_epoch: intent.root_epoch,
            permit: intent.permit.clone(),
            stop_seq: 0,
            guard_revision: 0,
            clock_domain: identity.clock_domain.clone(),
            surface: intent.surface.clone(),
            evidence_id: intent.evidence_id.clone(),
            intent_evidence_id: intent.evidence_id.clone(),
            source_sequence: 2,
            observed_ms: 100,
            valid_until_ms: 60_000,
            command_end_ms: 61_100,
            started_ms: 100,
            duration_ms: 61_000,
            latest_start_ms: 100,
            original_proof,
            current_proof: current_proof.clone(),
            context_witness: current_proof.device_context.clone(),
            coordinate_space: None,
            execution_domain: None,
            purpose: None,
            binding_digest: fingerprint.digest(),
            dependencies: std::collections::BTreeMap::from([
                ("authorization".into(), 1),
                ("window".into(), 1),
            ]),
            action: "activate_window",
            x: 756.0,
            y: 466.0,
            value: None,
            mouse: None,
            scroll: None,
            key: None,
        };
        let correlation = OwnedSubmitCorrelation::from_action_intent(
            intent,
            &request.request_id,
            identity.session_epoch,
        )
        .unwrap();
        let mut correlation = correlation;
        assert!(correlation.attach_window_activation(&request));
        (request, correlation)
    }

    fn start(
        worker: &FakeWorker,
        clock: SharedClock,
        identity: &DeviceIdentity,
    ) -> (GuardClient, GuardSnapshot) {
        let guard = GuardClient::start_execution(
            &worker.script,
            identity,
            clock,
            12345,
            42,
            "com.lingxi.computer-user.guard-fixture",
            Some("window-1"),
            None,
        )
        .unwrap();
        wait_ready(guard)
    }

    fn start_with_diagnostics(
        worker: &FakeWorker,
        clock: SharedClock,
        identity: &DeviceIdentity,
        diagnostics: KeyDiagnostics,
    ) -> (GuardClient, GuardSnapshot) {
        let guard = GuardClient::start_scoped_with_diagnostics(
            &worker.script,
            identity,
            clock,
            12345,
            Some((42, "com.lingxi.computer-user.guard-fixture".into())),
            Some("window-1".into()),
            None,
            diagnostics,
        )
        .unwrap();
        wait_ready(guard)
    }

    fn wait_ready(mut guard: GuardClient) -> (GuardClient, GuardSnapshot) {
        // These IPC tests are not product-liveness acceptance. Keep the real
        // production thresholds covered by guard_health_expired's boundary
        // test, while tolerating a descheduled Python mock under parallel load.
        guard.health_limits = GuardHealthLimits::MOCK_IPC;
        let deadline =
            Instant::now() + GuardHealthLimits::MOCK_IPC.startup + Duration::from_secs(1);
        loop {
            if let Some(snapshot) = guard.poll().unwrap()
                && snapshot.authority_clock_ready
            {
                return (guard, snapshot);
            }
            assert!(Instant::now() < deadline, "fake worker did not initialize");
            thread::sleep(Duration::from_millis(2));
        }
    }
    fn await_reply(guard: &mut GuardClient) -> Result<NativeSubmitResponse, String> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(reply) = guard.poll_submission()? {
                return Ok(reply);
            }
            if Instant::now() >= deadline {
                return Err("fake submission timed out".into());
            }
            thread::sleep(Duration::from_millis(2));
        }
    }
    fn wait_commands(worker: &FakeWorker, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while worker.commands().len() < count {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn background_execution_uses_an_explicit_birth_bound_startup_marker() {
        let clock = SharedClock::starting_now();
        let (identity, _, _, _) = fixture(clock, 1000);
        let worker = FakeWorker::new("argv_capture");
        let process = KernelProcessIdentity {
            pid: 42,
            start_seconds: 1_700_000_000,
            start_micros: 123_456,
        };
        let guard = GuardClient::start_background_ax_execution(
            &worker.script,
            &identity,
            clock,
            12345,
            42,
            "com.lingxi.computer-user.mvp-fixture",
            "window-7",
            &process,
        )
        .unwrap();
        assert!(guard.is_background_ax_execution());
        assert_eq!(
            guard.background_ax_scope(),
            Some(NativeBackgroundAxScope {
                pid: 42,
                bundle_id: "com.lingxi.computer-user.mvp-fixture".into(),
                window_id: "window-7".into(),
                start_seconds: 1_700_000_000,
                start_micros: 123_456,
            })
        );

        let deadline = Instant::now() + Duration::from_secs(2);
        let startup = loop {
            if let Some(value) = worker
                .commands()
                .into_iter()
                .find(|value| value.get("startup_argv").is_some())
            {
                break value;
            }
            assert!(
                Instant::now() < deadline,
                "fake helper did not capture argv"
            );
            thread::sleep(Duration::from_millis(2));
        };
        let args = startup["startup_argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(args.last(), Some(&"--execute-background-ax"));
        let start_seconds = args
            .iter()
            .position(|arg| *arg == "--execute-start-seconds")
            .unwrap();
        assert_eq!(args[start_seconds + 1], "1700000000");
        assert_eq!(args[start_seconds + 2], "--execute-start-micros");
        assert_eq!(args[start_seconds + 3], "123456");
        drop(guard);
    }

    #[test]
    fn process_start_identity_is_forwarded_only_with_a_matching_fixed_window() {
        let clock = SharedClock::starting_now();
        let (identity, _, _, _) = fixture(clock, 1000);
        let process = KernelProcessIdentity {
            pid: 42,
            start_seconds: 1_700_000_000,
            start_micros: 123_456,
        };

        {
            let worker = FakeWorker::new("argv_capture");
            let guard = GuardClient::start_execution(
                &worker.script,
                &identity,
                clock,
                12345,
                42,
                "com.lingxi.computer-user.guard-fixture",
                Some("window-1"),
                Some(&process),
            )
            .unwrap();
            let (mut guard, _) = wait_ready(guard);
            let startup = worker.commands().remove(0);
            let args: Vec<_> = startup["startup_argv"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap().to_string())
                .collect();
            let value_after = |flag: &str| {
                args.iter()
                    .position(|value| value == flag)
                    .and_then(|index| args.get(index + 1))
                    .map(String::as_str)
            };
            assert_eq!(value_after("--execute-pid"), Some("42"));
            assert_eq!(
                value_after("--execute-bundle"),
                Some("com.lingxi.computer-user.guard-fixture")
            );
            assert_eq!(value_after("--execute-window"), Some("window-1"));
            assert_eq!(value_after("--execute-start-seconds"), Some("1700000000"));
            assert_eq!(value_after("--execute-start-micros"), Some("123456"));
            assert_eq!(
                args.iter()
                    .filter(|value| value.as_str() == "--execute-start-seconds")
                    .count(),
                1
            );
            assert_eq!(
                args.iter()
                    .filter(|value| value.as_str() == "--execute-start-micros")
                    .count(),
                1
            );
            guard.stop();
        }

        {
            let worker = FakeWorker::new("argv_capture");
            let guard = GuardClient::start_execution(
                &worker.script,
                &identity,
                clock,
                12345,
                42,
                "com.lingxi.computer-user.guard-fixture",
                Some("window-1"),
                None,
            )
            .unwrap();
            let (mut guard, _) = wait_ready(guard);
            let startup = worker.commands().remove(0);
            let args = startup["startup_argv"].as_array().unwrap();
            assert!(!args.iter().any(|value| {
                matches!(
                    value.as_str(),
                    Some("--execute-start-seconds" | "--execute-start-micros")
                )
            }));
            guard.stop();

            let wrong_pid = KernelProcessIdentity {
                pid: 43,
                ..process.clone()
            };
            assert!(
                GuardClient::start_execution(
                    &worker.script,
                    &identity,
                    clock,
                    12345,
                    42,
                    "com.lingxi.computer-user.guard-fixture",
                    Some("window-1"),
                    Some(&wrong_pid),
                )
                .is_err()
            );
            assert!(
                GuardClient::start_execution(
                    &worker.script,
                    &identity,
                    clock,
                    12345,
                    42,
                    "com.lingxi.computer-user.guard-fixture",
                    None,
                    Some(&process),
                )
                .is_err()
            );
            let invalid = KernelProcessIdentity {
                start_micros: 1_000_000,
                ..process
            };
            assert!(
                GuardClient::start_execution(
                    &worker.script,
                    &identity,
                    clock,
                    12345,
                    42,
                    "com.lingxi.computer-user.guard-fixture",
                    Some("window-1"),
                    Some(&invalid),
                )
                .is_err()
            );
        }
    }

    #[test]
    fn saturated_diagnostics_and_stalled_writer_never_delay_submit_stop_or_kill() {
        let worker = FakeWorker::new("hold_probe");
        let clock = SharedClock::starting_now();
        let (identity, _, _, _) = fixture(clock, 1000);
        let (mut guard, _) = start(&worker, clock, &identity);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let diagnostics = OwnedKeyDiagnostics::spawn_writer(1, move |_| {
            entered_tx.send(()).unwrap();
            let _ = release_rx.recv();
            let _ = done_tx.send(());
        })
        .unwrap();
        guard.diagnostics = Some(diagnostics.clone());
        assert!(diagnostics.append("warmup", b"{}"));
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(diagnostics.append("metadata", b"{}"));
        for _ in 0..100 {
            assert!(!diagnostics.append("metadata", b"{}"));
        }
        assert_eq!(
            diagnostics.budget.load(Ordering::Relaxed) >> 16,
            DIAGNOSTIC_RECORDS
        );
        let (_, intent, authority, primitive) = fixture(clock, 1000);
        let before = Instant::now();
        guard
            .submit_press(
                &SubmitContext {
                    intent: &intent,
                    authority,
                },
                &primitive,
            )
            .unwrap();
        guard.stop();
        guard.revoke(&intent.permit);
        key_reader_failure(&guard.diagnostics, &guard.mailbox, "test_failure");
        guard.terminal = true;
        assert!(guard.latest_checked().is_err());
        assert!(
            before.elapsed() < Duration::from_millis(250),
            "full diagnostic queue or writer delayed stop/revoke/helper kill"
        );
        assert!(done_rx.try_recv().is_err(), "writer must still be stalled");
        drop(diagnostics);
        let before = Instant::now();
        drop(guard);
        assert!(
            before.elapsed() < Duration::from_millis(500),
            "GuardClient drop waited on the diagnostic writer"
        );
        assert!(done_rx.try_recv().is_err());
        release_tx.send(()).unwrap();
        done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(
            worker
                .commands()
                .iter()
                .filter(|c| c["command"] == "atomic_submit")
                .count(),
            0
        );
    }

    #[test]
    fn key_preflight_then_atomic_uses_exactly_one_immutable_packet_and_deadline() {
        let worker = FakeWorker::new("eligible");
        let clock = SharedClock::starting_now();
        let (identity, _, _, _) = fixture(clock, 1000);
        let (mut guard, _) = start(&worker, clock, &identity);
        let diagnostics = worker.dir.join("diagnostics");
        desktop_ipc::socket::private_dir(&diagnostics).unwrap();
        guard.diagnostics = OwnedKeyDiagnostics::in_directory(diagnostics.clone());
        let (_, intent, authority, primitive) = fixture(clock, 1000);
        guard
            .submit_press(
                &SubmitContext {
                    intent: &intent,
                    authority,
                },
                &primitive,
            )
            .unwrap();
        let outer = guard.submit_deadline;
        let reply = await_reply(&mut guard).unwrap();
        assert_eq!(reply.outcome, "applied");
        assert!(reply.submitted);
        let commands = worker.commands();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0]["command"], "authority_probe");
        assert_eq!(commands[1]["command"], "atomic_submit");
        for command in &commands {
            let witness_size = serde_json::to_vec(&command["probe"]["context_witness"])
                .unwrap()
                .len();
            assert!(
                witness_size
                    <= interaction_contracts::desktop::binding::MAX_DEVICE_CONTEXT_WITNESS_BYTES,
                "the transmitted current witness stays within 4 KiB"
            );
            let mut packet = serde_json::to_vec(command).unwrap();
            packet.push(b'\n');
            assert!(
                packet.len() as u64 <= RECORD_LIMIT,
                "the complete native command stays within the guard record limit"
            );
        }
        wait_diagnostics(guard.diagnostics.as_ref().unwrap(), 2);
        for (name, command) in ["00-authority-probe.json", "01-atomic-submit.json"]
            .iter()
            .zip(&commands)
        {
            let bytes = fs::read(diagnostics.join(name)).unwrap();
            assert_eq!(bytes.last(), Some(&b'\n'));
            assert_eq!(
                &serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
                command,
                "diagnostics retain the exact serialized envelope received by the worker"
            );
        }
        assert_eq!(fs::read_dir(&diagnostics).unwrap().count(), 2);
        assert_eq!(
            commands[0]["probe"], commands[1]["probe"],
            "all proofs, permit, IDs and timing are byte-equivalent values"
        );
        assert!(outer.is_some());
        assert_eq!(guard.submit_deadline, None);
        assert!(guard.poll_submission().unwrap().is_none());
        assert_eq!(worker.commands().len(), 2);
    }

    #[test]
    fn owned_receipts_mode_captures_ax_press_reply_without_outbound_packet() {
        let worker = FakeWorker::new("eligible");
        let clock = SharedClock::starting_now();
        let (identity, _, _, _) = fixture(clock, 1000);
        let diagnostics = worker.dir.join("diagnostics");
        desktop_ipc::socket::private_dir(&diagnostics).unwrap();
        let diagnostic_writer =
            OwnedKeyDiagnostics::in_directory_with_mode(diagnostics.clone(), false);
        let (mut guard, _) = start_with_diagnostics(&worker, clock, &identity, diagnostic_writer);
        // Worker startup can consume the original one-second evidence window.
        // Take a fresh synthetic observation after readiness, as the adjacent
        // native preflight test already does, instead of extending old authority.
        let (_, mut intent, mut authority, key_primitive) = fixture(clock, 1000);

        guard
            .submit_press(
                &SubmitContext {
                    intent: &intent,
                    authority: authority.clone(),
                },
                &key_primitive,
            )
            .unwrap();
        let key_reply = await_reply(&mut guard).unwrap();
        assert_eq!(key_reply.outcome, "applied");

        let primitive = Primitive::AxAction {
            action: interaction_contracts::desktop::primitive::AxActionKind::Press,
        };
        intent.binding_proof.as_mut().unwrap().focus_scope = None;
        intent.steps[0].primitive = primitive.clone();
        authority.evidence.purpose = ObservationPurpose::TargetInput;
        authority
            .evidence
            .binding_proof
            .as_mut()
            .unwrap()
            .focus_scope = None;
        guard
            .submit_press(
                &SubmitContext {
                    intent: &intent,
                    authority: authority.clone(),
                },
                &primitive,
            )
            .unwrap();

        let reply = await_reply(&mut guard).unwrap();
        assert_eq!(reply.outcome, "applied");
        wait_diagnostics(guard.diagnostics.as_ref().unwrap(), 3);
        let mut entries: Vec<_> = fs::read_dir(&diagnostics).unwrap().flatten().collect();
        entries.sort_by_key(|entry| entry.file_name());
        assert_eq!(entries.len(), 3, "receipts mode must omit outbound packets");
        assert_eq!(entries[0].file_name(), "00-probe-response.json");
        assert_eq!(entries[1].file_name(), "01-atomic-response.json");
        assert_eq!(entries[2].file_name(), "02-atomic-response.json");
        let probe: serde_json::Value =
            serde_json::from_slice(&fs::read(entries[0].path()).unwrap()).unwrap();
        assert_eq!(probe["kind"], "owned_key_guard_probe_response");
        for (entry, request_id) in [(&entries[1], "submit-1"), (&entries[2], "submit-2")] {
            let bytes = fs::read(entry.path()).unwrap();
            assert!(bytes.len() <= 1024);
            let saved: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(saved["kind"], "owned_key_guard_atomic_response");
            assert_eq!(saved["request_id"], request_id);
            assert_eq!(saved["outcome"], "applied");
            assert_eq!(saved["reason"], "fake receipt; no OS input");
            assert_eq!(saved["submitted"], true);
            assert!(saved["committed_ms"].as_u64().is_some());
            assert!(saved["returned_ms"].as_u64().is_some());
            assert!(saved["reason"].as_str().unwrap().len() <= 512);
        }
        let multibyte_reason = "é".repeat(300);
        assert_eq!(diagnostic_reason(&multibyte_reason).len(), 512);
        let commands = worker.commands();
        assert_eq!(commands.len(), 3);
        assert_eq!(commands[0]["command"], "authority_probe");
        assert_eq!(commands[1]["command"], "atomic_submit");
        assert_eq!(commands[2]["command"], "atomic_submit");
    }

    #[test]
    fn preflight_rejection_transport_loss_bad_kind_cross_id_and_guard_change_never_send_atomic() {
        for mode in [
            "reject",
            "exit_probe",
            "wrong_atomic",
            "cross_id",
            "guard_change",
        ] {
            let worker = FakeWorker::new(mode);
            let clock = SharedClock::starting_now();
            let (identity, _, _, _) = fixture(clock, 1000);
            let (mut guard, _) = start(&worker, clock, &identity);
            let (_, intent, authority, primitive) = fixture(clock, 1000);
            guard
                .submit_press(
                    &SubmitContext {
                        intent: &intent,
                        authority,
                    },
                    &primitive,
                )
                .unwrap();
            let reply = await_reply(&mut guard).unwrap();
            assert!(!reply.submitted, "mode {mode}");
            assert!(reply.committed_ms.is_none() && reply.returned_ms.is_none());
            assert_eq!(reply.outcome, "context_unresolved", "mode {mode}");
            assert_eq!(
                worker.commands().len(),
                1,
                "mode {mode} must never queue atomic input"
            );
        }
    }

    #[test]
    fn original_preflight_expiry_local_stop_and_revoke_are_definite_zero_input_rejections() {
        for mode in ["expiry", "stop", "revoke"] {
            let worker = FakeWorker::new("hold_probe");
            let clock = SharedClock::starting_now();
            let (identity, _, _, _) = fixture(clock, 1000);
            let (mut guard, _) = start(&worker, clock, &identity);
            let (_, intent, authority, primitive) = fixture(clock, 120);
            guard
                .submit_press(
                    &SubmitContext {
                        intent: &intent,
                        authority,
                    },
                    &primitive,
                )
                .unwrap();
            wait_commands(&worker, 1);
            match mode {
                "stop" => guard.stop(),
                "revoke" => guard.revoke(&intent.permit),
                _ => {}
            }
            let reply = await_reply(&mut guard).unwrap();
            assert_eq!(
                reply.outcome,
                match mode {
                    "stop" => "stop_latched",
                    "revoke" => "revoked",
                    _ => "expired",
                }
            );
            assert!(!reply.submitted);
            assert_eq!(worker.commands().len(), 1);
        }
    }

    #[test]
    fn after_atomic_send_transport_loss_and_late_probe_are_unknown_not_preflight_rejection() {
        for mode in ["exit_atomic", "late_probe"] {
            let worker = FakeWorker::new(mode);
            let clock = SharedClock::starting_now();
            let (identity, _, _, _) = fixture(clock, 1000);
            let (mut guard, _) = start(&worker, clock, &identity);
            let (_, intent, authority, primitive) = fixture(clock, 1000);
            guard
                .submit_press(
                    &SubmitContext {
                        intent: &intent,
                        authority,
                    },
                    &primitive,
                )
                .unwrap();
            assert!(await_reply(&mut guard).unwrap_err().contains("unknown"));
            assert_eq!(worker.commands().len(), 2);
        }
    }

    #[test]
    fn backend_guard_drop_preserves_preflight_zero_input_but_atomic_drop_is_unknown() {
        for atomic_sent in [false, true] {
            let worker = FakeWorker::new(if atomic_sent {
                "exit_atomic"
            } else {
                "hold_probe"
            });
            let clock = SharedClock::starting_now();
            let (identity, _, _, _) = fixture(clock, 1000);
            let (guard, snapshot) = start(&worker, clock, &identity);
            let mut backend = NativeAxBackend::new(
                (42, "com.lingxi.computer-user.guard-fixture".into()),
                Some("window-1".into()),
            );
            backend.attach(guard).unwrap();
            backend.snapshot(&snapshot);
            let (_, intent, authority, primitive) = fixture(clock, 1000);
            assert!(matches!(
                backend.submit(
                    &primitive,
                    &SubmitContext {
                        intent: &intent,
                        authority
                    }
                ),
                SubmitOutcome::Pending { .. }
            ));
            wait_commands(&worker, 1);
            if atomic_sent {
                let deadline = Instant::now() + Duration::from_secs(2);
                while worker.commands().len() < 2 {
                    let _ = backend.guard_mut().unwrap().poll_submission();
                    assert!(Instant::now() < deadline);
                    thread::sleep(Duration::from_millis(2));
                }
            }
            backend.disable();
            backend.disable();
            let completion = backend.poll_completion().unwrap();
            assert_eq!(
                completion.outcome,
                if atomic_sent {
                    SubmitOutcome::Unknown
                } else {
                    SubmitOutcome::Rejected
                }
            );
            assert!(completion.submitted_at_ms.is_none() && completion.input_end_ms.is_none());
            assert_eq!(worker.commands().len(), if atomic_sent { 2 } else { 1 });
        }
    }
}
