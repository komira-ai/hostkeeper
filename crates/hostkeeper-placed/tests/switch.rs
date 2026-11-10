//! The switch of `current` and `previous` (design section 4.7, "Placed paths"): `previous` is set
//! first and `current` second, each link made under a temporary name and renamed over the old
//! one; a repeat after a crash writes only what is missing; anything at either link but a link
//! to a version beside it is a `path_conflict` and is left as it is. And the reload the switch
//! is followed by.

mod support;

use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;

use hostkeeper_config::manifest::Class;
use hostkeeper_placed::placer::{MAX_SPELLED_OWNER, is_temporary, temporary_name};
use hostkeeper_placed::testing::{FaultFs, FsWrite, UnitCall};
use hostkeeper_placed::{Owner, PlacedError};
use support::{World, service, version};

const PACKAGES: &str = "/var/lib/hostkeeper/packages";

/// Makes the directories of `versions` of `name` under the package directory.
fn versions(world: &World, name: &str, versions: &[&str]) {
    for version in versions {
        fs::create_dir_all(world.host(format!("{PACKAGES}/{name}/{version}"))).expect("a version");
    }
}

fn link(world: &World, name: &str, which: &str) -> Option<PathBuf> {
    world.read_link(format!("{PACKAGES}/{name}/{which}"))
}

/// The `PathConflict` of `result`: the path and what holds it.
fn conflict<T: std::fmt::Debug>(result: Result<T, PlacedError>) -> (PathBuf, Owner) {
    match result {
        Err(PlacedError::PathConflict { path, owner }) => (path, owner),
        other => panic!("expected a path conflict, got {other:?}"),
    }
}

fn io_parts<T: std::fmt::Debug>(result: Result<T, PlacedError>) -> (&'static str, PathBuf) {
    match result {
        Err(PlacedError::Io { op, path, .. }) => (op, path),
        other => panic!("expected an Io error, got {other:?}"),
    }
}

/// Catches a first switch that writes `previous` for a release that never ran, a later one that
/// sets `previous` to anything but the version `current` named, and a repeat that writes again:
/// the first switch makes `current` alone, the second makes `previous` the old `current` and
/// `current` the new, each by a link renamed into place, and a repeat of either writes nothing.
#[test]
fn a_switch_sets_previous_to_the_old_current_and_current_to_the_new() {
    let world = World::new("switch-two");
    versions(&world, "tierstore", &["1.4.2", "1.5.0"]);
    let placer = world.placer();
    let name = service("tierstore");
    assert_eq!(placer.current("tierstore").expect("no link"), None);
    assert_eq!(placer.previous("tierstore").expect("no link"), None);

    assert!(
        placer
            .switch_current(&name, &version("1.4.2"))
            .expect("first")
    );
    assert_eq!(link(&world, "tierstore", "current"), Some("1.4.2".into()));
    assert_eq!(link(&world, "tierstore", "previous"), None);

    world.log.clear();
    assert!(
        placer
            .switch_current(&name, &version("1.5.0"))
            .expect("second")
    );
    assert_eq!(link(&world, "tierstore", "previous"), Some("1.4.2".into()));
    assert_eq!(link(&world, "tierstore", "current"), Some("1.5.0".into()));
    let tmp = PathBuf::from(format!("{PACKAGES}/tierstore/.hk~tierstore~tmp"));
    let dir = PathBuf::from(format!("{PACKAGES}/tierstore"));
    let renamed = |name: &str| FsWrite::Rename {
        from: tmp.clone(),
        to: dir.join(name),
    };
    let writes = world.writes();
    let at = |write: &FsWrite| writes.iter().position(|w| w == write).expect("written");
    assert!(
        at(&renamed("previous")) < at(&renamed("current")),
        "{writes:?}"
    );
    assert_eq!(
        placer.current("tierstore").expect("read"),
        Some("1.5.0".into())
    );
    assert_eq!(
        placer.previous("tierstore").expect("read"),
        Some("1.4.2".into())
    );

    world.log.clear();
    assert!(
        !placer
            .switch_current(&name, &version("1.5.0"))
            .expect("repeat")
    );
    assert!(world.writes().is_empty());
}

