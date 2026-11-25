//! The host prerequisites: the things outside any package that a host group's hosts must have,
//! which the coordinator names for each group (feature `prerequisites-v1`) and the daemon on each
//! host makes sure of, and the rules both sides read a set of them by.
//!
//! - [`Prerequisite`]: the prerequisites this release knows, each with its name on the wire and
//!   the platform it applies to; today one, [`Prerequisite::XcodeCommandLineTools`]
//!   (`xcode-command-line-tools`), on macOS;
//! - [`is_name`]: the grammar of a name, which a daemon checks before it knows the name, so that a
//!   name a newer coordinator sends is reported as not known rather than refused whole;
//! - [`check_set`]: a whole set: at most [`MAX_PREREQUISITES`] names, each by the grammar, none
//!   twice;
//! - [`digest`]: the SHA-256 of a set, which the coordinator sends with it and the daemon reports
//!   once it holds the set, so that a set is sent again only when the daemon does not hold it.

use sha2::{Digest, Sha256};

/// The most names of one set.
pub const MAX_PREREQUISITES: usize = 16;

/// The most bytes of one name.
pub const MAX_NAME_BYTES: usize = 64;

/// A prerequisite this release knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Prerequisite {
    /// Apple's Command Line Tools for Xcode (`xcode-select -p` names a developer directory, and
    /// `pkgutil` knows the package `com.apple.pkg.CLTools_Executables`, the tools' compiler is on
    /// disk, or the directory is a ready Xcode's), on macOS only.
    XcodeCommandLineTools,
}

/// Every prerequisite this release knows, in the order of their names.
pub const KNOWN: [Prerequisite; 1] = [Prerequisite::XcodeCommandLineTools];

impl Prerequisite {
    /// Its name on the wire and in the coordinator's file.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Prerequisite::XcodeCommandLineTools => "xcode-command-line-tools",
        }
    }

    /// The prerequisite named `name`, when this release knows it.
    #[must_use]
    pub fn parse(name: &str) -> Option<Prerequisite> {
        KNOWN.into_iter().find(|known| known.name() == name)
    }

    /// Whether it applies to a macOS host; a host of another platform reports it not applicable
    /// and does nothing for it.
    #[must_use]
    pub fn on_macos(self) -> bool {
        match self {
            Prerequisite::XcodeCommandLineTools => true,
        }
    }

    /// Whether it applies to a Linux host (systemd or OpenRC).
    #[must_use]
    pub fn on_linux(self) -> bool {
        match self {
            Prerequisite::XcodeCommandLineTools => false,
        }
    }
}

/// Why a set of names breaks a rule. A name is named by its place in the set, from 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PrerequisiteSetError {
    /// More than [`MAX_PREREQUISITES`] names.
    #[error("{count} prerequisites, more than the {MAX_PREREQUISITES} a set may hold")]
    TooMany {
        /// How many there are.
        count: usize,
    },
    /// A name that breaks the grammar of [`is_name`].
    #[error(
        "prerequisite {place} is not a prerequisite name (1 to {MAX_NAME_BYTES} bytes of a-z, 0-9 and -, starting with a letter, not ending with - or holding --)"
    )]
    Name {
        /// Its place, from 1.
        place: usize,
    },
    /// A name that an earlier one of the set already is.
    #[error("prerequisite {place} is prerequisite {first} again")]
    Twice {
        /// Its place, from 1.
        place: usize,
        /// The place of the first, from 1.
        first: usize,
    },
}

/// Whether `name` keeps the grammar of a prerequisite's name: 1 to [`MAX_NAME_BYTES`] bytes of
/// `a-z`, `0-9` and `-`, a letter first, not ending with `-` and holding no `--`.
#[must_use]
pub fn is_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    let Some(first) = bytes.first() else {
        return false;
    };
    bytes.len() <= MAX_NAME_BYTES
        && first.is_ascii_lowercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
        && !name.ends_with('-')
        && !name.contains("--")
}

/// Whether `names` keeps the rules of a set: at most [`MAX_PREREQUISITES`], each a name by
/// [`is_name`], none twice. A name this release does not know keeps the rules.
///
/// # Errors
/// The first rule broken ([`PrerequisiteSetError`]).
pub fn check_set(names: &[String]) -> Result<(), PrerequisiteSetError> {
    if names.len() > MAX_PREREQUISITES {
        return Err(PrerequisiteSetError::TooMany { count: names.len() });
    }
    for (index, name) in names.iter().enumerate() {
        let place = index + 1;
        if !is_name(name) {
            return Err(PrerequisiteSetError::Name { place });
        }
        if let Some(first) = names[..index].iter().position(|earlier| earlier == name) {
            return Err(PrerequisiteSetError::Twice {
                place,
                first: first + 1,
            });
        }
    }
    Ok(())
}

/// The SHA-256 of `names`, each followed by one newline, in order, as 64 lower-case hex
/// characters.
#[must_use]
pub fn digest(names: &[String]) -> String {
    let mut hash = Sha256::new();
    for name in names {
        hash.update(name.as_bytes());
        hash.update(b"\n");
    }
    hash.finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
#[path = "prerequisites_tests.rs"]
mod tests;
