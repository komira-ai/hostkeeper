//! The pkg's one script, `postinstall` ([`postinstall_text`]): it gives the binary directory of
//! hostkeeper's own update its first binary, so that the daemon's launchd job, which starts
//! `current` of that directory (`hostkeeper_launchd::plist::DAEMON_PROGRAM`), starts the binary
//! the pkg installed before any self-update. It is the counterpart of the Linux unit's
//! `ExecStartPre=` (`packaging/linux/hostkeeper.service`): one line that runs the installed
//! binary's `self-update-seed` (`hostkeeper_daemon::seed`) with the daemon's state directory, so
//! that it changes nothing when `current` starts a binary already, as after a self-update.
//!
//! The installer runs it as root after the payload is in place, with the path of the pkg, the
//! install location, the target volume and the root of the volume as its arguments, which it
//! ignores: the pkg installs at `/` ([`super::INSTALL_LOCATION`]) and the seed writes under the
//! daemon's fixed state directory. A seed that fails (it could not read the binary or write the
//! directory, or a running daemon holds the state directory's lock while `current` starts
//! nothing) fails the install with the seed's message, rather than leave a job that launchd cannot
//! start.
//!
//! The read-back ([`scripts_problems`], [`package_info_scripts_problems`]) holds the built pkg to
//! exactly that script: `PackageInfo` names `postinstall` and no `preinstall`, and the `Scripts`
//! archive holds `postinstall` alone, executable, with exactly these bytes.

use hostkeeper_launchd::plist::{DAEMON_STATE_DIR, INSTALLED_BINARY};

use super::{CpioEntry, TYPE_BITS, TYPE_FILE};

/// The name of the script, in the scripts directory and in the `Scripts` archive.
pub(super) const POSTINSTALL: &str = "postinstall";

/// The mode of the script: the installer runs it.
pub(super) const SCRIPT_MODE: u32 = 0o755;

/// The element `pkgbuild` writes in `PackageInfo` for a `postinstall` in the scripts directory.
const POSTINSTALL_ELEMENT: &str = "<postinstall file=\"./postinstall\"";

/// The text of `postinstall`.
pub(super) fn postinstall_text() -> String {
    format!(
        "#!/bin/sh\n\
         # The hostkeeper pkg's postinstall, written by `cargo xtask release macos-pkg`. The\n\
         # daemon's launchd job starts current of the binary directory of hostkeeper's own update;\n\
         # when current starts nothing, this puts the binary the pkg installed there and points\n\
         # current at it. When current starts a binary, as after a self-update, it changes nothing.\n\
         exec {INSTALLED_BINARY} self-update-seed --state-dir {DAEMON_STATE_DIR} --binary \
         {INSTALLED_BINARY}\n"
    )
}

/// What is wrong with the scripts `PackageInfo` names in `text`, one line each: it names the
/// `postinstall` and no `preinstall`.
pub(super) fn package_info_scripts_problems(text: &str, problems: &mut Vec<String>) {
    if text.contains("<preinstall") {
        problems.push(
            "PackageInfo names a preinstall script: the pkg's one script is its postinstall"
                .to_owned(),
        );
    }
    if !text.contains(POSTINSTALL_ELEMENT) {
        problems.push(format!(
            "PackageInfo names no postinstall script ({POSTINSTALL_ELEMENT}/>): nothing would \
             give the daemon's job a binary to start"
        ));
    }
}

/// What is wrong with the entries of the `Scripts` archive, one line each: besides its top
/// directory (`.`), it holds exactly `./postinstall`, a regular file with an execute bit and the
/// bytes of [`postinstall_text`]. The owner is not checked: the installer runs the script as root
/// whoever owns it in the archive, and installs nothing from it.
pub(super) fn scripts_problems(entries: &[CpioEntry], problems: &mut Vec<String>) {
    let want = format!("./{POSTINSTALL}");
    let mut found = 0;
    for entry in entries.iter().filter(|e| e.name != ".") {
        if entry.name != want {
            problems.push(format!(
                "Scripts holds {:?}: the pkg's one script is {want}",
                entry.name
            ));
            continue;
        }
        found += 1;
        if entry.mode & TYPE_BITS != TYPE_FILE {
            problems.push(format!(
                "Scripts: {want} is of type {:o}, not a regular file",
                entry.mode & TYPE_BITS
            ));
        } else if entry.mode & 0o111 == 0 {
            problems.push(format!(
                "Scripts: {want} has mode {:o}, which the installer cannot run",
                entry.mode & !TYPE_BITS
            ));
        }
        if entry.data != postinstall_text().as_bytes() {
            problems.push(format!(
                "Scripts: {want} is not the postinstall release macos-pkg writes"
            ));
        }
    }
    if found != 1 {
        problems.push(format!("Scripts holds {want} {found} times, not once"));
    }
}
