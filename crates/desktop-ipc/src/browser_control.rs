//! Private Device↔Main browser control protocol on inherited fd3.
//!
//! This channel is intentionally separate from desktop Message envelopes and
//! accepts only the closed browser-control DTOs. It carries one authenticated
//! Device session and at most one outstanding SetCheckbox request.

use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use interaction_contracts::desktop::browser::{BrowserPageProfile, BrowserReadScope};
use interaction_contracts::desktop::browser_control::{
    BROWSER_CONTROL_VERSION, BrowserControlBootstrap, BrowserControlHello, BrowserControlReady,
    BrowserControlReply, BrowserControlSubmit, MAX_BROWSER_CONTROL_BOOTSTRAP_BYTES,
    MAX_BROWSER_CONTROL_REPLY_BYTES, MAX_BROWSER_CONTROL_SUBMIT_BYTES,
};
use interaction_contracts::desktop::handshake::Configure;
use serde::Serialize;
use serde::de::DeserializeOwned;
use zeroize::Zeroizing;

pub const BOOTSTRAP_MAGIC: &[u8; 8] = b"CUBRBOOT";
pub const HELLO_MAGIC: &[u8; 8] = b"CUBRHELO";
pub const READY_MAGIC: &[u8; 8] = b"CUBRREDY";
pub const SUBMIT_MAGIC: &[u8; 8] = b"CUBRSUBM";
pub const REPLY_MAGIC: &[u8; 8] = b"CUBRREPL";

const PREFIX_BYTES: usize = 12;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const INVALID: &str = "invalid private browser control channel";

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, INVALID)
}

/// Open only the explicitly inherited duplex socket. This does not discover
/// a path or consult ambient configuration.
pub fn open_inherited_fd3() -> Result<UnixStream, String> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/fd/3")
        .map_err(|_| INVALID.to_string())?;
    if !file
        .metadata()
        .map_err(|_| INVALID.to_string())?
        .file_type()
        .is_socket()
    {
        return Err(INVALID.into());
    }
    let descriptor: OwnedFd = file.into();
    Ok(UnixStream::from(descriptor))
}

fn write_record<W: Write, T: Serialize>(
    writer: &mut W,
    magic: &[u8; 8],
    value: &T,
    max_bytes: usize,
    validate: impl FnOnce() -> Result<(), String>,
) -> io::Result<()> {
    validate().map_err(|_| invalid())?;
    let payload = Zeroizing::new(serde_json::to_vec(value).map_err(|_| invalid())?);
    if payload.is_empty() || payload.len() > max_bytes {
        return Err(invalid());
    }
    let length = u32::try_from(payload.len()).map_err(|_| invalid())?;
    writer.write_all(magic)?;
    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()
}

fn read_record<R: Read, T: DeserializeOwned>(
    reader: &mut R,
    expected_magic: &[u8; 8],
    max_bytes: usize,
    validate: impl FnOnce(&T) -> Result<(), String>,
) -> io::Result<T> {
    let mut prefix = [0; PREFIX_BYTES];
    reader.read_exact(&mut prefix)?;
    if &prefix[..8] != expected_magic {
        return Err(invalid());
    }
    let length = u32::from_be_bytes(prefix[8..12].try_into().map_err(|_| invalid())?) as usize;
    if length == 0 || length > max_bytes {
        return Err(invalid());
    }
    let mut payload = Zeroizing::new(vec![0; length]);
    reader.read_exact(&mut payload)?;
    let value: T = serde_json::from_slice(&payload).map_err(|_| invalid())?;
    validate(&value).map_err(|_| invalid())?;
    Ok(value)
}

pub fn write_bootstrap<W: Write>(
    writer: &mut W,
    bootstrap: &BrowserControlBootstrap,
) -> io::Result<()> {
    write_record(
        writer,
        BOOTSTRAP_MAGIC,
        bootstrap,
        MAX_BROWSER_CONTROL_BOOTSTRAP_BYTES,
        || bootstrap.validate(),
    )
}

pub fn read_bootstrap<R: Read>(reader: &mut R) -> io::Result<BrowserControlBootstrap> {
    read_record(
        reader,
        BOOTSTRAP_MAGIC,
        MAX_BROWSER_CONTROL_BOOTSTRAP_BYTES,
        BrowserControlBootstrap::validate,
    )
}

