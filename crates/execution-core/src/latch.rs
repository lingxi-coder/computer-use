//! The stop latch (design 14.8).
//!
//! A stop reaches the device directly from the operator, so the host may not
//! have seen it yet. The latch lets the device refuse the host's requests
//! until the host says which stop it has processed, and remembers which roots
//! are paused or cancelled so nothing but an explicit resume can lift a pause.

use std::collections::BTreeSet;

use interaction_contracts::desktop::device::StopDisposition;
use interaction_contracts::desktop::error::FailureCode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LatchVerdict {
    Open,
    /// The host has not acknowledged the latest stop.
    Behind {
        floor: u64,
    },
    Paused,
    Cancelled,
}

impl LatchVerdict {
    /// The failure code and reason for a refused request.
    pub fn refusal(&self, root: &str) -> Option<(FailureCode, String)> {
        match self {
            Self::Open => None,
            Self::Behind { floor } => Some((
                FailureCode::StopLatched,
                format!("stop latched: the host has not acknowledged stop {floor}"),
            )),
            Self::Paused => Some((
                FailureCode::StopLatched,
                format!("root {root} is paused until an explicit resume"),
            )),
            Self::Cancelled => Some((
                FailureCode::Cancelled,
                format!("root {root} was cancelled; start a new task"),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeError {
    Cancelled(String),
}

#[derive(Debug, Default)]
pub struct StopLatch {
    floor: u64,
    next_seq: u64,
    paused: BTreeSet<String>,
    tombstones: BTreeSet<String>,
}

impl StopLatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// The highest stop that has been latched. Requests must acknowledge at least this.
    pub fn floor(&self) -> u64 {
        self.floor
    }

    /// Latch a stop over the given roots and return its sequence number.
    pub fn latch(&mut self, disposition: StopDisposition, roots: &[String]) -> u64 {
        self.next_seq += 1;
        self.floor = self.next_seq;
        for root in roots {
            match disposition {
                StopDisposition::Pause => {
                    if !self.tombstones.contains(root) {
                        self.paused.insert(root.clone());
                    }
                }
                StopDisposition::Cancel => {
                    self.paused.remove(root);
                    self.tombstones.insert(root.clone());
                }
            }
        }
        self.floor
    }

    /// Whether a host request may proceed.
    pub fn check(&self, acked_stop_seq: u64, root: &str) -> LatchVerdict {
        if self.tombstones.contains(root) {
            LatchVerdict::Cancelled
        } else if acked_stop_seq < self.floor {
            LatchVerdict::Behind { floor: self.floor }
        } else if self.paused.contains(root) {
            LatchVerdict::Paused
        } else {
            LatchVerdict::Open
        }
    }

    /// Lift the pause on these roots. A cancelled root never comes back.
    pub fn resume(&mut self, roots: &[String]) -> Result<Vec<String>, ResumeError> {
        for root in roots {
            if self.tombstones.contains(root) {
                return Err(ResumeError::Cancelled(root.clone()));
            }
        }
        Ok(roots
            .iter()
            .filter(|root| self.paused.remove(root.as_str()))
            .cloned()
            .collect())
    }

    /// Every currently paused root, for a resume of everything.
    pub fn paused_roots(&self) -> Vec<String> {
        self.paused.iter().cloned().collect()
    }

    pub fn is_paused(&self, root: &str) -> bool {
        self.paused.contains(root)
    }

    pub fn is_cancelled(&self, root: &str) -> bool {
        self.tombstones.contains(root)
    }

    pub fn tombstone(&mut self, root: &str) {
        self.paused.remove(root);
        self.tombstones.insert(root.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    #[test]
    fn a_new_latch_lets_everything_through() {
        let latch = StopLatch::new();
        assert_eq!(latch.check(0, "root-1"), LatchVerdict::Open);
        assert_eq!(latch.floor(), 0);
    }

    #[test]
    fn requests_behind_the_floor_are_refused_until_they_catch_up() {
        let mut latch = StopLatch::new();
        let seq = latch.latch(StopDisposition::Pause, &roots(&["root-1"]));
        assert_eq!(seq, 1);
        assert_eq!(latch.check(0, "other"), LatchVerdict::Behind { floor: 1 });
        // Caught up: an unrelated root is fine, the paused one is not.
        assert_eq!(latch.check(1, "other"), LatchVerdict::Open);
        assert_eq!(latch.check(1, "root-1"), LatchVerdict::Paused);
    }

    #[test]
    fn only_an_explicit_resume_lifts_a_pause() {
        let mut latch = StopLatch::new();
        latch.latch(StopDisposition::Pause, &roots(&["root-1"]));
        assert_eq!(
            latch.check(5, "root-1"),
            LatchVerdict::Paused,
            "no amount of catching up resumes it"
        );
        assert_eq!(latch.resume(&roots(&["root-1"])), Ok(roots(&["root-1"])));
        assert_eq!(latch.check(1, "root-1"), LatchVerdict::Open);
        assert_eq!(
            latch.resume(&roots(&["root-1"])),
            Ok(vec![]),
            "resuming twice is harmless"
        );
    }

    #[test]
    fn cancelled_roots_never_come_back() {
        let mut latch = StopLatch::new();
        latch.latch(StopDisposition::Cancel, &roots(&["root-1"]));
        assert_eq!(latch.check(9, "root-1"), LatchVerdict::Cancelled);
        assert_eq!(
            latch.resume(&roots(&["root-1"])),
            Err(ResumeError::Cancelled("root-1".into()))
        );
        // A later pause cannot soften a tombstone into a pause.
        latch.latch(StopDisposition::Pause, &roots(&["root-1"]));
        assert!(!latch.is_paused("root-1"));
        assert!(latch.is_cancelled("root-1"));
    }

    #[test]
    fn sequence_numbers_only_increase() {
        let mut latch = StopLatch::new();
        assert_eq!(latch.latch(StopDisposition::Pause, &[]), 1);
        assert_eq!(latch.latch(StopDisposition::Pause, &[]), 2);
        assert_eq!(latch.floor(), 2);
    }

    #[test]
    fn refusals_carry_the_matching_failure_code() {
        let (code, _) = LatchVerdict::Behind { floor: 3 }.refusal("r").unwrap();
        assert_eq!(code, FailureCode::StopLatched);
        let (code, _) = LatchVerdict::Cancelled.refusal("r").unwrap();
        assert_eq!(code, FailureCode::Cancelled);
        assert!(LatchVerdict::Open.refusal("r").is_none());
    }
}
