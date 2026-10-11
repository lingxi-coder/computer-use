//! What the supervisor tells cu-device when it starts it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use desktop_ipc::SharedClock;
use desktop_ipc::browser_control::BrowserControlSession;
use interaction_contracts::desktop::browser::{BrowserPageProfile, BrowserReadScope};
use interaction_contracts::desktop::handshake::Configure;
use interaction_contracts::desktop::owned_registration::OwnedDocumentRegistration;
use interaction_contracts::desktop::source::KernelProcessIdentity;

use crate::owned_document_bootstrap::OwnedDocumentBootstrap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceBackendMode {
    Simulated,
    /// M2's observation entry point. Input stays unavailable until the native
    /// stop, hit-test, human-input and permission guards are validated.
    MacosObserve,
    /// Explicit trusted-launcher mode, restricted to one registered app/PID.
    MacosAxPress,
    /// Main-authorized background AXPress for one fixed process/window birth.
    MacosBackgroundAx,
    /// Main-authorized private-registration owned-text actions; Save is separate.
    MacosOwnedValue,
    /// Main-prepared owned browser fixture with a completed private fd3 handshake.
    HeadlessBrowser,
}

#[derive(Debug)]
pub struct DeviceConfig {
    pub backend: DeviceBackendMode,
    /// Private directory holding the sockets and the world log.
    pub socket_dir: PathBuf,
    /// The host's one-time credential, delivered over the private pipe.
    pub host_token: String,
    pub session_epoch: u64,
    pub clock: SharedClock,
    pub world_log: PathBuf,
    /// An explicitly selected native guard executable. Missing or failed
    /// guards keep execution unavailable; passive BackgroundAx may omit it.
    pub native_guard_path: Option<PathBuf>,
    pub native_target: Option<(u32, String)>,
    pub native_target_window: Option<String>,
    /// Kernel start identity granted with a scoped macOS AX target. This is
    /// accepted only for an explicit process, bundle, and window scope.
    pub native_target_process: Option<KernelProcessIdentity>,
    /// Private owned-document identity, retained only from the startup pipe.
    pub owned_document_registration: Option<Box<OwnedDocumentRegistration>>,
    /// Separate Main-issued Save grant. Omitted Configure settings keep Save off.
    pub owned_save_enabled: bool,
    /// Exact Main-prepared browser scope; it configures Core but grants no permit.
    pub(crate) browser_scope: Option<Box<BrowserReadScope>>,
    /// Authenticated private fd3 channel, consumed by the one Device backend.
    pub(crate) browser_control_session: Option<BrowserControlSession>,
}

fn setting<'a>(settings: &'a BTreeMap<String, String>, key: &str) -> Result<&'a str, String> {
    settings
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| format!("the supervisor did not give {key}"))
}

impl DeviceConfig {
    pub fn from_configure(configure: &Configure) -> Result<Self, String> {
        Self::from_configure_with_startup_bootstraps(configure, None, None)
    }

    /// Attach Main's private registration after parsing ordinary Configure
    /// settings. The registration cannot select an execution backend or target.
    pub fn from_configure_with_owned_bootstrap(
        configure: &Configure,
        bootstrap: Option<OwnedDocumentBootstrap>,
    ) -> Result<Self, String> {
        Self::from_configure_with_startup_bootstraps(configure, bootstrap, None)
    }

    pub fn from_configure_with_browser_control(
        configure: &Configure,
        browser_control_session: BrowserControlSession,
    ) -> Result<Self, String> {
        Self::from_configure_with_startup_bootstraps(configure, None, Some(browser_control_session))
    }

