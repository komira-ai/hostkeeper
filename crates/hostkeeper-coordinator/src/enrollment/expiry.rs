//! Expiry of the requests no operator approved in time (design 6.1: `pending` -> `unenrolled`,
//! the request expired).
//!
//! [`Enrollment::expire_requests`] is one sweep, at the time the enrollment's clock reads: every
//! request that has waited for an operator longer than the request TTL
//! (`--enrollment-request-ttl`, [`DEFAULT_REQUEST_TTL_SECS`] when not given) is expired by the
//! store ([`hostkeeper_enrollment::enroll::EnrollmentStore::expire`]) and audited
//! `request_expired`, once, under `coordinator`. The coordinator's reload tick runs it
//! ([`crate::listen::reload_every`]), so a request expires at most one tick after its TTL.
//!
//! The store decides which requests a sweep expired under its lock, so two sweeps never audit
//! one expiry. An entry that did not land (the audit write failed, or the coordinator stopped
//! between the store's write and the audit's) is written at the next start
//! ([`Enrollment::audit_to`]): a request the store holds expired is audited again when the last
//! entry the audit log holds for it is not its `request_expired`.

use std::collections::BTreeMap;

use hostkeeper_enrollment::enroll::RequestView;
use hostkeeper_enrollment::names::KeyHash;

use super::Enrollment;
use super::rpc::{ACTION_REQUESTED, audit_entry};
use crate::ops::{AuditEntry, OpsStore};

/// The audit action of a request that expired: its host is unenrolled.
pub(crate) const ACTION_EXPIRED: &str = "request_expired";

/// How long a request waits for an operator before it expires, unless
/// `--enrollment-request-ttl` says otherwise: 72 hours.
pub const DEFAULT_REQUEST_TTL_SECS: u64 = 72 * 3600;

/// The reason token of a host whose request expired, as `Enrollment.List` shows it.
pub(crate) const REASON_EXPIRED: &str = "request_expired";

/// The audit entry of the expiry of `view`.
fn expired_entry(view: &RequestView) -> AuditEntry {
    let ttl = view.expiry.map_or(0, |expiry| expiry.ttl_secs);
    audit_entry(
        ACTION_EXPIRED,
        view.renewal_of.as_ref(),
        format!(
            "request {}; round {}; no operator approved it within {ttl}s",
            view.request_id, view.round
        ),
    )
}

/// The request an `enroll_requested` or `request_expired` entry names, and whether it is the
/// expiry.
fn request_event(entry: &AuditEntry) -> Option<(&str, bool)> {
    let expired = entry.action == ACTION_EXPIRED;
    if !expired && entry.action != ACTION_REQUESTED {
        return None;
    }
    let id = entry
        .reason
        .as_deref()?
        .strip_prefix("request ")?
        .get(..64)?;
    Some((id, expired))
}

/// What the audit log says of each request's expiry: per request id, whether the last of its
/// `enroll_requested` and `request_expired` entries is the expiry.
#[derive(Debug, Default)]
pub(super) struct ExpiriesAudited(BTreeMap<String, bool>);

impl ExpiriesAudited {
    /// Takes `entry` in, in the order of the log.
    pub(super) fn see(&mut self, entry: &AuditEntry) {
        if let Some((id, expired)) = request_event(entry) {
            self.0.insert(id.to_owned(), expired);
        }
    }

    /// The expiry of `view` is in the log, or went with a segment retention deleted (it expired
    /// before `pruned_before`, the time of the oldest line kept, and the log holds nothing of
    /// it).
    fn holds(&self, view: &RequestView, pruned_before: Option<u64>) -> bool {
        match self.0.get(view.request_id.to_hex().as_str()) {
            Some(expired) => *expired,
            None => {
                let at = view.expiry.map_or(0, |expiry| expiry.expired_unix);
                pruned_before.is_some_and(|t| at < t)
            }
        }
    }
}

impl Enrollment {
    /// The same enrollment, expiring a request that has waited for an operator more than
    /// `secs` seconds (`--enrollment-request-ttl`).
    #[must_use]
    pub fn with_request_ttl(mut self, secs: u64) -> Self {
        self.request_ttl_secs = secs;
        self
    }

    /// How long a request waits for an operator before it expires, in seconds.
    #[must_use]
    pub fn request_ttl_secs(&self) -> u64 {
        self.request_ttl_secs
    }

    /// One sweep at the time the clock reads: every request that has waited longer than the
    /// request TTL expires, and each is audited `request_expired`. Returns the ids of the
    /// requests it expired. A sweep that cannot read or write the store is logged at error
    /// level; what it expired before the failure is audited, and the next sweep goes on.
    pub fn expire_requests(&self) -> Vec<KeyHash> {
        let now = self.clock.now_unix();
        let expired = match self.store.expire(now, self.request_ttl_secs) {
            Ok(expired) => expired,
            Err(why) => {
                tracing::error!(%why, "cannot expire the enrollment requests");
                // A request expired before the failure is audited at the next start.
                return Vec::new();
            }
        };
        for view in &expired {
            tracing::info!(
                request_id = %view.request_id,
                ttl_secs = self.request_ttl_secs,
                "an enrollment request expired: no operator approved it in time"
            );
            self.audit(expired_entry(view));
        }
        expired.iter().map(|view| view.request_id).collect()
    }

    /// Audits in `ops` every expiry the store holds whose entry `audited` does not (one whose
    /// audit did not land before a stop); part of [`Enrollment::audit_to`].
    pub(super) fn audit_missing_expiries(
        &self,
        ops: &OpsStore,
        audited: &ExpiriesAudited,
        pruned_before: Option<u64>,
    ) {
        let expired = match self.store.expired() {
            Ok(expired) => expired,
            Err(why) => {
                tracing::error!(%why, "cannot read the expired requests: none is audited again");
                return;
            }
        };
        for view in expired
            .iter()
            .filter(|view| !audited.holds(view, pruned_before))
        {
            tracing::info!(
                request_id = %view.request_id,
                "auditing an expiry whose entry did not land before the last stop"
            );
            super::approve::write_entry(ops, expired_entry(view));
        }
    }
}
