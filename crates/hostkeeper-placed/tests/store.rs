//! `placed.json` and the filesystem under it: the order and atomicity of a save, the keys of a
//! newer writer kept on rewrite, the records that are refused and left as they are, the tombstone
//! round trip, `RealFs` on every kind of file, and the directory accessors of `Roots`.

mod support;

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use hostkeeper_placed::fs::{FileKind, Fs, RealFs};
use hostkeeper_placed::roots::{LOCK_FILE, PACKAGES_DIR, PLACED_FILE, STATE_DIR, SYSTEMD_UNIT_DIR};
use hostkeeper_placed::store::{SCHEMA, TEMPORARY_FILE};
use hostkeeper_placed::testing::FsWrite;
use hostkeeper_placed::{
    Claims, Entry, PackageDirs, PathKind, PathState, PlacedError, PlacedRecord, Roots, Store,
    Tombstone,
};
use rustix::fs::{Mode, RawMode};
use rustix::process::umask;
use serde_json::{Value, json};
use support::{Spec, World};

const PLACED: &str = "/var/lib/hostkeeper/placed.json";
const TMP: &str = "/var/lib/hostkeeper/.hk~placed.json~tmp";
const STATE: &str = "/var/lib/hostkeeper";

fn entry(service: &str, version: &str) -> Entry {
    Entry::new(
        service,
        version,
        PathKind::File,
        PathState::Placed,
        Path::new("/var/lib/hostkeeper/packages/x/current/files/x.conf"),
    )
}

fn record_of(paths: &[(&str, Entry)]) -> PlacedRecord {
    let mut record = PlacedRecord::default();
    for (path, entry) in paths {
        record.paths.insert(PathBuf::from(path), entry.clone());
    }
    record
}

fn write_placed(world: &World, text: &str) {
    fs::write(world.host(PLACED), text).expect("write placed.json");
}

/// Saves `record` into the state directory `dir` through `fs`, holding the lock of the world's
/// `placed.json` for the call, as `Store::save` asks.
fn save_in(
    world: &World,
    fs: &dyn Fs,
    dir: &Path,
    record: &PlacedRecord,
) -> Result<(), PlacedError> {
    let lock = world.placer().lock().expect("the lock is free");
    Store::new(fs, dir).save(record, &lock)
}

fn io_parts(error: PlacedError) -> (&'static str, PathBuf) {
    match error {
        PlacedError::Io { op, path, .. } => (op, path),
        other => panic!("expected an Io error, got {other:?}"),
    }
}

/// Catches a save that writes `placed.json` in place, or in another order than the temporary
/// file, its fsync, the rename and the fsync of the directory; and a crash before the rename that
/// changes the old record.
#[test]
fn placed_json_write_is_temp_fsync_rename() {
    let world = World::new("store-order");
    let state = Path::new(STATE);
    save_in(&world, &world.fs, state, &PlacedRecord::default()).expect("first save");
    let old = world.placed_bytes().expect("placed.json exists");
    world.log.clear();

    let new = record_of(&[("/etc/x.conf", entry("x", "1.0.0"))]);
    save_in(&world, &world.fs, state, &new).expect("save");
    assert_eq!(
        world.writes(),
        vec![
            FsWrite::Write(PathBuf::from(TMP)),
            FsWrite::SyncFile(PathBuf::from(TMP)),
            FsWrite::Rename {
                from: PathBuf::from(TMP),
                to: PathBuf::from(PLACED),
            },
            FsWrite::SyncDir(PathBuf::from(STATE)),
        ]
    );
    assert_eq!(world.record(), new);
    assert!(!world.host(TMP).exists(), "the temporary file was renamed");
    assert_eq!(TMP, format!("{STATE}/{TEMPORARY_FILE}"));

    // A failure at any write before the rename leaves the old bytes.
    for n in 1..=3 {
        let world = World::new(&format!("store-crash-{n}"));
        save_in(&world, &world.fs, state, &PlacedRecord::default()).expect("first save");
        let old = world.placed_bytes().expect("placed.json exists");
        let failing = world.failing_at(n);
        let result = save_in(&world, &failing, state, &new);
        assert!(result.is_err(), "write {n} fails");
        assert_eq!(world.placed_bytes(), Some(old), "failure at write {n}");
    }
    assert!(!old.is_empty());
}