pub fn write_hello<W: Write>(
    writer: &mut W,
    hello: &BrowserControlHello,
    bootstrap: &BrowserControlBootstrap,
) -> io::Result<()> {
    write_record(
        writer,
        HELLO_MAGIC,
        hello,
        MAX_BROWSER_CONTROL_BOOTSTRAP_BYTES,
        || hello.validate_against(bootstrap),
    )
}

pub fn read_hello<R: Read>(
    reader: &mut R,
    bootstrap: &BrowserControlBootstrap,
) -> io::Result<BrowserControlHello> {
    read_record(
        reader,
        HELLO_MAGIC,
        MAX_BROWSER_CONTROL_BOOTSTRAP_BYTES,
        |hello: &BrowserControlHello| hello.validate_against(bootstrap),
    )
}

pub fn write_ready<W: Write>(
    writer: &mut W,
    ready: &BrowserControlReady,
    hello: &BrowserControlHello,
) -> io::Result<()> {
    write_record(
        writer,
        READY_MAGIC,
        ready,
        MAX_BROWSER_CONTROL_BOOTSTRAP_BYTES,
        || ready.validate_against(hello),
    )
}

pub fn read_ready<R: Read>(
    reader: &mut R,
    hello: &BrowserControlHello,
) -> io::Result<BrowserControlReady> {
    read_record(
        reader,
        READY_MAGIC,
        MAX_BROWSER_CONTROL_BOOTSTRAP_BYTES,
        |ready: &BrowserControlReady| ready.validate_against(hello),
    )
}

pub fn write_submit<W: Write>(writer: &mut W, submit: &BrowserControlSubmit) -> io::Result<()> {
    write_record(
        writer,
        SUBMIT_MAGIC,
        submit,
        MAX_BROWSER_CONTROL_SUBMIT_BYTES,
        || submit.validate(),
    )
}

pub fn read_submit<R: Read>(reader: &mut R) -> io::Result<BrowserControlSubmit> {
    read_record(
        reader,
        SUBMIT_MAGIC,
        MAX_BROWSER_CONTROL_SUBMIT_BYTES,
        BrowserControlSubmit::validate,
    )
}

pub fn write_reply<W: Write>(
    writer: &mut W,
    reply: &BrowserControlReply,
    submit: &BrowserControlSubmit,
) -> io::Result<()> {
    write_record(
        writer,
        REPLY_MAGIC,
        reply,
        MAX_BROWSER_CONTROL_REPLY_BYTES,
        || reply.validate_against(submit),
    )
}

pub fn read_reply<R: Read>(
    reader: &mut R,
    submit: &BrowserControlSubmit,
) -> io::Result<BrowserControlReply> {
    read_record(
        reader,
        REPLY_MAGIC,
        MAX_BROWSER_CONTROL_REPLY_BYTES,
        |reply: &BrowserControlReply| reply.validate_against(submit),
    )
}

/// Bootstrap received before Device reads Configure. Device validates the
/// Configure profile and epoch before it sends Hello.
pub struct PendingBrowserControl {
    stream: UnixStream,
    bootstrap: BrowserControlBootstrap,
}

impl PendingBrowserControl {
    pub fn read_inherited_fd3() -> Result<Self, String> {
        Self::read_from_stream(open_inherited_fd3()?)
    }

    pub fn read_from_stream(mut stream: UnixStream) -> Result<Self, String> {
        stream
            .set_read_timeout(Some(HANDSHAKE_TIMEOUT))
            .map_err(|_| INVALID.to_string())?;
        stream
            .set_write_timeout(Some(HANDSHAKE_TIMEOUT))
            .map_err(|_| INVALID.to_string())?;
        let bootstrap = read_bootstrap(&mut stream).map_err(|_| INVALID.to_string())?;
        Ok(Self { stream, bootstrap })
    }

    pub fn validate_configure(&self, configure: &Configure) -> Result<(), String> {
        configure.validate()?;
        if configure.role != interaction_contracts::desktop::handshake::PeerRole::Device
            || configure.session_epoch != self.bootstrap.scope.context.session_epoch
            || configure.browser_page_profile != Some(self.bootstrap.browser_page_profile)
        {
            return Err("browser control bootstrap does not match Device Configure".into());
        }
        Ok(())
    }

