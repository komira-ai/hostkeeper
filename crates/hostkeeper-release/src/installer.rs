//! The installers a manifest's rules know (packages.md sections 4.3 and 5.2, as
//! packages-repositories.md section 8 lists them), what each requires of a manifest and refuses
//! in it, and which are backends of the repository installer.
//!
//! The coordinator does not need this list: it picks an entry by the equality of installer
//! names and takes any name of the grammar ([`crate::grammar::is_installer_name`]), since no
//! daemon lists a name it does not know. The daemon applies the rules of the entry's installer.

use std::fmt;
use std::str::FromStr;

/// An installer this crate has the manifest rules of. Ordered by spelling.
///
/// The withdrawn installers `deb`, `rpm`, `arch` and `apk` (packages-repositories.md section 0)
/// are not here: no daemon may list them. A set that names them is still accepted by the
/// coordinator, as one that names any unknown installer is, and no host is ever picked for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Installer {
    // Declared in the order of the spellings, which the derived order follows.
    /// `apt`: the repository installer's backend for apt, which installs a package from an apt
    /// repository the host's configuration names (packages-repositories.md section 4).
    Apt,
    /// `hostkeeper-archive`: hostkeeper's own `tar.zst`, its manifest inside the attested bytes.
    HostkeeperArchive,
    /// `macos-pkg`: a signed macOS flat package, fetched as an artifact
    /// (packages-repositories.md section 4.7).
    MacosPkg,
    /// `oci-image`: an OCI image archive holding one image.
    OciImage,
    /// `tarball`: a plain tar, or one compressed with gzip, xz or zstd.
    Tarball,
}

/// A name that is none of the installers this crate knows.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0:?} is not an installer this daemon knows")]
pub struct UnknownInstaller(pub String);

impl Installer {
    /// Every installer, in the order of their spellings.
    pub const ALL: [Installer; 5] = [
        Installer::Apt,
        Installer::HostkeeperArchive,
        Installer::MacosPkg,
        Installer::OciImage,
        Installer::Tarball,
    ];

    /// The name the entry and the daemon's list spell it with.
    pub const fn as_str(self) -> &'static str {
        match self {
            Installer::Apt => "apt",
            Installer::HostkeeperArchive => "hostkeeper-archive",
            Installer::MacosPkg => "macos-pkg",
            Installer::OciImage => "oci-image",
            Installer::Tarball => "tarball",
        }
    }

    /// Whether the format's own tool knows the package by a name the manifest must give
    /// (`package`): required for these, refused for the others.
    pub const fn names_a_package(self) -> bool {
        matches!(self, Installer::Apt | Installer::MacosPkg)
    }

    /// Whether it is a backend of the repository installer, whose entries are repository
    /// entries (`Target.repository` set); the others install artifact entries. The coordinator
    /// never asks: it tells an entry's kind by its `repository` alone
    /// ([`crate::target::is_repository_entry`]).
    pub const fn is_repository(self) -> bool {
        matches!(self, Installer::Apt)
    }
}

impl FromStr for Installer {
    type Err = UnknownInstaller;

    /// The installer of exactly one of the spellings; no other case or spelling.
    fn from_str(text: &str) -> Result<Installer, UnknownInstaller> {
        Installer::ALL
            .into_iter()
            .find(|installer| installer.as_str() == text)
            .ok_or_else(|| UnknownInstaller(text.to_owned()))
    }
}

impl fmt::Display for Installer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
