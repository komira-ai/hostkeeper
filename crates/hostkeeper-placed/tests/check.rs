//! The refusal before anything changes and the write-ahead protocol of `Placer`: `check` and
//! `begin` refuse a path another service owns or something unlisted occupies, `begin` records
//! `pending`, `link` makes links into `current`, `switch_current` sets `previous` before
//! `current`, `reload` and `commit` finish.
//! Each case builds a scratch host and breaks one thing. `desired` and `relink_to` are in
//! `relink.rs`.

mod support;

use std::ffi::OsString;
use std::fmt::Debug;
use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use hostkeeper_config::manifest::Manifest;
use hostkeeper_placed::placer::is_temporary;
use hostkeeper_placed::testing::{FakeUnitControl, FsWrite, UnitCall};
use hostkeeper_placed::{
    Claims, FileKind, Fs, Owner, PathState, PlacedError, PlacedRecord, Placer, RealFs, Tombstone,
};
use support::{Node, Spec, WANTS, World, install, service, version};

const PACKAGES: &str = "/var/lib/hostkeeper/packages";
const PLACED_JSON: &str = "/var/lib/hostkeeper/placed.json";
const LOCK: &str = "/var/lib/hostkeeper/placed.lock";
const CONF: &str = "/etc/tierstore/tierstore.conf";
const A_CONF: &str = "/etc/x/a.conf";

/// One file of service `a` at `/etc/x/a.conf`.
fn a_conf() -> Spec {
    Spec::new().file("files/a.conf", A_CONF)
}

fn wants_path() -> String {
    format!("{WANTS}/tierstore@9301.service")
}

fn claims(world: &World, manifest: &Manifest) -> Claims {
    Claims::of(&world.roots, manifest).expect("the claims of a manifest")
}

/// The path and owner of a refusal; panics on anything else.
fn conflict<T: Debug>(result: Result<T, PlacedError>) -> (PathBuf, Owner) {
    match result {
        Err(PlacedError::PathConflict { path, owner }) => (path, owner),
        other => panic!("expected a path conflict, got {other:?}"),
    }
}

fn conflict_at(path: &str, owner: Owner) -> (PathBuf, Owner) {
    (PathBuf::from(path), owner)
}

fn package_link(world: &World, name: &str) -> Option<PathBuf> {
    world.read_link(format!("{PACKAGES}/tierstore/{name}"))
}

fn tierstore_dir(world: &World) {
    fs::create_dir_all(world.host(format!("{PACKAGES}/tierstore")))
        .expect("create the service directory");
}

fn make_parent(world: &World, path: &str) {
    let on_disk = world.host(path);
    fs::create_dir_all(on_disk.parent().expect("a parent")).expect("create the parent");
}

/// A filesystem over the real one whose reads of a link, or of a temporary name, fail.
struct Flaky {
    inner: RealFs,
    fail_read_link: bool,
    fail_temporary_kind: bool,
}

impl Flaky {
    fn under(world: &World, fail_read_link: bool, fail_temporary_kind: bool) -> Flaky {
        Flaky {
            inner: RealFs::new(world.root()),
            fail_read_link,
            fail_temporary_kind,
        }
    }
}

impl Fs for Flaky {
    fn host_path(&self, path: &Path) -> PathBuf {
        self.inner.host_path(path)
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.inner.read(path)
    }

    fn kind(&self, path: &Path) -> io::Result<Option<FileKind>> {
        let temporary = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(is_temporary);
        if self.fail_temporary_kind && temporary {
            return Err(io::Error::other("the fake cannot stat a temporary name"));
        }
        self.inner.kind(path)
    }

    fn read_link(&self, path: &Path) -> io::Result<PathBuf> {
        if self.fail_read_link {
            return Err(io::Error::other("the fake cannot read a link"));
        }
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
        self.inner.symlink(target, link)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.inner.remove_file(path)
    }

    fn create_dirs(&self, dir: &Path) -> io::Result<()> {
        self.inner.create_dirs(dir)
    }
}

