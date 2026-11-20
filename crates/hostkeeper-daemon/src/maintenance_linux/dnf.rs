//! The install of `dnf` ([`super::Upgrader`]) on a Red Hat family host.
//!
//! Scope `all` runs `dnf -y -q upgrade`. Scope `security` runs `dnf -y -q upgrade --security`,
//! then lists the updates left (`dnf check-update -q`, [`parse_dnf_check_update`]) and, when a
//! kernel package is among them, upgrades those packages by name: a newer kernel without an
//! advisory is due under the security scope too, as the maintenance look reports it
//! ([`DNF_KERNELS`]). Every upgrade carries one `--exclude=<name>` per package hostkeeper itself
//! installs, so none of them moves, and a kernel package of that set is not upgraded by name.
//!
//! **Configuration files.** rpm keeps a configuration file the host changed and writes the
//! package's own as `<file>.rpmnew`; nothing is passed to `dnf` for it.
//!
//! **The lock.** `dnf` waits by itself for the process that holds its lock, so the wait is bounded
//! by the deadline of the step, which is the step's own ([`LIST_DEADLINE`], [`INSTALL_DEADLINE`])
//! plus the lock wait.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use hostkeeper_launchd::{Launchctl, LaunchctlProcess};

use super::{INSTALL_DEADLINE, LIST_DEADLINE, Upgrade, Upgrader, install_failed};
use crate::maintenance_collect::{LinuxScope, listing};
use crate::maintenance_dnf::{DNF, DNF_KERNELS, parse_dnf_check_update};
use crate::maintenance_hook::KindError;

/// What `dnf check-update` exits with when it lists updates.
const UPDATES_AVAILABLE: i32 = 100;

/// The installer of a host with `dnf`.
pub struct DnfUpgrade {
    list: Box<dyn Launchctl>,
    install: Box<dyn Launchctl>,
}

impl DnfUpgrade {
    /// An installer that lists through `list` and upgrades through `install`.
    #[must_use]
    pub fn new(list: Box<dyn Launchctl>, install: Box<dyn Launchctl>) -> DnfUpgrade {
        DnfUpgrade { list, install }
    }

    /// The installer of this host: [`DNF`], the listing given [`LIST_DEADLINE`] and the upgrade
    /// [`INSTALL_DEADLINE`], each plus `lock_wait_secs`.
    #[must_use]
    pub fn at_host(lock_wait_secs: u64) -> DnfUpgrade {
        let lock = Duration::from_secs(lock_wait_secs);
        let process = |deadline: Duration| -> Box<dyn Launchctl> {
            Box::new(LaunchctlProcess::new(
                Path::new(DNF),
                deadline.saturating_add(lock),
            ))
        };
        DnfUpgrade::new(process(LIST_DEADLINE), process(INSTALL_DEADLINE))
    }

    /// Runs `dnf -y -q upgrade` with `extra` and then `excludes` and `packages`.
    fn upgrade_with(
        &self,
        what: &str,
        extra: &[&str],
        excludes: &[String],
        packages: &[String],
    ) -> Result<(), String> {
        let mut args: Vec<&str> = vec!["-y", "-q", "upgrade"];
        args.extend_from_slice(extra);
        args.extend(excludes.iter().map(String::as_str));
        args.extend(packages.iter().map(String::as_str));
        listing(&*self.install, what, &args, &[0]).map(|_| ())
    }

    /// The kernel packages `dnf check-update` lists that hostkeeper does not install, once each,
    /// in the order listed.
    fn kernels(&self, own: &BTreeSet<String>) -> Result<Vec<String>, String> {
        let (_, text) = listing(
            &*self.list,
            "dnf check-update",
            &["check-update", "-q"],
            &[0, UPDATES_AVAILABLE],
        )?;
        let mut kernels: Vec<String> = Vec::new();
        for update in parse_dnf_check_update(&text).updates {
            if DNF_KERNELS.contains(&update.name.as_str())
                && !own.contains(&update.name)
                && !kernels.contains(&update.name)
            {
                kernels.push(update.name);
            }
        }
        Ok(kernels)
    }

    fn run(&self, upgrade: &Upgrade<'_>) -> Result<(), String> {
        let excludes: Vec<String> = upgrade
            .own_packages
            .iter()
            .map(|name| format!("--exclude={name}"))
            .collect();
        match upgrade.scope {
            LinuxScope::All => self.upgrade_with("dnf upgrade", &[], &excludes, &[]),
            LinuxScope::Security => {
                self.upgrade_with("dnf upgrade --security", &["--security"], &excludes, &[])?;
                let kernels = self.kernels(upgrade.own_packages)?;
                if kernels.is_empty() {
                    return Ok(());
                }
                self.upgrade_with("dnf upgrade kernel", &[], &excludes, &kernels)
            }
        }
    }
}

impl Upgrader for DnfUpgrade {
    fn name(&self) -> &'static str {
        "dnf"
    }

    fn upgrade(&self, upgrade: &Upgrade<'_>) -> Result<(), KindError> {
        self.run(upgrade).map_err(install_failed)
    }
}

#[cfg(test)]
#[path = "dnf_tests.rs"]
mod tests;
