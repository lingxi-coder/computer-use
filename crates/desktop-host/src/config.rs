//! What the supervisor tells cu-host when it starts it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use desktop_ipc::SharedClock;
use interaction_contracts::desktop::handshake::Configure;
use interaction_contracts::desktop::operator::RunMode;

#[derive(Debug, Clone)]
pub struct HostConfig {
    pub socket_dir: PathBuf,
    pub db_path: PathBuf,
    pub device_token: String,
    pub device_epoch: u64,
    pub source_token: String,
    pub clock: SharedClock,
    pub initial_mode: RunMode,
}

fn setting<'a>(settings: &'a BTreeMap<String, String>, key: &str) -> Result<&'a str, String> {
    settings
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| format!("the supervisor did not give {key}"))
}

impl HostConfig {
    pub fn from_configure(configure: &Configure) -> Result<Self, String> {
        let settings = &configure.settings;
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
        Ok(Self {
            socket_dir: PathBuf::from(setting(settings, "socket_dir")?),
            db_path: PathBuf::from(setting(settings, "db_path")?),
            device_token: setting(settings, "device_token")?.to_string(),
            device_epoch: number("device_epoch")?,
            source_token: setting(settings, "source_token")?.to_string(),
            clock: SharedClock::from_origin(number("clock_origin_ms")?),
            initial_mode,
        })
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
        }
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
    }
}
