//! Private directories, one-time tokens and Unix sockets (design 4.6).
//!
//! The socket file name is not authentication and neither is a pid. What keeps
//! one protocol role apart from another is the private directory, the file
//! permissions and a one-time credential delivered over the inherited pipe and
//! checked in the handshake. This isolates roles; it does not claim to be a
//! sandbox against another malicious process of the same user.

use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::{Duration, Instant};

/// Unix socket paths have a short limit; a long path fails in a confusing way.
const MAX_SOCKET_PATH: usize = 100;

/// A directory only this user can enter.
pub fn private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

/// 128 bits from the system, as hex. It is used once and never logged.
pub fn new_token() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn check_length(path: &Path) -> io::Result<()> {
    if path.as_os_str().len() > MAX_SOCKET_PATH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "socket path is too long for a Unix socket: {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

/// Listen on a path, replacing a stale socket file, readable by this user only.
pub fn listen(path: &Path) -> io::Result<UnixListener> {
    check_length(path)?;
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Connect, waiting for the listener to appear for up to `timeout`.
pub fn connect(path: &Path, timeout: Duration) -> io::Result<UnixStream> {
    check_length(path)?;
    let deadline = Instant::now() + timeout;
    loop {
        match UnixStream::connect(path) {
            Ok(stream) => return Ok(stream),
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_long_hex_and_never_repeat() {
        let a = new_token().unwrap();
        let b = new_token().unwrap();
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn a_private_directory_and_its_socket_are_closed_to_others() {
        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join("p");
        private_dir(&private).unwrap();
        assert_eq!(
            fs::metadata(&private).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let socket = private.join("s.sock");
        let listener = listen(&socket).unwrap();
        assert_eq!(
            fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let client = connect(&socket, Duration::from_secs(1)).unwrap();
        drop((listener, client));
        // A stale file from an earlier run does not block a new listener.
        assert!(listen(&socket).is_ok());
    }

    #[test]
    fn a_path_too_long_for_a_socket_is_refused_clearly() {
        let long = Path::new("/tmp").join("x".repeat(120)).join("s.sock");
        let error = listen(&long).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn connecting_to_nothing_gives_up_after_the_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let started = Instant::now();
        assert!(connect(&dir.path().join("none.sock"), Duration::from_millis(100)).is_err());
        assert!(started.elapsed() >= Duration::from_millis(100));
    }
}