/// Catches conflict detection that runs after `begin` wrote something, or that misses the owner
/// recorded in `placed.json`: service `b` claims the path service `a` has placed, `check` and
/// `begin` both name `a`, and the tree, `placed.json` and the log are the same as before.
#[test]
fn a_second_package_claiming_a_placed_path_is_refused_before_any_switch() {
    let world = World::new("claimed");
    install(&world, "a", "1.0.0", &a_conf());
    let b = world.package("b", "1.0.0", &a_conf());
    let wanted = claims(&world, &b);
    let before = world.snapshot();
    let events = world.log.events();
    let placer = world.placer();
    let owner = Owner::Service("a".to_owned());

    assert_eq!(
        conflict(placer.check(&wanted)),
        conflict_at(A_CONF, owner.clone())
    );
    assert_eq!(
        conflict(placer.begin(&wanted)),
        conflict_at(A_CONF, owner.clone())
    );
    let direct = hostkeeper_placed::check(&wanted, &world.record(), &world.fs);
    assert_eq!(conflict(direct), conflict_at(A_CONF, owner));

    let refusal = placer.check(&wanted).expect_err("b is refused");
    assert_eq!(
        refusal.to_string(),
        "/etc/x/a.conf is held by the service a"
    );
    assert_eq!(refusal.reason(), "path_conflict");
    assert_eq!(world.snapshot(), before);
    assert_eq!(world.log.events(), events);
    assert_eq!(world.record().paths[Path::new(A_CONF)].service, "a");
}

/// Catches a check that trusts the disk over the record: a path `a` only began, with no link
/// yet, is still `a`'s and `b` is refused.
#[test]
fn a_pending_path_of_another_service_is_refused_before_its_link_exists() {
    let world = World::new("pending-owner");
    let a = world.package("a", "1.0.0", &a_conf());
    let placer = world.placer();
    placer.begin(&claims(&world, &a)).expect("a begins");
    let b = world.package("b", "1.0.0", &a_conf());
    let wanted = claims(&world, &b);
    world.log.clear();

    assert_eq!(
        conflict(placer.check(&wanted)),
        conflict_at(A_CONF, Owner::Service("a".to_owned()))
    );
    assert_eq!(
        conflict(placer.begin(&wanted)),
        conflict_at(A_CONF, Owner::Service("a".to_owned()))
    );
    assert!(world.writes().is_empty());
    assert_eq!(world.record().paths[Path::new(A_CONF)].service, "a");
}

/// Runs `check` and `begin` for a package claiming `/etc/x/a.conf` where `occupy` put something
/// the record does not list: both refuse with `owner`, and nothing is written or changed.
fn refuses_occupant<T>(name: &str, occupy: impl FnOnce(&Path) -> T, owner: Owner) {
    let world = World::new(name);
    make_parent(&world, A_CONF);
    let _held = occupy(&world.host(A_CONF));
    let wanted = claims(&world, &world.package("a", "1.0.0", &a_conf()));
    let before = world.snapshot();
    let placer = world.placer();

    assert_eq!(
        conflict(placer.check(&wanted)),
        conflict_at(A_CONF, owner.clone())
    );
    assert_eq!(conflict(placer.begin(&wanted)), conflict_at(A_CONF, owner));
    assert_eq!(
        world.snapshot().get(Path::new(A_CONF)),
        before.get(Path::new(A_CONF)),
        "the occupant is untouched, bytes and inode"
    );
    assert!(world.writes().is_empty());
    assert!(world.placed_bytes().is_none());
}

/// Catches an overwrite of a regular file the record does not list.
#[test]
fn a_path_occupied_by_an_unlisted_file_is_refused_and_never_overwritten() {
    refuses_occupant(
        "occupied-file",
        |path| fs::write(path, b"the operator's file\n").expect("write a file"),
        Owner::File,
    );
}

/// Catches a directory treated as free: a directory at the claimed path is refused and kept.
#[test]
fn a_path_occupied_by_a_directory_is_refused() {
    refuses_occupant(
        "occupied-dir",
        |path| fs::create_dir(path).expect("make a directory"),
        Owner::Directory,
    );
}

/// Catches a link the record does not list taken as the claimant's: it is refused with its
/// target named.
#[test]
fn a_path_occupied_by_an_unlisted_symlink_is_refused() {
    refuses_occupant(
        "occupied-link",
        |path| symlink("/somewhere/else", path).expect("make a link"),
        Owner::Link(PathBuf::from("/somewhere/else")),
    );
}

/// Catches a named pipe treated as free or as a file: it is `Owner::Other`.
#[test]
fn a_path_occupied_by_a_named_pipe_is_refused_as_other() {
    refuses_occupant("fifo", support::fifo, Owner::Other);
}

