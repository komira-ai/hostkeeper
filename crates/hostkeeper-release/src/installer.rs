//! The installers a manifest's rules know (packages.md sections 4.3 and 5.2), and what each
//! requires of a manifest and refuses in it.
//!
//! The coordinator does not need this list: it picks an entry by the equality of installer
//! names and takes any name of the grammar ([`crate::grammar::is_installer_name`]), since no
//! daemon lists a name it does not know. The daemon applies the rules of the entry's installer.

use std::fmt;
use std::str::FromStr;

/// An installer this crate has the manifest rules of. Ordered by spelling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Installer {
    // Declared in the order of the spellings, which the derived order follows.
    /// `apk`: an Alpine package.
    Apk,
    /// `arch`: an Arch package.
    Arch,
    /// `deb`: a Debian package.
    Deb,
    /// `hostkeeper-archive`: hostkeeper's own `tar.zst`, its manifest inside the attested bytes.
    HostkeeperArchive,
    /// `macos-pkg`: a macOS flat package.
    MacosPkg,
    /// `oci-image`: an OCI image archive holding one image.
    OciImage,
    /// `rpm`: an RPM package.
    Rpm,
    /// `tarball`: a plain tar, or one compressed with gzip, xz or zstd.
    Tarball,
}

/// A name that is none of the installers this crate knows.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0:?} is not an installer this daemon knows")]
pub struct UnknownInstaller(pub String);

impl Installer {
    /// Every installer, in the order of their spellings.
    pub const ALL: [Installer; 8] = [
        Installer::Apk,
        Installer::Arch,
        Installer::Deb,
        Installer::HostkeeperArchive,
        Installer::MacosPkg,
        Installer::OciImage,
        Installer::Rpm,
        Installer::Tarball,
    ];

    /// The name the entry and the daemon's list spell it with.
    pub const fn as_str(self) -> &'static str {
        match self {
            Installer::Apk => "apk",
            Installer::Arch => "arch",
            Installer::Deb => "deb",
            Installer::HostkeeperArchive => "hostkeeper-archive",
            Installer::MacosPkg => "macos-pkg",
            Installer::OciImage => "oci-image",
            Installer::Rpm => "rpm",
            Installer::Tarball => "tarball",
        }
    }

    /// Whether the format's own tool knows the package by a name the manifest must give
    /// (`package`): required for these, refused for the others.
    pub const fn names_a_package(self) -> bool {
        matches!(
            self,
            Installer::Apk
                | Installer::Arch
                | Installer::Deb
                | Installer::MacosPkg
                | Installer::Rpm
        )
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