/// Catches a repeat after a crash between the two links that rewrites a link already right: a
/// switch whose `current` write fails has made `previous`; the repeat on a healthy disk writes
/// `current` alone, and a switch back to the version `previous` names sets `previous` to the one
/// it leaves.
#[test]
fn a_switch_repeated_after_a_crash_between_the_links_writes_only_what_is_missing() {
    let world = World::new("switch-crash");
    versions(&world, "a", &["1.0.0", "2.0.0"]);
    world.set_link("a", "current", "1.0.0");
    let name = service("a");
    // Each link is three writes: the temporary link, the rename, the fsync of the directory.
    let failing = world.failing_at(4);
    let error = world
        .placer_on(&failing)
        .switch_current(&name, &version("2.0.0"))
        .expect_err("the crash");
    assert_eq!(io_parts(Err::<(), _>(error)).0, "link");
    assert_eq!(link(&world, "a", "previous"), Some("1.0.0".into()));
    assert_eq!(link(&world, "a", "current"), Some("1.0.0".into()));

    world.log.clear();
    assert!(
        world
            .placer()
            .switch_current(&name, &version("2.0.0"))
            .expect("repeat")
    );
    assert_eq!(link(&world, "a", "current"), Some("2.0.0".into()));
    assert_eq!(link(&world, "a", "previous"), Some("1.0.0".into()));
    let renames = world
        .writes()
        .into_iter()
        .filter(|write| matches!(write, FsWrite::Rename { .. }))
        .count();
    assert_eq!(renames, 1, "previous was already right");

    assert!(
        world
            .placer()
            .switch_current(&name, &version("1.0.0"))
            .expect("back")
    );
    assert_eq!(link(&world, "a", "current"), Some("1.0.0".into()));
    assert_eq!(link(&world, "a", "previous"), Some("2.0.0".into()));
}

/// Catches `current` and `previous` read as versions when something else is there: a regular file
/// at `current` and a directory at `previous` are path conflicts naming them, and
/// `switch_current` surfaces both without writing.
#[test]
fn a_current_or_previous_that_is_not_a_link_is_a_path_conflict() {
    let world = World::new("switch-not-a-link");
    versions(&world, "tierstore", &["1.5.0"]);
    let placer = world.placer();
    let current = format!("{PACKAGES}/tierstore/current");
    let previous = format!("{PACKAGES}/tierstore/previous");
    let to = version("1.5.0");

    fs::write(world.host(&current), b"a file\n").expect("write a file");
    let expected = (PathBuf::from(&current), Owner::File);
    assert_eq!(conflict(placer.current("tierstore")), expected);
    world.log.clear();
    assert_eq!(
        conflict(placer.switch_current(&service("tierstore"), &to)),
        expected
    );
    assert!(world.writes().is_empty());

    fs::remove_file(world.host(&current)).expect("remove the file");
    world.set_link("tierstore", "current", "1.5.0");
    fs::create_dir(world.host(&previous)).expect("a directory");
    let expected = (PathBuf::from(&previous), Owner::Directory);
    assert_eq!(conflict(placer.previous("tierstore")), expected);

    fs::remove_dir(world.host(&previous)).expect("remove the directory");
    hostkeeper_testkit::fifo(&world.host(&previous), 0o600).expect("a named pipe");
    let expected = (PathBuf::from(&previous), Owner::Other);
    assert_eq!(conflict(placer.previous("tierstore")), expected);
}

/// Catches `current` or `previous` taken as a version when its link names something else: a
/// target outside the service directory, an absolute path to a local version directory or a path
/// inside a version is a `PathConflict` of `Owner::Link(target)` from `current()`, `previous()`
/// and `switch_current`, which write nothing and keep the link.
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
        let world = World::new(&format!("switch-link-target-{name}"));
        versions(&world, "tierstore", &["1.5.0"]);
        world.set_link("tierstore", "current", target);
        let placer = world.placer();
        world.log.clear();
        let expected = (PathBuf::from(&current), Owner::Link(PathBuf::from(target)));

        assert_eq!(conflict(placer.current("tierstore")), expected, "{name}");
        let result = placer.switch_current(&service("tierstore"), &version("1.5.0"));
        assert_eq!(conflict(result), expected, "{name}");
        assert!(world.writes().is_empty(), "{name}");
        assert_eq!(world.read_link(&current), Some(PathBuf::from(target)));
        assert!(world.read_link(&previous).is_none());
    }

    let world = World::new("switch-link-target-previous");
    versions(&world, "tierstore", &["1.4.2"]);
    world.set_link("tierstore", "current", "1.4.2");
    world.set_link("tierstore", "previous", "../other/1.4.2");
    let placer = world.placer();
    world.log.clear();
    let expected = (
        PathBuf::from(&previous),
        Owner::Link(PathBuf::from("../other/1.4.2")),
    );
    assert_eq!(conflict(placer.previous("tierstore")), expected);
    let result = placer.switch_current(&service("tierstore"), &version("1.5.0"));
    assert_eq!(conflict(result), expected);
    assert!(world.writes().is_empty());
}

/// Catches a read that fails reported as a link that is not there: a failing `kind` is an
/// `inspect` error and a failing `read_link` a `read the link` error naming the link, from
/// `current`.
#[test]
fn a_link_that_cannot_be_read_is_an_io_error() {
    let world = World::new("switch-read");
    versions(&world, "a", &["1.0.0"]);
    world.set_link("a", "current", "1.0.0");
    let current = PathBuf::from(format!("{PACKAGES}/a/current"));
    for (n, op) in [(1, "inspect"), (2, "read the link")] {
        let flaky = FaultFs::new(world.root(), world.log.clone()).failing_read(n, false);
        let result = world.placer_on(&flaky).current("a");
        assert_eq!(io_parts(result), (op, current.clone()));
    }
}

