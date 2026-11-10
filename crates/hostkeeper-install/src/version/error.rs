//! Why a version directory was not published, and how the step record reports it: each
//! [`UnpackError`] has one reason token ([`UnpackError::reason`]).

use std::io;

/// The disk or the quota is full.
pub const REASON_NO_SPACE: &str = "no_space";
/// The version is on the host but is not what its seal says, or was sealed for another package.
pub const REASON_ARTIFACT_CHANGED: &str = "artifact_changed";
/// The seal of a version was written by a newer hostkeeper than this one.
pub const REASON_DAEMON_TOO_OLD: &str = "daemon_too_old";
/// The order names a version that is one of the package directory's own names (`current`,
/// `previous`, `*.seal`, `*.tmp`).
pub const REASON_RESERVED_VERSION: &str = "reserved_version";
/// The reason of [`UnpackError::VersionTooLong`].
pub const REASON_VERSION_TOO_LONG: &str = "version_too_long";
/// The staging directory or a seal cannot be read or written right now, for a reason that is not
/// a full disk: tried again later.
pub const REASON_IO_ERROR: &str = "io_error";

/// Why [`crate::version::stage_filled`] gave no sealed version. Whatever the error, nothing
/// outside the staging directory was changed and no version directory was created, with one
/// exception: an [`UnpackError::Io`] or [`UnpackError::NoSpace`] from the last steps of the
/// publish (the fsync of the service directory after the rename, the mode 0555 of the version
/// directory and its fsync) leaves the version in place, whole and sealed, and the next publish
/// of the same order takes it as it is and makes its directory 0555.
#[derive(Debug, thiserror::Error)]
pub enum UnpackError {
    #[error("no space left while publishing: {0}")]
    NoSpace(io::Error),
    /// The version is on the host already and is not what its seal says or what the order
    /// names. Nothing was touched.
    #[error("version {version} is on the host but {why}")]
    ArtifactChanged { version: String, why: String },
    /// The version is on the host already and its seal cannot be read now. Nothing was touched.
    #[error("the seal of version {version} cannot be read: {why}")]
    SealUnreadable { version: String, why: String },
    /// The version is on the host already with a seal of a schema this daemon does not read.
    /// Nothing was touched.
    #[error("the seal of version {version} has schema {found}, which this daemon does not read")]
    SealSchema { version: String, found: u64 },
    /// The order's version is a name the package directory holds for itself. Nothing was
    /// touched.
    #[error("the version {version:?} is a name the package directory holds for itself")]
    ReservedVersion { version: String },
    /// The order's version keeps its rule but is too long for the names the package directory
    /// derives from it (`.<version>.seal.tmp` would pass 255 bytes). Nothing was touched.
    #[error(
        "the version {version:?} is too long for the names the package directory derives from it"
    )]
    VersionTooLong { version: String },
    #[error("{what}: {source}")]
    Io {
        what: &'static str,
        #[source]
        source: io::Error,
    },
}

impl UnpackError {
    /// The reason token a step record reports the error with.
    pub fn reason(&self) -> &'static str {
        match self {
            UnpackError::NoSpace(_) => REASON_NO_SPACE,
            UnpackError::ArtifactChanged { .. } => REASON_ARTIFACT_CHANGED,
            UnpackError::SealUnreadable { .. } | UnpackError::Io { .. } => REASON_IO_ERROR,
            UnpackError::SealSchema { .. } => REASON_DAEMON_TOO_OLD,
            UnpackError::ReservedVersion { .. } => REASON_RESERVED_VERSION,
            UnpackError::VersionTooLong { .. } => REASON_VERSION_TOO_LONG,
        }
    }

    /// An error of a file-system call: [`UnpackError::NoSpace`] for `ENOSPC` and `EDQUOT`,
    /// [`UnpackError::Io`] naming `what` otherwise.
    pub(crate) fn of_io(what: &'static str, source: io::Error) -> UnpackError {
        use rustix::io::Errno;
        let full = Errno::from_io_error(&source)
            .is_some_and(|errno| (errno == Errno::NOSPC) | (errno == Errno::DQUOT));
        if full {
            UnpackError::NoSpace(source)
        } else {
            UnpackError::Io { what, source }
        }
    }
}
