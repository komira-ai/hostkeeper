//! [`unpack`]: a verified archive to a sealed version directory, and [`remove_orphans`].

use std::collections::BTreeSet;
use std::fs::File;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use hostkeeper_config::daemon::{Bounds, Sha256Hex};
use hostkeeper_config::manifest::{
    Expected, HostkeeperVersion, MANIFEST_FILE, MAX_MANIFEST_BYTES, Manifest,
};
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_steps::{
    ArtifactError, EntryKind, Layout, Seal, SealError, TreeEntry, read_seal, read_tree,
};
use sha2::{Digest, Sha256};

use crate::archive::{self, BLOCK, BUFFER, Extended, MAX_EXTENDED_BYTES, Stop, Stream, Tar};
use crate::error::{Rule, UnpackError, escape};
use crate::fs::{PackageFs, ServiceLock};
use crate::publish::publish;
use crate::seal;
use crate::verify::{self, HEAD_BYTES, Platform, StagedEntry, is_mach_o};

/// What to unpack: the service and version the order names, and the SHA-256 the archive was
/// verified to have.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnpackOrder {
    pub service: ServiceName,
    pub version: Version,
    pub package_digest: Sha256Hex,
}

/// The bounds of an unpack, from the daemon config's `[bounds]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The most decompressed bytes, headers and padding included.
    pub max_unpacked_bytes: u64,
    /// The most files and directories.
    pub max_entries: u64,
}

impl From<&Bounds> for Limits {
    fn from(bounds: &Bounds) -> Limits {
        Limits {
            max_unpacked_bytes: bounds.max_unpacked_bytes,
            max_entries: bounds.max_entries,
        }
    }
}

/// A sealed version directory.
#[derive(Clone, Debug, PartialEq)]
pub struct Unpacked {
    /// `packages/<service>/<version>`, where the port puts it.
    pub dir: PathBuf,
    pub seal: Seal,
    /// The manifest, loaded from the version directory.
    pub manifest: Manifest,
    /// `false` when the version was on the host already, sealed for the same archive, and
    /// nothing was unpacked.
    pub fresh: bool,
}

/// The version of the running daemon, which a package's `min_hostkeeper` is compared with:
/// this crate's version, which is the workspace's and so the daemon's.
pub fn running_hostkeeper() -> HostkeeperVersion {
    let none = HostkeeperVersion {
        major: 0,
        minor: 0,
        patch: 0,
    };
    HostkeeperVersion::parse(env!("CARGO_PKG_VERSION")).unwrap_or(none)
}

/// Unpacks the archive at `archive` as the version `order` names, into
/// `packages/<service>/<version>/` with its seal `<version>.seal`, through `fs`.
///
/// In order: refuses a version that is one of the names the package directory holds for
/// itself (`current`, `previous`, `*.seal`, `*.tmp`: [`UnpackError::ReservedVersion`]);
/// creates the service directory (0755) when it is missing; takes the service's unpack lock
/// for the rest of the call, so that no other unpack or orphan removal of the service runs
/// meanwhile (one that holds it already makes this an [`UnpackError::Io`] to try again);
/// removes the orphans of the service; returns the version if it is on the host already,
/// sealed for this archive and unchanged (`fresh: false`), and refuses it untouched if it is
/// not ([`UnpackError::ArtifactChanged`]); creates `.stage-<random>`; reads the archive in one
/// pass, hashing it, and writes each entry that keeps every rule into the staging directory,
/// with modes normalised (0555 when the tar mode has an execute bit and for directories, 0444
/// otherwise) and owner and times not taken from the archive; checks the archive's SHA-256;
/// on macOS (`platform` is [`Platform::MacOs`]) checks every entry of the staged tree for a
/// quarantine attribute and every Mach-O file for a valid code signature, before the manifest
/// is read (on Linux no such call is made); loads the manifest; makes the tree read-only and
/// durable and reads it back (it must be what was written); writes the seal, then renames the
/// tree in. On any error the staging directory, a temporary seal and a seal without its
/// version are removed, and nothing else is touched; see [`UnpackError`] for the one error
/// after which the version stays.
pub fn unpack(
    archive: &Path,
    order: &UnpackOrder,
    limits: &Limits,
    fs: &dyn PackageFs,
    platform: Platform<'_>,
) -> Result<Unpacked, UnpackError> {
    let service = Path::new(order.service.as_str());
    let version = order.version.as_str();
    let layout = Layout::package(&fs.real_path(service));
    if let Err(refused) = layout.check_name(version) {
        let version = version.to_owned();
        return Err(match refused.kind() {
            io::ErrorKind::InvalidFilename => UnpackError::VersionTooLong { version },
            _ => UnpackError::ReservedVersion { version },
        });
    }
    fs.create_service(service)
        .map_err(|e| UnpackError::of_io("cannot create the service directory", e))?;
    let _lock = lock(service, fs)?;
    remove_orphans_locked(service, fs)?;
    if let Some(found) = existing(&layout, order, fs)? {
        return Ok(found);
    }
    let stage = fs
        .create_stage(service)
        .map_err(|e| UnpackError::of_io("cannot create the staging directory", e))?;
    let result = stage_and_publish(archive, order, limits, fs, platform, &layout, &stage);
    if result.is_err() {
        // What a failed cleanup leaves, the next remove_orphans removes. The seal goes only
        // when the version is known to be absent: after the tree's rename it is the version's.
        let _ = fs.remove_tree(&stage);
        let _ = fs.remove_tree(&seal::temporary(service, version));
        if let Ok(false) = fs.exists(&service.join(version)) {
            let _ = fs.remove_tree(&seal::placed(service, version));
        }
    }
    result
}

