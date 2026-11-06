//! What the enrollment loop last said of the host's certificate, kept where the daemon's
//! heartbeat reads it ([`RenewalState`]): the last second the current certificate is valid,
//! and why the last step failed, if it did.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::enroller::{Enrollment, Error, Outcome};

/// What the enrollment loop's last step left.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Renewal {
    /// The last second the host's current certificate is valid, in seconds since the epoch;
    /// none while the host holds no certificate.
    pub not_after: Option<u64>,
    /// Why the last step (of enrollment or renewal, or of opening the enroller) failed, as its
    /// error says it; none once a step succeeds.
    pub last_error: Option<String>,
}

/// The enrollment loop's [`Renewal`], shared by every clone: [`crate::Enroller::run_loop`]
/// writes it after each step, and the daemon's heartbeat reads it ([`RenewalState::get`]).
/// It starts with no certificate and no error.
#[derive(Clone, Debug, Default)]
pub struct RenewalState(Arc<Mutex<Renewal>>);

impl RenewalState {
    /// What the last step left.
    #[must_use]
    pub fn get(&self) -> Renewal {
        self.lock().clone()
    }

    /// Notes that a step failed for `why`, before any enroller step could say otherwise (the
    /// enroller could not be opened): the certificate stays as it was noted.
    pub fn failed(&self, why: String) {
        self.lock().last_error = Some(why);
    }

    /// Notes the end of a step that came to `step` and left the host's enrollment at
    /// `enrollment`: the certificate's end is the enrollment's (none for a host not enrolled;
    /// kept as it was when the pki directory could not be read), and the error is the step's,
    /// cleared by a step that succeeded.
    pub(crate) fn record(&self, step: &Result<Outcome, Error>, enrollment: &Enrollment) {
        let mut renewal = self.lock();
        match enrollment {
            Enrollment::Enrolled { not_after, .. } => renewal.not_after = Some(*not_after),
            Enrollment::Unenrolled | Enrollment::Pending { .. } => renewal.not_after = None,
            Enrollment::Unreadable(_) => {}
        }
        renewal.last_error = step.as_ref().err().map(ToString::to_string);
    }

    fn lock(&self) -> MutexGuard<'_, Renewal> {
        // A writer never panics while it holds the lock: what it holds is whole.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
