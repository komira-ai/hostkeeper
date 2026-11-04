//! Why an unpack did not give a sealed version, and how the step record reports it: each
//! [`UnpackError`] has one outcome, one class and one reason token ([`UnpackError::outcome`]).

use std::fmt;
use std::io;

use hostkeeper_config::manifest::{Class as ManifestClass, ManifestRefusal};
use hostkeeper_hook::{Class, Outcome};

/// The archive breaks a bound or a shape rule (design section 5.5).
pub const REASON_BAD_ARCHIVE: &str = "bad_archive";
/// The archive's SHA-256 is not the one the order names.
pub const REASON_DIGEST_MISMATCH: &str = "digest_mismatch";
/// The manifest is refused (the loader's own token, `manifest_mismatch` for every refusal of
/// the package).
pub const REASON_MANIFEST_MISMATCH: &str = "manifest_mismatch";
/// The package needs a newer hostkeeper, or its version's seal was written by one.
pub const REASON_DAEMON_TOO_OLD: &str = "daemon_too_old";
/// The disk or the quota is full.
pub const REASON_NO_SPACE: &str = "no_space";
/// The version is on the host but is not what its seal says, or was sealed for another archive.
pub const REASON_ARTIFACT_CHANGED: &str = "artifact_changed";
/// The order names a version that is one of the package directory's own names (`current`,
/// `previous`, `*.seal`, `*.tmp`).
pub const REASON_RESERVED_VERSION: &str = "reserved_version";
/// The reason of [`UnpackError::VersionTooLong`].
pub const REASON_VERSION_TOO_LONG: &str = "version_too_long";
/// The archive, the staging directory or a seal cannot be read or written right now, for a
/// reason that is not a full disk: tried again later.
pub const REASON_IO_ERROR: &str = "io_error";

/// The rule an archive broke.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rule {
    /// More decompressed bytes than `max_unpacked_bytes`, counted as they leave the decoder.
    TooLarge,
    /// More entries than `max_entries`.
    TooManyEntries,
    /// A name over [`crate::MAX_NAME_BYTES`] bytes.
    NameTooLong,
    /// A name of more than [`crate::MAX_DEPTH`] components.
    TooDeep,
    /// A `manifest.toml` over 64 KiB.
    ManifestTooLarge,
    /// A top-level entry other than the file `manifest.toml` and the directories `bin`,
    /// `units` and `files`.
    TopLevel,
    /// An entry that is neither a regular file nor a directory: a link, a device, a fifo, a
    /// global header or any other tar type, by its type byte.
    Kind(u8),
    /// An absolute name, a `.` or `..` component, an empty component, a NUL.
    BadPath,
    /// A name that is not UTF-8.
    NotUtf8,
    /// An entry whose parent directory did not come before it.
    ParentMissing,
    /// A name that came before.
    Duplicate,
    /// A name equal to one that came before after simple lower-case folding.
    CaseFold,
    /// An extended attribute or an ACL (a pax `SCHILY.xattr.*`, `LIBARCHIVE.xattr.*`,
    /// `SCHILY.acl.*` or `security.*` record).
    ExtendedAttribute,
    /// A pax record that is not `path`, `size`, `mtime`, `uid`, `gid`, `uname` or `gname`.
    PaxRecord,
    /// A setuid, setgid or sticky bit.
    SpecialBits,
    /// A header that is not a ustar or GNU header with a correct checksum, or an extended header
    /// that is malformed, too large or not followed by an entry.
    Header,
    /// A byte after the end-of-archive marker that is not zero padding to the 512-byte block.
    Trailing,
    /// Compressed bytes after the end of the zstd frame (a second frame, or anything else).
    SecondFrame,
    /// The archive ends before its end-of-archive marker.
    Truncated,
    /// A zstd frame that declares a window above [`crate::MAX_WINDOW_BYTES`].
    Window { requested: u64 },
    /// The zstd decoder refused the stream.
    Decoder(String),
    /// What was read back from the staging directory is not what was written.
    NotAsWritten,
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Rule::TooLarge => write!(f, "more decompressed bytes than max_unpacked_bytes"),
            Rule::TooManyEntries => write!(f, "more entries than max_entries"),
            Rule::NameTooLong => write!(f, "a name over {} bytes", crate::MAX_NAME_BYTES),
            Rule::TooDeep => write!(f, "a name deeper than {}", crate::MAX_DEPTH),
            Rule::ManifestTooLarge => write!(f, "a manifest.toml over 65536 bytes"),
            Rule::TopLevel => write!(
                f,
                "a top-level entry other than the file manifest.toml and the directories bin, units and files"
            ),
            Rule::Kind(flag) => write!(
                f,
                "tar type {:?}, which is neither a regular file nor a directory",
                char::from(*flag)
            ),
            Rule::BadPath => write!(
                f,
                "an absolute name, a . or .. component, an empty component or a NUL"
            ),
            Rule::NotUtf8 => write!(f, "a name that is not UTF-8"),
            Rule::ParentMissing => write!(f, "an entry before its parent directory"),
            Rule::Duplicate => write!(f, "a name that came before"),
            Rule::CaseFold => write!(f, "a name equal to an earlier one after case folding"),
            Rule::ExtendedAttribute => write!(f, "an extended attribute or ACL"),
            Rule::PaxRecord => write!(f, "a pax record that is not allowed"),
            Rule::SpecialBits => write!(f, "a setuid, setgid or sticky bit"),
            Rule::Header => write!(f, "a malformed tar header"),
            Rule::Trailing => write!(f, "bytes after the end of the archive"),
            Rule::SecondFrame => write!(f, "compressed bytes after the zstd frame"),
            Rule::Truncated => write!(f, "the archive ends before its end marker"),
            Rule::Window { requested } => write!(
                f,
                "a zstd window of {requested} bytes, above the cap of {}",
                crate::MAX_WINDOW_BYTES
            ),
            Rule::Decoder(why) => write!(f, "the zstd stream is invalid: {why}"),
            Rule::NotAsWritten => write!(f, "the staged tree reads back other than it was written"),
        }
    }
}

