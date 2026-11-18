//! The rules of a manifest (packages.md section 4.4): today's `manifest.toml` rules
//! (`hostkeeper_config::manifest`) moved to the message, and those of the installers.
//!
//! [`check`] applies every rule that needs no host and no knowledge of the entry's installer;
//! the coordinator runs it on every entry of a set (section 3.4). [`check_for`] adds the rules of
//! the entry's installer, which only the daemon applies ([`crate::receipt`]), but for the rules
//! of the hook's program and boot step, which depend on whether the installer is native
//! ([`check_hook_of`]) and which the coordinator applies too ([`crate::target::check_entry`]).
//! Neither reads
//! `min_hostkeeper` against a daemon: [`check`] only parses it. What needs the staged tree is in
//! [`crate::staged`], which runs after the installer's `stage`.
//!
//! The rules run in a fixed order, and the first broken one is the answer: the size, the counts,
//! `min_hostkeeper`, the hook, `run`, the health URL, `place`, `params`, the containers, the
//! installer bounds; then, in [`check_for`], the installer's fields, the hook's last.

use std::collections::BTreeSet;

use hostkeeper_config::manifest::{
    BadHostkeeperVersion, HealthUrlError, HostkeeperVersion, REFUSED_PREFIXES, check_health_url,
    is_boot_step,
};
use hostkeeper_config::names::{
    PathError, ServiceName, UnitError, UnitName, check_path, check_relative, is_within,
    launchd_label, plist_label,
};
use hostkeeper_config::platform::Platform;
use hostkeeper_proto::hostkeeper::release::v1::{
    Container, Hook, InstallerBounds, Manifest, Place,
};
use prost::Message as _;

use crate::installer::Installer;
use crate::{
    MAX_CONTAINERS, MAX_INSTALLER_SECS, MAX_MANIFEST_BYTES, MAX_PARAMS, MAX_PLACE, MAX_RUN,
    MIN_HOST_PORT,
};

/// The `kind` of a `place` entry that links a unit file.
pub const PLACE_UNIT: &str = "unit";
/// The `kind` of a `place` entry that links a file to an absolute path.
pub const PLACE_FILE: &str = "file";

