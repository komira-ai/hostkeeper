//! The manifest of each package's native entry, compiled in: the hook's command and bounds, the
//! units a host runs, the health URL and the params the package's hook reads, and the oldest daemon that takes
//! the release. The values are the ones the package has always carried; the test that holds the
//! verb's whole answer for a release holds them too.

use hostkeeper_mdm_core::package as mdm;
use hostkeeper_proto::hostkeeper::release::v1::{Hook, Manifest, Param, StepBounds, param};

use super::INSTALLER_BOUNDS;
use crate::release::contract::{Package, Version};
use crate::release::native::hello;

/// The oldest daemon that takes hostkeeper's own package: [`hostkeeper_steps::OLDEST_READER`],
/// the oldest release that reads every file this release writes, the one constant the daemon
/// judges a release of hostkeeper by too (docs/design/two-programs.md, section 4.2). A daemon is
/// always older than the release of hostkeeper it is ordered, so the package cannot ask for its
/// own version: every daemon would answer the order `daemon_too_old` and no host would ever
/// update hostkeeper.
pub(super) const OLDEST_READER: Version = Version {
    major: hostkeeper_steps::OLDEST_READER.major,
    minor: hostkeeper_steps::OLDEST_READER.minor,
    patch: hostkeeper_steps::OLDEST_READER.patch,
};

/// The oldest hostkeeper that can run hello: the daemon of the first release.
const HELLO_MIN_HOSTKEEPER: &str = "0.1.0";

/// The unit file of the daemon's `.deb` that a host runs.
const HOSTKEEPER_UNIT: &str = "hostkeeper.service";

/// The argument after a service's program in its hook command: the program's update-hook verb.
const UPDATE_HOOK: &str = "update-hook";

/// A text param.
fn text(value: &str) -> Param {
    Param {
        value: Some(param::Value::Text(value.to_owned())),
    }
}

/// The manifest of the native entry of `package`'s release `version` on a Linux platform. A
/// service's hook command is the program its `.deb` installs ([`program`]) with `update-hook`;
/// hostkeeper's names none, since its daemon runs built-in steps.
pub(super) fn of(package: Package, version: &Version) -> Manifest {
    match package {
        Package::Hostkeeper => hostkeeper(version),
        Package::Hello | Package::HelloFailReady => hello_manifest(),
        Package::Mdm => mdm_manifest(version),
    }
}

/// The absolute path the `.deb` of `package` installs its program at, from the constants the
/// package builders write the files by; none for hostkeeper, whose daemon runs built-in steps.
pub(super) fn program(package: Package) -> Option<String> {
    match package {
        Package::Hostkeeper => None,
        Package::Hello | Package::HelloFailReady => Some(hello::DEB_PROGRAM.to_owned()),
        Package::Mdm => Some(format!("{}/{}", mdm::DEB_BIN, mdm::SERVICE)),
    }
}

/// The manifest of the native entry of `package`'s release `version` on a Mac, its signed
/// `.pkg`'s: the Linux one ([`of`]) with the `.pkg`'s identifier as its package (hostkeeper's the
/// daemon's launchd label, which its daemon takes as its own package; hello's its own), each slot
/// a launchd label (`hello.a`, `hello.b`) where hostkeeper's daemon job is no run unit, the
/// params' `units` `launchd`, and the hook command the program the `.pkg` installs. Only
/// hostkeeper and hello release a `.pkg`.
pub(super) fn of_pkg(package: Package, version: &Version) -> Manifest {
    let mut manifest = of(package, version);
    let (identifier, run) = match package {
        Package::Hostkeeper => (crate::release::macos_pkg::identifier(), Vec::new()),
        _ => (
            crate::release::native::pkg::IDENTIFIER.to_owned(),
            hello::SLOTS
                .iter()
                .map(|slot| format!("{}.{slot}", hello::NAME))
                .collect(),
        ),
    };
    manifest.package = Some(identifier);
    manifest.run = run;
    if let Some(units) = manifest.params.get_mut("units") {
        *units = text("launchd");
    }
    if let Some(hook) = manifest
        .hook
        .as_mut()
        .filter(|hook| !hook.command.is_empty())
    {
        hook.command = vec![hello::PKG_PROGRAM.to_owned(), UPDATE_HOOK.to_owned()];
    }
    manifest
}

