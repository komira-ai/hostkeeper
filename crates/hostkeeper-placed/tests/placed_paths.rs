//! How `Placer` makes and reads its links: a missing link is made at its own path in one step,
//! temporary names cannot be the name of a placed path, `current` and `previous` must name a
//! version beside them, units are linted at `begin`, one service's path blocks the paths above
//! and below it, a directory can be a source, and `placed.json` is written under its lock with
//! the reload owed recorded in it. Each case builds a scratch host and breaks one thing.

mod support;

use std::ffi::OsString;
use std::fmt::Debug;
use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use hostkeeper_placed::placer::{MAX_SPELLED_OWNER, is_temporary, temporary_name};
use hostkeeper_placed::testing::{FakeUnitControl, FsWrite, UnitCall};
use hostkeeper_placed::{
    Claims, FileKind, Fs, LintError, Owner, PathState, PlacedError, Placer, RealFs, Tombstone,
};
use support::{Spec, World, install, manifest_text, service, unit_text, version};

const PACKAGES: &str = "/var/lib/hostkeeper/packages";
const LOCK: &str = "/var/lib/hostkeeper/placed.lock";
const A_CONF: &str = "/etc/x/a.conf";
const RACED: &[u8] = b"arrived between the look and the link\n";

fn a_conf() -> Spec {
    Spec::new().file("files/a.conf", A_CONF)
}

fn conflict<T: Debug>(result: Result<T, PlacedError>) -> (PathBuf, Owner) {
    match result {
        Err(PlacedError::PathConflict { path, owner }) => (path, owner),
        other => panic!("expected a path conflict, got {other:?}"),
    }
}

fn conflict_at(path: &str, owner: Owner) -> (PathBuf, Owner) {
    (PathBuf::from(path), owner)
}

fn claims(world: &World, service_name: &str, version_text: &str, spec: &Spec) -> Claims {
    let manifest = world.package(service_name, version_text, spec);
    Claims::of(&world.roots, &manifest).expect("the claims of a manifest")
}

/// A filesystem over the real one that, when a link is made at `at`, first puts a regular file
/// there, as a process that got there between the look and the link would.
struct RacingFs {
    inner: RealFs,
    at: PathBuf,
    inode: Mutex<Option<u64>>,
}

impl RacingFs {
    fn under(world: &World, at: &str) -> RacingFs {
        RacingFs {
            inner: RealFs::new(world.root()),
            at: PathBuf::from(at),
            inode: Mutex::new(None),
        }
    }

    /// The inode of the file that raced in, `None` when none did.
    fn raced_inode(&self) -> Option<u64> {
        *self.inode.lock().expect("the inode lock")
    }
}

impl Fs for RacingFs {
    fn host_path(&self, path: &Path) -> PathBuf {
        self.inner.host_path(path)
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.inner.read(path)
    }

    fn kind(&self, path: &Path) -> io::Result<Option<FileKind>> {
        self.inner.kind(path)
    }

    fn read_link(&self, path: &Path) -> io::Result<PathBuf> {
        self.inner.read_link(path)
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<OsString>> {
        self.inner.list(dir)
    }

    fn write(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        self.inner.write(path, bytes)
    }

    fn sync_file(&self, path: &Path) -> io::Result<()> {
        self.inner.sync_file(path)
    }

    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        self.inner.sync_dir(dir)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.inner.rename(from, to)
    }

    fn symlink(&self, target: &Path, link: &Path) -> io::Result<()> {
        if link == self.at {
            let disk = self.inner.host_path(link);
            fs::write(&disk, RACED)?;
            *self.inode.lock().expect("the inode lock") = Some(fs::metadata(&disk)?.ino());
        }
        self.inner.symlink(target, link)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.inner.remove_file(path)
    }

    fn remove_dir(&self, dir: &Path) -> io::Result<()> {
        self.inner.remove_dir(dir)
    }

    fn create_dirs(&self, dir: &Path) -> io::Result<()> {
        self.inner.create_dirs(dir)
    }
}