/// Catches a record entry that blesses whatever is at its path: the record lists `a.conf` for
/// this service but a regular file replaced the link, so the new version is refused and the
/// file kept.
#[test]
fn a_listed_path_that_became_a_regular_file_is_refused() {
    let world = World::new("listed-file");
    install(&world, "a", "1.0.0", &a_conf());
    let on_disk = world.host(A_CONF);
    fs::remove_file(&on_disk).expect("remove the link");
    fs::write(&on_disk, b"edited by hand\n").expect("write a file");
    let wanted = claims(&world, &world.package("a", "1.1.0", &a_conf()));
    let before = world.snapshot();
    let events = world.log.events();
    let placer = world.placer();

    assert_eq!(
        conflict(placer.check(&wanted)),
        conflict_at(A_CONF, Owner::File)
    );
    assert_eq!(
        conflict(placer.begin(&wanted)),
        conflict_at(A_CONF, Owner::File)
    );
    assert_eq!(world.snapshot(), before);
    assert_eq!(world.log.events(), events);
    assert_eq!(world.record().paths[Path::new(A_CONF)].version, "1.0.0");
}

/// Catches a check that refuses a path whose link a crash never made: the record lists it for
/// this service and nothing is there, so the claim is accepted and begin re-records it.
#[test]
fn a_listed_path_whose_link_is_missing_is_accepted() {
    let world = World::new("listed-missing");
    install(&world, "a", "1.0.0", &a_conf());
    fs::remove_file(world.host(A_CONF)).expect("remove the link");
    let wanted = claims(&world, &world.package("a", "1.1.0", &a_conf()));
    let placer = world.placer();

    placer.check(&wanted).expect("the claimant's own path");
    placer.begin(&wanted).expect("begin");
    assert_eq!(world.record().paths[Path::new(A_CONF)].version, "1.1.0");
}

/// Catches an inspection failure swallowed as "free": a file where a directory of the path
/// should be is an `inspect` error naming the path, not a pass.
#[test]
fn a_path_that_cannot_be_inspected_is_an_io_error() {
    let world = World::new("inspect");
    make_parent(&world, "/etc/x");
    fs::write(world.host("/etc/x"), b"not a directory\n").expect("write a file");
    let wanted = claims(&world, &world.package("a", "1.0.0", &a_conf()));

    match world.placer().check(&wanted) {
        Err(PlacedError::Io { op, path, .. }) => {
            assert_eq!(op, "inspect");
            assert_eq!(path, Path::new(A_CONF));
        }
        other => panic!("expected an inspect error, got {other:?}"),
    }
}

/// Catches a link read failure reported as a free path: a link whose target cannot be read is
/// a `read the link` error, from `check` and from `current`.
#[test]
fn a_link_that_cannot_be_read_is_an_io_error() {
    let world = World::new("read-link");
    install(&world, "a", "1.0.0", &a_conf());
    let wanted = claims(&world, &world.package("a", "1.1.0", &a_conf()));
    let flaky = Flaky::under(&world, true, false);
    let placer = world.placer_on(&flaky);

    match placer.check(&wanted) {
        Err(PlacedError::Io { op, path, .. }) => {
            assert_eq!(op, "read the link");
            assert_eq!(path, Path::new(A_CONF));
        }
        other => panic!("expected a read error, got {other:?}"),
    }
    assert!(matches!(
        placer.current("a"),
        Err(PlacedError::Io {
            op: "read the link",
            ..
        })
    ));
}

/// Catches a claim of the same service seen as a conflict, or a version bump that writes
/// links: tierstore 1.5.0 over 1.4.2 is accepted, `begin` records its entries `pending` with
/// 1.5.0, `link` writes nothing, `switch_current` makes previous 1.4.2 and current 1.5.0, and
/// `commit` marks them `placed`.
#[test]
fn the_same_service_may_relink_its_own_paths_across_versions() {
    let world = World::new("relink-versions");
    install(&world, "tierstore", "1.4.2", &Spec::tierstore());
    let new = world.package("tierstore", "1.5.0", &Spec::tierstore());
    let wanted = claims(&world, &new);
    let placer = world.placer();

    placer.check(&wanted).expect("the same service's paths");
    let linking = placer.begin(&wanted).expect("begin");
    assert_eq!(linking.claims(), &wanted);
    let record = world.record();
    for claim in wanted.iter() {
        let entry = &record.paths[&claim.path];
        assert_eq!(entry.service, "tierstore");
        assert_eq!(entry.version, "1.5.0");
        assert_eq!(entry.kind, claim.kind);
        assert_eq!(entry.state, PathState::Pending);
        assert_eq!(entry.link_target, claim.link_target);
    }
    let wants = &record.paths[Path::new(&wants_path())];
    assert_eq!(wants.version, "1.4.2", "not in these claims, so untouched");
    assert_eq!(wants.state, PathState::Placed);

    world.log.clear();
    assert!(!placer.link(&linking).expect("link"), "already right");
    assert!(world.writes().is_empty());
    assert!(
        placer
            .switch_current(&service("tierstore"), &version("1.5.0"))
            .expect("switch")
    );
    assert_eq!(package_link(&world, "previous"), Some("1.4.2".into()));
    assert_eq!(package_link(&world, "current"), Some("1.5.0".into()));
    assert!(placer.commit(linking).expect("commit"));
    for claim in wanted.iter() {
        let entry = &world.record().paths[&claim.path].clone();
        assert_eq!(entry.state, PathState::Placed);
        assert_eq!(entry.version, "1.5.0");
    }
}

