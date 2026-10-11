//! Latest-only image delivery runs independently of source/control IO.
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use desktop_ipc::image::{AUTH_MAGIC, AUTH_OK, ImageFrame};
use desktop_ipc::socket;

const BUDGET: Duration = Duration::from_millis(250);
const POLL: Duration = Duration::from_millis(10);

pub struct ImageServer {
    pending: Arc<Mutex<PendingImages>>,
    cancel: Arc<AtomicBool>,
    current: Arc<Mutex<Option<UnixStream>>>,
    thread: Option<JoinHandle<()>>,
    path: PathBuf,
}

#[derive(Default)]
struct PendingImages {
    stream: Option<ImageFrame>,
    capture: Option<ImageFrame>,
    capture_expires: Option<Instant>,
}

impl PendingImages {
    fn take(&mut self) -> Option<ImageFrame> {
        if self
            .capture_expires
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.capture = None;
            self.capture_expires = None;
        }
        // Preserve native sequence order when an older stream image has not
        // reached Main yet. Later stream pixels cannot overwrite a capture.
        if self
            .stream
            .as_ref()
            .zip(self.capture.as_ref())
            .is_some_and(|(stream, capture)| {
                stream.meta.watch_id() == capture.meta.watch_id()
                    && stream.meta.source_sequence < capture.meta.source_sequence
            })
        {
            return self.stream.take();
        }
        self.capture.take().or_else(|| self.stream.take())
    }
    fn offer(&mut self, frame: ImageFrame) {
        if frame.meta.capture_info().is_some() {
            self.capture = Some(frame);
            self.capture_expires = Some(Instant::now() + Duration::from_secs(2));
        } else {
            self.stream = Some(frame);
        }
    }
}