/// Catches a temporary name that a placed path can have: service `a` has `/etc/x/.hk-a.conf.tmp`
/// placed (the name the old scheme used for the link of `/etc/x/a.conf`); service `b` then places
/// `/etc/x/a.conf` and even repoints it, and the link of `a`, its bytes and its entry are as
/// they were.
#[test]
fn a_recorded_path_named_like_an_old_temporary_survives_another_service_placing() {
    let world = World::new("old-temporary-name");
    let old = "/etc/x/.hk-a.conf.tmp";
    install(
        &world,
        "a",
        "1.0.0",
        &Spec::new().file("files/old.conf", old),
    );
    let link = world.read_link(old);
    let bytes = fs::read(
        world
            .resolve(old)
            .map(|at| world.host(at))
            .expect("resolves"),
    );
    let entry = world.record().paths[Path::new(old)].clone();

    install(&world, "b", "1.0.0", &a_conf());
    let wanted = claims(&world, "b", "1.0.0", &a_conf());
    fs::remove_file(world.host(A_CONF)).expect("remove b's link");
    symlink("/old/target", world.host(A_CONF)).expect("make a wrong link");
    let placer = world.placer();
    let linking = placer.begin(&wanted).expect("begin");
    assert!(placer.link(&linking).expect("link repoints"));

    assert_eq!(world.read_link(old), link);
    let after = fs::read(
        world
            .resolve(old)
            .map(|at| world.host(at))
            .expect("resolves"),
    );
    assert_eq!(after.expect("bytes"), bytes.expect("bytes"));
    assert_eq!(world.record().paths[Path::new(old)], entry);
    assert_eq!(entry.state, PathState::Placed);
    assert_eq!(world.record().paths[Path::new(A_CONF)].service, "b");
}

/// Catches a link made by temp-and-rename over whatever got there: a regular file that appears
/// at the destination between the look and the link makes `link` a `PathConflict` of
/// `Owner::File`, and the file's bytes and inode are the ones it arrived with, with no temporary
/// link left in the directory.
#[test]
fn a_file_that_appears_before_the_link_is_made_is_a_conflict_and_is_kept() {
    let world = World::new("race");
    let wanted = claims(&world, "a", "1.0.0", &a_conf());
    let linking = world.placer().begin(&wanted).expect("begin");
    let racing = RacingFs::under(&world, A_CONF);

    let result = world.placer_on(&racing).link(&linking);
    assert_eq!(conflict(result), conflict_at(A_CONF, Owner::File));

    let inode = racing.raced_inode().expect("the file raced in");
    let snapshot = world.snapshot();
    assert_eq!(
        snapshot.get(Path::new(A_CONF)),
        Some(&support::Node::File {
            bytes: RACED.to_vec(),
            inode
        })
    );
    let temporaries = snapshot
        .keys()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_temporary)
        })
        .count();
    assert_eq!(temporaries, 0);
}

/// Catches the write sequence of a fresh link: the missing directories, the link at its own
/// path (target `current/...`), then the fsync of the directory, and no rename.
#[test]
fn a_missing_link_is_made_at_its_own_path_in_three_writes() {
    let world = World::new("fresh-sequence");
    let wanted = claims(&world, "a", "1.0.0", &a_conf());
    let placer = world.placer();
    let linking = placer.begin(&wanted).expect("begin");
    world.log.clear();

    assert!(placer.link(&linking).expect("link"));
    let target = PathBuf::from(format!("{PACKAGES}/a/current/files/a.conf"));
    assert_eq!(
        world.writes(),
        [
            FsWrite::CreateDirs(PathBuf::from("/etc/x")),
            FsWrite::Symlink {
                target,
                link: PathBuf::from(A_CONF),
            },
            FsWrite::SyncDir(PathBuf::from("/etc/x")),
        ]
    );
}

