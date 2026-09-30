//! What the supervisor tells cu-device when it starts it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use desktop_ipc::SharedClock;
use interaction_contracts::desktop::handshake::Configure;

#[derive(Debug, Clone)]
pub struct DeviceConfig {
    /// Private directory holding the sockets and the world log.
    pub socket_dir: PathBuf,
    /// The host's one-time credential, delivered over the private pipe.
    pub host_token: String,
    pub session_epoch: u64,
    pub clock: SharedClock,
    pub world_log: PathBuf,
}

fn setting<'a>(settings: &'a BTreeMap<String, String>, key: &str) -> Result<&'a str, String> {
    settings
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| format!("the supervisor did not give {key}"))
}

impl DeviceConfig {
    pub fn from_configure(configure: &Configure) -> Result<Self, String> {
        let settings = &configure.settings;
        let origin: u64 = setting(settings, "clock_origin_ms")?
            .parse()
            .map_err(|_| "clock_origin_ms is not a number".to_string())?;
        Ok(Self {
            socket_dir: PathBuf::from(setting(settings, "socket_dir")?),
            host_token: setting(settings, "host_token")?.to_string(),
            session_epoch: configure.session_epoch,
            clock: SharedClock::from_origin(origin),
            world_log: PathBuf::from(setting(settings, "world_log")?),
        })
    }

    pub fn socket_path(&self) -> PathBuf {
        self.socket_dir.join("device.sock")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use interaction_contracts::desktop::handshake::PeerRole;

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
        };
        let config = DeviceConfig::from_configure(&configure).unwrap();
        assert_eq!(config.session_epoch, 3);
        assert_eq!(config.socket_path(), PathBuf::from("/tmp/x/device.sock"));
        configure.settings.remove("host_token");
        let error = DeviceConfig::from_configure(&configure).unwrap_err();
        assert!(error.contains("host_token"), "{error}");
    }
}