/// Catches an error that names the wrong step or path when each write of a save fails, and a
/// failed directory fsync that is swallowed.
#[test]
fn a_failed_write_of_a_save_names_its_step_and_path() {
    let state = Path::new(STATE);
    let new = record_of(&[("/etc/x.conf", entry("x", "1.0.0"))]);
    let expected = [
        (1, "write", TMP),
        (2, "fsync", TMP),
        (3, "rename", TMP),
        (4, "fsync", STATE),
    ];
    for (n, op, path) in expected {
        let world = World::new(&format!("store-step-{n}"));
        let failing = world.failing_at(n);
        let error = save_in(&world, &failing, state, &new).expect_err("fails");
        assert_eq!(io_parts(error), (op, PathBuf::from(path)), "write {n}");
    }
    // The record is on disk when only the directory fsync failed.
    let world = World::new("store-step-dir");
    let failing = world.failing_at(4);
    save_in(&world, &failing, state, &new).expect_err("fails");
    assert_eq!(world.record(), new);
}

/// Catches a save into a state directory that does not exist reporting success or a wrong path.
#[test]
fn a_save_into_a_missing_directory_is_an_io_error() {
    let world = World::new("store-missing-dir");
    let lock = world.placer().lock().expect("the lock is free");
    fs::remove_dir_all(world.host(STATE)).expect("remove the state directory under the lock");
    let error = Store::new(&world.fs, Path::new(STATE))
        .save(&PlacedRecord::default(), &lock)
        .expect_err("no such directory");
    assert_eq!(io_parts(error), ("write", PathBuf::from(TMP)));
}

/// Catches a record that holds a path which is not UTF-8 being written as a lossy or empty file
/// instead of refused before anything is written.
#[test]
fn a_record_that_cannot_be_serialised_is_refused_before_any_write() {
    let world = World::new("store-not-utf8");
    let bad = Path::new(OsStr::from_bytes(b"/etc/\xff.conf"));
    let mut record = PlacedRecord::default();
    record.paths.insert(bad.to_path_buf(), entry("x", "1.0.0"));
    let error = save_in(&world, &world.fs, Path::new(STATE), &record).expect_err("refused");
    match error {
        PlacedError::Corrupt { path, why } => {
            assert_eq!(path, PathBuf::from(PLACED));
            assert!(why.starts_with("the path /etc/"), "{why}");
        }
        other => panic!("expected Corrupt, got {other:?}"),
    }
    assert!(world.writes().is_empty());
    assert_eq!(world.placed_bytes(), None);
}

/// Catches a record type that drops the keys of a newer writer at the top level, in an entry or in
/// a tombstone when the record is written again.
#[test]
fn placed_json_preserves_keys_it_does_not_know() {
    let world = World::new("store-unknown");
    let original = json!({
        "schema": 1,
        "future_top": {"a": [1, 2, 3]},
        "paths": {
            "/etc/x.conf": {
                "service": "x",
                "version": "1.0.0",
                "kind": "file",
                "state": "placed",
                "link_target": "/var/lib/hostkeeper/packages/x/current/files/x.conf",
                "future_entry": "kept"
            }
        },
        "uninstalling": {"y": {"future_tombstone": 7}}
    });
    write_placed(&world, &original.to_string());
    let store = Store::new(&world.fs, Path::new(STATE));
    let record = store.load().expect("loads");
    save_in(&world, &world.fs, Path::new(STATE), &record).expect("saves");
    let bytes = fs::read(world.host(PLACED)).expect("read");
    let again: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(again, original);
}

/// Catches a saved file whose JSON shape, key names or trailing newline differ from the schema 1
/// record that the design shows.
#[test]
fn a_saved_record_has_the_schema_one_shape() {
    let world = World::new("store-shape");
    let mut record = PlacedRecord::default();
    record.paths.insert(
        PathBuf::from("/etc/systemd/system/x.service"),
        Entry::new(
            "x",
            "1.0.0",
            PathKind::Unit,
            PathState::Pending,
            Path::new("/t/u"),
        ),
    );
    record.paths.insert(
        PathBuf::from("/etc/systemd/system/w/x.service"),
        Entry::new(
            "x",
            "1.0.0",
            PathKind::WantsLink,
            PathState::Placed,
            Path::new("/t/w"),
        ),
    );
    save_in(&world, &world.fs, Path::new(STATE), &record).expect("saves");
    let bytes = world.placed_bytes().expect("written");
    assert_eq!(bytes.last(), Some(&b'\n'));
    let value: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        value,
        json!({
            "schema": 1,
            "paths": {
                "/etc/systemd/system/x.service": {
                    "service": "x", "version": "1.0.0", "kind": "unit",
                    "state": "pending", "link_target": "/t/u"
                },
                "/etc/systemd/system/w/x.service": {
                    "service": "x", "version": "1.0.0", "kind": "wants_link",
                    "state": "placed", "link_target": "/t/w"
                }
            },
            "uninstalling": {}
        })
    );
}

