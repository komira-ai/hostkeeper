//! Shared by the tests: a scratch directory that is the root of a host's state directory, unit
//! directory and package directories, with the fakes, and builders of version directories.
#![allow(dead_code, reason = "each test binary uses part of the shared support")]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Component, Path, PathBuf};

use hostkeeper_config::manifest::{Expected, HostkeeperVersion, Manifest};
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_placed::testing::{Event, FakeUnitControl, FaultFs, FsWrite, Log, UnitCall};
use hostkeeper_placed::{Fs, PlacedRecord, Placer, Roots, Store};

/// A unit's `WantedBy=` target, spelt in two pieces so that this file names no host.
pub const TARGET: &str = concat!("multi-user.", "target");
/// The `.wants` directory of [`TARGET`] in the unit directory.
pub const WANTS: &str = concat!("/etc/systemd/system/multi-user.", "target.wants");
/// The version of the daemon the manifests are read against.
pub const RUNNING: HostkeeperVersion = HostkeeperVersion {
    major: 0,
    minor: 4,
    patch: 0,
};

pub fn service(name: &str) -> ServiceName {
    ServiceName::new(name).expect("a service name")
}

pub fn version(text: &str) -> Version {
    Version::new(text).expect("a version")
}

/// A systemd unit whose `ExecStart=` is `exec`, wanted by [`TARGET`].
pub fn unit_text(exec: &str) -> String {
    format!(
        "[Unit]\nDescription=a test unit\n\n[Service]\nExecStart={exec}\n\n[Install]\nWantedBy={TARGET}\n"
    )
}

/// A systemd unit with no `[Install]` section.
pub fn unit_without_install(exec: &str) -> String {
    format!("[Unit]\nDescription=a test unit\n\n[Service]\nExecStart={exec}\n")
}

/// What a version directory holds besides its manifest.
#[derive(Clone, Debug, Default)]
pub struct Spec {
    /// `units/<name>` with its text.
    pub units: Vec<(String, String)>,
    /// `[units] run`.
    pub run: Vec<String>,
    /// `[[files]]`: `source` (below the version directory, `files/...`) and `path`.
    pub files: Vec<(String, String)>,
}

impl Spec {
    pub fn new() -> Spec {
        Spec::default()
    }

    /// tierstore as design section 4.7 shows it: the template `tierstore@.service`, the run
    /// instance `tierstore@9301.service`, and `files/tierstore.conf` at
    /// `/etc/tierstore/tierstore.conf`.
    pub fn tierstore() -> Spec {
        Spec::new()
            .unit(
                "tierstore@.service",
                &unit_text(
                    "/var/lib/hostkeeper/packages/tierstore/current/bin/tierstore serve --port %i",
                ),
            )
            .run("tierstore@9301.service")
            .file("files/tierstore.conf", "/etc/tierstore/tierstore.conf")
    }

    pub fn unit(mut self, name: &str, text: &str) -> Spec {
        self.units.push((name.to_owned(), text.to_owned()));
        self
    }

    pub fn run(mut self, unit: &str) -> Spec {
        self.run.push(unit.to_owned());
        self
    }

    pub fn file(mut self, source: &str, path: &str) -> Spec {
        self.files.push((source.to_owned(), path.to_owned()));
        self
    }
}

