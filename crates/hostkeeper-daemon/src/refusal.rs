//! Why the daemon did not run an order, or stopped without a verdict: one token of a closed set.
//!
//! A token is 1 to 64 bytes of `[a-z0-9_]`, the token grammar of the protobuf files
//! (`UpdateResult.reason` in `hostkeeper/agent/v1/agent.proto`). The set is closed: a later verb
//! adds its own variants here, and the heartbeat loop maps them onto the wire in one table.

use hostkeeper_config::manifest::{Class, ManifestRefusal};

/// Why an order was refused or stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Refusal {
    /// The order is malformed: no verb, no try, or a field that breaks its grammar. Refused
    /// before any lock is taken.
    InvalidOrder,
    /// The verb is not built into this daemon yet.
    NotImplemented,
    /// Another package's order of the service is unfinished, or holds the service's lock.
    Busy,
    /// The service is held by a halt until `clear_halt`.
    Halted,
    /// The service's held halt generation cannot be read: no order runs for it until it can.
    HaltUnreadable,
    /// The order has the try of the service's last order and other fields.
    TryConflict,
    /// The order's try is below the try of the service's last order.
    StaleTry,
    /// An `update` to a version below the one the service runs.
    BelowRunning,
    /// The package's manifest is refused: its name or version is not the order's, or it breaks a
    /// rule of the manifest.
    ManifestMismatch,
    /// The package needs a newer hostkeeper than this daemon (`min_hostkeeper`).
    DaemonTooOld,
    /// The release channel has no such package.
    PackageNotFound,
    /// The package's bytes are not the digest the order names.
    DigestMismatch,
    /// The package cannot be staged: the disk is full.
    NoSpace,
    /// The package could not be fetched, verified or unpacked for another reason.
    PackageFailed,
    /// The service's step record cannot be read: no order runs for it until it can.
    RecordUnreadable,
    /// The service's step record was written by a newer daemon, and is never changed.
    RecordNewer,
    /// A file of the daemon (the step record, the params file, a service directory) could not
    /// be read or written.
    StoreFailed,
    /// A release on the host cannot be read now: the release the order replaces, or the staged
    /// one when its check fails with an I/O error.
    ReleaseUnreadable,
    /// The step launcher failed; the step is left `running` in the record and a resend resumes it.
    LauncherFailed,
    /// The dead-man could not stop a step at its bound: the service stays locked.
    Unstopped,
}

impl Refusal {
    /// Every refusal, in the order of the enum.
    pub const ALL: [Refusal; 20] = [
        Refusal::InvalidOrder,
        Refusal::NotImplemented,
        Refusal::Busy,
        Refusal::Halted,
        Refusal::HaltUnreadable,
        Refusal::TryConflict,
        Refusal::StaleTry,
        Refusal::BelowRunning,
        Refusal::ManifestMismatch,
        Refusal::DaemonTooOld,
        Refusal::PackageNotFound,
        Refusal::DigestMismatch,
        Refusal::NoSpace,
        Refusal::PackageFailed,
        Refusal::RecordUnreadable,
        Refusal::RecordNewer,
        Refusal::StoreFailed,
        Refusal::ReleaseUnreadable,
        Refusal::LauncherFailed,
        Refusal::Unstopped,
    ];

    /// The token.
    pub const fn token(self) -> &'static str {
        match self {
            Refusal::InvalidOrder => "invalid_order",
            Refusal::NotImplemented => "not_implemented",
            Refusal::Busy => "busy",
            Refusal::Halted => "halted",
            Refusal::HaltUnreadable => "halt_unreadable",
            Refusal::TryConflict => "try_conflict",
            Refusal::StaleTry => "stale_try",
            Refusal::BelowRunning => "below_running",
            Refusal::ManifestMismatch => "manifest_mismatch",
            Refusal::DaemonTooOld => "daemon_too_old",
            Refusal::PackageNotFound => "package_not_found",
            Refusal::DigestMismatch => "digest_mismatch",
            Refusal::NoSpace => "no_space",
            Refusal::PackageFailed => "package_failed",
            Refusal::RecordUnreadable => "record_unreadable",
            Refusal::RecordNewer => "record_newer",
            Refusal::StoreFailed => "store_failed",
            Refusal::ReleaseUnreadable => "release_unreadable",
            Refusal::LauncherFailed => "launcher_failed",
            Refusal::Unstopped => "unstopped",
        }
    }
}

impl From<&ManifestRefusal> for Refusal {
    /// `daemon_too_old` for a manifest that needs a newer daemon (class `Environment`),
    /// `manifest_mismatch` for every other refusal: the tokens [`ManifestRefusal::reason`] gives.
    fn from(refusal: &ManifestRefusal) -> Refusal {
        match refusal.class() {
            Class::Environment => Refusal::DaemonTooOld,
            Class::Release => Refusal::ManifestMismatch,
        }
    }
}
