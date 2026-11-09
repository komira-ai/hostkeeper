//! `release check-linux-pkg`: a built deb, rpm or tarball, read back, holds no ELF
//! program that is not static and declares no dependency; with `--staged` it also carries the
//! files of [`nfpm_stage`](super::nfpm_stage) at the daemon's paths: `/usr/lib/hostkeeper/gh`,
//! `/usr/bin/hostkeeper` and `/usr/bin/hostkeeper-pxe`, regular files of mode 0755, and
//! `/usr/lib/hostkeeper/trusted_root.json`, 0644, each owned by root and group root, each with
//! the bytes of the file staged for it; and the console's issue link,
//! `/etc/issue.d/hostkeeper.issue`, a link that points at `/run/hostkeeper/enrollment.issue`
//! (`hostkeeper_daemon::console`), a path that exists only on a running host. Every way the
//! package differs is reported, one line each.
//!
//! The package is told apart by its first bytes and read with no tool:
//!
//! - a deb is an `ar` archive whose `data.tar.gz` member is the tree it installs and whose
//!   `control.tar.gz` holds `./control`, the file that declares its dependencies (`Depends` and
//!   `Pre-Depends`); an entry's mode and owner are its tar header's, and a link's target is its
//!   tar header's link name;
//! - an rpm is a lead, a signature header, a header and a gzip `cpio` (newc) payload. The mode,
//!   the owner and the group rpm installs a file with are the header's, the bytes are the
//!   payload's, a link's target is the header's `FILELINKTOS`, and the dependencies are the
//!   `REQUIRENAME` entries other than `rpmlib(...)` ([`rpm`]);
//! - a tarball is a gzip, xz or zstd tar, and declares nothing ([`archive`]).
//!
//! nfpm 2.48.0 compresses a deb's and an rpm's parts with gzip, its default, which `nfpm.yaml`
//! does not change; any other compression of them is refused by name rather than read.
//!
//! Every regular file of the package that starts with the ELF magic is read with [`super::elf`]
//! and refused when it asks for an interpreter, needs a library, or cannot be read.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use self::archive::{compressed, deb, tarred};
use super::elf;
use super::nfpm_stage::{GH_FILE, HOSTKEEPER_FILE, PXE_FILE, TRUSTED_ROOT_FILE};
use crate::release::Error;

/// Where the deb and the rpm install `gh`: `hostkeeper_daemon::cli::GH_BINARY`.
pub(crate) const INSTALLED_GH: &str = "/usr/lib/hostkeeper/gh";

/// Where they install the trusted root: `hostkeeper_daemon::cli::SIGSTORE_TRUSTED_ROOT`.
pub(crate) const INSTALLED_TRUSTED_ROOT: &str = "/usr/lib/hostkeeper/trusted_root.json";

/// Where the deb and the rpm install the console's issue link:
/// `hostkeeper_daemon::console::ISSUE_LINK`.
pub(crate) const INSTALLED_ISSUE_LINK: &str = "/etc/issue.d/hostkeeper.issue";

/// What that link points at: `hostkeeper_daemon::console::ENROLLMENT_ISSUE`.
pub(crate) const ISSUE_TARGET: &str = "/run/hostkeeper/enrollment.issue";

/// Where the deb and the rpm install the daemon.
pub(crate) const INSTALLED_HOSTKEEPER: &str = "/usr/bin/hostkeeper";

/// Where they install the boot server of network install.
pub(crate) const INSTALLED_PXE: &str = "/usr/bin/hostkeeper-pxe";

/// Each checked file: where it is installed, the staged file it must equal, its mode.
const CHECKED: [(&str, &str, u32); 4] = [
    (INSTALLED_GH, GH_FILE, 0o755),
    (INSTALLED_TRUSTED_ROOT, TRUSTED_ROOT_FILE, 0o644),
    (INSTALLED_HOSTKEEPER, HOSTKEEPER_FILE, 0o755),
    (INSTALLED_PXE, PXE_FILE, 0o755),
];

/// The file-type bits of a mode, and those of a regular file.
const TYPE_BITS: u32 = 0o170_000;
const REGULAR: u32 = 0o100_000;
const SYMLINK: u32 = 0o120_000;

/// What a package holds and declares.
struct Contents {
    kind: Kind,
    /// Every file, by installed path.
    files: BTreeMap<String, Installed>,
    /// The dependencies it declares, one per relation.
    depends: Vec<String>,
}

/// The format of a package.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Deb,
    Rpm,
    Tarball,
}

/// A file of the package as it is installed.
#[derive(Debug, PartialEq, Eq)]
struct Installed {
    /// With its type bits.
    mode: u32,
    owner: String,
    group: String,
    data: Vec<u8>,
    /// What a link points at; none for anything else.
    target: Option<String>,
}

/// Why the package cannot be read.
fn bad(why: impl std::fmt::Display) -> Error {
    Error::Failed(format!("not a package this check reads: {why}"))
}

/// `len` bytes of `bytes` at `at`.
fn take(bytes: &[u8], at: usize, len: usize) -> Result<&[u8], Error> {
    at.checked_add(len)
        .and_then(|end| bytes.get(at..end))
        .ok_or_else(|| bad(format!("cut short: {len} bytes at offset {at}")))
}

fn be_u32(bytes: &[u8], at: usize) -> Result<u32, Error> {
    let b = take(bytes, at, 4)?;
    Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// A whole gzip stream, decoded.
fn gunzip(bytes: &[u8], what: &str) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    flate2::read::MultiGzDecoder::new(bytes)
        .read_to_end(&mut out)
        .map_err(|e| bad(format!("{what}: {e}")))?;
    Ok(out)
}

