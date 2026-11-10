//! The mirror of the private repositories (docs/design/hostgroups.md, 6.6): the files apt reads
//! of a private repository, copied from the coordinator, which reads the origin with a credential
//! the host never holds.
//!
//! [`CoordinatorMirror`] is the [`Mirror`] the repository installer calls while it stages. It
//! reads every file by `GET /repositories/<name>/dists|pool/<suite>/...` ([`Fetch`], the
//! coordinator's agent listener over the host's mutual TLS: [`CoordinatorGet`]), and keeps them
//! under `<mirror root>/<name>/`, laid out as the origin lays them out, which the pushed entry's
//! `file:<mirror root>/<name>` URI names. `<suite>` is the host's group: the one suite of the
//! entry the host was sent.
//!
//! - **The index** ([`Mirror::index`]). `dists/<suite>/InRelease` is read first and its
//!   `SHA256:` list parsed; the daemon checks no signature (apt does, with the pushed keyring).
//!   Of the listed files, those under `<component>/binary-<architecture>/`,
//!   `<component>/binary-all/` and `<component>/i18n/` for the entry's components are read,
//!   each checked against its listed size and SHA-256, into a new directory beside the mirror
//!   (`.<name>.next`, directories 0755, files 0644), which then takes the mirror's place whole
//!   ([`swap`]). A file that is not what the list says, or is gone, means the origin published
//!   between two reads: everything is read again once, and a second time is
//!   [`MirrorError::Unreachable`]. An `InRelease` the coordinator does not have is
//!   [`MirrorError::NotPublished`]: the group's suite is not published yet.
//! - **The package** ([`Mirror::pool`]). The index's `Filename`, which must be under
//!   `pool/<suite>/`, is read into the mirror (a new file, then renamed), its size and SHA-256
//!   checked against the order's pin; other bytes are [`MirrorError::Digest`].
//! - **What stays.** [`prune`] removes, when a pushed set is applied, every mirror directory
//!   that is not a private entry of the set or holds another suite's `dists/`.
//!
//! The mirror serves a stage and nothing else: activation and rollback use the version
//! directory's kept archives.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_config::daemon::DaemonConfig;
use hostkeeper_install::repository::{Mirror, MirrorError, Repository, mirrored_name};
use hostkeeper_link::{CoordinatorGet, GetError, Got, Limit, Log, PkiCertSource, TcpTransport};
use hostkeeper_proto::hostkeeper::release::v1::Repository as Entry;
use hostkeeper_release::registry;
use hostkeeper_steps::Clock;
use sha2::{Digest, Sha256};

use crate::repository_push::{remove_path, swap};

/// The most bytes of an `InRelease`.
pub const INRELEASE_MAX: u64 = 1 << 20;
/// The most bytes of an index file.
pub const INDEX_MAX: u64 = 512 << 20;
/// Seconds an `InRelease` may take to arrive.
pub const INRELEASE_SECS: u64 = 120;
/// Seconds an index file may take to arrive.
pub const INDEX_SECS: u64 = 1800;
/// Seconds a package file may take to arrive; also the bound of each connection of the host's
/// transport to the coordinator.
pub const POOL_SECS: u64 = 3600;

/// The mode of a mirror directory and of a mirror file: what apt, which runs as another user,
/// reads them with.
const DIR_MODE: u32 = 0o755;
const FILE_MODE: u32 = 0o644;

