//! The `apt` upgrader of the maintenance hook's `linux` kind (docs/design/maintenance-updates.md
//! section 5.2 step 3): on a Debian or Ubuntu host, it refreshes the index, simulates the
//! `dist-upgrade` as the maintenance look does, holds the packages hostkeeper itself installs that
//! the simulation would move, and installs the updates of the run's scope with `apt-get`.
//!
//! Every `apt-get` run keeps the configuration files the host has: `--force-confdef` with
//! `--force-confold` keeps a file the host changed and takes the package's default only for a
//! file nobody changed, so no `dpkg` prompt is raised. The dpkg lock is waited for by `apt-get`
//! itself (`DPkg::Lock::Timeout`, the hook's `--package-lock-wait-secs`), and each program's
//! deadline is its own plus that wait; `apt-mark` has no lock timeout, so a hold that finds the
//! lock held fails at once. Only the install runs package scripts, so only it is run
//! through `/usr/bin/env` with [`SCRIPTS_ENV`].

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use hostkeeper_launchd::{Launchctl, LaunchctlProcess};

use crate::maintenance_apt::{APT_GET, AptUpdate, parse_apt_simulation};
use crate::maintenance_collect::{LinuxScope, Look, listing};
use crate::maintenance_hook::KindError;
use crate::maintenance_linux::{
    INSTALL_DEADLINE, LIST_DEADLINE, REFRESH_DEADLINE, Upgrade, Upgrader, install_failed,
    without_arch,
};

/// The program that marks the packages hostkeeper installs as held.
pub const APT_MARK: &str = "/usr/bin/apt-mark";

/// The program that gives the install its environment on its argument list.
pub const ENV: &str = "/usr/bin/env";

/// The environment of the install, which runs package scripts: a fixed `PATH`, since the
/// program's own environment is empty and `dpkg` refuses to install without one (it looks for
/// `ldconfig` and `start-stop-daemon` on it); `DEBIAN_FRONTEND=noninteractive`,
/// so that no script asks a question; `NEEDRESTART_SUSPEND=1`, which needrestart's apt hook
/// (`DPkg::Post-Invoke`, `/usr/lib/needrestart/apt-pinvoke`) reads as "do not run", so it does not
/// restart the services whose binaries the install replaced while the update drains one of them
/// and starts another; and `NEEDRESTART_MODE=l`, so that a needrestart that runs all the same
/// lists what it would restart and restarts nothing.
pub const SCRIPTS_ENV: [&str; 4] = [
    "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
    "DEBIAN_FRONTEND=noninteractive",
    "NEEDRESTART_SUSPEND=1",
    "NEEDRESTART_MODE=l",
];

/// The `apt` upgrader.
pub struct AptUpgrade {
    refresh: Box<dyn Launchctl>,
    simulate: Box<dyn Launchctl>,
    mark: Box<dyn Launchctl>,
    install: Box<dyn Launchctl>,
    lock_wait_secs: u64,
}

impl AptUpgrade {
    /// An upgrader refreshing the index with `refresh`, simulating with `simulate`, holding with
    /// `mark` (an `apt-mark`) and installing with `install` (an `env` that runs `apt-get`); each
    /// `apt-get` is given `lock_wait_secs` as its dpkg lock timeout.
    #[must_use]
    pub fn new(
        refresh: Box<dyn Launchctl>,
        simulate: Box<dyn Launchctl>,
        mark: Box<dyn Launchctl>,
        install: Box<dyn Launchctl>,
        lock_wait_secs: u64,
    ) -> AptUpgrade {
        AptUpgrade {
            refresh,
            simulate,
            mark,
            install,
            lock_wait_secs,
        }
    }

    /// The upgrader of this host: [`APT_GET`], [`APT_MARK`] and [`ENV`] as root, each bounded by
    /// its step's deadline plus `lock_wait_secs`.
    #[must_use]
    pub fn at_host(lock_wait_secs: u64) -> AptUpgrade {
        let lock = Duration::from_secs(lock_wait_secs);
        let port = |program: &str, deadline: Duration| -> Box<dyn Launchctl> {
            Box::new(LaunchctlProcess::new(
                Path::new(program),
                deadline.saturating_add(lock),
            ))
        };
        AptUpgrade::new(
            port(APT_GET, REFRESH_DEADLINE),
            port(APT_GET, LIST_DEADLINE),
            port(APT_MARK, LIST_DEADLINE),
            port(ENV, INSTALL_DEADLINE),
            lock_wait_secs,
        )
    }

