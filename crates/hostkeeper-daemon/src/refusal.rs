//! Why the daemon did not run an order, or stopped without a verdict: one token of a closed set.
//!
//! A token is 1 to 64 bytes of `[a-z0-9_]`, the token grammar of the protobuf files
//! (`UpdateResult.reason` in `hostkeeper/agent/v1/agent.proto`). The set is closed: a later verb
//! adds its own variants here, and the heartbeat loop maps them onto the wire in one table.

use hostkeeper_config::manifest::{Class, ManifestRefusal};
use hostkeeper_placed::PlacedError;

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
    /// The dead-man could not stop a step at its bound: the service stays locked. A `force` that
    /// could not stop the running step of its service ends with it too.
    Unstopped,
    /// A `force` or a `force_rollback` of the service `hostkeeper`, which updates itself by its
    /// own path.
    SelfService,
    /// An `install` of a service that has a release installed (`current` names one).
    AlreadyInstalled,
    /// An `update` or `force_rollback` of a service with no release installed.
    NotInstalled,
    /// A `force` of a service whose first install is under way.
    Installing,
    /// An order of a service whose uninstall is unfinished (its tombstone in `placed.json`
    /// stands), other than the uninstall itself.
    Uninstalling,
    /// An `uninstall` the installed release has no way to run: its manifest has no
    /// `[hook.timeout_secs] uninstall`, or the service is `hostkeeper` itself, which the package
    /// manager removes. Refused before anything changes.
    UninstallUnsupported,
    /// The package claims a path another service owns, or one occupied by something
    /// `placed.json` does not list. Refused before anything changes.
    PathConflict,
    /// `placed.json` could not be read or written, or a placed link, `current` or a directory of
    /// the service could not be made or removed.
    LinkFailed,
    /// An `update` of the service `hostkeeper` while its local list of bad versions
    /// (`<state-dir>/hostkeeper/bad-versions`) cannot be read: no self-update runs until it can.
    BadVersionsUnreadable,
}

impl Refusal {
    /// Every refusal, in the order of the enum.
    pub const ALL: [Refusal; 29] = [
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
        Refusal::SelfService,
        Refusal::AlreadyInstalled,
        Refusal::NotInstalled,
        Refusal::Installing,
        Refusal::Uninstalling,
        Refusal::UninstallUnsupported,
        Refusal::PathConflict,
        Refusal::LinkFailed,
        Refusal::BadVersionsUnreadable,
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
            Refusal::SelfService => "self_service",
            Refusal::AlreadyInstalled => "already_installed",
            Refusal::NotInstalled => "not_installed",
            Refusal::Installing => "installing",
            Refusal::Uninstalling => "uninstalling",
            Refusal::UninstallUnsupported => "uninstall_unsupported",
            Refusal::PathConflict => "path_conflict",
            Refusal::LinkFailed => "link_failed",
            Refusal::BadVersionsUnreadable => "bad_versions_unreadable",
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

impl From<&PlacedError> for Refusal {
    /// `path_conflict` for a path held by another service or by something `placed.json` does not
    /// list, `manifest_mismatch` for a package whose paths or units break a rule of linking,
    /// `uninstalling` for a tombstoned service, and `link_failed` for every failure of the host.
    fn from(error: &PlacedError) -> Refusal {
        match error {
            PlacedError::PathConflict { .. } => Refusal::PathConflict,
            PlacedError::Claim(_) | PlacedError::Lint(_) => Refusal::ManifestMismatch,
            PlacedError::Tombstoned { .. } => Refusal::Uninstalling,
            _ => Refusal::LinkFailed,
        }
    }
}