/// Catches a truncated `placed.json` that loads as an empty record, is rewritten, or names the
/// wrong file, whether read directly or through `Placer::begin`.
#[test]
fn a_corrupt_placed_json_is_an_error_and_is_not_rewritten() {
    let world = World::new("store-corrupt");
    let truncated = "{\"schema\": 1, \"paths\": {\"/etc/x.conf\": {\"service\": ";
    write_placed(&world, truncated);
    let store = Store::new(&world.fs, Path::new(STATE));
    match store.load() {
        Err(PlacedError::Corrupt { path, why }) => {
            assert_eq!(path, PathBuf::from(PLACED));
            assert!(!why.is_empty());
        }
        other => panic!("expected Corrupt, got {other:?}"),
    }
    assert_eq!(world.placed_bytes(), Some(truncated.as_bytes().to_vec()));

    let manifest = world.package("tierstore", "1.5.0", &Spec::tierstore());
    let placer = world.placer();
    let claims = Claims::of(placer.roots(), &manifest).expect("claims");
    match placer.begin(&claims) {
        Err(PlacedError::Corrupt { path, .. }) => assert_eq!(path, PathBuf::from(PLACED)),
        Err(other) => panic!("expected Corrupt, got {other:?}"),
        Ok(_) => panic!("begin must fail on a corrupt record"),
    }
    assert!(world.writes().is_empty());
    assert_eq!(world.placed_bytes(), Some(truncated.as_bytes().to_vec()));
}

/// Catches a record of the right syntax but the wrong shape (no schema, a schema that is not a
/// number, paths that are not a map) being accepted or defaulted.
#[test]
fn a_record_of_the_wrong_shape_is_corrupt() {
    for text in [
        "{}",
        "[]",
        "{\"schema\": \"1\"}",
        "{\"schema\": 1, \"paths\": []}",
        "{\"schema\": 1, \"paths\": {\"/a\": {\"service\": \"x\"}}}",
        "{\"schema\": 1, \"paths\": {\"/a\": {\"service\": \"x\", \"version\": \"1\", \"kind\": \"socket\", \"state\": \"placed\", \"link_target\": \"/t\"}}}",
    ] {
        let world = World::new("store-shape-bad");
        write_placed(&world, text);
        let result = Store::new(&world.fs, Path::new(STATE)).load();
        assert!(
            matches!(&result, Err(PlacedError::Corrupt { path, .. }) if path == Path::new(PLACED)),
            "{text}: {result:?}"
        );
        assert_eq!(world.placed_bytes(), Some(text.as_bytes().to_vec()));
    }
}

/// Catches a record of a newer schema being read as schema 1, downgraded on write, or refused
/// with another error, whether read directly or through `Placer::begin`.
#[test]
fn a_placed_json_of_a_newer_schema_is_refused_and_left_as_it_is() {
    let world = World::new("store-newer");
    let text = "{\"schema\": 2, \"paths\": {}, \"from_the_future\": true}";
    write_placed(&world, text);
    let store = Store::new(&world.fs, Path::new(STATE));
    let error = store.load().expect_err("refused");
    assert!(matches!(
        &error,
        PlacedError::NewerSchema { path, schema: 2 } if path == Path::new(PLACED)
    ));
    assert_eq!(
        error.to_string(),
        "/var/lib/hostkeeper/placed.json has schema 2; this daemon reads schema 1 and leaves it as it is"
    );
    assert_eq!(error.reason(), "link_failed");

    let manifest = world.package("tierstore", "1.5.0", &Spec::tierstore());
    let placer = world.placer();
    let claims = Claims::of(placer.roots(), &manifest).expect("claims");
    match placer.begin(&claims) {
        Err(PlacedError::NewerSchema { schema, .. }) => assert_eq!(schema, 2),
        Err(other) => panic!("expected NewerSchema, got {other:?}"),
        Ok(_) => panic!("begin must refuse a newer schema"),
    }
    assert!(world.writes().is_empty());
    assert_eq!(world.placed_bytes(), Some(text.as_bytes().to_vec()));
}

