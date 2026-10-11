//! Persistent SCStream session. Parsing and native IO run outside the source
//! loop; only a bounded latest-state mailbox crosses into the control loop.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use desktop_ipc::{SharedClock, socket};
use interaction_contracts::desktop::source::{
    FrameMeta, FrameStatus, SourceCommand, SourceHealth, SourceKind, SourceState, StructureSnapshot,
};
use serde_json::json;

use crate::config::{NativeConfig, write_owned_document_child_prefix};
use crate::native::{MAX_DIAGNOSTICS, MAX_HELPER_OUTPUT, private_json};

pub mod tracker;
use tracker::WatchTracker;

const DIAGNOSTICS_FILE: &str = "native-diagnostics.json";
const DIAGNOSTICS_TEMP: &str = "native-diagnostics.json.tmp";

/// This value never enters a WatchReport, Source health or an input proof.
#[derive(Default)]
struct DiagnosticBytes {
    prefix: Vec<u8>,
    drained_bytes: u64,
}

impl DiagnosticBytes {
    fn retain(&mut self, chunk: &[u8]) -> bool {
        let was_truncated = self.drained_bytes > self.prefix.len() as u64;
        self.drained_bytes = self.drained_bytes.saturating_add(chunk.len() as u64);
        let kept = chunk
            .len()
            .min(MAX_DIAGNOSTICS.saturating_sub(self.prefix.len()));
        self.prefix.extend_from_slice(&chunk[..kept]);
        kept > 0 || !was_truncated && self.drained_bytes > self.prefix.len() as u64
    }

    fn snapshot(&self, complete: bool, read_failed: bool) -> serde_json::Value {
        json!({
            "kind": "native_private_diagnostics",
            "raw_stderr": String::from_utf8_lossy(&self.prefix),
            "max_retained_bytes": MAX_DIAGNOSTICS,
            "retained_bytes": self.prefix.len(),
            "drained_bytes": self.drained_bytes,
            "discarded_bytes": self.drained_bytes.saturating_sub(self.prefix.len() as u64),
            "truncated": self.drained_bytes > self.prefix.len() as u64,
            "complete": complete,
            "read_failed": read_failed,
        })
    }
}

fn create_diagnostic_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}

/// Keep the published file handle and directory identity for this generation.
/// Every replacement uses a new private regular file and an atomic rename.
struct DiagnosticSink {
    directory: PathBuf,
    directory_identity: (u64, u64),
    published: File,
    writable: bool,
}

fn regular_file_matches(path: &Path, file: &File) -> io::Result<bool> {
    let path_meta = fs::symlink_metadata(path)?;
    let held_meta = file.metadata()?;
    Ok(path_meta.is_file()
        && path_meta.dev() == held_meta.dev()
        && path_meta.ino() == held_meta.ino())
}

impl DiagnosticSink {
    fn new(directory: &Path) -> io::Result<Self> {
        let metadata = fs::symlink_metadata(directory)?;
        if !metadata.is_dir() || metadata.mode() & 0o777 != 0o700 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "diagnostics require the owned private watch directory",
            ));
        }
        let mut published = create_diagnostic_file(&directory.join(DIAGNOSTICS_FILE))?;
        serde_json::to_writer(
            &mut published,
            &DiagnosticBytes::default().snapshot(false, false),
        )?;
        Ok(Self {
            directory: directory.to_path_buf(),
            directory_identity: (metadata.dev(), metadata.ino()),
            published,
            writable: true,
        })
    }

    fn publish(
        &mut self,
        bytes: &DiagnosticBytes,
        complete: bool,
        read_failed: bool,
    ) -> io::Result<()> {
        let metadata = fs::symlink_metadata(&self.directory)?;
        let target = self.directory.join(DIAGNOSTICS_FILE);
        if !metadata.is_dir()
            || (metadata.dev(), metadata.ino()) != self.directory_identity
            || !regular_file_matches(&target, &self.published)?
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the owned diagnostics path was replaced",
            ));
        }
        let temporary = self.directory.join(DIAGNOSTICS_TEMP);
        let mut replacement = create_diagnostic_file(&temporary)?;
        let result = (|| {
            serde_json::to_writer(&mut replacement, &bytes.snapshot(complete, read_failed))?;
            replacement.flush()?;
            if !regular_file_matches(&temporary, &replacement)?
                || !regular_file_matches(&target, &self.published)?
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "a diagnostics file was replaced",
                ));
            }
            fs::rename(&temporary, &target)?;
            Ok(())
        })();
        if result.is_err() && regular_file_matches(&temporary, &replacement).unwrap_or(false) {
            let _ = fs::remove_file(&temporary);
        }
        result?;
        self.published = replacement;
        Ok(())
    }

    fn retain_snapshot(&mut self, bytes: &DiagnosticBytes, complete: bool, read_failed: bool) {
        if self.writable && self.publish(bytes, complete, read_failed).is_err() {
            // Private diagnostic failure never invalidates application evidence.
            // Keep draining, and retain the last complete JSON file and handle.
            self.writable = false;
        }
    }
}

