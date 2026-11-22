//! The collector `apk` ([`crate::maintenance_collect`]): the updates `apk` offers on an Alpine
//! host. It refreshes the package index (`apk update -q`), then lists the installed packages that
//! have a newer version (`apk version -l <`), parsed by [`parse_apk_version`] and pinned by a
//! fixture; a line the parser does not know is counted and never guessed at. `apk` has no
//! security origin: no update is a security update, so under the security scope only a kernel is
//! due.
//!
//! **The lock.** The refresh takes apk's database lock; when another `apk` holds it, the refresh
//! fails at once (`Unable to lock database`, [`LOCKED`]) and is run again until the lock is free,
//! for at most the look's bound ([`LockWait`]), as the dnf and apt collectors wait. The listing
//! takes no lock.

use std::path::Path;
use std::time::Duration;

use hostkeeper_launchd::{Launchctl, LaunchctlProcess};

use crate::maintenance_collect::{Collector, LockWait, Look, Section, linux_item, listing};

/// The program that is run.
pub const APK: &str = "/sbin/apk";

/// How long the index refresh is waited for.
pub const APK_REFRESH_DEADLINE: Duration = Duration::from_secs(600);

/// How long the listing is waited for.
pub const APK_LIST_DEADLINE: Duration = Duration::from_secs(300);

/// The packages that are a kernel: a newer one needs a reboot.
pub const APK_KERNELS: [&str; 5] = [
    "linux-lts",
    "linux-virt",
    "linux-edge",
    "linux-stable",
    "linux-rpi",
];

/// What apk says when another holds its database lock.
pub const LOCKED: &str = "Unable to lock database";

const REFRESH_ARGS: [&str; 2] = ["update", "-q"];
const VERSION_ARGS: [&str; 3] = ["version", "-l", "<"];

/// One update `apk version -l <` lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApkUpdate {
    /// The package's name.
    pub name: String,
    /// The installed version and release (`1.36.1-r29`).
    pub from: String,
    /// The version and release it would be updated to.
    pub to: String,
}

/// What `apk version -l <` listed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApkListing {
    /// The updates, in the order listed.
    pub updates: Vec<ApkUpdate>,
    /// How many lines were not known.
    pub unparsed: u32,
}

/// What `apk version -l <` wrote: the header line (`Installed:` and `Available:`), blank lines and
/// rows `<name>-<version>-r<n> < <new version>` are known; anything else is counted.
#[must_use]
pub fn parse_apk_version(text: &str) -> ApkListing {
    let mut listing = ApkListing::default();
    for line in text.lines() {
        let row = match line.split_whitespace().collect::<Vec<_>>().as_slice() {
            [] | ["Installed:", "Available:"] => continue,
            [package, "<", to] => split_package(package).map(|(name, from)| ApkUpdate {
                name: name.to_owned(),
                from,
                to: (*to).to_owned(),
            }),
            _ => None,
        };
        match row {
            Some(update) => listing.updates.push(update),
            None => listing.unparsed = listing.unparsed.saturating_add(1),
        }
    }
    listing
}

/// `name-version-rN` as the name and `version-rN`, or `None` when it has not that shape.
fn split_package(package: &str) -> Option<(&str, String)> {
    let mut parts = package.rsplitn(3, '-');
    let (release, version, name) = (parts.next()?, parts.next()?, parts.next()?);
    Some((name, format!("{version}-{release}"))).filter(|(name, _)| !name.is_empty())
}

/// Whether an `apk` run that did not exit 0 found the lock held: it says [`LOCKED`].
#[must_use]
pub fn lock_held(code: Option<i32>, text: &str) -> bool {
    code != Some(0) && text.contains(LOCKED)
}

/// The collector of the updates `apk` offers.
pub struct Apk {
    refresh: Box<dyn Launchctl>,
    list: Box<dyn Launchctl>,
    lock: LockWait,
}

impl Apk {
    /// A collector that refreshes the index through `refresh`, waiting for a held lock as `lock`
    /// says, and lists through `list`: the same program, each with the deadline of its step.
    #[must_use]
    pub fn new(refresh: Box<dyn Launchctl>, list: Box<dyn Launchctl>, lock: LockWait) -> Apk {
        Apk {
            refresh,
            list,
            lock,
        }
    }

    /// The collector of this host: [`APK`], each refresh waited for [`APK_REFRESH_DEADLINE`], a
    /// held lock for at most `lock_wait_secs`, and the listing for [`APK_LIST_DEADLINE`].
    #[must_use]
    pub fn at_host(lock_wait_secs: u64) -> Apk {
        Apk::new(
            Box::new(LaunchctlProcess::new(Path::new(APK), APK_REFRESH_DEADLINE)),
            Box::new(LaunchctlProcess::new(Path::new(APK), APK_LIST_DEADLINE)),
            LockWait::host(lock_wait_secs),
        )
    }
}

impl Collector for Apk {
    fn name(&self) -> &'static str {
        "apk"
    }

    fn collect(&self, look: &Look<'_>) -> Result<Section, String> {
        self.lock
            .listing(&*self.refresh, "apk update", &REFRESH_ARGS, &[0], lock_held)?;
        let (_, text) = listing(&*self.list, "apk version", &VERSION_ARGS, &[0])?;
        let versions = parse_apk_version(&text);
        let mut section = Section {
            unparsed: versions.unparsed,
            ..Section::default()
        };
        for update in &versions.updates {
            let kernel = APK_KERNELS.contains(&update.name.as_str());
            if look.due(&update.name, false, kernel) {
                section.due.push(linux_item(
                    &update.name,
                    &update.from,
                    &update.to,
                    false,
                    kernel,
                ));
            }
        }
        Ok(section)
    }
}

#[cfg(test)]
#[path = "maintenance_apk_tests.rs"]
mod tests;