/// The text of the manifest of `service` `version` with `spec`'s run units and files.
pub fn manifest_text(service: &str, version: &str, spec: &Spec) -> String {
    let mut text = format!(
        "schema = 1\nname = \"{service}\"\nversion = \"{version}\"\nmin_hostkeeper = \"0.4.0\"\n\n\
         [hook]\ncommand = [\"bin/{service}\", \"update-hook\"]\nverify_deadline_secs = 1600\n\
         [hook.timeout_secs]\nprepare = 0\ncutover = 300\nverify = 1000\nrollback = 600\n\
         cleanup = 1300\n"
    );
    if !spec.run.is_empty() {
        let run: Vec<String> = spec.run.iter().map(|unit| format!("\"{unit}\"")).collect();
        text.push_str(&format!("\n[units]\nrun = [{}]\n", run.join(", ")));
    }
    for (source, path) in &spec.files {
        text.push_str(&format!(
            "\n[[files]]\nsource = \"{source}\"\npath = \"{path}\"\n"
        ));
    }
    text
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

impl World {
    pub fn new(name: &str) -> World {
        let base = Path::new(env!("CARGO_TARGET_TMPDIR"));
        let scratch = hostkeeper_testkit::Scratch::under(base, &format!("placed-{name}"))
            .expect("create the scratch directory");
        let log = Log::new();
        let world = World {
            fs: FaultFs::new(scratch.path(), log.clone()),
            units: FakeUnitControl::new(log.clone()),
            log,
            roots: Roots::linux(),
            scratch,
        };
        for dir in ["/var/lib/hostkeeper/packages", "/etc/systemd/system"] {
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

    /// Writes the version directory of `service` `version` with `spec`, and loads its manifest.
    pub fn package(&self, service: &str, version: &str, spec: &Spec) -> Manifest {
        let dir = self.version_dir(service, version);
        fs::create_dir_all(dir.join("bin")).expect("create bin/");
        fs::write(dir.join(format!("bin/{service}")), "#!/bin/false\n").expect("write the binary");
        for (name, text) in &spec.units {
            fs::create_dir_all(dir.join("units")).expect("create units/");
            fs::write(dir.join("units").join(name), text).expect("write a unit");
        }
        for (source, _) in &spec.files {
            let path = dir.join(source);
            fs::create_dir_all(path.parent().expect("a parent")).expect("create files/");
            fs::write(&path, format!("{source} of {version}\n")).expect("write a file");
        }
        fs::write(
            dir.join("manifest.toml"),
            manifest_text(service, version, spec),
        )
        .expect("write the manifest");
        self.load(service, version)
    }

    /// Loads the manifest of the version directory of `service` `version`.
    pub fn load(&self, service_name: &str, version_text: &str) -> Manifest {
        let expected = Expected {
            name: service(service_name),
            version: version(version_text),
        };
        Manifest::load(
            &self.version_dir(service_name, version_text),
            &expected,
            &RUNNING,
        )
        .expect("the manifest loads")
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

    /// The host path every link on the way from the host path `path` ends at, `None` when it
    /// does not resolve. Absolute link targets are read under the root, as the host reads them.
    pub fn resolve(&self, path: impl AsRef<Path>) -> Option<PathBuf> {
        let mut parts: Vec<OsString> = Vec::new();
        push_parts(&mut parts, path.as_ref());
        let mut at = PathBuf::from("/");
        let mut hops = 0;
        while let Some(part) = parts.pop() {
            let next = at.join(part);
            match fs::read_link(self.host(&next)) {
                Ok(target) => {
                    hops += 1;
                    if hops > 32 {
                        return None;
                    }
                    if target.is_absolute() {
                        at = PathBuf::from("/");
                    }
                    push_parts(&mut parts, &target);
                }
                Err(_) => at = next,
            }
        }
        self.host(&at).exists().then_some(at)
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

    /// Every link below the root outside the package directories, as host paths.
    pub fn placed_links(&self) -> Vec<PathBuf> {
        self.snapshot()
            .into_iter()
            .filter(|(path, node)| {
                matches!(node, Node::Link(_)) && !path.starts_with("/var/lib/hostkeeper/packages")
            })
            .map(|(path, _)| path)
            .collect()
    }
}

/// Makes a named pipe at `disk` (a path on disk): a file that is neither a regular file, a
/// directory nor a link.
pub fn fifo(disk: &Path) {
    rustix::fs::mknodat(
        rustix::fs::CWD,
        disk,
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_raw_mode(0o600),
        0,
    )
    .expect("make a named pipe");
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

/// Installs `spec` as `service` `version` the way an update does: begin, link, switch, reload,
/// commit, enable. Panics on any error.
pub fn install(world: &World, service_name: &str, version_text: &str, spec: &Spec) -> Manifest {
    let manifest = world.package(service_name, version_text, spec);
    let placer = world.placer();
    let claims = hostkeeper_placed::Claims::of(placer.roots(), &manifest).expect("claims");
    let linking = placer.begin(&claims).expect("begin");
    placer.link(&linking).expect("link");
    placer
        .switch_current(&service(service_name), &version(version_text))
        .expect("switch");
    placer.reload().expect("reload");
    placer.commit(linking).expect("commit");
    placer.enable_run_units(&manifest).expect("enable");
    manifest
}
