//! `release nfpm-stage`: the four files of the deb and the rpm, laid out where
//! `packaging/linux/nfpm.yaml` takes them from: the two the daemon's attestation check runs on,
//! and the two programs it ships. The daemon starts `gh` at `/usr/lib/hostkeeper/gh` and passes
//! it `/usr/lib/hostkeeper/trusted_root.json` as `--custom-trusted-root`
//! (`hostkeeper_daemon::cli::GH_BINARY` and `SIGSTORE_TRUSTED_ROOT`); the programs are
//! `hostkeeper` and `hostkeeper-pxe`, taken from `--binaries`.
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
//! 4. Reads `hostkeeper` and `hostkeeper-pxe` from `--binaries` and refuses either unless it is a
//!    64-bit little-endian ELF program of the architecture with no `PT_INTERP` and no
//!    `DT_NEEDED` entry (a static build; see [`super::elf`]).
//! 5. Writes `<out>/gh` (mode 0755), `<out>/trusted_root.json` (mode 0644) and the two programs
//!    (mode 0755), replacing what was there.

use std::fmt;
use std::fs::{OpenOptions, Permissions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256 as Sha};

use super::elf;
use crate::install::{self, Unpack};
use crate::release::Error;
use crate::release::contract::Version;

/// The file name of `gh` in the staging directory, the `src` of nfpm.yaml's `gh` entry.
pub(crate) const GH_FILE: &str = "gh";

/// The file name of the trusted root in the staging directory.
pub(crate) const TRUSTED_ROOT_FILE: &str = "trusted_root.json";

/// The file name of the daemon in the staging directory, and in the directory of `--binaries`.
pub(crate) const HOSTKEEPER_FILE: &str = "hostkeeper";

/// The file name of the boot server of network install in the staging directory, and in the
/// directory of `--binaries`.
pub(crate) const PXE_FILE: &str = "hostkeeper-pxe";

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
    /// The directory holding the static `hostkeeper` and `hostkeeper-pxe`.
    pub(crate) binaries: PathBuf,
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

/// Refuses the program `file` unless it is a static 64-bit little-endian ELF program for `arch`:
/// no interpreter and no needed library.
fn check_static(file: &str, program: &[u8], arch: Arch) -> Result<(), Error> {
    let refuse = |why: String| {
        Err(Error::Failed(format!(
            "{file} is not a static linux {arch} program: {why}"
        )))
    };
    let elf = match elf::parse(program) {
        Ok(elf) => elf,
        Err(why) => return refuse(why),
    };
    if elf.class != 2 || !elf.little_endian {
        return refuse(format!(
            "ELF class {} and data {}, not 2 (64-bit) and 1 (little-endian)",
            elf.class,
            if elf.little_endian { 1 } else { 2 }
        ));
    }
    if elf.machine != arch.elf_machine() {
        return refuse(format!(
            "ELF machine {:#x}, not {:#x}",
            elf.machine,
            arch.elf_machine()
        ));
    }
    if let Some(interpreter) = elf.interpreter {
        return refuse(format!("it asks for the interpreter {interpreter}"));
    }
    if let Some(library) = elf.needed.first() {
        return refuse(format!("it is linked to {library}"));
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
    let mut programs = Vec::new();
    for file in [HOSTKEEPER_FILE, PXE_FILE] {
        let path = request.binaries.join(file);
        let program = read(&path)?;
        check_static(&path.display().to_string(), &program, request.arch)?;
        programs.push((file, program));
    }
    std::fs::create_dir_all(&request.out)
        .map_err(Error::io(format!("create {}", request.out.display())))?;
    let gh_out = request.out.join(GH_FILE);
    let root_out = request.out.join(TRUSTED_ROOT_FILE);
    write(&gh_out, &gh, 0o755)?;
    write(&root_out, &root, 0o644)?;
    let mut lines = Vec::new();
    for (file, program) in &programs {
        let out = request.out.join(file);
        write(&out, program, 0o755)?;
        lines.push(format!(
            "{}: static {} program from {}, sha256 {}",
            out.display(),
            request.arch,
            request.binaries.join(file).display(),
            hex::encode(Sha::digest(program))
        ));
    }
    let mut all = vec![
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
    ];
    all.extend(lines);
    Ok(all)
}

#[cfg(test)]
mod tests;