    /// The option that makes `apt-get` wait for the dpkg lock.
    fn lock_option(&self) -> String {
        format!("DPkg::Lock::Timeout={}", self.lock_wait_secs)
    }
}

impl Upgrader for AptUpgrade {
    fn name(&self) -> &'static str {
        "apt"
    }

    fn upgrade(&self, upgrade: &Upgrade<'_>) -> Result<(), KindError> {
        steps(self, upgrade).map_err(install_failed)
    }
}

/// The steps of the install; the reason of the first that did not succeed.
fn steps(apt: &AptUpgrade, upgrade: &Upgrade<'_>) -> Result<(), String> {
    let lock = apt.lock_option();
    listing(
        &*apt.refresh,
        "apt-get update",
        &["-o", &lock, "update", "-qq"],
        &[0],
    )?;
    let (_, text) = listing(
        &*apt.simulate,
        "apt-get dist-upgrade simulation",
        &["-o", &lock, "--simulate", "-qq", "dist-upgrade"],
        &[0],
    )?;
    let updates = parse_apt_simulation(&text).updates;

    let held = moving_own(&updates, upgrade.own_packages);
    if !held.is_empty() {
        // apt-mark has no lock timeout of its own; a held lock fails the hold at once.
        let mut args = vec!["hold"];
        args.extend(held.iter().map(String::as_str));
        listing(&*apt.mark, "apt-mark hold", &args, &[0])?;
    }

    // Made before `args`, which borrows them.
    let names = security_names(&updates, upgrade.own_packages);
    let mut args: Vec<&str> = SCRIPTS_ENV.to_vec();
    args.extend([
        APT_GET,
        "-o",
        lock.as_str(),
        "-o",
        "Dpkg::Options::=--force-confdef",
        "-o",
        "Dpkg::Options::=--force-confold",
        "-qq",
        "-y",
    ]);
    let what = match upgrade.scope {
        // dist-upgrade keeps back the held packages, so the own packages stay where they are.
        LinuxScope::All => {
            args.push("dist-upgrade");
            "apt-get dist-upgrade"
        }
        LinuxScope::Security => {
            if names.is_empty() {
                return Ok(());
            }
            args.extend(["install", "--only-upgrade"]);
            args.extend(names.iter().map(String::as_str));
            "apt-get install"
        }
    };
    listing(&*apt.install, what, &args, &[0])?;
    Ok(())
}

/// The names, without architecture, sorted and each once, of the own packages the simulation
/// would move.
fn moving_own(updates: &[AptUpdate], own: &BTreeSet<String>) -> Vec<String> {
    updates
        .iter()
        .map(|update| without_arch(&update.package))
        .filter(|name| own.contains(*name))
        .map(str::to_owned)
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect()
}

/// The packages `install --only-upgrade` is given for the `security` scope: the updates of the
/// simulation the look makes due under that scope ([`AptUpdate::is_due`]: the installed packages
/// that are security updates or kernels and are not own packages), in the simulation's order,
/// each once and named as apt printed it. So what the look and the check after the install report
/// due is what this install names. A package not yet installed (a new `linux-image-<version>`, a
/// new library) is not named and not due: apt installs it only as a dependency of a named package
/// whose new version needs it (the kernel meta-package `linux-image-<flavour>` on Debian,
/// `linux-image-generic` on Ubuntu, which is named when it is upgraded).
fn security_names(updates: &[AptUpdate], own: &BTreeSet<String>) -> Vec<String> {
    let look = Look {
        scope: LinuxScope::Security,
        own_packages: own,
    };
    let mut names: Vec<String> = Vec::new();
    for update in updates {
        if update.is_due(&look) && !names.contains(&update.package) {
            names.push(update.package.clone());
        }
    }
    names
}

#[cfg(test)]
#[path = "apt_tests.rs"]
mod tests;