/// The service's unpack lock.
fn lock(service: &Path, fs: &dyn PackageFs) -> Result<ServiceLock, UnpackError> {
    fs.lock(service)
        .map_err(|e| UnpackError::of_io("cannot take the unpack lock of the service", e))
}

/// Removes what a crashed unpack of `service` left in `packages/<service>`: every `.stage-*`
/// directory, every `.<version>.seal.tmp`, and every `<version>.seal` with no `<version>`
/// beside it. Nothing else is touched. A service with no directory has nothing to remove. It
/// takes the service's unpack lock, so it never removes the staging directory or the seal of an
/// unpack under way: while one runs, it is an [`UnpackError::Io`] to try again.
pub fn remove_orphans(service: &ServiceName, fs: &dyn PackageFs) -> Result<(), UnpackError> {
    let dir = Path::new(service.as_str());
    let present = fs
        .exists(dir)
        .map_err(|e| UnpackError::of_io("cannot look at the package directory", e))?;
    if !present {
        return Ok(());
    }
    let _lock = lock(dir, fs)?;
    remove_orphans_locked(dir, fs)
}

/// [`remove_orphans`] under a lock the caller holds.
fn remove_orphans_locked(dir: &Path, fs: &dyn PackageFs) -> Result<(), UnpackError> {
    let names = fs
        .read_dir(dir)
        .map_err(|e| UnpackError::of_io("cannot list the package directory", e))?;
    let present: BTreeSet<&[u8]> = names.iter().map(|name| name.as_bytes()).collect();
    for name in &names {
        let bytes = name.as_bytes();
        let stage = bytes.starts_with(b".stage-");
        let tmp = bytes.starts_with(b".") & bytes.ends_with(b".seal.tmp");
        let lone = bytes
            .strip_suffix(b".seal")
            .is_some_and(|version| !present.contains(version));
        if stage | tmp | lone {
            fs.remove_tree(&dir.join(name))
                .map_err(|e| UnpackError::of_io("cannot remove an orphan", e))?;
        }
    }
    Ok(())
}