/// Catches a begin that writes again for a record that would not change: the second `begin`
/// of the same claims writes nothing and leaves `placed.json` as it was.
#[test]
fn a_repeated_begin_writes_nothing() {
    let world = World::new("begin-twice");
    let manifest = world.package("a", "1.0.0", &a_conf());
    let wanted = claims(&world, &manifest);
    let placer = world.placer();

    placer.begin(&wanted).expect("the first begin");
    assert!(!world.writes().is_empty());
    let bytes = world.placed_bytes();
    world.log.clear();
    placer.begin(&wanted).expect("the second begin");
    assert!(world.writes().is_empty());
    assert_eq!(world.placed_bytes(), bytes);
}

/// Catches an entry update that drops the keys of a newer writer: a begin for a new version
/// rewrites the entry and keeps the unknown keys of the entry and of the record.
#[test]
fn begin_updates_an_entry_and_keeps_the_keys_of_a_newer_writer() {
    let world = World::new("begin-extras");
    fs::write(
        world.host(PLACED_JSON),
        format!(
            "{{\"schema\":1,\"future_top\":true,\"paths\":{{\"{CONF}\":{{\"service\":\"tierstore\",\
             \"version\":\"1.4.2\",\"kind\":\"file\",\"state\":\"placed\",\
             \"link_target\":\"/old/target\",\"future_key\":7}}}}}}"
        ),
    )
    .expect("write placed.json");
    let spec = Spec::new().file("files/tierstore.conf", CONF);
    let manifest = world.package("tierstore", "1.5.0", &spec);
    let wanted = claims(&world, &manifest);

    world.placer().begin(&wanted).expect("begin");
    let bytes = world.placed_bytes().expect("placed.json");
    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    let entry = &value["paths"][CONF];
    assert_eq!(entry["version"], "1.5.0");
    assert_eq!(entry["state"], "pending");
    assert_eq!(
        entry["link_target"],
        format!("{PACKAGES}/tierstore/current/files/tierstore.conf").as_str()
    );
    assert_eq!(entry["future_key"], 7);
    assert_eq!(value["future_top"], true);
}

/// Catches `commit` marking a path another version or service recorded, or reporting a write
/// when nothing changed: a commit after the record lost the entries returns false, and a
/// commit of paths already `placed` returns false.
#[test]
fn commit_changes_only_the_pending_entries_of_its_version() {
    let world = World::new("commit-false");
    let manifest = world.package("a", "1.0.0", &a_conf());
    let wanted = claims(&world, &manifest);
    let placer = world.placer();

    let linking = placer.begin(&wanted).expect("begin");
    fs::remove_file(world.host(PLACED_JSON)).expect("remove the record");
    assert!(!placer.commit(linking).expect("commit of nothing"));

    let linking = placer.begin(&wanted).expect("begin again");
    assert!(placer.commit(linking).expect("commit"));
    let linking = placer.begin(&wanted).expect("begin of a placed path");
    let bytes = world.placed_bytes();
    world.log.clear();
    assert!(!placer.commit(linking).expect("commit of a placed path"));
    assert!(world.writes().is_empty());
    assert_eq!(world.placed_bytes(), bytes);
}

/// Catches a begin that records over a record it cannot read: a corrupt `placed.json` is
/// `Corrupt` from `check` and from `begin`, and is left as it is.
#[test]
fn a_corrupt_record_refuses_check_and_begin_and_is_left_alone() {
    let world = World::new("corrupt");
    fs::write(world.host(PLACED_JSON), b"not json").expect("write placed.json");
    let wanted = claims(&world, &world.package("a", "1.0.0", &a_conf()));
    let placer = world.placer();

    assert!(matches!(
        placer.check(&wanted),
        Err(PlacedError::Corrupt { .. })
    ));
    assert!(matches!(
        placer.begin(&wanted),
        Err(PlacedError::Corrupt { .. })
    ));
    assert_eq!(world.placed_bytes(), Some(b"not json".to_vec()));
    assert!(world.writes().is_empty());
}

