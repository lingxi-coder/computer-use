//! What the supervisor tells cu-source when it starts it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use desktop_ipc::SharedClock;
use interaction_contracts::desktop::handshake::Configure;

#[derive(Debug, Clone)]
pub struct SourceConfig {
    pub socket_dir: PathBuf,
    pub host_token: String,
    /// The device session this source reports for. The snapshots carry the
    /// device's identity, so the host can hand them to it as evidence.
    pub session_epoch: u64,
    pub clock: SharedClock,
    pub world_log: PathBuf,
    /// How often the window is reported when nothing asks for it.
    pub interval_ms: u64,
}

fn setting<'a>(settings: &'a BTreeMap<String, String>, key: &str) -> Result<&'a str, String> {
    settings
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| format!("the supervisor did not give {key}"))
}

impl SourceConfig {
    pub fn from_configure(configure: &Configure) -> Result<Self, String> {
        let settings = &configure.settings;
        let number = |key: &str| -> Result<u64, String> {
            setting(settings, key)?
                .parse()
                .map_err(|_| format!("{key} is not a number"))
        };
        Ok(Self {
            socket_dir: PathBuf::from(setting(settings, "socket_dir")?),
            host_token: setting(settings, "host_token")?.to_string(),
            session_epoch: configure.session_epoch,
            clock: SharedClock::from_origin(number("clock_origin_ms")?),
            world_log: PathBuf::from(setting(settings, "world_log")?),
            interval_ms: settings.get("interval_ms").map_or(Ok(50), |value| {
                value
                    .parse()
                    .map_err(|_| "interval_ms is not a number".to_string())
            })?,
        })
    }

    pub fn socket_path(&self) -> PathBuf {
        self.socket_dir.join("source.sock")
    }
}
