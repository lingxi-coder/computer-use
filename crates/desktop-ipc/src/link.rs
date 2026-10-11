//! Envelopes on a stream: building and sending them, and reading frames back.

use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
use std::thread::JoinHandle;
use std::time::Duration;

use interaction_contracts::desktop::PROTOCOL_VERSION;
use interaction_contracts::desktop::frame::{FrameDecoder, FrameError, encode_frame};
use interaction_contracts::desktop::ids::{MessageId, ProducerId};
use interaction_contracts::desktop::message::{Envelope, Message};

use crate::clock::SharedClock;

/// The writing end of a stream. It numbers what it sends, per producer, so a
/// gap on the other side means messages were lost.
pub struct Link<W: Write> {
    writer: W,
    producer: ProducerId,
    sequence: u64,
    clock: SharedClock,
}

impl<W: Write> Link<W> {
    pub fn new(writer: W, producer: &str, clock: SharedClock) -> Self {
        Self {
            writer,
            producer: ProducerId::new(producer).expect("a producer name is a valid id"),
            sequence: 0,
            clock,
        }
    }

    fn envelope(&mut self, in_reply_to: Option<MessageId>, message: Message) -> Envelope {
        self.sequence += 1;
        let id = format!("{}-{}", self.producer, self.sequence);
        Envelope {
            protocol_version: PROTOCOL_VERSION,
            message_id: MessageId::new(id.clone()).expect("a counter is a valid id"),
            in_reply_to,
            producer: self.producer.clone(),
            producer_sequence: self.sequence,
            emitted_ms: self.clock.now_ms(),
            trace_id: format!("trace-{id}"),
            message,
        }
    }

    fn write(&mut self, envelope: &Envelope) -> io::Result<()> {
        let frame = encode_frame(envelope).map_err(io::Error::other)?;
        self.writer.write_all(&frame)?;
        self.writer.flush()
    }

    pub fn send(&mut self, message: Message) -> io::Result<MessageId> {
        let envelope = self.envelope(None, message);
        let id = envelope.message_id.clone();
        self.write(&envelope)?;
        Ok(id)
    }

    pub fn reply(&mut self, to: &MessageId, message: Message) -> io::Result<MessageId> {
        let envelope = self.envelope(Some(to.clone()), message);
        let id = envelope.message_id.clone();
        self.write(&envelope)?;
        Ok(id)
    }

    pub fn clock(&self) -> SharedClock {
        self.clock
    }

    pub fn get_ref(&self) -> &W {
        &self.writer
    }

    pub fn get_mut(&mut self) -> &mut W {
        &mut self.writer
    }

    /// Move blocking writes off a control loop without changing message ids.
    pub fn queued(self, name: &str, capacity: usize) -> Link<QueuedWriter>
    where
        W: Send + 'static,
    {
        Link {
            writer: QueuedWriter::new(name, self.writer, capacity),
            producer: self.producer,
            sequence: self.sequence,
            clock: self.clock,
        }
    }
}

/// A bounded output queue. The caller never waits for the peer to read: a full
/// queue or a failed worker makes all further writes fail, so a safety loop can
/// close its gate. Each admission is one complete frame; nothing is silently
/// dropped. The owner must check `failed()` even when it has nothing to send.
pub struct QueuedWriter {
    sender: Option<SyncSender<Vec<u8>>>,
    failed: Arc<AtomicBool>,
    finished: Receiver<()>,
}

impl QueuedWriter {
    pub fn new<W: Write + Send + 'static>(name: &str, mut writer: W, capacity: usize) -> Self {
        assert!(capacity > 0, "an output queue must have space for a frame");
        let (sender, receiver) = mpsc::sync_channel::<Vec<u8>>(capacity);
        let failed = Arc::new(AtomicBool::new(false));
        let worker_failed = failed.clone();
        let (done, finished) = mpsc::channel();
        std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                while let Ok(frame) = receiver.recv() {
                    if worker_failed.load(Ordering::Acquire) {
                        break;
                    }
                    if writer
                        .write_all(&frame)
                        .and_then(|()| writer.flush())
                        .is_err()
                    {
                        worker_failed.store(true, Ordering::Release);
                        break;
                    }
                }
                let _ = done.send(());
            })
            .expect("an output worker can be spawned");
        Self {
            sender: Some(sender),
            failed,
            finished,
        }
    }

    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    /// Let queued final acknowledgements drain on normal shutdown, with a hard
    /// bound when the peer has stopped reading. Never call from a running loop.
    pub fn finish(&mut self, timeout: Duration) -> bool {
        self.sender = None;
        self.finished.recv_timeout(timeout).is_ok()
    }
}

