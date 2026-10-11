//! What the supervisor tells cu-host when it starts it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use desktop_ipc::SharedClock;
use interaction_contracts::desktop::browser::BrowserPageProfile;
use interaction_contracts::desktop::handshake::Configure;
use interaction_contracts::desktop::operator::RunMode;
use interaction_contracts::desktop::owned_registration::OwnedDocumentRegistration;
use interaction_engine::engine::NativeBackgroundWindowScope;

use crate::owned_document_bootstrap::OwnedDocumentBootstrap;

#[derive(Debug, Clone)]
pub struct HostConfig {
    pub socket_dir: PathBuf,
    pub db_path: PathBuf,
    pub device_token: String,
    pub device_epoch: u64,
    pub source_token: String,
    pub clock: SharedClock,
    pub initial_mode: RunMode,
    pub native_target_bundle: Option<String>,
    pub native_window_scope: Option<interaction_engine::engine::NativeWindowScope>,
    /// Background target evidence scope, kept separate from foreground
    /// activation/focus authority. It cannot grant Device input.
    pub native_background_window_scope: Option<NativeBackgroundWindowScope>,
    /// Supplied only by the private main-issued startup prefix.
    pub owned_document_registration: Option<Box<OwnedDocumentRegistration>>,
    /// Enabled only by the private Main-issued separate Save authorization.
    pub allow_owned_document_save: bool,
    pub planning_backend: PlanningBackend,
    /// The only supported browser page profile, selected by trusted Host
    /// configuration. GoalDraft cannot set this value.
    pub browser_page_profile: Option<BrowserPageProfile>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanningBackend {
    Fixture,
    NativeReference,
    /// Explicit fixed-plan backend for the closed HeadlessBrowser fixture.
    BrowserReference,
    /// Explicit local plumbing diagnostic; never a live-model fallback.
    OwnedReference,
    Model,
    Unconfigured,
}

fn setting<'a>(settings: &'a BTreeMap<String, String>, key: &str) -> Result<&'a str, String> {
    settings
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| format!("the supervisor did not give {key}"))
}

pub fn background_window_scope_from_configure(
    configure: &Configure,
) -> Result<Option<NativeBackgroundWindowScope>, String> {
    let settings = &configure.settings;
    let birth = match (
        settings.get("native_target_process_start_seconds"),
        settings.get("native_target_process_start_micros"),
    ) {
        (None, None) => None,
        (Some(seconds), Some(micros)) => {
            let seconds_value: u64 = seconds
                .parse()
                .map_err(|_| "invalid background target process start seconds")?;
            let micros_value: u64 = micros
                .parse()
                .map_err(|_| "invalid background target process start micros")?;
            if seconds_value == 0
                || seconds_value > 9_007_199_254_740_991
                || micros_value >= 1_000_000
                || seconds_value.to_string() != *seconds
                || micros_value.to_string() != *micros
            {
                return Err("background process birth identity is not canonical".into());
            }
            Some((seconds_value, micros_value))
        }
        _ => return Err("background process birth fields must be supplied together".into()),
    };
    let Some(window) = settings.get("native_background_window_id") else {
        if birth.is_some() {
            return Err("background process birth identity requires a background window".into());
        }
        return Ok(None);
    };
    if settings.contains_key("native_target_window_id") {
        return Err("foreground and background window scopes cannot be combined".into());
    }
    let app = settings
        .get("native_target_bundle")
        .cloned()
        .ok_or("background scope needs a native application")?;
    let pid_text = setting(settings, "native_target_pid")?;
    let pid: u32 = pid_text
        .parse()
        .map_err(|_| "invalid background target PID")?;
    if ![
        "com.lingxi.computer-user.guard-fixture",
        "com.lingxi.computer-user.mvp-fixture",
        "com.apple.TextEdit",
    ]
    .contains(&app.as_str())
        || pid == 0
        || pid > i32::MAX as u32
        || pid.to_string() != pid_text
        || window
            .strip_prefix("window-")
            .and_then(|value| value.parse::<u32>().ok())
            .is_none_or(|id| id == 0 || window != &format!("window-{id}"))
    {
        return Err("invalid fixed background AX scope".into());
    }
    let scope = NativeBackgroundWindowScope {
        app,
        pid,
        window: window.clone(),
        process_start_seconds: birth.map(|(seconds, _)| seconds),
        process_start_micros: birth.map(|(_, micros)| micros),
    };
    scope.validate()?;
    Ok(Some(scope))
}

