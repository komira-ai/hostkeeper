//! `hostkeeper-pxe fetch` (docs/design/network-install.md, section 5, item N8): download the
//! boot sets and the hostkeeper debs of a release into a new directory, verify them, and seal it.
//!
//! The steps, in order; each file is written once, measured in the pass that writes it:
//!
//! 1. The directory is made; it must not exist, and its parent must. Everything below is written
//!    under it, and on any refusal it is removed whole, so a refused fetch keeps nothing.
//! 2. `netboot.json` and its bundle `netboot.json.sigstore.json` are downloaded from the release
//!    (`<release>/netboot.json`), and the attestation is checked: the bundle must list the
//!    file's SHA-256, and the daemon's verifier must confirm that the trusted signer made it.
//!    Only then is `netboot.json` read ([`Netboot::parse`]) for the platforms asked for.
//! 3. For each platform, the hostkeeper deb of the release, `hostkeeper_<version>_<arch>.deb`, is
//!    downloaded to `deb/hostkeeper_<arch>.deb` (the path the seeds of item N6 fetch it by), at
//!    most `max_package_bytes` of the daemon config, and its attestation is checked the same way.
//!    The debs come before the ISOs, so a release whose deb is refused is refused before gigabytes
//!    are downloaded.
//! 4. For each platform, the ISO is downloaded from its pinned URL to its pinned name, stopped one
//!    byte past its pinned size, and must have that size and SHA-256. Only then is it opened, and
//!    the kernel and initrd are copied out of it, and the shim and GRUB out of the debs of its
//!    pool, each to its served name and each held to its own pin.
//! 5. The bundles, the copy of `netboot.json` and `gh`'s home (all under `.verify/`) are removed,
//!    and the seal is written last ([`crate::seal`]).
//!
//! Every refusal is a [`FetchError`] that names what was refused: the URL, the asset, or the
//! platform, part and served name of the boot file, and for a file taken out of the ISO or a deb
//! the path it was looked for and where.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, DirBuilder, File, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use hostkeeper_config::daemon::TrustedSigners;
use hostkeeper_package::bundle::{BUNDLE_SUFFIX, MAX_BUNDLE_BYTES};
use hostkeeper_package::net::{Timeouts, Transport};
use hostkeeper_package::ports::{Attested, Verifier};
use hostkeeper_steps::Clock;

use crate::attest::{AttestError, Checker, attest};
use crate::copy::{CopyError, Measured, copy};
use crate::deb::{self, DebError};
use crate::download::{DownloadError, download};
use crate::iso::{Iso, IsoError};
use crate::netboot::{BootSet, InDeb, InIso, Netboot, NetbootInvalid, Pin};
use crate::platform::BootPlatform;
use crate::seal::{SEAL_FILE, SEAL_FORMAT, Seal, SealedDeb, SealedFile, SealedNetboot, SealedSet};

/// The boot-set asset of a release.
pub const NETBOOT: &str = "netboot.json";
/// The most bytes of `netboot.json`.
pub const MAX_NETBOOT_BYTES: u64 = 1 << 20;
/// The directory of the fetched directory that holds what is checked and not kept: the bundles,
/// the copy of `netboot.json`, and `gh`'s home ([`GH_HOME`]). Mode 0700, removed before the seal
/// is written.
pub const VERIFY_DIR: &str = ".verify";
/// The directory under [`VERIFY_DIR`] that `gh` runs in.
pub const GH_HOME: &str = "gh";
/// The directory of the shims and GRUBs: that of `hostkeeper/boot.efi` (design section 3).
pub const LOADER_DIR: &str = "hostkeeper";
/// The directory of the debs.
pub const DEB_DIR: &str = "deb";

/// The ports a fetch runs on: the network, the verifier and the clock.
#[derive(Clone, Copy)]
pub struct Ports<'a> {
    pub transport: &'a dyn Transport,
    pub verifier: &'a dyn Verifier,
    pub clock: &'a dyn Clock,
}

/// What to fetch, and the bounds of the fetch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// The `https://` URL of the directory that holds the release's assets.
    pub release: String,
    /// The new directory.
    pub dir: PathBuf,
    /// The platforms whose boot sets and debs are fetched; at least one.
    pub platforms: BTreeSet<BootPlatform>,
    /// The daemon config's trusted signer.
    pub signers: TrustedSigners,
    /// The daemon config's bound on each attestation check.
    pub attestation_timeout_secs: u64,
    /// The daemon config's `max_package_bytes`: the most bytes of a deb.
    pub max_deb_bytes: u64,
    /// The bounds of each download.
    pub timeouts: Timeouts,
}

