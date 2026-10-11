//! The supervisor selects the source explicitly; native failures never select a fixture.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use desktop_ipc::SharedClock;
use interaction_contracts::desktop::handshake::Configure;
use interaction_contracts::desktop::owned_registration::{
    MAX_OWNED_REGISTRATION_BYTES, OwnedDocumentRegistration,
};
use interaction_contracts::desktop::task::RowTableScope;
use serde::{Deserialize, Serialize};

pub(crate) const REGISTERED_EDITOR_BUNDLES: &[&str] = &[
    "com.lingxi.computer-user.guard-fixture",
    "com.lingxi.computer-user.mvp-fixture",
    "com.apple.TextEdit",
];

#[derive(Debug, Clone)]
pub enum SourceBackend {
    Fixture { world_log: PathBuf },
    Native(Box<NativeConfig>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeCaptureMode {
    Watch,
    Probe,
}

#[derive(Debug, Clone)]
pub struct NativeConfig {
    pub helper_path: PathBuf,
    pub bundle_id: String,
    pub pid: Option<u32>,
    pub assets_dir: PathBuf,
    pub timeout_ms: u64,
    pub mode: NativeCaptureMode,
    pub text_policy: Option<TextReadPolicy>,
    /// Read-only exact public table scope; it grants no input authority.
    pub table_scope: Option<RowTableScope>,
    /// Fixed logical target for a persistent AX-only watch. This carries no
    /// permission to focus, capture pixels, or submit input.
    pub background_target: Option<BackgroundNativeTarget>,
    /// Only read from the private length-prefixed stdin bootstrap, never Configure.
    pub owned_document_bootstrap: Option<Box<OwnedDocumentRegistration>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundNativeTarget {
    pub window_id: String,
    /// Optional caller-pinned birth identity. When omitted, the Native helper
    /// reads and latches the actual process birth for this watch lifetime.
    pub process: Option<interaction_contracts::desktop::source::KernelProcessIdentity>,
}

impl BackgroundNativeTarget {
    pub fn validate(&self, bundle_id: &str, pid: Option<u32>) -> Result<(), String> {
        let valid_window_id = self
            .window_id
            .strip_prefix("window-")
            .and_then(|value| value.parse::<u32>().ok())
            .is_some_and(|value| value > 0 && value.to_string() == self.window_id[7..]);
        if !valid_window_id
            || pid.is_none_or(|value| value == 0 || value > i32::MAX as u32)
            || bundle_id.trim().is_empty()
            || bundle_id.len() > 256
            || bundle_id.chars().any(char::is_control)
        {
            return Err("invalid fixed background AX target identity".into());
        }
        if self.process.as_ref().is_some_and(|process| {
            Some(process.pid) != pid
                || process.start_seconds > 9_007_199_254_740_991
                || process.validate().is_err()
        }) {
            return Err("invalid fixed background AX process birth identity".into());
        }
        Ok(())
    }
}

/// Re-issue a fresh bootstrap only when forwarding the Source process's
/// already-accepted, read-only TextEdit scope to its native helper child.
/// Identity stays unchanged; the native helper still independently verifies
/// the live process and private file on each observation.
pub(crate) fn write_owned_document_child_prefix(
    config: &NativeConfig,
    writer: &mut impl Write,
    now_unix_ms: u64,
) -> Result<(), String> {
    let invalid = || "invalid private owned document registration".to_string();
    let Some(registration) = config.owned_document_bootstrap.as_deref() else {
        return Err(invalid());
    };
    if config.mode != NativeCaptureMode::Watch
        || config.bundle_id != "com.apple.TextEdit"
        || config.pid != Some(registration.pid)
        || config.text_policy.is_some()
        || config.table_scope.is_some()
        || config.background_target.is_some()
        || registration.bundle_id != config.bundle_id
        || registration.validate_identity().is_err()
    {
        return Err(invalid());
    }
    let mut child_registration = (*registration).clone();
    child_registration.issued_unix_ms = now_unix_ms;
    write_owned_document_prefix(&child_registration, writer, now_unix_ms)
}

pub fn read_owned_document_prefix(
    reader: &mut impl Read,
) -> Result<OwnedDocumentRegistration, String> {
    let mut header = [0u8; 4];
    reader
        .read_exact(&mut header)
        .map_err(|_| "invalid private owned document registration".to_string())?;
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 || length > MAX_OWNED_REGISTRATION_BYTES {
        return Err("invalid private owned document registration".into());
    }
    let mut bytes = vec![0u8; length];
    reader
        .read_exact(&mut bytes)
        .map_err(|_| "invalid private owned document registration".to_string())?;
    let registration: OwnedDocumentRegistration = serde_json::from_slice(&bytes)
        .map_err(|_| "invalid private owned document registration".to_string())?;
    registration.validate_at(unix_ms())?;
    Ok(registration)
}

pub fn write_owned_document_prefix(
    registration: &OwnedDocumentRegistration,
    writer: &mut impl Write,
    now_unix_ms: u64,
) -> Result<(), String> {
    registration.validate_at(now_unix_ms)?;
    let bytes = serde_json::to_vec(registration)
        .map_err(|_| "invalid private owned document registration".to_string())?;
    if bytes.is_empty() || bytes.len() > MAX_OWNED_REGISTRATION_BYTES {
        return Err("invalid private owned document registration".into());
    }
    let length = u32::try_from(bytes.len())
        .map_err(|_| "invalid private owned document registration".to_string())?;
    writer
        .write_all(&length.to_be_bytes())
        .and_then(|()| writer.write_all(&bytes))
        .map_err(|_| "writing private owned document registration failed".to_string())
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TextReadPolicy {
    RegisteredEditor { window_id: String },
}

#[derive(Debug, Clone)]
pub struct SourceConfig {
    pub socket_dir: PathBuf,
    pub host_token: String,
    pub image_token: Option<String>,
    pub session_epoch: u64,
    pub clock: SharedClock,
    pub clock_domain: String,
    pub backend: SourceBackend,
    pub interval_ms: u64,
}

fn setting<'a>(settings: &'a BTreeMap<String, String>, key: &str) -> Result<&'a str, String> {
    settings
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| format!("the supervisor did not give {key}"))
}

fn absolute_path(settings: &BTreeMap<String, String>, key: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(setting(settings, key)?);
    if !path.is_absolute() {
        return Err(format!("{key} must be an absolute path"));
    }
    Ok(path)
}

impl SourceConfig {
    pub fn from_configure(configure: &Configure) -> Result<Self, String> {
        Self::from_configure_with_owned_document(configure, None)
    }

