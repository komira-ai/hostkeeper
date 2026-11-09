//! The payload of a component (packages-repositories.md section 4.7, `stage` steps 5 and 6): its
//! `Bom`, as `lsbom -p fmug` lists it, and the expanded payload list the same paths with the
//! same kinds, modes and owners; every entry is root's, of group `wheel` or `admin`, with no
//! setuid, setgid or sticky bit, writable by neither group nor others, no device or fifo, and at
//! a path of the payload rule; every `run` label is the plist of its path in the payload, of that
//! `Label`, `Disabled`, whose program is a file of the payload.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use hostkeeper_launchd::Label;
use plist::Value;

use crate::host::{GID_ADMIN, GID_WHEEL, Kind, Meta};

/// The arguments of `lsbom` after which the `Bom`'s path comes: path, mode, uid and gid, one entry
/// a line, tab-separated.
pub const LSBOM_ARGS: [&str; 2] = ["-p", "fmug"];

/// The directories above a service's payload, which a payload may list as directories only.
pub const ABOVE: [&str; 3] = [
    "/Library",
    "/Library/Application Support",
    "/Library/LaunchDaemons",
];

/// Where a service's files go: `/Library/Application Support/<service>`.
pub fn service_dir(service: &str) -> String {
    format!("/Library/Application Support/{service}")
}

/// Where a `run` label's plist goes.
pub fn plist_path(label: &Label) -> String {
    format!("/Library/LaunchDaemons/{}.plist", label.as_str())
}

/// What the payload check found wrong; each is `manifest_invalid`.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PayloadError {
    #[error("the Bom's listing line {0:?} is not path, mode, uid and gid")]
    BadLine(String),
    #[error("the Bom lists {0} twice")]
    Twice(String),
    #[error("the Bom lists {0}, which the payload lacks")]
    NotInPayload(String),
    #[error("the payload holds {0}, which the Bom does not list")]
    NotInBom(String),
    #[error("{0} is {1:?} in the Bom and {2:?} in the payload")]
    Differs(String, Meta, Meta),
    #[error("{0} is owned by uid {1}, not root")]
    NotRoot(String, u32),
    #[error("{0} is of group {1}, neither wheel nor admin")]
    Group(String, u32),
    #[error("{0} has mode {1:o}: a setuid, setgid or sticky bit, or writable by group or others")]
    Mode(String, u32),
    #[error("{0} is a device, a fifo or a socket")]
    Special(String),
    #[error("{0} is outside {1}/ and the run labels' plists")]
    Outside(String, String),
    #[error("{0} must be a directory")]
    NotDir(String),
    #[error("the run label {0} has no plist {1} in the payload")]
    NoPlist(String, String),
    #[error("{0} is no property list of a job: {1}")]
    BadPlist(String, String),
    #[error("{0} has the Label {1:?}, not {2:?}")]
    Label(String, String, String),
    #[error("{0} is not Disabled")]
    NotDisabled(String),
    #[error("{0} names the program {1:?}, which is no file of the payload")]
    Program(String, String),
}

/// The `Bom`'s entries from `lsbom -p fmug`'s output, by logical path (`.` and `./x` as `/` and
/// `/x`; `/` itself is not listed).
pub fn parse_bom(listing: &str) -> Result<BTreeMap<String, Meta>, PayloadError> {
    let mut entries = BTreeMap::new();
    for line in listing.lines().filter(|line| !line.is_empty()) {
        let bad = || PayloadError::BadLine(line.into());
        let fields: Vec<&str> = line.split('\t').collect();
        let [path, mode, uid, gid] = fields[..] else {
            return Err(bad());
        };
        let path = match path {
            "." => continue,
            _ => path.strip_prefix("./").ok_or_else(bad)?,
        };
        let mode = u32::from_str_radix(mode, 8).map_err(|_| bad())?;
        let meta = Meta {
            kind: Kind::of_mode(mode),
            mode: mode & 0o7777,
            uid: uid.parse().map_err(|_| bad())?,
            gid: gid.parse().map_err(|_| bad())?,
        };
        let logical = format!("/{path}");
        if entries.insert(logical.clone(), meta).is_some() {
            return Err(PayloadError::Twice(logical));
        }
    }
    Ok(entries)
}