/// What a file of a boot set, or the deb of a platform, is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Iso,
    Shim,
    Grub,
    Kernel,
    Initrd,
    Deb,
}

impl fmt::Display for Part {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Part::Iso => "iso",
            Part::Shim => "shim",
            Part::Grub => "grub",
            Part::Kernel => "kernel",
            Part::Initrd => "initrd",
            Part::Deb => "deb",
        })
    }
}

/// One file of a platform: its part and the name it is served as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileId {
    pub platform: BootPlatform,
    pub part: Part,
    pub name: String,
}

impl fmt::Display for FileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} {}", self.platform, self.part, self.name)
    }
}

/// How many bytes a file had.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Taken {
    Exactly(u64),
    /// More than this; the copy stopped one byte past it.
    MoreThan(u64),
}

impl fmt::Display for Taken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Taken::Exactly(bytes) => write!(f, "{bytes} bytes"),
            Taken::MoreThan(bytes) => write!(f, "more than {bytes} bytes"),
        }
    }
}

/// Why a fetch was refused. The directory is gone unless the refusal is [`FetchError::DirExists`]
/// (it was never made) or [`FetchError::NotRemoved`].
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("{dir} already exists: fetch writes a new directory")]
    DirExists { dir: PathBuf },
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// A download that delivered no body: no connection, a status other than 2xx (a missing
    /// asset is a 404), a body that could not be read.
    #[error("{url}: {why}")]
    Download { url: String, why: String },
    /// `netboot.json`, a bundle or a deb over its bound.
    #[error("{url}: more than {cap} bytes")]
    TooLarge { url: String, cap: u64 },
    /// `netboot.json` or a deb is not attested by the trusted signer, or the check could not be
    /// made.
    #[error("{asset}: {why}")]
    Attestation { asset: String, why: AttestError },
    #[error("netboot.json: {0}")]
    Netboot(NetbootInvalid),
    #[error("{file}: {got}, but netboot.json pins {expected} bytes")]
    Size {
        file: FileId,
        expected: u64,
        got: Taken,
    },
    #[error("{file}: SHA-256 {got}, but netboot.json pins {expected}")]
    Digest {
        file: FileId,
        expected: String,
        got: String,
    },
    /// The path a pin names is not in the ISO, or not in the deb.
    #[error("{file}: {path} is not in {inside}")]
    Missing {
        file: FileId,
        path: String,
        inside: String,
    },
    #[error("{file}: {path} in {inside} is not a regular file")]
    NotAFile {
        file: FileId,
        path: String,
        inside: String,
    },
    /// The ISO or a deb in it is not of the form its reader takes.
    #[error("{file}: {inside} cannot be read: {why}")]
    Unreadable {
        file: FileId,
        inside: String,
        why: String,
    },
    /// A refusal, after which the directory could not be removed.
    #[error("{refusal}; and {dir} could not be removed: {source}")]
    NotRemoved {
        refusal: Box<FetchError>,
        dir: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Runs the fetch `request` describes (see the module's steps) and returns the seal it wrote.
pub fn fetch(ports: &Ports<'_>, request: &Request) -> Result<Seal, FetchError> {
    make_dir(&request.dir, 0o755).map_err(|source| match source.kind() {
        io::ErrorKind::AlreadyExists => FetchError::DirExists {
            dir: request.dir.clone(),
        },
        _ => FetchError::Io {
            path: request.dir.clone(),
            source,
        },
    })?;
    fill(ports, request).map_err(|refusal| discard(&request.dir, refusal))
}

/// `refusal`, once `dir` is removed; [`FetchError::NotRemoved`] when it cannot be.
pub(crate) fn discard(dir: &Path, refusal: FetchError) -> FetchError {
    match fs::remove_dir_all(dir) {
        Ok(()) => refusal,
        Err(source) => FetchError::NotRemoved {
            refusal: Box::new(refusal),
            dir: dir.to_owned(),
            source,
        },
    }
}

/// Steps 2 to 5, into the directory step 1 made.
fn fill(ports: &Ports<'_>, request: &Request) -> Result<Seal, FetchError> {
    let dir = &request.dir;
    let work = dir.join(VERIFY_DIR);
    for (path, mode) in [
        (work.clone(), 0o700),
        (work.join(GH_HOME), 0o700),
        (dir.join(LOADER_DIR), 0o755),
        (dir.join(DEB_DIR), 0o755),
    ] {
        make_dir(&path, mode).map_err(io_at(&path))?;
    }
    let fetcher = Fetcher {
        ports,
        request,
        work: &work,
    };

    let netboot_url = asset_url(&request.release, NETBOOT);
    let netboot_path = work.join(NETBOOT);
    let measured = fetcher.save(
        &netboot_url,
        &netboot_path,
        MAX_NETBOOT_BYTES,
        too_large(&netboot_url),
    )?;
    let attested = fetcher.attested(NETBOOT, &netboot_url, &netboot_path, &measured)?;
    let text = fs::read(&netboot_path).map_err(io_at(&netboot_path))?;
    let netboot = Netboot::parse(&text, &request.platforms).map_err(FetchError::Netboot)?;
    let sealed_netboot = SealedNetboot {
        commit: attested.commit.as_str().to_owned(),
        sha256: measured.sha256,
        source_ref: attested.source_ref.as_str().to_owned(),
    };

    let mut debs = Vec::new();
    for platform in netboot.sets.keys() {
        debs.push(fetcher.deb(*platform, &netboot.version)?);
    }
    let mut platforms = BTreeMap::new();
    for ((platform, set), deb) in netboot.sets.iter().zip(debs) {
        let sealed = fetcher.boot_set(*platform, set, deb)?;
        platforms.insert(platform.as_str().to_owned(), sealed);
    }

    fs::remove_dir_all(&work).map_err(io_at(&work))?;
    let seal = Seal {
        netboot: sealed_netboot,
        platforms,
        seal: SEAL_FORMAT,
        ubuntu: netboot.ubuntu,
        version: netboot.version,
    };
    let seal_path = dir.join(SEAL_FILE);
    let mut file = create(&seal_path)?;
    file.write_all(seal.to_json().as_bytes())
        .and_then(|()| file.sync_all())
        .and_then(|()| File::open(dir)?.sync_all())
        .map_err(io_at(&seal_path))?;
    Ok(seal)
}

/// The steps of one fetch, over its ports and request.
struct Fetcher<'a> {
    ports: &'a Ports<'a>,
    request: &'a Request,
    work: &'a Path,
}

impl Fetcher<'_> {
    /// Downloads `url` into the new file `path`, at most `cap` bytes; a body over the cap is
    /// `over(cap)`.
    fn save(
        &self,
        url: &str,
        path: &Path,
        cap: u64,
        over: impl FnOnce(u64) -> FetchError,
    ) -> Result<Measured, FetchError> {
        let mut file = create(path)?;
        let measured = download(
            self.ports.transport,
            url,
            self.request.timeouts,
            cap,
            &mut file,
        )
        .map_err(|error| match error {
            DownloadError::Body(CopyError::TooLarge(cap)) => over(cap),
            other => download_failure(url, path, other),
        })?;
        file.sync_all().map_err(io_at(path))?;
        Ok(measured)
    }

    /// Checks that the asset `asset`, downloaded from `url` to `path`, is attested: its bundle
    /// is downloaded from `url` with `.sigstore.json` appended, and checked ([`attest`]).
    fn attested(
        &self,
        asset: &str,
        url: &str,
        path: &Path,
        measured: &Measured,
    ) -> Result<Attested, FetchError> {
        let bundle_url = format!("{url}{BUNDLE_SUFFIX}");
        let bundle_path = self.work.join(format!("{asset}{BUNDLE_SUFFIX}"));
        self.save(
            &bundle_url,
            &bundle_path,
            MAX_BUNDLE_BYTES,
            too_large(&bundle_url),
        )?;
        let bundle = fs::read(&bundle_path).map_err(io_at(&bundle_path))?;
        let checker = Checker {
            verifier: self.ports.verifier,
            clock: self.ports.clock,
            signers: &self.request.signers,
            timeout_secs: self.request.attestation_timeout_secs,
        };
        attest(&checker, path, &measured.sha256, &bundle_path, &bundle).map_err(|why| {
            FetchError::Attestation {
                asset: asset.to_owned(),
                why,
            }
        })
    }

    /// Step 3 for `platform`: the deb of `version`, downloaded and attested.
    fn deb(&self, platform: BootPlatform, version: &str) -> Result<SealedDeb, FetchError> {
        let arch = platform.deb_arch();
        let asset = format!("hostkeeper_{version}_{arch}.deb");
        let name = format!("{DEB_DIR}/hostkeeper_{arch}.deb");
        let url = asset_url(&self.request.release, &asset);
        let path = self.request.dir.join(&name);
        let measured = self.save(&url, &path, self.request.max_deb_bytes, too_large(&url))?;
        let attested = self.attested(&asset, &url, &path, &measured)?;
        Ok(SealedDeb {
            bytes: measured.bytes,
            commit: attested.commit.as_str().to_owned(),
            name,
            sha256: measured.sha256,
            source_ref: attested.source_ref.as_str().to_owned(),
        })
    }

    /// Step 4 for `platform`: the ISO, then the four files taken out of it.
    fn boot_set(
        &self,
        platform: BootPlatform,
        set: &BootSet,
        deb: SealedDeb,
    ) -> Result<SealedSet, FetchError> {
        let pin = &set.iso.pin;
        let id = FileId {
            platform,
            part: Part::Iso,
            name: pin.name.clone(),
        };
        let path = self.request.dir.join(&pin.name);
        let measured = self.save(&set.iso.url, &path, pin.bytes, |cap| FetchError::Size {
            file: id.clone(),
            expected: pin.bytes,
            got: Taken::MoreThan(cap),
        })?;
        let iso_file = held(&id, pin, measured)?;
        let file = File::open(&path).map_err(io_at(&path))?;
        let iso = Iso::open(file).map_err(|error| FetchError::Unreadable {
            file: id.clone(),
            inside: pin.name.clone(),
            why: error.to_string(),
        })?;
        let taken = FromIso {
            dir: &self.request.dir,
            platform,
            iso: &iso,
            iso_name: &pin.name,
        };
        Ok(SealedSet {
            deb,
            grub: taken.copy_from_deb(Part::Grub, &set.grub)?,
            initrd: taken.copy_from_iso(Part::Initrd, &set.initrd)?,
            iso: iso_file,
            kernel: taken.copy_from_iso(Part::Kernel, &set.kernel)?,
            shim: taken.copy_from_deb(Part::Shim, &set.shim)?,
        })
    }
}