/// Why a manifest is refused: on the daemon, `Failed`, `release`, `manifest_invalid`; on the
/// coordinator, `target_invalid`. Each names the field and the value that break the rule.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    /// Over [`MAX_MANIFEST_BYTES`] encoded.
    #[error("the manifest is {bytes} bytes encoded, more than {MAX_MANIFEST_BYTES}")]
    TooLarge {
        /// Its encoded size.
        bytes: usize,
    },
    /// More entries of a repeated field than its limit.
    #[error("the manifest has {count} {field} entries, more than {limit}")]
    TooMany {
        /// The field: `run`, `place`, `params` or `containers`.
        field: &'static str,
        /// How many it has.
        count: usize,
        /// The most it may have.
        limit: usize,
    },
    /// `min_hostkeeper` is not a version.
    #[error("min_hostkeeper {0}")]
    MinHostkeeper(BadHostkeeperVersion),
    /// A field the entry's installer needs is absent.
    #[error("the manifest of the {installer} entry needs {field}")]
    Missing {
        /// The entry's installer.
        installer: Installer,
        /// The field.
        field: &'static str,
    },
    /// A field the entry's installer refuses is present.
    #[error("the manifest of the {installer} entry may not have {field}")]
    Refused {
        /// The entry's installer.
        installer: Installer,
        /// The field.
        field: &'static str,
    },
    /// `hook.command[0]` of an entry that is not native is not a relative path of plain names.
    #[error(
        "hook.command names the program {program:?}, which {why}; it is a path relative to the tree"
    )]
    HookProgram {
        /// The program as given.
        program: String,
        /// The rule of the path grammar it breaks.
        why: PathError,
    },
    /// `hook.command[0]` of a native entry is not an absolute path of plain names.
    #[error(
        "hook.command names the program {program:?}, which {why}; on a native entry it is the absolute path of a file the package installs"
    )]
    NativeHookProgram {
        /// The program as given.
        program: String,
        /// The rule of the path grammar it breaks.
        why: PathError,
    },
    /// `hook.boot` is not the name of an additive step.
    #[error(
        "hook.boot {0:?} is not the name of an additive step: 1 to 64 of a-z, 0-9 and `_`, and none of prepare, cutover, verify, rollback, cleanup, uninstall"
    )]
    Boot(String),
    /// `hook.boot` with an empty `command` ([`check_hook_of`]), whatever the installer: the
    /// built-in steps have no additive step.
    #[error("hook.boot {0:?} needs a hook.command; the built-in steps have no boot step")]
    BootWithoutCommand(String),
    /// `hook.timeout_secs` is absent.
    #[error("hook.timeout_secs is missing")]
    NoStepBounds,
    /// A step bound other than `prepare` is 0.
    #[error("hook.timeout_secs.{0} is 0; only prepare may be unbounded")]
    ZeroBound(&'static str),
    /// `verify_deadline_secs` is less than `verify` and `rollback` together.
    #[error(
        "hook.verify_deadline_secs {deadline} is less than the verify ({verify}) and rollback ({rollback}) bounds together"
    )]
    DeadlineTooShort {
        /// `hook.verify_deadline_secs`.
        deadline: u64,
        /// `hook.timeout_secs.verify`.
        verify: u64,
        /// `hook.timeout_secs.rollback`.
        rollback: u64,
    },
    /// `promotion_ceiling_secs` is 0.
    #[error("hook.promotion_ceiling_secs is 0; leave it out for no ceiling")]
    ZeroPromotionCeiling,
    /// A `run` entry is not a unit or a label of the service.
    #[error("the run entry {unit:?} {why}")]
    RunUnit {
        /// The entry.
        unit: String,
        /// The rule of the unit grammar it breaks.
        why: UnitError,
    },
    /// `health_url` breaks the rule of a health URL.
    #[error("health_url {url:?} {why}")]
    HealthUrl {
        /// The URL.
        url: String,
        /// The rule it breaks.
        why: HealthUrlError,
    },
    /// A `place` entry's `kind` is neither `unit` nor `file`.
    #[error("the place entry {entry:?} has the kind {kind:?}, neither unit nor file")]
    PlaceKind {
        /// The entry's source.
        entry: String,
        /// Its kind.
        kind: String,
    },
    /// A `place` entry's `source` is not a relative path of plain names.
    #[error("the place source {entry:?} {why}")]
    PlaceSource {
        /// The source.
        entry: String,
        /// The rule of the path grammar it breaks.
        why: PathError,
    },
    /// A unit `place` entry's `path` is not a unit file of the service.
    #[error("the unit place path {path:?} {why}")]
    PlaceUnit {
        /// The path.
        path: String,
        /// The rule of the unit grammar it breaks.
        why: UnitError,
    },
    /// A file `place` entry's `path` is not an absolute path of the grammar.
    #[error("the file place path {path:?} {why}")]
    PlacePath {
        /// The path.
        path: String,
        /// The rule of the path grammar it breaks.
        why: PathError,
    },
    /// A file `place` entry's `path` is a refused prefix or below one.
    #[error(
        "the file place path {path:?} is {prefix} or below it, where a package may not place a file"
    )]
    ReservedPath {
        /// The path.
        path: String,
        /// The first of [`REFUSED_PREFIXES`] it is within.
        prefix: &'static str,
    },
    /// Two `place` entries of one kind have one `path`.
    #[error("two place entries have the path {0:?}")]
    DuplicatePlace(String),
    /// A param's key is `updater` or in `updater.`.
    #[error("the params key {0:?} is in the namespace `updater.`, which only the daemon writes")]
    ReservedParam(String),
    /// A param holds no value.
    #[error("the params key {0:?} holds no value: a string, an integer, a float or a boolean")]
    EmptyParam(String),
    /// A container's `unit` is not a unit of the service.
    #[error("the container unit {unit:?} {why}")]
    ContainerUnit {
        /// The unit.
        unit: String,
        /// The rule of the unit grammar it breaks.
        why: UnitError,
    },
    /// A container's `unit` is a unit of another kind than `.service`.
    #[error("the container unit {0:?} is not a .service")]
    ContainerNotService(String),
    /// A container's `unit` is not in `run`.
    #[error("the container unit {0:?} is not in run")]
    ContainerNotRun(String),
    /// Two containers have one `unit`.
    #[error("two containers have the unit {0:?}")]
    DuplicateContainer(String),
    /// A container's host port is below [`MIN_HOST_PORT`] or above 65535.
    #[error("the container {unit:?} publishes the host port {port}, not 1024 to 65535")]
    HostPort {
        /// The container's unit.
        unit: String,
        /// The port.
        port: u32,
    },
    /// A container's port in the container is 0 or above 65535.
    #[error("the container {unit:?} publishes the container port {port}, not 1 to 65535")]
    ContainerPort {
        /// The container's unit.
        unit: String,
        /// The port.
        port: u32,
    },
    /// A volume's `host_path` is not an absolute path of the grammar.
    #[error("the volume host path {path:?} {why}")]
    VolumeHostPath {
        /// The path.
        path: String,
        /// The rule of the path grammar it breaks.
        why: PathError,
    },
    /// A volume's `host_path` is outside the service's data directory.
    #[error("the volume host path {path:?} is not {dir} or below it")]
    VolumeOutside {
        /// The path.
        path: String,
        /// `/var/lib/<service>`.
        dir: String,
    },
    /// A volume's `container_path` is not an absolute path of the grammar.
    #[error("the volume container path {path:?} {why}")]
    VolumeContainerPath {
        /// The path.
        path: String,
        /// The rule of the path grammar it breaks.
        why: PathError,
    },
    /// An installer bound is 0 or over [`MAX_INSTALLER_SECS`].
    #[error("installer_bounds.{field} is {secs}, not 1 to 3600")]
    InstallerBound {
        /// The bound: `stage_secs`, `activate_secs` or `remove_secs`.
        field: &'static str,
        /// Its value.
        secs: u64,
    },
}

