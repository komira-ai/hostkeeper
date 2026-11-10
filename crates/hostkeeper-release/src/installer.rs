//! The installers a manifest's rules know (packages-native.md section 10.1: `apt` and
//! `macos-pkg`), what each requires of a manifest and refuses in it, and which are backends of
//! the repository installer.
//!
//! The coordinator picks an entry by the equality of installer names and takes any name of the
//! grammar ([`crate::grammar::is_installer_name`]), since no daemon lists a name it does not know.
//! It asks this list one thing: whether an entry's installer is native ([`Installer::is_native`],
//! packages-native.md section 10), for the hook of a native entry, a `macos-pkg` entry's
//! attestation and hostkeeper's own list. The daemon applies the rules of the entry's installer.

use std::fmt;
use std::str::FromStr;

/// An installer this crate has the manifest rules of. Ordered by spelling.
///
/// The withdrawn installers are not here: `deb`, `rpm`, `arch` and `apk` (packages-repositories.md
/// section 0), and `hostkeeper-archive` ([`ARCHIVE_INSTALLER`]), `tarball` and `oci-image`
/// (packages-native.md section 10.1). No daemon may list them. A set that names them is still
/// accepted by the coordinator, as one that names any unknown installer is, and no host is ever
/// picked for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Installer {
    // Declared in the order of the spellings, which the derived order follows.
    /// `apt`: the repository installer's backend for apt, which installs a package from an apt
    /// repository the host's configuration names (packages-repositories.md section 4).
    Apt,
    /// `macos-pkg`: a signed macOS flat package, fetched as an artifact
    /// (packages-repositories.md section 4.7).
    MacosPkg,
}

/// The name earlier releases and daemons used for hostkeeper's own archive installer. It is never
/// an installer of this crate: an entry that names it is accepted as any unknown installer name
/// is, and never picked. The coordinator and the daemon read it only on old data, to know an
/// entry or a record that names it.
pub const ARCHIVE_INSTALLER: &str = "hostkeeper-archive";

/// A name that is none of the installers this crate knows.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0:?} is not an installer this daemon knows")]
pub struct UnknownInstaller(pub String);

impl Installer {
    /// Every installer, in the order of their spellings.
    pub const ALL: [Installer; 2] = [Installer::Apt, Installer::MacosPkg];

    /// The name the entry and the daemon's list spell it with.
    pub const fn as_str(self) -> &'static str {
        match self {
            Installer::Apt => "apt",
            Installer::MacosPkg => "macos-pkg",
        }
    }

    /// Whether it installs the platform's own package (packages-native.md section 10.1): a
    /// `.deb` through `apt`, a signed `.pkg` through `macos-pkg`. A native package's hook is its
    /// program at a fixed path, so its entry names no `hook.command`; and the service `hostkeeper` is set with one native
    /// entry per list.
    pub const fn is_native(self) -> bool {
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