/// Catches a missing `placed.json` (or a missing state directory) being an error rather than the
/// empty record of schema 1, and a load that creates the file.
#[test]
fn a_missing_placed_json_loads_as_the_empty_record() {
    let world = World::new("store-absent");
    let record = Store::new(&world.fs, Path::new(STATE))
        .load()
        .expect("loads");
    assert_eq!(record, PlacedRecord::default());
    assert_eq!(record.schema, SCHEMA);
    assert_eq!(record.schema, 1);
    assert!(record.paths.is_empty() && record.uninstalling.is_empty());
    assert_eq!(world.placed_bytes(), None);

    let record = Store::new(&world.fs, Path::new("/no/such/dir"))
        .load()
        .expect("loads");
    assert_eq!(record, PlacedRecord::default());
}

/// Catches a `paths` or `uninstalling` key that is required: a record of only a schema loads.
#[test]
fn a_record_with_only_a_schema_loads_with_empty_maps() {
    let world = World::new("store-minimal");
    write_placed(&world, "{\"schema\": 1}");
    let record = Store::new(&world.fs, Path::new(STATE))
        .load()
        .expect("loads");
    assert_eq!(record, PlacedRecord::default());
}

/// Catches a `placed.json` that is a directory being treated as absent (the empty record) rather
/// than as a failed read.
#[test]
fn a_placed_json_that_is_a_directory_is_an_io_error() {
    let world = World::new("store-dir");
    fs::create_dir(world.host(PLACED)).expect("mkdir");
    let error = Store::new(&world.fs, Path::new(STATE))
        .load()
        .expect_err("cannot read a directory");
    assert_eq!(io_parts(error), ("read", PathBuf::from(PLACED)));
}

/// Catches a tombstone that is lost, or grows keys, on a save and load round trip.
#[test]
fn the_tombstone_round_trip() {
    let world = World::new("store-tombstone");
    let mut record = record_of(&[("/etc/x.conf", entry("x", "1.0.0"))]);
    record
        .uninstalling
        .insert("x".to_owned(), Tombstone::default());
    let store = Store::new(&world.fs, Path::new(STATE));
    save_in(&world, &world.fs, Path::new(STATE), &record).expect("saves");
    assert_eq!(store.load().expect("loads"), record);
    let value: Value =
        serde_json::from_slice(&world.placed_bytes().expect("written")).expect("json");
    assert_eq!(value["uninstalling"], json!({"x": {}}));
}

/// Catches `Store::path` and `of_service` / `same_link` comparing too little or too much: a link
/// is the same only when service, version, kind and target all agree, whatever the state.
#[test]
fn store_path_service_filter_and_link_identity() {
    let world = World::new("store-queries");
    let store = Store::new(&world.fs, Path::new(STATE));
    assert_eq!(store.path(), PathBuf::from(PLACED));

    let record = record_of(&[
        ("/etc/a.conf", entry("a", "1.0.0")),
        ("/etc/b.conf", entry("b", "1.0.0")),
        ("/etc/c.conf", entry("a", "2.0.0")),
    ]);
    let of_a: Vec<&Path> = record.of_service("a").map(|(p, _)| p.as_path()).collect();
    assert_eq!(of_a, [Path::new("/etc/a.conf"), Path::new("/etc/c.conf")]);
    assert_eq!(record.of_service("nobody").count(), 0);

    let base = entry("a", "1.0.0");
    let pending = Entry::new(
        "a",
        "1.0.0",
        PathKind::File,
        PathState::Pending,
        Path::new("/var/lib/hostkeeper/packages/x/current/files/x.conf"),
    );
    assert!(base.same_link(&pending));
    assert!(!base.same_link(&entry("b", "1.0.0")));
    assert!(!base.same_link(&entry("a", "1.0.1")));
    let other_kind = Entry::new(
        "a",
        "1.0.0",
        PathKind::Unit,
        PathState::Placed,
        Path::new("/var/lib/hostkeeper/packages/x/current/files/x.conf"),
    );
    assert!(!base.same_link(&other_kind));
    let other_target = Entry::new(
        "a",
        "1.0.0",
        PathKind::File,
        PathState::Placed,
        Path::new("/elsewhere"),
    );
    assert!(!base.same_link(&other_target));
}