/// Applies the rules of every manifest of `service` for `platform`, whatever its installer and
/// host: the coordinator's check of each entry of the platform's list (section 3.4), and the
/// first part of the daemon's. The platform decides the unit names: systemd units on Linux,
/// launchd labels on macOS.
pub fn check(
    service: &ServiceName,
    platform: Platform,
    manifest: &Manifest,
) -> Result<(), ManifestError> {
    let bytes = manifest.encoded_len();
    if bytes > MAX_MANIFEST_BYTES {
        return Err(ManifestError::TooLarge { bytes });
    }
    counts(manifest)?;
    min_hostkeeper(manifest)?;
    if let Some(hook) = &manifest.hook {
        check_hook(hook)?;
    }
    let service = service.as_str();
    for unit in &manifest.run {
        run_unit(service, platform, unit).map_err(|why| ManifestError::RunUnit {
            unit: unit.clone(),
            why,
        })?;
    }
    if let Some(url) = &manifest.health_url {
        check_health_url(service, &manifest.run, url).map_err(|why| ManifestError::HealthUrl {
            url: url.clone(),
            why,
        })?;
    }
    places(service, platform, &manifest.place)?;
    params(manifest)?;
    containers(service, &manifest.run, &manifest.containers)?;
    if let Some(bounds) = &manifest.installer_bounds {
        installer_bounds(bounds)?;
    }
    Ok(())
}

/// Applies [`check`], then the rules of `installer`: the fields it requires present and those
/// it refuses absent. The daemon's check of a manifest on receipt, once its version and its
/// installer are known to fit ([`crate::receipt`]).
pub fn check_for(
    service: &ServiceName,
    platform: Platform,
    installer: Installer,
    manifest: &Manifest,
) -> Result<(), ManifestError> {
    check(service, platform, manifest)?;
    let missing = |field| ManifestError::Missing { installer, field };
    let refused = |field| ManifestError::Refused { installer, field };
    if manifest.hook.is_none() {
        return Err(missing("hook"));
    }
    if manifest.installer_bounds.is_none() {
        return Err(missing("installer_bounds"));
    }
    if manifest.package.is_none() {
        return Err(missing("package"));
    }
    if !manifest.place.is_empty() {
        return Err(refused("place"));
    }
    if !manifest.containers.is_empty() {
        return Err(refused("containers"));
    }
    check_hook_of(installer.is_native(), manifest)
}

