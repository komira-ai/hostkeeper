//! The manifest of each package's native entry, compiled in: the hook bounds, the units a host
//! runs, the health URL and the params the package's hook reads, and the oldest daemon that takes
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

/// A text param.
fn text(value: &str) -> Param {
    Param {
        value: Some(param::Value::Text(value.to_owned())),
    }
}

/// The manifest of the native entry of `package`'s release `version` on a Linux platform. The
/// hook names no command: a native package's hook is its program at the fixed path, and
/// hostkeeper's daemon runs built-in steps.
pub(super) fn of(package: Package, version: &Version) -> Manifest {
    match package {
        Package::Hostkeeper => hostkeeper(version),
        Package::Hello | Package::HelloFailReady => hello_manifest(),
        Package::Mdm => mdm_manifest(version),
    }
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
