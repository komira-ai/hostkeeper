//! The manifest of an installed version, as the daemon's reader builds it from the version's sealed
//! `target.json` (docs/design/hostkeeper-design.md, section 4.7): the service's lifecycle, its
//! hook and bounds, its units and files, and the rules the release's own checks share with it.
//!
//! - [`Manifest`], [`Hook`], [`Timeouts`] and [`FileEntry`] are the value; [`Expected`] is the
//!   order it is read for and [`HostkeeperVersion`] the floor `min_hostkeeper` and the running
//!   daemon are compared in.
//! - [`ManifestRefusal`] says why a version is refused, with its class ([`Class`]) and its reason
//!   token.
//! - [`check_health_url`], [`is_boot_step`] and [`REFUSED_PREFIXES`] are the rules of a health
//!   URL, of the name of a hook's boot step and of the paths a package may not place a file at,
//!   which a release manifest (crate `hostkeeper-release`) is checked by as well.
//!
//! This module reads no file.

use std::collections::BTreeSet;
use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::num::NonZeroU64;

use crate::names::{ServiceName, UnitName, Version};
use crate::platform::Platform;
use crate::url;

/// The directories a placed file's path may not be, or lie below: hostkeeper's and the updater's
/// own, the unit directory (units go through `units/`), the system's and the services' data.
pub const REFUSED_PREFIXES: [&str; 8] = [
    "/etc/hostkeeper",
    "/etc/updater",
    "/var/lib/hostkeeper",
    "/var/lib/updater",
    "/etc/systemd/system",
    "/usr",
    "/boot",
    "/var/lib",
];
/// The steps of an update and the `uninstall` step, which `boot` may not name.
pub const STEP_NAMES: [&str; 6] = [
    "prepare",
    "cutover",
    "verify",
    "rollback",
    "cleanup",
    "uninstall",
];
/// The most bytes of the name of the `boot` step.
pub const MAX_BOOT_NAME: usize = 64;

/// A version of hostkeeper: `major.minor.patch`, three whole numbers, with no pre-release or
/// build suffix. Ordered by the three numbers in turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct HostkeeperVersion {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

/// Why a text is not a [`HostkeeperVersion`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0:?} is not major.minor.patch: three whole numbers without leading zeros or a suffix")]
pub struct BadHostkeeperVersion(pub String);

/// One number of a version: digits, and no leading zero but in `0` itself.
fn number(part: &str) -> Option<u64> {
    let digits = !part.is_empty() & part.bytes().all(|b| b.is_ascii_digit());
    let no_leading_zero = (part.len() == 1) | !part.starts_with('0');
    (digits & no_leading_zero)
        .then(|| part.parse().ok())
        .flatten()
}

impl HostkeeperVersion {
    pub fn parse(text: &str) -> Result<HostkeeperVersion, BadHostkeeperVersion> {
        let bad = || BadHostkeeperVersion(text.to_owned());
        let mut parts = text.split('.').map(number);
        let mut next = || parts.next().flatten().ok_or_else(bad);
        let version = HostkeeperVersion {
            major: next()?,
            minor: next()?,
            patch: next()?,
        };
        let rest = parts.next();
        rest.is_none().then_some(version).ok_or_else(bad)
    }
}

impl fmt::Display for HostkeeperVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The order a manifest is read for: the service and the version the daemon was told to install,
/// and the platform of the host (its own, [`Platform::current`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expected {
    pub name: ServiceName,
    pub version: Version,
    pub platform: Platform,
}

/// The kind of a refusal, as a step record reports it: the package is bad (`Release`), or the
/// host cannot take it yet (`Environment`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Environment,
    Release,
}

/// Why a manifest was refused. [`ManifestRefusal::class`] and [`ManifestRefusal::reason`] say
/// how a step record reports it; the text says what was wrong.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestRefusal {
    #[error("the manifest cannot be read: {why}")]
    Unreadable { why: String },
    /// `key` is dotted (`hook.timeout_secs.verify`).
    #[error("the manifest's {key} {why}")]
    Invalid { key: String, why: String },
    #[error("the package needs hostkeeper {needs}; this daemon is {running}")]
    DaemonTooOld {
        needs: HostkeeperVersion,
        running: HostkeeperVersion,
    },
}

impl ManifestRefusal {
    /// `Environment` for [`ManifestRefusal::DaemonTooOld`], `Release` for every other refusal.
    pub fn class(&self) -> Class {
        match self {
            ManifestRefusal::DaemonTooOld { .. } => Class::Environment,
            ManifestRefusal::Unreadable { .. } | ManifestRefusal::Invalid { .. } => Class::Release,
        }
    }