fn drain_diagnostics(input: &mut impl Read, mut sink: Option<DiagnosticSink>) -> DiagnosticBytes {
    let mut bytes = DiagnosticBytes::default();
    let mut buffer = [0u8; 4_096];
    let read_failed = loop {
        match input.read(&mut buffer) {
            Ok(0) => break false,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break true,
            Ok(count) => {
                if bytes.retain(&buffer[..count])
                    && let Some(sink) = sink.as_mut()
                {
                    sink.retain_snapshot(&bytes, false, false);
                }
                // Once the prefix and first truncation flag are saved, the
                // remaining stream is only drained, without per-chunk file IO.
            }
        }
    };
    if let Some(sink) = sink.as_mut() {
        sink.retain_snapshot(&bytes, true, read_failed);
    }
    bytes
}

#[derive(Debug, Clone)]
pub struct WatchReport {
    pub stream_id: String,
    pub sequence: u64,
    pub frame_meta: Option<FrameMeta>,
    pub image: Option<Arc<[u8]>>,
    pub snapshot: Option<StructureSnapshot>,
    pub health: Vec<SourceHealth>,
    pub manifest: Option<PathBuf>,
    pub capture_failure: Option<CaptureFailure>,
}

#[derive(Debug, Clone)]
pub struct CaptureFailure {
    pub request_id: interaction_contracts::desktop::ids::RequestId,
    pub watch_id: String,
    pub code: String,
}

impl WatchReport {
    pub(super) fn new(stream_id: String, sequence: u64) -> Self {
        Self {
            stream_id,
            sequence,
            frame_meta: None,
            image: None,
            snapshot: None,
            health: Vec::new(),
            manifest: None,
            capture_failure: None,
        }
    }
}

#[derive(Default)]
struct Mailbox {
    // First COMPLETE must survive a burst of IDLE records. There are at most
    // two pending frame events, one AX result, and two health states per source.
    // A loss must survive subsequent recovery, including health embedded in an
    // overwritten frame or AX result. Keeping the most recent loss also keeps
    // its original recovery floor when several loss/recovery cycles coalesce.
    complete: Option<WatchReport>,
    frame: Option<WatchReport>,
    snapshot: Option<WatchReport>,
    health: [Option<WatchReport>; 4],
    negative: [Option<WatchReport>; 4],
    terminal_report: Option<WatchReport>,
    discarded: u64,
    coalesced: u64,
    terminal: bool,
    capture: Option<WatchReport>,
}

