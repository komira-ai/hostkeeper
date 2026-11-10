//! The macOS checks of a package's files: whether an entry carries a `com.apple.quarantine`
//! attribute, and whether a Mach-O binary carries a valid code signature (docs/design/
//! hostkeeper-design.md, section 5.5, step 3, third row, last clause).
//!
//! This module holds the port the checks go through ([`MacVerify`]) and what it answers
//! ([`SignatureVerdict`], [`VerifyIoError`]). The daemon's `HostMacVerify` is the implementation
//! that reads real attributes and runs `codesign`; the `macos-device` job runs it on a Mac.

use std::io;
use std::path::Path;

/// The macOS checks of a package's files, made by the adapter the daemon provides.
pub trait MacVerify {
    /// Whether the entry at `path` (a file or a directory, a link not followed) carries a
    /// `com.apple.quarantine` extended attribute.
    fn quarantine(&self, path: &Path) -> Result<bool, VerifyIoError>;

    /// What `codesign --verify --strict` says of the Mach-O file `binary`: whether it holds a
    /// valid signature of its own bytes, ad-hoc or of any identity. Not a team identity, not
    /// notarisation.
    fn signature(&self, binary: &Path) -> Result<SignatureVerdict, VerifyIoError>;
}

/// What the code signature check says of one binary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignatureVerdict {
    /// A valid signature of the binary's bytes.
    Valid,
    /// No signature at all.
    Unsigned,
    /// A signature that does not verify; `note` is the tool's own words.
    Invalid { note: String },
}

/// Why a check could not be made: the tool did not run or did not finish. The package is not
/// judged.
#[derive(Debug, thiserror::Error)]
pub enum VerifyIoError {
    /// The tool could not be started.
    #[error("{tool} could not be run: {source}")]
    Spawn {
        tool: &'static str,
        #[source]
        source: io::Error,
    },
    /// The tool was killed or ran out of time.
    #[error("{tool} did not finish: {why}")]
    Unfinished { tool: &'static str, why: String },
}
