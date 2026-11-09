//! `release deb`: the `.deb` of hello (and its failing build) or of mdm for one Linux platform,
//! written byte for byte the same for one commit and version, with no tool.
//!
//! The file is `<package>_<version>_<arch>.deb` (`amd64` for linux-x86_64, `arm64` for
//! linux-arm64), where `<version>` is the release's, `<version>-fail-ready` for hello's failing
//! build (the version its program answers `/version` with). The `Version` of its `control` is the
//! package's Debian version ([`Package::deb_version`]): the release's, or `<version>~fail.ready`
//! for the failing build, which sorts below the release; so the failing build's file name and its
//! `control` differ in the version, by design. It is an `ar` archive of three
//! members in this order, each dated the commit's committer time, owned by 0 and 0, mode 0644:
//!
//! - `debian-binary`, `2.0` and a line break;
//! - `control.tar.gz`, a gzip ustar of the directory `./` and the file `control` alone (the tar
//!   writer drops a leading `./` from every other name, which dpkg reads the same): no maintainer
//!   script, so
//!   dpkg runs nothing of the package's own at install, upgrade or removal. `control` names the
//!   package, its version and architecture, the maintainer, the installed size and the
//!   description, and declares no dependency (the programs are those the archive package
//!   carries, built for the release runner's glibc);
//! - `data.tar.gz`, a gzip ustar of `./` and the files of [`super::hello`] or [`super::mdm`] by
//!   their paths from `/` (`usr/lib/hello/hello`), each directory above them listed first (mode
//!   0755), every entry owned by root and group root
//!   (uid and gid 0), dated the commit.
//!
//! Hostkeeper's own `.deb` is not this verb's: nfpm builds it from `packaging/linux/nfpm.yaml`,
//! with the scripts a bootstrap package needs (packages-native.md section 10.4). mdm's is built
//! for linux-x86_64 only, from `--archives`, the pinned archives `release mdm-fetch` downloaded.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::hello::{self, File};
use super::{DIR_MODE, mdm};
use crate::release::Error;
use crate::release::contract::{Commit, Package, Platform, Service, Version};
use crate::release::ports::GitHistory;

/// What `release deb` builds the package from.
pub(crate) struct Request {
    pub(crate) service: Service,
    pub(crate) platform: Platform,
    pub(crate) version: Version,
    /// The commit built: every time in the package is its committer time.
    pub(crate) commit: Commit,
    /// The service's program for the platform.
    pub(crate) binary: PathBuf,
    /// Hello's failing build: its units start the program with `--fail-ready`.
    pub(crate) fail_ready: bool,
    /// The directory of mdm's pinned archives; none for hello.
    pub(crate) archives: Option<PathBuf>,
    /// The directory the package is written to.
    pub(crate) out: PathBuf,
}

/// Why hostkeeper's `.deb` is not this verb's.
pub(crate) const HOSTKEEPER_REFUSED: &str = "hostkeeper's .deb is built by nfpm from packaging/linux/nfpm.yaml, with its bootstrap scripts: release deb builds the .deb of hello or mdm";

/// Why a `.deb` is not built for macOS.
const MACOS_REFUSED: &str =
    "a .deb is built for linux-x86_64 or linux-arm64; macOS takes the .pkg (release macos-pkg)";

/// Why hello's `.deb` takes no `--archives`.
const HELLO_ARCHIVES: &str =
    "--archives is an option of mdm's .deb: only it carries pinned third-party tools";

/// Why mdm's `.deb` takes no `--fail-ready`.
const MDM_FAIL_READY: &str = "--fail-ready is an option of hello's .deb: mdm has no failing build";

/// Why mdm's `.deb` needs `--archives`.
const MDM_ARCHIVES: &str = "mdm's .deb carries the pinned tools of hostkeeper-mdm-core's pins.txt: pass --archives with the directory `release mdm-fetch` filled";

/// Why mdm's `.deb` is built for linux-x86_64 alone.
const MDM_PLATFORM: &str =
    "mdm's .deb is built for linux-x86_64 only: the pinned tools are linux-amd64 builds";

/// Who the package's `control` names as its maintainer: the project, as nfpm.yaml does.
const MAINTAINER: &str = "The hostkeeper project <https://github.com/komira-ai/hostkeeper>";