impl Mailbox {
    fn publish(&mut self, mut report: WatchReport) {
        if self.terminal {
            return;
        }
        for health in std::mem::take(&mut report.health) {
            let index = match health.source {
                SourceKind::Screenshot => 0,
                SourceKind::Ax => 1,
                SourceKind::Ocr => 2,
                SourceKind::AxObserver => 3,
            };
            let failed = health.state != SourceState::Available;
            let mut status = WatchReport::new(report.stream_id.clone(), report.sequence);
            status.health.push(health);
            status.manifest = report.manifest.clone();
            if failed {
                self.negative[index] = Some(status.clone());
            }
            if self.health[index].replace(status).is_some() {
                self.coalesced = self.coalesced.saturating_add(1);
            }
        }
        let important = report
            .frame_meta
            .as_ref()
            .is_some_and(|frame| frame.status == FrameStatus::Complete)
            || report.snapshot.is_some();
        let slot = if report.capture_failure.is_some()
            || report
                .frame_meta
                .as_ref()
                .is_some_and(|frame| frame.capture_info().is_some())
        {
            &mut self.capture
        } else if report
            .frame_meta
            .as_ref()
            .is_some_and(|frame| frame.status == FrameStatus::Complete)
        {
            &mut self.complete
        } else if report.frame_meta.is_some() {
            &mut self.frame
        } else if report.snapshot.is_some() {
            &mut self.snapshot
        } else {
            return;
        };
        if slot.replace(report).is_some() {
            if important {
                self.discarded = self.discarded.saturating_add(1);
            } else {
                self.coalesced = self.coalesced.saturating_add(1);
            }
        }
    }

    fn fail(&mut self, report: WatchReport) {
        self.complete = None;
        self.frame = None;
        self.snapshot = None;
        self.capture = None;
        self.negative = Default::default();
        self.health = Default::default();
        self.terminal_report = Some(report);
        self.terminal = true;
    }