/// The rules of the hook that depend on the entry's installer. `hook.command` may be empty for
/// every installer (the built-in steps). When it is not, its program is an absolute path of
/// plain names (the file a native package installs) on a native entry, and a relative path of
/// plain names (a file of the staged tree) on any other; its arguments are free. `hook.boot`
/// needs a `hook.command` for every installer, since the built-in steps have no boot step. The
/// coordinator applies it to every entry ([`crate::target::check_entry`], native by
/// [`crate::target::is_native_entry`]), the daemon in [`check_for`] (native by
/// [`Installer::is_native`]).
pub fn check_hook_of(native: bool, manifest: &Manifest) -> Result<(), ManifestError> {
    let Some(hook) = &manifest.hook else {
        return Ok(());
    };
    let Some(program) = hook.command.first() else {
        return match &hook.boot {
            Some(name) => Err(ManifestError::BootWithoutCommand(name.clone())),
            None => Ok(()),
        };
    };
    let program = program.clone();
    if native {
        // A space is a path character here: a `.pkg` installs under `/Library/Application
        // Support/`, and the program is run by its argument vector, never by a shell or a unit
        // file's command line. Every other rule of the grammar holds.
        check_path(&program.replace(' ', "_")).map_err(|why| ManifestError::NativeHookProgram {
            program: program.clone(),
            why,
        })
    } else {
        check_relative(&program).map_err(|why| ManifestError::HookProgram { program, why })
    }
}

/// Parses `min_hostkeeper`, which every manifest gives.
pub fn min_hostkeeper(manifest: &Manifest) -> Result<HostkeeperVersion, ManifestError> {
    HostkeeperVersion::parse(&manifest.min_hostkeeper).map_err(ManifestError::MinHostkeeper)
}

fn counts(manifest: &Manifest) -> Result<(), ManifestError> {
    let over = [
        ("run", manifest.run.len(), MAX_RUN),
        ("place", manifest.place.len(), MAX_PLACE),
        ("params", manifest.params.len(), MAX_PARAMS),
        ("containers", manifest.containers.len(), MAX_CONTAINERS),
    ]
    .into_iter()
    .find(|(_, count, limit)| count > limit);
    match over {
        Some((field, count, limit)) => Err(ManifestError::TooMany {
            field,
            count,
            limit,
        }),
        None => Ok(()),
    }
}

/// The hook: its boot step's name and its bounds as `[hook]` has them today. The shape of its
/// program, and a boot step without one, depend on the installer ([`check_hook_of`]).
fn check_hook(hook: &Hook) -> Result<(), ManifestError> {
    if let Some(name) = hook.boot.as_ref().filter(|name| !is_boot_step(name)) {
        return Err(ManifestError::Boot(name.clone()));
    }
    let bounds = hook.timeout_secs.ok_or(ManifestError::NoStepBounds)?;
    let zero = [
        ("cutover", bounds.cutover),
        ("verify", bounds.verify),
        ("rollback", bounds.rollback),
        ("cleanup", bounds.cleanup),
        ("uninstall", bounds.uninstall.unwrap_or(1)),
    ]
    .into_iter()
    .find(|(_, secs)| *secs == 0);
    if let Some((step, _)) = zero {
        return Err(ManifestError::ZeroBound(step));
    }
    let enough = bounds
        .verify
        .checked_add(bounds.rollback)
        .is_some_and(|both| hook.verify_deadline_secs >= both);
    if !enough {
        return Err(ManifestError::DeadlineTooShort {
            deadline: hook.verify_deadline_secs,
            verify: bounds.verify,
            rollback: bounds.rollback,
        });
    }
    if hook.promotion_ceiling_secs == Some(0) {
        return Err(ManifestError::ZeroPromotionCeiling);
    }
    Ok(())
}

/// Whether `platform` runs services under launchd rather than systemd.
fn is_launchd(platform: Platform) -> bool {
    platform == Platform::MacosArm64
}

/// A `run` entry: on a launchd platform a label of the service, on any other a systemd unit
/// of the service (`.service`, `.socket` or `.timer`; no template).
fn run_unit(service: &str, platform: Platform, unit: &str) -> Result<(), UnitError> {
    if is_launchd(platform) {
        launchd_label(service, unit).map(drop)
    } else {
        UnitName::of(service, unit, false).map(drop)
    }
}

/// The `place` entries: a relative source, and a unit file of the service or an absolute path
/// outside [`REFUSED_PREFIXES`]; no path twice.
fn places(service: &str, platform: Platform, places: &[Place]) -> Result<(), ManifestError> {
    let mut seen = BTreeSet::new();
    for place in places {
        check_relative(&place.source).map_err(|why| ManifestError::PlaceSource {
            entry: place.source.clone(),
            why,
        })?;
        match place.kind.as_str() {
            PLACE_UNIT => place_unit(service, platform, &place.path)?,
            PLACE_FILE => place_file(&place.path)?,
            _ => {
                return Err(ManifestError::PlaceKind {
                    entry: place.source.clone(),
                    kind: place.kind.clone(),
                });
            }
        }
        if !seen.insert((place.kind.as_str(), place.path.as_str())) {
            return Err(ManifestError::DuplicatePlace(place.path.clone()));
        }
    }
    Ok(())
}

