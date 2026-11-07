//! The pkg's one script, `postinstall` ([`postinstall_text`], the text
//! `hostkeeper_launchd::postinstall::postinstall` writes for the pkg's paths): it gives the binary directory of
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
//! directory `pkgutil --expand` makes of the pkg's scripts archive holds `postinstall` alone, a
//! regular file of mode 0755 with exactly these bytes.

use std::os::unix::fs::MetadataExt;
use std::path::Path;

use hostkeeper_launchd::plist::{DAEMON_STATE_DIR, INSTALLED_BINARY};
use hostkeeper_launchd::postinstall::postinstall;

use super::{Error, TYPE_BITS};

/// The name of the script, in the scripts directory and in the expanded pkg's `Scripts`.
pub(super) const POSTINSTALL: &str = "postinstall";

/// The mode of the script: the installer runs it.
pub(super) const SCRIPT_MODE: u32 = 0o755;

/// The element `pkgbuild` writes in `PackageInfo` for a `postinstall` in the scripts directory.
const POSTINSTALL_ELEMENT: &str = "<postinstall file=\"./postinstall\"";

/// The text of `postinstall`: the seed of the daemon's state directory from the installed
/// binary.
pub(super) fn postinstall_text() -> String {
    postinstall(Path::new(INSTALLED_BINARY), Path::new(DAEMON_STATE_DIR))
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

/// What is wrong with the `Scripts` directory `dir` that `pkgutil --expand` makes of the pkg's
/// scripts archive, one line each: it holds exactly `postinstall`, a regular file (not a link,
/// which is not followed) of mode 0755 with the bytes of [`postinstall_text`]. The owner is not
/// checked: the installer runs the script as root whoever owns it, and installs nothing from it.
/// `Err` when the directory or the script cannot be read.
pub(super) fn scripts_problems(dir: &Path, problems: &mut Vec<String>) -> Result<(), Error> {
    let what = format!("read {}", dir.display());
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .and_then(|read| read.collect::<Result<Vec<_>, _>>())
        .map_err(Error::io(&what))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let mut found = false;
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name != POSTINSTALL {
            problems.push(format!(
                "Scripts holds {name:?}: the pkg's one script is {POSTINSTALL}"
            ));
            continue;
        }
        found = true;
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path)
            .map_err(Error::io(format!("read {}", path.display())))?;
        if !meta.file_type().is_file() {
            problems.push(format!(
                "Scripts: {POSTINSTALL} is of type {:o}, not a regular file",
                meta.mode() & TYPE_BITS
            ));
            continue;
        }
        let mode = meta.mode() & !TYPE_BITS;
        if mode != SCRIPT_MODE {
            problems.push(format!(
                "Scripts: {POSTINSTALL} has mode {mode:o}, not {SCRIPT_MODE:o}"
            ));
        }
        let bytes = std::fs::read(&path).map_err(Error::io(format!("read {}", path.display())))?;
        if bytes != postinstall_text().as_bytes() {
            problems.push(format!(
                "Scripts: {POSTINSTALL} is not the postinstall release macos-pkg writes"
            ));
        }
    }
    if !found {
        problems.push(format!("Scripts holds no {POSTINSTALL}"));
    }
    Ok(())
}
