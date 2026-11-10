//! hostkeeper's own `.pkg` (packages-native.md section 10.4): the service `hostkeeper` with the
//! identifier of the daemon's launchd label ([`identifier`]). The installer holds it to no rule a
//! package does not keep; this module names it ([`is_own`]) and lists the entries that
//! `cargo xtask release macos-pkg` lays out (`crates/xtask/src/release/macos_pkg.rs`, whose
//! payload a test of the xtask crate compares with [`list`]): the paths of [`files`], the log
//! directory, and the directories above them, each of the kind and the mode it lists.

use std::collections::{BTreeMap, BTreeSet};

use hostkeeper_launchd::Label;
use hostkeeper_launchd::plist::{DAEMON_LOG_DIR, INSTALLED_BINARY};

use super::payload::plist_path;
use crate::host::Kind;

/// The service whose package is hostkeeper's own.
pub const SERVICE: &str = "hostkeeper";

/// The script of the package, in the component's `Scripts` and in its `PackageInfo`, which loads the daemon's job.
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