/// The version as it is on the host: `None` when there is no `<version>`, the version when it
/// is sealed for the order's archive and its tree has the seal's digest, an error otherwise.
fn existing(
    layout: &Layout,
    order: &UnpackOrder,
    fs: &dyn PackageFs,
) -> Result<Option<Unpacked>, UnpackError> {
    let version = order.version.as_str();
    let dir = layout.version_path(version);
    let present = fs
        .exists(&Path::new(order.service.as_str()).join(version))
        .map_err(|e| UnpackError::of_io("cannot look at the version", e))?;
    if !present {
        return Ok(None);
    }
    let changed = |why: String| UnpackError::ArtifactChanged {
        version: version.to_owned(),
        why,
    };
    let unreadable = |why: String| UnpackError::SealUnreadable {
        version: version.to_owned(),
        why,
    };
    let bytes = match read_seal(&layout.seal_path(version)) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(changed("has no seal".to_owned()));
        }
        read => read.map_err(|e| unreadable(e.to_string()))?,
    };
    let seal = Seal::parse(&bytes).map_err(|e| match e {
        SealError::Schema(found) => UnpackError::SealSchema {
            version: version.to_owned(),
            found,
        },
        other => unreadable(other.to_string()),
    })?;
    if seal.package_digest() != order.package_digest.as_str() {
        return Err(changed(format!(
            "was sealed for the archive {}, not {}",
            seal.package_digest(),
            order.package_digest.as_str()
        )));
    }
    match layout.check_tree(version) {
        Err(e) if e.kind() == io::ErrorKind::InvalidData => return Err(changed(e.to_string())),
        checked => checked.map_err(|e| UnpackError::of_io("cannot read the version", e))?,
    }
    let manifest = Manifest::load(&dir, &expected(order), &running_hostkeeper())?;
    Ok(Some(Unpacked {
        dir,
        seal,
        manifest,
        fresh: false,
    }))
}

fn expected(order: &UnpackOrder) -> Expected {
    Expected {
        name: order.service.clone(),
        version: order.version.clone(),
    }
}

/// The pass, the checks after it, the seal and the publish, into the staging directory `stage`.
fn stage_and_publish(
    archive: &Path,
    order: &UnpackOrder,
    limits: &Limits,
    fs: &dyn PackageFs,
    platform: Platform<'_>,
    layout: &Layout,
    stage: &Path,
) -> Result<Unpacked, UnpackError> {
    let file = File::open(archive).map_err(|e| UnpackError::of_io("cannot open the archive", e))?;
    let mut pass = Pass::new(fs, stage, limits);
    let sha = pass.run(file)?;
    let found = hex::encode(sha);
    if found != order.package_digest.as_str() {
        return Err(UnpackError::DigestMismatch {
            expected: order.package_digest.as_str().to_owned(),
            found,
        });
    }
    let real = fs.real_path(stage);
    if let Platform::MacOs(mac) = platform {
        verify::verify_staged_tree(&real, &pass.staged, mac, pass.produced)?;
    }
    let manifest = Manifest::load(&real, &expected(order), &running_hostkeeper())?;
    let sealing = |e| UnpackError::of_io("cannot seal the staging directory", e);
    // Contents first, then the directories that hold them, the staging directory last.
    let dirs: Vec<&Path> = pass.dirs.iter().rev().map(PathBuf::as_path).collect();
    for dir in dirs.iter().copied().chain([stage]) {
        fs.set_mode(dir, 0o555).map_err(sealing)?;
    }
    for dir in dirs.iter().copied().chain([stage]) {
        fs.sync_dir(dir).map_err(sealing)?;
    }
    let tree = read_tree(&real).map_err(|e| match e {
        ArtifactError::Io(e) => UnpackError::of_io("cannot read back the staged tree", e),
        // A file that became a link or a special file since it was written.
        _ => broken(b"the staged tree", Rule::NotAsWritten, pass.produced),
    })?;
    pass.entries.sort_by(|a, b| a.name.cmp(&b.name));
    if tree.entries != pass.entries {
        return Err(UnpackError::BadArchive {
            entry: "the staged tree".to_owned(),
            rule: Rule::NotAsWritten,
            at: pass.produced,
        });
    }
    // Both digests are 64 lower-case hex digits (read_tree's and Sha256Hex's), so the seal is
    // always made; the error is built eagerly so that no code path exists only for it.
    let unsealable = UnpackError::Io {
        what: "cannot seal",
        source: io::Error::other("a digest that is not 64 lower-case hex digits"),
    };
    let seal = Seal::new(&tree.digest, order.package_digest.as_str())
        .ok()
        .ok_or(unsealable)?;
    let service = Path::new(order.service.as_str());
    publish(fs, service, stage, order.version.as_str(), &seal)?;
    Ok(Unpacked {
        dir: layout.version_path(order.version.as_str()),
        seal,
        manifest,
        fresh: true,
    })
}

