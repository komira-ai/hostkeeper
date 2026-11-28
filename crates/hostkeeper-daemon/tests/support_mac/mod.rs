//! A Mac maintenance run end to end, in process (`support_e2e`: the real coordinator, and on each
//! host the real daemon, its heartbeat loop and its run record), over a fake Mac.
//!
//! The fake Mac ([`Machine`]) is a scratch directory standing for the Mac's files (`/Applications`
//! with its Xcodes and installer apps, the update account's file, root's with mode 0600 there as
//! on a Mac but owned by the test's user, and device management's settings), the programs the
//! maintenance hook and the daemon's report run (`sw_vers`, `ioreg`, `softwareupdate`, `fdesetup`,
//! `pkgutil`, `dscl`, `sysadminctl`, `xcode-select`, each Xcode's `xcodebuild`, `xip`, `codesign`
//! and the installer app's `startosinstall`, and the probe's `stat`, `who`, `id`, `launchctl` and
//! `env`, which answer as a farm Mac did with nobody logged in), the coordinator's Xcode store as
//! the hook's stage reads it, a boot id that changes when the Mac restarts, and one service,
//! `alpha`, whose hook declares `drain` and `resume`.
//!
//! Every program the maintenance hook runs is one line of the Mac's effects, in order, with its
//! arguments (a path under the scratch directory written as the Mac's own path), beside each hook
//! step's start, each drain and resume, each fetch from the Xcode store and each boot into a new
//! boot id; what each install program got on its standard input is kept apart ([`Input`]), and so
//! is each line the hook logged. The report's programs are not effects: the daemon looks again on
//! its own schedule.
//!
//! The maintenance hook's `prepare`, `cutover`, restart and `verify` are the real hook's over the
//! real `macos` kind (its stage and its apply), the real `xcode` kind (its stage and its apply)
//! and the real `probe` kind (its clock at [`PROBED_UNIX`]),
//! each over the fake Mac's programs and files, and the daemon's report is the real Mac
//! collectors' (`macos`, `mac`, `xcode`, `update-account`, `mdm`) over the same programs and files.
//! The Mac's heartbeats name the platform `darwin-arm64`, as a Mac's daemon does.
//!
//! An install asks for a restart when it runs with `--restart`, when it is `startosinstall`, or
//! when `fdesetup authrestart` follows it; the restart (`Node::reboot`) boots the Mac into the
//! build the install put on it, with nothing listed by `softwareupdate` any more; `Node::come_back`
//! does the same for a Mac whose daemon already stopped.
#![allow(
    dead_code,
    unused_imports,
    reason = "each test binary uses part of the shared support, and of what it re-exports"
)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hostkeeper_daemon::mac::ioreg_args;
use hostkeeper_daemon::maintenance_account::{self, UpdateAccountFacts};
use hostkeeper_daemon::maintenance_collect::{Collector, HostRoot};
use hostkeeper_daemon::maintenance_hook::macos::{AccountFile, Apply, Installers, Programs, Stage};
use hostkeeper_daemon::maintenance_hook::xcode::{self, store::Store};
use hostkeeper_daemon::maintenance_hook::{
    Ending as HookEnding, HookStep, Kind, MacosKind, ProbeKind, Rebooter, Registry, Unwired,
    XcodeKind,
};
use hostkeeper_daemon::maintenance_mac::{FreeSpace, MacPorts, MacReadiness};
use hostkeeper_daemon::maintenance_macos::{Macos, MacosPorts};
use hostkeeper_daemon::maintenance_mdm::MdmConflicts;
use hostkeeper_daemon::maintenance_plist::HostPlists;
use hostkeeper_daemon::maintenance_xcode::Xcodes;
use hostkeeper_launchd::{
    Launchctl, LaunchctlError, LaunchctlOutput, PlatformKind, StdinError, StdinRun,
};
use hostkeeper_link::{Got, Limit, Log, UnixClock};
use hostkeeper_package::gh::{Ran, Runner, Spawn};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_proto::hostkeeper::operator::v1 as op;
use hostkeeper_steps::{BootId, BootIdError, BootIdSource};
use prost::Message;

use crate::support::Scratch;
pub use crate::support_e2e::{
    Host, Ordered, SERVICE, World, after_drained, first_offer, one_at_a_time,
};

mod machine;
mod programs;

pub use machine::{Account, Input, Mac, Machine};

/// A host of a Mac test.
pub type Node = crate::support_e2e::Node<Machine>;