/// Catches a failed write of the record reported as success: with the first write failing,
/// `begin` is an `Io` error and nothing is recorded; a `commit` whose write fails is an `Io`
/// error and leaves the entries `pending`.
#[test]
fn a_failed_write_of_the_record_fails_begin_and_commit() {
    let world = World::new("record-write");
    let wanted = claims(&world, &world.package("a", "1.0.0", &a_conf()));

    let failing = world.failing_at(1);
    let result = world.placer_on(&failing).begin(&wanted);
    assert!(matches!(result, Err(PlacedError::Io { op: "write", .. })));
    assert!(world.placed_bytes().is_none());

    let linking = world.placer().begin(&wanted).expect("begin");
    let failing = world.failing_at(1);
    let result = world.placer_on(&failing).commit(linking);
    assert!(matches!(result, Err(PlacedError::Io { op: "write", .. })));
    assert_eq!(
        world.record().paths[Path::new(A_CONF)].state,
        PathState::Pending
    );
}

/// Catches a link made out of `current`, which a switch would not move: every placed path of
/// the claims reads back as a link starting with the service's `current` directory, and one
/// change of `current` moves what each resolves to.
#[test]
fn every_placed_link_is_relative_to_current_not_a_version_directory() {
    let world = World::new("current-relative");
    install(&world, "tierstore", "1.4.2", &Spec::tierstore());
    let manifest = install(&world, "tierstore", "1.5.0", &Spec::tierstore());
    let current = format!("{PACKAGES}/tierstore/current/");

    let mut seen = 0;
    for claim in claims(&world, &manifest).iter() {
        let target = world.read_link(&claim.path).expect("a placed link");
        assert!(
            target.starts_with(&current),
            "{} points at {}",
            claim.path.display(),
            target.display()
        );
        seen += 1;
    }
    assert_eq!(seen, 2);
    assert_eq!(world.placed_links().len(), 3, "the wants link too");
    let conf = format!("{PACKAGES}/tierstore/1.5.0/files/tierstore.conf");
    assert_eq!(world.resolve(CONF), Some(PathBuf::from(conf)));
    world.set_link("tierstore", "current", "1.4.2");
    let old = format!("{PACKAGES}/tierstore/1.4.2/files/tierstore.conf");
    assert_eq!(world.resolve(CONF), Some(PathBuf::from(old)));
}

/// Catches a service being uninstalled getting linked again: with a tombstone saved, `begin`
/// is `Tombstoned`, writes nothing and places nothing; the tombstone is read back from the
/// store as it was saved.
#[test]
fn a_tombstoned_service_cannot_be_linked() {
    let world = World::new("tombstone");
    let manifest = world.package("tierstore", "1.5.0", &Spec::tierstore());
    let wanted = claims(&world, &manifest);
    let placer = world.placer();
    let mut record = PlacedRecord::default();
    record
        .uninstalling
        .insert("tierstore".to_owned(), Tombstone::default());
    placer.store().save(&record).expect("save the tombstone");
    assert_eq!(placer.store().load().expect("load"), record);
    let bytes = world.placed_bytes();
    world.log.clear();

    match placer.begin(&wanted) {
        Err(PlacedError::Tombstoned { service }) => assert_eq!(service, "tierstore"),
        other => panic!("expected a tombstone refusal, got {other:?}"),
    }
    assert!(world.writes().is_empty());
    assert_eq!(world.placed_bytes(), bytes);
    assert!(world.placed_links().is_empty());
    let refusal = placer.begin(&wanted).expect_err("still refused");
    assert_eq!(refusal.reason(), "uninstalling");
}

/// Catches a tombstone of one service blocking another: `other` is being uninstalled and
/// service `a` still begins.
#[test]
fn a_tombstone_of_another_service_does_not_block() {
    let world = World::new("tombstone-other");
    let wanted = claims(&world, &world.package("a", "1.0.0", &a_conf()));
    let placer = world.placer();
    let mut record = PlacedRecord::default();
    record
        .uninstalling
        .insert("other".to_owned(), Tombstone::default());
    placer.store().save(&record).expect("save the tombstone");

    placer.begin(&wanted).expect("begin");
    assert!(world.record().uninstalling.contains_key("other"));
    assert!(world.record().paths.contains_key(Path::new(A_CONF)));
}