/// The files taken out of one platform's ISO, into the fetched directory.
struct FromIso<'a> {
    dir: &'a Path,
    platform: BootPlatform,
    iso: &'a Iso,
    iso_name: &'a str,
}

impl FromIso<'_> {
    fn id(&self, part: Part, pin: &Pin) -> FileId {
        FileId {
            platform: self.platform,
            part,
            name: pin.name.clone(),
        }
    }

    /// The file `file` of the ISO, copied to its served name and held to its pin.
    fn copy_from_iso(&self, part: Part, file: &InIso) -> Result<SealedFile, FetchError> {
        let id = self.id(part, &file.pin);
        let inside = |error| iso_failure(&id, &file.iso_path, self.iso_name, error);
        let extent = self.iso.find(&file.iso_path).map_err(inside)?;
        let mut reader = self.iso.reader(extent).map_err(inside)?;
        let path = self.dir.join(&file.pin.name);
        let mut out = create(&path)?;
        let measured = copy(&mut reader, &mut out, file.pin.bytes)
            .map_err(|error| copy_failure(&id, &file.pin, self.iso_name, &path, error))?;
        out.sync_all().map_err(io_at(&path))?;
        held(&id, &file.pin, measured)
    }

    /// The file `file` of a deb of the ISO's pool, copied to its served name and held to its
    /// pin.
    fn copy_from_deb(&self, part: Part, file: &InDeb) -> Result<SealedFile, FetchError> {
        let id = self.id(part, &file.pin);
        let inside = |error| iso_failure(&id, &file.iso_path, self.iso_name, error);
        let extent = self.iso.find(&file.iso_path).map_err(inside)?;
        let mut reader = self.iso.reader(extent).map_err(inside)?;
        let path = self.dir.join(&file.pin.name);
        let mut out = create(&path)?;
        let measured = deb::extract(&mut reader, &file.deb_path, &mut out, file.pin.bytes)
            .map_err(|error| deb_failure(&id, file, &path, error))?;
        out.sync_all().map_err(io_at(&path))?;
        held(&id, &file.pin, measured)
    }
}

