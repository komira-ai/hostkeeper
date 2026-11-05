//! `release package`: hostkeeper's own package, the archive of design sections 4.7 and 5.5,
//! built the same bytes every time for one commit and version.
//!
//! The archive is a ustar tar, compressed by [`compress`], holding exactly these entries in this
//! order: the directory `bin/`, the file `bin/hostkeeper` (mode 0555), the file `manifest.toml`
//! (0444), the directory `units/` and the file `units/hostkeeper.service` (0444). Directories are
//! 0555. Every header has uid and gid 0, empty user and group names and, as its time, the
//! committer time of the commit built, so the bytes depend on the inputs alone. The archive is
//! written to the name [`package_name`] gives, with beside it a file of the same name plus
//! [`DIGEST_SUFFIX`] holding `<sha256>  <package name>` and a line break.
//!
//! Before the archive is built, the unit is checked (design section 4.7: a unit names package
//! files only through `current`) and the entry list is checked against the rules of section 5.5
//! that apply to a list this module builds ([`check_entries`]).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::Error;
use super::contract::{Commit, DIGEST_SUFFIX, Platform, Version, package_name};
use super::ports::GitHistory;

/// Why there is no macos-arm64 package yet: its units are launchd plists, which the macOS
/// daemon (design build item B18) defines.
const MACOS_LATER: &str = "the macos-arm64 package waits for the macOS daemon: its units are launchd plists, not the systemd unit this verb packages";

/// What `release package` builds a package from.
pub(crate) struct Request {
    pub(crate) platform: Platform,
    pub(crate) version: Version,
    pub(crate) commit: Commit,
    /// The hostkeeper binary of the platform.
    pub(crate) binary: PathBuf,
    /// The systemd unit.
    pub(crate) unit: PathBuf,
    /// The directory the package and its digest file are written to.
    pub(crate) out: PathBuf,
}

/// The `[hook]` tables of the manifest: the one place the hook values are set. The manifest
/// loader requires a hook with bounded steps, so hostkeeper's own package carries one; the daemon
/// does not run it for its own package, because hostkeeper's self-update is compiled into the
/// daemon (design section 6.6). The manifest says so in a comment.
const HOOK: &str = r#"# The daemon does not run this hook for its own package: hostkeeper's self-update is
# compiled into the daemon. The table is here because every manifest has a bounded hook.
[hook]
command = ["bin/hostkeeper", "update-hook"]
verify_deadline_secs = 600

[hook.timeout_secs]
prepare = 300
cutover = 300
verify = 300
rollback = 300
cleanup = 300
"#;

/// The unit file the package holds, under `units/`.
const UNIT_FILE: &str = "hostkeeper.service";

/// Where the host links the package: every unit names package files through `current`.
const PACKAGES: &str = "/var/lib/hostkeeper/packages/";

/// What must follow [`PACKAGES`] wherever a unit names it.
const CURRENT: &str = "hostkeeper/current/";

/// The binary the unit must start.
const BINARY_PATH: &str = "/var/lib/hostkeeper/packages/hostkeeper/current/bin/hostkeeper";

/// The top-level names a package may hold (design section 5.5).
const TOP_LEVEL: [&str; 4] = ["manifest.toml", "bin", "units", "files"];

/// The modes an entry may have: executable or not, never writable.
const MODES: [u32; 2] = [0o555, 0o444];

/// One entry of the archive.
struct Entry {
    /// The path; a directory's ends with `/`.
    name: String,
    directory: bool,
    mode: u32,
    data: Vec<u8>,
}

impl Entry {
    fn file(name: &str, mode: u32, data: Vec<u8>) -> Entry {
        Entry {
            name: name.to_owned(),
            directory: false,
            mode,
            data,
        }
    }

    fn dir(name: &str) -> Entry {
        Entry {
            name: name.to_owned(),
            directory: true,
            mode: 0o555,
            data: Vec::new(),
        }
    }
}

/// The manifest of hostkeeper's package for `version` built for `platform`: it needs a daemon of
/// its own version, and names the platform the daemon checks against its own.
fn manifest(version: &Version, platform: Platform) -> String {
    let platform = platform.name();
    format!(
        "schema = 1\nname = \"hostkeeper\"\nversion = \"{version}\"\nplatform = \"{platform}\"\nmin_hostkeeper = \"{version}\"\n\n{HOOK}\n[units]\nrun = [\"{UNIT_FILE}\"]\n"
    )
}

/// The one function that chooses the compression of the package. It goes with
/// `contract::PACKAGE_SUFFIX`: a change of one is a change of the other.
pub(crate) fn compress(tar: &[u8]) -> Vec<u8> {
    ruzstd::encoding::compress_to_vec(tar, ruzstd::encoding::CompressionLevel::Fastest)
}

