//! One bounded native observation job. The helper owns all Apple framework IO;
//! this module owns process deadlines, output limits and the wire projection.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use desktop_ipc::socket;
use interaction_contracts::desktop::device::SurfaceRef;
use interaction_contracts::desktop::ids::RequestId;
use interaction_contracts::desktop::owned_document::OwnedDocumentReceiverEvidence;
#[cfg(test)]
use interaction_contracts::desktop::source::AxEnabledObservation;
use interaction_contracts::desktop::source::{
    AppIdentity, CompositionState, FocusContext, GlobalFocusObservation, LayerKind, MenuContext,
    MenuOpenerAnchor, ObjectIdentityContinuity, ObservationFact, ObserverCoverage, SourceHealth,
    SourceIdentityBoundary, SourceKind, SourceState, SourceWindowContext, StructureSnapshot,
    TableViewport, TargetContext, UiObject, WindowInfo,
};
use interaction_contracts::desktop::table_scope::TableScopeSearch;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::NativeConfig;

pub mod watch;

pub(crate) fn validate_requested_table_scope(
    config: &NativeConfig,
    actual: Option<&TableScopeSearch>,
) -> Result<(), String> {
    let Some(actual) = actual else {
        return Ok(());
    };
    let expected = config
        .table_scope
        .as_ref()
        .ok_or("native table census was not requested")?;
    let Some(crate::config::TextReadPolicy::RegisteredEditor { window_id }) = &config.text_policy
    else {
        return Err("native table census has no fixed window scope".into());
    };
    if actual.requested_scope != *expected
        || actual.process_id != config.pid.unwrap_or(0)
        || actual.window_id != *window_id
    {
        return Err("native table census changed its requested process/window/table scope".into());
    }
    Ok(())
}

// Tiny helper watchdog fixtures measure protocol/failure behavior, not aggregate
// process startup performance. Share one lane so unrelated helper starts cannot
// consume their deadlines on a loaded macOS host. Production stays concurrent.
#[cfg(test)]
pub(crate) static HELPER_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub const MAX_HELPER_OUTPUT: usize = 768 * 1024;
const MAX_DIAGNOSTICS: usize = 16 * 1024;
const MAX_CAPTURE_BYTES: u64 = 12 * 1024 * 1024;
const RETAINED_OBSERVATIONS: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativePermissions {
    pub accessibility: bool,
    pub screen_capture: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeStructure {
    app: AppIdentity,
    #[serde(default)]
    global_focus: Option<GlobalFocusObservation>,
    #[serde(default)]
    background_window_context: Option<NativeBackgroundWindowContext>,
    #[serde(default)]
    identity_boundary: Option<SourceIdentityBoundary>,
    #[serde(default)]
    owned_document_evidence: Option<OwnedDocumentReceiverEvidence>,
    observer_coverage: ObserverCoverage,
    windows: Vec<WindowInfo>,
    active_layer: Option<String>,
    complete: bool,
    incomplete_reason: Option<String>,
    objects: Vec<UiObject>,
    menu_anchors: Vec<MenuOpenerAnchor>,
    menu_context: Option<MenuContext>,
    focus_context: Option<FocusContext>,
    window_context: Option<SourceWindowContext>,
    #[serde(default)]
    table_viewports: Vec<TableViewport>,
    #[serde(default)]
    table_scope_search: Option<TableScopeSearch>,
}

/// Native helper facts before Source attaches the public hash and clock stamps.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeBackgroundWindowContext {
    pub window: SourceWindowContext,
    pub hidden: ObservationFact<bool>,
    pub minimized: ObservationFact<bool>,
}

pub(crate) fn unavailable_global_focus() -> GlobalFocusObservation {
    GlobalFocusObservation {
        app: ObservationFact::Unavailable,
        window_id: ObservationFact::Unavailable,
    }
}