/// Why [`crate::unpack`] gave no sealed version. Whatever the error, nothing outside the staging
/// directory was changed and no version directory was created, with one exception: an
/// [`UnpackError::Io`] or [`UnpackError::NoSpace`] from the last step of the publish (the fsync
/// of the service directory after the rename) leaves the version in place, whole and sealed,
/// and the next unpack of the same order takes it as it is.
#[derive(Debug, thiserror::Error)]
pub enum UnpackError {
    /// `entry` is the entry's name with every byte that is not printable ASCII escaped, or
    /// `the archive` for a rule of the stream; `at` is the count of decompressed bytes read
    /// when the rule was broken.
    #[error("bad archive: {entry}: {rule} (after {at} decompressed bytes)")]
    BadArchive { entry: String, rule: Rule, at: u64 },
    #[error("the archive's SHA-256 is {found}, not {expected}")]
    DigestMismatch { expected: String, found: String },
    #[error(transparent)]
    Manifest(#[from] ManifestRefusal),
    #[error("no space left while unpacking: {0}")]
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
    /// How a step record reports the error: the outcome (`Failure` for a bad package,
    /// `Deferred` for what the host must fix or wait out), the class and the reason token.
    pub fn outcome(&self) -> (Outcome, Class, &'static str) {
        const FAILED: (Outcome, Class) = (Outcome::Failure, Class::Release);
        const DEFERRED: (Outcome, Class) = (Outcome::Deferred, Class::Environment);
        let (kind, reason) = match self {
            UnpackError::BadArchive { .. } => (FAILED, REASON_BAD_ARCHIVE),
            UnpackError::DigestMismatch { .. } => (FAILED, REASON_DIGEST_MISMATCH),
            UnpackError::Manifest(refusal) => match refusal.class() {
                ManifestClass::Release => (FAILED, refusal.reason()),
                ManifestClass::Environment => (DEFERRED, refusal.reason()),
            },
            UnpackError::NoSpace(_) => (DEFERRED, REASON_NO_SPACE),
            UnpackError::ArtifactChanged { .. } => (FAILED, REASON_ARTIFACT_CHANGED),
            UnpackError::SealUnreadable { .. } => (DEFERRED, REASON_IO_ERROR),
            UnpackError::SealSchema { .. } => (DEFERRED, REASON_DAEMON_TOO_OLD),
            UnpackError::ReservedVersion { .. } => (FAILED, REASON_RESERVED_VERSION),
            UnpackError::VersionTooLong { .. } => (FAILED, REASON_VERSION_TOO_LONG),
            UnpackError::Io { .. } => (DEFERRED, REASON_IO_ERROR),
        };
        (kind.0, kind.1, reason)
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

/// `name` with every byte that is not printable ASCII (and `\`) escaped, for an error text.
pub(crate) fn escape(name: &[u8]) -> String {
    name.escape_ascii().to_string()
}