/// The one pass over the archive: what was written so far, and what the rules need to know.
struct Pass<'a> {
    fs: &'a dyn PackageFs,
    stage: &'a Path,
    limits: &'a Limits,
    /// The names written, and their simple lower-case folds.
    names: BTreeSet<String>,
    folded: BTreeSet<String>,
    /// The directories written, by name and as paths in the order they were made.
    dir_names: BTreeSet<String>,
    dirs: Vec<PathBuf>,
    /// Every entry written, as the digest of the staged tree must read it back.
    entries: Vec<TreeEntry>,
    /// Every entry written, as the macOS checks need it.
    staged: Vec<StagedEntry>,
    /// The decompressed bytes read when the pass ended.
    produced: u64,
    buf: Vec<u8>,
}

/// The error of a broken rule at the entry `name` (empty: the stream), after `at` bytes.
fn broken(name: &[u8], rule: Rule, at: u64) -> UnpackError {
    let entry = if name.is_empty() {
        "the archive".to_owned()
    } else {
        escape(name)
    };
    UnpackError::BadArchive { entry, rule, at }
}

/// The error of a stop of the archive reader.
fn stopped(stop: Stop, name: &[u8], at: u64) -> UnpackError {
    match stop {
        Stop::Rule(rule) => broken(name, rule, at),
        Stop::Io(e) => UnpackError::of_io("cannot read the archive", e),
    }
}

fn write_error(e: io::Error) -> UnpackError {
    UnpackError::of_io("cannot write the staging directory", e)
}