/// Catches a write of a switch that names the wrong step or path when it fails: the temporary
/// link, the rename and the fsync of the directory each name theirs, and a switch whose first
/// read fails writes nothing.
#[test]
fn a_failed_write_of_a_switch_names_its_step_and_path() {
    let dir = format!("{PACKAGES}/a");
    let tmp = format!("{dir}/.hk~a~tmp");
    for (n, op, path) in [(1, "link", &tmp), (2, "rename", &tmp), (3, "fsync", &dir)] {
        let world = World::new(&format!("switch-step-{n}"));
        versions(&world, "a", &["1.0.0"]);
        let failing = world.failing_at(n);
        let result = world
            .placer_on(&failing)
            .switch_current(&service("a"), &version("1.0.0"));
        assert_eq!(io_parts(result), (op, PathBuf::from(path)), "write {n}");
    }
}

/// Catches a temporary link a crash left that blocks the next switch: a leftover at the temporary
/// name is removed, and the switch makes its links.
#[test]
fn a_temporary_link_a_crash_left_is_replaced() {
    let world = World::new("switch-leftover");
    versions(&world, "a", &["1.0.0"]);
    std::os::unix::fs::symlink("0.9.0", world.host(format!("{PACKAGES}/a/.hk~a~tmp")))
        .expect("the leftover");
    assert!(
        world
            .placer()
            .switch_current(&service("a"), &version("1.0.0"))
            .expect("switch")
    );
    assert_eq!(link(&world, "a", "current"), Some("1.0.0".into()));
    let names: Vec<OsString> = fs::read_dir(world.host(format!("{PACKAGES}/a")))
        .expect("list")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    assert!(
        names
            .iter()
            .all(|name| !is_temporary(&name.to_string_lossy())),
        "{names:?}"
    );

    // A leftover that cannot be removed is the switch's error: the removal names it.
    let world = World::new("switch-leftover-stuck");
    versions(&world, "a", &["1.0.0"]);
    let tmp = world.host(format!("{PACKAGES}/a/.hk~a~tmp"));
    std::os::unix::fs::symlink("0.9.0", &tmp).expect("the leftover");
    let failing = world.failing_at(1);
    let result = world
        .placer_on(&failing)
        .switch_current(&service("a"), &version("1.0.0"));
    assert_eq!(
        io_parts(result),
        ("remove", PathBuf::from(format!("{PACKAGES}/a/.hk~a~tmp")))
    );
}

/// Catches a temporary name that grows with the service name past a path component's 255 bytes
/// (a service name may be 255 bytes), or two long names sharing one temporary name: an owner of up
/// to 64 bytes is spelled out, a longer one is named by a hash, and every form is a temporary
/// name.
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
    assert!(!is_temporary("current"));
    assert!(!is_temporary(".hk~a"));
}

/// Catches a switch that fails for a long service name because its temporary link name is too
/// long for the file system: `current` of a 200-byte service switches, `previous` first.
#[test]
fn a_service_with_a_200_byte_name_switches_current() {
    let world = World::new("switch-long-service");
    let name = "s".repeat(200);
    versions(&world, &name, &["1.0.0", "2.0.0"]);
    world.set_link(&name, "current", "1.0.0");
    let switched = world
        .placer()
        .switch_current(&service(&name), &version("2.0.0"))
        .expect("switch");
    assert!(switched);
    assert_eq!(link(&world, &name, "current"), Some("2.0.0".into()));
    assert_eq!(link(&world, &name, "previous"), Some("1.0.0".into()));
}

/// Catches a reload that runs without the lock of `placed.json`, or leaves it held: the reload is
/// one `daemon-reload` call, refused while another holds the lock, and a failed reload is the
/// units' error with the lock released again.
#[test]
fn a_reload_is_one_daemon_reload_under_the_lock() {
    let world = World::new("switch-reload");
    let placer = world.placer();
    placer.reload().expect("reload");
    assert_eq!(world.unit_calls(), [UnitCall::DaemonReload]);
    assert_eq!(placer.roots(), &world.roots);

    let held = placer.lock().expect("the lock is free");
    assert!(matches!(placer.reload(), Err(PlacedError::Locked { .. })));
    assert_eq!(world.unit_calls(), [UnitCall::DaemonReload], "nothing ran");
    drop(held);

    world.units.fail_reload();
    let error = placer.reload().expect_err("the reload fails");
    assert!(matches!(&error, PlacedError::Units(_)), "{error:?}");
    assert_eq!(error.class(), Class::Environment);
    assert_eq!(error.reason(), "daemon_reload_failed");
    drop(placer.lock().expect("the lock was released"));
}
