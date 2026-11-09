//! hostkeeper's own `.pkg` (packages-native.md section 10.4): the one package the `macos-pkg`
//! installer lets carry a script, and the one held to its own list of paths instead of the payload
//! rule of packages-repositories.md section 4.7. It is the service `hostkeeper` with the
//! identifier of the daemon's launchd label ([`identifier`]); a `.pkg` of any other service, or of
//! this service under another identifier, keeps every check of section 4.7.
//!
//! What it may hold beyond a service's package:
//!
//! - the script `postinstall` alone, in the component's `Scripts` and as the `<postinstall>`
//!   element of its `PackageInfo` ([`POSTINSTALL`]; `shape` holds it to that), where it loads the
//!   daemon's job; a product archive's own `Scripts`, a `preinstall`, `preupgrade` or
//!   `postupgrade`, and every `Distribution` script element stay refused;
//! - the paths of [`list`], each of the kind and the mode it lists, which are the entries
//!   `cargo xtask release macos-pkg` lays out (`crates/xtask/src/release/macos_pkg.rs`, whose
//!   payload a test of the xtask crate compares with this list), and the directories above them;
//! - a plist for the daemon's job that is not `Disabled`: the job is loaded by the script and
//!   runs at load, so `payload::check_jobs` does not require `Disabled` of it.
//!
//! Every entry still has the owner, group and mode bits `payload` requires of every package.

use std::collections::{BTreeMap, BTreeSet};

use hostkeeper_launchd::Label;
use hostkeeper_launchd::plist::{DAEMON_LOG_DIR, INSTALLED_BINARY};

use super::payload::{self, PayloadError, owned, plist_path};
use crate::host::{Kind, Meta};

/// The service whose package is hostkeeper's own.
pub const SERVICE: &str = "hostkeeper";

/// The only script of the package, in the component's `Scripts` and in its `PackageInfo`.
pub const POSTINSTALL: &str = "postinstall";

/// The mode of each directory of the list: what macOS gives the directories above them.
pub const DIR_MODE: u32 = 0o755;

/// Where the package installs the pinned `gh`.
pub const GH_PATH: &str = "/usr/local/lib/hostkeeper/gh";

/// Where the package installs the Sigstore trusted root.
pub const TRUSTED_ROOT_PATH: &str = "/usr/local/lib/hostkeeper/trusted_root.json";

/// Where the package installs the newsyslog entries of the daemon's output files (`/etc` is a
/// link into `/private`, which the payload lists).
pub const NEWSYSLOG_PATH: &str = "/private/etc/newsyslog.d/hostkeeper.conf";

/// The package's identifier: the daemon's launchd label.
pub fn identifier() -> String {
    Label::daemon().as_str().to_owned()
}

/// Whether a package of `service` with the identifier `identifier` is hostkeeper's own.
pub fn is_own(service: &str, identifier: &str) -> bool {
    (service == SERVICE) & (identifier == self::identifier())
}

/// The files of the list with their modes, by logical path: the daemon binary, the daemon's
/// plist, the newsyslog entries, `gh` and the trusted root.
pub fn files() -> Vec<(String, u32)> {
    vec![
        (INSTALLED_BINARY.to_owned(), 0o755),
        (plist_path(&Label::daemon()), 0o644),
        (NEWSYSLOG_PATH.to_owned(), 0o644),
        (GH_PATH.to_owned(), 0o755),
        (TRUSTED_ROOT_PATH.to_owned(), 0o644),
    ]
}

/// The log directory of the daemon's output files, as the payload lists it (under `/private`).
pub fn log_dir() -> String {
    format!("/private{DAEMON_LOG_DIR}")
}

/// Every entry of the package's payload by logical path with its kind and mode: [`files`], the
/// log directory, and each directory above them, all of mode [`DIR_MODE`].
pub fn list() -> BTreeMap<String, (Kind, u32)> {
    let mut list = BTreeMap::new();
    let dirs = std::iter::once(log_dir());
    let paths: Vec<String> = files().into_iter().map(|(path, _)| path).collect();
    for path in paths.iter().cloned().chain(dirs.clone()) {
        let mut parent = path.as_str();
        while let Some((dir, _)) = parent.rsplit_once('/').filter(|(dir, _)| !dir.is_empty()) {
            list.insert(dir.to_owned(), (Kind::Dir, DIR_MODE));
            parent = dir;
        }
    }
    for dir in dirs {
        list.insert(dir, (Kind::Dir, DIR_MODE));
    }
    for (path, mode) in files() {
        list.insert(path, (Kind::File, mode));
    }
    list
}

/// The directories of [`list`].
pub fn dirs() -> BTreeSet<String> {
    list()
        .into_iter()
        .filter(|(_, (kind, _))| *kind == Kind::Dir)
        .map(|(path, _)| path)
        .collect()
}

/// Checks the `Bom`'s entries against the expanded payload's and every entry against the list.
pub fn check(
    bom: &BTreeMap<String, Meta>,
    payload: &BTreeMap<String, Meta>,
) -> Result<(), PayloadError> {
    let list = list();
    payload::check_with(bom, payload, &|path, meta| {
        owned(path, meta)?;
        match list.get(path) {
            None => Err(PayloadError::OutsideOwn(path.into())),
            Some(&(kind, mode)) if (kind != meta.kind) | (mode != meta.mode) => Err(
                PayloadError::OwnEntry(path.into(), meta.kind, meta.mode, kind, mode),
            ),
            Some(_) => Ok(()),
        }
    })
}