    fn from_configure_with_startup_bootstraps(
        configure: &Configure,
        bootstrap: Option<OwnedDocumentBootstrap>,
        browser_control_session: Option<BrowserControlSession>,
    ) -> Result<Self, String> {
        configure.validate()?;
        if configure.role != interaction_contracts::desktop::handshake::PeerRole::Device {
            return Err("device requires Configure for its own role".into());
        }
        let settings = &configure.settings;
        let origin: u64 = setting(settings, "clock_origin_ms")?
            .parse()
            .map_err(|_| "clock_origin_ms is not a number".to_string())?;
        if settings.contains_key("execution_domain") {
            return Err("execution domain is derived from the trusted device backend".into());
        }
        let browser_profile = browser_control_session
            .as_ref()
            .map(|session| session.browser_page_profile);
        let browser_scope = browser_control_session
            .as_ref()
            .map(|session| session.scope.clone());
        if let Some(session) = &browser_control_session {
            if configure.session_epoch != session.scope.context.session_epoch
                || configure.browser_page_profile != Some(session.browser_page_profile)
            {
                return Err("Device Configure does not match the private browser scope".into());
            }
            if settings.contains_key("backend")
                || settings.contains_key("native_target_pid")
                || settings.contains_key("native_target_bundle")
                || settings.contains_key("native_target_window_id")
                || settings.contains_key("native_guard_path")
                || settings.contains_key("owned_save")
            {
                return Err("browser Device configuration cannot select native input".into());
            }
        } else if configure.browser_page_profile.is_some() {
            return Err("browser profile requires the private authenticated fd3 channel".into());
        }
        if bootstrap.is_some() && browser_control_session.is_some() {
            return Err("owned-document and browser startup scopes are mutually exclusive".into());
        }
        let backend = if browser_control_session.is_some() {
            DeviceBackendMode::HeadlessBrowser
        } else {
            match settings.get("backend").map(String::as_str) {
                None | Some("simulated") => DeviceBackendMode::Simulated,
                Some("macos-observe") => DeviceBackendMode::MacosObserve,
                Some("macos-ax-press") => DeviceBackendMode::MacosAxPress,
                Some("macos-background-ax") => DeviceBackendMode::MacosBackgroundAx,
                Some("macos-owned-value") => DeviceBackendMode::MacosOwnedValue,
                Some(other) => return Err(format!("unknown device backend {other}")),
            }
        };
        let owned_save_requested = match settings.get("owned_save").map(String::as_str) {
            None => false,
            Some("1") => true,
            Some(_) => return Err("owned_save must be the explicit value 1".into()),
        };
        if owned_save_requested
            && (backend != DeviceBackendMode::MacosOwnedValue || bootstrap.is_none())
        {
            return Err("owned_save requires the private owned-value startup scope".into());
        }
        let native_target = if matches!(
            backend,
            DeviceBackendMode::MacosAxPress | DeviceBackendMode::MacosBackgroundAx
        ) {
            let pid: u32 = setting(settings, "native_target_pid")?
                .parse()
                .map_err(|_| "invalid native target PID")?;
            let bundle = setting(settings, "native_target_bundle")?;
            if pid == 0
                || pid > i32::MAX as u32
                || ![
                    "com.lingxi.computer-user.guard-fixture",
                    "com.lingxi.computer-user.mvp-fixture",
                    "com.apple.TextEdit",
                ]
                .contains(&bundle)
            {
                return Err("native AX execution target is outside the registered scope".into());
            }
            let process_birth_supplied = settings
                .contains_key("native_target_process_start_seconds")
                || settings.contains_key("native_target_process_start_micros");
            if settings
                .get("native_guard_path")
                .is_none_or(|s| s.is_empty())
                && (backend == DeviceBackendMode::MacosAxPress || process_birth_supplied)
            {
                return Err("native_guard_path required for AX execution".into());
            }
            Some((pid, bundle.into()))
        } else {
            None
        };
        let native_target_window = settings.get("native_target_window_id").cloned();
        if let Some(window) = &native_target_window
            && (native_target.is_none()
                || window
                    .strip_prefix("window-")
                    .and_then(|s| s.parse::<u32>().ok())
                    .is_none_or(|n| n == 0 || window != &format!("window-{n}")))
        {
            return Err("native window scope needs a valid AX execution target/window".into());
        }
        if backend == DeviceBackendMode::MacosBackgroundAx && native_target_window.is_none() {
            return Err("macos-background-ax requires a fixed native window scope".into());
        }
        let native_target_process = match (
            settings.get("native_target_process_start_seconds"),
            settings.get("native_target_process_start_micros"),
        ) {
            (None, None) => None,
            (Some(start_seconds), Some(start_micros)) => {
                let Some((pid, _)) = native_target.as_ref() else {
                    return Err(
                        "native target process identity requires a scoped macOS AX backend".into(),
                    );
                };
                if !matches!(
                    backend,
                    DeviceBackendMode::MacosAxPress | DeviceBackendMode::MacosBackgroundAx
                ) || native_target_window.is_none()
                {
                    return Err(
                        "native target process identity requires an AX process/bundle/window scope"
                            .into(),
                    );
                }
                let start_seconds_value: u64 = start_seconds
                    .parse()
                    .map_err(|_| "invalid native target process start seconds")?;
                let start_micros_value: u64 = start_micros
                    .parse()
                    .map_err(|_| "invalid native target process start micros")?;
                if start_seconds_value.to_string() != start_seconds.as_str()
                    || start_micros_value.to_string() != start_micros.as_str()
                    || start_seconds_value > 9_007_199_254_740_991
                {
                    return Err(
                        "native target process identity must use bounded canonical integers".into(),
                    );
                }
                let process = KernelProcessIdentity {
                    pid: *pid,
                    start_seconds: start_seconds_value,
                    start_micros: start_micros_value,
                };
                process.validate()?;
                Some(process)
            }
            _ => {
                return Err(
                    "native target process start seconds and micros must be supplied together"
                        .into(),
                );
            }
        };
        if backend == DeviceBackendMode::MacosBackgroundAx
            && native_target_process.is_some()
            && settings
                .get("native_guard_path")
                .is_none_or(|path| path.is_empty())
        {
            return Err("active macos-background-ax requires native_guard_path".into());
        }
        let mut config = Self {
            backend,
            socket_dir: PathBuf::from(setting(settings, "socket_dir")?),
            host_token: setting(settings, "host_token")?.to_string(),
            session_epoch: configure.session_epoch,
            clock: SharedClock::from_origin(origin),
            world_log: PathBuf::from(setting(settings, "world_log")?),
            native_guard_path: settings.get("native_guard_path").map(PathBuf::from),
            native_target,
            native_target_window,
            native_target_process,
            owned_document_registration: None,
            owned_save_enabled: false,
            browser_scope,
            browser_control_session,
        };
        if let Some(bootstrap) = bootstrap {
            match config.backend {
                DeviceBackendMode::MacosObserve
                    if config.native_target_window.is_none() && config.native_target.is_none() =>
                {
                    config.owned_document_registration = Some(bootstrap.into_registration());
                }
                DeviceBackendMode::MacosOwnedValue => {
                    if settings.contains_key("native_target_pid")
                        || settings.contains_key("native_target_bundle")
                        || settings.contains_key("native_target_window_id")
                        || config.native_guard_path.is_none()
                    {
                        return Err(
                            "owned value mode accepts only private startup process scope and a native guard".into(),
                        );
                    }
                    let registration = bootstrap.into_registration();
                    registration.validate_identity()?;
                    if registration.executable_path
                        != "/System/Applications/TextEdit.app/Contents/MacOS/TextEdit"
                        || registration.bundle_id != "com.apple.TextEdit"
                    {
                        return Err(
                            "owned value process is outside the private TextEdit scope".into()
                        );
                    }
                    config.native_target = Some((registration.pid, registration.bundle_id.clone()));
                    config.owned_document_registration = Some(registration);
                    config.owned_save_enabled = owned_save_requested;
                }
                _ => {
                    return Err(
                        "private owned document registration is not valid for this backend".into(),
                    );
                }
            }
        } else if config.backend == DeviceBackendMode::MacosOwnedValue {
            return Err("macos-owned-value requires Main's private startup registration".into());
        }
        if config.backend == DeviceBackendMode::HeadlessBrowser
            && browser_profile != Some(BrowserPageProfile::FixtureBackgroundOption)
        {
            return Err("browser Device backend requires its exact private fixture profile".into());
        }
        Ok(config)
    }

