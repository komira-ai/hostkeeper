//! `release nfpm-stage`: the two files of the deb and the rpm that the daemon's attestation check
//! runs on, laid out where `packaging/linux/nfpm.yaml` takes them from. The daemon starts `gh` at
//! `/usr/lib/hostkeeper/gh` and passes it `/usr/lib/hostkeeper/trusted_root.json` as
//! `--custom-trusted-root` (`hostkeeper_daemon::cli::GH_BINARY` and `SIGSTORE_TRUSTED_ROOT`).
//!
//! What it does, in this order, writing nothing until every check has passed:
//!
//! 1. Reads `--gh-archive`, the release archive of `gh` the workflow downloaded, and refuses it
//!    unless its SHA-256 is `--gh-sha256`, the pin of that architecture (`install::checked`, the
//!    check of `install-tool`).
//! 2. Takes `gh_<version>_linux_<arch>/bin/gh` out of it, and refuses a file that is not a 64-bit
//!    little-endian ELF program for the machine of `--arch`, so a deb of one architecture never
//!    carries the `gh` of the other.
//! 3. Reads `--trusted-root`, the Sigstore trusted root the workflow captured with
//!    `gh attestation trusted-root`, and refuses a missing or empty file, and one with a
//!    non-blank line that is not a JSON object whose `mediaType` is a Sigstore trusted root.
//! 4. Writes `<out>/gh` (mode 0755) and `<out>/trusted_root.json` (mode 0644), replacing what
//!    was there.

use std::fmt;
use std::fs::{OpenOptions, Permissions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256 as Sha};

use crate::install::{self, Unpack};
use crate::release::Error;
use crate::release::contract::Version;

/// The file name of `gh` in the staging directory, the `src` of nfpm.yaml's `gh` entry.
pub(crate) const GH_FILE: &str = "gh";

/// The file name of the trusted root in the staging directory.
pub(crate) const TRUSTED_ROOT_FILE: &str = "trusted_root.json";

/// What every line of a trusted root names as its `mediaType`, before its `;version=` part.
pub(crate) const TRUSTED_ROOT_MEDIA_TYPE: &str = "application/vnd.dev.sigstore.trustedroot+json";

/// The architecture of a deb or an rpm, as nfpm's `ARCH` and `gh`'s archive names write it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum Arch {
    /// linux-x86_64.
    Amd64,
    /// linux-arm64.
    Arm64,
}

impl Arch {
    /// The `e_machine` of an ELF program for this architecture: `EM_X86_64`, `EM_AARCH64`.
    fn elf_machine(self) -> u16 {
        match self {
            Arch::Amd64 => 0x3e,
            Arch::Arm64 => 0xb7,
        }
    }
}

impl fmt::Display for Arch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Arch::Amd64 => "amd64",
            Arch::Arm64 => "arm64",
        })
    }
}

/// What `release nfpm-stage` stages from.
pub(crate) struct Request {
    pub(crate) arch: Arch,
    /// The pinned version of `gh`: it names the archive's top directory.
    pub(crate) gh_version: Version,
    /// The downloaded release archive of `gh` for `arch`.
    pub(crate) gh_archive: PathBuf,
    /// The pinned SHA-256 of that archive.
    pub(crate) gh_sha256: install::Sha256,
    /// The trusted root `gh attestation trusted-root` printed.
    pub(crate) trusted_root: PathBuf,
    /// The staging directory; made when missing.
    pub(crate) out: PathBuf,
}

/// The path of `gh` inside its release archive of `version` for `arch`.
pub(crate) fn gh_member(version: &Version, arch: Arch) -> PathBuf {
    PathBuf::from(format!("gh_{version}_linux_{arch}/bin/gh"))
}

