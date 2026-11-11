//! What the renewal of the MDM name's certificate reports to the alerts: whether it is failing
//! (since when, and why), whether the DNS provider refused the credential, and when the next
//! attempt may start. In memory, like the book of alerts that reads it: a restart starts clean and
//! the first failed attempt after it raises the alert again.

use std::sync::{Mutex, PoisonError};

/// A failing renewal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failing {
    /// When the first failure of this run of failures happened, unix seconds.
    pub since_unix: u64,
    /// The last failure, in words that hold no secret.
    pub message: String,
    /// The failures in this run.
    pub attempts: u32,
}

/// What the alerts read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// The renewal is failing.
    pub failing: Option<Failing>,
    /// The provider refused the credential at the last attempt that reached it.
    pub dns_rejected: Option<String>,
}

#[derive(Debug, Default)]
struct Inner {
    snapshot: Snapshot,
    retry_at_unix: u64,
    /// The version of the DNS credential the last failure was under.
    credential_version: u32,
}

/// The status of the renewal, shared by the renewer and the alerts.
#[derive(Debug, Default)]
pub struct TlsStatus {
    inner: Mutex<Inner>,
}

impl TlsStatus {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// An attempt at `now_unix` failed with `message`; the next may start at `retry_at_unix`;
    /// `rejected` says the DNS provider refused the credential.
    pub fn failed(
        &self,
        now_unix: u64,
        retry_at_unix: u64,
        message: &str,
        rejected: bool,
        credential_version: u32,
    ) {
        let mut inner = self.lock();
        inner.credential_version = credential_version;
        let (since_unix, attempts) = inner
            .snapshot
            .failing
            .as_ref()
            .map_or((now_unix, 1), |was| {
                (was.since_unix, was.attempts.saturating_add(1))
            });
        inner.snapshot.failing = Some(Failing {
            since_unix,
            message: message.to_owned(),
            attempts,
        });
        inner.snapshot.dns_rejected = rejected.then(|| message.to_owned());
        inner.retry_at_unix = retry_at_unix;
    }

    /// The DNS credential held is at `version`. A failure under another version no longer says
    /// what the credential will do: the wait after it ends and the refusal is no longer reported;
    /// the failure itself stands until an attempt ends in a certificate.
    pub fn credential_is(&self, version: u32) {
        let mut inner = self.lock();
        if inner.retry_at_unix != 0 && inner.credential_version != version {
            inner.retry_at_unix = 0;
            inner.snapshot.dns_rejected = None;
        }
    }

    /// An attempt ended in a certificate.
    pub fn succeeded(&self) {
        *self.lock() = Inner::default();
    }

    /// When the next attempt may start, unix seconds; 0 when none waits.
    #[must_use]
    pub fn retry_at_unix(&self) -> u64 {
        self.lock().retry_at_unix
    }

    /// What the alerts read now.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        self.lock().snapshot.clone()
    }
}