impl Write for QueuedWriter {
    fn write(&mut self, frame: &[u8]) -> io::Result<usize> {
        if self.failed() {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "output worker failed",
            ));
        }
        let Some(sender) = self.sender.as_ref() else {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "output queue closed",
            ));
        };
        match sender.try_send(frame.to_vec()) {
            Ok(()) => Ok(frame.len()),
            Err(error) => {
                self.failed.store(true, Ordering::Release);
                let kind = match error {
                    TrySendError::Full(_) => io::ErrorKind::WouldBlock,
                    TrySendError::Disconnected(_) => io::ErrorKind::BrokenPipe,
                };
                Err(io::Error::new(kind, "output queue is full or disconnected"))
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.failed() {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "output worker failed",
            ))
        } else {
            Ok(())
        }
    }
}

#[derive(Debug)]
pub enum ReadError {
    Io(io::Error),
    Frame(FrameError),
    /// The stream ended in the middle of a frame.
    Truncated,
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "io: {error}"),
            Self::Frame(error) => write!(f, "{error}"),
            Self::Truncated => f.write_str("the stream ended inside a frame"),
        }
    }
}

impl std::error::Error for ReadError {}

/// The reading end: a stream and what has been read from it but not yet
/// framed. It travels with its buffer into the reader thread, so bytes that
/// arrived behind the handshake are not lost.
pub struct FramedReader<R: Read> {
    reader: R,
    decoder: FrameDecoder,
}

impl<R: Read> FramedReader<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            decoder: FrameDecoder::default(),
        }
    }

    /// The next envelope. `Ok(None)` is a clean end of the stream.
    pub fn read_next(&mut self) -> Result<Option<Envelope>, ReadError> {
        loop {
            match self.decoder.next_envelope() {
                Ok(Some(envelope)) => return Ok(Some(envelope)),
                Ok(None) => {}
                Err(error) => return Err(ReadError::Frame(error)),
            }
            let mut buffer = [0u8; 8192];
            match self.reader.read(&mut buffer) {
                Ok(0) => {
                    return if self.decoder.buffered() == 0 {
                        Ok(None)
                    } else {
                        Err(ReadError::Truncated)
                    };
                }
                Ok(read) => self.decoder.push(&buffer[..read]),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(ReadError::Io(error)),
            }
        }
    }
}

/// What a reader thread reports.
#[derive(Debug)]
pub enum Inbound {
    Frame(Box<Envelope>),
    /// The other end closed the stream cleanly.
    Closed,
    /// The stream failed or carried something that is not a valid frame. The
    /// connection is dropped: there is no telling where the next frame starts.
    Broken(String),
}

/// Read a stream on its own thread and report everything through `tx`, tagged
/// by `wrap`. The thread ends when the stream does, or when nobody listens.
pub fn spawn_reader<R, E>(
    name: &str,
    mut reader: FramedReader<R>,
    tx: Sender<E>,
    wrap: impl Fn(Inbound) -> E + Send + 'static,
) -> JoinHandle<()>
where
    R: Read + Send + 'static,
    E: Send + 'static,
{
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            loop {
                let inbound = match reader.read_next() {
                    Ok(Some(envelope)) => Inbound::Frame(Box::new(envelope)),
                    Ok(None) => Inbound::Closed,
                    Err(error) => Inbound::Broken(error.to_string()),
                };
                let last = !matches!(inbound, Inbound::Frame(_));
                if tx.send(wrap(inbound)).is_err() || last {
                    break;
                }
            }
        })
        .expect("a reader thread can be spawned")
}

#[cfg(test)]
mod tests {
    use super::*;
    use interaction_contracts::desktop::device::{CommandAck, ResumeCommand, StopScope};
    use std::sync::mpsc;

    fn clock() -> SharedClock {
        SharedClock::starting_now()
    }