impl Node {
    /// The Mac `host` of the group `group` of the test `test`, running `alpha` 1.0.0 on a fake
    /// Mac set up as `mac` says; its daemon started.
    pub fn mac(world: &Arc<World>, test: &str, host: &str, group: &str, mac: Mac) -> Node {
        let machine = Machine::new(&format!("{test}-{host}"), mac);
        machine.serve_xcode_store(world);
        Node::on(world, test, host, group, machine)
    }
}

/// The update account's name.
pub const ACCOUNT: &str = "hkupdate";
/// The update account's password: it holds the characters a property list escapes.
pub const PASSWORD: &str = "fix & <ship> 'it'";
/// The update account's password as the property list of `fdesetup authrestart` writes it.
pub const PASSWORD_ESCAPED: &str = "fix &amp; &lt;ship&gt; &apos;it&apos;";

/// The macOS every fake Mac runs at first.
pub const OLD_VERSION: &str = "26.6";
/// The build of [`OLD_VERSION`].
pub const OLD_BUILD: &str = "25G62";
/// The minor update `softwareupdate --list` offers, by its label.
pub const MINOR_LABEL: &str = "macOS Tahoe 26.7-25H62";
/// The version of the minor update.
pub const MINOR_VERSION: &str = "26.7";
/// The build of the minor update.
pub const MINOR_BUILD: &str = "25H62";
/// The major upgrade `softwareupdate --list-full-installers` offers.
pub const MAJOR_VERSION: &str = "27.1";
/// The build of the major upgrade.
pub const MAJOR_BUILD: &str = "26B70";
/// The name of the installer app the full installer's download leaves in `/Applications`, in two
/// pieces so that the hygiene lint does not read its last two labels as a host.
pub const INSTALLER_NAME: &str = concat!("Install macOS 27", ".app");
/// [`INSTALLER_NAME`]'s path on the Mac.
pub const INSTALLER_APP: &str = concat!("/Applications/Install macOS 27", ".app");

/// The Xcode the Mac has at first, which a turn that installs [`NEW_XCODE`] removes.
pub const OLD_XCODE: &str = "25.4";
/// The build of [`OLD_XCODE`].
pub const OLD_XCODE_BUILD: &str = "25E5";
/// The Xcode the coordinator's store holds.
pub const NEW_XCODE: &str = "27.2";
/// The build of [`NEW_XCODE`].
pub const NEW_XCODE_BUILD: &str = "27B52";
/// The bytes of [`NEW_XCODE`]'s archive, as the store holds it.
pub const NEW_XCODE_ARCHIVE: &[u8] = b"the archive of Xcode 27.2 as Apple ships it";

/// The free bytes of the data volume: more than twice the largest download.
pub const FREE: u64 = 80_000_000_000;
/// The free bytes of a data volume too full for the minor update.
pub const FULL_DISK_FREE: u64 = 1_000_000_000;
/// The bytes of the minor update, as `softwareupdate --list` gives them.
pub const MINOR_BYTES: u64 = 4_718_592 * 1024;

/// `softwareupdate --list` on a Mac of macOS 26: the minor update and a major upgrade.
const LIST: &str = include_str!("../fixtures/maintenance/softwareupdate/macos26_list_major.txt");
/// `softwareupdate --list-full-installers` on a Mac of macOS 26.
const FULL: &str =
    include_str!("../fixtures/maintenance/softwareupdate/macos26_full_installers.txt");
/// A listing of `softwareupdate` that offers nothing.
const NONE: &str = include_str!("../fixtures/maintenance/softwareupdate/macos26_list_none.txt");
/// `ioreg` of the Mac's platform device.
const IOREG: &str = include_str!("../fixtures/maintenance/mac/ioreg.txt");
/// `pkgutil --pkg-info` of the Command Line Tools.
const PKGUTIL: &str = include_str!("../fixtures/maintenance/mac/pkgutil_clt.txt");
/// `pkgutil --check-signature` of an archive Apple signed.
const PKGUTIL_SIGNATURE: &str =
    include_str!("../fixtures/maintenance/mac/pkgutil_check_signature.txt");
/// `fdesetup list`: the update account is a FileVault user.
const FILEVAULT_USERS: &str = include_str!("../fixtures/maintenance/mac/fdesetup_list.txt");