/// Catches a destination basename too long for a name derived from it: a link whose name is 250
/// bytes is placed, and repointed through the short temporary name of its service.
#[test]
fn a_250_byte_destination_name_links_and_repoints() {
    let world = World::new("long-name");
    let path = format!("/etc/x/{}", "c".repeat(250));
    assert_eq!(Path::new(&path).file_name().map(|n| n.len()), Some(250));
    let spec = Spec::new().file("files/long.conf", &path);
    install(&world, "a", "1.0.0", &spec);
    let target = PathBuf::from(format!("{PACKAGES}/a/current/files/long.conf"));
    assert_eq!(world.read_link(&path), Some(target.clone()));
    assert!(world.record().paths.contains_key(Path::new(&path)));

    fs::remove_file(world.host(&path)).expect("remove the link");
    symlink("/old/target", world.host(&path)).expect("make a wrong link");
    let wanted = claims(&world, "a", "1.0.0", &spec);
    let placer = world.placer();
    let linking = placer.begin(&wanted).expect("begin");
    world.log.clear();
    assert!(placer.link(&linking).expect("link repoints"));
    assert_eq!(world.read_link(&path), Some(target));
    let tmp = PathBuf::from("/etc/x/.hk~a~tmp");
    assert!(world.writes().contains(&FsWrite::Rename {
        from: tmp,
        to: PathBuf::from(&path),
    }));
}

/// Catches `current` or `previous` taken as a version when its link names something else: a
/// target outside the service directory, an absolute path to a local version directory or a
/// path inside a version is a `PathConflict` of `Owner::Link(target)` from `current()`,
/// `previous()` and `switch_current`, which write nothing and keep the link.
#[test]
fn a_current_or_previous_that_names_more_than_a_version_is_a_path_conflict() {
    let current = format!("{PACKAGES}/tierstore/current");
    let previous = format!("{PACKAGES}/tierstore/previous");
    let absolute = format!("{PACKAGES}/tierstore/1.5.0");
    let targets = [
        ("outside", "../other/1.5.0"),
        ("absolute", absolute.as_str()),
        ("inside", "1.5.0/bin"),
    ];
    for (name, target) in targets {
        let world = World::new(&format!("link-target-{name}"));
        world.package("tierstore", "1.5.0", &Spec::tierstore());
        world.set_link("tierstore", "current", target);
        let placer = world.placer();
        world.log.clear();
        let expected = conflict_at(&current, Owner::Link(PathBuf::from(target)));

        assert_eq!(conflict(placer.current("tierstore")), expected, "{name}");
        let result = placer.switch_current(&service("tierstore"), &version("1.5.0"));
        assert_eq!(conflict(result), expected, "{name}");
        assert!(world.writes().is_empty(), "{name}");
        assert_eq!(world.read_link(&current), Some(PathBuf::from(target)));
        assert!(world.read_link(&previous).is_none());
    }

    let world = World::new("link-target-previous");
    fs::create_dir_all(world.host(format!("{PACKAGES}/tierstore"))).expect("create the service");
    world.set_link("tierstore", "current", "1.4.2");
    world.set_link("tierstore", "previous", "../other/1.4.2");
    let placer = world.placer();
    world.log.clear();
    let expected = conflict_at(&previous, Owner::Link(PathBuf::from("../other/1.4.2")));
    assert_eq!(conflict(placer.previous("tierstore")), expected);
    let result = placer.switch_current(&service("tierstore"), &version("1.5.0"));
    assert_eq!(conflict(result), expected);
    assert!(world.writes().is_empty());
}

/// The spec of a tierstore whose unit pins the version directory of 1.5.0.
fn pinned() -> Spec {
    let exec = format!("{PACKAGES}/tierstore/1.5.0/bin/tierstore serve --port %i");
    Spec::new()
        .unit("tierstore@.service", &unit_text(&exec))
        .run("tierstore@9301.service")
}

