//! How an installer step fails: an outcome, a class and a reason, as a hook step ends (packages
//! design section 5.1). The reasons are those of `macos-pkg`'s failure table
//! (packages-repositories.md section 4.7), and their tokens are only ever added.

use std::fmt;

/// Who has to act: a release that is wrong, or a host that must be fixed or waited for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// The package or the order is wrong; the same bytes fail on every host.
    Release,
    /// The host is wrong or busy; the same bytes may install elsewhere or later.
    Environment,
}

/// Whether the order failed, or is to be sent again later.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// `Failed`: the step ended and changed nothing it was not allowed to keep.
    Failed,
    /// `Deferred`: the step did not start its change; the order is to be sent again.
    Deferred,
}

/// Why a step failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Reason {
    /// The entry names a repository or a package version, which an artifact entry never does.
    OrderInvalid,
    /// The package is unsigned, its chain is not trusted, or its leaf is no configured signer.
    Signature,
    /// The package's shape, identity, version, listing or payload breaks a rule of `stage`.
    ManifestInvalid,
    /// Another service records the identifier, or another package's receipt lists a payload file.
    PackageConflict,
    /// A parent directory of a payload path is not root's, or another account can write it.
    UnsafeDirectory,
    /// macOS's installer is busy with another install.
    PackageManagerBusy,
    /// `installer` failed, the receipt does not show the version, or an installed file differs.
    InstallerError,
    /// The host's files could not be read or written, or a tool could not be run.
    IoError,
}

impl Reason {
    /// Every reason, in token order.
    pub const ALL: [Reason; 8] = [
        Reason::InstallerError,
        Reason::IoError,
        Reason::ManifestInvalid,
        Reason::OrderInvalid,
        Reason::PackageConflict,
        Reason::PackageManagerBusy,
        Reason::Signature,
        Reason::UnsafeDirectory,
    ];

    /// The reason as the verdict carries it.
    pub const fn token(self) -> &'static str {
        match self {
            Reason::OrderInvalid => "order_invalid",
            Reason::Signature => "signature",
            Reason::ManifestInvalid => "manifest_invalid",
            Reason::PackageConflict => "package_conflict",
            Reason::UnsafeDirectory => "unsafe_directory",
            Reason::PackageManagerBusy => "package_manager_busy",
            Reason::InstallerError => "installer_error",
            Reason::IoError => "io_error",
        }
    }

    /// Who has to act on it.
    pub const fn class(self) -> Class {
        match self {
            Reason::OrderInvalid
            | Reason::ManifestInvalid
            | Reason::PackageConflict
            | Reason::InstallerError => Class::Release,
            Reason::Signature
            | Reason::UnsafeDirectory
            | Reason::PackageManagerBusy
            | Reason::IoError => Class::Environment,
        }
    }

    /// `Deferred` for a busy installer, which a later try may pass; `Failed` otherwise.
    pub const fn outcome(self) -> Outcome {
        match self {
            Reason::PackageManagerBusy => Outcome::Deferred,
            _ => Outcome::Failed,
        }
    }
}

/// A step's failure: its reason and what the daemon reports with it.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub struct Failure {
    reason: Reason,
    detail: String,
}

impl Failure {
    /// A failure for `reason`, with `detail` as the report's text.
    pub fn new(reason: Reason, detail: impl Into<String>) -> Failure {
        Failure {
            reason,
            detail: detail.into(),
        }
    }

    /// Why the step failed.
    pub fn reason(&self) -> Reason {
        self.reason
    }

    /// What the report says beside the reason.
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.reason.token(), self.detail)
    }
}

/// The reason a step that succeeded reports when it found and corrected drift.
pub const DRIFT_REPAIRED: &str = "drift_repaired";

#[cfg(test)]
#[path = "outcome_tests.rs"]
mod tests;