/// The URL of the release asset `asset`.
fn asset_url(release: &str, asset: &str) -> String {
    format!("{}/{asset}", release.trim_end_matches('/'))
}

/// Makes the directory `path` with the mode `mode`, whatever the umask.
fn make_dir(path: &Path, mode: u32) -> io::Result<()> {
    DirBuilder::new().mode(mode).create(path)?;
    fs::set_permissions(path, Permissions::from_mode(mode))
}

/// Makes the new file `path`, mode 0644 whatever the umask, never through a link.
fn create(path: &Path) -> Result<File, FetchError> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(path)
        .map_err(io_at(path))?;
    file.set_permissions(Permissions::from_mode(0o644))
        .map_err(io_at(path))?;
    Ok(file)
}

/// The [`FetchError::Io`] of `path`.
fn io_at(path: &Path) -> impl FnOnce(io::Error) -> FetchError + '_ {
    move |source| FetchError::Io {
        path: path.to_owned(),
        source,
    }
}

/// The [`FetchError::TooLarge`] of `url`.
fn too_large(url: &str) -> impl FnOnce(u64) -> FetchError + '_ {
    move |cap| FetchError::TooLarge {
        url: url.to_owned(),
        cap,
    }
}

/// The refusal of a download of `url` into `path` that did not deliver its body within its cap.
pub(crate) fn download_failure(url: &str, path: &Path, error: DownloadError) -> FetchError {
    match error {
        DownloadError::Body(CopyError::Write(source)) => FetchError::Io {
            path: path.to_owned(),
            source,
        },
        DownloadError::Body(CopyError::TooLarge(cap)) => FetchError::TooLarge {
            url: url.to_owned(),
            cap,
        },
        DownloadError::Body(CopyError::Read(e)) => FetchError::Download {
            url: url.to_owned(),
            why: format!("reading the body failed: {e}"),
        },
        other => FetchError::Download {
            url: url.to_owned(),
            why: other.to_string(),
        },
    }
}

