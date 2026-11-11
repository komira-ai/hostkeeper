//! Services of any name on a host whose daemon announces apt, for the tests whose subject is not
//! the installer but what the daemon does around an order: a service running a version that was
//! installed earlier ([`installed`]), and a version published in the repository with the entry and
//! the order that carry it ([`publish`], [`install_of`], [`update_of`]).
//!
//! An entry's manifest must run at least one unit, so these services run `<service>.service`:
//! the daemon's test manifest with a `[units]` table added. Each package carries a hook at its
//! fixed path, so its steps go through the rig's scripted launcher as a hook's do.

use std::fs;
use std::sync::Arc;

use hostkeeper_daemon::testing::{ScriptedLauncher, manifest};
use hostkeeper_daemon::{Daemon, Ports};
use hostkeeper_proto::hostkeeper::agent::v1::Order;
use hostkeeper_proto::hostkeeper::release::v1::Target;

use super::seen::succeeded;
use super::{AptRig, Deb, bridge, with_entry};
use crate::support::{BASE, OWN, config, install, update, versions::entry_of};

/// The test manifest of `service` at `version`, running the unit `<service>.service`.
pub fn manifest_text(service: &str, version: &str) -> String {
    format!(
        "{}\n[units]\nrun = [\"{service}.service\"]\n",
        manifest(service, version)
    )
}

/// The package of `service` at `package_version`: a program, its unit and a hook at the fixed
/// path.
pub fn deb_of(service: &str, package_version: &str) -> Deb {
    Deb::new(service, package_version, "amd64")
        .file(
            &format!("usr/bin/{service}"),
            0o755,
            &format!("#!/bin/sh\necho {package_version}\n"),
        )
        .file(
            &format!("usr/lib/systemd/system/{service}.service"),
            0o644,
            &format!(
                "[Unit]\nDescription={service}\n[Service]\nExecStart=/usr/bin/{service}\n[Install]\nWantedBy=multi-user.target\n"
            ),
        )
        .file(
            &format!("usr/lib/{service}/hostkeeper-hook"),
            0o755,
            "#!/bin/sh\nexit 0\n",
        )
}

/// `service` runs `version`, installed by an earlier order that left no record of its own: the
/// order runs on a daemon of its own with a launcher of its own (so the rig's launcher records
/// nothing of it and has no script consumed by it), and the step record it left is removed, as a
/// host that was installed before the daemon under test started has none. What stays is what
/// the installer leaves: the version directory with its copy of the index and the package,
/// `current`, the record of `placed.json`, and the package installed and held.
pub fn installed(apt: &AptRig, service: &str, version: &str) {
    let launcher = ScriptedLauncher::new();
    launcher.answer_tools(bridge(Arc::clone(&apt.host)));
    let ports = Ports {
        launcher,
        ..apt.ports()
    };
    let daemon = Daemon::new(config(), apt.rig.dirs(), ports, OWN);
    let order = install_of(apt, service, version, 1);
    assert_eq!(daemon.order(&order), succeeded(), "{service} {version}");
    drop(daemon);
    fs::remove_dir_all(apt.rig.state.join(service)).expect("the step record of the install");
}

/// A service with no `current` is installed at [`BASE`] first, so that an update of it runs.
pub fn based(apt: &AptRig, service: &str) {
    let current = apt.rig.packages.join(service).join("current");
    if fs::symlink_metadata(current).is_err() {
        installed(apt, service, BASE);
    }
}

/// The package of `service` at `version` (as the package version `<version>-1`) published in the
/// repository, and the entry that names it, pinned to its file.
pub fn publish(apt: &AptRig, service: &str, version: &str) -> Target {
    let package_version = format!("{version}-1");
    let pin = apt.publish(deb_of(service, &package_version));
    let (mut entry, _) = entry_of(service, version, &manifest_text(service, version));
    entry.digest = pin;
    entry
}

/// The order to install `service` at `version`, its package published for it.
pub fn install_of(apt: &AptRig, service: &str, version: &str, order_try: u64) -> Order {
    let entry = publish(apt, service, version);
    with_entry(install(service, version, &entry.digest, order_try), &entry)
}

/// The order to update `service` to `version`, its package published for it. A service that is
/// not running yet runs [`BASE`] first ([`based`]).
pub fn update_of(apt: &AptRig, service: &str, version: &str, order_try: u64) -> Order {
    based(apt, service);
    let entry = publish(apt, service, version);
    with_entry(update(service, version, &entry.digest, order_try), &entry)
}