impl HostConfig {
    pub fn from_configure(configure: &Configure) -> Result<Self, String> {
        Self::from_configure_with_owned_bootstrap(configure, None)
    }

    /// Build configuration using a registration read from the private startup
    /// pipe. Configure itself intentionally has no route to this value.
    pub fn from_configure_with_owned_bootstrap(
        configure: &Configure,
        bootstrap: Option<OwnedDocumentBootstrap>,
    ) -> Result<Self, String> {
        let settings = &configure.settings;
        if settings.contains_key("execution_domain") {
            return Err("execution domain is selected only by the Device manifest".into());
        }
        let number = |key: &str| -> Result<u64, String> {
            setting(settings, key)?
                .parse()
                .map_err(|_| format!("{key} is not a number"))
        };
        let initial_mode = match settings.get("initial_mode").map(String::as_str) {
            None | Some("observe") => RunMode::Observe,
            Some("shadow") => RunMode::Shadow,
            Some("execute") => RunMode::Execute,
            Some(other) => return Err(format!("unknown initial_mode {other}")),
        };
        let native_target_bundle = settings.get("native_target_bundle").cloned();
        if native_target_bundle.as_ref().is_some_and(|b| {
            b.trim().is_empty() || b.len() > 256 || b.chars().any(char::is_control)
        }) {
            return Err("native observation application is invalid".into());
        }
        let planning_backend = match settings.get("planning_backend").map(String::as_str) {
            None if native_target_bundle.is_none() => PlanningBackend::Fixture,
            None | Some("unconfigured") => PlanningBackend::Unconfigured,
            Some("fixture") if native_target_bundle.is_none() => PlanningBackend::Fixture,
            Some("native_reference")
                if matches!(
                    native_target_bundle.as_deref(),
                    Some("com.lingxi.computer-user.guard-fixture")
                        | Some("com.lingxi.computer-user.mvp-fixture")
                ) =>
            {
                PlanningBackend::NativeReference
            }
            Some("browser_reference")
                if configure.browser_page_profile.is_some() && native_target_bundle.is_none() =>
            {
                PlanningBackend::BrowserReference
            }
            Some("model") => PlanningBackend::Model,
            Some("owned_reference") => PlanningBackend::OwnedReference,
            _ => return Err("planning backend does not match the registered application".into()),
        };
        let allow_owned_document_save = match settings.get("owned_save").map(String::as_str) {
            None | Some("0") => false,
            Some("1") => true,
            Some(_) => return Err("owned Save authorization setting is invalid".into()),
        };
        let native_window_scope = if let Some(window) = settings.get("native_target_window_id") {
            let app = native_target_bundle
                .clone()
                .ok_or("activation needs a native application")?;
            let pid: u32 = setting(settings, "native_target_pid")?
                .parse()
                .map_err(|_| "invalid activation PID")?;
            if ![
                "com.lingxi.computer-user.guard-fixture",
                "com.lingxi.computer-user.mvp-fixture",
                "com.apple.TextEdit",
            ]
            .contains(&app.as_str())
                || pid == 0
                || pid > i32::MAX as u32
                || window
                    .strip_prefix("window-")
                    .and_then(|s| s.parse::<u32>().ok())
                    .is_none_or(|id| id == 0 || window != &format!("window-{id}"))
            {
                return Err("invalid fixed activation scope".into());
            }
            Some(interaction_engine::engine::NativeWindowScope {
                app,
                pid,
                window: window.clone(),
            })
        } else {
            None
        };
        let native_background_window_scope = background_window_scope_from_configure(configure)?;
        if configure.browser_page_profile.is_some()
            && (native_target_bundle.is_some()
                || native_window_scope.is_some()
                || native_background_window_scope.is_some()
                || bootstrap.is_some())
        {
            return Err(
                "the browser fixture profile cannot be combined with native observation scopes"
                    .into(),
            );
        }
        let mut config = Self {
            socket_dir: PathBuf::from(setting(settings, "socket_dir")?),
            db_path: PathBuf::from(setting(settings, "db_path")?),
            device_token: setting(settings, "device_token")?.to_string(),
            device_epoch: number("device_epoch")?,
            source_token: setting(settings, "source_token")?.to_string(),
            clock: SharedClock::from_origin(number("clock_origin_ms")?),
            initial_mode,
            native_target_bundle,
            native_window_scope,
            native_background_window_scope,
            owned_document_registration: None,
            allow_owned_document_save: false,
            planning_backend,
            browser_page_profile: configure.browser_page_profile,
        };
        if let Some(bootstrap) = bootstrap {
            let registration = bootstrap.into_registration();
            let pid_matches = settings
                .get("native_target_pid")
                .is_some_and(|pid| pid == &registration.pid.to_string());
            if registration.validate_identity().is_err()
                || config.native_target_bundle.as_deref() != Some("com.apple.TextEdit")
                || config.initial_mode != RunMode::Observe
                || config.native_window_scope.is_some()
                || config.native_background_window_scope.is_some()
                || !pid_matches
            {
                return Err(
                    "owned document registration does not match observe configuration".into(),
                );
            }
            config.owned_document_registration = Some(registration);
        }
        if config.planning_backend == PlanningBackend::OwnedReference
            && (config.owned_document_registration.is_none()
                || config.native_target_bundle.as_deref() != Some("com.apple.TextEdit")
                || config.native_window_scope.is_some()
                || config.native_background_window_scope.is_some()
                || config.initial_mode != RunMode::Observe)
        {
            return Err("owned reference planning requires private TextEdit observation".into());
        }
        if allow_owned_document_save {
            let private_textedit_identity = config
                .owned_document_registration
                .as_deref()
                .is_some_and(|registration| {
                    config.native_target_bundle.as_deref() == Some("com.apple.TextEdit")
                        && settings
                            .get("native_target_pid")
                            .is_some_and(|pid| pid == &registration.pid.to_string())
                        && config.native_window_scope.is_none()
                        && config.native_background_window_scope.is_none()
                        && config.initial_mode == RunMode::Observe
                });
            if !private_textedit_identity
                || !matches!(
                    config.planning_backend,
                    PlanningBackend::OwnedReference | PlanningBackend::Model
                )
            {
                return Err(
                    "owned Save requires private TextEdit observation and an approved planner"
                        .into(),
                );
            }
        }
        config.allow_owned_document_save = allow_owned_document_save;
        Ok(config)
    }

