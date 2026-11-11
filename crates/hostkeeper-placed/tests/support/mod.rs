//! Shared by the tests: a scratch directory that is the root of a host's state directory, unit
//! directory and package directories, with the fakes.
#![allow(dead_code, reason = "each test binary uses part of the shared support")]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Component, Path, PathBuf};

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_placed::testing::{Event, FakeUnitControl, FaultFs, FsWrite, Log, UnitCall};
use hostkeeper_placed::{Fs, PlacedRecord, Placer, Roots, Store};

pub fn service(name: &str) -> ServiceName {
    ServiceName::new(name).expect("a service name")
}

pub fn version(text: &str) -> Version {
    Version::new(text).expect("a version")
}

/// What is at one path of the scratch tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    File { bytes: Vec<u8>, inode: u64 },
    Dir,
    Link(PathBuf),
    Other,
}

/// A host under a scratch directory: `/var/lib/hostkeeper`, `/var/lib/hostkeeper/packages` and
/// `/etc/systemd/system` exist below its root. `fs` records every write in `log`, which the
/// fake unit control shares.
pub struct World {
    pub scratch: hostkeeper_testkit::Scratch,
    pub log: Log,
    pub fs: FaultFs,
    pub units: FakeUnitControl,
    pub roots: Roots,
}

/// The unit directory of a Mac, where launchd reads its plists.
pub const LAUNCHD_DIR: &str = "/Library/LaunchDaemons";

impl World {
    pub fn new(name: &str) -> World {
        World::with_roots(name, Roots::linux())
    }

    /// A host whose unit directory is launchd's, [`LAUNCHD_DIR`]; the state and packages
    /// directories are as on Linux, as they are on a Mac.
    pub fn launchd(name: &str) -> World {
        let linux = Roots::linux();
        World::with_roots(
            name,
            Roots::new(
                linux.state_dir(),
                Path::new(LAUNCHD_DIR),
                linux.packages().dir(),
            ),
        )
    }

    fn with_roots(name: &str, roots: Roots) -> World {
        let base = Path::new(env!("CARGO_TARGET_TMPDIR"));
        let scratch = hostkeeper_testkit::Scratch::under(base, &format!("placed-{name}"))
            .expect("create the scratch directory");
        let log = Log::new();
        let world = World {
            fs: FaultFs::new(scratch.path(), log.clone()),
            units: FakeUnitControl::new(log.clone()),
            log,
            roots,
            scratch,
        };
        let dirs = [world.roots.packages().dir(), world.roots.unit_dir()];
        for dir in dirs {
            fs::create_dir_all(world.host(dir)).expect("create a host directory");
        }
        world
    }

    /// The scratch directory, the host's `/`.
    pub fn root(&self) -> &Path {
        self.scratch.path()
    }

    /// Where the host path `path` is on disk.
    pub fn host(&self, path: impl AsRef<Path>) -> PathBuf {
        let path = path.as_ref();
        self.root().join(path.strip_prefix("/").unwrap_or(path))
    }

    /// A placer over [`World::fs`] and [`World::units`].
    pub fn placer(&self) -> Placer<'_> {
        Placer::new(self.roots.clone(), &self.fs, &self.units)
    }

    /// A placer over `fs` and [`World::units`].
    pub fn placer_on<'a>(&'a self, fs: &'a dyn Fs) -> Placer<'a> {
        Placer::new(self.roots.clone(), fs, &self.units)
    }

    /// A filesystem under this root whose `n`-th write and every later one fail, sharing the log.
    pub fn failing_at(&self, n: u64) -> FaultFs {
        FaultFs::failing_at(self.root(), n, self.log.clone())
    }

    /// The version directory on disk.
    pub fn version_dir(&self, service: &str, version: &str) -> PathBuf {
        self.host(format!("/var/lib/hostkeeper/packages/{service}/{version}"))
    }

    /// Points the link `<packages>/<service>/<name>` (`current` or `previous`) at `version`,
    /// replacing what is there.
    pub fn set_link(&self, service: &str, name: &str, version: &str) {
        let link = self.host(format!("/var/lib/hostkeeper/packages/{service}/{name}"));
        let _ = fs::remove_file(&link);
        symlink(version, &link).expect("make the link");
    }

    /// The target of the link at the host path `path`, `None` when it is not a link.
    pub fn read_link(&self, path: impl AsRef<Path>) -> Option<PathBuf> {
        fs::read_link(self.host(path)).ok()
    }

    /// The record on disk.
    pub fn record(&self) -> PlacedRecord {
        Store::new(&self.fs, self.roots.state_dir())
            .load()
            .expect("placed.json loads")
    }

    /// The bytes of `placed.json`, `None` when there is none.
    pub fn placed_bytes(&self) -> Option<Vec<u8>> {
        fs::read(self.host("/var/lib/hostkeeper/placed.json")).ok()
    }

    /// Every path below the root, as a host path, with what is there.
    pub fn snapshot(&self) -> BTreeMap<PathBuf, Node> {
        let mut out = BTreeMap::new();
        walk(self.root(), Path::new("/"), &mut out);
        out
    }

    /// The writes logged so far.
    pub fn writes(&self) -> Vec<FsWrite> {
        self.log
            .events()
            .into_iter()
            .filter_map(|event| match event {
                Event::Fs(write) => Some(write),
                Event::Unit(_) | Event::Mark(_) => None,
            })
            .collect()
    }

    /// The unit calls logged so far.
    pub fn unit_calls(&self) -> Vec<UnitCall> {
        self.log
            .events()
            .into_iter()
            .filter_map(|event| match event {
                Event::Unit(call) => Some(call),
                Event::Fs(_) | Event::Mark(_) => None,
            })
            .collect()
    }
}

/// Makes a named pipe at `disk` (a path on disk): a file that is neither a regular file, a
/// directory nor a link.
pub fn fifo(disk: &Path) {
    hostkeeper_testkit::fifo(disk, 0o600).expect("make a named pipe");
}

/// Pushes the plain components of `path` on `parts` so that the first is popped first.
fn push_parts(parts: &mut Vec<OsString>, path: &Path) {
    let plain: Vec<OsString> = path
        .components()
        .filter_map(|part| match part {
            Component::Normal(name) => Some(name.to_os_string()),
            _ => None,
        })
        .collect();
    parts.extend(plain.into_iter().rev());
}

fn walk(disk: &Path, host: &Path, out: &mut BTreeMap<PathBuf, Node>) {
    let mut entries: Vec<_> = fs::read_dir(disk)
        .expect("list a directory")
        .map(|entry| entry.expect("a directory entry"))
        .collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let disk_path = entry.path();
        let host_path = host.join(entry.file_name());
        let meta = fs::symlink_metadata(&disk_path).expect("stat");
        let node = if meta.file_type().is_symlink() {
            Node::Link(fs::read_link(&disk_path).expect("read a link"))
        } else if meta.is_dir() {
            walk(&disk_path, &host_path, out);
            Node::Dir
        } else if meta.is_file() {
            Node::File {
                bytes: fs::read(&disk_path).expect("read a file"),
                inode: meta.ino(),
            }
        } else {
            Node::Other
        };
        out.insert(host_path, node);
    }
}
