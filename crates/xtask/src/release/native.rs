//! The native packages of the services hostkeeper deploys (docs/design/packages-native.md
//! sections 10.2 and 10.8, item A20): `hello` and its failing build as a `.deb` and a `.pkg`,
//! and `mdm` as a `.deb`. What every service's native package holds, and what this module's
//! builders and checks hold them to:
//!
//! - **The hook at its fixed path** ([`HOOK`]): `/usr/lib/<package>/hostkeeper-hook` in a `.deb`,
//!   `/Library/Application Support/<service>/hostkeeper-hook` in a `.pkg`, a regular file of mode
//!   0755. It is the two-line wrapper of [`hook_wrapper`]: it runs the service's program, which
//!   sits beside it in the same tree, with `update-hook` and every argument it was given. The
//!   daemon runs it from the sealed tree (`<version dir>/tree/<path> <step> ...`), so the program
//!   it runs is the one of the version being installed, before that version is installed.
//! - **No maintainer script**: a `.deb`'s control archive holds `control` (and may hold
//!   `md5sums` and `conffiles`, which this builder does not write), and a `.pkg`'s component has
//!   no `Scripts`.
//! - **Files only at the service's paths** ([`deb_path_problem`]): in a `.deb`, under
//!   `/usr/bin/`, `/usr/lib/<package>/`, `/usr/share/<package>/`, `/usr/share/doc/<package>/`
//!   and `/etc/<service>/`, and its units in `/usr/lib/systemd/system/` as plain files named
//!   for the service ([`unit_named_for`]), whether or not they are run units; in a `.pkg`, under
//!   `/Library/Application Support/<service>/` and one plist per job in
//!   `/Library/LaunchDaemons/`.
//! - **Units installed disabled**: nothing under `/etc/systemd/` and no script, so nothing but
//!   hostkeeper enables or starts a unit; a `.pkg`'s plists carry `Disabled` true.
//!
//! [`deb`] is `release deb`, the `.deb` of hello or mdm, written here byte for byte (an `ar` of
//! `debian-binary`, `control.tar.gz` and `data.tar.gz`, every time the commit's); [`pkg`] is the
//! service half of `release macos-pkg` and `release check-macos-pkg`; [`hello`] and [`mdm`] say
//! what each service's packages hold. `release check-linux-pkg --service` reads a built `.deb`
//! back against the rules above (`package::linux_check::service`).

use std::path::PathBuf;

use crate::release::Error;
use crate::release::contract::Service;

pub(crate) mod deb;
pub(crate) mod hello;
pub(crate) mod mdm;
pub(crate) mod pkg;

/// The file name of the hook in every native package.
pub(crate) const HOOK: &str = "hostkeeper-hook";

/// The mode of the hook and of every program of a native package.
pub(crate) const PROGRAM_MODE: u32 = 0o755;

/// The mode of every other file: a unit, a plist.
pub(crate) const FILE_MODE: u32 = 0o644;

/// The mode of every directory.
pub(crate) const DIR_MODE: u32 = 0o755;

/// Where a `.deb` installs its units.
pub(crate) const SYSTEMD_UNITS: &str = "/usr/lib/systemd/system";

/// The kinds of unit a service's package may install: the kinds the daemon runs.
pub(crate) const UNIT_KINDS: [&str; 3] = ["service", "socket", "timer"];

/// The text of the hook of a package whose program is `program`, a path relative to the hook's
/// directory: it runs that program with `update-hook` and every argument it was given. `$0` is
/// the path the daemon ran the hook by, `<version dir>/tree/<path of the hook>`, so `${0%/*}` is
/// the hook's directory of that same tree.
pub(crate) fn hook_wrapper(program: &str) -> String {
    format!("#!/bin/sh\nexec \"${{0%/*}}/{program}\" update-hook \"$@\"\n")
}

/// Whether `name` is a unit file a package of `service` may install in
/// `/usr/lib/systemd/system/`: `<service>.<kind>`, `<service>-<more>.<kind>` or
/// `<service>@<more>.<kind>`, of a kind of [`UNIT_KINDS`], with no `/`.
pub(crate) fn unit_named_for(service: &str, name: &str) -> bool {
    let Some((stem, kind)) = name.rsplit_once('.') else {
        return false;
    };
    if !UNIT_KINDS.contains(&kind) || name.contains('/') {
        return false;
    }
    match stem.strip_prefix(service) {
        Some("") => true,
        Some(rest) => {
            let more = rest.get(1..).unwrap_or_default();
            (rest.starts_with('-') | rest.starts_with('@')) & !more.is_empty()
        }
        None => false,
    }
}