impl<'a> Pass<'a> {
    fn new(fs: &'a dyn PackageFs, stage: &'a Path, limits: &'a Limits) -> Pass<'a> {
        Pass {
            fs,
            stage,
            limits,
            names: BTreeSet::new(),
            folded: BTreeSet::new(),
            dir_names: BTreeSet::new(),
            dirs: Vec::new(),
            entries: Vec::new(),
            staged: Vec::new(),
            produced: 0,
            buf: vec![0u8; BUFFER],
        }
    }

    /// Reads the archive to its end and writes every entry. Returns the archive's SHA-256.
    fn run(&mut self, file: File) -> Result<[u8; 32], UnpackError> {
        let stream =
            Stream::open(file, self.limits.max_unpacked_bytes).map_err(|s| stopped(s, &[], 0))?;
        let mut tar = Tar::new(stream);
        let mut extended: Option<Extended> = None;
        let mut long: Option<Vec<u8>> = None;
        loop {
            let at = tar.produced();
            let block = tar.block().map_err(|s| stopped(s, &[], tar.produced()))?;
            if block.iter().all(|&b| b == 0) {
                if extended.is_some() | long.is_some() {
                    return Err(broken(&[], Rule::Header, at));
                }
                let sha = tar.end().map_err(|s| stopped(s, &[], tar.produced()))?;
                self.produced = at;
                return Ok(sha);
            }
            let header = archive::header(&block).map_err(|rule| broken(&[], rule, at))?;
            let size = header.size;
            match header.flag {
                b'x' => {
                    if extended.is_some() | (size > MAX_EXTENDED_BYTES) {
                        return Err(broken(&header.name, Rule::Header, at));
                    }
                    let data = tar
                        .extended(size)
                        .map_err(|s| stopped(s, &header.name, at))?;
                    let records = archive::pax(&data).map_err(|r| broken(&header.name, r, at))?;
                    extended = Some(records);
                }
                b'L' => {
                    if long.is_some() | (size > crate::MAX_NAME_BYTES as u64 + 1) {
                        let rule = if long.is_some() {
                            Rule::Header
                        } else {
                            Rule::NameTooLong
                        };
                        return Err(broken(&header.name, rule, at));
                    }
                    let mut data = tar
                        .extended(size)
                        .map_err(|s| stopped(s, &header.name, at))?;
                    while data.last() == Some(&0) {
                        data.pop();
                    }
                    long = Some(data);
                }
                flag => {
                    let records = extended.take().unwrap_or_default();
                    let name = records.path.or(long.take()).unwrap_or(header.name);
                    let size = records.size.unwrap_or(size);
                    self.entry(&mut tar, &name, header.mode, size, flag, at)?;
                }
            }
        }
    }

    /// Checks one entry against every rule, then writes it. Nothing of an entry that breaks a
    /// rule is written.
    fn entry(
        &mut self,
        tar: &mut Tar,
        raw: &[u8],
        mode: u64,
        size: u64,
        flag: u8,
        at: u64,
    ) -> Result<(), UnpackError> {
        let fail = |rule| broken(raw, rule, at);
        let dir = match flag {
            b'0' | 0 => false,
            b'5' => true,
            other => return Err(fail(Rule::Kind(other))),
        };
        if self.entries.len() as u64 >= self.limits.max_entries {
            return Err(fail(Rule::TooManyEntries));
        }
        let parts = archive::name(raw, dir).map_err(fail)?;
        let depth = parts.len();
        let known_dir = matches!(parts[0], "bin" | "units" | "files");
        let first = depth == 1;
        let top = (first & (parts[0] == MANIFEST_FILE) & !dir) | (first & known_dir & dir);
        if !(top | (!first & known_dir)) {
            return Err(fail(Rule::TopLevel));
        }
        if mode & 0o7000 != 0 {
            return Err(fail(Rule::SpecialBits));
        }
        let parent = parts[..depth - 1].join("/");
        if !first & !self.dir_names.contains(&parent) {
            return Err(fail(Rule::ParentMissing));
        }
        let name = parts.join("/");
        if self.names.contains(&name) {
            return Err(fail(Rule::Duplicate));
        }
        let folded = archive::fold(&name);
        if self.folded.contains(&folded) {
            return Err(fail(Rule::CaseFold));
        }
        if dir & (size != 0) {
            return Err(fail(Rule::Header));
        }
        if (name == MANIFEST_FILE) & (size > MAX_MANIFEST_BYTES) {
            return Err(fail(Rule::ManifestTooLarge));
        }
        let padded = size.div_ceil(BLOCK as u64).saturating_mul(BLOCK as u64);
        if tar.produced().saturating_add(padded) > self.limits.max_unpacked_bytes {
            return Err(fail(Rule::TooLarge));
        }
        let path = self.stage.join(&name);
        self.names.insert(name.clone());
        self.folded.insert(folded);
        if dir {
            self.fs.create_dir(&path).map_err(write_error)?;
            self.dirs.push(path);
            self.dir_names.insert(name.clone());
            self.staged.push(StagedEntry {
                name: name.clone(),
                mach_o: false,
            });
            self.entries.push(TreeEntry {
                name: name.into_bytes(),
                kind: EntryKind::Dir,
                mode: 0o555,
                size: 0,
                content: None,
            });
            return Ok(());
        }
        let mut out = self.fs.create_file(&path).map_err(write_error)?;
        let mut sha = Sha256::new();
        let mut left = size;
        let mut head = Vec::with_capacity(HEAD_BYTES);
        while left > 0 {
            let n = left.min(BUFFER as u64) as usize;
            let chunk = &mut self.buf[..n];
            tar.data(chunk)
                .map_err(|s| stopped(s, raw, tar.produced()))?;
            sha.update(&*chunk);
            let take = (HEAD_BYTES - head.len()).min(n);
            head.extend_from_slice(&chunk[..take]);
            out.write(chunk).map_err(write_error)?;
            left -= n as u64;
        }
        tar.pad(size).map_err(|s| stopped(s, raw, tar.produced()))?;
        let normal = if mode & 0o111 != 0 { 0o555 } else { 0o444 };
        out.set_mode(normal).map_err(write_error)?;
        out.sync().map_err(write_error)?;
        self.staged.push(StagedEntry {
            name: name.clone(),
            mach_o: is_mach_o(&head),
        });
        let mut content = [0u8; 32];
        content.copy_from_slice(&sha.finalize());
        self.entries.push(TreeEntry {
            name: name.into_bytes(),
            kind: EntryKind::File,
            mode: normal,
            size,
            content: Some(content),
        });
        Ok(())
    }
}