/// Catches `begin` recording a package whose unit runs a pinned version directory: the
/// `ExecStart=` naming `<packages>/tierstore/1.5.0/...` is `Lint(PackagePath)` naming file, key,
/// path and service, and no write is logged, `placed.json` is as it was and the link stays.
#[test]
fn begin_refuses_a_unit_pinned_to_a_version_directory() {
    let world = World::new("begin-pinned");
    install(&world, "tierstore", "1.4.2", &Spec::tierstore());
    let wanted = claims(&world, "tierstore", "1.5.0", &pinned());
    let bytes = world.placed_bytes();
    let before = world.snapshot();
    world.log.clear();

    match world.placer().begin(&wanted) {
        Err(PlacedError::Lint(LintError::PackagePath {
            file,
            key,
            path,
            service,
            ..
        })) => {
            assert_eq!(file, "tierstore@.service");
            assert_eq!(key, "ExecStart=");
            assert_eq!(path, format!("{PACKAGES}/tierstore/1.5.0/bin/tierstore"));
            assert_eq!(service, "tierstore");
        }
        other => panic!("expected a package path refusal, got {other:?}"),
    }
    assert!(world.writes().is_empty());
    assert_eq!(world.placed_bytes(), bytes);
    assert_eq!(world.snapshot(), before);
}

/// Catches `begin` recording a unit it cannot read: a unit file that is gone from the version
/// directory is `Lint(Unreadable)` naming it, with no write logged and no record made.
#[test]
fn begin_refuses_a_unit_file_it_cannot_read() {
    let world = World::new("begin-unreadable");
    let wanted = claims(&world, "tierstore", "1.5.0", &Spec::tierstore());
    let file = world
        .version_dir("tierstore", "1.5.0")
        .join("units/tierstore@.service");
    fs::remove_file(file).expect("remove the unit file");
    world.log.clear();

    match world.placer().begin(&wanted) {
        Err(PlacedError::Lint(LintError::Unreadable { file, .. })) => {
            assert_eq!(file, "tierstore@.service");
        }
        other => panic!("expected an unreadable refusal, got {other:?}"),
    }
    assert!(world.writes().is_empty());
    assert!(world.placed_bytes().is_none());
}

/// Catches a claim next to another service's path: with `/etc/shared` recorded for `a`, `b`
/// claiming `/etc/shared/child.conf` is refused as `Owner::Service("a")` by `check` and `begin`,
/// and with `/etc/shared/child.conf` recorded for `a`, `b` claiming `/etc/shared` is refused
/// the same; nothing is written either time.
#[test]
fn another_services_path_above_or_below_a_claim_refuses_it() {
    let owner = Owner::Service("a".to_owned());

    let world = World::new("shared-above");
    install(
        &world,
        "a",
        "1.0.0",
        &Spec::new().file("files/shared", "/etc/shared"),
    );
    let child = Spec::new().file("files/child.conf", "/etc/shared/child.conf");
    let wanted = claims(&world, "b", "1.0.0", &child);
    let bytes = world.placed_bytes();
    world.log.clear();
    let placer = world.placer();
    let expected = conflict_at("/etc/shared/child.conf", owner.clone());
    assert_eq!(conflict(placer.check(&wanted)), expected);
    assert_eq!(conflict(placer.begin(&wanted)), expected);
    assert!(world.writes().is_empty());
    assert_eq!(world.placed_bytes(), bytes);

    let world = World::new("shared-below");
    let child = Spec::new().file("files/child.conf", "/etc/shared/child.conf");
    install(&world, "a", "1.0.0", &child);
    let whole = Spec::new().file("files/shared", "/etc/shared");
    let wanted = claims(&world, "b", "1.0.0", &whole);
    let bytes = world.placed_bytes();
    world.log.clear();
    let placer = world.placer();
    let expected = conflict_at("/etc/shared", owner);
    assert_eq!(conflict(placer.check(&wanted)), expected);
    assert_eq!(conflict(placer.begin(&wanted)), expected);
    assert!(world.writes().is_empty());
    assert_eq!(world.placed_bytes(), bytes);
}