/// Builds the package of `request` and its digest file; the lines to print.
pub(crate) fn package(git: &dyn GitHistory, request: &Request) -> Result<Vec<String>, Error> {
    if request.platform == Platform::MacosArm64 {
        return Err(Error::Failed(MACOS_LATER.to_owned()));
    }
    let binary = read(&request.binary)?;
    let unit = read(&request.unit)?;
    check_unit(&request.unit, &unit)?;
    let entries = entries(&request.version, request.platform, binary, unit);
    check_entries(&entries)?;
    let mtime = git.commit_time(&request.commit)?;
    let bytes = compress(&archive(&entries, mtime)?);
    let name = package_name(&request.version, request.platform);
    let hex = hex::encode(Sha256::digest(&bytes));
    std::fs::create_dir_all(&request.out)
        .map_err(Error::io(format!("create {}", request.out.display())))?;
    write(&request.out.join(&name), &bytes)?;
    let digest = format!("{hex}  {name}\n");
    write(
        &request.out.join(format!("{name}{DIGEST_SUFFIX}")),
        digest.as_bytes(),
    )?;
    Ok(vec![
        format!("{name}: sha256 {hex}"),
        format!("sha256={hex}"),
    ])
}

fn read(path: &Path) -> Result<Vec<u8>, Error> {
    std::fs::read(path).map_err(Error::io(format!("read {}", path.display())))
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    std::fs::write(path, bytes).map_err(Error::io(format!("write {}", path.display())))
}

/// Refuses a unit that is not UTF-8 text, does not start the package's binary through
/// `current`, or names a package file by any other path (design section 4.7).
fn check_unit(path: &Path, unit: &[u8]) -> Result<(), Error> {
    let text = std::str::from_utf8(unit)
        .map_err(|_| Error::Failed(format!("{} is not UTF-8 text", path.display())))?;
    let starts_it = text
        .lines()
        .any(|l| l.starts_with("ExecStart") & l.contains(BINARY_PATH));
    if !starts_it {
        return Err(Error::Failed(format!(
            "{} has no ExecStart line naming {BINARY_PATH}",
            path.display()
        )));
    }
    if text
        .split(PACKAGES)
        .skip(1)
        .any(|rest| !rest.starts_with(CURRENT))
    {
        return Err(Error::Failed(format!(
            "{} names a package file by a path other than {PACKAGES}{CURRENT}",
            path.display()
        )));
    }
    Ok(())
}

/// The entries of the package, sorted by name.
fn entries(version: &Version, platform: Platform, binary: Vec<u8>, unit: Vec<u8>) -> Vec<Entry> {
    let mut entries = vec![
        Entry::dir("bin/"),
        Entry::file("bin/hostkeeper", 0o555, binary),
        Entry::file(
            "manifest.toml",
            0o444,
            manifest(version, platform).into_bytes(),
        ),
        Entry::dir("units/"),
        Entry::file(&format!("units/{UNIT_FILE}"), 0o444, unit),
    ];
    entries.sort_by(|a, b| a.name.as_str().cmp(b.name.as_str()));
    entries
}

/// Why `path` (a name without its closing `/`) cannot be an entry of a package, if it cannot.
fn path_problem(path: &str) -> Option<&'static str> {
    if path.starts_with('/') {
        Some("is an absolute path")
    } else if path.split('/').any(str::is_empty) {
        Some("has an empty component")
    } else if path.split('/').any(|c| c == "..") {
        Some("has a .. component")
    } else {
        None
    }
}

/// The shape rules of design section 5.5 that apply to a list this module builds: top-level
/// names only `manifest.toml`, `bin/`, `units/` and `files/`; no absolute path, no `..`, no empty
/// component; no duplicate name and none equal after ASCII case folding; modes exactly 0555 or
/// 0444. (Links and devices cannot be listed: an [`Entry`] is a file or a directory.)
fn check_entries(entries: &[Entry]) -> Result<(), Error> {
    let mut seen = BTreeSet::new();
    let mut problems = Vec::new();
    for entry in entries {
        let name = &entry.name;
        let path = name.trim_end_matches('/');
        let top = path.split_once('/').map_or(path, |(top, _)| top);
        if let Some(why) = path_problem(path) {
            problems.push(format!("{name}: {why}"));
        } else if !TOP_LEVEL.contains(&top) {
            problems.push(format!(
                "{name}: top-level name is not manifest.toml, bin/, units/ or files/"
            ));
        }
        if !seen.insert(path.to_ascii_lowercase()) {
            problems.push(format!(
                "{name}: the same name as an earlier entry, ignoring ASCII case"
            ));
        }
        if !MODES.contains(&entry.mode) {
            problems.push(format!("{name}: mode {:o} is not 555 or 444", entry.mode));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(Error::Refused(problems))
    }
}

/// The tar of `entries`, every header owned by root, with no names, and dated `mtime`.
fn archive(entries: &[Entry], mtime: u64) -> Result<Vec<u8>, Error> {
    let what = "build the package archive";
    let mut builder = tar::Builder::new(Vec::new());
    for entry in entries {
        let mut header = tar::Header::new_ustar();
        header.set_entry_type(if entry.directory {
            tar::EntryType::Directory
        } else {
            tar::EntryType::Regular
        });
        header.set_mode(entry.mode);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(mtime);
        header.set_size(entry.data.len() as u64);
        builder
            .append_data(&mut header, &entry.name, entry.data.as_slice())
            .map_err(Error::io(what))?;
    }
    builder.into_inner().map_err(Error::io(what))
}

#[cfg(test)]
mod tests;
