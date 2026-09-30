//! One time base for the processes of a session (design 4.4 and 4.7).
//!
//! The processes run on one machine, so they share the system clock. The
//! supervisor picks an origin and tells every process, and each reads time as
//! milliseconds since that origin. This is a simulation convenience: a real
//! deployment declares a monotonic clock domain and a boot identity instead,
//! and takes the sleep behaviour of the chosen clock into account.

use std::time::{SystemTime, UNIX_EPOCH};

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SharedClock {
    origin_unix_ms: u64,
}

impl SharedClock {
    /// Begin a new time base now. Only the supervisor does this.
    pub fn starting_now() -> Self {
        Self {
            origin_unix_ms: unix_ms(),
        }
    }

    pub fn from_origin(origin_unix_ms: u64) -> Self {
        Self { origin_unix_ms }
    }

    pub fn origin_unix_ms(&self) -> u64 {
        self.origin_unix_ms
    }

    pub fn now_ms(&self) -> u64 {
        unix_ms().saturating_sub(self.origin_unix_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn processes_with_the_same_origin_agree_on_the_time() {
        let supervisor = SharedClock::starting_now();
        let child = SharedClock::from_origin(supervisor.origin_unix_ms());
        let a = supervisor.now_ms();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let b = child.now_ms();
        assert!((20..500).contains(&(b - a)), "{a} {b}");
        assert_eq!(child.origin_unix_ms(), supervisor.origin_unix_ms());
    }
}
