//! The Mach-O programs this repository builds and ships for macOS, each with the stable code-signing
//! identifier the release signs it under, and the designated requirement that signature has.
//!
//! The release signs each of them with the Developer ID Application identity of the team that
//! signs the installer pkgs ([`APPLICATION_IDENTITY`]), under its identifier, with the hardened
//! runtime and a trusted timestamp (`artifacts.yml`, job `sign-code`). A Developer ID signature's
//! designated requirement names only the identifier and the team, never a hash of the program
//! ([`MacProgram::requirement`]), so it is the same for every release: the privacy profile that
//! grants hostkeeper Full Disk Access (`packaging/macos-mdm/`) names hostkeeper by it, and
//! `release check-code-signature` refuses a built program whose requirement is any other.
//!
//! `gh`, which the installer pkg also carries, is not in the table: it is GitHub's program,
//! shipped with GitHub's own signature, byte for byte as its pinned SHA-256 says.

/// The Team ID of the Developer ID identities of the release, the installer's and the
/// application's.
pub(crate) const TEAM_ID: &str = "U7RTX4XD3X";

/// The Developer ID Application identity the release signs the programs with.
pub(crate) const APPLICATION_IDENTITY: &str =
    "Developer ID Application: Komira AI, Inc. (U7RTX4XD3X)";

/// One program of the table: the name of its binary in the build and its signing identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MacProgram {
    /// The binary's name, as cargo builds it (`target/release/<name>`).
    pub(crate) name: &'static str,
    /// Its code-signing identifier, the same in every release.
    pub(crate) identifier: &'static str,
}

/// Every Mach-O program the release builds for macOS.
pub(crate) const MAC_PROGRAMS: [MacProgram; 4] = [
    MacProgram {
        name: "hostkeeper",
        identifier: "dev.hostkeeper.hostkeeper",
    },
    MacProgram {
        name: "hk",
        identifier: "dev.hostkeeper.hk",
    },
    MacProgram {
        name: "hello",
        identifier: "dev.hostkeeper.hello",
    },
    MacProgram {
        name: "hostkeeper-bootstrap",
        identifier: "dev.hostkeeper.bootstrap",
    },
];

impl MacProgram {
    /// The program of the table whose binary is called `name`, if any.
    pub(crate) fn named(name: &str) -> Option<MacProgram> {
        MAC_PROGRAMS.into_iter().find(|p| p.name == name)
    }

    /// The designated requirement of the program signed by the Developer ID Application identity
    /// of [`TEAM_ID`], as `codesign --display --requirements -` prints it after `designated => `:
    /// the identifier, Apple's anchor, the Developer ID intermediate's and leaf's marker
    /// extensions, and the team as the leaf's organisational unit.
    pub(crate) fn requirement(&self) -> String {
        format!(
            "identifier \"{}\" and anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] /* exists */ and certificate leaf[field.1.2.840.113635.100.6.1.13] /* exists */ and certificate leaf[subject.OU] = {TEAM_ID}",
            self.identifier
        )
    }
}

#[cfg(test)]
mod drift_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod profile_tests;
