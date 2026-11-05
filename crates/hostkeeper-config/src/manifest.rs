//! The manifest: `manifest.toml` at the top of a package, the service's lifecycle
//! (docs/design/hostkeeper-design.md, section 4.7).
//!
//! ```toml
//! schema         = 1
//! name           = "tierstore"
//! version        = "1.5.0"
//! min_hostkeeper = "0.4.0"
//!
//! [hook]
//! command = ["bin/tierstore", "update-hook"]
//! boot    = "restore"
//! verify_deadline_secs   = 1600
//! promotion_ceiling_secs = 3600
//! cleanup_retries        = 3
//! [hook.timeout_secs]
//! prepare = 0
//! cutover = 300
//! verify  = 1000
//! rollback = 600
//! cleanup = 1300
//! uninstall = 300
//!
//! [units]
//! run = ["tierstore@9301.service", "tierstore@9302.service"]
//!
//! [health]
//! url = "http://127.0.0.1:{port}/healthz"
//!
//! [[files]]
//! source = "files/tierstore.conf"
//! path   = "/etc/tierstore/tierstore.conf"
//!
//! [params]
//! ```
//!
//! [`Manifest::load`] applies the load rules of section 4.7 in this order: the file, the syntax
//! and the schema, every key against the tables that may hold it, `name` and `version` against
//! the order, every other rule, and `min_hostkeeper` last, so a malformed manifest is never
//! reported as one the daemon is too old for. There is no `user`, `capabilities`, write-path,
//! signer, release-URL or `min_version` key: each is an unknown key. Which paths another package
//! owns, linking and the contents of unit files are not judged here.

use std::collections::BTreeSet;
use std::fmt;
use std::fs::OpenOptions;
use std::io::{self, Read};
use std::net::{Ipv4Addr, Ipv6Addr};
use std::num::NonZeroU64;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use crate::names::{
    PLIST_SUFFIX, ServiceName, UNIT_SUFFIXES, UnitError, UnitName, Version, check_path,
    check_relative, is_within, launchd_label, plist_label,
};
use crate::table::{self, Fields, Problem, Shape};
use crate::url;

/// The name of the manifest in a version directory.
pub const MANIFEST_FILE: &str = "manifest.toml";
/// The most bytes of a manifest.
pub const MAX_MANIFEST_BYTES: u64 = 65_536;
/// The one schema this loader reads.
pub const SCHEMA: i64 = 1;
/// The default of `[hook] cleanup_retries`.
pub const DEFAULT_CLEANUP_RETRIES: u32 = 3;
/// The directories a `[[files]] path` may not be, or lie below: hostkeeper's and the updater's
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

const TIMEOUTS: &[(&str, Shape)] = &[
    ("prepare", Shape::Value),
    ("cutover", Shape::Value),
    ("verify", Shape::Value),
    ("rollback", Shape::Value),
    ("cleanup", Shape::Value),
    ("uninstall", Shape::Value),
];

const HOOK: &[(&str, Shape)] = &[
    ("command", Shape::Value),
    ("boot", Shape::Value),
    ("verify_deadline_secs", Shape::Value),
    ("promotion_ceiling_secs", Shape::Value),
    ("cleanup_retries", Shape::Value),
    ("timeout_secs", Shape::Table(TIMEOUTS)),
];

const UNITS: &[(&str, Shape)] = &[("run", Shape::Value)];
const HEALTH: &[(&str, Shape)] = &[("url", Shape::Value)];
const FILES: &[(&str, Shape)] = &[("source", Shape::Value), ("path", Shape::Value)];

const TOP: &[(&str, Shape)] = &[
    ("schema", Shape::Value),
    ("name", Shape::Value),
    ("version", Shape::Value),
    ("min_hostkeeper", Shape::Value),
    ("hook", Shape::Table(HOOK)),
    ("units", Shape::Table(UNITS)),
    ("health", Shape::Table(HEALTH)),
    ("files", Shape::Tables(FILES)),
    ("params", Shape::Open),
];

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