/// Catches a directory source linked file by file or removed with its contents: `files/conf.d/`
/// with two files, placed at `/etc/tierstore/conf.d`, is one link to `current/files/conf.d`,
/// both files resolve into the version directory, and `remove_all` removes the link only: the
/// package files stay and the record entry goes.
#[test]
fn a_directory_source_is_one_link_and_removal_leaves_the_package_files() {
    let world = World::new("directory-source");
    let at = "/etc/tierstore/conf.d";
    let spec = Spec::new().file("files/conf.d", at);
    let dir = world.version_dir("tierstore", "1.5.0");
    fs::create_dir_all(dir.join("bin")).expect("create bin/");
    fs::write(dir.join("bin/tierstore"), "#!/bin/false\n").expect("write the binary");
    fs::create_dir_all(dir.join("files/conf.d")).expect("create the source directory");
    fs::write(dir.join("files/conf.d/one.conf"), "one\n").expect("write a file");
    fs::write(dir.join("files/conf.d/two.conf"), "two\n").expect("write a file");
    fs::write(
        dir.join("manifest.toml"),
        manifest_text("tierstore", "1.5.0", &spec),
    )
    .expect("write the manifest");
    let manifest = world.load("tierstore", "1.5.0");
    let wanted = Claims::of(&world.roots, &manifest).expect("claims");
    let placer = world.placer();

    let linking = placer.begin(&wanted).expect("begin");
    assert!(placer.link(&linking).expect("link"));
    placer
        .switch_current(&service("tierstore"), &version("1.5.0"))
        .expect("switch");
    placer.reload().expect("reload");
    placer.commit(linking).expect("commit");

    assert_eq!(
        world.read_link(at),
        Some(PathBuf::from(format!(
            "{PACKAGES}/tierstore/current/files/conf.d"
        )))
    );
    let resolved = world.resolve(at).expect("the directory resolves");
    assert_eq!(
        resolved,
        PathBuf::from(format!("{PACKAGES}/tierstore/1.5.0/files/conf.d"))
    );
    assert!(world.host(&resolved).is_dir());
    for (file, text) in [("one.conf", "one\n"), ("two.conf", "two\n")] {
        let inside = world.resolve(format!("{at}/{file}")).expect("resolves");
        assert_eq!(fs::read_to_string(world.host(inside)).expect("read"), text);
    }

    let removal = placer.remove_all(&service("tierstore")).expect("remove");
    assert_eq!(removal.removed, [PathBuf::from(at)]);
    assert!(removal.left_alone.is_empty());
    assert!(fs::symlink_metadata(world.host(at)).is_err());
    assert!(world.record().paths.is_empty());
    for file in ["one.conf", "two.conf"] {
        assert!(dir.join("files/conf.d").join(file).is_file(), "{file}");
    }
}

/// Catches a reload owed that is not recorded, or not kept when the reload fails: `begin` writes
/// `reload_pending` into `placed.json`; a `reload` that succeeds removes it; a `begin` of a new
/// version sets it again, a failing service manager leaves it set, and a later working one clears
/// it; a reload with nothing owed writes nothing.
#[test]
fn begin_owes_a_reload_that_only_a_successful_reload_clears() {
    let world = World::new("reload-owed");
    let placer = world.placer();
    placer
        .begin(&claims(&world, "a", "1.0.0", &a_conf()))
        .expect("begin");
    let json: serde_json::Value =
        serde_json::from_slice(&world.placed_bytes().expect("placed.json")).expect("json");
    assert_eq!(json["reload_pending"], true);
    assert!(world.record().reload_pending);

    placer.reload().expect("reload");
    assert!(!world.record().reload_pending);
    let json: serde_json::Value =
        serde_json::from_slice(&world.placed_bytes().expect("placed.json")).expect("json");
    assert!(
        json.get("reload_pending").is_none(),
        "written only when set"
    );

    world.log.clear();
    placer.reload().expect("a reload with nothing owed");
    assert!(world.writes().is_empty());
    assert_eq!(world.unit_calls(), [UnitCall::DaemonReload]);

    placer
        .begin(&claims(&world, "a", "1.1.0", &a_conf()))
        .expect("begin of a new version");
    assert!(world.record().reload_pending);
    let failing = FakeUnitControl::failing(world.log.clone());
    let broken = Placer::new(world.roots.clone(), &world.fs, &failing);
    assert!(matches!(broken.reload(), Err(PlacedError::Units(_))));
    assert!(world.record().reload_pending, "still owed");

    placer.reload().expect("a working reload");
    assert!(!world.record().reload_pending);
}