    pub fn from_configure_with_owned_document(
        configure: &Configure,
        owned_document_bootstrap: Option<OwnedDocumentRegistration>,
    ) -> Result<Self, String> {
        let settings = &configure.settings;
        let number = |key: &str| -> Result<u64, String> {
            setting(settings, key)?
                .parse()
                .map_err(|_| format!("{key} is not a number"))
        };
        let backend = match settings.get("source_backend").map(String::as_str) {
            None | Some("fixture") => SourceBackend::Fixture {
                world_log: PathBuf::from(setting(settings, "world_log")?),
            },
            Some("native") => {
                let mode = match settings.get("native_capture_mode").map(String::as_str) {
                    None | Some("watch") => NativeCaptureMode::Watch,
                    Some("probe") => NativeCaptureMode::Probe,
                    Some(other) => return Err(format!("unknown native_capture_mode {other}")),
                };
                let bundle_id = setting(settings, "target_bundle_id")?.to_string();
                if bundle_id.trim().is_empty()
                    || bundle_id.len() > 256
                    || bundle_id.chars().any(char::is_control)
                {
                    return Err("target_bundle_id must be a nonempty bounded identifier".into());
                }
                let timeout_ms = settings.get("native_timeout_ms").map_or(Ok(1_500), |v| {
                    v.parse().map_err(|_| "native_timeout_ms is not a number")
                })?;
                if !(100..=10_000).contains(&timeout_ms) {
                    return Err("native_timeout_ms must be between 100 and 10000".into());
                }
                let pid = settings
                    .get("target_pid")
                    .map(|value| {
                        value
                            .parse::<u32>()
                            .ok()
                            .filter(|pid| *pid > 0)
                            .ok_or("target_pid must be a positive process id")
                    })
                    .transpose()?;
                let text_policy = match settings.get("native_text_policy").map(String::as_str) {
                    None => None,
                    Some("registered_editor") => {
                        let window = setting(settings, "native_text_window_id")?;
                        if pid.is_none()
                            || !REGISTERED_EDITOR_BUNDLES.contains(&bundle_id.as_str())
                            || window
                                .strip_prefix("window-")
                                .and_then(|s| s.parse::<u32>().ok())
                                .is_none_or(|n| n == 0)
                        {
                            return Err(
                                "registered text policy needs a registered app/PID/window".into()
                            );
                        }
                        Some(TextReadPolicy::RegisteredEditor {
                            window_id: window.into(),
                        })
                    }
                    Some(_) => return Err("unknown native text policy".into()),
                };
                let table_scope = settings
                    .get("native_table_scope")
                    .map(|raw| {
                        if raw.len() > 4096 {
                            return Err(
                                "native table scope exceeds its request byte budget".to_string()
                            );
                        }
                        let scope: RowTableScope = serde_json::from_str(raw)
                            .map_err(|_| "invalid native table scope".to_string())?;
                        scope.validate()?;
                        if pid.is_none_or(|pid| pid > i32::MAX as u32) || text_policy.is_none() {
                            return Err(
                                "native table scope needs a fixed registered app/PID/window"
                                    .to_string(),
                            );
                        }
                        Ok(scope)
                    })
                    .transpose()?;
                let background_window = settings.get("native_background_window_id");
                let process_seconds = settings.get("native_target_process_start_seconds");
                let process_micros = settings.get("native_target_process_start_micros");
                let pinned_process =
                    match (process_seconds, process_micros) {
                        (None, None) => None,
                        (Some(seconds), Some(micros)) => Some(
                            interaction_contracts::desktop::source::KernelProcessIdentity {
                                pid: pid.ok_or("background AX target requires target_pid")?,
                                start_seconds: seconds.parse().map_err(
                                    |_| "native_target_process_start_seconds is not a number",
                                )?,
                                start_micros: micros.parse().map_err(
                                    |_| "native_target_process_start_micros is not a number",
                                )?,
                            },
                        ),
                        _ => return Err(
                            "background AX process birth must include seconds and micros together"
                                .into(),
                        ),
                    };
                let background_target = match background_window {
                    None if pinned_process.is_none() => None,
                    None => {
                        return Err("background AX process birth requires a fixed window id".into());
                    }
                    Some(window_id) => {
                        let target = BackgroundNativeTarget {
                            window_id: window_id.clone(),
                            process: pinned_process,
                        };
                        target.validate(&bundle_id, pid)?;
                        if mode != NativeCaptureMode::Watch
                            || table_scope.is_some()
                            || text_policy.is_some()
                            || owned_document_bootstrap.is_some()
                        {
                            return Err(
                                "background AX target requires an unscoped native watch".into()
                            );
                        }
                        Some(target)
                    }
                };
                if let Some(bootstrap) = &owned_document_bootstrap {
                    bootstrap.validate_at(unix_ms())?;
                    if mode != NativeCaptureMode::Watch
                        || text_policy.is_some()
                        || table_scope.is_some()
                        || pid != Some(bootstrap.pid)
                        || bundle_id != bootstrap.bundle_id
                    {
                        return Err(
                            "private owned document bootstrap does not match native watch".into(),
                        );
                    }
                }
                SourceBackend::Native(Box::new(NativeConfig {
                    helper_path: absolute_path(settings, "native_helper_path")?,
                    bundle_id,
                    pid,
                    assets_dir: absolute_path(settings, "native_assets_dir")?,
                    timeout_ms,
                    mode,
                    text_policy,
                    table_scope,
                    background_target,
                    owned_document_bootstrap: owned_document_bootstrap.clone().map(Box::new),
                }))
            }
            Some(other) => return Err(format!("unknown source_backend {other}")),
        };
        let native = matches!(backend, SourceBackend::Native(_));
        if owned_document_bootstrap.is_some() && !native {
            return Err("private owned document bootstrap requires native watch".into());
        }
        let image_token = settings.get("image_token").cloned();
        if image_token.as_ref().is_some_and(|token| {
            !native
                || token.len() != 32
                || !token
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }) {
            return Err(
                "image_token requires native source and 32 lowercase hexadecimal bytes".into(),
            );
        }
        let interval_ms = settings
            .get("interval_ms")
            .map_or(Ok(if native { 1_000 } else { 50 }), |value| {
                value.parse().map_err(|_| "interval_ms is not a number")
            })?;
        if !(5..=60_000).contains(&interval_ms) {
            return Err("interval_ms must be between 5 and 60000".into());
        }
        let clock_domain = settings.get("clock_domain").cloned().unwrap_or_else(|| {
            if native {
                // SharedClock uses wall time. This entry slice does not mislabel it.
                format!("macos/{}/wall", configure.session_epoch)
            } else {
                format!("sim/{}/monotonic", configure.session_epoch)
            }
        });
        if clock_domain.trim().is_empty() || clock_domain.chars().any(char::is_control) {
            return Err("clock_domain must be a nonempty identifier".into());
        }
        Ok(Self {
            socket_dir: PathBuf::from(setting(settings, "socket_dir")?),
            host_token: setting(settings, "host_token")?.to_string(),
            image_token,
            session_epoch: configure.session_epoch,
            clock: SharedClock::from_origin(number("clock_origin_ms")?),
            clock_domain,
            backend,
            interval_ms,
        })
    }