pub(crate) fn validate_native_global_focus_window(
    snapshot: &StructureSnapshot,
) -> Result<(), String> {
    let (
        ObservationFact::Known { value: focus_app },
        ObservationFact::Known {
            value: focus_window,
        },
    ) = (&snapshot.global_focus.app, &snapshot.global_focus.window_id)
    else {
        return Ok(());
    };
    if focus_app.bundle_id != snapshot.app.bundle_id || focus_app.pid != snapshot.app.pid {
        return Ok(());
    }
    let matches: Vec<_> = snapshot
        .windows
        .iter()
        .filter(|window| window.window_id.as_str() == focus_window.as_str())
        .collect();
    let [window] = matches.as_slice() else {
        return Err(
            "native global focus window does not resolve to one observed target window".into(),
        );
    };
    if window.owner_bundle_id != focus_app.bundle_id || window.kind != LayerKind::Window {
        return Err(
            "native global focus window has a different owner or is not an observed AX window"
                .into(),
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeOcr {
    complete: bool,
    reason: Option<String>,
    blocks: Vec<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeObservation {
    protocol_version: u16,
    kind: String,
    permissions: NativePermissions,
    structure: NativeStructure,
    structure_started_uptime_ns: u64,
    structure_finished_uptime_ns: u64,
    frame: Option<Value>,
    ocr: NativeOcr,
    errors: Vec<String>,
}

/// Source owns this conversion; callers must validate helper-local provenance
/// before converting its stamps to the outer snapshot clock and sequence.
pub(crate) fn normalize_snapshot_provenance(snapshot: &mut StructureSnapshot, sequence: u64) {
    snapshot.source_sequence = sequence;
    for anchor in &mut snapshot.menu_anchors {
        anchor.verified_source_sequence = sequence;
        anchor.verified_observed_ms = snapshot.observed_ms;
    }
    if let Some(focus) = snapshot.focus_context.as_mut() {
        let composition_proof_matches_raw_focus = match &focus.composition {
            CompositionState::Marked { proof } | CompositionState::Unmarked { proof } => {
                proof.focused_instance == focus.focused_instance
                    && proof.verified_source_sequence == focus.verified_source_sequence
                    && proof.verified_observed_ms == focus.verified_observed_ms
            }
            CompositionState::Unknown | CompositionState::NotApplicable => false,
        };
        focus.session_epoch = snapshot.session_epoch;
        focus.verified_source_sequence = sequence;
        focus.verified_observed_ms = snapshot.observed_ms;
        if composition_proof_matches_raw_focus {
            match &mut focus.composition {
                CompositionState::Marked { proof } | CompositionState::Unmarked { proof } => {
                    proof.verified_source_sequence = sequence;
                    proof.verified_observed_ms = snapshot.observed_ms;
                }
                CompositionState::Unknown | CompositionState::NotApplicable => {}
            }
        }
    }
    if let Some(window) = snapshot.window_context.as_mut() {
        window.verified_source_sequence = sequence;
        window.verified_observed_ms = snapshot.observed_ms;
    }
    if let Some(TargetContext::NativeWindow { source, .. }) = snapshot.target_context.as_mut() {
        source.window.verified_source_sequence = sequence;
        source.window.verified_observed_ms = snapshot.observed_ms;
    }
    if let Some(context) = snapshot.menu_context.as_mut() {
        context.opener.verified_source_sequence = sequence;
        context.opener.verified_observed_ms = snapshot.observed_ms;
    }
}

#[derive(Debug)]
pub struct NativeReport {
    pub snapshot: Option<StructureSnapshot>,
    pub health: Vec<SourceHealth>,
    pub manifest: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct Job {
    observe: bool,
    observed_ms: u64,
    sequence: u64,
}

/// The source loop can keep receiving EOF and refresh messages while a helper
/// is busy. There is one running job and at most one queued job, never an IO
/// thread per incoming refresh. Refreshes are coalesced by the shell.
pub struct NativeWorker {
    jobs: Option<SyncSender<Job>>,
    results: Receiver<Result<NativeReport, String>>,
    cancel: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    busy: bool,
}

impl NativeWorker {
    pub fn new(
        config: NativeConfig,
        session_epoch: u64,
        clock_domain: String,
    ) -> Result<Self, String> {
        if config.owned_document_bootstrap.is_some() {
            return Err(
                "private owned-document bootstrap requires a persistent native watch".into(),
            );
        }
        socket::private_dir(&config.assets_dir).map_err(|e| format!("native assets: {e}"))?;
        let run_dir = config.assets_dir.join(format!(
            "run-{}",
            socket::new_token().map_err(|e| format!("native run identity: {e}"))?
        ));
        socket::private_dir(&run_dir).map_err(|e| format!("native run directory: {e}"))?;
        let (jobs, incoming) = mpsc::sync_channel::<Job>(1);
        let (done, results) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let cancelled = cancel.clone();
        let worker = std::thread::Builder::new()
            .name("source-native-job".into())
            .spawn(move || {
                let mut retained = std::collections::VecDeque::new();
                while let Ok(job) = incoming.recv() {
                    if cancelled.load(Ordering::Acquire) {
                        break;
                    }
                    let output_dir = run_dir.join(format!("observation-{}", job.sequence));
                    let result = observe_job(
                        &config,
                        &job,
                        session_epoch,
                        &clock_domain,
                        &output_dir,
                        &cancelled,
                    );
                    if result
                        .as_ref()
                        .is_ok_and(|report| report.manifest.is_some())
                    {
                        retained.push_back(output_dir.clone());
                        while retained.len() > RETAINED_OBSERVATIONS {
                            if let Some(old) = retained.pop_front() {
                                let _ = fs::remove_dir_all(old);
                            }
                        }
                    } else {
                        let _ = fs::remove_dir_all(&output_dir);
                    }
                    if cancelled.load(Ordering::Acquire) || done.send(result).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| format!("native source worker: {e}"))?;
        Ok(Self {
            jobs: Some(jobs),
            results,
            cancel,
            worker: Some(worker),
            busy: false,
        })
    }

    pub fn request(&mut self, observe: bool, observed_ms: u64, sequence: u64) -> bool {
        if self.busy {
            return false;
        }
        let job = Job {
            observe,
            observed_ms,
            sequence,
        };
        if self
            .jobs
            .as_ref()
            .is_some_and(|jobs| jobs.try_send(job).is_ok())
        {
            self.busy = true;
            true
        } else {
            false
        }
    }

    pub fn take_report(&mut self) -> Option<Result<NativeReport, String>> {
        match self.results.try_recv() {
            Ok(result) => {
                self.busy = false;
                Some(result)
            }
            Err(mpsc::TryRecvError::Disconnected) if self.busy => {
                self.busy = false;
                Some(Err("the native source worker exited".into()))
            }
            _ => None,
        }
    }
}

impl Drop for NativeWorker {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        self.jobs = None;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn short(reason: impl AsRef<str>) -> String {
    reason.as_ref().chars().take(1_024).collect()
}

pub fn unavailable(reason: &str, at_ms: u64) -> Vec<SourceHealth> {
    [
        SourceKind::Ax,
        SourceKind::AxObserver,
        SourceKind::Screenshot,
        SourceKind::Ocr,
    ]
    .map(|source| SourceHealth {
        source,
        state: SourceState::Unavailable,
        reason: Some(short(reason)),
        at_ms,
        provenance: None,
    })
    .to_vec()
}

fn health(
    source: SourceKind,
    available: bool,
    complete: bool,
    reason: &str,
    at_ms: u64,
) -> SourceHealth {
    SourceHealth {
        source,
        state: if !available {
            SourceState::Unavailable
        } else if complete {
            SourceState::Available
        } else {
            SourceState::Degraded
        },
        reason: (!available || !complete).then(|| short(reason)),
        at_ms,
        provenance: None,
    }
}

/// Recheck native observer claims at the process boundary. The contracts crate
/// validates the wire DTO too, but the adapter also ties every reported failure
/// to a currently projected read-scoped object before publishing it.
pub(crate) fn validate_observer_coverage(
    objects: &[UiObject],
    coverage: &ObserverCoverage,
) -> Result<(), String> {
    let mut continuity = std::collections::HashMap::with_capacity(objects.len());
    for object in objects {
        if continuity
            .insert(object.instance_ref.as_str(), object.identity_continuity)
            .is_some()
        {
            return Err("native structure contains a duplicate object reference".into());
        }
    }

    let expected_covered = objects
        .iter()
        .filter(|object| {
            object.identity_continuity == ObjectIdentityContinuity::NotificationCovered
        })
        .count();
    let expected_read_scoped = objects.len() - expected_covered;
    if usize::try_from(coverage.covered_objects).ok() != Some(expected_covered)
        || usize::try_from(coverage.read_scoped_objects).ok() != Some(expected_read_scoped)
        || coverage.failed_registrations as usize > coverage.read_scoped_objects as usize * 8
        || coverage.failures.iter().any(|failure| {
            continuity.get(failure.instance_ref.as_str())
                != Some(&ObjectIdentityContinuity::ReadScoped)
        })
    {
        return Err(
            "native observer coverage does not match its projected object continuity".into(),
        );
    }
    Ok(())
}

pub(crate) fn observer_coverage_reason(coverage: &ObserverCoverage) -> Option<String> {
    match coverage.state {
        SourceState::Available => None,
        SourceState::Degraded => Some(format!(
            "AX notification coverage is partial ({} read-scoped objects, {} failed registrations)",
            coverage.read_scoped_objects, coverage.failed_registrations
        )),
        SourceState::Unavailable => Some("AX notification observer is unavailable".into()),
    }
}

fn observe_job(
    config: &NativeConfig,
    job: &Job,
    session_epoch: u64,
    clock_domain: &str,
    output_dir: &Path,
    cancel: &AtomicBool,
) -> Result<NativeReport, String> {
    let request = if job.observe {
        socket::private_dir(output_dir)
            .map_err(|e| format!("native observation directory: {e}"))?;
        json!({
            "protocol_version": 1,
            "command": "observe",
            "bundle_id": config.bundle_id,
            "pid": config.pid,
            "output_dir": output_dir,
            "timeout_ms": config.timeout_ms,
            "max_nodes": 512,
            "max_depth": 12,
            "max_ocr_blocks": 128,
            "text_policy": config.text_policy,
            "table_scope": config.table_scope,
        })
    } else {
        json!({"protocol_version":1,"command":"permissions"})
    };
    let response = run_helper(config, &request, cancel)?;
    if response["protocol_version"] != 1 {
        return Err("unsupported native helper protocol version".into());
    }
    if response["kind"] == "error" {
        return Err(short(
            response["error"].as_str().unwrap_or("native helper error"),
        ));
    }
    if !job.observe {
        if response["kind"] != "permissions" {
            return Err("the native helper did not return permissions".into());
        }
        let permissions: NativePermissions = serde_json::from_value(json!({
            "accessibility": response["accessibility"],
            "screen_capture": response["screen_capture"],
        }))
        .map_err(|e| format!("invalid native permissions: {e}"))?;
        return Ok(NativeReport {
            snapshot: None,
            health: vec![
                health(
                    SourceKind::Ax,
                    permissions.accessibility,
                    false,
                    if permissions.accessibility {
                        "Accessibility permission has been probed; AX has not been observed"
                    } else {
                        "Accessibility permission is unavailable"
                    },
                    job.observed_ms,
                ),
                // A permission check cannot prove that capture or OCR is working.
                health(
                    SourceKind::Screenshot,
                    permissions.screen_capture,
                    false,
                    if permissions.screen_capture {
                        "Screen Recording permission is available; capture has not been observed"
                    } else {
                        "Screen Recording permission is unavailable"
                    },
                    job.observed_ms,
                ),
                health(
                    SourceKind::Ocr,
                    permissions.screen_capture,
                    false,
                    if permissions.screen_capture {
                        "OCR has not been observed"
                    } else {
                        "OCR is unavailable without Screen Recording permission"
                    },
                    job.observed_ms,
                ),
                SourceHealth {
                    source: SourceKind::AxObserver,
                    state: SourceState::Unavailable,
                    reason: Some("AX observer coverage has not been observed".into()),
                    at_ms: job.observed_ms,
                    provenance: None,
                },
            ],
            manifest: None,
        });
    }
    if response["kind"] != "observation" {
        return Err("the native helper did not return an observation".into());
    }
    let observation: NativeObservation = serde_json::from_value(response.clone())
        .map_err(|e| format!("invalid native observation: {e}"))?;
    if observation.structure.app.bundle_id != config.bundle_id
        || config
            .pid
            .is_some_and(|pid| observation.structure.app.pid != pid)
    {
        return Err("the native helper observed a different target application".into());
    }
    if observation.structure.app.pid == 0 {
        return Err("the target application is missing or ambiguous".into());
    }
    if observation.structure.owned_document_evidence.is_some()
        || config.owned_document_bootstrap.is_some()
        || observation.structure.background_window_context.is_some()
        || config.background_target.is_some()
    {
        return Err(
            "one-shot native observations cannot carry private or background-watch evidence".into(),
        );
    }
    if !observation.structure.menu_anchors.is_empty()
        || observation.structure.menu_context.is_some()
        || observation.structure.focus_context.is_some()
        || observation.structure.window_context.is_some()
    {
        return Err("one-shot native observations cannot retain menu authority".into());
    }
    if observation
        .structure
        .objects
        .iter()
        .any(|object| object.secure && object.value.is_some())
    {
        return Err("the native helper returned a secure field value".into());
    }
    if observation.structure.objects.len() > 512
        || observation.structure.windows.len() > 128
        || observation.ocr.blocks.len() > 128
    {
        return Err("the native helper exceeded an observation count limit".into());
    }
    validate_observer_coverage(
        &observation.structure.objects,
        &observation.structure.observer_coverage,
    )?;
    if observation.structure.observer_coverage.state != SourceState::Unavailable
        || observation.structure.observer_coverage.covered_objects != 0
        || observation.structure.observer_coverage.read_scoped_objects as usize
            != observation.structure.objects.len()
        || observation.structure.observer_coverage.failed_registrations != 0
        || !observation.structure.observer_coverage.failures.is_empty()
        || observation.structure.identity_boundary.is_some()
    {
        return Err(
            "one-shot native observations cannot claim persistent identity or observer coverage"
                .into(),
        );
    }
    let frame_present = match &observation.frame {
        Some(frame) => {
            validate_asset(output_dir, frame)?;
            true
        }
        None => false,
    };
    validate_requested_table_scope(config, observation.structure.table_scope_search.as_ref())?;
    let snapshot = StructureSnapshot {
        snapshot_id: RequestId::new(format!("native-{session_epoch}-{}", job.sequence))
            .map_err(|e| e.to_string())?,
        session_epoch,
        surface: SurfaceRef {
            id: "main".into(),
            epoch: session_epoch,
        },
        clock_domain: clock_domain.into(),
        source_sequence: job.sequence,
        // Recorded before helper launch: conservatively older than the oldest
        // AX read, rather than refreshing an observation with its arrival time.
        observed_ms: job.observed_ms,
        identity_boundary: observation.structure.identity_boundary,
        // The one-shot observation does not attest the image content identity.
        frame_ref: None,
        observer_coverage: observation.structure.observer_coverage,
        owned_document_evidence: None,
        app: observation.structure.app,
        windows: observation.structure.windows,
        global_focus: observation
            .structure
            .global_focus
            .unwrap_or_else(unavailable_global_focus),
        target_context: None,
        complete: observation.structure.complete && observation.permissions.accessibility,
        incomplete_reason: if !observation.permissions.accessibility {
            Some("Accessibility permission is unavailable".into())
        } else {
            observation.structure.incomplete_reason
        },
        objects: observation.structure.objects,
        menu_anchors: observation.structure.menu_anchors,
        menu_context: observation.structure.menu_context,
        focus_context: observation.structure.focus_context,
        window_context: observation.structure.window_context,
        table_viewports: observation.structure.table_viewports,
        table_scope_search: observation.structure.table_scope_search,
    };
    validate_native_global_focus_window(&snapshot)?;
    snapshot
        .validate()
        .map_err(|e| format!("invalid native structure: {e}"))?;
    let reason = observation
        .errors
        .iter()
        .take(4)
        .cloned()
        .collect::<Vec<_>>()
        .join("; ");
    let health = vec![
        health(
            SourceKind::Ax,
            observation.permissions.accessibility,
            snapshot.complete,
            snapshot
                .incomplete_reason
                .as_deref()
                .unwrap_or("AX read was incomplete"),
            job.observed_ms,
        ),
        health(
            SourceKind::Screenshot,
            observation.permissions.screen_capture && frame_present,
            true,
            if reason.is_empty() {
                "capture did not produce an image"
            } else {
                &reason
            },
            job.observed_ms,
        ),
        health(
            SourceKind::Ocr,
            frame_present && observation.permissions.screen_capture,
            observation.ocr.complete,
            observation
                .ocr
                .reason
                .as_deref()
                .unwrap_or("OCR was incomplete"),
            job.observed_ms,
        ),
        SourceHealth {
            source: SourceKind::AxObserver,
            state: snapshot.observer_coverage.state,
            reason: observer_coverage_reason(&snapshot.observer_coverage),
            at_ms: job.observed_ms,
            provenance: None,
        },
    ];
    let manifest = output_dir.join("manifest.json");
    let manifest_value = json!({
        "protocol_version": 1,
        "snapshot": snapshot,
        "source_clock": "helper uptime bounds and caller wall-clock lower bound; no SCStream timestamp guarantee",
        "native": response,
    });
    private_json(&manifest, &manifest_value)?;
    let latest_tmp = config.assets_dir.join("latest.json.tmp");
    private_json(&latest_tmp, &json!({"manifest": manifest}))?;
    fs::rename(latest_tmp, config.assets_dir.join("latest.json"))
        .map_err(|e| format!("publishing native asset manifest: {e}"))?;
    Ok(NativeReport {
        snapshot: Some(snapshot),
        health,
        manifest: Some(manifest),
    })
}

fn validate_asset(output_dir: &Path, frame: &Value) -> Result<(), String> {
    let filename = frame["png_file"]
        .as_str()
        .ok_or("capture has no PNG basename")?;
    let mut components = Path::new(filename).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return Err("capture PNG must be a basename inside its observation directory".into());
    }
    let bytes = frame["byte_length"]
        .as_u64()
        .ok_or("capture has no byte length")?;
    if bytes == 0 || bytes > MAX_CAPTURE_BYTES {
        return Err("capture PNG exceeded its byte limit".into());
    }
    let metadata =
        fs::symlink_metadata(output_dir.join(filename)).map_err(|e| format!("capture PNG: {e}"))?;
    if !metadata.is_file() || metadata.len() != bytes {
        return Err("capture PNG is not a regular file with the declared size".into());
    }
    Ok(())
}

fn private_json(path: &Path, value: &Value) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .map_err(|e| format!("native manifest: {e}"))?;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("native manifest permissions: {e}"))?;
    serde_json::to_writer(&mut file, value).map_err(|e| format!("native manifest: {e}"))
}

/// This timeout bounds helper IO independently of the helper's own watchdog.
/// A malformed, oversized, crashed or timed-out helper never produces evidence.
fn run_helper(
    config: &NativeConfig,
    request: &Value,
    cancel: &AtomicBool,
) -> Result<Value, String> {
    let bytes = serde_json::to_vec(request).map_err(|e| e.to_string())?;
    if bytes.len() > 8_192 {
        return Err("native helper request exceeded its byte limit".into());
    }
    let mut child = Command::new(&config.helper_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("starting native source helper: {e}"))?;
    let write = child.stdin.take().expect("piped stdin").write_all(&bytes);
    if let Err(error) = write {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("native helper request: {error}"));
    }
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let (out_tx, out_rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout
            .take((MAX_HELPER_OUTPUT + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("native helper output: {e}"))
            .and_then(|_| {
                if bytes.len() > MAX_HELPER_OUTPUT {
                    Err("native helper output exceeded its byte limit".into())
                } else {
                    Ok(bytes)
                }
            });
        let _ = out_tx.send(result);
    });
    let (err_tx, err_rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut stderr = stderr;
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 4_096];
        loop {
            match stderr.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    let keep = count.min(MAX_DIAGNOSTICS.saturating_sub(bytes.len()));
                    bytes.extend_from_slice(&chunk[..keep]);
                }
            }
        }
        let _ = err_tx.send(bytes);
    });
    let deadline = Instant::now() + Duration::from_millis(config.timeout_ms);
    let mut output = None;
    let mut status = None;
    let result = loop {
        if cancel.load(Ordering::Acquire) {
            break Err("native observation cancelled".into());
        }
        if Instant::now() >= deadline {
            break Err("native source helper timed out".into());
        }
        if output.is_none() {
            match out_rx.try_recv() {
                Ok(Ok(bytes)) => output = Some(bytes),
                Ok(Err(error)) => break Err(error),
                Err(mpsc::TryRecvError::Disconnected) => {
                    break Err("native helper output reader exited".into());
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if status.is_none() {
            match child.try_wait() {
                Ok(exit) => status = exit,
                Err(error) => break Err(format!("native helper status: {error}")),
            }
        }
        if let (Some(bytes), Some(status)) = (&output, &status) {
            if !status.success() {
                let diagnostics = err_rx.try_recv().unwrap_or_default();
                // Drain diagnostics for pipe liveness; they may contain UI text
                // and are never copied into public health messages or logs.
                break Err(format!(
                    "native helper exited with {status} ({} diagnostic bytes)",
                    diagnostics.len()
                ));
            }
            break serde_json::from_slice(bytes)
                .map_err(|e| format!("invalid native helper JSON: {e}"));
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    if result.is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // Successful fixtures check parsing and retention, not helper latency.
    // Leave scheduling slack when the whole workspace suite is running.
    const FIXTURE_TIMEOUT_MS: u64 = 5_000;

    fn helper(script: &str, timeout_ms: u64) -> (tempfile::TempDir, NativeConfig) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("helper");
        // Drain the single request with a shell builtin. Extra `cat` children
        // added startup latency and descriptor lifetimes to unrelated assertions.
        fs::write(
            &path,
            format!("#!/bin/sh\nIFS= read -r request || :\n{script}\n"),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let config = NativeConfig {
            helper_path: path,
            bundle_id: "test.app".into(),
            pid: None,
            assets_dir: dir.path().join("assets"),
            timeout_ms,
            mode: crate::config::NativeCaptureMode::Probe,
            table_scope: None,
            background_target: None,
            text_policy: None,
            owned_document_bootstrap: None,
        };
        (dir, config)
    }

    #[test]
    fn permissions_are_passive_capability_checks_not_capture_evidence() {
        let _lane = HELPER_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (_dir, config) = helper(
            "printf '%s' '{\"protocol_version\":1,\"kind\":\"permissions\",\"accessibility\":false,\"screen_capture\":true}'",
            FIXTURE_TIMEOUT_MS,
        );
        let report = observe_job(
            &config,
            &Job {
                observe: false,
                observed_ms: 42,
                sequence: 0,
            },
            1,
            "macos/1/wall",
            &config.assets_dir,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(report.snapshot.is_none());
        assert_eq!(report.health[0].state, SourceState::Unavailable);
        assert_eq!(report.health[1].state, SourceState::Degraded);
        assert!(report.manifest.is_none());
    }

    #[test]
    fn helper_timeout_and_oversized_output_never_admit_evidence() {
        let _lane = HELPER_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (_dir, config) = helper("exec /bin/sleep 5", 100);
        let started = Instant::now();
        assert!(
            run_helper(
                &config,
                &json!({"command":"permissions"}),
                &AtomicBool::new(false)
            )
            .unwrap_err()
            .contains("timed out")
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        let (_dir, config) = helper("exec /usr/bin/head -c 786433 /dev/zero", FIXTURE_TIMEOUT_MS);
        let error = run_helper(
            &config,
            &json!({"command":"permissions"}),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(error.contains("byte limit"), "{error}");
    }

    #[test]
    fn stopping_a_worker_cancels_the_running_helper_promptly() {
        let _lane = HELPER_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (_dir, config) = helper("exec /bin/sleep 5", 5_000);
        let mut worker = NativeWorker::new(config, 1, "macos/1/wall".into()).unwrap();
        assert!(worker.request(false, 0, 0));
        assert!(!worker.request(false, 0, 1), "one running job is the bound");
        std::thread::sleep(Duration::from_millis(30));
        let started = Instant::now();
        drop(worker);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn capture_paths_cannot_escape_or_name_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            validate_asset(
                dir.path(),
                &json!({"png_file":"../outside.png","byte_length":1})
            )
            .is_err()
        );
        let outside = dir.path().join("outside");
        fs::write(&outside, "a").unwrap();
        std::os::unix::fs::symlink(&outside, dir.path().join("capture.png")).unwrap();
        assert!(
            validate_asset(
                dir.path(),
                &json!({"png_file":"capture.png","byte_length":1})
            )
            .is_err()
        );
    }

    fn observation(permissions: bool) -> Value {
        json!({
            "protocol_version": 1, "kind": "observation",
            "permissions": {"accessibility": permissions, "screen_capture": false},
            "structure": {
                "app": {"bundle_id": "test.app", "name": "Test", "pid": 42},
                "windows": [], "active_layer": null, "complete": true,
                "incomplete_reason": null, "objects": [],
                "observer_coverage":{"state":"unavailable","covered_objects":0,
                    "read_scoped_objects":0,"failed_registrations":0,"failures":[]},
                "menu_anchors": [], "menu_context": null
            },
            "structure_started_uptime_ns": 100, "structure_finished_uptime_ns": 200,
            "frame": null,
            "ocr": {"complete": false, "reason": "no capture", "blocks": []},
            "errors": ["Screen Recording permission unavailable"]
        })
    }

    fn native_observation(permissions: bool) -> Value {
        json!({
            "protocol_version":1,"kind":"observation",
            "permissions":{"accessibility":permissions,"screen_capture":false},
            "structure":observation(permissions)["structure"].clone(),
            "structure_started_uptime_ns":100,"structure_finished_uptime_ns":200,
            "frame":null,
            "ocr":{"complete":false,"reason":"no capture","blocks":[]},
            "errors":[]
        })
    }

    fn table_viewport_observation() -> Value {
        let mut response = native_observation(true);
        response["structure"]["windows"] = json!([{
            "window_id":"window-1","title":"Window",
            "frame":{"x":0.0,"y":0.0,"width":300.0,"height":100.0},
            "z_index":0,"kind":"window","modal":false,"owner_bundle_id":"test.app"
        }]);
        response["structure"]["global_focus"] = json!({
            "app":{"state":"known","value":{"bundle_id":"test.app","name":"Test","pid":42}},
            "window_id":{"state":"known","value":"window-1"}
        });
        response["structure"]["objects"] = json!([
            {
                "instance_ref":"window-ref","identity_continuity":"read_scoped",
                "parent":null,"window_id":"window-1","role":"AXWindow",
                "subrole":"AXStandardWindow","title":"Window","description":null,
                "value":null,"enabled":true,"focused":false,
                "frame":{"x":0.0,"y":0.0,"width":300.0,"height":100.0},
                "actions":[],"in_view":true,"group":null,"secure":false,"scroll":null
            },
            {
                "instance_ref":"scroll-ref","identity_continuity":"read_scoped",
                "parent":"window-ref","window_id":"window-1","role":"AXScrollArea",
                "subrole":null,"title":null,"description":null,"value":null,
                "enabled":true,"focused":false,
                "frame":{"x":0.0,"y":0.0,"width":300.0,"height":100.0},
                "actions":[],"in_view":true,"group":null,"secure":false,"scroll":null
            },
            {
                "instance_ref":"table-ref","identity_continuity":"read_scoped",
                "parent":"scroll-ref","window_id":"window-1","role":"AXTable",
                "subrole":null,"title":null,"description":null,"value":null,
                "enabled":true,"focused":false,
                "frame":{"x":0.0,"y":0.0,"width":300.0,"height":400.0},
                "actions":[],"in_view":true,"group":null,"secure":false,"scroll":null
            },
            {
                "instance_ref":"row-ref","identity_continuity":"read_scoped",
                "parent":"table-ref","window_id":"window-1","role":"AXRow",
                "subrole":null,"title":null,"description":null,"value":null,
                "enabled":true,"focused":false,
                "frame":{"x":0.0,"y":0.0,"width":300.0,"height":30.0},
                "actions":[],"in_view":true,"group":null,"secure":false,"scroll":null
            },
            {
                "instance_ref":"cell-ref","identity_continuity":"read_scoped",
                "parent":"row-ref","window_id":"window-1","role":"AXCell",
                "subrole":null,"title":null,"description":null,"value":null,
                "enabled":true,"focused":false,
                "frame":{"x":0.0,"y":0.0,"width":150.0,"height":30.0},
                "actions":[],"in_view":true,"group":null,"secure":false,"scroll":null
            },
            {
                "instance_ref":"column-ref","identity_continuity":"read_scoped",
                "parent":"table-ref","window_id":"window-1","role":"AXColumn",
                "subrole":null,"title":"Name","description":null,"value":null,
                "enabled":true,"focused":false,
                "frame":{"x":0.0,"y":0.0,"width":150.0,"height":30.0},
                "actions":[],"in_view":true,"group":null,"secure":false,"scroll":null
            }
        ]);
        response["structure"]["observer_coverage"]["read_scoped_objects"] = json!(6);
        response["structure"]["table_viewports"] = json!([{
            "table_instance_ref":"table-ref",
            "scroll_container_instance_ref":"scroll-ref",
            "process_id":42,
            "table_frame":{"x":0.0,"y":0.0,"width":300.0,"height":400.0},
            "viewport_frame":{"x":0.0,"y":0.0,"width":300.0,"height":100.0},
            "ax_enabled":"enabled",
            "visible_rows_complete":true,"visible_rows":[{
                "instance_ref":"row-ref","parent_instance_ref":"table-ref",
                "frame":{"x":0.0,"y":0.0,"width":300.0,"height":30.0},
                "accessibility_identifier":null,"ax_enabled":"enabled",
                "selected":false,"ax_selected_settable":true,
                "ax_index":42,"cells_complete":true,"cells":[{
                    "instance_ref":"cell-ref","parent_instance_ref":"row-ref",
                    "frame":{"x":0.0,"y":0.0,"width":150.0,"height":30.0},
                    "row_ax_index":42,"column_ax_index":2,
                    "column_instance_ref":"column-ref","header_instance_ref":null,
                    "column_header":"Name","value":{"type":"text","value":"Quarterly report"}
                }]
            }],
            "columns_complete":true,
            "columns":[{
                "instance_ref":"column-ref","parent_instance_ref":"table-ref",
                "frame":{"x":0.0,"y":0.0,"width":150.0,"height":30.0},
                "header_instance_ref":null,"header":"Name","ax_index":2
            }]
        }]);
        response
    }

    fn read_scoped_object() -> Value {
        json!({
            "instance_ref":"read-object-1","identity_continuity":"read_scoped",
            "parent":null,"window_id":"window-1","role":"AXWindow",
            "subrole":"AXStandardWindow","title":"Window","description":null,
            "value":null,"enabled":true,"focused":false,
            "frame":{"x":0.0,"y":0.0,"width":100.0,"height":80.0},
            "actions":[],"in_view":true,"group":null,"secure":false,"scroll":null
        })
    }

    fn window() -> Value {
        json!({
            "window_id":"window-1","title":"Window",
            "frame":{"x":0.0,"y":0.0,"width":100.0,"height":80.0},
            "z_index":0,"kind":"window","modal":false,"owner_bundle_id":"test.app"
        })
    }

    #[test]
    fn native_wire_requires_explicit_object_continuity_and_observer_coverage() {
        let mut response = native_observation(true);
        response["structure"]["windows"] = json!([window()]);
        response["structure"]["objects"] = json!([read_scoped_object()]);
        response["structure"]["observer_coverage"]["read_scoped_objects"] = json!(1);
        assert!(serde_json::from_value::<NativeObservation>(response.clone()).is_ok());

        let mut missing_object_mode = response.clone();
        missing_object_mode["structure"]["objects"][0]
            .as_object_mut()
            .unwrap()
            .remove("identity_continuity");
        assert!(serde_json::from_value::<NativeObservation>(missing_object_mode).is_err());

        response["structure"]
            .as_object_mut()
            .unwrap()
            .remove("observer_coverage");
        assert!(serde_json::from_value::<NativeObservation>(response).is_err());
    }

    #[test]
    fn one_shot_native_observation_accepts_read_scoped_coverage_only() {
        let _lane = HELPER_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mut response = native_observation(true);
        response["structure"]["windows"] = json!([window()]);
        response["structure"]["objects"] = json!([read_scoped_object()]);
        response["structure"]["observer_coverage"]["read_scoped_objects"] = json!(1);
        let (_dir, config) = response_helper(&response);
        let output_dir = config.assets_dir.join("observation-1");
        let report = observe_job(
            &config,
            &Job {
                observe: true,
                observed_ms: 50,
                sequence: 1,
            },
            1,
            "macos/1/wall",
            &output_dir,
            &AtomicBool::new(false),
        )
        .unwrap();
        let snapshot = report.snapshot.unwrap();
        assert_eq!(snapshot.observer_coverage.state, SourceState::Unavailable);
        assert_eq!(snapshot.observer_coverage.read_scoped_objects, 1);
        assert_eq!(
            snapshot.objects[0].identity_continuity,
            ObjectIdentityContinuity::ReadScoped
        );
        assert_eq!(
            report
                .health
                .iter()
                .find(|item| item.source == SourceKind::AxObserver)
                .unwrap()
                .state,
            SourceState::Unavailable
        );

        response["structure"]["objects"][0]["identity_continuity"] = json!("notification_covered");
        response["structure"]["observer_coverage"] = json!({
            "state":"available","covered_objects":1,"read_scoped_objects":0,
            "failed_registrations":0,"failures":[]
        });
        let (_dir, config) = response_helper(&response);
        let output_dir = config.assets_dir.join("observation-1");
        assert!(
            observe_job(
                &config,
                &Job {
                    observe: true,
                    observed_ms: 50,
                    sequence: 1
                },
                1,
                "macos/1/wall",
                &output_dir,
                &AtomicBool::new(false),
            )
            .unwrap_err()
            .contains("one-shot native observations cannot claim persistent identity")
        );
    }

    #[test]
    fn native_projection_preserves_actual_global_focus_without_active_layer() {
        let _lane = HELPER_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mut response = native_observation(true);
        response["structure"]["windows"] = json!([window()]);
        response["structure"]["objects"] = json!([read_scoped_object()]);
        response["structure"]["observer_coverage"]["read_scoped_objects"] = json!(1);
        response["structure"]["active_layer"] = Value::Null;
        response["structure"]["global_focus"] = json!({
            "app":{"state":"known","value":{"bundle_id":"test.app","name":"Test","pid":42}},
            "window_id":{"state":"known","value":"window-1"}
        });
        let (_dir, config) = response_helper(&response);
        let report = observe_job(
            &config,
            &Job {
                observe: true,
                observed_ms: 50,
                sequence: 1,
            },
            1,
            "macos/1/wall",
            &config.assets_dir.join("observation-1"),
            &AtomicBool::new(false),
        )
        .unwrap();
        let snapshot = report.snapshot.unwrap();
        assert_eq!(
            snapshot.global_focus.known_window_for_app(&snapshot.app),
            Some("window-1")
        );

        response["structure"]["global_focus"]["window_id"]["value"] = json!("window-missing");
        let (_dir, config) = response_helper(&response);
        let error = observe_job(
            &config,
            &Job {
                observe: true,
                observed_ms: 50,
                sequence: 1,
            },
            1,
            "macos/1/wall",
            &config.assets_dir.join("observation-1"),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(error.contains("global focus window does not resolve"));
    }

    #[test]
    fn native_table_viewport_preserves_actual_row_and_cell_indexes() {
        let _lane = HELPER_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mut response = table_viewport_observation();
        let (_dir, config) = response_helper(&response);
        let output_dir = config.assets_dir.join("table-index-1");
        let report = observe_job(
            &config,
            &Job {
                observe: true,
                observed_ms: 50,
                sequence: 1,
            },
            1,
            "macos/1/wall",
            &output_dir,
            &AtomicBool::new(false),
        )
        .unwrap();
        let snapshot = report.snapshot.unwrap();
        let row = &snapshot.table_viewports[0].visible_rows[0];
        assert_eq!(
            snapshot.table_viewports[0].ax_enabled,
            AxEnabledObservation::Enabled
        );
        assert_eq!(row.ax_enabled, AxEnabledObservation::Enabled);
        assert_eq!(row.ax_index, Some(42));
        assert_eq!(row.cells[0].row_ax_index, Some(42));
        assert_eq!(row.cells[0].column_ax_index, Some(2));
        assert_eq!(snapshot.table_viewports[0].columns[0].ax_index, Some(2));

        for (target, field) in [
            ("row", "ax_index"),
            ("cell", "row_ax_index"),
            ("cell", "column_ax_index"),
        ] {
            for (case, omit_field) in [("null", false), ("omitted", true)] {
                response = table_viewport_observation();
                let object = if target == "row" {
                    &mut response["structure"]["table_viewports"][0]["visible_rows"][0]
                } else {
                    &mut response["structure"]["table_viewports"][0]["visible_rows"][0]["cells"][0]
                };
                if omit_field {
                    object.as_object_mut().unwrap().remove(field);
                } else {
                    object[field] = Value::Null;
                }
                if field == "column_ax_index" {
                    let cell = &mut response["structure"]["table_viewports"][0]["visible_rows"][0]
                        ["cells"][0];
                    cell["column_instance_ref"] = Value::Null;
                    cell["column_header"] = Value::Null;
                }
                let (_dir, config) = response_helper(&response);
                let output_dir = config
                    .assets_dir
                    .join(format!("table-index-{target}-{field}-{case}"));
                let report = observe_job(
                    &config,
                    &Job {
                        observe: true,
                        observed_ms: 50,
                        sequence: 1,
                    },
                    1,
                    "macos/1/wall",
                    &output_dir,
                    &AtomicBool::new(false),
                )
                .unwrap();
                let snapshot = report.snapshot.unwrap();
                let row = &snapshot.table_viewports[0].visible_rows[0];
                assert_eq!(row.ax_index, if target == "row" { None } else { Some(42) });
                assert_eq!(
                    row.cells[0].row_ax_index,
                    if field == "row_ax_index" {
                        None
                    } else {
                        Some(42)
                    }
                );
                assert_eq!(
                    row.cells[0].column_ax_index,
                    if field == "column_ax_index" {
                        None
                    } else {
                        Some(2)
                    }
                );
            }
        }

        for (target, field, invalid) in [
            ("row", "ax_index", json!(true)),
            ("row", "ax_index", json!(42.5)),
            ("row", "ax_index", json!(u64::from(u32::MAX) + 1)),
            ("cell", "row_ax_index", json!(true)),
            ("cell", "row_ax_index", json!(42.5)),
            ("cell", "row_ax_index", json!(u64::from(u32::MAX) + 1)),
            ("cell", "column_ax_index", json!(true)),
            ("cell", "column_ax_index", json!(2.5)),
            ("cell", "column_ax_index", json!(64)),
        ] {
            response = table_viewport_observation();
            let object = if target == "row" {
                &mut response["structure"]["table_viewports"][0]["visible_rows"][0]
            } else {
                &mut response["structure"]["table_viewports"][0]["visible_rows"][0]["cells"][0]
            };
            object[field] = invalid;
            let (_dir, config) = response_helper(&response);
            let output_dir = config
                .assets_dir
                .join(format!("table-index-invalid-{target}-{field}"));
            assert!(
                observe_job(
                    &config,
                    &Job {
                        observe: true,
                        observed_ms: 50,
                        sequence: 1
                    },
                    1,
                    "macos/1/wall",
                    &output_dir,
                    &AtomicBool::new(false),
                )
                .is_err(),
                "invalid {target}.{field} must reject native JSON"
            );
        }

        response = table_viewport_observation();
        response["structure"]["table_viewports"][0]["columns"][0]["ax_index"] = json!(64);
        let (_dir, config) = response_helper(&response);
        let output_dir = config.assets_dir.join("table-index-invalid");
        let error = observe_job(
            &config,
            &Job {
                observe: true,
                observed_ms: 50,
                sequence: 1,
            },
            1,
            "macos/1/wall",
            &output_dir,
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(error.contains("invalid native structure"), "{error}");
    }

    #[test]
    fn native_table_and_row_ax_enabled_states_are_mandatory_and_roundtrip() {
        let _lane = HELPER_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        for (wire, generic_enabled, expected) in [
            ("enabled", true, AxEnabledObservation::Enabled),
            ("disabled", false, AxEnabledObservation::Disabled),
            (
                "attribute_unsupported",
                false,
                AxEnabledObservation::AttributeUnsupported,
            ),
            ("unavailable", false, AxEnabledObservation::Unavailable),
        ] {
            let mut response = table_viewport_observation();
            response["structure"]["objects"][2]["enabled"] = json!(generic_enabled);
            response["structure"]["objects"][3]["enabled"] = json!(generic_enabled);
            response["structure"]["table_viewports"][0]["ax_enabled"] = json!(wire);
            response["structure"]["table_viewports"][0]["visible_rows"][0]["ax_enabled"] =
                json!(wire);
            let (_dir, config) = response_helper(&response);
            let output_dir = config.assets_dir.join(format!("table-ax-enabled-{wire}"));
            let report = observe_job(
                &config,
                &Job {
                    observe: true,
                    observed_ms: 50,
                    sequence: 1,
                },
                1,
                "macos/1/wall",
                &output_dir,
                &AtomicBool::new(false),
            )
            .unwrap();
            let snapshot = report.snapshot.unwrap();
            assert_eq!(
                snapshot.table_viewports[0].ax_enabled, expected,
                "table {wire}"
            );
            assert_eq!(
                snapshot.table_viewports[0].visible_rows[0].ax_enabled, expected,
                "row {wire}"
            );
        }

        for (scope, missing_field) in [("table", "ax_enabled"), ("row", "ax_enabled")] {
            let mut response = table_viewport_observation();
            let record = if scope == "table" {
                &mut response["structure"]["table_viewports"][0]
            } else {
                &mut response["structure"]["table_viewports"][0]["visible_rows"][0]
            };
            record.as_object_mut().unwrap().remove(missing_field);
            assert!(
                serde_json::from_value::<NativeObservation>(response).is_err(),
                "missing table viewport {scope}.{missing_field} must be rejected"
            );
        }

        let mut unknown = table_viewport_observation();
        unknown["structure"]["table_viewports"][0]["visible_rows"][0]["ax_enabled"] =
            json!("maybe");
        assert!(
            serde_json::from_value::<NativeObservation>(unknown).is_err(),
            "unknown AXEnabled state must be rejected"
        );
    }

    fn response_helper(value: &Value) -> (tempfile::TempDir, NativeConfig) {
        let (dir, config) = helper(
            "IFS= read -r response < \"$0.response\" || :\nprintf '%s' \"$response\"",
            FIXTURE_TIMEOUT_MS,
        );
        fs::write(
            config.helper_path.with_extension("response"),
            serde_json::to_vec(value).unwrap(),
        )
        .unwrap();
        (dir, config)
    }

    #[test]
    fn native_table_census_must_match_the_exact_requested_scope_process_and_window() {
        let (_dir, mut config) = helper("exit 0", FIXTURE_TIMEOUT_MS);
        let scope = serde_json::from_value(json!({
            "scroll_path":[{"role":"ax_scroll_area", "label":"Archive"}],
            "column_headers":["Name"]
        }))
        .unwrap();
        let census = TableScopeSearch {
            requested_scope: scope,
            process_id: 42,
            window_id: "window-7".into(),
            state: interaction_contracts::desktop::table_scope::TableScopeSearchState::Complete,
            candidates: vec![],
        };
        assert!(validate_requested_table_scope(&config, None).is_ok());
        assert!(validate_requested_table_scope(&config, Some(&census)).is_err());
        config.table_scope = Some(census.requested_scope.clone());
        config.pid = Some(42);
        config.text_policy = Some(crate::config::TextReadPolicy::RegisteredEditor {
            window_id: "window-7".into(),
        });
        assert!(validate_requested_table_scope(&config, Some(&census)).is_ok());
        assert!(validate_requested_table_scope(&config, None).is_ok());
        for changed in ["scope", "pid", "window"] {
            let mut wrong = census.clone();
            match changed {
                "scope" => wrong.requested_scope.scroll_path[0].label = "Other".into(),
                "pid" => wrong.process_id = 43,
                _ => wrong.window_id = "window-8".into(),
            }
            assert!(
                validate_requested_table_scope(&config, Some(&wrong)).is_err(),
                "{changed}"
            );
        }
        config.text_policy = None;
        assert!(validate_requested_table_scope(&config, Some(&census)).is_err());
    }

    #[test]
    fn unsolicited_table_census_is_rejected_before_native_snapshot_projection() {
        let _lane = HELPER_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mut response = observation(true);
        response["structure"]["table_scope_search"] = json!({
            "requested_scope": {"scroll_path":[{"role":"ax_scroll_area", "label":"Archive"}], "column_headers":["Name"]},
            "process_id":42, "window_id":"window-1",
            "state":{"state":"complete"}, "candidates":[]
        });
        let (_dir, config) = response_helper(&response);
        let error = observe_job(
            &config,
            &Job {
                observe: true,
                observed_ms: 42,
                sequence: 7,
            },
            3,
            "macos/3/wall",
            &config.assets_dir.join("observation-7"),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(error.contains("native table census was not requested"));
    }

    #[test]
    fn native_projection_preserves_same_read_census_without_upgrading_partial_structure() {
        let _lane = HELPER_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mut response = table_viewport_observation();
        response["structure"]["active_layer"] = json!("window-1");
        response["structure"]["complete"] = json!(false);
        response["structure"]["incomplete_reason"] = json!("AX node budget exceeded");
        response["structure"]["objects"][1]["description"] = json!("Archive");
        let scope = json!({"scroll_path":[{"role":"ax_scroll_area", "label":"Archive"}], "column_headers":["Name"]});
        response["structure"]["table_scope_search"] = json!({
            "requested_scope": scope,
            "process_id":42, "window_id":"window-1",
            "state":{"state":"complete"},
            "candidates":[{
                "table_instance_ref":"table-ref", "scroll_container_instance_ref":"scroll-ref",
                "scroll_path":[{"instance_ref":"scroll-ref", "label":"Archive", "frame":{"x":0.0,"y":0.0,"width":300.0,"height":100.0}}],
                "column_headers":["Name"]
            }]
        });
        let (_dir, mut config) = response_helper(&response);
        config.pid = Some(42);
        config.text_policy = Some(crate::config::TextReadPolicy::RegisteredEditor {
            window_id: "window-1".into(),
        });
        config.table_scope = Some(serde_json::from_value(scope).unwrap());
        let report = observe_job(
            &config,
            &Job {
                observe: true,
                observed_ms: 42,
                sequence: 7,
            },
            3,
            "macos/3/wall",
            &config.assets_dir.join("observation-7"),
            &AtomicBool::new(false),
        )
        .unwrap();
        let snapshot = report.snapshot.unwrap();
        assert!(!snapshot.complete);
        assert_eq!(snapshot.observed_ms, 42);
        assert_eq!(snapshot.source_sequence, 7);
        assert_eq!(
            snapshot
                .table_scope_search
                .unwrap()
                .unique_candidate()
                .unwrap()
                .table_instance_ref
                .as_str(),
            "table-ref"
        );
    }

    #[test]
    fn native_projection_preserves_context_and_denied_permissions_make_it_incomplete() {
        let _lane = HELPER_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (_dir, config) = response_helper(&observation(false));
        let output = config.assets_dir.join("observation-7");
        let report = observe_job(
            &config,
            &Job {
                observe: true,
                observed_ms: 42,
                sequence: 7,
            },
            3,
            "macos/3/wall",
            &output,
            &AtomicBool::new(false),
        )
        .unwrap();
        let snapshot = report.snapshot.unwrap();
        assert_eq!(snapshot.session_epoch, 3);
        assert_eq!(snapshot.surface.epoch, 3);
        assert_eq!(snapshot.source_sequence, 7);
        assert_eq!(snapshot.clock_domain, "macos/3/wall");
        assert_eq!(
            snapshot.observed_ms, 42,
            "helper completion never refreshes source age"
        );
        assert!(!snapshot.complete);
        assert!(
            snapshot
                .incomplete_reason
                .unwrap()
                .contains("Accessibility")
        );
        assert!(
            report
                .health
                .iter()
                .all(|health| health.state == SourceState::Unavailable)
        );
        let manifest = report.manifest.unwrap();
        assert_eq!(
            fs::metadata(manifest).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(output).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn a_missing_or_different_native_target_never_produces_a_snapshot() {
        let _lane = HELPER_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        for bad_target in [json!(0), json!(99)] {
            let mut response = observation(true);
            response["structure"]["app"]["pid"] = bad_target;
            let (_dir, mut config) = response_helper(&response);
            config.pid = Some(42);
            assert!(
                observe_job(
                    &config,
                    &Job {
                        observe: true,
                        observed_ms: 42,
                        sequence: 7
                    },
                    3,
                    "macos/3/wall",
                    &config.assets_dir.join("observation-7"),
                    &AtomicBool::new(false)
                )
                .is_err()
            );
        }
    }

    fn await_report(worker: &mut NativeWorker) -> Result<NativeReport, String> {
        let deadline = Instant::now() + Duration::from_millis(FIXTURE_TIMEOUT_MS + 2_000);
        loop {
            if let Some(report) = worker.take_report() {
                return report;
            }
            assert!(Instant::now() < deadline, "mock native job completed");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn native_retention_and_failed_job_cleanup_bound_assets() {
        let _lane = HELPER_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (_dir, config) = response_helper(&observation(true));
        let mut worker = NativeWorker::new(config.clone(), 1, "macos/1/wall".into()).unwrap();
        for sequence in 1..=12 {
            assert!(worker.request(true, sequence, sequence));
            assert!(await_report(&mut worker).unwrap().snapshot.is_some());
        }
        let run = fs::read_dir(&config.assets_dir)
            .unwrap()
            .filter_map(Result::ok)
            .find(|entry| entry.file_type().unwrap().is_dir())
            .unwrap()
            .path();
        assert_eq!(fs::read_dir(&run).unwrap().count(), RETAINED_OBSERVATIONS);
        fs::write(config.helper_path.with_extension("response"), "not json").unwrap();
        assert!(worker.request(true, 13, 13));
        assert!(await_report(&mut worker).is_err());
        assert!(!run.join("observation-13").exists());
        assert_eq!(fs::read_dir(&run).unwrap().count(), RETAINED_OBSERVATIONS);
    }
}