    /// The reason token: `daemon_too_old` for a daemon older than `min_hostkeeper`, the host's to
    /// fix, and `manifest_mismatch` for every refusal of the package itself (design section 5.5).
    pub fn reason(&self) -> &'static str {
        match self {
            ManifestRefusal::DaemonTooOld { .. } => "daemon_too_old",
            ManifestRefusal::Unreadable { .. } | ManifestRefusal::Invalid { .. } => {
                "manifest_mismatch"
            }
        }
    }
}

/// The bound of each step, in seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeouts {
    /// `None` is the unbounded form (`prepare = 0`).
    pub prepare: Option<NonZeroU64>,
    pub cutover: NonZeroU64,
    pub verify: NonZeroU64,
    pub rollback: NonZeroU64,
    pub cleanup: NonZeroU64,
    /// `None` when the manifest has no `uninstall` step.
    pub uninstall: Option<NonZeroU64>,
}

/// The `[hook]` table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hook {
    /// The program, relative to the version directory, and its leading arguments.
    pub command: Vec<String>,
    /// The step run from `current` at daemon start, if any.
    pub boot: Option<String>,
    pub verify_deadline_secs: u64,
    /// `None` means no ceiling.
    pub promotion_ceiling_secs: Option<NonZeroU64>,
    pub cleanup_retries: u32,
    pub timeout_secs: Timeouts,
}

/// One `[[files]]` entry: `source` under `files/` of the version directory, placed at `path`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub source: String,
    pub path: String,
}

/// A manifest that passed every load rule.
#[derive(Clone, Debug, PartialEq)]
pub struct Manifest {
    pub name: ServiceName,
    pub version: Version,
    /// The operating system and CPU family the package was built for.
    pub platform: Platform,
    pub min_hostkeeper: HostkeeperVersion,
    pub hook: Hook,
    /// The names of the files in `units/`, each the service's.
    pub unit_files: BTreeSet<String>,
    /// `[units] run`: each a unit file or an instance of a template among them.
    pub run: Vec<String>,
    /// `[health] url`, if any.
    pub health_url: Option<String>,
    pub files: Vec<FileEntry>,
    /// `[params]`: scalars only, no `updater.*` key.
    pub params: toml::Table,
}

/// Whether `name` may name a hook's additive boot step: 1 to [`MAX_BOOT_NAME`] bytes of
/// `[a-z0-9_]`, and none of [`STEP_NAMES`]. The rule of `[hook] boot`, and of a release
/// manifest's `hook.boot` (crate `hostkeeper-release`).
pub fn is_boot_step(name: &str) -> bool {
    let token = (1..=MAX_BOOT_NAME).contains(&name.len())
        & name
            .bytes()
            .all(|b| b.is_ascii_lowercase() | b.is_ascii_digit() | (b == b'_'));
    token & !STEP_NAMES.contains(&name)
}

/// Whether `host` is a loopback literal: the address `127.0.0.1`, or `::1` in brackets in any
/// spelling (`[::1]`, `[0:0:0:0:0:0:0:1]`).
fn is_loopback(host: &str) -> bool {
    let v4 = host.parse::<Ipv4Addr>() == Ok(Ipv4Addr::LOCALHOST);
    let v6 = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .is_some_and(|inner| inner.parse::<Ipv6Addr>() == Ok(Ipv6Addr::LOCALHOST));
    v4 | v6
}

/// Why a health URL is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HealthUrlError {
    /// It is not a URL of the grammar: the reason, as a phrase after the URL.
    #[error("{0}")]
    Syntax(&'static str),
    /// Another scheme than `http`, or a host that is not a loopback literal.
    #[error("is not an http:// URL to a loopback address (127.0.0.1 or [::1])")]
    NotLoopback,
    /// `{port}` in the port's place, and a run unit that is not an instance named by its port.
    #[error("has {{port}}, but the run unit {unit:?} is not an instance with a port number")]
    PortWithoutInstance {
        /// The first such unit.
        unit: String,
    },
}

/// The rule of a health URL, `[health] url` here and a release manifest's `health_url` (crate
/// `hostkeeper-release`): `http://` to a loopback literal (design section 5.8). A `{port}` in the
/// port's place needs every unit of `run` to be an instance whose name is its port; with no run
/// unit there is nothing to probe, and the URL passes.
pub fn check_health_url(service: &str, run: &[String], text: &str) -> Result<(), HealthUrlError> {
    let parts = url::parts(text, true).map_err(HealthUrlError::Syntax)?;
    if (parts.scheme != "http") | !is_loopback(parts.host) {
        return Err(HealthUrlError::NotLoopback);
    }
    let unit = run.iter().find(|unit| {
        UnitName::of(service, unit, false)
            .ok()
            .and_then(|name| name.port())
            .is_none()
    });
    match (parts.port == Some(url::PORT_PLACEHOLDER), unit) {
        (true, Some(unit)) => Err(HealthUrlError::PortWithoutInstance { unit: unit.clone() }),
        _ => Ok(()),
    }
}