/// Catches a lock that is not taken: while another holder has `placed.lock`, `begin`, `reload`
/// and `commit` are `Locked` naming the lock file and change nothing; once it is released they
/// run.
#[test]
fn the_lock_makes_begin_reload_and_commit_refuse() {
    let world = World::new("locked");
    let wanted = claims(&world, &world.package("a", "1.0.0", &a_conf()));
    let placer = world.placer();
    let lock = world.host(LOCK);
    let hold = || {
        let file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock)
            .expect("open the lock file");
        file.try_lock().expect("take the lock");
        file
    };

    let holder = hold();
    match placer.begin(&wanted) {
        Err(PlacedError::Locked { path, .. }) => assert_eq!(path, Path::new(LOCK)),
        other => panic!("expected a locked refusal, got {other:?}"),
    }
    assert!(matches!(placer.reload(), Err(PlacedError::Locked { .. })));
    assert!(world.writes().is_empty());
    assert!(world.unit_calls().is_empty());
    assert!(world.placed_bytes().is_none());
    drop(holder);

    let linking = placer.begin(&wanted).expect("begin once released");
    let holder = hold();
    let refusal = placer.commit(linking).expect_err("commit under the lock");
    assert!(matches!(refusal, PlacedError::Locked { .. }));
    assert_eq!(refusal.reason(), "link_failed");
    assert_eq!(
        world.record().paths[Path::new(A_CONF)].state,
        PathState::Pending
    );
    drop(holder);
    placer.reload().expect("reload once released");
    assert_eq!(world.unit_calls(), [UnitCall::DaemonReload]);
}

/// Catches a missing state directory read as an unlocked host: with no state directory the
/// lock file cannot be opened, and `begin` is an `Io` error of `open` naming it.
#[test]
fn a_missing_state_directory_fails_begin_on_the_open_of_the_lock() {
    let world = World::new("no-state");
    let wanted = claims(&world, &world.package("a", "1.0.0", &a_conf()));
    fs::remove_dir_all(world.host("/var/lib/hostkeeper")).expect("remove the state directory");

    match world.placer().begin(&wanted) {
        Err(PlacedError::Io { op, path, .. }) => {
            assert_eq!(op, "open");
            assert_eq!(path, Path::new(LOCK));
        }
        other => panic!("expected an open error, got {other:?}"),
    }
}

/// Catches `current` and `previous` read as versions when something else is there: a regular
/// file at `current` and a directory at `previous` are path conflicts naming them, and
/// `switch_current` surfaces both without writing.
#[test]
fn a_current_or_previous_that_is_not_a_link_is_a_path_conflict() {
    let world = World::new("not-a-link");
    tierstore_dir(&world);
    let placer = world.placer();
    let current = format!("{PACKAGES}/tierstore/current");
    let previous = format!("{PACKAGES}/tierstore/previous");
    assert_eq!(placer.current("tierstore").expect("no link"), None);
    assert_eq!(placer.previous("tierstore").expect("no link"), None);

    fs::write(world.host(&current), b"a file\n").expect("write a file");
    assert_eq!(
        conflict(placer.current("tierstore")),
        conflict_at(&current, Owner::File)
    );
    let to = version("1.5.0");
    assert_eq!(
        conflict(placer.switch_current(&service("tierstore"), &to)),
        conflict_at(&current, Owner::File)
    );

    fs::remove_file(world.host(&current)).expect("remove the file");
    world.set_link("tierstore", "current", "1.4.2");
    fs::create_dir(world.host(&previous)).expect("make a directory");
    assert_eq!(
        conflict(placer.previous("tierstore")),
        conflict_at(&previous, Owner::Directory)
    );
    world.log.clear();
    assert_eq!(
        conflict(placer.switch_current(&service("tierstore"), &to)),
        conflict_at(&previous, Owner::Directory)
    );
    assert!(world.writes().is_empty());
    assert_eq!(package_link(&world, "current"), Some("1.4.2".into()));
}