/// The project's home, in `control`.
const HOMEPAGE: &str = "https://github.com/komira-ai/hostkeeper";

/// The Debian architecture of a Linux platform.
pub(crate) fn arch(platform: Platform) -> Option<&'static str> {
    match platform {
        Platform::LinuxX86_64 => Some("amd64"),
        Platform::LinuxArm64 => Some("arm64"),
        Platform::MacosArm64 => None,
    }
}

/// The file name of the `.deb` of `package` at the version text `version` for `arch`.
pub(crate) fn deb_name(package: &str, version: &str, arch: &str) -> String {
    format!("{package}_{version}_{arch}.deb")
}

/// Builds the `.deb` of `request`, dated by the commit's time in git; the lines to print.
pub(crate) fn deb(git: &dyn GitHistory, request: &Request) -> Result<Vec<String>, Error> {
    deb_with(
        request,
        || Ok(git.commit_time(&request.commit)?),
        crate::release::mdm_fetch::built_in_pins,
    )
}

/// [`deb`], with the package's times from `mtime` and mdm's pins from `pins` (each asked for only
/// when needed).
pub(crate) fn deb_with(
    request: &Request,
    mtime: impl FnOnce() -> Result<u64, Error>,
    pins: impl FnOnce() -> Result<hostkeeper_mdm_core::pins::Pins, Error>,
) -> Result<Vec<String>, Error> {
    let refuse = |why: &str| Err(Error::Failed(why.to_owned()));
    let Some(arch) = arch(request.platform) else {
        return refuse(MACOS_REFUSED);
    };
    let package = match (request.service, request.archives.as_deref()) {
        (Service::Hostkeeper, _) => return refuse(HOSTKEEPER_REFUSED),
        (Service::Hello, Some(_)) => return refuse(HELLO_ARCHIVES),
        (Service::Hello, None) if request.fail_ready => Package::HelloFailReady,
        (Service::Hello, None) => Package::Hello,
        (Service::Mdm, _) if request.fail_ready => return refuse(MDM_FAIL_READY),
        (Service::Mdm, None) => return refuse(MDM_ARCHIVES),
        (Service::Mdm, Some(_)) if request.platform != Platform::LinuxX86_64 => {
            return refuse(MDM_PLATFORM);
        }
        (Service::Mdm, Some(_)) => Package::Mdm,
    };
    let binary = std::fs::read(&request.binary)
        .map_err(Error::io(format!("read {}", request.binary.display())))?;
    let files = match request.archives.as_deref() {
        None => hello::deb_files(binary, request.fail_ready),
        Some(dir) => mdm::deb_files(binary, mdm::tools(&pins()?, dir)?),
    };
    let name = request.service.name();
    let version = package.version_text(&request.version);
    let bytes = build(
        name,
        &package.deb_version(&request.version),
        arch,
        &description(request.service),
        &files,
        mtime()?,
    )?;
    let file = deb_name(name, &version, arch);
    std::fs::create_dir_all(&request.out)
        .map_err(Error::io(format!("create {}", request.out.display())))?;
    let path = request.out.join(&file);
    std::fs::write(&path, &bytes).map_err(Error::io(format!("write {}", path.display())))?;
    let hex = hex::encode(Sha256::digest(&bytes));
    Ok(vec![format!("{file}: sha256 {hex}")])
}

/// The `Description` of the service's package: a summary line and its continuation.
fn description(service: Service) -> String {
    match service {
        Service::Mdm => "the MDM server stack hostkeeper deploys\n The servers that enroll the farm's Macs (NanoHUB, the SCEP server, NanoDEP, Caddy and\n lego) and the program mdm, their update hook and Apple-facing verbs. Installed and run\n by hostkeeper.".to_owned(),
        _ => "hello, the example service of hostkeeper\n An HTTP server on loopback with an update hook, deployed by hostkeeper to prove an\n install, an update and a rollback.".to_owned(),
    }
}