/// A unit file a tarball places: on a launchd platform `<label>.plist` of a label of the
/// service, on any other a systemd unit of the service, templates allowed, as a file of
/// `units/` today.
fn place_unit(service: &str, platform: Platform, path: &str) -> Result<(), ManifestError> {
    let checked = if is_launchd(platform) {
        plist_label(service, path).map(drop)
    } else {
        UnitName::of(service, path, true).map(drop)
    };
    checked.map_err(|why| ManifestError::PlaceUnit {
        path: path.to_owned(),
        why,
    })
}

/// A file a tarball places: absolute, in the path grammar, and none of [`REFUSED_PREFIXES`] or
/// below one, as a `[[files]] path` today.
fn place_file(path: &str) -> Result<(), ManifestError> {
    check_path(path).map_err(|why| ManifestError::PlacePath {
        path: path.to_owned(),
        why,
    })?;
    match REFUSED_PREFIXES
        .iter()
        .find(|prefix| is_within(path, prefix))
    {
        Some(prefix) => Err(ManifestError::ReservedPath {
            path: path.to_owned(),
            prefix,
        }),
        None => Ok(()),
    }
}

/// `params`: no key in the namespace `updater.`, and every value set. The message holds
/// scalars only.
fn params(manifest: &Manifest) -> Result<(), ManifestError> {
    for (key, param) in &manifest.params {
        if (key == "updater") | key.starts_with("updater.") {
            return Err(ManifestError::ReservedParam(key.clone()));
        }
        if param.value.is_none() {
            return Err(ManifestError::EmptyParam(key.clone()));
        }
    }
    Ok(())
}

/// The containers: each unit a `.service` of the service, in `run` and no other container's;
/// ports a host may give an unprivileged process; volumes under the service's data directory.
fn containers(
    service: &str,
    run: &[String],
    containers: &[Container],
) -> Result<(), ManifestError> {
    let mut seen = BTreeSet::new();
    for container in containers {
        let unit = &container.unit;
        let name =
            UnitName::of(service, unit, false).map_err(|why| ManifestError::ContainerUnit {
                unit: unit.clone(),
                why,
            })?;
        if name.suffix != ".service" {
            return Err(ManifestError::ContainerNotService(unit.clone()));
        }
        if !run.contains(unit) {
            return Err(ManifestError::ContainerNotRun(unit.clone()));
        }
        if !seen.insert(unit.as_str()) {
            return Err(ManifestError::DuplicateContainer(unit.clone()));
        }
        ports(container)?;
        volumes(service, container)?;
    }
    Ok(())
}

fn ports(container: &Container) -> Result<(), ManifestError> {
    let unit = || container.unit.clone();
    for port in &container.ports {
        if !(MIN_HOST_PORT..=65535).contains(&port.host) {
            return Err(ManifestError::HostPort {
                unit: unit(),
                port: port.host,
            });
        }
        if !(1..=65535).contains(&port.container) {
            return Err(ManifestError::ContainerPort {
                unit: unit(),
                port: port.container,
            });
        }
    }
    Ok(())
}

fn volumes(service: &str, container: &Container) -> Result<(), ManifestError> {
    let dir = format!("/var/lib/{service}");
    for volume in &container.volumes {
        let host = &volume.host_path;
        check_path(host).map_err(|why| ManifestError::VolumeHostPath {
            path: host.clone(),
            why,
        })?;
        if !is_within(host, &dir) {
            return Err(ManifestError::VolumeOutside {
                path: host.clone(),
                dir,
            });
        }
        check_path(&volume.container_path).map_err(|why| ManifestError::VolumeContainerPath {
            path: volume.container_path.clone(),
            why,
        })?;
    }
    Ok(())
}

fn installer_bounds(bounds: &InstallerBounds) -> Result<(), ManifestError> {
    let out = [
        ("stage_secs", bounds.stage_secs),
        ("activate_secs", bounds.activate_secs),
        ("remove_secs", bounds.remove_secs),
    ]
    .into_iter()
    .find(|(_, secs)| !(1..=MAX_INSTALLER_SECS).contains(secs));
    match out {
        Some((field, secs)) => Err(ManifestError::InstallerBound { field, secs }),
        None => Ok(()),
    }
}