/// Catches `RealFs::kind` naming a link by what it points at, or a missing path as an error, or a
/// socket as a file; and an error other than not-found (a file used as a directory) as `None`.
#[test]
fn real_fs_kind_of_every_kind_of_file() {
    let world = World::new("realfs-kind");
    let real = RealFs::new(world.root());
    fs::write(world.host("/a-file"), "x").expect("file");
    fs::create_dir(world.host("/a-dir")).expect("dir");
    symlink("/a-dir", world.host("/a-link")).expect("link");
    symlink("/nothing", world.host("/dangling")).expect("dangling link");

    support::fifo(&world.host("/a-pipe"));

    let kind = |path: &str| real.kind(Path::new(path)).expect("kind");
    assert_eq!(kind("/a-file"), Some(FileKind::File));
    assert_eq!(kind("/a-dir"), Some(FileKind::Dir));
    assert_eq!(kind("/a-link"), Some(FileKind::Symlink));
    assert_eq!(kind("/dangling"), Some(FileKind::Symlink));
    assert_eq!(kind("/a-pipe"), Some(FileKind::Other));
    assert_eq!(kind("/missing"), None);
    assert_eq!(kind("/a-dir/missing"), None);

    let error = real
        .kind(Path::new("/a-file/child"))
        .expect_err("not a directory");
    assert_eq!(error.raw_os_error(), Some(libc::ENOTDIR));
}

/// Catches `RealFs` reading outside its root, and `host_path` mishandling an absolute or a
/// relative path.
#[test]
fn real_fs_reads_under_its_root() {
    let world = World::new("realfs-read");
    let real = RealFs::new(world.root());
    assert_eq!(
        real.host_path(Path::new("/etc/x")),
        world.root().join("etc/x")
    );
    assert_eq!(
        real.host_path(Path::new("etc/x")),
        world.root().join("etc/x")
    );

    fs::write(world.host("/f"), "hello").expect("file");
    assert_eq!(real.read(Path::new("/f")).expect("read"), b"hello");
    let error = real.read(Path::new("/missing")).expect_err("missing");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);

    symlink("target-name", world.host("/l")).expect("link");
    assert_eq!(
        real.read_link(Path::new("/l")).expect("read_link"),
        PathBuf::from("target-name")
    );
    assert!(real.read_link(Path::new("/f")).is_err(), "not a link");
    assert!(real.read_link(Path::new("/missing")).is_err());
}

/// Catches `RealFs::list` returning names unsorted, including a link or temporary file wrongly,
/// or reporting a missing directory or a file as an empty listing.
#[test]
fn real_fs_lists_sorted_names_and_fails_on_a_missing_directory() {
    let world = World::new("realfs-list");
    let real = RealFs::new(world.root());
    fs::create_dir(world.host("/d")).expect("dir");
    for name in ["c", "a", "b"] {
        fs::write(world.host("/d").join(name), "").expect("file");
    }
    fs::create_dir(world.host("/d/sub")).expect("subdir");
    assert_eq!(
        real.list(Path::new("/d")).expect("list"),
        ["a", "b", "c", "sub"].map(std::ffi::OsString::from)
    );
    fs::create_dir(world.host("/empty")).expect("dir");
    assert!(real.list(Path::new("/empty")).expect("list").is_empty());

    let error = real.list(Path::new("/missing")).expect_err("missing");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert!(
        real.list(Path::new("/d/a")).is_err(),
        "a file is not a directory"
    );
}

/// Catches `RealFs::write` that appends instead of truncating, or follows a link at the path and
/// overwrites its target.
#[test]
fn real_fs_write_truncates_and_never_follows_a_link() {
    let world = World::new("realfs-write");
    let real = RealFs::new(world.root());
    real.write(Path::new("/f"), b"a longer first text")
        .expect("write");
    real.write(Path::new("/f"), b"short").expect("rewrite");
    assert_eq!(fs::read(world.host("/f")).expect("read"), b"short");

    fs::write(world.host("/victim"), "untouched").expect("victim");
    symlink("/victim", world.host("/trap")).expect("link");
    assert!(real.write(Path::new("/trap"), b"overwritten").is_err());
    assert_eq!(fs::read(world.host("/victim")).expect("read"), b"untouched");

    assert!(real.write(Path::new("/no/dir/f"), b"x").is_err());
}