/// The `control` of the package `name` at `version` for `arch`, whose files hold `bytes` bytes.
pub(crate) fn control(
    name: &str,
    version: &str,
    arch: &str,
    description: &str,
    bytes: u64,
) -> String {
    let kib = bytes.div_ceil(1024);
    format!(
        "Package: {name}\n\
         Version: {version}\n\
         Architecture: {arch}\n\
         Maintainer: {MAINTAINER}\n\
         Installed-Size: {kib}\n\
         Section: misc\n\
         Priority: optional\n\
         Homepage: {HOMEPAGE}\n\
         Description: {description}\n"
    )
}

/// The `.deb` of `files` as the package `name` at `version` for `arch`, every time `mtime`.
pub(crate) fn build(
    name: &str,
    version: &str,
    arch: &str,
    description: &str,
    files: &[File],
    mtime: u64,
) -> Result<Vec<u8>, Error> {
    let size: u64 = files.iter().map(|f| f.data.len() as u64).sum();
    let control_text = control(name, version, arch, description, size);
    let control_tar = tar(
        &[
            Entry::Dir("./".to_owned()),
            Entry::File("./control".to_owned(), 0o644, control_text.into_bytes()),
        ],
        mtime,
    )?;
    let mut dirs = BTreeSet::new();
    for file in files {
        let mut parent = Path::new(&file.path).parent();
        while let Some(dir) = parent.filter(|d| *d != Path::new("/")) {
            dirs.insert(format!(".{}/", dir.display()));
            parent = dir.parent();
        }
    }
    let mut entries = vec![Entry::Dir("./".to_owned())];
    entries.extend(dirs.into_iter().map(Entry::Dir));
    entries.extend(
        files
            .iter()
            .map(|f| Entry::File(format!(".{}", f.path), f.mode, f.data.clone())),
    );
    let data_tar = tar(&entries, mtime)?;
    Ok(ar(
        &[
            ("debian-binary", b"2.0\n".to_vec()),
            ("control.tar.gz", gzip(&control_tar)?),
            ("data.tar.gz", gzip(&data_tar)?),
        ],
        mtime,
    ))
}

/// One entry of a tar this module writes.
enum Entry {
    /// A directory (its name ends in `/`).
    Dir(String),
    /// A regular file: its name, mode and bytes.
    File(String, u32, Vec<u8>),
}

/// The ustar of `entries`, in order, each owned by root and group root and dated `mtime`.
fn tar(entries: &[Entry], mtime: u64) -> Result<Vec<u8>, Error> {
    let what = "build a tar of the .deb";
    let mut builder = tar::Builder::new(Vec::new());
    for entry in entries {
        let (name, kind, mode, data): (&str, tar::EntryType, u32, &[u8]) = match entry {
            Entry::Dir(name) => (name, tar::EntryType::Directory, DIR_MODE, &[]),
            Entry::File(name, mode, data) => (name, tar::EntryType::Regular, *mode, data),
        };
        let mut header = tar::Header::new_ustar();
        header.set_entry_type(kind);
        header.set_mode(mode);
        header.set_uid(0);
        header.set_gid(0);
        header.set_username("root").map_err(Error::io(what))?;
        header.set_groupname("root").map_err(Error::io(what))?;
        header.set_mtime(mtime);
        header.set_size(data.len() as u64);
        builder
            .append_data(&mut header, name, data)
            .map_err(Error::io(what))?;
    }
    builder.into_inner().map_err(Error::io(what))
}

/// `bytes` gzip-compressed, with no name and no time in the gzip header.
fn gzip(bytes: &[u8]) -> Result<Vec<u8>, Error> {
    use std::io::Write;
    let what = "gzip a part of the .deb";
    let mut encoder = flate2::GzBuilder::new().write(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes).map_err(Error::io(what))?;
    encoder.finish().map_err(Error::io(what))
}

/// The `ar` archive of `members`, in order, each dated `mtime`, owned by 0 and 0, mode 0644, and
/// padded to an even length with a line break.
fn ar(members: &[(&str, Vec<u8>)], mtime: u64) -> Vec<u8> {
    let mut out = b"!<arch>\n".to_vec();
    for (name, data) in members {
        let header = format!(
            "{name:<16}{mtime:<12}{:<6}{:<6}{:<8}{:<10}`\n",
            0,
            0,
            100_644,
            data.len()
        );
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(data);
        if data.len() % 2 == 1 {
            out.push(b'\n');
        }
    }
    out
}

#[cfg(test)]
mod tests;