/// The order a manifest is read for: the service and the version the daemon was told to install.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expected {
    pub name: ServiceName,
    pub version: Version,
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
    #[error("the manifest is larger than {MAX_MANIFEST_BYTES} bytes")]
    TooLarge,
    #[error("the manifest is not TOML: line {line}: {message}")]
    Syntax { line: usize, message: String },
    #[error("the manifest has schema {found}; this daemon reads schema {SCHEMA} only")]
    Schema { found: i64 },
    /// `table` is `""` for the top level, and `files[0]` for the first `[[files]]` entry.
    #[error("the manifest has the unknown key {key:?} in the table {table:?}")]
    UnknownKey { table: String, key: String },
    /// `key` is dotted (`hook.timeout_secs.verify`).
    #[error("the manifest's {key} {why}")]
    Invalid { key: String, why: String },
    #[error("the manifest is for the service {found:?}, not {expected:?}")]
    NameMismatch { expected: String, found: String },
    #[error("the manifest is for the version {found:?}, not {expected:?}")]
    VersionMismatch { expected: String, found: String },
    #[error("hook.timeout_secs.{step} is 0; only prepare may be unbounded")]
    ZeroBound { step: &'static str },
    #[error(
        "hook.verify_deadline_secs {deadline} is less than the verify ({verify}) and rollback ({rollback}) bounds together"
    )]
    DeadlineTooShort {
        deadline: u64,
        verify: u64,
        rollback: u64,
    },
    #[error("the [[files]] source {entry:?} {why}")]
    FileSource { entry: String, why: String },
    #[error("the [[files]] path {path:?} {why}")]
    FilePath { path: String, why: String },
    #[error(
        "the [[files]] path {path:?} is {prefix} or below it, where a package may not place a file"
    )]
    ReservedPath { path: String, prefix: &'static str },
    #[error("two [[files]] entries have the path {path:?}")]
    DuplicatePath { path: String },
    #[error("the unit file units/{file} {why}")]
    UnitFile { file: String, why: UnitError },
    #[error("units/{file} is not a regular file (a link, a directory or another kind)")]
    UnitNotAFile { file: String },
    #[error("the [units] run entry {unit:?} {why}")]
    RunUnit { unit: String, why: String },
    #[error(
        "the health URL has {{port}}, but the run unit {unit:?} is not an instance with a port number"
    )]
    PortWithoutInstance { unit: String },
    #[error("the params key {key:?} is in the namespace `updater.`, which only the daemon writes")]
    ReservedParam { key: String },
    #[error("the params key {key:?} is not a string, an integer, a float or a boolean")]
    NonScalarParam { key: String },
    #[error("the package needs hostkeeper {needs}; this daemon is {running}")]
    DaemonTooOld {
        needs: HostkeeperVersion,
        running: HostkeeperVersion,
    },
}

impl ManifestRefusal {
    /// The class and the reason token of each refusal. Every refusal of the package itself is
    /// `manifest_mismatch` (design section 5.5); a daemon older than `min_hostkeeper` is the
    /// host's to fix, `daemon_too_old`.
    fn kind(&self) -> (Class, &'static str) {
        const BAD: (Class, &str) = (Class::Release, "manifest_mismatch");
        match self {
            ManifestRefusal::DaemonTooOld { .. } => (Class::Environment, "daemon_too_old"),
            ManifestRefusal::Unreadable { .. }
            | ManifestRefusal::TooLarge
            | ManifestRefusal::Syntax { .. }
            | ManifestRefusal::Schema { .. }
            | ManifestRefusal::UnknownKey { .. }
            | ManifestRefusal::Invalid { .. }
            | ManifestRefusal::NameMismatch { .. }
            | ManifestRefusal::VersionMismatch { .. }
            | ManifestRefusal::ZeroBound { .. }
            | ManifestRefusal::DeadlineTooShort { .. }
            | ManifestRefusal::FileSource { .. }
            | ManifestRefusal::FilePath { .. }
            | ManifestRefusal::ReservedPath { .. }
            | ManifestRefusal::DuplicatePath { .. }
            | ManifestRefusal::UnitFile { .. }
            | ManifestRefusal::UnitNotAFile { .. }
            | ManifestRefusal::RunUnit { .. }
            | ManifestRefusal::PortWithoutInstance { .. }
            | ManifestRefusal::ReservedParam { .. }
            | ManifestRefusal::NonScalarParam { .. } => BAD,
        }
    }