/// Catches the write operations of `RealFs` (fsync, rename, symlink, remove, create_dirs) acting
/// on the wrong path, not failing on a missing one, or `create_dirs` not making parents or
/// failing on a directory that exists.
#[test]
fn real_fs_write_operations() {
    let world = World::new("realfs-ops");
    let real = RealFs::new(world.root());
    fs::write(world.host("/f"), "x").expect("file");

    real.sync_file(Path::new("/f")).expect("fsync a file");
    assert!(real.sync_file(Path::new("/missing")).is_err());
    real.sync_dir(Path::new("/")).expect("fsync a directory");
    assert!(real.sync_dir(Path::new("/missing")).is_err());

    real.rename(Path::new("/f"), Path::new("/g"))
        .expect("rename");
    assert!(!world.host("/f").exists());
    assert_eq!(fs::read(world.host("/g")).expect("read"), b"x");
    assert!(real.rename(Path::new("/f"), Path::new("/h")).is_err());

    real.symlink(Path::new("/g"), Path::new("/l"))
        .expect("symlink");
    assert_eq!(world.read_link("/l"), Some(PathBuf::from("/g")));
    assert!(
        real.symlink(Path::new("/g"), Path::new("/l")).is_err(),
        "exists"
    );

    real.remove_file(Path::new("/l")).expect("remove");
    assert!(real.remove_file(Path::new("/l")).is_err(), "already gone");
    assert!(world.host("/g").exists(), "the target stays");

    real.create_dirs(Path::new("/p/q/r")).expect("create_dirs");
    assert!(world.host("/p/q/r").is_dir());
    real.create_dirs(Path::new("/p/q/r")).expect("again");
    assert!(
        real.create_dirs(Path::new("/g/child")).is_err(),
        "under a file"
    );
}

/// Catches the directory accessors of `Roots` and `PackageDirs` composing the wrong paths, and
/// the Linux constants drifting from the design.
#[test]
fn roots_and_package_dirs_name_the_design_paths() {
    let roots = Roots::linux();
    assert_eq!(roots.state_dir(), Path::new("/var/lib/hostkeeper"));
    assert_eq!(roots.unit_dir(), Path::new("/etc/systemd/system"));
    assert_eq!(
        roots.packages().dir(),
        Path::new("/var/lib/hostkeeper/packages")
    );
    assert_eq!(
        roots.placed_file(),
        PathBuf::from("/var/lib/hostkeeper/placed.json")
    );
    assert_eq!(
        roots.lock_file(),
        PathBuf::from("/var/lib/hostkeeper/placed.lock")
    );
    assert_eq!(STATE_DIR, "/var/lib/hostkeeper");
    assert_eq!(PACKAGES_DIR, "/var/lib/hostkeeper/packages");
    assert_eq!(SYSTEMD_UNIT_DIR, "/etc/systemd/system");
    assert_eq!((PLACED_FILE, LOCK_FILE), ("placed.json", "placed.lock"));

    let packages = roots.packages();
    assert_eq!(
        packages.service("x"),
        PathBuf::from("/var/lib/hostkeeper/packages/x")
    );
    assert_eq!(
        packages.current("x"),
        PathBuf::from("/var/lib/hostkeeper/packages/x/current")
    );
    assert_eq!(
        packages.previous("x"),
        PathBuf::from("/var/lib/hostkeeper/packages/x/previous")
    );
    assert_eq!(
        packages.version("x", "1.2.3"),
        PathBuf::from("/var/lib/hostkeeper/packages/x/1.2.3")
    );

    let custom = Roots::new(Path::new("/s"), Path::new("/u"), Path::new("/p"));
    assert_eq!(custom.state_dir(), Path::new("/s"));
    assert_eq!(custom.unit_dir(), Path::new("/u"));
    assert_eq!(custom.packages(), &PackageDirs::new(Path::new("/p")));
    assert_eq!(custom.placed_file(), PathBuf::from("/s/placed.json"));
    assert_eq!(custom.lock_file(), PathBuf::from("/s/placed.lock"));
    assert_ne!(custom, roots);
}

/// Puts the process creation mask back to what it was, when dropped, even on a failed assertion.
struct UmaskGuard(Mode);

impl UmaskGuard {
    fn set(mask: RawMode) -> UmaskGuard {
        UmaskGuard(umask(Mode::from_raw_mode(mask)))
    }
}

impl Drop for UmaskGuard {
    fn drop(&mut self) {
        umask(self.0);
    }
}

/// The permission bits of the host path `path` on disk.
fn mode_of(world: &World, path: &str) -> u32 {
    fs::metadata(world.host(path))
        .expect("stat")
        .permissions()
        .mode()
        & 0o777
}

/// The text of a schema 1 record with one path entry built from the given parts.
fn one_entry(path: &str, service: &str, version: &str, link_target: &str) -> String {
    json!({
        "schema": 1,
        "paths": {
            path: {
                "service": service,
                "version": version,
                "kind": "file",
                "state": "placed",
                "link_target": link_target
            }
        }
    })
    .to_string()
}