    pub fn device_socket(&self) -> PathBuf {
        self.socket_dir.join("device.sock")
    }

    pub fn source_socket(&self) -> PathBuf {
        self.socket_dir.join("source.sock")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use interaction_contracts::desktop::handshake::PeerRole;
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn configure(mode: Option<&str>) -> Configure {
        let mut settings = BTreeMap::new();
        for (key, value) in [
            ("socket_dir", "/tmp/x"),
            ("db_path", "/tmp/x/host.db"),
            ("device_token", "0123456789abcdef0123456789abcdef"),
            ("device_epoch", "2"),
            ("source_token", "fedcba9876543210fedcba9876543210"),
            ("clock_origin_ms", "1700000000000"),
        ] {
            settings.insert(key.to_string(), value.to_string());
        }
        if let Some(mode) = mode {
            settings.insert("initial_mode".into(), mode.into());
        }
        Configure {
            role: PeerRole::Host,
            session_epoch: 1,
            settings,
            browser_page_profile: None,
        }
    }

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
        let bytes = serde_json::to_vec(&payload).unwrap();
        let mut frame = (bytes.len() as u32).to_be_bytes().to_vec();
        frame.extend(bytes);
        OwnedDocumentBootstrap::read(&mut &frame[..]).unwrap()
    }

    #[test]
    fn the_default_mode_is_observe_and_a_bad_one_is_refused() {
        assert_eq!(
            HostConfig::from_configure(&configure(None))
                .unwrap()
                .initial_mode,
            RunMode::Observe
        );
        assert_eq!(
            HostConfig::from_configure(&configure(Some("execute")))
                .unwrap()
                .initial_mode,
            RunMode::Execute
        );
        assert!(HostConfig::from_configure(&configure(Some("yolo"))).is_err());
        let config = HostConfig::from_configure(&configure(None)).unwrap();
        assert_eq!(config.device_epoch, 2);
        assert_eq!(config.source_socket(), PathBuf::from("/tmp/x/source.sock"));
        let mut selected_by_settings = configure(None);
        selected_by_settings
            .settings
            .insert("execution_domain".into(), "background_ax".into());
        assert!(
            HostConfig::from_configure(&selected_by_settings)
                .unwrap_err()
                .contains("Device manifest")
        );
    }

