//! cu-device: started by the supervisor, configured over stdin, reporting on
//! stdout. Logs go to stderr so they can never be mistaken for frames.

use std::process::ExitCode;

use desktop_ipc::link::{FramedReader, Link};
use device_macos::{DeviceConfig, shell};
use interaction_contracts::desktop::handshake::PeerRole;
use interaction_contracts::desktop::message::Message;

fn main() -> ExitCode {
    let mut input = FramedReader::new(std::io::stdin());
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
    let config = match DeviceConfig::from_configure(&configure) {
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
