//! The pxe package's manifest and unit: one [`Description`] of the service, rendered once, so the
//! manifest's `[params]` and the unit's command line cannot name different files. The package is
//! for the two Linux platforms, which differ only in the package's file name and the manifest's
//! platform line; macos-arm64 has no pxe package (the caller refuses it).
//!
//! - `units/pxe.service`: one `Type=notify` unit running `pxe serve --notify-systemd` with the
//!   settings file and the boot set `current` names, both from the description. `serve` sends
//!   readiness only after its whole start, so the restart of the `cutover` step returns once the
//!   new boot set is being served or has been refused.
//! - The manifest's hook is the package's own `bin/pxe`, with no hook arguments: the daemon runs
//!   `bin/pxe <step> ... --params <file>`. `prepare` has no bound (every download and attestation
//!   check inside it is bounded already, and an ISO is several GiB).
//! - The manifest's `[params]` are `release` (the release download URL of the version, which holds
//!   `netboot.json`), `settings`, `boot_dir` and `unit`, for the hook to read.
//!
//! The text is committed as goldens under `packaging/pxe/linux/` and compared by this module's
//! tests.

use super::{Contents, Unit, binary_path};
use crate::release::contract::{PXE, Platform, REPO, Version};

/// The oldest hostkeeper that can run the package: the daemon of the first release.
const MIN_HOSTKEEPER: &str = "0.1.0";

/// Everything the package says about the service, in one place.
pub(super) struct Description {
    /// The service's name: its package, binary and unit.
    name: &'static str,
    /// The unit's file name, which the manifest runs and the hook restarts.
    unit: &'static str,
    /// The root-owned settings file `serve --settings` and the hook read.
    settings: &'static str,
    /// The directory of the boot sets: `<boot_dir>/<version>` and the link `<boot_dir>/current`.
    boot_dir: &'static str,
}

/// The pxe service.
pub(super) const PXE_SERVICE: Description = Description {
    name: PXE,
    unit: "pxe.service",
    settings: "/etc/pxe/pxe.toml",
    boot_dir: "/var/lib/pxe",
};

/// Where a release's assets are downloaded from, up to the tag: `<this>/v<version>/<asset>`.
fn release_url(version: &Version) -> String {
    format!(
        "https://github.com/{REPO}/releases/download/{}",
        version.tag()
    )
}

/// The manifest and unit of the pxe package of `version` for `platform`.
pub(super) fn contents(platform: Platform, version: &Version) -> Contents {
    PXE_SERVICE.contents(platform, version)
}

impl Description {
    fn contents(&self, platform: Platform, version: &Version) -> Contents {
        Contents {
            manifest: self.manifest(platform, version),
            units: vec![Unit {
                file: self.unit.to_owned(),
                origin: format!("units/{}", self.unit),
                data: self.service_unit().into_bytes(),
            }],
            trusted_root: None,
        }
    }

    /// `manifest.toml`: the platform, the hook, the unit and the params the hook reads.
    fn manifest(&self, platform: Platform, version: &Version) -> String {
        let platform = platform.name();
        let name = self.name;
        let unit = self.unit;
        let settings = self.settings;
        let boot_dir = self.boot_dir;
        let release = release_url(version);
        format!(
            "schema = 1\n\
             name = \"{name}\"\n\
             version = \"{version}\"\n\
             platform = \"{platform}\"\n\
             min_hostkeeper = \"{MIN_HOSTKEEPER}\"\n\
             \n\
             [hook]\n\
             command = [\"bin/{name}\"]\n\
             verify_deadline_secs = 300\n\
             \n\
             [hook.timeout_secs]\n\
             prepare = 0\n\
             cutover = 120\n\
             verify = 30\n\
             rollback = 120\n\
             cleanup = 60\n\
             uninstall = 60\n\
             \n\
             [units]\n\
             run = [\"{unit}\"]\n\
             \n\
             [params]\n\
             release = \"{release}\"\n\
             settings = \"{settings}\"\n\
             boot_dir = \"{boot_dir}\"\n\
             unit = \"{unit}\"\n"
        )
    }

    /// `units/pxe.service`.
    fn service_unit(&self) -> String {
        let name = self.name;
        let program = binary_path(name);
        let settings = self.settings;
        let boot_dir = self.boot_dir;
        format!(
            "[Unit]\n\
             Description={name}, the boot server of network install\n\
             Wants=network-online.target\n\
             After=network-online.target\n\
             \n\
             [Service]\n\
             Type=notify\n\
             ExecStart={program} serve --notify-systemd --settings {settings} --dir {boot_dir}/current\n\
             Restart=on-failure\n\
             RestartSec=5\n\
             \n\
             [Install]\n\
             WantedBy=multi-user.target\n"
        )
    }
}

#[cfg(test)]
mod tests;
