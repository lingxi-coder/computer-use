//! Hello, HelloAck, Ready (design 4.6), over a Unix socket.
//!
//! Each connection starts with a Hello carrying the protocol range, the peer's
//! role, its process identity and the one-time credential. The listening side
//! checks all of it and answers with the negotiated version; only after its own
//! self-checks does it say Ready and, with it, what it can really do. A health
//! signal proves the process is alive; it refreshes no evidence and no permit.

use std::os::unix::net::UnixStream;
use std::time::Duration;

use interaction_contracts::desktop::PROTOCOL_VERSION;
use interaction_contracts::desktop::handshake::{
    ExpectedPeer, HandshakeError, Hello, HelloAck, PeerRole, ProcessIdentity, Ready, Shutdown,
    verify_hello,
};
use interaction_contracts::desktop::message::Message;

use crate::clock::SharedClock;
use crate::link::{FramedReader, Link, ReadError};

/// How long a peer may take to say hello.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// A socket with both directions, and what has been read from it.
pub struct Connection {
    pub reader: FramedReader<UnixStream>,
    pub link: Link<UnixStream>,
}

impl Connection {
    pub fn new(stream: UnixStream, producer: &str, clock: SharedClock) -> std::io::Result<Self> {
        let writer = stream.try_clone()?;
        Ok(Self {
            reader: FramedReader::new(stream),
            link: Link::new(writer, producer, clock),
        })
    }

    /// Bound how long a read may block, for the handshake only.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        // The reader owns one end of the socket; the writer's clone shares it.
        self.link.get_ref().set_read_timeout(timeout)
    }
}

#[derive(Debug)]
pub enum HandshakeFailure {
    Io(String),
    Protocol(HandshakeError),
    /// The peer sent something else where a handshake message was due.
    Unexpected(String),
    Refused(String),
}

impl std::fmt::Display for HandshakeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(reason) => write!(f, "handshake io: {reason}"),
            Self::Protocol(error) => write!(f, "handshake refused: {error}"),
            Self::Unexpected(what) => write!(f, "handshake got {what}"),
            Self::Refused(reason) => write!(f, "the peer refused: {reason}"),
        }
    }
}

impl std::error::Error for HandshakeFailure {}

impl From<ReadError> for HandshakeFailure {
    fn from(error: ReadError) -> Self {
        Self::Io(error.to_string())
    }
}

/// This process, as a peer of `role`.
pub fn make_hello(role: PeerRole, token: &str, session_epoch: u64, clock: SharedClock) -> Hello {
    Hello {
        protocol_min: PROTOCOL_VERSION,
        protocol_max: PROTOCOL_VERSION,
        role,
        build_revision: env!("CARGO_PKG_VERSION").to_string(),
        process: ProcessIdentity {
            pid: std::process::id(),
            start_time_ms: clock.origin_unix_ms(),
        },
        session_epoch,
        worker_epoch: 1,
        token: token.to_string(),
    }
}

/// The listening side: read the peer's Hello, check it, answer. On a refusal
/// the peer is told why, best effort, and the caller drops the connection.
pub fn accept_hello(
    conn: &mut Connection,
    expected: &ExpectedPeer,
    supported: (u16, u16),
) -> Result<Hello, HandshakeFailure> {
    conn.set_read_timeout(Some(HANDSHAKE_TIMEOUT))
        .map_err(|error| HandshakeFailure::Io(error.to_string()))?;
    let envelope = conn
        .reader
        .read_next()?
        .ok_or_else(|| HandshakeFailure::Io("the peer closed before saying hello".into()))?;
    let Message::Hello(hello) = envelope.message else {
        return Err(HandshakeFailure::Unexpected(envelope.message.name().into()));
    };
    match verify_hello(&hello, expected, supported) {
        Ok(ack) => {
            conn.link
                .reply(&envelope.message_id, Message::HelloAck(ack))
                .map_err(|error| HandshakeFailure::Io(error.to_string()))?;
            Ok(hello)
        }
        Err(error) => {
            let _ = conn.link.reply(
                &envelope.message_id,
                Message::Shutdown(Shutdown {
                    reason: error.to_string(),
                }),
            );
            Err(HandshakeFailure::Protocol(error))
        }
    }
}

