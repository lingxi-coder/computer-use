//! Private FD5 request channel. Pixel and safety I/O run on one bounded worker;
//! the Host control thread never waits for a socket, PNG, or human confirmation.

use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::fd::OwnedFd;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, SyncSender},
};
use std::time::Duration;

use desktop_ipc::computer_control::{
    ComputerImageResponse, ComputerSafetyReply, ComputerSafetyRequest, read_response,
    read_safety_reply, write_request, write_safety_request,
};
use interaction_contracts::desktop::computer::{ComputerFence, ComputerFrameRequest};

const FRAME_TIMEOUT: Duration = Duration::from_secs(2);
const SAFETY_TIMEOUT: Duration = Duration::from_secs(30);

struct DeadlineStream<'a> {
    stream: &'a mut UnixStream,
    clock: &'a (dyn Fn() -> u64 + Send + Sync),
    deadline_ms: u64,
    operation_timeout: Duration,
    read_timeout: Duration,
    write_timeout: Duration,
}
impl DeadlineStream<'_> {
    fn remaining(&self) -> io::Result<Duration> {
        let remaining = self.deadline_ms.saturating_sub((self.clock)());
        if remaining == 0 {
            Err(io::Error::from(io::ErrorKind::TimedOut))
        } else {
            Ok(Duration::from_millis(remaining).min(self.operation_timeout))
        }
    }
}
impl Read for DeadlineStream<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let timeout = self.remaining()?;
        if timeout != self.read_timeout {
            self.stream.set_read_timeout(Some(timeout))?;
            self.read_timeout = timeout;
        }
        self.stream.read(buffer)
    }
}
impl Write for DeadlineStream<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let timeout = self.remaining()?;
        if timeout != self.write_timeout {
            self.stream.set_write_timeout(Some(timeout))?;
            self.write_timeout = timeout;
        }
        self.stream.write(buffer)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.remaining()?;
        self.stream.flush()
    }
}

pub fn open_inherited_fd5() -> Result<UnixStream, String> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/fd/5")
        .map_err(|_| "private computer channel is unavailable".to_string())?;
    if !file
        .metadata()
        .map_err(|_| "invalid computer descriptor")?
        .file_type()
        .is_socket()
    {
        return Err("invalid computer descriptor".into());
    }
    let descriptor: OwnedFd = file.into();
    Ok(UnixStream::from(descriptor))
}