/// Catches a load that trusts the known fields of a record that parsed: a wrong schema, a path or
/// link target that is relative or has a dot component, a service or version outside its grammar
/// or a tombstone for a non-service is `Corrupt` naming `placed.json`, and the file is not touched.
#[test]
fn a_record_that_breaks_the_rules_of_its_fields_is_corrupt_and_left_as_it_is() {
    const TARGET: &str = "/var/lib/hostkeeper/packages/x/current/files/x.conf";
    let cases = [
        (
            "{\"schema\": 0, \"paths\": {}}".to_owned(),
            "its schema is 0",
        ),
        (
            one_entry("etc/x.conf", "x", "1.0.0", TARGET),
            "the path etc/x.conf",
        ),
        (
            one_entry("/etc/./x.conf", "x", "1.0.0", TARGET),
            "the path /etc/./x.conf",
        ),
        (
            one_entry("/etc/../x.conf", "x", "1.0.0", TARGET),
            "the path /etc/../x.conf",
        ),
        (
            one_entry("/etc/x.conf", "x", "1.0.0", "current/files/x"),
            "the link target current/files/x",
        ),
        (
            one_entry("/etc/x.conf", "x", "1.0.0", "/a/../b"),
            "the link target /a/../b",
        ),
        (
            one_entry("/etc/x.conf", "/etc/x", "1.0.0", TARGET),
            "\"/etc/x\" is not a service name",
        ),
        (
            one_entry("/etc/x.conf", "a/b", "1.0.0", TARGET),
            "\"a/b\" is not a service name",
        ),
        (
            one_entry("/etc/x.conf", "", "1.0.0", TARGET),
            "\"\" is not a service name",
        ),
        (
            one_entry("/etc/x.conf", "x", "1/0", TARGET),
            "\"1/0\" is not a version",
        ),
        (
            one_entry("/etc/x.conf", "x", ".hidden", TARGET),
            "\".hidden\" is not a version",
        ),
        (
            json!({"schema": 1, "uninstalling": {"a/b": {}}}).to_string(),
            "the tombstone \"a/b\" is not a service name",
        ),
    ];
    for (n, (text, fragment)) in cases.into_iter().enumerate() {
        let world = World::new(&format!("store-semantic-{n}"));
        write_placed(&world, &text);
        let result = Store::new(&world.fs, Path::new(STATE)).load();
        match result {
            Err(PlacedError::Corrupt { path, why }) => {
                assert_eq!(path, PathBuf::from(PLACED), "{text}");
                assert!(why.contains(fragment), "{text}: {why}");
            }
            other => panic!("{text}: expected Corrupt, got {other:?}"),
        }
        assert_eq!(world.placed_bytes(), Some(text.into_bytes()));
        assert!(world.writes().is_empty());
    }
}

/// Catches a validation that refuses a good record: a path and link target in the grammar, a
/// service with `_` and `-`, a version with `+` and a tombstone for such a service all load.
#[test]
fn a_record_whose_fields_are_in_their_grammars_loads() {
    let world = World::new("store-semantic-ok");
    let text = json!({
        "schema": 1,
        "paths": {
            "/etc/my_svc-2/x.conf": {
                "service": "my_svc-2",
                "version": "1.0.0-rc.1+build_5",
                "kind": "file",
                "state": "pending",
                "link_target": "/var/lib/hostkeeper/packages/my_svc-2/current/files/x.conf"
            }
        },
        "uninstalling": {"old-one": {}}
    });
    write_placed(&world, &text.to_string());
    let record = Store::new(&world.fs, Path::new(STATE))
        .load()
        .expect("loads");
    assert_eq!(record.paths.len(), 1);
    assert!(record.uninstalling.contains_key("old-one"));
}