/// Checks the `Bom`'s entries against the expanded payload's and against the rules of an entry.
pub fn check(
    bom: &BTreeMap<String, Meta>,
    payload: &BTreeMap<String, Meta>,
    service: &str,
    run: &[Label],
) -> Result<(), PayloadError> {
    if let Some(path) = bom.keys().find(|path| !payload.contains_key(*path)) {
        return Err(PayloadError::NotInPayload(path.clone()));
    }
    if let Some(path) = payload.keys().find(|path| !bom.contains_key(*path)) {
        return Err(PayloadError::NotInBom(path.clone()));
    }
    let plists: BTreeSet<String> = run.iter().map(plist_path).collect();
    let home = service_dir(service);
    for (path, listed) in bom {
        let found = payload[path];
        if *listed != found {
            return Err(PayloadError::Differs(path.clone(), *listed, found));
        }
        entry(path, listed, &home, &plists)?;
    }
    Ok(())
}

fn entry(path: &str, meta: &Meta, home: &str, plists: &BTreeSet<String>) -> Result<(), PayloadError> {
    if meta.uid != 0 {
        return Err(PayloadError::NotRoot(path.into(), meta.uid));
    }
    if meta.gid != GID_WHEEL && meta.gid != GID_ADMIN {
        return Err(PayloadError::Group(path.into(), meta.gid));
    }
    if meta.mode & 0o7022 != 0 {
        return Err(PayloadError::Mode(path.into(), meta.mode));
    }
    if meta.kind == Kind::Other {
        return Err(PayloadError::Special(path.into()));
    }
    let under_home = path == home || path.starts_with(&format!("{home}/"));
    if ABOVE.contains(&path) {
        return match meta.kind {
            Kind::Dir => Ok(()),
            _ => Err(PayloadError::NotDir(path.into())),
        };
    }
    if under_home || (plists.contains(path) && meta.kind == Kind::File) {
        return Ok(());
    }
    Err(PayloadError::Outside(path.into(), home.into()))
}

/// Checks each `run` label's plist in the expanded payload at `payload` (which stands for `/`):
/// its `Label`, `Disabled` true, and a program (`Program`, else the first of `ProgramArguments`)
/// that is a file of `bom`.
pub fn check_jobs(
    payload: &Path,
    bom: &BTreeMap<String, Meta>,
    run: &[Label],
) -> Result<(), PayloadError> {
    for label in run {
        let path = plist_path(label);
        if bom.get(&path).map(|meta| meta.kind) != Some(Kind::File) {
            return Err(PayloadError::NoPlist(label.as_str().into(), path));
        }
        let real = payload.join(path.trim_start_matches('/'));
        let job = Value::from_file(&real).map_err(|e| PayloadError::BadPlist(path.clone(), e.to_string()))?;
        let job = job
            .as_dictionary()
            .ok_or_else(|| PayloadError::BadPlist(path.clone(), "not a dictionary".into()))?;
        let found = job.get("Label").and_then(Value::as_string).unwrap_or_default();
        if found != label.as_str() {
            return Err(PayloadError::Label(path, found.into(), label.as_str().into()));
        }
        if job.get("Disabled").and_then(Value::as_boolean) != Some(true) {
            return Err(PayloadError::NotDisabled(path));
        }
        let program = program(job).unwrap_or_default();
        if bom.get(program).map(|meta| meta.kind) != Some(Kind::File) {
            return Err(PayloadError::Program(path, program.into()));
        }
    }
    Ok(())
}

/// A job's program: `Program`, or else the first of `ProgramArguments`.
pub fn program(job: &plist::Dictionary) -> Option<&str> {
    job.get("Program").and_then(Value::as_string).or_else(|| {
        job.get("ProgramArguments")
            .and_then(Value::as_array)
            .and_then(|args| args.first())
            .and_then(Value::as_string)
    })
}

/// The labels of the jobs whose plists a payload tree (standing for `/`) holds in
/// `/Library/LaunchDaemons`: the `run` labels of the version staged from it.
pub fn labels_of(tree: &BTreeMap<String, Meta>) -> Vec<Label> {
    tree.keys()
        .filter_map(|path| path.strip_prefix("/Library/LaunchDaemons/"))
        .filter_map(|name| name.strip_suffix(".plist"))
        .filter_map(|label| Label::parse(label).ok())
        .collect()
}

#[cfg(test)]
#[path = "payload_tests.rs"]
mod tests;
