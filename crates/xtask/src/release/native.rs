//! The native packages of the services hostkeeper deploys (docs/design/packages-native.md
//! sections 10.2 and 10.8, item A20): `hello` and its failing build as a `.deb` and a `.pkg`,
//! and `mdm` as a `.deb`. What every service's native package holds, and what this module's
//! builders and checks hold them to:
//!
//! - **The program the manifest names**: the entry's `hook.command` (`release targets`) is the
//!   absolute path the package installs the service's program at, then `update-hook`; the daemon
//!   runs it from the sealed tree (`<version dir>/tree/<path> update-hook <step> ...`), so the
//!   program it runs is the one of the version being installed, before that version is installed.
//!   The package holds no wrapper for it.
//! - **No rule about the rest**: hostkeeper installs the package as built, with any maintainer
//!   script, path or unit; what these builders write is their own choice. A `.deb` holds `control`
//!   alone in its control archive, installs under `/usr/lib/<package>/` and
//!   `/usr/lib/systemd/system/`, and a `.pkg`'s component has no `Scripts`, its payload is under
//!   `/Library/Application Support/<service>/` and `/Library/LaunchDaemons/`, and its plists
//!   carry `Disabled` true.
//!
//! [`deb`] is `release deb`, the `.deb` of hello or mdm, written here byte for byte (an `ar` of
//! `debian-binary`, `control.tar.gz` and `data.tar.gz`, every time the commit's); [`pkg`] is the
//! service half of `release macos-pkg` and `release check-macos-pkg`; [`hello`] and [`mdm`] say
//! what each service's packages hold. `release check-linux-pkg --service` reads a built `.deb`
//! back against what this builder writes (`package::linux_check::service`).

use std::path::PathBuf;

use crate::release::Error;
use crate::release::contract::Service;

pub(crate) mod deb;
pub(crate) mod hello;
pub(crate) mod mdm;
pub(crate) mod pkg;

/// The mode of every program of a native package.
pub(crate) const PROGRAM_MODE: u32 = 0o755;

/// The mode of every other file: a unit, a plist.
pub(crate) const FILE_MODE: u32 = 0o644;

/// The mode of every directory.
pub(crate) const DIR_MODE: u32 = 0o755;

/// Where a `.deb` installs its units.
pub(crate) const SYSTEMD_UNITS: &str = "/usr/lib/systemd/system";

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