    /// `Environment` for [`ManifestRefusal::DaemonTooOld`], `Release` for every other refusal.
    pub fn class(&self) -> Class {
        self.kind().0
    }

    /// The reason token: `daemon_too_old` or `manifest_mismatch`.
    pub fn reason(&self) -> &'static str {
        self.kind().1
    }
}

impl From<Problem> for ManifestRefusal {
    fn from(problem: Problem) -> ManifestRefusal {
        match problem {
            Problem::Unknown { table, key } => ManifestRefusal::UnknownKey { table, key },
            Problem::Invalid { key, why } => ManifestRefusal::Invalid { key, why },
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

/// Reads `path` as text: a regular file, not a link, at most [`MAX_MANIFEST_BYTES`]. The file is
/// opened without following a link or waiting on a pipe, and the kind is read on the open file.
fn read_manifest(path: &Path) -> Result<String, ManifestRefusal> {
    let unreadable = |why: String| ManifestRefusal::Unreadable { why };
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| unreadable(format!("{}: {e}", path.display())))?;
    let regular = file.metadata().is_ok_and(|meta| meta.is_file());
    if !regular {
        return Err(unreadable(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    file.take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| unreadable(format!("{}: {e}", path.display())))?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(ManifestRefusal::TooLarge);
    }
    String::from_utf8(bytes).map_err(|_| unreadable("it is not UTF-8 text".to_owned()))
}

fn bound(fields: &Fields<'_>, step: &'static str) -> Result<NonZeroU64, ManifestRefusal> {
    NonZeroU64::new(fields.number(step)?).ok_or(ManifestRefusal::ZeroBound { step })
}

fn timeouts(hook: &Fields<'_>) -> Result<Timeouts, ManifestRefusal> {
    let table = hook
        .table("timeout_secs")?
        .ok_or_else(|| hook.invalid("timeout_secs", "is missing"))?;
    let fields = Fields::new("hook.timeout_secs", table);
    let uninstall = fields
        .get("uninstall")
        .map(|_| bound(&fields, "uninstall"))
        .transpose()?;
    Ok(Timeouts {
        prepare: NonZeroU64::new(fields.number("prepare")?),
        cutover: bound(&fields, "cutover")?,
        verify: bound(&fields, "verify")?,
        rollback: bound(&fields, "rollback")?,
        cleanup: bound(&fields, "cleanup")?,
        uninstall,
    })
}

/// The `boot` step: a reason-token-shaped name that is none of [`STEP_NAMES`].
fn boot(hook: &Fields<'_>) -> Result<Option<String>, ManifestRefusal> {
    let Some(name) = hook.opt_string("boot")? else {
        return Ok(None);
    };
    let token = (1..=MAX_BOOT_NAME).contains(&name.len())
        & name
            .bytes()
            .all(|b| b.is_ascii_lowercase() | b.is_ascii_digit() | (b == b'_'));
    if !token | STEP_NAMES.contains(&name) {
        return Err(hook
            .invalid(
                "boot",
                format!(
                    "{name:?} is not the name of an additive step: 1 to {MAX_BOOT_NAME} of a-z, 0-9 and `_`, and none of {}",
                    STEP_NAMES.join(", ")
                ),
            )
            .into());
    }
    Ok(Some(name.to_owned()))
}

fn hook(top: &Fields<'_>) -> Result<Hook, ManifestRefusal> {
    let table = top
        .table("hook")?
        .ok_or_else(|| top.invalid("hook", "is missing"))?;
    let fields = Fields::new("hook", table);
    let command = fields.strings("command")?;
    let program = command
        .first()
        .ok_or_else(|| fields.invalid("command", "is empty"))?;
    check_relative(program).map_err(|why| {
        fields.invalid(
            "command",
            format!("names the program {program:?}, which {why}; it is a path relative to the version directory"),
        )
    })?;
    let boot = boot(&fields)?;
    let verify_deadline_secs = fields.number("verify_deadline_secs")?;
    let promotion_ceiling_secs = fields
        .get("promotion_ceiling_secs")
        .map(|_| {
            fields
                .number("promotion_ceiling_secs")
                .map(NonZeroU64::new)?
                .ok_or_else(|| {
                    fields.invalid(
                        "promotion_ceiling_secs",
                        "is 0; leave it out for no ceiling",
                    )
                })
        })
        .transpose()?;
    let retries = fields.number_or("cleanup_retries", u64::from(DEFAULT_CLEANUP_RETRIES))?;
    let cleanup_retries = u32::try_from(retries)
        .map_err(|_| fields.invalid("cleanup_retries", "is larger than 4294967295"))?;
    let timeout_secs = timeouts(&fields)?;
    let (verify, rollback) = (timeout_secs.verify.get(), timeout_secs.rollback.get());
    let enough = verify
        .checked_add(rollback)
        .is_some_and(|needed| verify_deadline_secs >= needed);
    if !enough {
        return Err(ManifestRefusal::DeadlineTooShort {
            deadline: verify_deadline_secs,
            verify,
            rollback,
        });
    }
    Ok(Hook {
        command: command.into_iter().map(str::to_owned).collect(),
        boot,
        verify_deadline_secs,
        promotion_ceiling_secs,
        cleanup_retries,
        timeout_secs,
    })
}

/// The names of the files in `units/`, each a regular file (not a link, a directory or a pipe)
/// whose name is `service`'s; none when there is no `units/`.
fn unit_files(version_dir: &Path, service: &str) -> Result<BTreeSet<String>, ManifestRefusal> {
    let dir = version_dir.join("units");
    let listed = std::fs::read_dir(&dir).and_then(|entries| {
        entries
            .map(|entry| entry.and_then(|entry| Ok((entry.file_name(), entry.file_type()?))))
            .collect::<io::Result<Vec<_>>>()
    });
    let names = match listed {
        Ok(names) => names,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            return Err(ManifestRefusal::Unreadable {
                why: format!("{}: {e}", dir.display()),
            });
        }
    };
    let mut files = BTreeSet::new();
    for (name, kind) in names {
        let file = name.to_string_lossy().into_owned();
        let checked = if file.ends_with(PLIST_SUFFIX) {
            plist_label(service, &file).map(|_| ())
        } else {
            UnitName::of(service, &file, true).map(|_| ())
        };
        checked.map_err(|why| ManifestRefusal::UnitFile {
            file: file.clone(),
            why,
        })?;
        // The kind of the entry itself: a link is not followed.
        if !kind.is_file() {
            return Err(ManifestRefusal::UnitNotAFile { file });
        }
        files.insert(file);
    }
    // A package carries systemd units or launchd plists, never both.
    let (plists, systemd): (Vec<&String>, Vec<&String>) =
        files.iter().partition(|file| file.ends_with(PLIST_SUFFIX));
    if let Some(file) = plists.first().filter(|_| !systemd.is_empty()) {
        return Err(ManifestRefusal::UnitFile {
            file: (*file).clone(),
            why: UnitError::MixedKinds,
        });
    }
    Ok(files)
}

/// `[units] run`: each entry is a unit of the service that is a unit file, or an instance of a
/// template that is one; an entry without a systemd suffix is a launchd label whose
/// `<label>.plist` is a file of `units/`.
fn run_units(
    top: &Fields<'_>,
    service: &str,
    unit_files: &BTreeSet<String>,
) -> Result<Vec<String>, ManifestRefusal> {
    let Some(table) = top.table("units")? else {
        return Ok(Vec::new());
    };
    let run = Fields::new("units", table).strings("run")?;
    for unit in &run {
        let refuse = |why: String| ManifestRefusal::RunUnit {
            unit: (*unit).to_owned(),
            why,
        };
        let linked = if UNIT_SUFFIXES.iter().any(|suffix| unit.ends_with(suffix)) {
            let name = UnitName::of(service, unit, false).map_err(|why| refuse(why.to_string()))?;
            unit_files.contains(*unit)
                | name
                    .template()
                    .is_some_and(|template| unit_files.contains(&template))
        } else {
            let label = launchd_label(service, unit).map_err(|why| refuse(why.to_string()))?;
            unit_files.contains(&format!("{label}{PLIST_SUFFIX}"))
        };
        if !linked {
            return Err(refuse(
                "is neither a file of units/ nor an instance of a template there".to_owned(),
            ));
        }
    }
    Ok(run.into_iter().map(str::to_owned).collect())
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

/// `[health] url`: `http://` to a loopback literal (design section 5.8). A `{port}` in the port's
/// place needs every run unit to be an instance whose name is its port; with no run unit there is
/// nothing to probe, and the URL loads.
fn health(
    top: &Fields<'_>,
    service: &str,
    run: &[String],
) -> Result<Option<String>, ManifestRefusal> {
    let Some(table) = top.table("health")? else {
        return Ok(None);
    };
    let fields = Fields::new("health", table);
    let text = fields.string("url")?;
    let parts = url::parts(text, true).map_err(|why| fields.invalid("url", why))?;
    if (parts.scheme != "http") | !is_loopback(parts.host) {
        return Err(fields
            .invalid(
                "url",
                "is not an http:// URL to a loopback address (127.0.0.1 or [::1])",
            )
            .into());
    }
    if parts.port == Some(url::PORT_PLACEHOLDER)
        && let Some(unit) = run.iter().find(|unit| {
            UnitName::of(service, unit, false)
                .ok()
                .and_then(|name| name.port())
                .is_none()
        })
    {
        return Err(ManifestRefusal::PortWithoutInstance { unit: unit.clone() });
    }
    Ok(Some(text.to_owned()))
}

/// A `[[files]] source`: `files/<path>` in the path grammar, present in the version directory
/// as a regular file or a directory, with no link on the way.
fn check_source(version_dir: &Path, source: &str) -> Result<(), ManifestRefusal> {
    let refuse = |why: String| ManifestRefusal::FileSource {
        entry: source.to_owned(),
        why,
    };
    check_relative(source).map_err(|why| refuse(why.to_string()))?;
    if !source.starts_with("files/") {
        return Err(refuse("is not below files/".to_owned()));
    }
    let mut path = version_dir.to_path_buf();
    let mut last = None;
    for part in source.split('/') {
        path.push(part);
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|e| refuse(format!("is not in the package: {e}")))?;
        if meta.file_type().is_symlink() {
            return Err(refuse(format!(
                "passes through the link {}",
                path.display()
            )));
        }
        last = Some(meta.file_type());
    }
    let kind_ok = last.is_some_and(|kind| kind.is_file() | kind.is_dir());
    if !kind_ok {
        return Err(refuse("is not a regular file or a directory".to_owned()));
    }
    Ok(())
}

/// A `[[files]] path`: absolute, in the path grammar, and none of [`REFUSED_PREFIXES`] or below
/// one, compared on whole components.
fn check_target(path: &str) -> Result<(), ManifestRefusal> {
    check_path(path).map_err(|why| ManifestRefusal::FilePath {
        path: path.to_owned(),
        why: why.to_string(),
    })?;
    match REFUSED_PREFIXES
        .iter()
        .find(|prefix| is_within(path, prefix))
    {
        Some(prefix) => Err(ManifestRefusal::ReservedPath {
            path: path.to_owned(),
            prefix,
        }),
        None => Ok(()),
    }
}

fn files(top: &Fields<'_>, version_dir: &Path) -> Result<Vec<FileEntry>, ManifestRefusal> {
    let mut seen = BTreeSet::new();
    let mut entries = Vec::new();
    for (at, table) in top.tables("files")?.into_iter().enumerate() {
        let name = format!("files[{at}]");
        let fields = Fields::new(&name, table);
        let source = fields.string("source")?;
        let path = fields.string("path")?;
        check_source(version_dir, source)?;
        check_target(path)?;
        if !seen.insert(path.to_owned()) {
            return Err(ManifestRefusal::DuplicatePath {
                path: path.to_owned(),
            });
        }
        entries.push(FileEntry {
            source: source.to_owned(),
            path: path.to_owned(),
        });
    }
    Ok(entries)
}

/// `[params]`: no key in the namespace `updater.` (a bare `updater.x` is a table `updater`), and
/// scalars only.
fn params(top: &Fields<'_>) -> Result<toml::Table, ManifestRefusal> {
    let Some(table) = top.table("params")? else {
        return Ok(toml::Table::new());
    };
    let refusal = table.iter().find_map(|(key, value)| {
        let reserved = (key == "updater") | key.starts_with("updater.");
        let scalar = matches!(
            value,
            toml::Value::String(_)
                | toml::Value::Integer(_)
                | toml::Value::Float(_)
                | toml::Value::Boolean(_)
        );
        let key = key.clone();
        match (reserved, scalar) {
            (true, _) => Some(ManifestRefusal::ReservedParam { key }),
            (false, false) => Some(ManifestRefusal::NonScalarParam { key }),
            (false, true) => None,
        }
    });
    refusal.map_or_else(|| Ok(table.clone()), Err)
}

impl Manifest {
    /// Reads `<version_dir>/manifest.toml` for the order `expected` and applies every load rule
    /// of design section 4.7, `min_hostkeeper` against `running` last. A manifest the running
    /// daemon is too old for is [`ManifestRefusal::DaemonTooOld`], class `Environment`; every
    /// other refusal is class `Release`. The ownership of the version directory is not judged
    /// here.
    pub fn load(
        version_dir: &Path,
        expected: &Expected,
        running: &HostkeeperVersion,
    ) -> Result<Manifest, ManifestRefusal> {
        let text = read_manifest(&version_dir.join(MANIFEST_FILE))?;
        let document = table::parse(&text)
            .map_err(|(line, message)| ManifestRefusal::Syntax { line, message })?;
        let top = Fields::new("", &document);
        let schema = top.integer("schema")?;
        if schema != SCHEMA {
            return Err(ManifestRefusal::Schema { found: schema });
        }
        if let Some(problem) = table::unknown_key("", &document, TOP) {
            return Err(problem.into());
        }
        let name = top.string("name")?;
        if name != expected.name.as_str() {
            return Err(ManifestRefusal::NameMismatch {
                expected: expected.name.to_string(),
                found: name.to_owned(),
            });
        }
        let version = top.string("version")?;
        if version != expected.version.as_str() {
            return Err(ManifestRefusal::VersionMismatch {
                expected: expected.version.to_string(),
                found: version.to_owned(),
            });
        }
        let service = expected.name.as_str();
        let hook = hook(&top)?;
        let unit_files = unit_files(version_dir, service)?;
        let run = run_units(&top, service, &unit_files)?;
        let health_url = health(&top, service, &run)?;
        let files = files(&top, version_dir)?;
        let params = params(&top)?;
        let floor = top.string("min_hostkeeper")?;
        let min_hostkeeper = HostkeeperVersion::parse(floor)
            .map_err(|why| top.invalid("min_hostkeeper", why.to_string()))?;
        if min_hostkeeper > *running {
            return Err(ManifestRefusal::DaemonTooOld {
                needs: min_hostkeeper,
                running: *running,
            });
        }
        Ok(Manifest {
            name: expected.name.clone(),
            version: expected.version.clone(),
            min_hostkeeper,
            hook,
            unit_files,
            run,
            health_url,
            files,
            params,
        })
    }
}