/// The hook command of `package`'s entry: its [`program`] and `update-hook`, or none.
fn command(package: Package) -> Vec<String> {
    program(package)
        .map(|program| vec![program, UPDATE_HOOK.to_owned()])
        .unwrap_or_default()
}

/// Hostkeeper's: every step bounded at 300 seconds and the verify deadline at 600 (the table is
/// here because the daemon requires a bounded hook, and no daemon that takes the release runs
/// it), the unit file of the daemon's `.deb`, and the floor [`OLDEST_READER`], or the release's
/// own version when that is older (a build of the workspace's unreleased version).
fn hostkeeper(version: &Version) -> Manifest {
    Manifest {
        package: Some(Package::Hostkeeper.service().name().to_owned()),
        hook: Some(Hook {
            timeout_secs: Some(StepBounds {
                prepare: 300,
                cutover: 300,
                verify: 300,
                rollback: 300,
                cleanup: 300,
                uninstall: None,
            }),
            verify_deadline_secs: 600,
            command: command(Package::Hostkeeper),
            ..Hook::default()
        }),
        run: vec![HOSTKEEPER_UNIT.to_owned()],
        min_hostkeeper: (*version).min(OLDEST_READER).to_string(),
        installer_bounds: Some(INSTALLER_BOUNDS),
        ..Manifest::default()
    }
}

/// Hello's: the two slots' units, the readiness URL on its port, and the params its hook reads.
fn hello_manifest() -> Manifest {
    Manifest {
        package: Some(hello::NAME.to_owned()),
        hook: Some(Hook {
            command: command(Package::Hello),
            timeout_secs: Some(StepBounds {
                prepare: 30,
                cutover: 60,
                verify: 30,
                rollback: 60,
                cleanup: 30,
                uninstall: Some(60),
            }),
            verify_deadline_secs: 120,
            ..Hook::default()
        }),
        run: hello::SLOTS
            .iter()
            .map(|slot| format!("{}@{slot}.service", hello::NAME))
            .collect(),
        health_url: Some(format!("http://127.0.0.1:{}/readyz", hello::PORT)),
        params: [
            (
                "port".to_owned(),
                Param {
                    value: Some(param::Value::Integer(i64::from(hello::PORT))),
                },
            ),
            ("record_dir".to_owned(), text(hello::RECORD_DIR)),
            ("units".to_owned(), text("systemd")),
        ]
        .into_iter()
        .collect(),
        min_hostkeeper: HELLO_MIN_HOSTKEEPER.to_owned(),
        installer_bounds: Some(INSTALLER_BOUNDS),
        ..Manifest::default()
    }
}

/// Mdm's: the hook bounds, run units, health URL and params of `hostkeeper_mdm_core::package`,
/// and the floor hostkeeper's own release has, [`OLDEST_READER`], or the release's own version
/// when that is older.
fn mdm_manifest(version: &Version) -> Manifest {
    let secs = |step: &str| {
        mdm::TIMEOUTS
            .iter()
            .find(|(name, _)| *name == step)
            .map_or(0, |(_, secs)| *secs)
    };
    Manifest {
        package: Some(mdm::SERVICE.to_owned()),
        hook: Some(Hook {
            command: command(Package::Mdm),
            timeout_secs: Some(StepBounds {
                prepare: secs("prepare"),
                cutover: secs("cutover"),
                verify: secs("verify"),
                rollback: secs("rollback"),
                cleanup: secs("cleanup"),
                uninstall: Some(secs("uninstall")),
            }),
            verify_deadline_secs: mdm::VERIFY_DEADLINE_SECS,
            ..Hook::default()
        }),
        run: mdm::RUN.iter().map(|unit| (*unit).to_owned()).collect(),
        health_url: Some(mdm::HEALTH_URL.to_owned()),
        params: [
            ("settings".to_owned(), text(mdm::SETTINGS_FILE)),
            ("record_dir".to_owned(), text(mdm::RECORD_DIR)),
        ]
        .into_iter()
        .collect(),
        min_hostkeeper: (*version).min(OLDEST_READER).to_string(),
        installer_bounds: Some(INSTALLER_BOUNDS),
        ..Manifest::default()
    }
}