    pub fn socket_path(&self) -> PathBuf {
        self.socket_dir.join("device.sock")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use desktop_ipc::browser_control::{
        BrowserControlSession, PendingBrowserControl, read_hello, write_bootstrap, write_ready,
    };
    use interaction_contracts::desktop::browser::BrowserPageProfile;
    use interaction_contracts::desktop::browser_control::{
        BrowserControlBootstrap, BrowserControlReady, BrowserControlToken,
    };
    use interaction_contracts::desktop::device::ExecutionObservation;
    use interaction_contracts::desktop::handshake::PeerRole;
    use interaction_contracts::desktop::message::Message;
    use interaction_contracts::desktop::samples;
    use serde_json::json;
    use std::os::unix::net::UnixStream;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn owned_bootstrap() -> OwnedDocumentBootstrap {
        let payload = json!({
            "version": 1,
            "registration_id": "123e4567-e89b-42d3-a456-426614174000",
            "pid": 1234,
            "start_seconds": 1_700_000_000,
            "start_micros": 123456,
            "executable_path": "/System/Applications/TextEdit.app/Contents/MacOS/TextEdit",
            "bundle_id": "com.apple.TextEdit",
            "document_path": "/private/tmp/owned/document.txt",
            "document_sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "document_dev": 1,
            "document_ino": 2,
            "directory_dev": 3,
            "directory_ino": 4,
            "issued_unix_ms": SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64,
        });
        let payload = serde_json::to_vec(&payload).unwrap();
        let mut frame = (payload.len() as u32).to_be_bytes().to_vec();
        frame.extend(payload);
        OwnedDocumentBootstrap::read(&mut &frame[..]).unwrap()
    }

    fn browser_session() -> BrowserControlSession {
        let evidence = samples::representative_messages()
            .into_iter()
            .find_map(|(_, message)| match message {
                Message::Observation(ExecutionObservation::Browser { observation }) => {
                    Some(*observation)
                }
                _ => None,
            })
            .expect("synthetic browser scope");
        let bootstrap = BrowserControlBootstrap {
            version: interaction_contracts::desktop::browser_control::BROWSER_CONTROL_VERSION,
            token: BrowserControlToken::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .unwrap(),
            channel_generation: 5,
            browser_page_profile: BrowserPageProfile::FixtureBackgroundOption,
            scope: evidence.scope,
        };
        let (device, mut main) = UnixStream::pair().unwrap();
        let main_bootstrap = bootstrap.clone();
        let server = thread::spawn(move || {
            write_bootstrap(&mut main, &main_bootstrap).unwrap();
            let hello = read_hello(&mut main, &main_bootstrap).unwrap();
            write_ready(
                &mut main,
                &BrowserControlReady {
                    version:
                        interaction_contracts::desktop::browser_control::BROWSER_CONTROL_VERSION,
                    channel_generation: hello.channel_generation,
                    session_epoch: hello.session_epoch,
                    scope_id: hello.scope_id.clone(),
                    scope_generation: hello.scope_generation,
                    next_sequence: 1,
                },
                &hello,
            )
            .unwrap();
        });
        let pending = PendingBrowserControl::read_from_stream(device).unwrap();
        let session = pending.finish().unwrap();
        server.join().unwrap();
        session
    }

    #[test]
    fn a_complete_configuration_is_read_and_a_missing_key_is_named() {
        let mut settings = BTreeMap::new();
        for (key, value) in [
            ("socket_dir", "/tmp/x"),
            ("host_token", "0123456789abcdef0123456789abcdef"),
            ("clock_origin_ms", "1700000000000"),
            ("world_log", "/tmp/x/world.log"),
        ] {
            settings.insert(key.to_string(), value.to_string());
        }
        let mut configure = Configure {
            role: PeerRole::Device,
            session_epoch: 3,
            settings,
            browser_page_profile: None,
        };
        let config = DeviceConfig::from_configure(&configure).unwrap();
        assert_eq!(config.backend, DeviceBackendMode::Simulated);
        assert_eq!(config.session_epoch, 3);
        assert_eq!(config.socket_path(), PathBuf::from("/tmp/x/device.sock"));
        configure
            .settings
            .insert("execution_domain".into(), "background_ax".into());
        assert!(
            DeviceConfig::from_configure(&configure)
                .unwrap_err()
                .contains("derived")
        );
        configure.settings.remove("execution_domain");
        configure
            .settings
            .insert("backend".into(), "macos-observe".into());
        assert_eq!(
            DeviceConfig::from_configure(&configure).unwrap().backend,
            DeviceBackendMode::MacosObserve
        );
        configure
            .settings
            .insert("backend".into(), "macos-background-ax".into());
        assert!(
            DeviceConfig::from_configure(&configure)
                .unwrap_err()
                .contains("native_target_pid")
        );
        configure.settings.insert("backend".into(), "bad".into());
        assert!(
            DeviceConfig::from_configure(&configure)
                .unwrap_err()
                .contains("backend")
        );
        configure.settings.remove("backend");
        configure.settings.remove("host_token");
        let error = DeviceConfig::from_configure(&configure).unwrap_err();
        assert!(error.contains("host_token"), "{error}");
    }

    #[test]
    fn browser_profile_requires_exact_private_fd3_scope_and_never_selects_native_input() {
        let settings = BTreeMap::from([
            ("socket_dir".into(), "/tmp/x".into()),
            (
                "host_token".into(),
                "0123456789abcdef0123456789abcdef".into(),
            ),
            ("clock_origin_ms".into(), "1700000000000".into()),
            ("world_log".into(), "/tmp/x/world.log".into()),
        ]);
        let configure = Configure {
            role: PeerRole::Device,
            session_epoch: 7,
            settings,
            browser_page_profile: Some(BrowserPageProfile::FixtureBackgroundOption),
        };
        assert!(DeviceConfig::from_configure(&configure).is_err());

        let config =
            DeviceConfig::from_configure_with_browser_control(&configure, browser_session())
                .unwrap();
        assert_eq!(config.backend, DeviceBackendMode::HeadlessBrowser);
        assert_eq!(config.session_epoch, 7);
        assert!(config.browser_scope.is_some());
        assert!(config.browser_control_session.is_some());
        assert!(config.native_target.is_none());
        assert!(config.native_guard_path.is_none());

        let mut native_settings = configure.settings.clone();
        native_settings.insert("backend".into(), "macos-observe".into());
        let mixed = Configure {
            settings: native_settings,
            ..configure.clone()
        };
        assert!(
            DeviceConfig::from_configure_with_browser_control(&mixed, browser_session()).is_err()
        );

        let stale = Configure {
            session_epoch: 8,
            ..configure
        };
        assert!(
            DeviceConfig::from_configure_with_browser_control(&stale, browser_session()).is_err()
        );
    }

    #[test]
    fn native_execution_requires_a_registered_scope_and_explicit_guard() {
        let mut settings = BTreeMap::from([
            ("socket_dir".into(), "/tmp/x".into()),
            (
                "host_token".into(),
                "0123456789abcdef0123456789abcdef".into(),
            ),
            ("clock_origin_ms".into(), "1700000000000".into()),
            ("world_log".into(), "/tmp/x/world.log".into()),
            ("backend".into(), "macos-ax-press".into()),
            ("native_target_pid".into(), "42".into()),
            (
                "native_target_bundle".into(),
                "com.lingxi.computer-user.guard-fixture".into(),
            ),
        ]);
        let configure = |settings| Configure {
            role: PeerRole::Device,
            session_epoch: 1,
            settings,
            browser_page_profile: None,
        };
        assert!(DeviceConfig::from_configure(&configure(settings.clone())).is_err());
        settings.insert("native_guard_path".into(), "/tmp/owned-guard".into());
        let config = DeviceConfig::from_configure(&configure(settings.clone())).unwrap();
        assert_eq!(
            config.native_target,
            Some((42, "com.lingxi.computer-user.guard-fixture".into()))
        );
        let mut mvp = settings.clone();
        mvp.insert(
            "native_target_bundle".into(),
            "com.lingxi.computer-user.mvp-fixture".into(),
        );
        assert_eq!(
            DeviceConfig::from_configure(&configure(mvp.clone()))
                .unwrap()
                .native_target,
            Some((42, "com.lingxi.computer-user.mvp-fixture".into()))
        );
        for (key, value) in [
            ("native_target_pid", "0"),
            ("native_target_pid", "2147483648"),
            ("native_target_bundle", "arbitrary.app"),
        ] {
            let mut invalid = settings.clone();
            invalid.insert(key.into(), value.into());
            assert!(DeviceConfig::from_configure(&configure(invalid)).is_err());
        }
        mvp.insert("native_target_bundle".into(), "unregistered.app".into());
        assert!(DeviceConfig::from_configure(&configure(mvp)).is_err());
        for window in ["", "window-0", "window-4294967296", "other"] {
            let mut bad = settings.clone();
            bad.insert("native_target_window_id".into(), window.into());
            assert!(DeviceConfig::from_configure(&configure(bad)).is_err());
        }
        let mut scoped = settings.clone();
        scoped.insert("native_target_window_id".into(), "window-1".into());
        assert_eq!(
            DeviceConfig::from_configure(&configure(scoped.clone()))
                .unwrap()
                .native_target_window
                .as_deref(),
            Some("window-1")
        );
        scoped.insert("backend".into(), "macos-observe".into());
        assert!(DeviceConfig::from_configure(&configure(scoped)).is_err());
        settings.insert("backend".into(), "macos-observe".into());
        assert!(
            DeviceConfig::from_configure(&configure(settings))
                .unwrap()
                .native_target
                .is_none()
        );
    }

    #[test]
    fn native_target_process_identity_is_a_valid_pair_only_for_ax_window_scope() {
        let configure = |settings| Configure {
            role: PeerRole::Device,
            session_epoch: 1,
            settings,
            browser_page_profile: None,
        };
        let base = BTreeMap::from([
            ("socket_dir".into(), "/tmp/x".into()),
            (
                "host_token".into(),
                "0123456789abcdef0123456789abcdef".into(),
            ),
            ("clock_origin_ms".into(), "1700000000000".into()),
            ("world_log".into(), "/tmp/x/world.log".into()),
            ("backend".into(), "macos-ax-press".into()),
            ("native_target_pid".into(), "42".into()),
            (
                "native_target_bundle".into(),
                "com.lingxi.computer-user.guard-fixture".into(),
            ),
            ("native_target_window_id".into(), "window-7".into()),
            ("native_guard_path".into(), "/tmp/guard".into()),
        ]);
        let mut paired = base.clone();
        paired.insert(
            "native_target_process_start_seconds".into(),
            "1700000000".into(),
        );
        paired.insert("native_target_process_start_micros".into(), "123456".into());
        let parsed = DeviceConfig::from_configure(&configure(paired.clone())).unwrap();
        assert_eq!(
            parsed.native_target_process,
            Some(KernelProcessIdentity {
                pid: 42,
                start_seconds: 1_700_000_000,
                start_micros: 123_456,
            })
        );

        for missing in [
            "native_target_process_start_seconds",
            "native_target_process_start_micros",
        ] {
            let mut partial = paired.clone();
            partial.remove(missing);
            assert!(DeviceConfig::from_configure(&configure(partial)).is_err());
        }
        for (key, invalid) in [
            ("native_target_process_start_seconds", "not-a-number"),
            ("native_target_process_start_seconds", "0"),
            ("native_target_process_start_seconds", "9007199254740992"),
            ("native_target_process_start_seconds", "+1700000000"),
            ("native_target_process_start_seconds", "01700000000"),
            ("native_target_process_start_micros", "1000000"),
            ("native_target_process_start_micros", "-1"),
            ("native_target_process_start_micros", "+123456"),
            ("native_target_process_start_micros", "0123456"),
        ] {
            let mut malformed = paired.clone();
            malformed.insert(key.into(), invalid.into());
            assert!(DeviceConfig::from_configure(&configure(malformed)).is_err());
        }
        for mode in ["simulated", "macos-observe", "macos-owned-value"] {
            let mut wrong_mode = paired.clone();
            wrong_mode.insert("backend".into(), mode.into());
            assert!(DeviceConfig::from_configure(&configure(wrong_mode)).is_err());
        }
        let mut private_grant = paired.clone();
        private_grant.insert("backend".into(), "macos-owned-value".into());
        assert!(
            DeviceConfig::from_configure_with_owned_bootstrap(
                &configure(private_grant),
                Some(owned_bootstrap()),
            )
            .is_err()
        );
        let mut missing_window = paired.clone();
        missing_window.remove("native_target_window_id");
        assert!(DeviceConfig::from_configure(&configure(missing_window)).is_err());

        let without_pair = DeviceConfig::from_configure(&configure(base)).unwrap();
        assert!(without_pair.native_target_process.is_none());
    }

    #[test]
    fn background_ax_requires_fixed_window_and_treats_birth_pair_as_active_grant() {
        let configure = |settings| Configure {
            role: PeerRole::Device,
            session_epoch: 1,
            settings,
            browser_page_profile: None,
        };
        let mut settings = BTreeMap::from([
            ("socket_dir".into(), "/tmp/x".into()),
            (
                "host_token".into(),
                "0123456789abcdef0123456789abcdef".into(),
            ),
            ("clock_origin_ms".into(), "1700000000000".into()),
            ("world_log".into(), "/tmp/x/world.log".into()),
            ("backend".into(), "macos-background-ax".into()),
            ("native_target_pid".into(), "42".into()),
            (
                "native_target_bundle".into(),
                "com.lingxi.computer-user.mvp-fixture".into(),
            ),
            ("native_guard_path".into(), "/tmp/guard".into()),
            ("native_target_window_id".into(), "window-7".into()),
            (
                "native_target_process_start_seconds".into(),
                "1700000000".into(),
            ),
            ("native_target_process_start_micros".into(), "123456".into()),
        ]);
        let parsed = DeviceConfig::from_configure(&configure(settings.clone())).unwrap();
        assert_eq!(parsed.backend, DeviceBackendMode::MacosBackgroundAx);
        assert_eq!(
            parsed.native_target,
            Some((42, "com.lingxi.computer-user.mvp-fixture".into()))
        );
        assert_eq!(parsed.native_target_window.as_deref(), Some("window-7"));
        assert_eq!(
            parsed.native_target_process,
            Some(KernelProcessIdentity {
                pid: 42,
                start_seconds: 1_700_000_000,
                start_micros: 123_456,
            })
        );

        {
            let key = "native_target_window_id";
            let mut missing = settings.clone();
            missing.remove(key);
            assert!(
                DeviceConfig::from_configure(&configure(missing)).is_err(),
                "background mode must reject missing {key}"
            );
        }

        let mut passive = settings.clone();
        passive.remove("native_target_process_start_seconds");
        passive.remove("native_target_process_start_micros");
        let passive = DeviceConfig::from_configure(&configure(passive)).unwrap();
        assert_eq!(passive.backend, DeviceBackendMode::MacosBackgroundAx);
        assert!(passive.native_target_process.is_none());

        let mut passive_without_guard = settings.clone();
        passive_without_guard.remove("native_target_process_start_seconds");
        passive_without_guard.remove("native_target_process_start_micros");
        passive_without_guard.remove("native_guard_path");
        let passive_without_guard =
            DeviceConfig::from_configure(&configure(passive_without_guard)).unwrap();
        assert!(passive_without_guard.native_target_process.is_none());
        assert!(passive_without_guard.native_guard_path.is_none());

        for missing in [
            "native_target_process_start_seconds",
            "native_target_process_start_micros",
        ] {
            let mut partial = settings.clone();
            partial.remove(missing);
            assert!(
                DeviceConfig::from_configure(&configure(partial)).is_err(),
                "background mode must reject a partial pair missing {missing}"
            );
        }

        let mut no_guard = settings.clone();
        no_guard.remove("native_guard_path");
        assert!(DeviceConfig::from_configure(&configure(no_guard)).is_err());

        settings.insert("native_target_window_id".into(), "window-07".into());
        assert!(DeviceConfig::from_configure(&configure(settings)).is_err());
    }

    #[test]
    fn private_registration_is_only_observation_context_and_public_settings_cannot_supply_it() {
        let mut settings = BTreeMap::from([
            ("socket_dir".into(), "/tmp/x".into()),
            (
                "host_token".into(),
                "0123456789abcdef0123456789abcdef".into(),
            ),
            ("clock_origin_ms".into(), "1700000000000".into()),
            ("world_log".into(), "/tmp/x/world.log".into()),
            ("backend".into(), "macos-observe".into()),
            (
                "owned_document_registration".into(),
                serde_json::to_string(&json!({
                    "registration_id": "123e4567-e89b-42d3-a456-426614174000",
                    "document_path": "/private/customer/document.txt"
                }))
                .unwrap(),
            ),
        ]);
        let configure = |settings| Configure {
            role: PeerRole::Device,
            session_epoch: 1,
            settings,
            browser_page_profile: None,
        };

        let ordinary = DeviceConfig::from_configure(&configure(settings.clone())).unwrap();
        assert!(ordinary.owned_document_registration.is_none());

        let owned = DeviceConfig::from_configure_with_owned_bootstrap(
            &configure(settings.clone()),
            Some(owned_bootstrap()),
        )
        .unwrap();
        assert_eq!(owned.backend, DeviceBackendMode::MacosObserve);
        assert!(owned.native_target.is_none());
        assert!(owned.native_target_window.is_none());
        assert_eq!(
            owned.owned_document_registration.as_ref().unwrap().pid,
            1234
        );

        settings.insert("backend".into(), "simulated".into());
        assert!(
            DeviceConfig::from_configure_with_owned_bootstrap(
                &configure(settings.clone()),
                Some(owned_bootstrap()),
            )
            .is_err()
        );

        settings.insert("backend".into(), "macos-ax-press".into());
        settings.insert("native_target_pid".into(), "1234".into());
        settings.insert("native_target_bundle".into(), "com.apple.TextEdit".into());
        settings.insert("native_guard_path".into(), "/tmp/guard".into());
        assert!(
            DeviceConfig::from_configure_with_owned_bootstrap(
                &configure(settings.clone()),
                Some(owned_bootstrap()),
            )
            .is_err()
        );

        settings.insert("backend".into(), "macos-observe".into());
        settings.insert("native_target_window_id".into(), "window-7".into());
        assert!(
            DeviceConfig::from_configure_with_owned_bootstrap(
                &configure(settings),
                Some(owned_bootstrap()),
            )
            .is_err()
        );
    }

    #[test]
    fn owned_value_mode_requires_private_registration_and_derives_its_only_process_scope() {
        let configure = |settings| Configure {
            role: PeerRole::Device,
            session_epoch: 1,
            settings,
            browser_page_profile: None,
        };
        let base = BTreeMap::from([
            ("socket_dir".into(), "/tmp/x".into()),
            (
                "host_token".into(),
                "0123456789abcdef0123456789abcdef".into(),
            ),
            ("clock_origin_ms".into(), "1700000000000".into()),
            ("world_log".into(), "/tmp/x/world.log".into()),
            ("backend".into(), "macos-owned-value".into()),
            ("native_guard_path".into(), "/tmp/guard".into()),
        ]);
        assert!(DeviceConfig::from_configure(&configure(base.clone())).is_err());
        let config = DeviceConfig::from_configure_with_owned_bootstrap(
            &configure(base.clone()),
            Some(owned_bootstrap()),
        )
        .unwrap();
        assert_eq!(config.backend, DeviceBackendMode::MacosOwnedValue);
        assert_eq!(
            config.native_target,
            Some((1234, "com.apple.TextEdit".into()))
        );
        assert!(config.native_target_window.is_none());
        assert!(!config.owned_save_enabled);
        assert_eq!(
            config
                .owned_document_registration
                .as_deref()
                .unwrap()
                .executable_path,
            "/System/Applications/TextEdit.app/Contents/MacOS/TextEdit"
        );

        let mut public_scope = base;
        public_scope.insert("native_target_pid".into(), "1234".into());
        assert!(
            DeviceConfig::from_configure_with_owned_bootstrap(
                &configure(public_scope),
                Some(owned_bootstrap()),
            )
            .is_err()
        );

        let mut no_guard = BTreeMap::from([
            ("socket_dir".into(), "/tmp/x".into()),
            (
                "host_token".into(),
                "0123456789abcdef0123456789abcdef".into(),
            ),
            ("clock_origin_ms".into(), "1700000000000".into()),
            ("world_log".into(), "/tmp/x/world.log".into()),
            ("backend".into(), "macos-owned-value".into()),
        ]);
        no_guard.remove("native_guard_path");
        assert!(
            DeviceConfig::from_configure_with_owned_bootstrap(
                &configure(no_guard),
                Some(owned_bootstrap()),
            )
            .is_err()
        );
    }

    #[test]
    fn owned_save_setting_requires_the_private_owned_startup_and_is_off_when_omitted() {
        let configure = |settings| Configure {
            role: PeerRole::Device,
            session_epoch: 1,
            settings,
            browser_page_profile: None,
        };
        let mut settings = BTreeMap::from([
            ("socket_dir".into(), "/tmp/x".into()),
            (
                "host_token".into(),
                "0123456789abcdef0123456789abcdef".into(),
            ),
            ("clock_origin_ms".into(), "1700000000000".into()),
            ("world_log".into(), "/tmp/x/world.log".into()),
            ("backend".into(), "macos-owned-value".into()),
            ("native_guard_path".into(), "/tmp/guard".into()),
        ]);
        let ordinary = DeviceConfig::from_configure_with_owned_bootstrap(
            &configure(settings.clone()),
            Some(owned_bootstrap()),
        )
        .unwrap();
        assert!(!ordinary.owned_save_enabled);

        settings.insert("owned_save".into(), "1".into());
        assert!(DeviceConfig::from_configure(&configure(settings.clone())).is_err());
        let granted = DeviceConfig::from_configure_with_owned_bootstrap(
            &configure(settings.clone()),
            Some(owned_bootstrap()),
        )
        .unwrap();
        assert!(granted.owned_save_enabled);

        for invalid in ["0", "true", "yes"] {
            let mut bad = settings.clone();
            bad.insert("owned_save".into(), invalid.into());
            assert!(
                DeviceConfig::from_configure_with_owned_bootstrap(
                    &configure(bad),
                    Some(owned_bootstrap()),
                )
                .is_err()
            );
        }
        let mut observe = settings;
        observe.insert("backend".into(), "macos-observe".into());
        assert!(
            DeviceConfig::from_configure_with_owned_bootstrap(
                &configure(observe),
                Some(owned_bootstrap()),
            )
            .is_err()
        );
    }
}
