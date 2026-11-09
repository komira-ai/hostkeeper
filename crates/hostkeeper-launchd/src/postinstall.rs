//! The `postinstall` script of the macOS installer pkg (docs/design/two-programs.md, section
//! 4.1). The installer runs it as root once the payload is in place, with the path of the pkg,
//! the install location, the target volume and the root of the volume as its arguments, which
//! the script ignores. It asks launchd whether the daemon's job is loaded (`launchctl print
//! system/<label>`, its output discarded: it exits 0 when the job is loaded and non-zero, 113,
//! when it is not). A loaded job is left alone and the script exits 0: an `installer -pkg` the
//! daemon itself runs (its self-update through macos-pkg, docs/design/packages-native.md, section
//! 10.4) must not boot the running daemon out in the middle of its update, so the script never
//! runs `launchctl bootout`, and the plist's new text takes effect at the next boot. When the job
//! is not loaded (a first install, or a host that booted it out) the script runs `launchctl
//! bootstrap system <plist>`, which loads the job the pkg installed (its `RunAtLoad` starts it);
//! that status is the script's, so a failing bootstrap fails the install with `launchctl`'s
//! message.
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
/// (docs/design/hostkeeper-mdm.md, section 4), which the daemon's start reads
/// (`hostkeeper_daemon::mac_config::at_start`).
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
             # The hostkeeper pkg's postinstall: load the daemon's launchd job when launchd does\n\
             # not have it loaded, and leave it alone when it does. An installer the daemon\n\
             # itself runs (its self-update) must not boot the running daemon out, so the job is\n\
             # never booted out here; its new plist takes effect at the next boot. A first\n\
             # install, or a host where the job is not loaded, gets it loaded.\n\
             {launchctl} print {job} >/dev/null 2>&1 && exit 0\n\
             exec {launchctl} bootstrap system {plist}\n"
        )
    }
}

/// `path` as one word of a POSIX shell: in single quotes, each `'` in it written `'\''`. Bytes
/// that are not UTF-8 are shown as U+FFFD.
fn quoted(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', r"'\''"))
}