    /// Send Hello only after the caller has read and matched Device Configure.
    pub fn finish(mut self) -> Result<BrowserControlSession, String> {
        let hello = BrowserControlHello {
            version: BROWSER_CONTROL_VERSION,
            token: self.bootstrap.token.clone(),
            channel_generation: self.bootstrap.channel_generation,
            session_epoch: self.bootstrap.scope.context.session_epoch,
            browser_page_profile: self.bootstrap.browser_page_profile,
            scope_id: self.bootstrap.scope.scope_id.clone(),
            scope_generation: self.bootstrap.scope.generation,
        };
        write_hello(&mut self.stream, &hello, &self.bootstrap).map_err(|_| INVALID.to_string())?;
        let ready = read_ready(&mut self.stream, &hello).map_err(|_| INVALID.to_string())?;
        Ok(BrowserControlSession {
            browser_page_profile: self.bootstrap.browser_page_profile,
            scope: self.bootstrap.scope,
            channel_generation: self.bootstrap.channel_generation,
            next_sequence: ready.next_sequence,
            stream: self.stream,
        })
    }
}

impl std::fmt::Debug for PendingBrowserControl {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingBrowserControl")
            .field("scope_id", &self.bootstrap.scope.scope_id)
            .field("scope_generation", &self.bootstrap.scope.generation)
            .field("channel_generation", &self.bootstrap.channel_generation)
            .finish_non_exhaustive()
    }
}

/// A completed fd3 handshake. The one-use token is dropped after the peer's
/// Ready matches this exact scope and generation.
pub struct BrowserControlSession {
    pub browser_page_profile: BrowserPageProfile,
    pub scope: Box<BrowserReadScope>,
    pub channel_generation: u64,
    pub next_sequence: u64,
    stream: UnixStream,
}

impl BrowserControlSession {
    pub fn into_parts(
        self,
    ) -> (
        UnixStream,
        BrowserPageProfile,
        Box<BrowserReadScope>,
        u64,
        u64,
    ) {
        (
            self.stream,
            self.browser_page_profile,
            self.scope,
            self.channel_generation,
            self.next_sequence,
        )
    }
}

