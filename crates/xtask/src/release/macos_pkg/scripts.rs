//! The pkg's one script, `postinstall` ([`postinstall_text`], the text
//! `hostkeeper_launchd::postinstall::Postinstall::pkg` writes for the pkg's paths). It asks, then
//! loads:
//!
//! 1. `launchctl print` of the daemon's job, its output discarded: when the job is loaded the
//!    script exits 0 and touches nothing. An `installer -pkg` the daemon itself runs (its
//!    self-update through macos-pkg, docs/design/packages-native.md, section 10.4) must not boot
//!    the running daemon out in the middle of its update; the plist's new text takes effect at
//!    the next boot. The script never runs `launchctl bootout`.
//! 2. `launchctl bootstrap` of the daemon's plist, when the job is not loaded (a first install, or
//!    a host that booted it out), which loads the job (its `RunAtLoad` starts it); its status is
//!    the script's, so a failure fails the install with its message.
//!
//! The script runs the installed binary at no point. The job's program is that binary
//! (`hostkeeper_launchd::plist::INSTALLED_BINARY`), and `hostkeeper daemon` started from it seeds
//! the binary directory of hostkeeper's own update and execs `current` of it, and writes the
//! config from the managed preferences (`hostkeeper_daemon::packaged`,
//! `hostkeeper_daemon::mac_config::at_start`), at each start.
//!
//! The installer runs it as root after the payload is in place, with the path of the pkg, the
//! install location, the target volume and the root of the volume as its arguments, which it
//! ignores: the pkg installs at `/` ([`super::INSTALL_LOCATION`]) and the commands name fixed
//! paths.
//!
//! The read-back ([`scripts_problems`], [`package_info_scripts_problems`]) holds the built pkg to
//! exactly that script: `PackageInfo` names `postinstall` and no `preinstall`, and the `Scripts`
//! directory `pkgutil --expand` makes of the pkg's scripts archive holds `postinstall` alone, a
//! regular file of mode 0755 with exactly these bytes.

use std::os::unix::fs::MetadataExt;
use std::path::Path;

use hostkeeper_launchd::postinstall::Postinstall;

use super::{Error, TYPE_BITS};

/// The name of the script, in the scripts directory and in the expanded pkg's `Scripts`.
pub(super) const POSTINSTALL: &str = "postinstall";

/// The mode of the script: the installer runs it.
pub(super) const SCRIPT_MODE: u32 = 0o755;

/// The element `pkgbuild` writes in `PackageInfo` for a `postinstall` in the scripts directory.
const POSTINSTALL_ELEMENT: &str = "<postinstall file=\"./postinstall\"";

/// The text of `postinstall`: the check that the daemon's job is loaded and the load of it when
/// it is not, for the pkg's paths.
pub(super) fn postinstall_text() -> String {
    Postinstall::pkg().script()
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
             load the daemon's job"
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