/// Catches `current` written before `previous`: with the write of `current` failing, previous
/// is already 1.4.2 and current is still 1.4.2; a repeat finishes it; a further repeat
/// returns false and writes nothing.
#[test]
fn switch_current_sets_previous_before_current_and_a_repeat_changes_nothing() {
    let world = World::new("switch-order");
    tierstore_dir(&world);
    world.set_link("tierstore", "current", "1.4.2");
    world.set_link("tierstore", "previous", "1.3.0");
    let name = service("tierstore");
    let to = version("1.5.0");

    // previous: symlink, rename, fsync of the directory; the fourth write is current's link.
    let crashing = world.failing_at(4);
    let result = world.placer_on(&crashing).switch_current(&name, &to);
    assert!(matches!(result, Err(PlacedError::Io { op: "link", .. })));
    assert_eq!(package_link(&world, "previous"), Some("1.4.2".into()));
    assert_eq!(package_link(&world, "current"), Some("1.4.2".into()));

    world.log.clear();
    let placer = world.placer();
    assert!(placer.switch_current(&name, &to).expect("the repeat"));
    assert_eq!(package_link(&world, "previous"), Some("1.4.2".into()));
    assert_eq!(package_link(&world, "current"), Some("1.5.0".into()));
    let dir = PathBuf::from(format!("{PACKAGES}/tierstore"));
    assert_eq!(
        world.writes(),
        [
            FsWrite::Symlink {
                target: "1.5.0".into(),
                link: dir.join(".hk-current.tmp"),
            },
            FsWrite::Rename {
                from: dir.join(".hk-current.tmp"),
                to: dir.join("current"),
            },
            FsWrite::SyncDir(dir),
        ],
        "previous already names the old current, so only current is written"
    );

    world.log.clear();
    assert!(
        !placer
            .switch_current(&name, &to)
            .expect("the second repeat")
    );
    assert!(world.writes().is_empty());
}

/// Catches a first install writing `previous`: with no `current`, only `current` is written.
#[test]
fn a_first_install_writes_only_current() {
    let world = World::new("first-switch");
    tierstore_dir(&world);
    let placer = world.placer();
    let name = service("tierstore");

    assert!(
        placer
            .switch_current(&name, &version("1.5.0"))
            .expect("switch")
    );
    assert_eq!(package_link(&world, "current"), Some("1.5.0".into()));
    assert_eq!(package_link(&world, "previous"), None);
    let dir = PathBuf::from(format!("{PACKAGES}/tierstore"));
    assert_eq!(
        world.writes(),
        [
            FsWrite::Symlink {
                target: "1.5.0".into(),
                link: dir.join(".hk-current.tmp"),
            },
            FsWrite::Rename {
                from: dir.join(".hk-current.tmp"),
                to: dir.join("current"),
            },
            FsWrite::SyncDir(dir),
        ]
    );
}

/// Catches a stale temporary link in the way: a `.hk-<name>.tmp` a crash left is replaced, and
/// the link is made after the missing directory.
#[test]
fn a_stale_temporary_link_is_replaced() {
    let world = World::new("stale-tmp");
    let wanted = claims(&world, &world.package("a", "1.0.0", &a_conf()));
    let placer = world.placer();
    let linking = placer.begin(&wanted).expect("begin");
    make_parent(&world, A_CONF);
    symlink("/stale", world.host("/etc/x/.hk-a.conf.tmp")).expect("leave a temporary link");
    world.log.clear();

    assert!(placer.link(&linking).expect("link"));
    let tmp = PathBuf::from("/etc/x/.hk-a.conf.tmp");
    let dir = PathBuf::from("/etc/x");
    let target = PathBuf::from(format!("{PACKAGES}/a/current/files/a.conf"));
    assert_eq!(
        world.writes(),
        [
            FsWrite::CreateDirs(dir.clone()),
            FsWrite::Remove(tmp.clone()),
            FsWrite::Symlink {
                target: target.clone(),
                link: tmp.clone(),
            },
            FsWrite::Rename {
                from: tmp,
                to: PathBuf::from(A_CONF),
            },
            FsWrite::SyncDir(dir),
        ]
    );
    assert_eq!(world.read_link(A_CONF), Some(target));
    assert!(world.read_link("/etc/x/.hk-a.conf.tmp").is_none());
}

/// Catches a failed removal of a stale temporary reported as success, and a failed look at it:
/// the removal failing is an `Io` error of `remove`, and a temporary name that cannot be
/// inspected is an `Io` error of `inspect` naming it.
#[test]
fn a_stale_temporary_that_cannot_be_dealt_with_fails_the_link() {
    let world = World::new("stale-tmp-fail");
    let wanted = claims(&world, &world.package("a", "1.0.0", &a_conf()));
    let linking = world.placer().begin(&wanted).expect("begin");
    make_parent(&world, A_CONF);
    symlink("/stale", world.host("/etc/x/.hk-a.conf.tmp")).expect("leave a temporary link");

    let failing = world.failing_at(2);
    let result = world.placer_on(&failing).link(&linking);
    assert!(matches!(result, Err(PlacedError::Io { op: "remove", .. })));
    assert!(world.read_link(A_CONF).is_none());

    let flaky = Flaky::under(&world, false, true);
    match world.placer_on(&flaky).link(&linking) {
        Err(PlacedError::Io { op, path, .. }) => {
            assert_eq!(op, "inspect");
            assert_eq!(path, Path::new("/etc/x/.hk-a.conf.tmp"));
        }
        other => panic!("expected an inspect error, got {other:?}"),
    }
}