    #[test]
    fn interaction_fixture_is_an_exact_registered_native_window_scope() {
        let mut config = configure(None);
        config.settings.insert(
            "native_target_bundle".into(),
            "com.lingxi.computer-user.mvp-fixture".into(),
        );
        config
            .settings
            .insert("native_target_pid".into(), "42".into());
        config
            .settings
            .insert("native_target_window_id".into(), "window-7".into());
        let host = HostConfig::from_configure(&config).unwrap();
        let scope = host.native_window_scope.expect("registered native scope");
        assert_eq!(scope.app.as_str(), "com.lingxi.computer-user.mvp-fixture");
        assert_eq!(scope.pid, 42);
        assert_eq!(scope.window.as_str(), "window-7");

        config
            .settings
            .insert("native_target_bundle".into(), "unregistered.app".into());
        assert!(HostConfig::from_configure(&config).is_err());
    }

    #[test]
    fn background_window_scope_is_separate_and_birth_is_optional_but_atomic() {
        let mut config = configure(None);
        config.settings.insert(
            "native_target_bundle".into(),
            "com.lingxi.computer-user.mvp-fixture".into(),
        );
        config
            .settings
            .insert("native_target_pid".into(), "42".into());
        config
            .settings
            .insert("native_background_window_id".into(), "window-7".into());

        let passive = HostConfig::from_configure(&config).unwrap();
        assert!(passive.native_window_scope.is_none());
        let scope = passive
            .native_background_window_scope
            .expect("background evidence scope");
        assert_eq!(scope.app, "com.lingxi.computer-user.mvp-fixture");
        assert_eq!(scope.pid, 42);
        assert_eq!(scope.window, "window-7");
        assert_eq!(scope.process_start_seconds, None);
        assert_eq!(scope.process_start_micros, None);

        config.settings.insert(
            "native_target_process_start_seconds".into(),
            "1700000000".into(),
        );
        config
            .settings
            .insert("native_target_process_start_micros".into(), "123456".into());
        let active_scope = HostConfig::from_configure(&config)
            .unwrap()
            .native_background_window_scope
            .unwrap();
        assert_eq!(active_scope.process_start_seconds, Some(1_700_000_000));
        assert_eq!(active_scope.process_start_micros, Some(123_456));

        for (key, invalid) in [
            ("native_target_process_start_seconds", "01700000000"),
            ("native_target_process_start_seconds", "9007199254740992"),
            ("native_target_process_start_micros", "1000000"),
            ("native_target_pid", "042"),
            ("native_background_window_id", "window-07"),
        ] {
            let mut malformed = config.clone();
            malformed.settings.insert(key.into(), invalid.into());
            assert!(
                HostConfig::from_configure(&malformed).is_err(),
                "{key}={invalid}"
            );
        }

        let mut partial_pair = config.clone();
        partial_pair
            .settings
            .remove("native_target_process_start_micros");
        assert!(HostConfig::from_configure(&partial_pair).is_err());

        let mut mixed = config.clone();
        mixed
            .settings
            .insert("native_target_window_id".into(), "window-8".into());
        assert!(HostConfig::from_configure(&mixed).is_err());

        let mut birth_without_window = configure(None);
        birth_without_window.settings.insert(
            "native_target_process_start_seconds".into(),
            "1700000000".into(),
        );
        birth_without_window
            .settings
            .insert("native_target_process_start_micros".into(), "123456".into());
        assert!(HostConfig::from_configure(&birth_without_window).is_err());
    }

