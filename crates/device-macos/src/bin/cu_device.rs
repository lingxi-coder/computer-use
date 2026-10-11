//! cu-device: started by the supervisor, configured over stdin, reporting on
//! stdout. Logs go to stderr so they can never be mistaken for frames.

use std::io::Read;
use std::process::ExitCode;

use desktop_ipc::browser_control::PendingBrowserControl;
use desktop_ipc::link::{FramedReader, Link};
use device_macos::{DeviceConfig, owned_document_bootstrap::OwnedDocumentBootstrap, shell};
use interaction_contracts::desktop::handshake::PeerRole;
use interaction_contracts::desktop::message::Message;

fn read_owned_bootstrap(
    args: &[String],
    stdin: &mut impl Read,
) -> Result<Option<OwnedDocumentBootstrap>, String> {
    match args {
        [] => Ok(None),
        [arg] if arg == "--owned-document-bootstrap" => {
            OwnedDocumentBootstrap::read(stdin).map(Some)
        }
        _ => Err("unsupported startup arguments".into()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrivateStartupMode {
    None,
    OwnedDocument,
    BrowserControl,
}

fn private_startup_mode(args: &[String]) -> Result<PrivateStartupMode, String> {
    match args {
        [] => Ok(PrivateStartupMode::None),
        [arg] if arg == "--owned-document-bootstrap" => Ok(PrivateStartupMode::OwnedDocument),
        [arg] if arg == "--browser-control" => Ok(PrivateStartupMode::BrowserControl),
        _ => Err("unsupported startup arguments".into()),
    }
}

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let mut stdin = std::io::stdin();
    let startup_mode = match private_startup_mode(&args) {
        Ok(mode) => mode,
        Err(error) => {
            eprintln!("cu-device: {error}");
            return ExitCode::from(2);
        }
    };
    // The private browser bootstrap arrives on inherited fd3 before Configure;
    // it never shares stdin's public Configure framing.
    let browser_pending = if startup_mode == PrivateStartupMode::BrowserControl {
        match PendingBrowserControl::read_inherited_fd3() {
            Ok(pending) => Some(pending),
            Err(error) => {
                eprintln!("cu-device: {error}");
                return ExitCode::from(2);
            }
        }
    } else {
        None
    };
    let bootstrap = if startup_mode == PrivateStartupMode::OwnedDocument {
        match read_owned_bootstrap(&args, &mut stdin) {
            Ok(bootstrap) => bootstrap,
            Err(error) => {
                eprintln!("cu-device: {error}");
                return ExitCode::from(2);
            }
        }
    } else {
        None
    };
    let mut input = FramedReader::new(stdin);
    // The first frame configures the process; nothing else is accepted before it.
    let configure = match input.read_next() {
        Ok(Some(envelope)) => match envelope.message {
            Message::Configure(configure) if configure.role == PeerRole::Device => configure,
            other => {
                eprintln!(
                    "cu-device: expected Configure for the device, got {}",
                    other.name()
                );
                return ExitCode::from(2);
            }
        },
        Ok(None) => {
            eprintln!("cu-device: the supervisor closed before configuring it");
            return ExitCode::from(2);
        }
        Err(error) => {
            eprintln!("cu-device: {error}");
            return ExitCode::from(2);
        }
    };
    let browser_session = match browser_pending {
        Some(pending) => {
            if let Err(error) = pending.validate_configure(&configure) {
                eprintln!("cu-device: {error}");
                return ExitCode::from(2);
            }
            match pending.finish() {
                Ok(session) => Some(session),
                Err(error) => {
                    eprintln!("cu-device: {error}");
                    return ExitCode::from(2);
                }
            }
        }
        None => None,
    };
    let config_result = match browser_session {
        Some(session) => DeviceConfig::from_configure_with_browser_control(&configure, session),
        None => DeviceConfig::from_configure_with_owned_bootstrap(&configure, bootstrap),
    };
    let config = match config_result {
        Ok(config) => config,
        Err(error) => {
            eprintln!("cu-device: {error}");
            return ExitCode::from(2);
        }
    };
    let link = Link::new(std::io::stdout(), "device", config.clock);
    match shell::run(config, link, input) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("cu-device: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn frame() -> Vec<u8> {
        let value = json!({
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
        let payload = serde_json::to_vec(&value).unwrap();
        let mut bytes = (payload.len() as u32).to_be_bytes().to_vec();
        bytes.extend(payload);
        bytes
    }

    #[test]
    fn only_no_prefix_or_one_owned_prefix_is_supported_and_prefix_precedes_configure() {
        let mut bytes = frame();
        bytes.extend_from_slice(b"CONFIGURE");
        let mut input = &bytes[..];
        let args = vec!["--owned-document-bootstrap".to_string()];
        assert!(read_owned_bootstrap(&args, &mut input).unwrap().is_some());
        assert_eq!(input, b"CONFIGURE");

        let mut input = b"CONFIGURE".as_slice();
        assert!(read_owned_bootstrap(&[], &mut input).unwrap().is_none());
        assert_eq!(input, b"CONFIGURE");

        for args in [
            vec!["--model-bootstrap"],
            vec!["--owned-document-bootstrap", "--owned-document-bootstrap"],
            vec!["--browser-control"],
            vec!["--browser-bootstrap"],
            vec!["--other"],
        ] {
            let args = args.into_iter().map(String::from).collect::<Vec<_>>();
            let mut input = b"private-prefix".as_slice();
            assert_eq!(
                read_owned_bootstrap(&args, &mut input).err().unwrap(),
                "unsupported startup arguments"
            );
            assert_eq!(input, b"private-prefix");
        }
    }

    #[test]
    fn browser_control_is_the_only_fd3_startup_marker_and_never_consumes_stdin() {
        assert_eq!(
            private_startup_mode(&["--browser-control".into()]).unwrap(),
            PrivateStartupMode::BrowserControl
        );
        for args in [
            vec!["--browser-bootstrap"],
            vec!["--browser-control", "--browser-control"],
            vec!["--owned-document-bootstrap", "--browser-control"],
        ] {
            let args = args.into_iter().map(String::from).collect::<Vec<_>>();
            assert_eq!(
                private_startup_mode(&args).unwrap_err(),
                "unsupported startup arguments"
            );
        }
        let mut stdin = b"CONFIGURE".as_slice();
        assert!(read_owned_bootstrap(&["--browser-control".into()], &mut stdin).is_err());
        assert_eq!(stdin, b"CONFIGURE");
    }
}
