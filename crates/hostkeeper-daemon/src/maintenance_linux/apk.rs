//! The installer of an Alpine host: `apk` ([`ApkUpgrade`]). It refreshes the package index, lists
//! the installed packages that have a newer version (`apk version -l <`, parsed by
//! [`crate::maintenance_apk::parse_apk_version`]), pins hostkeeper's own packages and upgrades.
//!
//! `apk` has no hold, so a package of hostkeeper's that the listing shows with an update is added
//! to `/etc/apk/world` at its installed version (`name=version`); a pin that is already there is
//! set again, which changes nothing. `apk` has no security origin either, so the `security` scope
//! is the kernel alone and the `all` scope upgrades every package but the pinned ones. A
//! configuration file the host changed is kept, and the package's own is written beside it as
//! `.apk-new`. Every run that takes the database lock passes `--wait` with the lock wait; the
//! listing takes no lock.

use std::path::Path;
use std::time::Duration;

use hostkeeper_launchd::{Launchctl, LaunchctlProcess};

use super::{INSTALL_DEADLINE, LIST_DEADLINE, REFRESH_DEADLINE, Upgrade, Upgrader, install_failed};
use crate::maintenance_apk::{APK, APK_KERNELS, ApkUpdate, parse_apk_version};
use crate::maintenance_collect::{LinuxScope, listing};
use crate::maintenance_hook::KindError;

/// The installer of an Alpine host.
pub struct ApkUpgrade {
    refresh: Box<dyn Launchctl>,
    list: Box<dyn Launchctl>,
    install: Box<dyn Launchctl>,
    lock_wait_secs: u64,
}

impl ApkUpgrade {
    /// An installer that refreshes the index through `refresh`, lists through `list` and pins and
    /// upgrades through `install`: the same program, each with the deadline of its step.
    /// `lock_wait_secs` is how long a run waits for the database lock.
    #[must_use]
    pub fn new(
        refresh: Box<dyn Launchctl>,
        list: Box<dyn Launchctl>,
        install: Box<dyn Launchctl>,
        lock_wait_secs: u64,
    ) -> ApkUpgrade {
        ApkUpgrade {
            refresh,
            list,
            install,
            lock_wait_secs,
        }
    }

    /// The installer of this host: [`APK`], each step with its deadline and `lock_wait_secs` on
    /// top of it.
    #[must_use]
    pub fn at_host(lock_wait_secs: u64) -> ApkUpgrade {
        let lock = Duration::from_secs(lock_wait_secs);
        let port = |deadline: Duration| -> Box<dyn Launchctl> {
            Box::new(LaunchctlProcess::new(
                Path::new(APK),
                deadline.saturating_add(lock),
            ))
        };
        ApkUpgrade::new(
            port(REFRESH_DEADLINE),
            port(LIST_DEADLINE),
            port(INSTALL_DEADLINE),
            lock_wait_secs,
        )
    }

    /// The arguments of a run that takes the lock: `--wait`, the wait, then `rest`.
    fn locked(&self, rest: &[&str]) -> Vec<String> {
        let mut args = vec!["--wait".to_owned(), self.lock_wait_secs.to_string()];
        args.extend(rest.iter().map(|arg| (*arg).to_owned()));
        args
    }

    /// Runs `tool` with `args` and needs exit 0; the reason otherwise.
    fn step(tool: &dyn Launchctl, what: &str, args: &[String]) -> Result<(), String> {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        listing(tool, what, &args, &[0]).map(|_| ())
    }

    fn apply(&self, upgrade: &Upgrade<'_>) -> Result<(), String> {
        Self::step(
            &*self.refresh,
            "apk update",
            &self.locked(&["update", "-q"]),
        )?;
        let (_, text) = listing(&*self.list, "apk version", &["version", "-l", "<"], &[0])?;
        let updates = parse_apk_version(&text).updates;

        let pins: Vec<String> = updates
            .iter()
            .filter(|update| upgrade.own_packages.contains(&update.name))
            .map(|update| format!("{}={}", update.name, update.from))
            .collect();
        if !pins.is_empty() {
            let mut args = self.locked(&["add", "-q"]);
            args.extend(pins);
            Self::step(&*self.install, "apk add (pin)", &args)?;
        }

        match upgrade.scope {
            LinuxScope::All => Self::step(
                &*self.install,
                "apk upgrade",
                &self.locked(&["upgrade", "-q"]),
            ),
            LinuxScope::Security => {
                let kernels: Vec<&ApkUpdate> = updates
                    .iter()
                    .filter(|update| {
                        APK_KERNELS.contains(&update.name.as_str())
                            && !upgrade.own_packages.contains(&update.name)
                    })
                    .collect();
                if kernels.is_empty() {
                    return Ok(());
                }
                let mut args = self.locked(&["upgrade", "-q"]);
                args.extend(kernels.iter().map(|update| update.name.clone()));
                Self::step(&*self.install, "apk upgrade", &args)
            }
        }
    }
}

impl Upgrader for ApkUpgrade {
    fn name(&self) -> &'static str {
        "apk"
    }

    fn upgrade(&self, upgrade: &Upgrade<'_>) -> Result<(), KindError> {
        self.apply(upgrade).map_err(install_failed)
    }
}

#[cfg(test)]
#[path = "apk_tests.rs"]
mod tests;