/// The connecting side: say hello and read the answer.
pub fn send_hello(conn: &mut Connection, hello: Hello) -> Result<HelloAck, HandshakeFailure> {
    conn.set_read_timeout(Some(HANDSHAKE_TIMEOUT))
        .map_err(|error| HandshakeFailure::Io(error.to_string()))?;
    conn.link
        .send(Message::Hello(hello))
        .map_err(|error| HandshakeFailure::Io(error.to_string()))?;
    let envelope = conn
        .reader
        .read_next()?
        .ok_or_else(|| HandshakeFailure::Io("the peer closed without answering".into()))?;
    match envelope.message {
        Message::HelloAck(ack) => Ok(ack),
        Message::Shutdown(Shutdown { reason }) => Err(HandshakeFailure::Refused(reason)),
        other => Err(HandshakeFailure::Unexpected(other.name().into())),
    }
}

/// Read the peer's Ready, which says what it can really do.
pub fn read_ready(conn: &mut Connection) -> Result<Ready, HandshakeFailure> {
    let envelope = conn
        .reader
        .read_next()?
        .ok_or_else(|| HandshakeFailure::Io("the peer closed before it was ready".into()))?;
    match envelope.message {
        Message::Ready(ready) => {
            // After the handshake reads may block as long as they need to. A peer
            // that has already gone makes the system refuse the change (macOS
            // answers EINVAL); the next read reports that properly, so this
            // is best effort.
            let _ = conn.set_read_timeout(None);
            Ok(ready)
        }
        other => Err(HandshakeFailure::Unexpected(other.name().into())),
    }
}

/// What a listening process expects of whoever connects.
#[derive(Debug, Clone)]
pub struct Acceptance {
    pub producer: String,
    pub role: PeerRole,
    pub token: String,
    pub session_epoch: u64,
}