    #[test]
    fn owned_reference_requires_private_registration_and_never_changes_initial_mode() {
        let mut config = configure(None);
        config
            .settings
            .insert("planning_backend".into(), "owned_reference".into());
        assert!(HostConfig::from_configure(&config).is_err());
        config
            .settings
            .insert("native_target_bundle".into(), "com.apple.TextEdit".into());
        config
            .settings
            .insert("native_target_pid".into(), "1234".into());
        assert!(HostConfig::from_configure(&config).is_err());
        let valid =
            HostConfig::from_configure_with_owned_bootstrap(&config, Some(owned_bootstrap()))
                .unwrap();
        assert_eq!(valid.planning_backend, PlanningBackend::OwnedReference);
        assert_eq!(valid.initial_mode, RunMode::Observe);
        config
            .settings
            .insert("initial_mode".into(), "execute".into());
        assert!(
            HostConfig::from_configure_with_owned_bootstrap(&config, Some(owned_bootstrap()))
                .is_err()
        );
    }

    #[test]
    fn owned_save_setting_is_default_off_and_accepts_only_private_owned_planner_scopes() {
        let mut configure = configure(None);
        configure
            .settings
            .insert("planning_backend".into(), "owned_reference".into());
        configure
            .settings
            .insert("native_target_bundle".into(), "com.apple.TextEdit".into());
        configure
            .settings
            .insert("native_target_pid".into(), "1234".into());
        let baseline =
            HostConfig::from_configure_with_owned_bootstrap(&configure, Some(owned_bootstrap()))
                .unwrap();
        assert!(!baseline.allow_owned_document_save);

        configure.settings.insert("owned_save".into(), "1".into());
        assert!(HostConfig::from_configure(&configure).is_err());
        let enabled =
            HostConfig::from_configure_with_owned_bootstrap(&configure, Some(owned_bootstrap()))
                .unwrap();
        assert!(enabled.allow_owned_document_save);

        configure.settings.insert("owned_save".into(), "yes".into());
        assert!(
            HostConfig::from_configure_with_owned_bootstrap(&configure, Some(owned_bootstrap()),)
                .is_err()
        );

        configure.settings.insert("owned_save".into(), "1".into());
        configure
            .settings
            .insert("planning_backend".into(), "unconfigured".into());
        assert!(
            HostConfig::from_configure_with_owned_bootstrap(&configure, Some(owned_bootstrap()),)
                .is_err()
        );

        configure
            .settings
            .insert("planning_backend".into(), "model".into());
        assert!(HostConfig::from_configure(&configure).is_err());
        let model =
            HostConfig::from_configure_with_owned_bootstrap(&configure, Some(owned_bootstrap()))
                .unwrap();
        assert_eq!(model.planning_backend, PlanningBackend::Model);
        assert!(model.allow_owned_document_save);

        configure
            .settings
            .insert("native_target_pid".into(), "1235".into());
        assert!(
            HostConfig::from_configure_with_owned_bootstrap(&configure, Some(owned_bootstrap()),)
                .is_err()
        );
        configure
            .settings
            .insert("native_target_pid".into(), "1234".into());
        configure
            .settings
            .insert("native_target_bundle".into(), "other.app".into());
        assert!(
            HostConfig::from_configure_with_owned_bootstrap(&configure, Some(owned_bootstrap()),)
                .is_err()
        );
    }

    #[test]
    fn native_planning_is_explicit_and_reference_is_limited_to_registered_fixtures() {
        let mut c = configure(None);
        c.settings
            .insert("native_target_bundle".into(), "com.apple.TextEdit".into());
        assert_eq!(
            HostConfig::from_configure(&c).unwrap().planning_backend,
            PlanningBackend::Unconfigured
        );
        c.settings.insert(
            "native_target_bundle".into(),
            "com.lingxi.computer-user.mvp-fixture".into(),
        );
        assert_eq!(
            HostConfig::from_configure(&c).unwrap().planning_backend,
            PlanningBackend::Unconfigured,
            "native reference planning is never enabled by bundle identity alone"
        );
        c.settings
            .insert("native_target_bundle".into(), "com.apple.TextEdit".into());
        c.settings
            .insert("planning_backend".into(), "fixture".into());
        assert!(HostConfig::from_configure(&c).is_err());
        c.settings
            .insert("planning_backend".into(), "native_reference".into());
        assert!(HostConfig::from_configure(&c).is_err());
        c.settings.insert(
            "native_target_bundle".into(),
            "com.lingxi.computer-user.guard-fixture".into(),
        );
        assert_eq!(
            HostConfig::from_configure(&c).unwrap().planning_backend,
            PlanningBackend::NativeReference
        );
        c.settings.insert(
            "native_target_bundle".into(),
            "com.lingxi.computer-user.mvp-fixture".into(),
        );
        assert_eq!(
            HostConfig::from_configure(&c).unwrap().planning_backend,
            PlanningBackend::NativeReference
        );
        c.settings
            .insert("native_target_bundle".into(), "unregistered.app".into());
        assert!(HostConfig::from_configure(&c).is_err());
        c.settings
            .insert("planning_backend".into(), "unconfigured".into());
        assert!(
            HostConfig::from_configure(&c).is_ok(),
            "unknown apps can still be observed"
        );
    }