/// Refuses `program` unless it is a 64-bit little-endian ELF file for `arch`.
fn check_elf(program: &[u8], arch: Arch) -> Result<(), Error> {
    let refuse = |why: String| {
        Err(Error::Failed(format!(
            "gh is not a linux {arch} program: {why}"
        )))
    };
    // e_ident (16 bytes), e_type (2), e_machine (2).
    let Some(head) = program.get(..20) else {
        return refuse(format!(
            "{} bytes, too short for an ELF header",
            program.len()
        ));
    };
    if head[..4] != *b"\x7fELF" {
        return refuse("no ELF magic".to_owned());
    }
    if head[4] != 2 || head[5] != 1 {
        return refuse(format!(
            "ELF class {} and data {}, not 2 (64-bit) and 1 (little-endian)",
            head[4], head[5]
        ));
    }
    let machine = u16::from_le_bytes([head[18], head[19]]);
    if machine != arch.elf_machine() {
        return refuse(format!(
            "ELF machine {machine:#x}, not {:#x}",
            arch.elf_machine()
        ));
    }
    Ok(())
}

/// Refuses `text`, read from `origin`, unless it holds a trusted root on each non-blank line, and
/// at least one.
fn check_trusted_root(origin: &Path, text: &[u8]) -> Result<(), Error> {
    let refuse = |why: String| {
        Err(Error::Failed(format!(
            "{} is not a Sigstore trusted root: {why}",
            origin.display()
        )))
    };
    let Ok(text) = std::str::from_utf8(text) else {
        return refuse("not UTF-8 text".to_owned());
    };
    let mut roots = 0;
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let at = n + 1;
        let media_type = match serde_json::from_str::<serde_json::Value>(line) {
            Ok(serde_json::Value::Object(object)) => object
                .get("mediaType")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            Ok(_) => return refuse(format!("line {at} is not a JSON object")),
            Err(e) => return refuse(format!("line {at} is not JSON: {e}")),
        };
        let Some(media_type) = media_type else {
            return refuse(format!("line {at} has no mediaType"));
        };
        let base = media_type.split(';').next().unwrap_or_default();
        if base != TRUSTED_ROOT_MEDIA_TYPE {
            return refuse(format!(
                "line {at} has mediaType {media_type:?}, not {TRUSTED_ROOT_MEDIA_TYPE}"
            ));
        }
        roots += 1;
    }
    if roots == 0 {
        return refuse("it is empty".to_owned());
    }
    Ok(())
}

fn read(path: &Path) -> Result<Vec<u8>, Error> {
    std::fs::read(path).map_err(Error::io(format!("read {}", path.display())))
}

/// Writes `data` to `path` with exactly `mode`, whatever was there.
fn write(path: &Path, data: &[u8], mode: u32) -> Result<(), Error> {
    let what = format!("write {}", path.display());
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(mode)
        .open(path)
        .map_err(Error::io(&what))?;
    file.write_all(data)
        .and_then(|()| file.set_permissions(Permissions::from_mode(mode)))
        .map_err(Error::io(&what))
}

/// Stages `gh` and the trusted root of `request`: the lines to print.
pub(crate) fn stage(request: &Request) -> Result<Vec<String>, Error> {
    let archive = read(&request.gh_archive)?;
    let member = gh_member(&request.gh_version, request.arch);
    let gh = install::checked(
        &archive,
        &request.gh_sha256,
        &Unpack::TarGz {
            member: member.clone(),
        },
    )
    .map_err(|e| Error::Failed(format!("{}: {e}", request.gh_archive.display())))?;
    check_elf(&gh, request.arch)?;
    let root = read(&request.trusted_root)?;
    check_trusted_root(&request.trusted_root, &root)?;
    std::fs::create_dir_all(&request.out)
        .map_err(Error::io(format!("create {}", request.out.display())))?;
    let gh_out = request.out.join(GH_FILE);
    let root_out = request.out.join(TRUSTED_ROOT_FILE);
    write(&gh_out, &gh, 0o755)?;
    write(&root_out, &root, 0o644)?;
    Ok(vec![
        format!(
            "{}: gh {} for {} from {}, sha256 {}",
            gh_out.display(),
            request.gh_version,
            request.arch,
            member.display(),
            hex::encode(Sha::digest(&gh))
        ),
        format!(
            "{}: from {}, sha256 {}",
            root_out.display(),
            request.trusted_root.display(),
            hex::encode(Sha::digest(&root))
        ),
    ])
}

#[cfg(test)]
mod tests;
