//! cu-source: started by the supervisor, configured over stdin.

use std::process::ExitCode;

use desktop_ipc::link::{FramedReader, Link};
use interaction_contracts::desktop::handshake::PeerRole;
use interaction_contracts::desktop::message::Message;
use source_macos::{SourceConfig, shell};

fn main() -> ExitCode {
    let mut input = FramedReader::new(std::io::stdin());
    let configure = match input.read_next() {
        Ok(Some(envelope)) => match envelope.message {
            Message::Configure(configure) if configure.role == PeerRole::Source => configure,
            other => {
                eprintln!(
                    "cu-source: expected Configure for the source, got {}",
                    other.name()
                );
                return ExitCode::from(2);
            }
        },
        Ok(None) | Err(_) => {
            eprintln!("cu-source: the supervisor did not configure it");
            return ExitCode::from(2);
        }
    };
    let config = match SourceConfig::from_configure(&configure) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("cu-source: {error}");
            return ExitCode::from(2);
        }
    };
    let link = Link::new(std::io::stdout(), "source", config.clock);
    match shell::run(config, link, input) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("cu-source: {error}");
            ExitCode::FAILURE
        }
    }
}