    #[test]
    fn browser_reference_requires_the_typed_browser_profile_and_no_native_scope() {
        let mut c = configure(None);
        c.settings
            .insert("planning_backend".into(), "browser_reference".into());
        assert!(HostConfig::from_configure(&c).is_err());

        c.browser_page_profile = Some(
            interaction_contracts::desktop::browser::BrowserPageProfile::FixtureBackgroundOption,
        );
        let config = HostConfig::from_configure(&c).unwrap();
        assert_eq!(config.planning_backend, PlanningBackend::BrowserReference);
        assert_eq!(
            config.browser_page_profile, c.browser_page_profile,
            "the profile comes from the typed Configure field"
        );

        c.settings
            .insert("native_target_bundle".into(), "com.apple.TextEdit".into());
        assert!(HostConfig::from_configure(&c).is_err());
    }

    #[test]
    fn only_a_matching_private_registration_can_set_owned_observation_without_window_scope() {
        let mut configure = configure(None);
        configure
            .settings
            .insert("native_target_bundle".into(), "com.apple.TextEdit".into());
        configure
            .settings
            .insert("native_target_pid".into(), "1234".into());
        configure.settings.insert(
            "owned_document_registration".into(),
            serde_json::to_string(&json!({
                "registration_id": "123e4567-e89b-42d3-a456-426614174000",
                "document_path": "/private/customer/document.txt"
            }))
            .unwrap(),
        );

        let public_config = HostConfig::from_configure(&configure).unwrap();
        assert!(public_config.owned_document_registration.is_none());

        let config =
            HostConfig::from_configure_with_owned_bootstrap(&configure, Some(owned_bootstrap()))
                .unwrap();
        assert_eq!(
            config.native_target_bundle.as_deref(),
            Some("com.apple.TextEdit")
        );
        assert_eq!(config.initial_mode, RunMode::Observe);
        assert!(
            config.native_window_scope.is_none(),
            "the owned registration is not an input grant"
        );
        assert_eq!(
            config.owned_document_registration.as_ref().unwrap().pid,
            1234
        );

        let mut mismatch = configure.clone();
        mismatch
            .settings
            .insert("native_target_pid".into(), "1235".into());
        assert!(
            HostConfig::from_configure_with_owned_bootstrap(&mismatch, Some(owned_bootstrap()))
                .is_err()
        );

        let mut wrong_bundle = configure.clone();
        wrong_bundle
            .settings
            .insert("native_target_bundle".into(), "other.app".into());
        assert!(
            HostConfig::from_configure_with_owned_bootstrap(&wrong_bundle, Some(owned_bootstrap()))
                .is_err()
        );

        let mut noncanonical_pid = configure.clone();
        noncanonical_pid
            .settings
            .insert("native_target_pid".into(), "01234".into());
        assert!(
            HostConfig::from_configure_with_owned_bootstrap(
                &noncanonical_pid,
                Some(owned_bootstrap())
            )
            .is_err()
        );

        let mut execute = configure.clone();
        execute
            .settings
            .insert("initial_mode".into(), "execute".into());
        assert!(
            HostConfig::from_configure_with_owned_bootstrap(&execute, Some(owned_bootstrap()))
                .is_err()
        );

        let mut scoped = configure;
        scoped
            .settings
            .insert("native_target_window_id".into(), "window-7".into());
        assert!(
            HostConfig::from_configure_with_owned_bootstrap(&scoped, Some(owned_bootstrap()))
                .is_err()
        );
    }
}