/// Catches a record written without the lock or over another writer: while one lock is held a
/// second `lock()` and a `begin` are `Locked`; a tombstone saved under the lock, a `begin` of
/// another service after it, and a second tombstone saved under a new lock all survive in the
/// record.
#[test]
fn writes_under_the_lock_do_not_lose_each_other() {
    let world = World::new("lock-sequence");
    let placer = world.placer();
    let wanted = claims(&world, "a", "1.0.0", &a_conf());

    let lock = placer.lock().expect("the lock");
    match placer.lock() {
        Err(PlacedError::Locked { path, .. }) => assert_eq!(path, Path::new(LOCK)),
        other => panic!("expected the lock to be held, got {other:?}"),
    }
    assert!(matches!(
        placer.begin(&wanted),
        Err(PlacedError::Locked { .. })
    ));
    let mut record = placer.store().load().expect("load");
    record
        .uninstalling
        .insert("gone".to_owned(), Tombstone::default());
    placer.store().save(&record, &lock).expect("save");
    drop(lock);

    placer.begin(&wanted).expect("begin of another service");

    let lock = placer.lock().expect("the lock again");
    let mut record = placer.store().load().expect("load");
    record
        .uninstalling
        .insert("later".to_owned(), Tombstone::default());
    placer.store().save(&record, &lock).expect("save");
    drop(lock);

    let record = world.record();
    let tombstones: Vec<&str> = record.uninstalling.keys().map(String::as_str).collect();
    assert_eq!(tombstones, ["gone", "later"]);
    assert_eq!(record.paths[Path::new(A_CONF)].state, PathState::Pending);
}

/// Catches a temporary name that grows with the service name past a path component's 255 bytes
/// (a service name may be 255 bytes), or two long names sharing one temporary name: an owner of
/// up to 64 bytes is spelled out, a longer one is named by a hash, and every form is a
/// temporary name.
#[test]
fn temporary_names_stay_short_for_long_service_names() {
    assert_eq!(temporary_name("tierstore"), ".hk~tierstore~tmp");
    let spelled = "s".repeat(MAX_SPELLED_OWNER);
    assert_eq!(temporary_name(&spelled), format!(".hk~{spelled}~tmp"));
    let long = "s".repeat(MAX_SPELLED_OWNER + 1);
    let longest = "s".repeat(255);
    let other = format!("{}t", "s".repeat(254));
    for owner in [&long, &longest, &other] {
        let name = temporary_name(owner);
        assert_eq!(name.len(), ".hk~#~tmp".len() + 16, "{name}");
        assert!(name.starts_with(".hk~#") && is_temporary(&name), "{name}");
        assert_eq!(temporary_name(owner), name, "the same owner, the same name");
    }
    assert_ne!(temporary_name(&longest), temporary_name(&other));
    assert_ne!(temporary_name(&long), temporary_name(&longest));
}

/// Catches a switch that fails for a long service name because its temporary link name is too
/// long for the file system: `current` of a 200-byte service switches, `previous` first.
#[test]
fn a_service_with_a_200_byte_name_switches_current() {
    let world = World::new("long-service");
    let name = "s".repeat(200);
    let dir = world.host(format!("/var/lib/hostkeeper/packages/{name}"));
    fs::create_dir_all(dir.join("1.0.0")).expect("the old version");
    fs::create_dir_all(dir.join("2.0.0")).expect("the new version");
    world.set_link(&name, "current", "1.0.0");
    let switched = world
        .placer()
        .switch_current(&service(&name), &version("2.0.0"))
        .expect("switch");
    assert!(switched);
    assert_eq!(
        fs::read_link(dir.join("current")).expect("current"),
        Path::new("2.0.0")
    );
    assert_eq!(
        fs::read_link(dir.join("previous")).expect("previous"),
        Path::new("1.0.0")
    );
    let names: Vec<OsString> = fs::read_dir(&dir)
        .expect("list")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    assert!(
        names.iter().all(|n| !is_temporary(&n.to_string_lossy())),
        "{names:?}"
    );
}
