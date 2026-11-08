//! What [`crate::Daemon::status`] reports of the host besides its services: the host's
//! enrollment as the enrollment loop read it after its last step, and the heartbeat loop's link
//! to the coordinator. The two loops write it ([`crate::links`]); `status` reads it.
//! Neither loop reads it back, so what it holds changes nothing the loops do.

use std::sync::{Mutex, MutexGuard, PoisonError};

use hostkeeper_enroll::Enrollment;
use hostkeeper_link::BeatOutcome;

/// The heartbeat loop's link to the coordinator, since the daemon started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LinkHealth {
    /// The unix second of the last heartbeat the coordinator answered; `None` when none was.
    pub last_success_unix: Option<u64>,
    /// The heartbeats in a row not sent or not answered since the last one answered.
    pub consecutive_failures: u64,
}

impl LinkHealth {
    /// The link after a heartbeat that came to `outcome`, at unix second `now_unix`: an answered
    /// one is the last success and ends the failures; one not sent or not answered adds a
    /// failure; one the loop did not try to send (no certificate, or the installed services
    /// could not be read) changes nothing.
    pub fn after(self, outcome: &BeatOutcome, now_unix: u64) -> LinkHealth {
        match outcome {
            BeatOutcome::Sent => LinkHealth {
                last_success_unix: Some(now_unix),
                consecutive_failures: 0,
            },
            BeatOutcome::NotSent(_) => LinkHealth {
                consecutive_failures: self.consecutive_failures.saturating_add(1),
                ..self
            },
            BeatOutcome::NotEnrolled | BeatOutcome::NoFacts => self,
        }
    }
}

/// The enrollment and the link, each `None` until its loop wrote it first.
#[derive(Debug, Default)]
pub struct NodeState {
    enrollment: Mutex<Option<Enrollment>>,
    link: Mutex<Option<LinkHealth>>,
}

/// What `mutex` holds: a writer that panicked left a whole value, which is still the last one.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl NodeState {
    /// The host's enrollment the enrollment loop read last.
    pub fn enrollment(&self) -> Option<Enrollment> {
        lock(&self.enrollment).clone()
    }

    /// Holds `enrollment` as the host's enrollment.
    pub fn set_enrollment(&self, enrollment: Enrollment) {
        *lock(&self.enrollment) = Some(enrollment);
    }

    /// The link after the last heartbeat.
    pub fn link(&self) -> Option<LinkHealth> {
        *lock(&self.link)
    }

    /// Holds `link` as the link after the last heartbeat.
    pub fn set_link(&self, link: LinkHealth) {
        *lock(&self.link) = Some(link);
    }

    /// The link after one more heartbeat, which came to `outcome` at unix second `now_unix`
    /// ([`LinkHealth::after`]).
    pub fn beat(&self, outcome: &BeatOutcome, now_unix: u64) {
        let mut link = lock(&self.link);
        *link = Some(link.unwrap_or_default().after(outcome, now_unix));
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::Arc;

    use hostkeeper_link::LinkError;

    use super::*;

    /// Catches a success that does not end the failures, a failure that does not count, and a
    /// heartbeat never tried counted either way: from the start, two refusals count two, one
    /// not tried leaves them, an answer ends them at its second, and a later refusal keeps that
    /// second.
    #[test]
    fn each_heartbeat_moves_the_link_by_its_outcome() {
        let refused = BeatOutcome::NotSent(LinkError::Transport(io::ErrorKind::ConnectionRefused));
        let node = NodeState::default();
        assert_eq!(node.link(), None);
        node.beat(&refused, 10);
        node.beat(&refused, 20);
        let two = LinkHealth {
            last_success_unix: None,
            consecutive_failures: 2,
        };
        assert_eq!(node.link(), Some(two));
        node.beat(&BeatOutcome::NotEnrolled, 30);
        node.beat(&BeatOutcome::NoFacts, 40);
        assert_eq!(node.link(), Some(two));
        node.beat(&BeatOutcome::Sent, 50);
        let answered = LinkHealth {
            last_success_unix: Some(50),
            consecutive_failures: 0,
        };
        assert_eq!(node.link(), Some(answered));
        node.beat(&refused, 60);
        assert_eq!(
            node.link(),
            Some(LinkHealth {
                consecutive_failures: 1,
                ..answered
            })
        );
    }

    /// Catches a count that wraps to zero after the largest one.
    #[test]
    fn the_failure_count_stops_at_its_largest_value() {
        let full = LinkHealth {
            last_success_unix: None,
            consecutive_failures: u64::MAX,
        };
        let refused = BeatOutcome::NotSent(LinkError::Transport(io::ErrorKind::TimedOut));
        assert_eq!(full.after(&refused, 1), full);
    }

    /// Catches an enrollment not kept, or the one before kept in its place; and a lock a
    /// panicking writer left that loses what it held.
    #[test]
    fn the_last_enrollment_is_kept_and_survives_a_panicking_writer() {
        let node = Arc::new(NodeState::default());
        assert_eq!(node.enrollment(), None);
        node.set_enrollment(Enrollment::Unenrolled);
        node.set_enrollment(Enrollment::Unreadable("why".to_owned()));
        assert_eq!(
            node.enrollment(),
            Some(Enrollment::Unreadable("why".to_owned()))
        );
        let held = Arc::clone(&node);
        let panicked = std::thread::spawn(move || {
            let _guard = lock(&held.enrollment);
            panic!("a writer panics while it holds the lock");
        })
        .join();
        assert!(panicked.is_err());
        assert_eq!(
            node.enrollment(),
            Some(Enrollment::Unreadable("why".to_owned()))
        );
        node.set_link(LinkHealth::default());
        assert_eq!(node.link(), Some(LinkHealth::default()));
    }
}