struct DeadlineWriter<'a> {
    stream: &'a mut UnixStream,
    cancel: &'a AtomicBool,
    deadline: Instant,
}
impl Write for DeadlineWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        loop {
            if self.cancel.load(Ordering::Acquire) || Instant::now() >= self.deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "image delivery deadline",
                ));
            }
            match self.stream.write(bytes) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(2))
                }
                result => return result,
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl ImageServer {
    pub fn new(path: &Path, token: String, epoch: u64) -> io::Result<Self> {
        let listener = socket::listen(path)?;
        listener.set_nonblocking(true)?;
        let pending: Arc<Mutex<PendingImages>> = Arc::new(Mutex::new(PendingImages::default()));
        let cancel = Arc::new(AtomicBool::new(false));
        let current: Arc<Mutex<Option<UnixStream>>> = Arc::new(Mutex::new(None));
        let (frames, cancelled, connection) = (pending.clone(), cancel.clone(), current.clone());
        let worker = thread::Builder::new()
            .name("source-images".into())
            .spawn(move || {
                // One authenticated supervisor connection per source lifetime.
                let mut stream = loop {
                    if cancelled.load(Ordering::Acquire) {
                        return;
                    }
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let Ok(copy) = stream.try_clone() else {
                                continue;
                            };
                            *connection.lock().unwrap() = Some(copy);
                            let _ = stream.set_nonblocking(true);
                            let deadline = Instant::now() + BUDGET;
                            let mut auth = [0; 48];
                            let mut used = 0;
                            while used < auth.len()
                                && Instant::now() < deadline
                                && !cancelled.load(Ordering::Acquire)
                            {
                                match stream.read(&mut auth[used..]) {
                                    Ok(0) => break,
                                    Ok(count) => used += count,
                                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                                        thread::sleep(Duration::from_millis(2))
                                    }
                                    Err(_) => break,
                                }
                            }
                            if used == auth.len()
                                && &auth[..8] == AUTH_MAGIC
                                && auth[8..40] == *token.as_bytes()
                                && auth[40..] == epoch.to_be_bytes()
                            {
                                let mut out = DeadlineWriter {
                                    stream: &mut stream,
                                    cancel: &cancelled,
                                    deadline,
                                };
                                if out.write_all(AUTH_OK).is_ok() {
                                    break stream;
                                }
                            }
                            *connection.lock().unwrap() = None;
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => thread::sleep(POLL),
                        Err(_) => return,
                    }
                };
                while !cancelled.load(Ordering::Acquire) {
                    let frame = frames.lock().unwrap().take();
                    if let Some(frame) = frame {
                        // Size/container/checksum validation and JSON serialization
                        // happen on this worker, never on the stop/control loop.
                        let deadline = Instant::now() + BUDGET;
                        let mut out = DeadlineWriter {
                            stream: &mut stream,
                            cancel: &cancelled,
                            deadline,
                        };
                        if let Err(error) = frame.write_to_with_check(&mut out, || {
                            if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
                                Err(io::Error::new(
                                    io::ErrorKind::TimedOut,
                                    "image delivery deadline",
                                ))
                            } else {
                                Ok(())
                            }
                        }) && error.kind() != io::ErrorKind::InvalidData
                        {
                            break;
                        }
                    } else {
                        thread::sleep(POLL);
                    }
                }
                let _ = stream.shutdown(Shutdown::Both);
                *connection.lock().unwrap() = None;
            })?;
        Ok(Self {
            pending,
            cancel,
            current,
            thread: Some(worker),
            path: path.into(),
        })
    }

    /// A second Complete replaces the unsent frame. At most one pending and one
    /// in-flight immutable payload are retained, irrespective of consumer speed.
    pub fn offer(&self, frame: ImageFrame) {
        self.pending.lock().unwrap().offer(frame);
    }
    pub fn clear(&self) {
        *self.pending.lock().unwrap() = PendingImages::default();
    }
}
impl Drop for ImageServer {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        self.clear();
        if let Some(stream) = self.current.lock().unwrap().as_ref() {
            let _ = stream.shutdown(Shutdown::Both);
        }
        if let Some(worker) = self.thread.take() {
            let _ = worker.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    fn auth(stream: &mut UnixStream, token: &str) {
        stream.write_all(AUTH_MAGIC).unwrap();
        stream.write_all(token.as_bytes()).unwrap();
        stream.write_all(&7u64.to_be_bytes()).unwrap();
    }
    #[test]
    fn private_authenticated_channel_delivers_the_shared_packet_and_rejects_wrong_cookie() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("images.sock");
        let token = "a".repeat(32);
        let server = ImageServer::new(&path, token.clone(), 7).unwrap();
        let mut bad = UnixStream::connect(&path).unwrap();
        bad.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        auth(&mut bad, &"b".repeat(32));
        assert!(bad.read_exact(&mut [0; 8]).is_err());
        let mut good = UnixStream::connect(&path).unwrap();
        good.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        auth(&mut good, &token);
        let mut ack = [0; 8];
        good.read_exact(&mut ack).unwrap();
        assert_eq!(&ack, AUTH_OK);
        let packet = include_bytes!("../../../contracts/desktop-v1/image-channel/packet.bin");
        let original = desktop_ipc::image::read_from(&mut &packet[..]).unwrap();
        server.offer(original.clone());
        let received = desktop_ipc::image::read_from(&mut good).unwrap();
        assert_eq!(received.meta, original.meta);
        assert_eq!(received.bytes, original.bytes);
        drop(server);
        assert!(!path.exists());
    }
    #[test]
    fn shutdown_is_bounded_even_when_an_authenticated_consumer_never_reads_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("images.sock");
        let token = "a".repeat(32);
        let server = ImageServer::new(&path, token.clone(), 7).unwrap();
        let mut peer = UnixStream::connect(&path).unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        auth(&mut peer, &token);
        peer.read_exact(&mut [0; 8]).unwrap();
        let packet = include_bytes!("../../../contracts/desktop-v1/image-channel/packet.bin");
        let mut frame = desktop_ipc::image::read_from(&mut &packet[..]).unwrap();
        let mut bytes = frame.bytes[..33].to_vec();
        let padding = 8 * 1024 * 1024;
        bytes.extend_from_slice(&(padding as u32).to_be_bytes());
        bytes.extend_from_slice(b"tEXt");
        bytes.resize(bytes.len() + padding + 4, 0);
        bytes.extend_from_slice(&frame.bytes[33..]);
        frame.meta.byte_length = bytes.len() as u64;
        frame.meta.checksum = format!("{:x}", Sha256::digest(&bytes));
        frame.bytes = bytes.into();
        server.offer(frame.clone());
        thread::sleep(Duration::from_millis(40));
        for _ in 0..32 {
            server.offer(frame.clone());
        }
        assert!(server.pending.lock().unwrap().stream.is_some());
        let start = Instant::now();
        drop(server);
        assert!(start.elapsed() < Duration::from_millis(500));
    }
}
