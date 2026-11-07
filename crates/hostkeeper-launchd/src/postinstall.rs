//! The `postinstall` script of the macOS installer pkg (docs/design/hostkeeper-mdm.md, section
//! 4). The installer runs it as root once the payload is in place, with the path of the pkg, the
//! install location, the target volume and the root of the volume as its arguments, which the
//! script ignores. It runs three commands, in order, and each one that fails ends the script with
//! its own exit status and the message it wrote on standard error, which fails the install:
//!
//! 1. the installed binary's `self-update-seed`. The daemon's launchd job starts `current` of the
//!    binary directory of hostkeeper's own update ([`crate::plist::DAEMON_PROGRAM`]), and launchd
//!    has no step before a start (the Linux unit's `ExecStartPre=`), so the pkg gives that
//!    directory its first binary at install: the seed points `current` at the installed binary
//!    when `current` starts nothing and changes nothing otherwise.
//! 2. the installed binary's `mac-config`, which writes the daemon config from the managed
//!    preferences the MDM installed, keeps a config that is there already, and fails when there
//!    is neither. It exits 0 only when the config file exists, so the job below is loaded only
//!    then.
//! 3. `launchctl bootstrap system <plist>`, which loads the daemon's job (its `RunAtLoad` starts
//!    it), after `launchctl bootout system/<label>` boots out any copy loaded before. The
//!    boot-out's status and standard error are not judged: it fails when no copy is loaded, as
//!    on a first install, and a copy it could not boot out makes the bootstrap fail, with
//!    `launchctl`'s message, which fails the install.
//!
//! [`Postinstall::pkg`] is the pkg's: the paths the payload installs and the daemon reads. A
//! test gives other paths ([`Postinstall`]'s fields), so the script can run outside `/`.

use std::path::{Path, PathBuf};

use crate::label::Label;
use crate::launchctl::LAUNCHCTL;
use crate::platform::Platform;
use crate::plist::{DAEMON_STATE_DIR, INSTALLED_BINARY};

/// The daemon config the daemon reads by default (`hostkeeper_daemon::cli::DEFAULT_CONFIG`), the
/// file `mac-config` writes or keeps.
pub const DAEMON_CONFIG: &str = "/etc/hostkeeper/hostkeeper.toml";

/// The managed preferences of the domain `dev.hostkeeper.config`, as macOS writes them from the
/// config profile's `com.apple.ManagedClient.preferences` payload: the file `mac-config` reads.
/// The path is Apple's convention for managed preferences, still to verify on a device
/// (docs/design/hostkeeper-mdm.md, section 4), so `mac-config` takes it as a flag whose default
/// this is.
pub const MANAGED_CONFIG: &str = "/Library/Managed Preferences/dev.hostkeeper.config.plist";

/// The paths the `postinstall` names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Postinstall {
    /// The binary the pkg installed, which runs the first two commands and is the one the seed
    /// puts in.
    pub binary: PathBuf,
    /// The daemon's state directory, the seed's `--state-dir`.
    pub state_dir: PathBuf,
    /// The managed preferences file, `mac-config`'s `--managed`.
    pub managed_config: PathBuf,
    /// The daemon config, `mac-config`'s `--config`.
    pub config: PathBuf,
    /// The `launchctl` program.
    pub launchctl: PathBuf,
    /// The daemon's launchd plist the pkg installed.
    pub plist: PathBuf,
}

impl Postinstall {
    /// The pkg's: [`INSTALLED_BINARY`], [`DAEMON_STATE_DIR`], [`MANAGED_CONFIG`],
    /// [`DAEMON_CONFIG`], [`LAUNCHCTL`], and the daemon's plist in the Mac's unit directory
    /// (`/Library/LaunchDaemons/dev.hostkeeper.hostkeeper.plist`).
    pub fn pkg() -> Postinstall {
        let mac = Platform::macos();
        let plist = mac
            .unit_dir
            .join(format!("{}{}", Label::daemon(), mac.unit_suffix));
        Postinstall {
            binary: PathBuf::from(INSTALLED_BINARY),
            state_dir: PathBuf::from(DAEMON_STATE_DIR),
            managed_config: PathBuf::from(MANAGED_CONFIG),
            config: PathBuf::from(DAEMON_CONFIG),
            launchctl: PathBuf::from(LAUNCHCTL),
            plist,
        }
    }

    /// The text of the script. Each path is quoted for the shell, so any UTF-8 path is passed as
    /// one word, unchanged.
    pub fn script(&self) -> String {
        let binary = quoted(&self.binary);
        let state_dir = quoted(&self.state_dir);
        let managed = quoted(&self.managed_config);
        let config = quoted(&self.config);
        let launchctl = quoted(&self.launchctl);
        let plist = quoted(&self.plist);
        let job = quoted(Path::new(&format!("system/{}", Label::daemon())));
        format!(
            "#!/bin/sh\n\
             # The hostkeeper pkg's postinstall: three commands, in order. One that fails ends the\n\
             # script with its status and its message, which fails the install.\n\
             # 1. When current of the binary directory of hostkeeper's own update starts nothing,\n\
             #    put the binary the pkg installed there and point current at it; when current\n\
             #    starts a binary, as after a self-update, change nothing.\n\
             # 2. Write the daemon config from the managed preferences the MDM installed. A config\n\
             #    that is there is kept; with neither, fail.\n\
             # 3. Load the daemon's launchd job, once any copy loaded before is booted out (which\n\
             #    fails when none is loaded).\n\
             {binary} self-update-seed --state-dir {state_dir} --binary {binary} || exit\n\
             {binary} mac-config --managed {managed} --config {config} || exit\n\
             {launchctl} bootout {job} 2>/dev/null\n\
             exec {launchctl} bootstrap system {plist}\n"
        )
    }
}

/// `path` as one word of a POSIX shell: in single quotes, each `'` in it written `'\''`. Bytes
/// that are not UTF-8 are shown as U+FFFD.
fn quoted(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', r"'\''"))
}