/// The [`FetchError::Unreadable`] of `id`, whose container `inside` failed with `error`.
fn unreadable(id: &FileId, inside: &str, error: &dyn fmt::Display) -> FetchError {
    FetchError::Unreadable {
        file: id.clone(),
        inside: inside.to_owned(),
        why: error.to_string(),
    }
}

/// The refusal of `id`, whose `path` could not be found in the ISO `iso_name`.
pub(crate) fn iso_failure(id: &FileId, path: &str, iso_name: &str, error: IsoError) -> FetchError {
    match error {
        IsoError::NotFound => FetchError::Missing {
            file: id.clone(),
            path: path.to_owned(),
            inside: iso_name.to_owned(),
        },
        IsoError::NotAFile => FetchError::NotAFile {
            file: id.clone(),
            path: path.to_owned(),
            inside: iso_name.to_owned(),
        },
        other => unreadable(id, iso_name, &other),
    }
}

/// The refusal of `id`, the file `file` of a deb, being copied to `path`.
pub(crate) fn deb_failure(id: &FileId, file: &InDeb, path: &Path, error: DebError) -> FetchError {
    match error {
        DebError::NotFound => FetchError::Missing {
            file: id.clone(),
            path: file.deb_path.clone(),
            inside: file.iso_path.clone(),
        },
        DebError::NotAFile => FetchError::NotAFile {
            file: id.clone(),
            path: file.deb_path.clone(),
            inside: file.iso_path.clone(),
        },
        DebError::Copy(error) => copy_failure(id, &file.pin, &file.iso_path, path, error),
        DebError::Malformed(why) => unreadable(id, &file.iso_path, &why),
    }
}

/// The refusal of `id`, pinned by `pin`, whose copy out of `inside` into `path` failed.
pub(crate) fn copy_failure(
    id: &FileId,
    pin: &Pin,
    inside: &str,
    path: &Path,
    error: CopyError,
) -> FetchError {
    match error {
        CopyError::TooLarge(cap) => FetchError::Size {
            file: id.clone(),
            expected: pin.bytes,
            got: Taken::MoreThan(cap),
        },
        CopyError::Read(e) => unreadable(id, inside, &e),
        CopyError::Write(source) => FetchError::Io {
            path: path.to_owned(),
            source,
        },
    }
}

/// `measured` as the sealed file `id`, when it is its pin's size and SHA-256.
fn held(id: &FileId, pin: &Pin, measured: Measured) -> Result<SealedFile, FetchError> {
    if measured.bytes != pin.bytes {
        return Err(FetchError::Size {
            file: id.clone(),
            expected: pin.bytes,
            got: Taken::Exactly(measured.bytes),
        });
    }
    if measured.sha256 != pin.sha256 {
        return Err(FetchError::Digest {
            file: id.clone(),
            expected: pin.sha256.clone(),
            got: measured.sha256,
        });
    }
    Ok(SealedFile {
        bytes: measured.bytes,
        name: pin.name.clone(),
        sha256: measured.sha256,
    })
}

#[cfg(test)]
#[path = "fetch_tests.rs"]
mod tests;
