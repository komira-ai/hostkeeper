//! The `postinstall` script of the macOS installer pkg (docs/design/two-programs.md, section
//! 4.1). The installer runs it as root once the payload is in place, with the path of the pkg,
//! the install location, the target volume and the root of the volume as its arguments, which
//! the script ignores. It runs two commands: `launchctl bootout system/<label>`, which boots out
//! any copy of the daemon's job loaded before, and then `launchctl bootstrap system <plist>`,
//! which loads the job the pkg installed (its `RunAtLoad` starts it). The boot-out's status and
//! standard error are not judged: it fails when no copy is loaded, as on a first install, and a
//! copy it could not boot out makes the bootstrap fail, with `launchctl`'s message, which fails
//! the install. The bootstrap's status is the script's.
//!
//! The script writes nothing else. The daemon's start does what a postinstall did before
//! (`hostkeeper_daemon::packaged`, `hostkeeper_daemon::mac_config::at_start`): the job's program,
//! the binary the pkg installed ([`crate::plist::INSTALLED_BINARY`]), gives the binary directory
//! of hostkeeper's own update its first binary and execs `current`, and the daemon writes its
//! config from the managed preferences ([`MANAGED_CONFIG`]) when [`DAEMON_CONFIG`] is absent. A
//! pkg installed before the managed preferences are there installs, and the daemon refuses each
//! start launchd makes, naming the missing file in its standard error.
//!
//! [`Postinstall::pkg`] is the pkg's: the `launchctl` the Mac has and the plist the payload
//! installs. A test gives other paths ([`Postinstall`]'s fields).

use std::path::{Path, PathBuf};

use crate::label::Label;
use crate::launchctl::LAUNCHCTL;
use crate::platform::Platform;

/// The daemon config the daemon reads by default (`hostkeeper_daemon::cli::DEFAULT_CONFIG`), the
/// file a Mac's daemon writes at its start from [`MANAGED_CONFIG`] when it is absent.
pub const DAEMON_CONFIG: &str = "/etc/hostkeeper/hostkeeper.toml";

/// The managed preferences of the domain `dev.hostkeeper.config`, as macOS writes them from the
/// config profile's `com.apple.ManagedClient.preferences` payload: the file a Mac's daemon builds
/// [`DAEMON_CONFIG`] from. The path is Apple's convention for managed preferences
/// (docs/design/hostkeeper-mdm.md, section 4), so `mac-config` takes it as a flag whose default
/// this is.
pub const MANAGED_CONFIG: &str = "/Library/Managed Preferences/dev.hostkeeper.config.plist";

/// The paths the `postinstall` names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Postinstall {
    /// The `launchctl` program.
    pub launchctl: PathBuf,
    /// The daemon's launchd plist the pkg installed.
    pub plist: PathBuf,
}

impl Postinstall {
    /// The pkg's: [`LAUNCHCTL`], and the daemon's plist in the Mac's unit directory
    /// (`/Library/LaunchDaemons/dev.hostkeeper.hostkeeper.plist`).
    pub fn pkg() -> Postinstall {
        let mac = Platform::macos();
        let plist = mac
            .unit_dir
            .join(format!("{}{}", Label::daemon(), mac.unit_suffix));
        Postinstall {
            launchctl: PathBuf::from(LAUNCHCTL),
            plist,
        }
    }

    /// The text of the script. Each path is quoted for the shell, so any UTF-8 path is passed as
    /// one word, unchanged.
    pub fn script(&self) -> String {
        let launchctl = quoted(&self.launchctl);
        let plist = quoted(&self.plist);
        let job = quoted(Path::new(&format!("system/{}", Label::daemon())));
        format!(
            "#!/bin/sh\n\
             # The hostkeeper pkg's postinstall: load the daemon's launchd job, once any copy\n\
             # loaded before is booted out (which fails when none is loaded). The job's program,\n\
             # the binary the pkg installed, puts itself in the binary directory of hostkeeper's\n\
             # own update when current starts nothing and runs current, at each start.\n\
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