/// The coordinator, as the mirror reads it: one `GET` of a path, the body given to a sink.
pub trait Fetch: Send + Sync {
    /// Reads `path`; see [`CoordinatorGet::get`].
    ///
    /// # Errors
    /// [`GetError`]: why the coordinator gave no body.
    fn get(
        &self,
        path: &str,
        limit: Limit,
        sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> Result<Got, GetError>;
}

impl Fetch for CoordinatorGet {
    fn get(
        &self,
        path: &str,
        limit: Limit,
        sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> Result<Got, GetError> {
        CoordinatorGet::get(self, path, limit, sink)
    }
}

/// The coordinator as the host's mirror reads it: [`CoordinatorGet`] over TCP, each connection
/// bounded by its call's deadline, with the certificate of the pki directory `pki` (its own reading
/// of it, which logs nothing: the heartbeat's source says what is wrong with the pair). None
/// when the config's coordinator address is not one that can be dialed; the heartbeat refuses
/// the same address and says so.
pub fn over_coordinator(
    config: &DaemonConfig,
    pki: &Path,
    clock: Arc<dyn Clock>,
) -> Option<Arc<dyn Fetch>> {
    let certs = Arc::new(PkiCertSource::new(pki, Arc::new(Quiet)));
    let transport = Arc::new(TcpTransport::new(Arc::clone(&clock)));
    CoordinatorGet::new(config, pki, certs, transport, clock)
        .ok()
        .map(|get| Arc::new(get) as Arc<dyn Fetch>)
}

/// A log that keeps nothing.
struct Quiet;

impl Log for Quiet {
    fn line(&self, _text: &str) {}
}

/// The [`Mirror`] over the coordinator.
pub struct CoordinatorMirror {
    fetch: Arc<dyn Fetch>,
    root: PathBuf,
}

impl fmt::Debug for CoordinatorMirror {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CoordinatorMirror {{ root: {} }}", self.root.display())
    }
}

/// What reading one file came to.
enum Read {
    /// The file is at the destination: its size and SHA-256.
    Done { size: u64, sha256: String },
    /// The coordinator has no such file.
    Absent,
    /// The body is longer than the bound.
    Over,
}

/// How one pass over the index ended.
enum Pass {
    Done,
    /// The origin changed between two reads, in this way.
    Changed(String),
}

impl CoordinatorMirror {
    /// The mirror under `root`, reading through `fetch`.
    pub fn new(root: PathBuf, fetch: Arc<dyn Fetch>) -> CoordinatorMirror {
        CoordinatorMirror { fetch, root }
    }