/// Accept connections on a listener for as long as the process lives. Each one
/// is handshaken on this thread; only a peer that passed is handed on, tagged
/// by `wrap`. A refused connection is dropped and logged, nothing more.
pub fn spawn_acceptor<E: Send + 'static>(
    name: &str,
    listener: std::os::unix::net::UnixListener,
    tx: std::sync::mpsc::Sender<E>,
    clock: SharedClock,
    acceptance: Acceptance,
    wrap: impl Fn(Box<Connection>) -> E + Send + 'static,
) {
    let label = acceptance.producer.clone();
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let Ok(mut conn) = Connection::new(stream, &acceptance.producer, clock) else {
                    continue;
                };
                let expected = ExpectedPeer {
                    role: acceptance.role,
                    token: acceptance.token.clone(),
                    session_epoch: acceptance.session_epoch,
                    transport_pid: None,
                };
                match accept_hello(&mut conn, &expected, (PROTOCOL_VERSION, PROTOCOL_VERSION)) {
                    Ok(_) => {
                        if tx.send(wrap(Box::new(conn))).is_err() {
                            break;
                        }
                    }
                    Err(error) => eprintln!("{label}: a connection was refused: {error}"),
                }
            }
        })
        .expect("the acceptor thread can be spawned");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::socket;
    use interaction_contracts::desktop::handshake::PeerRole;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn pair() -> (Connection, Connection) {
        let (a, b) = UnixStream::pair().unwrap();
        let clock = SharedClock::starting_now();
        (
            Connection::new(a, "host", clock).unwrap(),
            Connection::new(b, "device", clock).unwrap(),
        )
    }

    fn expected(role: PeerRole) -> ExpectedPeer {
        ExpectedPeer {
            role,
            token: TOKEN.into(),
            session_epoch: 1,
            transport_pid: None,
        }
    }

    #[test]
    fn a_peer_with_the_right_role_token_and_epoch_is_acknowledged_and_ready_follows() {
        let (mut client, mut server) = pair();
        let clock = SharedClock::starting_now();
        let handle = std::thread::spawn(move || {
            let hello = accept_hello(&mut server, &expected(PeerRole::Host), (1, 1)).unwrap();
            server
                .link
                .send(Message::Ready(Ready {
                    role: PeerRole::Device,
                    capabilities: None,
                    notes: vec![],
                }))
                .unwrap();
            hello
        });
        let ack = send_hello(&mut client, make_hello(PeerRole::Host, TOKEN, 1, clock)).unwrap();
        assert_eq!(ack.negotiated_version, 1);
        let ready = read_ready(&mut client).unwrap();
        assert_eq!(ready.role, PeerRole::Device);
        let hello = handle.join().unwrap();
        assert_eq!(hello.role, PeerRole::Host);
        assert_eq!(hello.process.pid, std::process::id());
    }

    #[test]
    fn a_wrong_token_role_or_epoch_is_refused_and_told_why() {
        let clock = SharedClock::starting_now();
        for (hello, reason) in [
            (
                make_hello(PeerRole::Host, "ffffffffffffffffffffffffffffffff", 1, clock),
                "token",
            ),
            (make_hello(PeerRole::Source, TOKEN, 1, clock), "role"),
            (make_hello(PeerRole::Host, TOKEN, 9, clock), "epoch"),
        ] {
            let (mut client, mut server) = pair();
            let handle = std::thread::spawn(move || {
                accept_hello(&mut server, &expected(PeerRole::Host), (1, 1)).unwrap_err()
            });
            let error = send_hello(&mut client, hello).unwrap_err();
            assert!(
                matches!(error, HandshakeFailure::Refused(_)),
                "{reason}: {error}"
            );
            assert!(matches!(
                handle.join().unwrap(),
                HandshakeFailure::Protocol(_)
            ));
        }
    }

    #[test]
    fn a_peer_that_speaks_no_common_version_is_refused() {
        let (mut client, mut server) = pair();
        let clock = SharedClock::starting_now();
        let mut hello = make_hello(PeerRole::Host, TOKEN, 1, clock);
        hello.protocol_min = 2;
        hello.protocol_max = 3;
        let handle = std::thread::spawn(move || {
            accept_hello(&mut server, &expected(PeerRole::Host), (1, 1)).unwrap_err()
        });
        let error = send_hello(&mut client, hello).unwrap_err();
        assert!(
            error.to_string().contains("no common protocol version"),
            "{error}"
        );
        handle.join().unwrap();
    }

    #[test]
    fn something_other_than_a_hello_first_ends_the_handshake() {
        let (mut client, mut server) = pair();
        client
            .link
            .send(Message::Shutdown(Shutdown { reason: "x".into() }))
            .unwrap();
        let error = accept_hello(&mut server, &expected(PeerRole::Host), (1, 1)).unwrap_err();
        assert!(matches!(error, HandshakeFailure::Unexpected(_)), "{error}");
    }

    #[test]
    fn it_works_over_a_real_socket_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("d.sock");
        let listener = socket::listen(&path).unwrap();
        let clock = SharedClock::starting_now();
        let handle = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut conn = Connection::new(stream, "device", clock).unwrap();
            accept_hello(&mut conn, &expected(PeerRole::Host), (1, 1)).map(|hello| hello.role)
        });
        let stream = socket::connect(&path, Duration::from_secs(1)).unwrap();
        let mut conn = Connection::new(stream, "host", clock).unwrap();
        send_hello(&mut conn, make_hello(PeerRole::Host, TOKEN, 1, clock)).unwrap();
        assert_eq!(handle.join().unwrap().unwrap(), PeerRole::Host);
    }
}
