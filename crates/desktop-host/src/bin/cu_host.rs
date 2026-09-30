//! cu-host: started by the supervisor, configured over stdin.

use std::process::ExitCode;

use desktop_host::{HostConfig, shell};
use desktop_ipc::link::{FramedReader, Link};
use interaction_contracts::desktop::handshake::PeerRole;
use interaction_contracts::desktop::message::Message;

fn main() -> ExitCode {
    let mut input = FramedReader::new(std::io::stdin());
    let configure = match input.read_next() {
        Ok(Some(envelope)) => match envelope.message {
            Message::Configure(configure) if configure.role == PeerRole::Host => configure,
            other => {
                eprintln!(
                    "cu-host: expected Configure for the host, got {}",
                    other.name()
                );
                return ExitCode::from(2);
            }
        },
        Ok(None) | Err(_) => {
            eprintln!("cu-host: the supervisor did not configure it");
            return ExitCode::from(2);
        }
    };
    let config = match HostConfig::from_configure(&configure) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("cu-host: {error}");
            return ExitCode::from(2);
        }
    };
    let link = Link::new(std::io::stdout(), "host", config.clock);
    match shell::run(config, link, input) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("cu-host: {error}");
            ExitCode::FAILURE
        }
    }
}
