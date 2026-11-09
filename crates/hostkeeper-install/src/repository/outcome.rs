//! How a step of the repository installer fails: an outcome, a class and a reason, as a hook
//! step ends (packages.md section 5.1). The reasons are those of packages-repositories.md
//! section 4.5, the entry's own (section 2.3) and those of packages-native.md section 10.2; their
//! tokens are only ever added.

use std::fmt;

/// Who has to act: a release that is wrong, or a host that must be fixed or waited for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// The package or the order is wrong; the same bytes fail on every host.
    Release,
    /// The host is wrong or busy; the same bytes may install elsewhere or later.
    Environment,
}

impl Class {
    /// The class as the verdict carries it.
    pub const fn token(self) -> &'static str {
        match self {
            Class::Release => "release",
            Class::Environment => "environment",
        }
    }
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
    /// The entry's shape is not its installer's: an artifact entry naming a repository or a
    /// package version, or a repository entry naming an artifact (section 2.3).
    OrderInvalid,
    /// A repository installer's entry with no repository: only a coordinator that dropped the
    /// field can send it (section 2.3).
    CoordinatorTooOld,
    /// The order's repository is not an entry of this host's configuration.
    RepositoryUnknown,
    /// The repository could not be reached, or the download failed.
    FetchFailed,
    /// The suite's signature did not verify, or its `Valid-Until` passed.
    Signature,
    /// The index lists no package of that version for this host's architecture.
    NotInRepository,
    /// The index's SHA-256 of the package is not the order's pin.
    PackageDigestMismatch,
    /// The index's size of the package is over `max_package_bytes`.
    PackageTooLarge,
    /// The index named the pin, but the downloaded file is other bytes.
    StoreDigestMismatch,
    /// The package's control fields, its tree or the entry's manifest break a rule of `stage`.
    ManifestInvalid,
    /// The package carries a maintainer script (packages-native.md section 10.2).
    PackageScripts,
    /// Another service records the same package.
    PackageConflict,
    /// A dependency is neither installed nor in the one repository.
    DependencyMissing,
    /// The install would change another package, or a purge would leave a dependent one.
    DependencyChanges,
    /// The package manager's lock is held past the step's bound.
    PackageManagerBusy,
    /// dpkg was interrupted before this run and needs `dpkg --configure -a`.
    DpkgInterrupted,
    /// The install failed, or the package manager does not show the version after it.
    DpkgError,
    /// The host's files could not be read or written, or a tool could not be run.
    IoError,
}

impl Reason {
    /// Every reason, in token order.
    pub const ALL: [Reason; 18] = [
        Reason::CoordinatorTooOld,
        Reason::DependencyChanges,
        Reason::DependencyMissing,
        Reason::DpkgError,
        Reason::DpkgInterrupted,
        Reason::FetchFailed,
        Reason::IoError,
        Reason::ManifestInvalid,
        Reason::NotInRepository,
        Reason::OrderInvalid,
        Reason::PackageConflict,
        Reason::PackageDigestMismatch,
        Reason::PackageManagerBusy,
        Reason::PackageScripts,
        Reason::PackageTooLarge,
        Reason::RepositoryUnknown,
        Reason::Signature,
        Reason::StoreDigestMismatch,
    ];

    /// The reason as the verdict carries it.
    pub const fn token(self) -> &'static str {
        match self {
            Reason::OrderInvalid => "order_invalid",
            Reason::CoordinatorTooOld => "coordinator_too_old",
            Reason::RepositoryUnknown => "repository_unknown",
            Reason::FetchFailed => "fetch_failed",
            Reason::Signature => "signature",
            Reason::NotInRepository => "not_in_repository",
            Reason::PackageDigestMismatch => "package_digest_mismatch",
            Reason::PackageTooLarge => "package_too_large",
            Reason::StoreDigestMismatch => "store_digest_mismatch",
            Reason::ManifestInvalid => "manifest_invalid",
            Reason::PackageScripts => "package_scripts",
            Reason::PackageConflict => "package_conflict",
            Reason::DependencyMissing => "dependency_missing",
            Reason::DependencyChanges => "dependency_changes",
            Reason::PackageManagerBusy => "package_manager_busy",
            Reason::DpkgInterrupted => "dpkg_interrupted",
            Reason::DpkgError => "dpkg_error",
            Reason::IoError => "io_error",
        }
    }

    /// Who has to act on it (section 4.5's table).
    pub const fn class(self) -> Class {
        match self {
            Reason::OrderInvalid
            | Reason::PackageDigestMismatch
            | Reason::PackageTooLarge
            | Reason::ManifestInvalid
            | Reason::PackageScripts
            | Reason::PackageConflict
            | Reason::DpkgError => Class::Release,
            Reason::CoordinatorTooOld
            | Reason::RepositoryUnknown
            | Reason::FetchFailed
            | Reason::Signature
            | Reason::NotInRepository
            | Reason::StoreDigestMismatch
            | Reason::DependencyMissing
            | Reason::DependencyChanges
            | Reason::PackageManagerBusy
            | Reason::DpkgInterrupted
            | Reason::IoError => Class::Environment,
        }
    }

    /// `Deferred` for what a later try may pass with nothing changed on the host (section 4.5);
    /// `Failed` otherwise.
    pub const fn outcome(self) -> Outcome {
        match self {
            Reason::CoordinatorTooOld
            | Reason::FetchFailed
            | Reason::NotInRepository
            | Reason::StoreDigestMismatch
            | Reason::PackageManagerBusy => Outcome::Deferred,
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

/// The reason a step that succeeded reports when it found and corrected drift (section 4.4).
pub const DRIFT_REPAIRED: &str = "drift_repaired";
