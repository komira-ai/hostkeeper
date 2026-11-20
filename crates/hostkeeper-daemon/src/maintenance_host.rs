//! The collector `host` of the maintenance look ([`crate::maintenance_collect`]): what a Linux
//! host runs (its `os-release`, the running kernel, the newest kernel installed), when it booted,
//! and the reboot a newer installed kernel makes due (docs/design/maintenance-updates.md,
//! section 1).

use std::cmp::Ordering;
use std::path::Path;
use std::time::Duration;

use hostkeeper_launchd::{Launchctl, LaunchctlProcess};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_proto::maintenance::{MAX_KERNEL_TEXT, MAX_OS_TEXT};

use crate::maintenance_collect::{
    Collector, HostRoot, Look, Section, clip, compare_versions, listing, reboot_item,
};

/// The program that names the running kernel's release.
pub const UNAME: &str = "/usr/bin/uname";

/// How long `uname` is given.
pub const UNAME_DEADLINE: Duration = Duration::from_secs(60);

/// The step's name in a failed look's reason.
const UNAME_STEP: &str = "uname -r";

/// The file that names the operating system.
const OS_RELEASE: &str = "/etc/os-release";

/// The directory with one directory per kernel installed.
const MODULES: &str = "/lib/modules";

/// The kernel's boot id.
const BOOT_ID: &str = "/proc/sys/kernel/random/boot_id";

/// The kernel's counters, among them the boot time.
const PROC_STAT: &str = "/proc/stat";

/// The collector `host`: the facts of the host and the reboot a newer kernel makes due.
pub struct HostFacts {
    uname: Box<dyn Launchctl>,
    root: HostRoot,
}

impl HostFacts {
    /// The collector over `uname` (the program that names the running kernel) and the files under
    /// `root`.
    #[must_use]
    pub fn new(uname: Box<dyn Launchctl>, root: HostRoot) -> HostFacts {
        HostFacts { uname, root }
    }

    /// The collector over the host's own `uname` ([`UNAME`], bounded by [`UNAME_DEADLINE`]) and
    /// its own files.
    #[must_use]
    pub fn at_host() -> HostFacts {
        HostFacts::new(
            Box::new(LaunchctlProcess::new(Path::new(UNAME), UNAME_DEADLINE)),
            HostRoot::host(),
        )
    }

    /// The release of the running kernel, as `uname -r` prints it.
    fn running_kernel(&self) -> Result<String, String> {
        let (_, text) = listing(self.uname.as_ref(), UNAME_STEP, &["-r"], &[0])?;
        let release = text.trim();
        if release.is_empty() {
            return Err(format!("{UNAME_STEP} printed nothing"));
        }
        Ok(release.to_owned())
    }

    /// The greatest release among the kernels installed (the directories of `/lib/modules` that
    /// hold a `kernel` directory), or `running` when there is none.
    fn newest_kernel(&self, running: &str) -> Result<String, String> {
        Ok(self
            .root
            .dirs_in(MODULES)?
            .into_iter()
            .filter(|name| self.root.exists(&format!("{MODULES}/{name}/kernel")))
            .max_by(|a, b| compare_versions(a, b))
            .unwrap_or_else(|| running.to_owned()))
    }
}

impl Collector for HostFacts {
    fn name(&self) -> &'static str {
        "host"
    }

    fn collect(&self, _look: &Look<'_>) -> Result<Section, String> {
        let (os_name, os_version) = parse_os_release(&self.root.read(OS_RELEASE)?);
        let running = self.running_kernel()?;
        let newest = self.newest_kernel(&running)?;
        let boot_id = self.root.read(BOOT_ID)?;
        let boot_unix = self
            .root
            .read_if_there(PROC_STAT)?
            .map_or(0, |text| parse_boot_time(&text));
        let due = (compare_versions(&newest, &running) == Ordering::Greater)
            .then(|| reboot_item("kernel", &running))
            .into_iter()
            .collect();
        Ok(Section {
            host: pb::MaintenanceHost {
                os_name: clip(&os_name, MAX_OS_TEXT),
                os_version: clip(&os_version, MAX_OS_TEXT),
                running_kernel: clip(&running, MAX_KERNEL_TEXT),
                newest_kernel: clip(&newest, MAX_KERNEL_TEXT),
                boot_id: clip(boot_id.trim(), MAX_KERNEL_TEXT),
                boot_unix,
                ..pb::MaintenanceHost::default()
            },
            due,
            ..Section::default()
        })
    }
}

/// The `ID` and the `VERSION_ID` of an `os-release` file, each empty when the file has no such
/// line; a value may be in double or single quotes, which are taken off.
#[must_use]
pub fn parse_os_release(text: &str) -> (String, String) {
    let value = |key: &str| -> String {
        text.lines()
            .find_map(|line| line.trim().strip_prefix(key)?.strip_prefix('='))
            .map(unquote)
            .unwrap_or_default()
    };
    (value("ID"), value("VERSION_ID"))
}

/// `value` without one pair of double or single quotes around it.
fn unquote(value: &str) -> String {
    let value = value.trim();
    value
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|inner| inner.strip_suffix('\''))
        })
        .unwrap_or(value)
        .to_owned()
}

/// The Unix time of the `btime` line of `/proc/stat`; 0 when there is none or it is not a number.
#[must_use]
pub fn parse_boot_time(text: &str) -> u64 {
    text.lines()
        .find_map(|line| line.strip_prefix("btime "))
        .and_then(|seconds| seconds.trim().parse().ok())
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "maintenance_host_tests.rs"]
mod tests;
