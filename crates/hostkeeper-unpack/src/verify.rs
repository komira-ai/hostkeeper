//! The checks of the staged tree on macOS (docs/design/hostkeeper-design.md, section 5.5, step 3,
//! third row, last clause): no entry carries a `com.apple.quarantine` attribute, and every
//! Mach-O binary carries a valid code signature.
//!
//! This module is pure: it holds the port the checks go through ([`MacVerify`]), the platform
//! the unpack runs on ([`Platform`]), the test for a Mach-O file and the walk over the staged
//! tree. No implementation of the port that reads real attributes or runs `codesign` is in this
//! crate; the daemon's macOS construction is to provide one.
//!
//! Every Mach-O file of the tree is checked, which covers `bin/` and every file the manifest
//! can name as a hook or a run executable, and needs no manifest, so the check runs before the
//! manifest is read. Scripts and other files have no signature and are never sent to the check.

use std::fmt;
use std::io;
use std::path::Path;

use crate::error::{Rule, UnpackError, escape};

/// The macOS checks of a staged tree, made by the adapter the daemon provides.
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

/// The platform the daemon runs on. On macOS it carries the verifier, so that the Linux
/// unpack cannot reach the checks.
pub enum Platform<'a> {
    Linux,
    MacOs(&'a dyn MacVerify),
}

impl fmt::Debug for Platform<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Platform::Linux => write!(f, "Linux"),
            Platform::MacOs(_) => write!(f, "MacOs"),
        }
    }
}

/// An entry written to the staging directory, as the checks need it.
pub(crate) struct StagedEntry {
    /// The entry's name in the tree, `/`-separated.
    pub(crate) name: String,
    /// Whether the entry is a file that starts with a Mach-O magic number ([`is_mach_o`]).
    pub(crate) mach_o: bool,
}

/// How many leading bytes of a file [`is_mach_o`] reads.
pub(crate) const HEAD_BYTES: usize = 8;

/// Whether a file whose first bytes are `head` (up to [`HEAD_BYTES`] of them) is a Mach-O file:
/// a thin 32-bit or 64-bit image in either byte order, or a fat (universal) image. The fat
/// magic `CA FE BA BE` is shared with Java class files, whose major version follows it and is
/// 45 or more, where a fat image has its architecture count, which is small; so it is a
/// Mach-O only when the next four bytes, big-endian, are under 45.
pub(crate) fn is_mach_o(head: &[u8]) -> bool {
    match head {
        [0xFE, 0xED, 0xFA, 0xCE | 0xCF, ..]
        | [0xCE | 0xCF, 0xFA, 0xED, 0xFE, ..]
        | [0xCA, 0xFE, 0xBA, 0xBF, ..]
        | [0xBE | 0xBF, 0xBA, 0xFE, 0xCA, ..] => true,
        [0xCA, 0xFE, 0xBA, 0xBE, a, b, c, d, ..] => u32::from_be_bytes([*a, *b, *c, *d]) < 45,
        _ => false,
    }
}

/// Checks the staged tree at `root` (the real path of the staging directory): first every
/// entry, files and directories, for a quarantine attribute, then every Mach-O file for a
/// valid signature. `at` is the count of decompressed bytes the pass read.
pub(crate) fn verify_staged_tree(
    root: &Path,
    entries: &[StagedEntry],
    mac: &dyn MacVerify,
    at: u64,
) -> Result<(), UnpackError> {
    let unavailable = |entry: &StagedEntry, source| UnpackError::VerifyUnavailable {
        entry: entry.name.clone(),
        source,
    };
    let refused = |entry: &StagedEntry, rule| UnpackError::BadArchive {
        entry: escape(entry.name.as_bytes()),
        rule,
        at,
    };
    for entry in entries {
        match mac.quarantine(&root.join(&entry.name)) {
            Ok(false) => {}
            Ok(true) => return Err(refused(entry, Rule::Quarantined)),
            Err(e) => return Err(unavailable(entry, e)),
        }
    }
    for entry in entries.iter().filter(|e| e.mach_o) {
        match mac.signature(&root.join(&entry.name)) {
            Ok(SignatureVerdict::Valid) => {}
            Ok(SignatureVerdict::Unsigned) => return Err(refused(entry, Rule::Unsigned)),
            Ok(SignatureVerdict::Invalid { note }) => {
                return Err(refused(entry, Rule::InvalidSignature(note)));
            }
            Err(e) => return Err(unavailable(entry, e)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::is_mach_o;

    /// Catches a Mach-O magic that is not recognised: every thin and fat magic, in both byte
    /// orders, is a Mach-O file, with or without bytes after it where none are needed.
    #[test]
    fn every_mach_o_magic_is_recognised() {
        let magics: [[u8; 4]; 7] = [
            [0xFE, 0xED, 0xFA, 0xCF],
            [0xCF, 0xFA, 0xED, 0xFE],
            [0xFE, 0xED, 0xFA, 0xCE],
            [0xCE, 0xFA, 0xED, 0xFE],
            [0xCA, 0xFE, 0xBA, 0xBF],
            [0xBE, 0xBA, 0xFE, 0xCA],
            [0xBF, 0xBA, 0xFE, 0xCA],
        ];
        for magic in magics {
            assert!(is_mach_o(&magic), "{magic:02x?}");
            assert!(
                is_mach_o(&[magic.as_slice(), &[0u8; 4]].concat()),
                "{magic:02x?}"
            );
        }
    }

    /// Catches a Java class file taken for a fat Mach-O: `CA FE BA BE` counts only with a small
    /// architecture count after it, and not when the file ends before that count.
    #[test]
    fn the_shared_fat_magic_needs_a_small_architecture_count() {
        assert!(is_mach_o(&[0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 2]));
        assert!(is_mach_o(&[0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 44]));
        assert!(!is_mach_o(&[0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 45]));
        assert!(!is_mach_o(&[0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 65]));
        assert!(!is_mach_o(&[0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0]));
        assert!(!is_mach_o(&[0xCA, 0xFE, 0xBA, 0xBE]));
    }

    /// Catches a script, an empty file or a short file taken for a Mach-O file.
    #[test]
    fn other_files_are_not_mach_o() {
        assert!(!is_mach_o(b""));
        assert!(!is_mach_o(&[0xCF, 0xFA, 0xED]));
        assert!(!is_mach_o(b"#!/bin/sh\n"));
        assert!(!is_mach_o(&[0x7F, b'E', b'L', b'F', 2, 1, 1, 0]));
        assert!(!is_mach_o(&[0xFE, 0xED, 0xFA, 0xCD]));
    }
}