    /// The registry name of the private repository `repository` is read from: the last
    /// component of its `file:` URI, which must be a directory of this mirror.
    fn name_of<'a>(&self, repository: &'a Repository) -> Result<&'a str, MirrorError> {
        mirrored_name(repository, &self.root).ok_or_else(|| {
            MirrorError::Io(format!(
                "the entry {} is read from {}, which is no directory of the mirror {}",
                repository.name,
                repository.uri,
                self.root.display()
            ))
        })
    }

    /// Reads `path` from the coordinator into the new file `dest`, mode 0644, counting the size
    /// and the SHA-256 of what arrived.
    fn read(&self, path: &str, dest: &Path, limit: Limit) -> Result<Read, MirrorError> {
        let io_error = |e: io::Error| MirrorError::Io(format!("{}: {e}", dest.display()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(FILE_MODE)
            .open(dest)
            .map_err(io_error)?;
        file.set_permissions(Permissions::from_mode(FILE_MODE))
            .map_err(io_error)?;
        let mut hash = Sha256::new();
        let mut size = 0_u64;
        let got = self.fetch.get(path, limit, &mut |bytes| {
            hash.update(bytes);
            size += bytes.len() as u64;
            file.write_all(bytes)
        });
        match got {
            Ok(Got::Body(_)) => {
                file.sync_all().map_err(io_error)?;
                Ok(Read::Done {
                    size,
                    sha256: hex::encode(hash.finalize()),
                })
            }
            Ok(Got::NotFound) => Ok(Read::Absent),
            Err(GetError::TooLong { .. }) => Ok(Read::Over),
            Err(GetError::Sink(kind)) => Err(MirrorError::Io(format!(
                "{}: {}",
                dest.display(),
                io::Error::from(kind)
            ))),
            Err(other) => Err(MirrorError::Unreachable(format!("{path}: {other}"))),
        }
    }

    /// One pass: the new directory of `repository`, filled and put in the mirror's place. What
    /// does not end in the swap leaves no new directory behind.
    fn pass(
        &self,
        repository: &Repository,
        name: &str,
        architecture: &str,
    ) -> Result<Pass, MirrorError> {
        let next = self.root.join(format!(".{name}.next"));
        let _ = remove_path(&next);
        let result = self.fill(repository, name, architecture, &next);
        if !matches!(result, Ok(Pass::Done)) {
            let _ = remove_path(&next);
        }
        result
    }

    /// Reads the index files of `repository` into the new directory `next`, and swaps it in.
    fn fill(
        &self,
        repository: &Repository,
        name: &str,
        architecture: &str,
        next: &Path,
    ) -> Result<Pass, MirrorError> {
        let io_error = |e: io::Error| MirrorError::Io(format!("{}: {e}", next.display()));
        make_dir(&self.root).map_err(io_error)?;
        make_dir(next).map_err(io_error)?;
        let dists = format!("dists/{}", repository.suite);
        make_dirs(next, &dists).map_err(io_error)?;
        let release_path = format!("{dists}/InRelease");
        let release = next.join(&release_path);
        let url = |path: &str| format!("/repositories/{name}/{path}");
        let limit = |max_bytes, deadline_secs| Limit {
            max_bytes,
            deadline_secs,
        };
        let text = match self.read(
            &url(&release_path),
            &release,
            limit(INRELEASE_MAX, INRELEASE_SECS),
        )? {
            Read::Done { .. } => fs::read_to_string(&release).map_err(io_error)?,
            Read::Absent => {
                return Err(MirrorError::NotPublished(format!(
                    "the coordinator has no {release_path} of the repository {name}"
                )));
            }
            Read::Over => {
                return Err(MirrorError::Unreachable(format!(
                    "{release_path} is longer than {INRELEASE_MAX} bytes"
                )));
            }
        };
        let listed = wanted(&text, &repository.components, architecture)
            .map_err(|why| MirrorError::Unreachable(format!("{release_path}: {why}")))?;
        for (path, (sha256, size)) in &listed {
            let relative = format!("{dists}/{path}");
            // Every listed path is under a component's directory (`wanted`), so it has a parent.
            let parent = Path::new(path)
                .parent()
                .and_then(Path::to_str)
                .unwrap_or_default();
            make_dirs(next, &format!("{dists}/{parent}")).map_err(io_error)?;
            let changed = match self.read(
                &url(&relative),
                &next.join(&relative),
                limit((*size).min(INDEX_MAX), INDEX_SECS),
            )? {
                Read::Done {
                    size: got,
                    sha256: found,
                } if got == *size && found == *sha256 => continue,
                Read::Done {
                    size: got,
                    sha256: found,
                } => format!(
                    "{path} is {got} bytes with the SHA-256 {found}, not the {size} bytes and {sha256} that {release_path} lists"
                ),
                Read::Absent => format!("{path}, which {release_path} lists, is gone"),
                Read::Over => {
                    format!("{path} is longer than the {size} bytes {release_path} lists")
                }
            };
            return Ok(Pass::Changed(changed));
        }
        swap(next, &self.root.join(name)).map_err(io_error)?;
        Ok(Pass::Done)
    }
}

impl Mirror for CoordinatorMirror {
    fn index(&self, repository: &Repository, architecture: &str) -> Result<(), MirrorError> {
        let name = self.name_of(repository)?;
        let mut changed = String::new();
        for _ in 0..2 {
            match self.pass(repository, name, architecture)? {
                Pass::Done => return Ok(()),
                Pass::Changed(why) => changed = why,
            }
        }
        Err(MirrorError::Unreachable(format!(
            "the repository changed while it was read, twice: {changed}"
        )))
    }

    fn pool(
        &self,
        repository: &Repository,
        filename: &str,
        sha256: &str,
        size: u64,
    ) -> Result<(), MirrorError> {
        let name = self.name_of(repository)?;
        let suite = &repository.suite;
        if !under(filename, &format!("pool/{suite}/")) {
            return Err(MirrorError::Unreachable(format!(
                "the index's Filename {filename:?} is not a path under pool/{suite}/"
            )));
        }
        let dir = self.root.join(name);
        let dest = dir.join(filename);
        let io_error = |e: io::Error| MirrorError::Io(format!("{}: {e}", dest.display()));
        make_dir(&self.root).map_err(io_error)?;
        make_dir(&dir).map_err(io_error)?;
        let parent = Path::new(filename)
            .parent()
            .and_then(Path::to_str)
            .unwrap_or_default();
        make_dirs(&dir, parent).map_err(io_error)?;
        let part = dest.with_file_name(format!(
            ".{}.new",
            dest.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
        ));
        let _ = remove_path(&part);
        let read = self.read(
            &format!("/repositories/{name}/{filename}"),
            &part,
            Limit {
                max_bytes: size,
                deadline_secs: POOL_SECS,
            },
        );
        let refuse = |error: MirrorError| {
            let _ = remove_path(&part);
            Err(error)
        };
        match read {
            Ok(Read::Done {
                size: got,
                sha256: found,
            }) if got == size && found == sha256 => fs::rename(&part, &dest).map_err(io_error),
            Ok(Read::Done {
                size: got,
                sha256: found,
            }) => refuse(MirrorError::Digest(format!(
                "{filename} is {got} bytes with the SHA-256 {found}, not the {size} bytes and {sha256} of the order"
            ))),
            Ok(Read::Absent) => refuse(MirrorError::NotPublished(format!(
                "the coordinator has no {filename} of the repository {name}"
            ))),
            Ok(Read::Over) => refuse(MirrorError::Digest(format!(
                "{filename} is longer than the {size} bytes of the order"
            ))),
            Err(error) => refuse(error),
        }
    }
}

/// Removes the mirror directories under `root` that `entries` (a pushed set) do not keep: a
/// directory that is not a private entry's name, and one whose `dists/` holds another suite than
/// the entry's one (a host that changed group loses its old group's files). The staging
/// directory of a kept entry (`.<name>.next`) is left to the fill that may be making it, which
/// replaces it whole. Nothing is removed when `root` is not there.
///
/// # Errors
/// The first file that could not be listed or removed; the rest were still tried.
pub fn prune(root: &Path, entries: &[Entry]) -> io::Result<()> {
    let kept: BTreeMap<&str, &str> = entries
        .iter()
        .filter(|entry| registry::is_private(entry))
        .filter_map(|entry| Some((entry.name.as_str(), entry.suites.first()?.as_str())))
        .collect();
    let listing = match fs::read_dir(root) {
        Ok(listing) => listing,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let mut first = None;
    for item in listing {
        let removed = item.and_then(|item| {
            let name = item.file_name();
            let name = name.to_str();
            let staging = name
                .and_then(|name| name.strip_prefix('.')?.strip_suffix(".next"))
                .is_some_and(|stem| kept.contains_key(stem));
            let keep = staging
                || name
                    .and_then(|name| kept.get(name))
                    .is_some_and(|suite| holds_only(&item.path().join("dists"), suite));
            if keep {
                Ok(())
            } else {
                remove_path(&item.path())
            }
        });
        if let Err(e) = removed {
            first.get_or_insert(e);
        }
    }
    first.map_or(Ok(()), Err)
}

/// Whether the directory `dists` holds no suite but `suite` (or is not there).
fn holds_only(dists: &Path, suite: &str) -> bool {
    match fs::read_dir(dists) {
        Ok(listing) => listing
            .filter_map(Result::ok)
            .all(|item| item.file_name() == suite),
        Err(_) => true,
    }
}

/// The files `release` lists under its `SHA256:` field that the host needs: those under
/// `<component>/binary-<architecture>/`, `<component>/binary-all/` and `<component>/i18n/`,
/// by path with their SHA-256 and size. `Err` says why the text is no list: no `SHA256:` field,
/// a line that is not `<sha256> <size> <path>`, a path that is not a plain relative one, or a
/// path listed twice.
fn wanted(
    release: &str,
    components: &[String],
    architecture: &str,
) -> Result<BTreeMap<String, (String, u64)>, String> {
    let mut lines = release.lines().skip_while(|line| *line != "SHA256:");
    if lines.next().is_none() {
        return Err("lists no SHA256 files".to_owned());
    }
    let prefixes: Vec<String> = components
        .iter()
        .flat_map(|component| {
            [
                format!("{component}/binary-{architecture}/"),
                format!("{component}/binary-all/"),
                format!("{component}/i18n/"),
            ]
        })
        .collect();
    let mut files = BTreeMap::new();
    for line in lines.take_while(|line| line.starts_with(' ')) {
        let mut fields = line.split_whitespace();
        let (Some(sha256), Some(size), Some(path), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(format!(
                "has the line {line:?}, which is no file of the list"
            ));
        };
        let size = size
            .parse::<u64>()
            .ok()
            .filter(|_| sha256.len() == 64 && sha256.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| format!("has the line {line:?}, which is no file of the list"))?;
        if !prefixes
            .iter()
            .any(|prefix| path.starts_with(prefix.as_str()))
        {
            continue;
        }
        if !under(path, "") {
            return Err(format!(
                "lists the path {path:?}, which is not a plain relative path"
            ));
        }
        if files
            .insert(path.to_owned(), (sha256.to_ascii_lowercase(), size))
            .is_some()
        {
            return Err(format!("lists {path} twice"));
        }
    }
    Ok(files)
}

/// Whether `path` starts with `prefix` and is a plain relative path: segments of printable
/// ASCII with no `%`, none empty, `.` or `..`.
fn under(path: &str, prefix: &str) -> bool {
    path.starts_with(prefix)
        && path.len() <= 1024
        && path.split('/').all(|segment| {
            !matches!(segment, "" | "." | "..")
                && segment.bytes().all(|b| b.is_ascii_graphic() && b != b'%')
        })
}

/// Makes the directory `path` (its parent is there), mode 0755 whatever the umask; one that is
/// there is left as it is.
fn make_dir(path: &Path) -> io::Result<()> {
    match fs::create_dir(path) {
        Ok(()) => fs::set_permissions(path, Permissions::from_mode(DIR_MODE)),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        Err(e) => Err(e),
    }
}

/// Makes the directories of the relative path `relative` under `base`, each 0755.
fn make_dirs(base: &Path, relative: &str) -> io::Result<()> {
    let mut path = base.to_path_buf();
    for segment in relative.split('/').filter(|segment| !segment.is_empty()) {
        path.push(segment);
        make_dir(&path)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "mirror_tests.rs"]
mod tests;