/// Catches a link that is half made after a crash and called done: each of the four writes of a
/// fresh link failing is an `Io` error of its step, and no link is at the path until the
/// directory is synced.
#[test]
fn every_write_of_a_fresh_link_can_fail() {
    let steps = ["create", "link", "rename", "fsync"];
    for (at, op) in steps.iter().enumerate() {
        let n = at as u64 + 1;
        let world = World::new(&format!("link-fault-{n}"));
        let wanted = claims(&world, &world.package("a", "1.0.0", &a_conf()));
        let linking = world.placer().begin(&wanted).expect("begin");
        let failing = world.failing_at(n);

        match world.placer_on(&failing).link(&linking) {
            Err(PlacedError::Io { op: got, .. }) => assert_eq!(got, *op, "write {n}"),
            other => panic!("write {n} should fail, got {other:?}"),
        }
        assert_eq!(world.read_link(A_CONF).is_some(), n == 4, "write {n}");
    }
}

/// Catches a link that is right being rewritten, and a wrong link being kept: a repeat of
/// `link` returns false and writes nothing, and a listed link pointing elsewhere is pointed
/// back at `current`.
#[test]
fn link_is_idempotent_and_repairs_a_wrong_link_the_record_lists() {
    let world = World::new("link-repair");
    install(&world, "a", "1.0.0", &a_conf());
    let wanted = claims(&world, &world.package("a", "1.1.0", &a_conf()));
    let placer = world.placer();
    let linking = placer.begin(&wanted).expect("begin");
    world.log.clear();
    assert!(!placer.link(&linking).expect("link"));
    assert!(world.writes().is_empty());

    fs::remove_file(world.host(A_CONF)).expect("remove the link");
    symlink("/old/target", world.host(A_CONF)).expect("make a wrong link");
    assert!(placer.link(&linking).expect("link"));
    assert_eq!(
        world.read_link(A_CONF),
        Some(PathBuf::from(format!("{PACKAGES}/a/current/files/a.conf")))
    );
}

/// Catches a link written over something that arrived after `begin`: a regular file and then a
/// directory at the path make `link` a conflict, untouched.
#[test]
fn link_never_replaces_what_arrived_after_begin() {
    let world = World::new("link-conflict");
    let wanted = claims(&world, &world.package("a", "1.0.0", &a_conf()));
    let placer = world.placer();
    let linking = placer.begin(&wanted).expect("begin");
    make_parent(&world, A_CONF);
    let on_disk = world.host(A_CONF);

    fs::write(&on_disk, b"arrived late\n").expect("write a file");
    let before = world.snapshot();
    assert_eq!(
        conflict(placer.link(&linking)),
        conflict_at(A_CONF, Owner::File)
    );
    assert_eq!(world.snapshot(), before);

    fs::remove_file(&on_disk).expect("remove the file");
    fs::create_dir(&on_disk).expect("make a directory");
    assert_eq!(
        conflict(placer.link(&linking)),
        conflict_at(A_CONF, Owner::Directory)
    );
    assert_eq!(world.snapshot().get(Path::new(A_CONF)), Some(&Node::Dir));
}

/// Catches a reload that is not asked for or whose failure is lost: `reload` calls
/// `daemon-reload` once, and a failing service manager is `Units`, reported as
/// `daemon_reload_failed`.
#[test]
fn reload_asks_for_one_daemon_reload_and_reports_its_failure() {
    let world = World::new("reload");
    world.placer().reload().expect("reload");
    assert_eq!(world.unit_calls(), [UnitCall::DaemonReload]);

    let failing = FakeUnitControl::failing(world.log.clone());
    let placer = Placer::new(world.roots.clone(), &world.fs, &failing);
    let refusal = placer.reload().expect_err("the reload fails");
    assert!(matches!(refusal, PlacedError::Units(_)));
    assert_eq!(refusal.reason(), "daemon_reload_failed");
}