/// What an entry of a `.deb`'s data archive is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EntryKind {
    Dir,
    File,
    Link,
    /// A device, a fifo, or anything else a package must not install.
    Other,
}

/// The directories of the host above the service's own: a `.deb` may list them as directories,
/// and nothing else at them.
const ABOVE: [&str; 9] = [
    "/",
    "/etc",
    "/usr",
    "/usr/bin",
    "/usr/lib",
    "/usr/lib/systemd",
    "/usr/share",
    "/usr/share/doc",
    SYSTEMD_UNITS,
];

/// Why a `.deb` of `service` must not install the entry at the absolute path `path` of kind
/// `kind`, if it must not (docs/design/packages-native.md section 10.2): an entry is allowed
/// below `/usr/lib/<service>/`, `/usr/share/<service>/`, `/usr/share/doc/<service>/` and
/// `/etc/<service>/` (and those directories themselves), a file or a link directly in
/// `/usr/bin/`, a plain unit file named for the service directly in `/usr/lib/systemd/system/`,
/// and the directories above those as directories; a device or a fifo nowhere.
pub(crate) fn deb_path_problem(service: &str, path: &str, kind: EntryKind) -> Option<String> {
    if kind == EntryKind::Other {
        return Some("neither a directory, a regular file nor a link".to_owned());
    }
    let path = path.trim_end_matches('/');
    let path = if path.is_empty() { "/" } else { path };
    if path.split('/').any(|c| c == "..") {
        return Some("has a .. component".to_owned());
    }
    let own = [
        format!("/usr/lib/{service}"),
        format!("/usr/share/{service}"),
        format!("/usr/share/doc/{service}"),
        format!("/etc/{service}"),
    ];
    if own
        .iter()
        .any(|root| path == root || path.starts_with(&format!("{root}/")))
    {
        return None;
    }
    if ABOVE.contains(&path) {
        return match kind {
            EntryKind::Dir => None,
            _ => Some("a directory of the host, which the package may list only as one".to_owned()),
        };
    }
    let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
    match dir {
        "/usr/bin" if kind != EntryKind::Dir => None,
        SYSTEMD_UNITS if kind == EntryKind::File && unit_named_for(service, name) => None,
        SYSTEMD_UNITS => Some(format!(
            "in {SYSTEMD_UNITS}/ a package of {service} installs only plain unit files named \
             {service}.<kind>, {service}-*.<kind> or {service}@*.<kind> (kinds service, socket, \
             timer): no drop-in, link or unit of another name"
        )),
        _ => Some(format!(
            "outside the paths of a package of {service}: /usr/bin/, /usr/lib/{service}/, \
             /usr/share/{service}/, /usr/share/doc/{service}/, /etc/{service}/ and its units in \
             {SYSTEMD_UNITS}/"
        )),
    }
}

/// What `macos-pkg` and `check-macos-pkg` build or check for `service`: `Some` with `gh` and the
/// trusted root for hostkeeper's pkg (both required, `--fail-ready` refused), `None` for hello's
/// (both refused); mdm has no pkg.
pub(crate) fn macos_inputs(
    service: Service,
    fail_ready: bool,
    gh: Option<PathBuf>,
    trusted_root: Option<PathBuf>,
) -> Result<Option<(PathBuf, PathBuf)>, Error> {
    let refuse = |why: &str| Err(Error::Failed(why.to_owned()));
    match (service, gh, trusted_root) {
        (Service::Mdm, ..) => refuse(MDM_NO_PKG),
        (Service::Hello, None, None) => Ok(None),
        (Service::Hello, ..) => refuse(HELLO_PKG_INPUTS),
        (Service::Hostkeeper, ..) if fail_ready => {
            refuse(crate::release::package::FAIL_READY_ONLY_HELLO)
        }
        (Service::Hostkeeper, Some(gh), Some(root)) => Ok(Some((gh, root))),
        (Service::Hostkeeper, ..) => refuse(HOSTKEEPER_PKG_INPUTS),
    }
}

/// Why mdm has no pkg.
pub(crate) const MDM_NO_PKG: &str =
    "mdm has no pkg: it is built for linux-x86_64 only, as a .deb (release deb --service mdm)";

/// Why hello's pkg takes no `--gh` or `--trusted-root`.
pub(crate) const HELLO_PKG_INPUTS: &str =
    "hello's pkg takes no --gh or --trusted-root: only hostkeeper's pkg installs them";

/// Why hostkeeper's pkg needs `--gh` and `--trusted-root`.
pub(crate) const HOSTKEEPER_PKG_INPUTS: &str = "hostkeeper's pkg installs the pinned gh and the Sigstore trusted root: pass --gh and --trusted-root";

#[cfg(test)]
mod tests;
