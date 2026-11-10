//! What pxe's `.deb` holds (docs/design/two-programs.md section 6.1): the program `pxe` at
//! `/usr/lib/pxe/pxe` (a program no one types: the unit and the daemon's hook run it) and its
//! unit `/usr/lib/systemd/system/pxe.service`, the committed golden
//! `packaging/pxe/deb/pxe.service`, and nothing else: no maintainer script, no conffile. The
//! `.deb` is built for both Linux platforms from the static musl `pxe` of the release's build.
//!
//! The constants are xtask's own: xtask builds on macOS and the `hostkeeper-pxe` crate is
//! Linux-only, so xtask does not depend on it. A test of that crate parses the golden's
//! `ExecStart` with the program's own command line, so the unit and the program cannot drift.

use super::hello::File;
use super::{FILE_MODE, PROGRAM_MODE, SYSTEMD_UNITS};

/// The service's name: its package, its program and its unit.
pub(crate) const NAME: &str = "pxe";

/// Where the `.deb` installs the program, and the program of the hook command.
pub(crate) const DEB_PROGRAM: &str = "/usr/lib/pxe/pxe";

/// The unit the package installs and the manifest runs.
pub(crate) const UNIT: &str = "pxe.service";

/// The settings file the unit reads and the hook's `settings` param names; the operator's.
pub(crate) const SETTINGS_FILE: &str = "/etc/pxe/pxe.toml";

/// The directory of the boot sets the hook builds, a version directory each and `current`.
pub(crate) const BOOT_DIR: &str = "/var/lib/pxe";

/// The committed unit.
const UNIT_TEXT: &str = include_str!("../../../../../packaging/pxe/deb/pxe.service");

/// The files of pxe's `.deb` with the program `binary`, in path order.
pub(crate) fn deb_files(binary: Vec<u8>) -> Vec<File> {
    let mut files = vec![
        File {
            path: DEB_PROGRAM.to_owned(),
            mode: PROGRAM_MODE,
            data: binary,
        },
        File {
            path: format!("{SYSTEMD_UNITS}/{UNIT}"),
            mode: FILE_MODE,
            data: UNIT_TEXT.as_bytes().to_vec(),
        },
    ];
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

#[cfg(test)]
mod tests;