/// The installed path of an archive path: `./usr/x` and `usr/x` are `/usr/x`.
fn installed_path(name: &str) -> String {
    format!("/{}", name.trim_start_matches("./").trim_start_matches('/'))
}

/// The problems of `found` against the files staged in `staged`, then of the issue link, one
/// line each.
fn staged_problems(
    found: &BTreeMap<String, Installed>,
    staged: &Path,
) -> Result<Vec<String>, Error> {
    let mut out = Vec::new();
    for (dst, file, mode) in CHECKED {
        let path = staged.join(file);
        let want = std::fs::read(&path).map_err(Error::io(format!("read {}", path.display())))?;
        let Some(got) = found.get(dst) else {
            out.push(format!("{dst}: not in the package"));
            continue;
        };
        if got.mode & TYPE_BITS != REGULAR {
            out.push(format!("{dst}: not a regular file"));
        }
        if got.mode & 0o7777 != mode {
            out.push(format!("{dst}: mode {:o}, not {mode:o}", got.mode & 0o7777));
        }
        if (got.owner.as_str(), got.group.as_str()) != ("root", "root") {
            out.push(format!(
                "{dst}: owned by {}:{}, not root:root",
                got.owner, got.group
            ));
        }
        if got.data != want {
            out.push(format!(
                "{dst}: its bytes are not those of {}",
                path.display()
            ));
        }
    }
    let link = INSTALLED_ISSUE_LINK;
    match found.get(link) {
        None => out.push(format!("{link}: not in the package")),
        Some(got) if got.mode & TYPE_BITS != SYMLINK => out.push(format!("{link}: not a link")),
        Some(got) if got.target.as_deref() != Some(ISSUE_TARGET) => out.push(format!(
            "{link}: points at {}, not {ISSUE_TARGET}",
            got.target.as_deref().unwrap_or("nothing")
        )),
        Some(_) => {}
    }
    Ok(out)
}

/// The regular files of `found` that are ELF programs: the paths of the static ones, and the
/// problems of the others, one line each.
fn programs(found: &BTreeMap<String, Installed>) -> (Vec<&str>, Vec<String>) {
    let (mut static_programs, mut out) = (Vec::new(), Vec::new());
    for (path, file) in found {
        if file.mode & TYPE_BITS != REGULAR || !elf::is_elf(&file.data) {
            continue;
        }
        let elf = match elf::parse(&file.data) {
            Ok(elf) => elf,
            Err(why) => {
                out.push(format!("{path}: an ELF file this check cannot read: {why}"));
                continue;
            }
        };
        let before = out.len();
        if let Some(interpreter) = &elf.interpreter {
            out.push(format!(
                "{path}: ELF program with the interpreter {interpreter}, so not static"
            ));
        }
        for library in &elf.needed {
            out.push(format!("{path}: ELF program linked to {library}"));
        }
        if out.len() == before {
            static_programs.push(path.as_str());
        }
    }
    (static_programs, out)
}

/// Reads `package`, whose bytes are `bytes`, as the format its first bytes name.
fn read(package: &Path, bytes: &[u8]) -> Result<Contents, Error> {
    if bytes.starts_with(b"!<arch>\n") {
        deb(bytes)
    } else if bytes.starts_with(&[0xed, 0xab, 0xee, 0xdb]) {
        rpm::read(bytes)
    } else if compressed(bytes) {
        tarred(bytes)
    } else {
        Err(bad(format!(
            "{} is neither a deb, an rpm nor a tarball",
            package.display()
        )))
    }
}

/// Checks the package at `package`: the lines to print. With `staged`, also against the files
/// staged there.
pub(crate) fn check(package: &Path, staged: Option<&Path>) -> Result<Vec<String>, Error> {
    let bytes = std::fs::read(package).map_err(Error::io(format!("read {}", package.display())))?;
    let contents = read(package, &bytes)?;
    let name = package.display();
    if staged.is_some() && contents.kind == Kind::Tarball {
        return Err(Error::Failed(format!(
            "{name}: --staged does not apply to a tarball, which installs nothing at the daemon's paths"
        )));
    }
    let mut problems = match staged {
        Some(staged) => staged_problems(&contents.files, staged)?,
        None => Vec::new(),
    };
    let (static_programs, elf_problems) = programs(&contents.files);
    problems.extend(elf_problems);
    problems.extend(
        contents
            .depends
            .iter()
            .map(|dependency| format!("declares the dependency {dependency}")),
    );
    if !problems.is_empty() {
        return Err(Error::Refused(
            problems
                .into_iter()
                .map(|p| format!("{name}: {p}"))
                .collect(),
        ));
    }
    let mut lines = Vec::new();
    if staged.is_some() {
        lines.extend(CHECKED.iter().map(|(dst, _, mode)| {
            format!("{name}: {dst} is {mode:o} root:root and the staged bytes")
        }));
        lines.push(format!(
            "{name}: {INSTALLED_ISSUE_LINK} is a link to {ISSUE_TARGET}"
        ));
    }
    lines.extend(
        static_programs
            .iter()
            .map(|path| format!("{name}: {path} is a static ELF program")),
    );
    if static_programs.is_empty() {
        lines.push(format!("{name}: holds no ELF program"));
    }
    if contents.kind != Kind::Tarball {
        lines.push(format!("{name}: declares no dependency"));
    }
    Ok(lines)
}

mod archive;
mod rpm;
#[cfg(test)]
mod tests;
