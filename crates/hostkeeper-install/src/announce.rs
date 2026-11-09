//! Which installers the daemon announces (packages.md section 5.4), decided once at its start
//! from its platform and the host's `os-release`.
//!
//! No installer is announced by every host: [`announce`] gives none, and the daemon adds the
//! native installer of the host's package database when the host may run it.
//!
//! A Linux host's **family** comes from `/etc/os-release`, or `/usr/lib/os-release` when that is
//! missing or cannot be read, as `os-release(5)` orders them: its `ID`, then each token of `ID_LIKE` in order, and
//! the first that names a family wins (`debian`, `ubuntu`: [`Family::Deb`]; `fedora`, `rhel`,
//! `centos`, `amzn`, `suse`, `opensuse`: [`Family::Rpm`]). An unknown `ID`, an empty file, both
//! files missing, and a file with a malformed line (one with no `=`, or a value whose quote is
//! not closed) give no family. A macOS host has no family and never reads `os-release`.
//!
//! The family chooses the one native installer a host may announce, the one of its own package
//! database: the daemon adds `apt` for the `deb` family when the host also has apt's tools, apt
//! 2.2 or later and a repository entry ([`crate::repository::announce`]). `macos-pkg` (A9) on a
//! Mac and a `dnf` backend (A8) for `rpm` are not announced yet. The daemon reports the family it found at its start.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use hostkeeper_release::Installer;

/// The most bytes of an `os-release` file read; a longer file is read as one that cannot be
/// read, and the next file is tried.
pub const MAX_OS_RELEASE_BYTES: u64 = 64 * 1024;

/// The files `os-release(5)` names, in the order they are read, relative to the host's root.
pub const OS_RELEASE_FILES: [&str; 2] = ["etc/os-release", "usr/lib/os-release"];

/// The package database a Linux host's distribution keeps, which decides the one native
/// installer it may announce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    /// Debian, Ubuntu and their derivatives: `dpkg`.
    Deb,
    /// RHEL, Fedora, Amazon Linux, SUSE and theirs: `rpm`.
    Rpm,
}

impl Family {
    /// The family's name: `deb` or `rpm`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Family::Deb => "deb",
            Family::Rpm => "rpm",
        }
    }

    /// The family a token of `ID` or `ID_LIKE` names, if any.
    fn of_token(token: &str) -> Option<Family> {
        match token {
            "debian" | "ubuntu" => Some(Family::Deb),
            "fedora" | "rhel" | "centos" | "amzn" | "suse" | "opensuse" => Some(Family::Rpm),
            _ => None,
        }
    }
}

/// What the daemon announces, decided at its start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Announcement {
    /// The installers, in the order the heartbeat lists them.
    pub installers: Vec<Installer>,
    /// The host's family, `None` on macOS and on a Linux host whose `os-release` names none.
    pub family: Option<Family>,
}

impl Default for Announcement {
    /// What a daemon announces whose host's files were not read: no installer and no family.
    fn default() -> Announcement {
        Announcement {
            installers: Vec::new(),
            family: None,
        }
    }
}

impl Announcement {
    /// The names of the installers, as `HeartbeatRequest.installers` carries them.
    pub fn names(&self) -> Vec<String> {
        self.installers
            .iter()
            .map(|installer| installer.as_str().to_owned())
            .collect()
    }
}

/// The host the daemon decides for: whether it is a Mac, and the directory its files are read
/// under (`/` on a host, a scratch directory in a test).
#[derive(Clone, Copy, Debug)]
pub struct OsHost<'a> {
    /// Whether the host is macOS.
    pub macos: bool,
    /// The root the host's files are read under.
    pub root: &'a Path,
}

/// The announcement of `host`, reading its files with [`read_bounded`].
pub fn announce(host: OsHost<'_>) -> Announcement {
    announce_with(host, &read_bounded)
}

/// The announcement of `host`, reading each file it needs through `read`, which answers the
/// file's text or `None` when it cannot be read.
pub fn announce_with(host: OsHost<'_>, read: &dyn Fn(&Path) -> Option<String>) -> Announcement {
    let family = match host.macos {
        true => None,
        false => os_release(host.root, read).as_deref().and_then(family),
    };
    Announcement {
        installers: Vec::new(),
        family,
    }
}

/// The text of the first `os-release` file under `root` that can be read.
fn os_release(root: &Path, read: &dyn Fn(&Path) -> Option<String>) -> Option<String> {
    OS_RELEASE_FILES
        .iter()
        .map(|file| root.join(file))
        .find_map(|path: PathBuf| read(&path))
}

/// The text of `path` when it is a readable file of at most [`MAX_OS_RELEASE_BYTES`] bytes of
/// UTF-8, following a link (`/etc/os-release` is often one to `/usr/lib/os-release`).
pub fn read_bounded(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_OS_RELEASE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    let fits = bytes.len() as u64 <= MAX_OS_RELEASE_BYTES;
    fits.then(|| String::from_utf8(bytes).ok()).flatten()
}

/// The family `text`, an `os-release` file, names: its `ID`, then each token of `ID_LIKE`, the
/// first that names one. `None` for a file with a malformed line.
pub fn family(text: &str) -> Option<Family> {
    let mut id = None;
    let mut like = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() | line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once('=')?;
        let value = unquote(value)?;
        match key {
            "ID" => id = Some(value),
            "ID_LIKE" => like = Some(value),
            _ => {}
        }
    }
    let like = like.unwrap_or_default();
    id.iter()
        .map(String::as_str)
        .chain(like.split_whitespace())
        .find_map(Family::of_token)
}

/// The value of an assignment, as a shell reads it: unquoted, or inside one pair of double or
/// single quotes, a backslash in double quotes escaping the next character. `None` when a quote
/// is not closed or text follows the closing one.
fn unquote(value: &str) -> Option<String> {
    let mut chars = value.chars();
    let quote = match chars.clone().next() {
        Some(quote @ ('"' | '\'')) => quote,
        _ => return Some(value.to_owned()),
    };
    chars.next();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        match c {
            c if c == quote => return chars.as_str().is_empty().then_some(out),
            '\\' if quote == '"' => out.push(chars.next()?),
            c => out.push(c),
        }
    }
    None
}