/// Catches a `reload_pending` that is lost on a save and load round trip, written when it is not
/// set, or required when it is absent.
#[test]
fn reload_pending_round_trips_and_is_written_only_when_set() {
    let world = World::new("store-reload-pending");
    let store = Store::new(&world.fs, Path::new(STATE));
    let mut record = record_of(&[("/etc/x.conf", entry("x", "1.0.0"))]);
    assert!(!record.reload_pending, "the default owes nothing");

    save_in(&world, &world.fs, Path::new(STATE), &record).expect("saves");
    let value: Value =
        serde_json::from_slice(&world.placed_bytes().expect("written")).expect("json");
    assert!(
        !value
            .as_object()
            .expect("an object")
            .contains_key("reload_pending"),
        "{value}"
    );
    assert!(!store.load().expect("loads").reload_pending);

    record.reload_pending = true;
    save_in(&world, &world.fs, Path::new(STATE), &record).expect("saves");
    let value: Value =
        serde_json::from_slice(&world.placed_bytes().expect("written")).expect("json");
    assert_eq!(value["reload_pending"], json!(true));
    let loaded = store.load().expect("loads");
    assert!(loaded.reload_pending);
    assert_eq!(loaded, record);

    record.reload_pending = false;
    save_in(&world, &world.fs, Path::new(STATE), &record).expect("saves");
    let value: Value =
        serde_json::from_slice(&world.placed_bytes().expect("written")).expect("json");
    assert!(
        !value
            .as_object()
            .expect("an object")
            .contains_key("reload_pending")
    );

    write_placed(&world, "{\"schema\": 1, \"reload_pending\": false}");
    assert!(!store.load().expect("loads").reload_pending);
}

/// Catches a `Store::save` that needs no lock: the lock `Placer::lock` returns is the token a
/// save takes, and with it held a second lock is refused while the save still works.
#[test]
fn a_save_is_made_under_the_lock_of_placer_lock() {
    let world = World::new("store-save-lock");
    let placer = world.placer();
    let lock = placer.lock().expect("the lock is free");
    placer
        .store()
        .save(&record_of(&[("/etc/x.conf", entry("x", "1.0.0"))]), &lock)
        .expect("saves under the lock");
    assert!(matches!(placer.lock(), Err(PlacedError::Locked { .. })));
    assert_eq!(world.record().paths.len(), 1);
}

/// Catches a `host_path` that lets `..` climb out of the root: a write to `/../outside/file` lands
/// at `<root>/outside/file`, the directory beside the root is untouched, and `.` and `/` parts
/// are dropped too.
#[test]
fn real_fs_host_path_keeps_every_spelling_under_the_root() {
    let world = World::new("realfs-escape");
    fs::create_dir(world.host("/jail")).expect("the root");
    fs::create_dir(world.host("/outside")).expect("the sentinel directory");
    fs::write(world.host("/outside/sentinel"), "keep").expect("sentinel");
    let jail = world.host("/jail");
    let real = RealFs::new(&jail);

    assert_eq!(
        real.host_path(Path::new("/../outside/file")),
        jail.join("outside/file")
    );
    assert_eq!(real.host_path(Path::new("/../../..")), jail);
    assert_eq!(real.host_path(Path::new("/a/../b/./c")), jail.join("a/b/c"));
    assert_eq!(real.host_path(Path::new("..")), jail);

    real.create_dirs(Path::new("/../outside"))
        .expect("create_dirs");
    real.write(Path::new("/../outside/file"), b"inside")
        .expect("write");
    assert_eq!(
        fs::read(jail.join("outside/file")).expect("read"),
        b"inside"
    );
    assert_eq!(
        real.read(Path::new("/../outside/file")).expect("read"),
        b"inside"
    );
    let beside: Vec<_> = fs::read_dir(world.host("/outside"))
        .expect("list")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    assert_eq!(beside, vec![std::ffi::OsString::from("sentinel")]);
    assert_eq!(
        fs::read(world.host("/outside/sentinel")).expect("read"),
        b"keep"
    );
}

/// Catches `create_dirs` giving a directory the creation mask took bits from, or changing a
/// directory that was already there: under umask 077 each directory made is 0755 and one that
/// existed with 0700 stays 0700.
#[test]
fn create_dirs_makes_0755_whatever_the_mask_and_leaves_existing_directories() {
    let world = World::new("realfs-umask");
    let real = RealFs::new(world.root());
    fs::create_dir(world.host("/old")).expect("an existing directory");
    fs::set_permissions(world.host("/old"), fs::Permissions::from_mode(0o700)).expect("chmod");

    let _mask = UmaskGuard::set(0o077);
    real.create_dirs(Path::new("/old/new/deeper"))
        .expect("create_dirs");
    real.create_dirs(Path::new("/fresh/a/b"))
        .expect("create_dirs");
    real.create_dirs(Path::new("/old/new/deeper"))
        .expect("again");

    assert_eq!(mode_of(&world, "/old"), 0o700);
    for made in [
        "/old/new",
        "/old/new/deeper",
        "/fresh",
        "/fresh/a",
        "/fresh/a/b",
    ] {
        assert_eq!(mode_of(&world, made), 0o755, "{made}");
    }
}
