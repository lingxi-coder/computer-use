//! cu-source: started by the supervisor, configured over stdin.

use std::process::ExitCode;

use desktop_ipc::link::{FramedReader, Link};
use interaction_contracts::desktop::handshake::PeerRole;
use interaction_contracts::desktop::message::Message;
use source_macos::{SourceConfig, config::read_owned_document_prefix, shell};

fn main() -> ExitCode {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let mut raw_stdin = std::io::stdin();
    let owned_document_bootstrap = match args.as_slice() {
        [] => None,
        [argument] if argument == "--owned-document-bootstrap" => {
            match read_owned_document_prefix(&mut raw_stdin) {
                Ok(bootstrap) => Some(bootstrap),
                Err(_) => {
                    eprintln!("cu-source: invalid private owned document bootstrap");
                    return ExitCode::from(2);
                }
            }
        }
        _ => {
            eprintln!("cu-source: unsupported private startup arguments");
            return ExitCode::from(2);
        }
    };
    let mut input = FramedReader::new(raw_stdin);
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
    let config = match SourceConfig::from_configure_with_owned_document(
        &configure,
        owned_document_bootstrap,
    ) {
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