    pub fn socket_path(&self) -> PathBuf {
        self.socket_dir.join("source.sock")
    }
}

/// Dynamic scoped reads are available only on the fixed, registered Native
/// watch lane. Probe workers have immutable request configuration, fixture
/// snapshots are not native scoped certificates, and private owned-document
/// sessions have a separate source contract.
pub(crate) fn validate_refresh_table_scope(
    config: &SourceConfig,
    table_scope: Option<&RowTableScope>,
) -> Result<(), &'static str> {
    let Some(scope) = table_scope else {
        return Ok(());
    };
    if scope.validate().is_err() {
        return Err("invalid_table_scope");
    }
    let SourceBackend::Native(native) = &config.backend else {
        return Err("table_scope_requires_native_watch");
    };
    let registered_window = matches!(
        &native.text_policy,
        Some(TextReadPolicy::RegisteredEditor { window_id })
            if window_id.len() <= 256
                && window_id.strip_prefix("window-")
                    .and_then(|value| value.parse::<u32>().ok())
                    .is_some_and(|number| number > 0)
    );
    if native.mode != NativeCaptureMode::Watch
        || native.owned_document_bootstrap.is_some()
        || native.background_target.is_some()
        || !REGISTERED_EDITOR_BUNDLES.contains(&native.bundle_id.as_str())
        || native
            .pid
            .is_none_or(|pid| pid == 0 || pid > i32::MAX as u32)
        || !registered_window
    {
        return Err("table_scope_requires_registered_watch_target");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use interaction_contracts::desktop::handshake::PeerRole;
    use std::io::Cursor;

    fn owned_registration(issued_unix_ms: u64) -> OwnedDocumentRegistration {
        OwnedDocumentRegistration {
            version: 1,
            registration_id: "11111111-1111-4111-8111-111111111111".into(),
            pid: 42,
            start_seconds: 1_700_000_000,
            start_micros: 123_456,
            executable_path: "/System/Applications/TextEdit.app/Contents/MacOS/TextEdit".into(),
            bundle_id: "com.apple.TextEdit".into(),
            document_path: "/private/tmp/owned-test/document.txt".into(),
            document_sha256: "a".repeat(64),
            document_dev: 1,
            document_ino: 2,
            directory_dev: 1,
            directory_ino: 3,
            issued_unix_ms,
        }
    }

    fn configure() -> Configure {
        Configure {
            role: PeerRole::Source,
            session_epoch: 2,
            browser_page_profile: None,
            settings: [
                ("socket_dir", "/tmp/cu"),
                ("host_token", "token"),
                ("clock_origin_ms", "1"),
                ("world_log", "/tmp/cu/world.log"),
            ]
            .map(|(key, value)| (key.into(), value.into()))
            .into(),
        }
    }

    #[test]
    fn fixture_is_the_default_and_native_requires_an_explicit_configuration() {
        let mut config = configure();
        let source = SourceConfig::from_configure(&config).unwrap();
        assert!(matches!(source.backend, SourceBackend::Fixture { .. }));
        config
            .settings
            .insert("source_backend".into(), "native".into());
        config.settings.remove("world_log");
        assert!(SourceConfig::from_configure(&config).is_err());
        for (key, value) in [
            ("native_helper_path", "/tmp/cu/helper"),
            ("native_assets_dir", "/tmp/cu/assets"),
            ("target_bundle_id", "com.apple.TextEdit"),
        ] {
            config.settings.insert(key.into(), value.into());
        }
        let source = SourceConfig::from_configure(&config).unwrap();
        assert!(matches!(source.backend, SourceBackend::Native(_)));
        assert_eq!(source.clock_domain, "macos/2/wall");
        assert_eq!(source.interval_ms, 1_000);
        config
            .settings
            .insert("native_helper_path".into(), "relative".into());
        assert!(SourceConfig::from_configure(&config).is_err());
        config
            .settings
            .insert("source_backend".into(), "typo".into());
        assert!(SourceConfig::from_configure(&config).is_err());
    }
    #[test]
    fn registered_text_policy_requires_a_fixed_registered_process_and_window() {
        let mut configure = configure();
        for (key, value) in [
            ("source_backend", "native"),
            ("native_helper_path", "/tmp/cu/helper"),
            ("native_assets_dir", "/tmp/cu/assets"),
            ("target_bundle_id", "com.apple.TextEdit"),
            ("target_pid", "42"),
            ("native_text_policy", "registered_editor"),
            ("native_text_window_id", "window-1"),
        ] {
            configure.settings.insert(key.into(), value.into());
        }
        let source = SourceConfig::from_configure(&configure).unwrap();
        let SourceBackend::Native(native) = source.backend else {
            panic!("expected native source");
        };
        assert!(native.table_scope.is_none());
        assert!(matches!(
            native.text_policy,
            Some(TextReadPolicy::RegisteredEditor { .. })
        ));
        for (key, value) in [
            ("target_pid", "0"),
            ("native_text_window_id", "window-0"),
            ("target_bundle_id", "unregistered.app"),
            ("native_text_policy", "anything"),
        ] {
            let mut bad = configure.clone();
            bad.settings.insert(key.into(), value.into());
            assert!(SourceConfig::from_configure(&bad).is_err(), "{key}");
        }
        configure.settings.remove("target_pid");
        assert!(SourceConfig::from_configure(&configure).is_err());
    }

    #[test]
    fn background_target_is_a_fixed_ax_only_watch_profile() {
        let mut config = configure();
        for (key, value) in [
            ("source_backend", "native"),
            ("native_helper_path", "/tmp/cu/helper"),
            ("native_assets_dir", "/tmp/cu/assets"),
            ("target_bundle_id", "com.apple.TextEdit"),
            ("target_pid", "42"),
            ("native_background_window_id", "window-7"),
            ("native_target_process_start_seconds", "1700000000"),
            ("native_target_process_start_micros", "123456"),
        ] {
            config.settings.insert(key.into(), value.into());
        }
        let background_source = SourceConfig::from_configure(&config).unwrap();
        let SourceBackend::Native(native) = background_source.backend.clone() else {
            panic!("expected native source");
        };
        assert_eq!(
            native.background_target,
            Some(BackgroundNativeTarget {
                window_id: "window-7".into(),
                process: Some(
                    interaction_contracts::desktop::source::KernelProcessIdentity {
                        pid: 42,
                        start_seconds: 1_700_000_000,
                        start_micros: 123_456,
                    }
                ),
            })
        );
        assert!(native.table_scope.is_none());
        assert!(native.text_policy.is_none());
        let scope = RowTableScope {
            scroll_path: vec![interaction_contracts::desktop::task::RowContainerScope {
                role: interaction_contracts::desktop::task::RowContainerRole::AxScrollArea,
                label: "Archive".into(),
            }],
            column_headers: vec!["Name".into()],
        };
        assert_eq!(
            validate_refresh_table_scope(&background_source, Some(&scope)),
            Err("table_scope_requires_registered_watch_target")
        );
        assert!(validate_refresh_table_scope(&background_source, None).is_ok());

        let mut source_latches_birth = config.clone();
        source_latches_birth
            .settings
            .remove("native_target_process_start_seconds");
        source_latches_birth
            .settings
            .remove("native_target_process_start_micros");
        let SourceBackend::Native(native) = SourceConfig::from_configure(&source_latches_birth)
            .unwrap()
            .backend
        else {
            panic!("expected native source");
        };
        assert!(matches!(
            native.background_target,
            Some(BackgroundNativeTarget { process: None, .. })
        ));

        for key in [
            "native_background_window_id",
            "native_target_process_start_seconds",
            "native_target_process_start_micros",
        ] {
            let mut partial = config.clone();
            partial.settings.remove(key);
            assert!(
                SourceConfig::from_configure(&partial).is_err(),
                "partial {key}"
            );
        }
        for (key, value) in [
            ("native_background_window_id", "window-07"),
            ("native_background_window_id", "window-0"),
            ("native_target_process_start_seconds", "0"),
            ("native_target_process_start_seconds", "9007199254740992"),
            ("native_target_process_start_micros", "1000000"),
            ("target_pid", "2147483648"),
        ] {
            let mut invalid = config.clone();
            invalid.settings.insert(key.into(), value.into());
            assert!(
                SourceConfig::from_configure(&invalid).is_err(),
                "{key}={value}"
            );
        }

        let mut probe = config.clone();
        probe
            .settings
            .insert("native_capture_mode".into(), "probe".into());
        assert!(SourceConfig::from_configure(&probe).is_err());
        let mut table = config.clone();
        table
            .settings
            .insert("native_text_policy".into(), "registered_editor".into());
        table
            .settings
            .insert("native_text_window_id".into(), "window-7".into());
        table.settings.insert("native_table_scope".into(),
            r#"{"scroll_path":[{"role":"ax_scroll_area","label":"Archive"}],"column_headers":["Name"]}"#.into());
        assert!(SourceConfig::from_configure(&table).is_err());
    }

    #[test]
    fn interaction_fixture_text_policy_is_registered_only_for_native_watch() {
        let mut configure = configure();
        for (key, value) in [
            ("source_backend", "native"),
            ("native_helper_path", "/tmp/cu/helper"),
            ("native_assets_dir", "/tmp/cu/assets"),
            ("target_bundle_id", "com.lingxi.computer-user.mvp-fixture"),
            ("target_pid", "42"),
            ("native_text_policy", "registered_editor"),
            ("native_text_window_id", "window-7"),
        ] {
            configure.settings.insert(key.into(), value.into());
        }
        let SourceBackend::Native(native) =
            SourceConfig::from_configure(&configure).unwrap().backend
        else {
            panic!("expected native source");
        };
        assert!(native.table_scope.is_none());
        assert!(matches!(
            native.text_policy,
            Some(TextReadPolicy::RegisteredEditor { window_id }) if window_id == "window-7"
        ));

        configure
            .settings
            .insert("target_bundle_id".into(), "unregistered.app".into());
        assert!(SourceConfig::from_configure(&configure).is_err());
    }

    #[test]
    fn table_scope_requires_a_fixed_window_and_only_accepts_bounded_public_scope() {
        let mut config = configure();
        for (key, value) in [
            ("source_backend", "native"),
            ("native_helper_path", "/tmp/cu/helper"),
            ("native_assets_dir", "/tmp/cu/assets"),
            ("target_bundle_id", "com.lingxi.computer-user.mvp-fixture"),
            ("target_pid", "42"),
            ("native_text_policy", "registered_editor"),
            ("native_text_window_id", "window-7"),
        ] {
            config.settings.insert(key.into(), value.into());
        }
        let scope = serde_json::json!({
            "scroll_path": [{"role": "ax_scroll_area", "label": "Archive"}],
            "column_headers": ["Name"]
        });
        config
            .settings
            .insert("native_table_scope".into(), scope.to_string());
        let SourceBackend::Native(native) = SourceConfig::from_configure(&config).unwrap().backend
        else {
            panic!("expected native source");
        };
        assert_eq!(
            serde_json::to_value(native.table_scope.unwrap()).unwrap(),
            scope
        );

        for key in ["target_pid", "native_text_policy", "native_text_window_id"] {
            let mut bad = config.clone();
            bad.settings.remove(key);
            assert!(SourceConfig::from_configure(&bad).is_err(), "missing {key}");
        }
        let mut invalid_pid = config.clone();
        invalid_pid
            .settings
            .insert("target_pid".into(), "2147483648".into());
        assert!(SourceConfig::from_configure(&invalid_pid).is_err());
        for malformed in [
            serde_json::json!({"scroll_path": [], "column_headers": ["Name"]}),
            serde_json::json!({"scroll_path": [{"role":"ax_scroll_area", "label":"Archive", "instance_ref":"old"}], "column_headers":["Name"]}),
            serde_json::json!({"scroll_path": [{"role":"ax_scroll_area", "label":"Archive"}], "column_headers":["Name"], "row_index": 101}),
            serde_json::json!({"scroll_path": [{"role":"ax_scroll_area", "label":"Archive"}], "column_headers":["Name", " name "]}),
        ] {
            let mut bad = config.clone();
            bad.settings
                .insert("native_table_scope".into(), malformed.to_string());
            assert!(SourceConfig::from_configure(&bad).is_err(), "{malformed}");
        }
        config
            .settings
            .insert("native_table_scope".into(), " ".repeat(4097));
        assert!(SourceConfig::from_configure(&config).is_err());
    }

    #[test]
    fn dynamic_refresh_scope_requires_registered_native_watch_and_none_clears_any_lane() {
        let mut native_configure = configure();
        for (key, value) in [
            ("source_backend", "native"),
            ("native_helper_path", "/tmp/cu/helper"),
            ("native_assets_dir", "/tmp/cu/assets"),
            ("target_bundle_id", "com.lingxi.computer-user.mvp-fixture"),
            ("target_pid", "42"),
            ("native_text_policy", "registered_editor"),
            ("native_text_window_id", "window-7"),
        ] {
            native_configure.settings.insert(key.into(), value.into());
        }
        let source = SourceConfig::from_configure(&native_configure).unwrap();
        let scope = RowTableScope {
            scroll_path: vec![interaction_contracts::desktop::task::RowContainerScope {
                role: interaction_contracts::desktop::task::RowContainerRole::AxScrollArea,
                label: "Archive".into(),
            }],
            column_headers: vec!["Name".into()],
        };
        assert!(validate_refresh_table_scope(&source, Some(&scope)).is_ok());
        assert!(validate_refresh_table_scope(&source, None).is_ok());

        let mut probe = source.clone();
        let SourceBackend::Native(native) = &mut probe.backend else {
            unreachable!();
        };
        native.mode = NativeCaptureMode::Probe;
        assert_eq!(
            validate_refresh_table_scope(&probe, Some(&scope)),
            Err("table_scope_requires_registered_watch_target")
        );

        let fixture = SourceConfig::from_configure(&configure()).unwrap();
        assert_eq!(
            validate_refresh_table_scope(&fixture, Some(&scope)),
            Err("table_scope_requires_native_watch")
        );
        assert!(validate_refresh_table_scope(&fixture, None).is_ok());

        let mut unregistered = source.clone();
        let SourceBackend::Native(native) = &mut unregistered.backend else {
            unreachable!();
        };
        native.bundle_id = "unregistered.app".into();
        assert!(validate_refresh_table_scope(&unregistered, Some(&scope)).is_err());

        let mut no_fixed_window = source;
        let SourceBackend::Native(native) = &mut no_fixed_window.backend else {
            unreachable!();
        };
        native.text_policy = None;
        assert!(validate_refresh_table_scope(&no_fixed_window, Some(&scope)).is_err());

        let mut owned_configure = configure();
        for (key, value) in [
            ("source_backend", "native"),
            ("native_helper_path", "/tmp/cu/helper"),
            ("native_assets_dir", "/tmp/cu/assets"),
            ("target_bundle_id", "com.apple.TextEdit"),
            ("target_pid", "42"),
        ] {
            owned_configure.settings.insert(key.into(), value.into());
        }
        let owned = SourceConfig::from_configure_with_owned_document(
            &owned_configure,
            Some(owned_registration(unix_ms())),
        )
        .unwrap();
        assert_eq!(
            validate_refresh_table_scope(&owned, Some(&scope)),
            Err("table_scope_requires_registered_watch_target")
        );
        assert!(validate_refresh_table_scope(&owned, None).is_ok());
    }

    #[test]
    fn private_owned_bootstrap_uses_bounded_length_prefix_and_strict_json() {
        let now = unix_ms();
        let registration = owned_registration(now);
        let mut framed = Vec::new();
        write_owned_document_prefix(&registration, &mut framed, now).unwrap();
        let mut input = Cursor::new(framed.clone());
        assert_eq!(
            read_owned_document_prefix(&mut input).unwrap(),
            registration
        );
        assert_eq!(input.position() as usize, framed.len());

        let payload = serde_json::to_vec(&registration).unwrap();
        for malformed in [
            String::from_utf8(payload.clone()).unwrap().replacen(
                "\"version\":1,",
                "\"version\":1,\"version\":1,",
                1,
            ),
            String::from_utf8(payload.clone())
                .unwrap()
                .trim_end_matches('}')
                .to_string()
                + ",\"unexpected\":true}",
        ] {
            let malformed = malformed.into_bytes();
            let mut bytes = (malformed.len() as u32).to_be_bytes().to_vec();
            bytes.extend(malformed);
            assert!(read_owned_document_prefix(&mut Cursor::new(bytes)).is_err());
        }
        assert!(read_owned_document_prefix(&mut Cursor::new(8_193_u32.to_be_bytes())).is_err());
        assert!(write_owned_document_prefix(&registration, &mut Vec::new(), now + 5_001).is_err());
        assert!(
            write_owned_document_prefix(&owned_registration(now + 1_001), &mut Vec::new(), now)
                .is_err()
        );
        let stale = serde_json::to_vec(&owned_registration(now.saturating_sub(5_001))).unwrap();
        let mut stale_frame = (stale.len() as u32).to_be_bytes().to_vec();
        stale_frame.extend(stale);
        assert!(read_owned_document_prefix(&mut Cursor::new(stale_frame)).is_err());
    }

    #[test]
    fn accepted_owned_source_scope_gets_a_fresh_child_prefix_without_changing_identity() {
        let accepted_at = unix_ms();
        let registration = owned_registration(accepted_at);
        let mut configure = configure();
        for (key, value) in [
            ("source_backend", "native"),
            ("native_helper_path", "/tmp/cu/helper"),
            ("native_assets_dir", "/tmp/cu/assets"),
            ("target_bundle_id", "com.apple.TextEdit"),
            ("target_pid", "42"),
        ] {
            configure.settings.insert(key.into(), value.into());
        }
        let accepted = SourceConfig::from_configure_with_owned_document(
            &configure,
            Some(registration.clone()),
        )
        .unwrap();
        let SourceBackend::Native(mut native) = accepted.backend else {
            panic!("accepted owned registration must select native watch");
        };

        // Model the long-lived Source after its one-time Main bootstrap has
        // aged out. Its private child handoff may refresh freshness only.
        native
            .owned_document_bootstrap
            .as_mut()
            .unwrap()
            .issued_unix_ms = accepted_at.saturating_sub(6_000);
        let child_started_at = unix_ms();
        let mut framed = Vec::new();
        write_owned_document_child_prefix(&native, &mut framed, child_started_at).unwrap();
        let child_registration = read_owned_document_prefix(&mut Cursor::new(framed)).unwrap();
        let mut expected = registration.clone();
        expected.issued_unix_ms = child_started_at;
        assert_eq!(child_registration, expected);

        let original = native.owned_document_bootstrap.as_deref().unwrap();
        assert_eq!(original.issued_unix_ms, accepted_at.saturating_sub(6_000));
        let document_path = original.document_path.clone();

        let mut wrong_pid = native.clone();
        wrong_pid.pid = Some(43);
        let error =
            write_owned_document_child_prefix(&wrong_pid, &mut Vec::new(), child_started_at)
                .unwrap_err();
        assert!(!error.contains(&document_path));

        let mut wrong_bundle = native.clone();
        wrong_bundle.bundle_id = "com.example.Other".into();
        let error =
            write_owned_document_child_prefix(&wrong_bundle, &mut Vec::new(), child_started_at)
                .unwrap_err();
        assert!(!error.contains(&document_path));

        let mut invalid_identity = native;
        invalid_identity
            .owned_document_bootstrap
            .as_mut()
            .unwrap()
            .registration_id = "not-a-registration-id".into();
        let error =
            write_owned_document_child_prefix(&invalid_identity, &mut Vec::new(), child_started_at)
                .unwrap_err();
        assert!(!error.contains(&document_path));
    }

    #[test]
    fn private_owned_bootstrap_requires_an_exact_native_watch_and_no_public_text_policy() {
        let now = unix_ms();
        let registration = owned_registration(now);
        let mut configure = configure();
        for (key, value) in [
            ("source_backend", "native"),
            ("native_helper_path", "/tmp/cu/helper"),
            ("native_assets_dir", "/tmp/cu/assets"),
            ("target_bundle_id", "com.apple.TextEdit"),
            ("target_pid", "42"),
        ] {
            configure.settings.insert(key.into(), value.into());
        }
        let source = SourceConfig::from_configure_with_owned_document(
            &configure,
            Some(registration.clone()),
        )
        .unwrap();
        let SourceBackend::Native(native) = source.backend else {
            panic!("expected native source");
        };
        assert_eq!(native.mode, NativeCaptureMode::Watch);
        assert!(native.table_scope.is_none());
        assert!(native.text_policy.is_none());
        assert!(native.owned_document_bootstrap.is_some());

        for (key, value) in [
            ("target_pid", "43"),
            ("target_bundle_id", "com.example.Other"),
            ("native_capture_mode", "probe"),
            ("source_backend", "fixture"),
        ] {
            let mut bad = configure.clone();
            bad.settings.insert(key.into(), value.into());
            assert!(
                SourceConfig::from_configure_with_owned_document(&bad, Some(registration.clone()))
                    .is_err(),
                "{key}"
            );
        }
        let mut public_policy = configure;
        public_policy
            .settings
            .insert("native_text_policy".into(), "registered_editor".into());
        public_policy
            .settings
            .insert("native_text_window_id".into(), "window-1".into());
        assert!(
            SourceConfig::from_configure_with_owned_document(&public_policy, Some(registration))
                .is_err()
        );
    }
}