    fn take(&mut self) -> Vec<WatchReport> {
        let mut pending = [
            self.complete.take(),
            self.frame.take(),
            self.snapshot.take(),
            self.capture.take(),
            self.terminal_report.take(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        pending.extend(self.health.iter_mut().filter_map(Option::take));
        pending.extend(self.negative.iter_mut().filter_map(Option::take));
        pending.sort_by_key(|report| report.sequence);
        let mut reports: Vec<WatchReport> = Vec::new();
        for report in pending {
            if let Some(previous) = reports.last_mut()
                && previous.stream_id == report.stream_id
                && previous.sequence == report.sequence
            {
                // One native record can occupy a body slot and several health
                // slots. Reunite it so no source receives its stamp twice.
                for health in report.health {
                    if !previous
                        .health
                        .iter()
                        .any(|old| old.source == health.source)
                    {
                        previous.health.push(health);
                    }
                }
                if previous.frame_meta.is_none() {
                    previous.frame_meta = report.frame_meta;
                }
                if previous.image.is_none() {
                    previous.image = report.image;
                }
                if previous.snapshot.is_none() {
                    previous.snapshot = report.snapshot;
                }
                if previous.manifest.is_none() {
                    previous.manifest = report.manifest;
                }
                if previous.capture_failure.is_none() {
                    previous.capture_failure = report.capture_failure;
                }
            } else {
                reports.push(report);
            }
        }
        for report in &mut reports {
            if let Some(stream) = report
                .frame_meta
                .as_mut()
                .and_then(|frame| frame.stream_info_mut())
            {
                stream.dropped_frames = stream.dropped_frames.saturating_add(self.discarded);
            }
        }
        reports
    }
}

pub struct NativeWatch {
    mailbox: Arc<Mutex<Mailbox>>,
    cancel: Arc<AtomicBool>,
    ended: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    run_dir: PathBuf,
    watch_id: String,
    controls: Arc<Mutex<WatchControls>>,
    supports_capture: bool,
}

#[derive(Default)]
struct WatchControls {
    capture: Option<SourceCommand>,
    cancel: Option<SourceCommand>,
}

impl NativeWatch {
    pub fn new(
        config: NativeConfig,
        session_epoch: u64,
        clock_domain: String,
        clock: SharedClock,
        interval_ms: u64,
    ) -> Result<Self, String> {
        socket::private_dir(&config.assets_dir)
            .map_err(|error| format!("watch assets: {error}"))?;
        let run_dir = config.assets_dir.join(format!(
            "watch-{}",
            socket::new_token().map_err(|error| error.to_string())?
        ));
        socket::private_dir(&run_dir).map_err(|error| format!("watch run directory: {error}"))?;
        let mailbox = Arc::new(Mutex::new(Mailbox::default()));
        let cancel = Arc::new(AtomicBool::new(false));
        let ended = Arc::new(AtomicBool::new(false));
        let output = mailbox.clone();
        let cancelled = cancel.clone();
        let finished = ended.clone();
        let assets = run_dir.clone();
        let watch_id = socket::new_token().map_err(|error| error.to_string())?;
        let expected_watch = watch_id.clone();
        let controls = Arc::new(Mutex::new(WatchControls::default()));
        let supports_capture = config.owned_document_bootstrap.is_none();
        let control_queue = controls.clone();
        let worker = std::thread::Builder::new()
            .name("source-native-watch".into())
            .spawn(move || {
                let mut tracker = WatchTracker::new(
                    config.clone(),
                    session_epoch,
                    clock_domain,
                    clock.origin_unix_ms(),
                    interval_ms,
                );
                if config.owned_document_bootstrap.is_none() {
                    tracker.bind_watch(expected_watch);
                }
                let result = watch_process(
                    &config,
                    &assets,
                    clock,
                    &cancelled,
                    &output,
                    &mut tracker,
                    &control_queue,
                );
                if !cancelled.load(Ordering::Acquire) {
                    let reason = result
                        .err()
                        .unwrap_or_else(|| "the native watch channel closed".into());
                    let report = tracker.failure_report(&reason, unix_ms());
                    output
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .fail(report);
                }
                finished.store(true, Ordering::Release);
            })
            .map_err(|error| format!("native watch worker: {error}"))?;
        Ok(Self {
            mailbox,
            cancel,
            ended,
            worker: Some(worker),
            run_dir,
            watch_id,
            controls,
            supports_capture,
        })
    }

    pub fn take_reports(&self) -> Vec<WatchReport> {
        self.mailbox
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }

    pub fn ended(&self) -> bool {
        self.ended.load(Ordering::Acquire)
    }

    pub fn request_capture(&self, command: SourceCommand) -> Result<(), String> {
        command.validate()?;
        let SourceCommand::CaptureNow { watch_id, .. } = &command else {
            return Err("capture command required".into());
        };
        if !self.supports_capture || watch_id != &self.watch_id || self.ended() {
            return Err("capture watch is unavailable".into());
        }
        let mut controls = self.controls.lock().unwrap();
        if controls.capture.is_some() {
            return Err("capture queue is busy".into());
        }
        controls.capture = Some(command);
        Ok(())
    }

    pub fn cancel_capture(
        &self,
        request_id: &interaction_contracts::desktop::ids::RequestId,
        watch_id: &str,
    ) {
        if watch_id != self.watch_id {
            return;
        }
        let mut controls = self.controls.lock().unwrap();
        if controls.capture.as_ref().is_some_and(|command|
            matches!(command, SourceCommand::CaptureNow { request_id: id, .. } if id == request_id)) {
            controls.capture = None;
        }
        controls.cancel = Some(SourceCommand::CancelCapture {
            request_id: request_id.clone(),
            watch_id: watch_id.into(),
        });
        let mut mailbox = self.mailbox.lock().unwrap();
        if mailbox.capture.as_ref().is_some_and(|report| {
            report
                .frame_meta
                .as_ref()
                .and_then(FrameMeta::capture_info)
                .is_some_and(|capture| &capture.request_id == request_id)
                || report
                    .capture_failure
                    .as_ref()
                    .is_some_and(|failure| &failure.request_id == request_id)
        }) {
            mailbox.capture = None;
        }
    }
}

impl Drop for NativeWatch {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        // The source process owns these assets, and failure/restart must not
        // accumulate a new eight-frame directory on every helper generation.
        let _ = fs::remove_dir_all(&self.run_dir);
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

fn watch_process(
    config: &NativeConfig,
    assets: &std::path::Path,
    clock: SharedClock,
    cancel: &AtomicBool,
    mailbox: &Mutex<Mailbox>,
    tracker: &mut WatchTracker,
    controls: &Mutex<WatchControls>,
) -> Result<(), String> {
    let interval_ms = tracker.interval_ms();
    let mut request = json!({"protocol_version":1,"command":"watch","bundle_id":config.bundle_id,
        "pid":config.pid,"output_dir":assets,"timeout_ms":config.timeout_ms,
        "interval_ms":interval_ms,"clock_origin_unix_ms":clock.origin_unix_ms(),"text_policy":config.text_policy,
        "table_scope":config.table_scope,
        "max_nodes":512,"max_depth":12,"max_ocr_blocks":128,"session_epoch":tracker.session_epoch()});
    if let Some(target) = &config.background_target {
        let mut wire = json!({"window_id": target.window_id});
        if let Some(process) = &target.process {
            wire["process"] = serde_json::to_value(process).map_err(|error| error.to_string())?;
        }
        request["background_target"] = wire;
    }
    if config.owned_document_bootstrap.is_none() {
        request["watch_id"] = json!(tracker.bound_watch_id());
        request["surface"] = json!({"id":"main","epoch":tracker.session_epoch()});
    }
    let mut request = serde_json::to_vec(&request).map_err(|error| error.to_string())?;
    // A complete line also lets diagnostic helpers consume the request without
    // relying on EOF timing while other processes are starting concurrently.
    request.push(b'\n');
    if request.len() > 8_192 {
        return Err("native watch request exceeded its byte limit".into());
    }
    // Create the private destination before the helper can populate its assets.
    // Failure disables diagnostics only, never the Source protocol or evidence.
    let diagnostic_sink = DiagnosticSink::new(assets).ok();
    let mut command = Command::new(&config.helper_path);
    if config.owned_document_bootstrap.is_some() {
        command.arg("--owned-document-bootstrap");
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("starting native watch helper: {error}"))?;
    let mut input = child.stdin.take().expect("piped stdin");
    let write = {
        let prefix = config
            .owned_document_bootstrap
            .as_ref()
            .map(|_| write_owned_document_child_prefix(config, &mut input, unix_ms()));
        match prefix {
            Some(Ok(())) | None => input.write_all(&request),
            Some(Err(error)) => Err(io::Error::other(error)),
        }
    };
    if let Err(error) = write {
        terminate(&mut child);
        return Err(format!("native watch request: {error}"));
    }
    // Control writes run on this worker and cannot stall Source's control loop.
    let nonblocking = rustix::fs::fcntl_getfl(&input)
        .and_then(|flags| rustix::fs::fcntl_setfl(&input, flags | rustix::fs::OFlags::NONBLOCK));
    if nonblocking.is_err() {
        terminate(&mut child);
        return Err("native control pipe could not become nonblocking".into());
    }
    let mut input = config.owned_document_bootstrap.is_none().then_some(input);
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    // The reader cannot allocate past a record limit and at most four records
    // wait for checksum/provenance processing. Backpressure is never an
    // unbounded message vector or a thread per captured frame.
    let (records, received) = mpsc::sync_channel(4);
    let reader = std::thread::spawn(move || {
        let mut input = BufReader::with_capacity(8_192, stdout);
        loop {
            let line = read_record(&mut input);
            let final_record = !matches!(line, Ok(Some(_)));
            if records.send(line).is_err() || final_record {
                break;
            }
        }
    });
    let diagnostics = std::thread::spawn(move || {
        let mut input = stderr;
        drain_diagnostics(&mut input, diagnostic_sink)
    });
    let timeout = Duration::from_millis(
        interval_ms
            .saturating_mul(3)
            .max(3_000)
            .max(config.timeout_ms),
    );
    let mut last_record = Instant::now();
    let mut assets_kept = VecDeque::new();
    let result = 'watch: loop {
        if cancel.load(Ordering::Acquire) {
            break Ok(());
        }
        let (cancel_command, capture_command) = {
            let mut queue = controls.lock().unwrap();
            (queue.cancel.take(), queue.capture.take())
        };
        for command in cancel_command.into_iter().chain(capture_command) {
            match tracker.control_record(&command, clock.now_ms()) {
                Ok(Some(record)) => {
                    let Some(input) = input.as_mut() else {
                        break 'watch Err(
                            "native control is unavailable for this watch profile".into()
                        );
                    };
                    let mut bytes =
                        serde_json::to_vec(&record).map_err(|error| error.to_string())?;
                    bytes.push(b'\n');
                    if bytes.len() > 8192 {
                        break 'watch Err("native control exceeds byte budget".into());
                    }
                    let deadline = Instant::now() + Duration::from_millis(50);
                    let mut sent = 0;
                    while sent < bytes.len()
                        && !cancel.load(Ordering::Acquire)
                        && Instant::now() < deadline
                    {
                        match input.write(&bytes[sent..]) {
                            Ok(0) => break,
                            Ok(count) => sent += count,
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                                std::thread::sleep(Duration::from_millis(1))
                            }
                            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                            Err(_) => break,
                        }
                    }
                    if sent != bytes.len() {
                        break 'watch Err(
                            "native capture control pipe failed its bounded write".into()
                        );
                    }
                }
                Ok(None) => {}
                Err(failure) => {
                    let mut report = WatchReport::new(failure.watch_id.clone(), tracker.sequence());
                    report.capture_failure = Some(failure);
                    mailbox.lock().unwrap().publish(report);
                }
            }
        }
        if last_record.elapsed() >= timeout {
            break Err("native watch produced no callback before its deadline".into());
        }
        match received.recv_timeout(Duration::from_millis(10)) {
            Ok(Ok(Some(line))) => {
                let value = match serde_json::from_slice::<serde_json::Value>(&line) {
                    Ok(value) => value,
                    Err(error) => break Err(format!("invalid native NDJSON record: {error}")),
                };
                let asset = value
                    .get("frame")
                    .and_then(|frame| frame.get("png_file"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned);
                let report = match tracker.ingest(value, assets, unix_ms()) {
                    Ok(report) => report,
                    Err(error) => break Err(error),
                };
                if let Some(asset) = asset {
                    assets_kept.push_back(assets.join(asset));
                    while assets_kept.len() > 8 {
                        if let Some(old) = assets_kept.pop_front()
                            && let Err(error) = fs::remove_file(old)
                            && error.kind() != std::io::ErrorKind::NotFound
                        {
                            break 'watch Err(format!("native capture retention failed: {error}"));
                        }
                    }
                }
                if let Some(manifest) = &report.manifest {
                    let temporary = config.assets_dir.join("latest.json.tmp");
                    if let Err(error) = private_json(&temporary, &json!({"manifest":manifest}))
                        .and_then(|_| {
                            fs::rename(&temporary, config.assets_dir.join("latest.json"))
                                .map_err(|error| format!("publishing watch manifest: {error}"))
                        })
                    {
                        break Err(error);
                    }
                }
                mailbox
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .publish(report);
                last_record = Instant::now();
            }
            Ok(Ok(None)) => {
                break Err(tracker.closed_channel_reason("the native watch stdout channel closed"));
            }
            Ok(Err(error)) => break Err(error),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                break Err(tracker.closed_channel_reason("the native watch reader exited"));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => match child.try_wait() {
                Ok(Some(status)) => {
                    let reason = format!("native watch helper exited with {status}");
                    break Err(if status.success() {
                        tracker.closed_channel_reason(&reason)
                    } else {
                        reason
                    });
                }
                Err(error) => break Err(format!("native watch helper status: {error}")),
                Ok(None) => {}
            },
        }
    };
    terminate(&mut child);
    // Drop the receiver first: a full bounded queue must not trap its reader
    // during cancellation. The trusted native helper never forks descendants.
    drop(received);
    let _ = reader.join();
    let _ = diagnostics.join();
    result
}

fn terminate(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn read_record<R: BufRead>(input: &mut R) -> Result<Option<Vec<u8>>, String> {
    let mut record = Vec::new();
    loop {
        let chunk = input
            .fill_buf()
            .map_err(|error| format!("native NDJSON read: {error}"))?;
        if chunk.is_empty() {
            return if record.is_empty() {
                Ok(None)
            } else {
                Err("native NDJSON ended inside a record".into())
            };
        }
        let end = chunk
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|position| position + 1)
            .unwrap_or(chunk.len());
        if record.len() + end > MAX_HELPER_OUTPUT {
            return Err("native NDJSON record exceeded its byte limit".into());
        }
        record.extend_from_slice(&chunk[..end]);
        let complete = chunk[end - 1] == b'\n';
        input.consume(end);
        if complete {
            if record.len() == 1 {
                return Err("native NDJSON contains an empty record".into());
            }
            return Ok(Some(record));
        }
    }
}

#[cfg(test)]
mod tests;