/// Where the Mac keeps its update account.
const ACCOUNT_FILE: &str = "/Library/Application Support/hostkeeper/update-account";
/// The device-management setting that conflicts with the daemon's install.
pub const MDM_SETTING: &str = "AutomaticallyInstallMacOSUpdates";
/// Where device management writes [`MDM_SETTING`].
const MDM_FILE: &str = "/Library/Managed Preferences/com.apple.SoftwareUpdate.plist";

/// The programs of a Mac, by their path.
pub const SW_VERS: &str = "/usr/bin/sw_vers";
pub const SOFTWAREUPDATE: &str = "/usr/sbin/softwareupdate";
pub const FDESETUP: &str = "/usr/bin/fdesetup";
pub const XCODE_SELECT: &str = "/usr/bin/xcode-select";
pub const XIP: &str = "/usr/bin/xip";
pub const CODESIGN: &str = "/usr/bin/codesign";
const IOREG_PROGRAM: &str = "/usr/sbin/ioreg";
pub const PKGUTIL_PROGRAM: &str = "/usr/sbin/pkgutil";
const DSCL: &str = "/usr/bin/dscl";
const SYSADMINCTL: &str = "/usr/sbin/sysadminctl";
/// The probe's programs the other constants do not name.
pub const STAT: &str = "/usr/bin/stat";
pub const WHO: &str = "/usr/bin/who";
pub const ID: &str = "/usr/bin/id";
pub const LAUNCHCTL: &str = "/bin/launchctl";
pub const ENV: &str = "/usr/bin/env";
/// The second the probe's clock reads.
pub const PROBED_UNIX: u64 = 1_900_000_500;

/// The `xcodebuild` of the Xcode `version`, by its path on the Mac.
pub fn xcodebuild(version: &str) -> String {
    format!("/Applications/Xcode-{version}.app/Contents/Developer/usr/bin/xcodebuild")
}

/// The `version.plist` of an Xcode of `version` and `build`, as Apple writes it.
fn version_plist(version: &str, build: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n\
         \t<key>CFBundleShortVersionString</key>\n\t<string>{version}</string>\n\
         \t<key>ProductBuildVersion</key>\n\t<string>{build}</string>\n</dict>\n</plist>\n"
    )
}

/// The property list `fdesetup authrestart -inputplist` gets on its standard input for the update
/// account.
pub fn restart_plist() -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n\
         \t<key>Username</key>\n\t<string>{ACCOUNT}</string>\n\
         \t<key>Password</key>\n\t<string>{PASSWORD_ESCAPED}</string>\n\
         </dict>\n</plist>\n"
    )
}

/// `words` as the arguments of a program.
pub fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

/// Each host of the run `run`: its id, its step, why `macos` was skipped and the daemon's reason.
pub fn turns(
    world: &World,
    run: &str,
) -> Vec<(String, op::MaintenanceHostStep, op::MaintenanceSkip, String)> {
    world
        .hosts(run)
        .iter()
        .map(|host| {
            (
                host.host_id.clone(),
                host.step(),
                host.skipped_because(),
                host.reason.clone(),
            )
        })
        .collect()
}

/// Asserts that the update account's password is nowhere but on the install programs' standard
/// input: in no effect (no argument), no line the hook logged, no heartbeat the Mac sent, no
/// file of the daemon's state (its run record, the hook's parameters and plan) and not in the
/// reason of a halt.
pub fn password_only_on_stdin(node: &Node) {
    let held = |text: &str| text.contains(PASSWORD);
    assert!(!node.machine.effects().iter().any(|line| held(line)));
    assert!(!node.machine.logged().iter().any(|line| held(line)));
    let needle = PASSWORD.as_bytes();
    let holds = |bytes: &[u8]| bytes.windows(needle.len()).any(|window| window == needle);
    assert!(
        !node
            .sent()
            .iter()
            .any(|request| holds(&request.encode_to_vec()))
    );
    let mut dirs = vec![node.rig.state.clone()];
    let mut scanned = 0;
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).expect("a state directory") {
            let path = entry.expect("an entry").path();
            match path.is_dir() {
                true => dirs.push(path),
                false => {
                    scanned += 1;
                    assert!(
                        !holds(&std::fs::read(&path).expect("a state file")),
                        "{} holds the password",
                        path.display()
                    );
                }
            }
        }
    }
    assert!(scanned > 0, "the daemon's state holds files to look in");
    let record = node.record().map(|record| format!("{record:?}"));
    assert!(!record.is_some_and(|text| held(&text)));
}