pub struct ComputerImagePortResult {
    pub request: ComputerFrameRequest,
    pub response: Result<ComputerImageResponse, String>,
    /// A complete wire reply was consumed, but this Root no longer wants it.
    pub cancelled: bool,
}
pub struct ComputerSafetyPortResult {
    pub request: ComputerSafetyRequest,
    pub response: Result<ComputerSafetyReply, String>,
    pub cancelled: bool,
}
pub enum ComputerPortResult {
    Image(Box<ComputerImagePortResult>),
    Safety(Box<ComputerSafetyPortResult>),
}
impl ComputerPortResult {
    pub fn fence(&self) -> &ComputerFence {
        match self {
            Self::Image(result) => &result.request.fence,
            Self::Safety(result) => &result.request.fence,
        }
    }
    fn failed(&self) -> bool {
        match self {
            Self::Image(result) => result.response.is_err(),
            Self::Safety(result) => result.response.is_err(),
        }
    }
    fn mark_cancelled(&mut self) {
        match self {
            Self::Image(result) => result.cancelled = true,
            Self::Safety(result) => result.cancelled = true,
        }
    }
}
enum Operation {
    Image(Box<ComputerFrameRequest>),
    Safety(Box<ComputerSafetyRequest>),
}
impl Operation {
    fn fence(&self) -> &ComputerFence {
        match self {
            Self::Image(request) => &request.fence,
            Self::Safety(request) => &request.fence,
        }
    }
    fn timeout(&self) -> Duration {
        match self {
            Self::Image(_) => FRAME_TIMEOUT,
            Self::Safety(_) => SAFETY_TIMEOUT,
        }
    }
}
struct Pending {
    fence: ComputerFence,
    cancelled: bool,
}
pub struct ComputerImagePort {
    stream: UnixStream,
    requests: Option<SyncSender<Operation>>,
    results: Receiver<ComputerPortResult>,
    pending: Option<Pending>,
    closed: Arc<AtomicBool>,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
}
impl ComputerImagePort {
    pub fn new(
        stream: UnixStream,
        clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    ) -> Result<Self, String> {
        stream
            .set_read_timeout(Some(FRAME_TIMEOUT))
            .map_err(|_| "computer read timeout")?;
        stream
            .set_write_timeout(Some(FRAME_TIMEOUT))
            .map_err(|_| "computer write timeout")?;
        let mut worker_stream = stream.try_clone().map_err(|_| "computer socket clone")?;
        let (sender, requests) = mpsc::sync_channel::<Operation>(1);
        let (results_sender, results) = mpsc::sync_channel(1);
        let closed = Arc::new(AtomicBool::new(false));
        let worker_closed = closed.clone();
        let worker_clock = clock.clone();
        std::thread::Builder::new()
            .name("host-computer-port".into())
            .spawn(move || {
                while let Ok(operation) = requests.recv() {
                    let fence = operation.fence().clone();
                    let timeout = operation.timeout();
                    let initial_timeout =
                        Duration::from_millis(fence.deadline_ms.saturating_sub(worker_clock()))
                            .min(timeout);
                    let reset = if initial_timeout.is_zero() {
                        Err(io::Error::from(io::ErrorKind::TimedOut))
                    } else {
                        worker_stream
                            .set_read_timeout(Some(initial_timeout))
                            .and_then(|()| worker_stream.set_write_timeout(Some(initial_timeout)))
                    };
                    let checkpoint = || {
                        if worker_closed.load(Ordering::Acquire)
                            || worker_clock() >= fence.deadline_ms
                        {
                            Err(io::Error::new(
                                io::ErrorKind::ConnectionAborted,
                                "computer request cancelled or expired",
                            ))
                        } else {
                            Ok(())
                        }
                    };
                    let mut stream = DeadlineStream {
                        stream: &mut worker_stream,
                        clock: &*worker_clock,
                        deadline_ms: fence.deadline_ms,
                        operation_timeout: timeout,
                        read_timeout: initial_timeout,
                        write_timeout: initial_timeout,
                    };
                    let ready = reset.and_then(|()| checkpoint());
                    let result = match operation {
                        Operation::Image(request) => {
                            let response = ready
                                .and_then(|()| write_request(&request, &mut stream, checkpoint))
                                .and_then(|()| {
                                    read_response(
                                        &mut stream,
                                        &request,
                                        || worker_clock(),
                                        checkpoint,
                                    )
                                })
                                .map_err(|error| {
                                    format!(
                                        "private computer image request failed ({:?})",
                                        error.kind()
                                    )
                                });
                            ComputerPortResult::Image(Box::new(ComputerImagePortResult {
                                request: *request,
                                response,
                                cancelled: false,
                            }))
                        }
                        Operation::Safety(request) => {
                            let response = ready
                                .and_then(|()| {
                                    write_safety_request(&request, &mut stream, checkpoint)
                                })
                                .and_then(|()| {
                                    read_safety_reply(
                                        &mut stream,
                                        &request,
                                        || worker_clock(),
                                        checkpoint,
                                    )
                                })
                                .map_err(|error| {
                                    format!(
                                        "private computer safety request failed ({:?})",
                                        error.kind()
                                    )
                                });
                            ComputerPortResult::Safety(Box::new(ComputerSafetyPortResult {
                                request: *request,
                                response,
                                cancelled: false,
                            }))
                        }
                    };
                    let failed = result.failed();
                    if failed {
                        worker_closed.store(true, Ordering::Release);
                        let _ = worker_stream.shutdown(Shutdown::Both);
                    }
                    if results_sender.try_send(result).is_err() || failed {
                        break;
                    }
                }
                let _ = worker_stream.shutdown(Shutdown::Both);
            })
            .map_err(|_| "computer port worker could not start".to_string())?;
        Ok(Self {
            stream,
            requests: Some(sender),
            results,
            pending: None,
            closed,
            clock,
        })
    }
    pub fn is_busy(&self) -> bool {
        self.pending.is_some()
    }
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
    pub fn request(&mut self, request: ComputerFrameRequest) -> Result<(), String> {
        request
            .validate()
            .map_err(|_| "invalid computer frame request")?;
        self.enqueue(Operation::Image(Box::new(request)))
    }
    pub fn request_safety(&mut self, request: ComputerSafetyRequest) -> Result<(), String> {
        request
            .fence
            .validate()
            .map_err(|_| "invalid computer safety fence")?;
        self.enqueue(Operation::Safety(Box::new(request)))
    }
    fn enqueue(&mut self, operation: Operation) -> Result<(), String> {
        let fence = operation.fence().clone();
        if self.closed.load(Ordering::Acquire)
            || self.pending.is_some()
            || (self.clock)() >= fence.deadline_ms
        {
            return Err("computer port is closed, busy, or expired".into());
        }
        self.requests
            .as_ref()
            .ok_or("computer port is closed")?
            .try_send(operation)
            .map_err(|_| "computer request queue is unavailable")?;
        self.pending = Some(Pending {
            fence,
            cancelled: false,
        });
        Ok(())
    }
    /// Retire this result without abandoning a partially received packet.
    /// Stop uses close(), which interrupts I/O immediately and seals the port.
    pub fn cancel_scope(&mut self, fence: &ComputerFence) {
        if let Some(pending) = self.pending.as_mut()
            && &pending.fence == fence
        {
            pending.cancelled = true;
        }
    }
    pub fn poll(&mut self) -> Option<ComputerPortResult> {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| (self.clock)() >= pending.fence.deadline_ms)
        {
            self.close();
        }
        let mut result = self.results.try_recv().ok()?;
        if self
            .pending
            .as_ref()
            .is_none_or(|pending| pending.cancelled || &pending.fence != result.fence())
            || self.closed.load(Ordering::Acquire)
            || (self.clock)() >= result.fence().deadline_ms
        {
            result.mark_cancelled();
        }
        self.pending = None;
        if result.failed() {
            self.close();
        }
        Some(result)
    }
    pub fn close(&mut self) {
        self.closed.store(true, Ordering::Release);
        self.requests = None;
        self.pending = None;
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}
impl Drop for ComputerImagePort {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
#[path = "computer_image_tests.rs"]
mod tests;