    #[test]
    fn what_one_end_sends_the_other_reads_with_numbering_and_replies_intact() {
        let mut bytes = Vec::new();
        let first_id;
        {
            let mut link = Link::new(&mut bytes, "host", clock());
            first_id = link
                .send(Message::ResumeCommand(ResumeCommand {
                    scope: StopScope::All,
                }))
                .unwrap();
            link.reply(&first_id, Message::CommandAck(CommandAck::ok()))
                .unwrap();
        }
        let mut reader = FramedReader::new(bytes.as_slice());
        let first = reader.read_next().unwrap().unwrap();
        let second = reader.read_next().unwrap().unwrap();
        assert_eq!((first.producer_sequence, second.producer_sequence), (1, 2));
        assert_eq!(first.message_id, first_id);
        assert_eq!(second.in_reply_to, Some(first_id));
        assert_eq!(first.producer.as_str(), "host");
        assert!(reader.read_next().unwrap().is_none(), "a clean end");
    }

    #[test]
    fn a_stream_cut_inside_a_frame_is_an_error_not_an_end() {
        let mut bytes = Vec::new();
        Link::new(&mut bytes, "host", clock())
            .send(Message::CommandAck(CommandAck::ok()))
            .unwrap();
        bytes.truncate(bytes.len() - 3);
        let mut reader = FramedReader::new(bytes.as_slice());
        assert!(matches!(reader.read_next(), Err(ReadError::Truncated)));
    }

    #[test]
    fn a_hostile_length_is_refused_before_anything_is_buffered() {
        let bytes = (u32::MAX).to_be_bytes().to_vec();
        let mut reader = FramedReader::new(bytes.as_slice());
        assert!(matches!(
            reader.read_next(),
            Err(ReadError::Frame(FrameError::TooLarge { .. }))
        ));
    }

    #[test]
    fn a_reader_thread_reports_frames_then_the_end() {
        let mut bytes = Vec::new();
        Link::new(&mut bytes, "device", clock())
            .send(Message::CommandAck(CommandAck::ok()))
            .unwrap();
        let (tx, rx) = mpsc::channel();
        spawn_reader(
            "test",
            FramedReader::new(std::io::Cursor::new(bytes)),
            tx,
            |i| i,
        );
        assert!(matches!(rx.recv().unwrap(), Inbound::Frame(_)));
        assert!(matches!(rx.recv().unwrap(), Inbound::Closed));
        assert!(rx.recv().is_err(), "the thread ended");
    }

    struct BlockedWriter {
        started: Sender<()>,
        release: Receiver<()>,
    }

    impl Write for BlockedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.started.send(()).unwrap();
            self.release.recv().unwrap();
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_blocked_peer_cannot_block_admission_and_a_full_queue_fails_closed() {
        let (started, began) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let mut writer = QueuedWriter::new(
            "blocked-output-test",
            BlockedWriter {
                started,
                release: released,
            },
            1,
        );
        writer.write_all(b"first").unwrap();
        began.recv_timeout(Duration::from_secs(1)).unwrap();
        writer.write_all(b"second").unwrap();
        assert_eq!(
            writer.write_all(b"third").unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert!(writer.failed());
        assert_eq!(
            writer.write_all(b"fourth").unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        release.send(()).unwrap();
        assert!(writer.finish(Duration::from_secs(1)));
    }

    #[test]
    fn queued_shutdown_has_a_bound_when_the_peer_never_reads() {
        let (started, began) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let mut writer = QueuedWriter::new(
            "blocked-shutdown-test",
            BlockedWriter {
                started,
                release: released,
            },
            1,
        );
        writer.write_all(b"first").unwrap();
        began.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(!writer.finish(Duration::from_millis(20)));
        release.send(()).unwrap();
        assert!(writer.finish(Duration::from_secs(1)));
    }

    #[test]
    fn a_background_write_failure_is_visible_without_sending_another_frame() {
        struct FailedWriter;
        impl Write for FailedWriter {
            fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "peer left"))
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut writer = QueuedWriter::new("failed-output-test", FailedWriter, 1);
        // Admission succeeds; the separate writer discovers the peer loss.
        let _ = writer.write_all(b"first");
        assert!(writer.finish(Duration::from_secs(1)));
        assert!(writer.failed());
    }
}