impl std::fmt::Debug for BrowserControlSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserControlSession")
            .field("browser_page_profile", &self.browser_page_profile)
            .field("scope_id", &self.scope.scope_id)
            .field("scope_generation", &self.scope.generation)
            .field("channel_generation", &self.channel_generation)
            .field("next_sequence", &self.next_sequence)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use interaction_contracts::desktop::browser::BrowserTargetContext;
    use interaction_contracts::desktop::browser_control::{
        BROWSER_CONTROL_VERSION, BrowserControlToken,
    };
    use interaction_contracts::desktop::handshake::{Configure, PeerRole};
    use std::collections::BTreeMap;
    use std::os::unix::net::UnixStream;
    use std::thread;

    fn bootstrap() -> BrowserControlBootstrap {
        BrowserControlBootstrap {
            version: BROWSER_CONTROL_VERSION,
            token: BrowserControlToken::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .unwrap(),
            channel_generation: 3,
            browser_page_profile: BrowserPageProfile::FixtureBackgroundOption,
            scope: Box::new(BrowserReadScope {
                scope_id: "550e8400-e29b-41d4-a716-446655440000".into(),
                generation: 1,
                context: BrowserTargetContext {
                    browser_session_id: "session-1".into(),
                    browser_context_id: "context-1".into(),
                    page_target_id: "target-1".into(),
                    cdp_session_id: "cdp-1".into(),
                    frame_id: "frame-1".into(),
                    document_loader_id: "loader-1".into(),
                    fixture_file_url:
                        "file:///tmp/computer-use/fixtures/headless/readonly-page.html".into(),
                    session_epoch: 7,
                },
                clock_domain: "headless/7/wall".into(),
                issued_ms: 100,
            }),
        }
    }

    #[test]
    fn private_fd3_handshake_binds_one_exact_scope_and_first_sequence() {
        let (device, mut main) = UnixStream::pair().unwrap();
        let expected = bootstrap();
        let main_bootstrap = expected.clone();
        let main_thread = thread::spawn(move || {
            write_bootstrap(&mut main, &main_bootstrap).unwrap();
            let hello = read_hello(&mut main, &main_bootstrap).unwrap();
            let ready = BrowserControlReady {
                version: BROWSER_CONTROL_VERSION,
                channel_generation: hello.channel_generation,
                session_epoch: hello.session_epoch,
                scope_id: hello.scope_id.clone(),
                scope_generation: hello.scope_generation,
                next_sequence: 1,
            };
            write_ready(&mut main, &ready, &hello).unwrap();
        });
        let pending = {
            let mut device = device;
            let bootstrap = read_bootstrap(&mut device).unwrap();
            PendingBrowserControl {
                stream: device,
                bootstrap,
            }
        };
        let session = pending.finish().unwrap();
        assert_eq!(session.scope.as_ref(), expected.scope.as_ref());
        assert_eq!(session.browser_page_profile, expected.browser_page_profile);
        assert_eq!(session.channel_generation, expected.channel_generation);
        assert_eq!(session.next_sequence, 1);
        main_thread.join().unwrap();
    }

    #[test]
    fn private_fd3_handshake_retires_on_wrong_ready_identity() {
        let (mut device, mut main) = UnixStream::pair().unwrap();
        let main_thread = thread::spawn(move || {
            let expected = bootstrap();
            write_bootstrap(&mut main, &expected).unwrap();
            let hello = read_hello(&mut main, &expected).unwrap();
            let ready = BrowserControlReady {
                version: BROWSER_CONTROL_VERSION,
                channel_generation: hello.channel_generation,
                session_epoch: hello.session_epoch,
                scope_id: "550e8400-e29b-41d4-a716-446655440001".into(),
                scope_generation: hello.scope_generation,
                next_sequence: 1,
            };
            write_record(
                &mut main,
                READY_MAGIC,
                &ready,
                MAX_BROWSER_CONTROL_BOOTSTRAP_BYTES,
                || Ok(()),
            )
            .unwrap();
        });
        let bootstrap = read_bootstrap(&mut device).unwrap();
        assert!(
            PendingBrowserControl {
                stream: device,
                bootstrap
            }
            .finish()
            .is_err()
        );
        main_thread.join().unwrap();
    }

    #[test]
    fn configure_scope_mismatch_fails_before_device_sends_hello() {
        let (device, mut main) = UnixStream::pair().unwrap();
        let main_bootstrap = bootstrap();
        let server = thread::spawn(move || {
            write_bootstrap(&mut main, &main_bootstrap).unwrap();
            let mut hello_prefix = [0; 8];
            assert_eq!(main.read(&mut hello_prefix).unwrap(), 0);
        });
        let pending = PendingBrowserControl::read_from_stream(device).unwrap();
        let mismatched = Configure {
            role: PeerRole::Device,
            session_epoch: 8,
            settings: BTreeMap::new(),
            browser_page_profile: Some(BrowserPageProfile::FixtureBackgroundOption),
        };
        assert!(pending.validate_configure(&mismatched).is_err());
        drop(pending);
        server.join().unwrap();
    }

    #[test]
    fn decoder_rejects_wrong_magic_oversize_and_unknown_fields_before_admission() {
        let value = serde_json::to_vec(&bootstrap()).unwrap();
        let mut wrong_magic = Vec::new();
        wrong_magic.extend_from_slice(HELLO_MAGIC);
        wrong_magic.extend_from_slice(&(value.len() as u32).to_be_bytes());
        wrong_magic.extend_from_slice(&value);
        assert!(read_bootstrap(&mut wrong_magic.as_slice()).is_err());

        let mut oversized = Vec::new();
        oversized.extend_from_slice(BOOTSTRAP_MAGIC);
        oversized
            .extend_from_slice(&((MAX_BROWSER_CONTROL_BOOTSTRAP_BYTES + 1) as u32).to_be_bytes());
        assert!(read_bootstrap(&mut oversized.as_slice()).is_err());

        let mut unknown = serde_json::to_value(bootstrap()).unwrap();
        unknown["extra"] = serde_json::json!(true);
        let payload = serde_json::to_vec(&unknown).unwrap();
        let mut frame = BOOTSTRAP_MAGIC.to_vec();
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(&payload);
        assert!(read_bootstrap(&mut frame.as_slice()).is_err());
    }
}
