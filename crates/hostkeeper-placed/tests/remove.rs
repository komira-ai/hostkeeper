//! Removal of placed paths and the `WantedBy=` links of the `run` units: `unlink_unlisted`,
//! `remove_all`, `enable_run_units` and `disable`. Each case installs tierstore under a scratch
//! root, then reads the order of writes and unit calls, the links on disk and `placed.json`.

mod support;

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use hostkeeper_placed::testing::{Event, FakeUnitControl, FaultFs, FsWrite, UnitCall};
use hostkeeper_placed::{
    ClaimError, Claims, Entry, LintError, PathKind, PathState, PlacedError, Placer, Removal, Store,
};
use support::{Spec, World, install, service, unit_text, unit_without_install};

const UNIT_DIR: &str = "/etc/systemd/system";
const CONF: &str = "/etc/tierstore/tierstore.conf";
const STATE: &str = "/var/lib/hostkeeper";

fn exec() -> String {
    unit_text("/var/lib/hostkeeper/packages/tierstore/current/bin/tierstore cache")
}

fn wants(unit: &str) -> PathBuf {
    Path::new(support::WANTS).join(unit)
}

fn unit_path(unit: &str) -> PathBuf {
    Path::new(UNIT_DIR).join(unit)
}

/// tierstore 1.4.2: the template, its instance, and the plain unit `tierstore-cache.service`,
/// which is also in `run`.
fn spec_1_4_2() -> Spec {
    Spec::tierstore()
        .unit("tierstore-cache.service", &exec())
        .run("tierstore-cache.service")
}

/// tierstore 1.5.0 installed under a fresh scratch root, with the log of the install forgotten.
fn tierstore_world(name: &str) -> World {
    let world = World::new(name);
    install(&world, "tierstore", "1.5.0", &Spec::tierstore());
    world.log.clear();
    world
}

/// A world whose tierstore 1.5.0 is begun, linked, switched and committed but not enabled.
fn unenabled_world(name: &str) -> (World, hostkeeper_config::manifest::Manifest) {
    let world = World::new(name);
    let manifest = world.package("tierstore", "1.5.0", &Spec::tierstore());
    let placer = world.placer();
    let claims = Claims::of(placer.roots(), &manifest).expect("claims");
    let linking = placer.begin(&claims).expect("begin");
    placer.link(&linking).expect("link");
    placer
        .switch_current(&service("tierstore"), &support::version("1.5.0"))
        .expect("switch");
    placer.commit(linking).expect("commit");
    world.log.clear();
    (world, manifest)
}

fn ghost_entry(kind: PathKind) -> Entry {
    Entry::new(
        "ghost",
        "1.0.0",
        kind,
        PathState::Placed,
        Path::new("/nowhere"),
    )
}

/// Catches a reload that runs before the removals or once per unit, and a stop that is missing,
/// doubled for a unit and its `WantedBy=` link, or run after the link went: the exact order of
/// the log is stop, wants link and its directory, unit file and its directory, the record, then
/// one reload; the listed template, instance link and file are untouched.
#[test]
fn removing_a_version_unlists_stops_disables_unlinks_then_reloads_in_that_order() {
    let world = World::new("remove-order");
    install(&world, "tierstore", "1.4.2", &spec_1_4_2());
    let manifest = install(&world, "tierstore", "1.5.0", &Spec::tierstore());
    assert!(world.read_link(wants("tierstore-cache.service")).is_some());
    world.log.clear();

    let removal = world
        .placer()
        .unlink_unlisted(&manifest)
        .expect("the removal");

    let state = PathBuf::from(STATE);
    let tmp = state.join(".hk-placed.json.tmp");
    assert_eq!(
        world.log.events(),
        [
            Event::Unit(UnitCall::Stop("tierstore-cache.service".to_owned())),
            Event::Fs(FsWrite::Remove(wants("tierstore-cache.service"))),
            Event::Fs(FsWrite::SyncDir(PathBuf::from(support::WANTS))),
            Event::Fs(FsWrite::Remove(unit_path("tierstore-cache.service"))),
            Event::Fs(FsWrite::SyncDir(PathBuf::from(UNIT_DIR))),
            Event::Fs(FsWrite::Write(tmp.clone())),
            Event::Fs(FsWrite::SyncFile(tmp.clone())),
            Event::Fs(FsWrite::Rename {
                from: tmp,
                to: state.join("placed.json"),
            }),
            Event::Fs(FsWrite::SyncDir(state)),
            Event::Unit(UnitCall::DaemonReload),
        ]
    );
    assert_eq!(
        removal,
        Removal {
            stopped: vec!["tierstore-cache.service".to_owned()],
            removed: vec![
                wants("tierstore-cache.service"),
                unit_path("tierstore-cache.service")
            ],
            left_alone: Vec::new(),
            reloaded: true,
        }
    );
    assert!(world.read_link(unit_path("tierstore@.service")).is_some());
    assert!(world.read_link(wants("tierstore@9301.service")).is_some());
    assert!(world.read_link(CONF).is_some());
    assert!(
        world
            .read_link(unit_path("tierstore-cache.service"))
            .is_none()
    );
    assert!(world.read_link(wants("tierstore-cache.service")).is_none());
    let listed: BTreeSet<PathBuf> = world.record().paths.keys().cloned().collect();
    assert_eq!(
        listed,
        BTreeSet::from([
            unit_path("tierstore@.service"),
            wants("tierstore@9301.service"),
            PathBuf::from(CONF),
        ])
    );
}

/// Catches a removal that deletes a path the record lists but whose content is not the recorded
/// link: the regular file keeps its bytes, is reported in `left_alone`, and the record drops it
/// together with every other entry of the service.
#[test]
fn remove_all_never_deletes_a_path_the_record_does_not_own() {
    let world = tierstore_world("remove-foreign");
    let conf = world.host(CONF);
    fs::remove_file(&conf).expect("remove the link");
    fs::write(&conf, b"mine\n").expect("write a file");

    let removal = world
        .placer()
        .remove_all(&service("tierstore"))
        .expect("the removal");

    assert_eq!(
        removal,
        Removal {
            stopped: vec!["tierstore@9301.service".to_owned()],
            removed: vec![
                wants("tierstore@9301.service"),
                unit_path("tierstore@.service")
            ],
            left_alone: vec![PathBuf::from(CONF)],
            reloaded: true,
        }
    );
    assert_eq!(fs::read(conf).expect("the file"), b"mine\n");
    assert!(world.placed_links().is_empty());
    assert!(world.record().paths.is_empty());
    assert_eq!(
        world.unit_calls(),
        [
            UnitCall::Stop("tierstore@9301.service".to_owned()),
            UnitCall::DaemonReload
        ]
    );
}

/// Catches a removal that treats any link as the recorded one: a link at a recorded path that
/// points elsewhere is left on disk with its target and reported, not deleted.
#[test]
fn remove_all_leaves_a_link_that_points_elsewhere() {
    let world = tierstore_world("remove-repointed");
    let conf = world.host(CONF);
    fs::remove_file(&conf).expect("remove the link");
    symlink("/elsewhere", conf).expect("make a link");

    let removal = world
        .placer()
        .remove_all(&service("tierstore"))
        .expect("the removal");

    assert_eq!(removal.left_alone, [PathBuf::from(CONF)]);
    assert!(!removal.removed.contains(&PathBuf::from(CONF)));
    assert_eq!(world.read_link(CONF), Some(PathBuf::from("/elsewhere")));
    assert!(world.record().paths.is_empty());
}

/// Catches a stop of the unit of a `WantedBy=` link that is not the recorded one: the wants path
/// is a regular file, so nothing is stopped for it, the file stays, and the other paths go.
#[test]
fn a_foreign_wants_path_is_not_stopped_and_not_removed() {
    let world = tierstore_world("remove-foreign-wants");
    let link = world.host(wants("tierstore@9301.service"));
    fs::remove_file(&link).expect("remove the link");
    fs::write(&link, b"kept\n").expect("write a file");

    let removal = world
        .placer()
        .remove_all(&service("tierstore"))
        .expect("the removal");

    assert_eq!(
        removal,
        Removal {
            stopped: Vec::new(),
            removed: vec![unit_path("tierstore@.service"), PathBuf::from(CONF)],
            left_alone: vec![wants("tierstore@9301.service")],
            reloaded: true,
        }
    );
    assert_eq!(fs::read(link).expect("the file"), b"kept\n");
    assert!(
        world
            .unit_calls()
            .iter()
            .all(|call| !matches!(call, UnitCall::Stop(_)))
    );
}

/// Catches a removal that fails, or writes, for a link already gone: it counts as removed with
/// no remove write for it, and its unit is still stopped.
#[test]
fn a_path_whose_link_is_already_gone_counts_as_removed_without_a_write() {
    let world = tierstore_world("remove-gone");
    fs::remove_file(world.host(CONF)).expect("remove the link");
    fs::remove_file(world.host(wants("tierstore@9301.service"))).expect("remove the link");
    world.log.clear();

    let removal = world
        .placer()
        .remove_all(&service("tierstore"))
        .expect("the removal");

    assert_eq!(removal.stopped, ["tierstore@9301.service"]);
    assert_eq!(
        removal.removed,
        [
            wants("tierstore@9301.service"),
            unit_path("tierstore@.service"),
            PathBuf::from(CONF)
        ]
    );
    assert!(removal.left_alone.is_empty());
    let removes: Vec<FsWrite> = world
        .writes()
        .into_iter()
        .filter(|write| matches!(write, FsWrite::Remove(_)))
        .collect();
    assert_eq!(removes, [FsWrite::Remove(unit_path("tierstore@.service"))]);
    assert!(world.record().paths.is_empty());
}

/// Catches a removal that writes the record or reloads when the service has no path: nothing is
/// written, no unit call is made, and the result is empty.
#[test]
fn remove_all_of_a_service_with_no_paths_writes_nothing_and_does_not_reload() {
    let world = World::new("remove-none");

    let removal = world
        .placer()
        .remove_all(&service("ghost"))
        .expect("the removal");

    assert_eq!(removal, Removal::default());
    assert!(world.log.events().is_empty());
    assert!(world.placed_bytes().is_none());
}

/// Catches a reload after a removal that took no unit or `WantedBy=` link: a service that places
/// only a file has the file removed and the record written, with no stop and no reload.
#[test]
fn removing_only_a_file_does_not_reload() {
    let world = World::new("remove-file-only");
    install(
        &world,
        "filesvc",
        "1.0.0",
        &Spec::new().file("files/filesvc.conf", "/etc/filesvc/filesvc.conf"),
    );
    world.log.clear();

    let removal = world
        .placer()
        .remove_all(&service("filesvc"))
        .expect("the removal");

    assert_eq!(
        removal,
        Removal {
            stopped: Vec::new(),
            removed: vec![PathBuf::from("/etc/filesvc/filesvc.conf")],
            left_alone: Vec::new(),
            reloaded: false,
        }
    );
    assert!(world.unit_calls().is_empty());
    assert_eq!(
        world.writes()[..2],
        [
            FsWrite::Remove(PathBuf::from("/etc/filesvc/filesvc.conf")),
            FsWrite::SyncDir(PathBuf::from("/etc/filesvc")),
        ]
    );
    assert!(world.record().paths.is_empty());
}

/// Catches a removal that acts before the units answer: a failing stop makes `remove_all` return
/// `PlacedError::Units` with every link, the record and the disk as they were.
#[test]
fn a_failing_stop_leaves_everything_in_place() {
    let world = tierstore_world("remove-stop-fails");
    let before = world.record();
    let snapshot = world.snapshot();
    world.log.clear();
    let failing = FakeUnitControl::failing(world.log.clone());
    let placer = Placer::new(world.roots.clone(), &world.fs, &failing);

    let err = placer
        .remove_all(&service("tierstore"))
        .expect_err("the stop fails");

    assert!(matches!(err, PlacedError::Units(_)));
    assert_eq!(err.reason(), "daemon_reload_failed");
    assert_eq!(world.record(), before);
    assert_eq!(world.snapshot(), snapshot);
    assert!(world.writes().is_empty());
}

/// Catches a write failing at any point of a removal being swallowed or leaving a reload behind:
/// the n-th write failing, for every n, is `PlacedError::Io` with no reload, and the record
/// still lists every path until its own rename; one write more than needed succeeds.
#[test]
fn a_failing_write_at_any_step_of_remove_all_is_an_io_error_without_a_reload() {
    let total = {
        let world = tierstore_world("remove-count");
        let counting = FaultFs::new(world.root(), world.log.clone());
        world
            .placer_on(&counting)
            .remove_all(&service("tierstore"))
            .expect("the removal");
        counting.writes()
    };
    assert_eq!(total, 10);
    for n in 1..=total {
        let world = tierstore_world(&format!("remove-fail-{n}"));
        let before = world.record();
        let failing = world.failing_at(n);

        let err = world
            .placer_on(&failing)
            .remove_all(&service("tierstore"))
            .expect_err("the write fails");

        assert!(matches!(err, PlacedError::Io { .. }), "write {n}: {err}");
        assert!(!world.unit_calls().contains(&UnitCall::DaemonReload));
        if n <= 9 {
            assert_eq!(world.record(), before, "write {n}");
        }
    }
}

/// Catches an error in naming the link being removed: a failing remove names the operation
/// `remove` and the path, and a failing directory sync names `fsync` and the directory.
#[test]
fn a_failed_remove_and_a_failed_sync_name_the_operation_and_the_path() {
    let world = tierstore_world("remove-io-names");
    let failing = world.failing_at(1);
    let err = world
        .placer_on(&failing)
        .remove_all(&service("tierstore"))
        .expect_err("the remove fails");
    assert!(matches!(
        &err,
        PlacedError::Io { op: "remove", path, .. } if *path == wants("tierstore@9301.service")
    ));

    let world = tierstore_world("remove-io-names-sync");
    let failing = world.failing_at(2);
    let err = world
        .placer_on(&failing)
        .remove_all(&service("tierstore"))
        .expect_err("the sync fails");
    assert!(matches!(
        &err,
        PlacedError::Io { op: "fsync", path, .. } if *path == Path::new(support::WANTS)
    ));
}

/// Catches a removal that ignores a path it cannot inspect: a recorded path below a regular file
/// is `PlacedError::Io` for `inspect`, and nothing is stopped or written.
#[test]
fn a_recorded_path_that_cannot_be_inspected_is_an_io_error() {
    let world = World::new("remove-inspect");
    fs::write(world.host("/etc/blocker"), b"file").expect("write a file");
    let mut record = world.record();
    record.paths.insert(
        PathBuf::from("/etc/blocker/child"),
        ghost_entry(PathKind::File),
    );
    Store::new(&world.fs, world.roots.state_dir())
        .save(&record)
        .expect("save");
    world.log.clear();

    let err = world
        .placer()
        .remove_all(&service("ghost"))
        .expect_err("the inspect fails");

    assert!(matches!(
        &err,
        PlacedError::Io { op: "inspect", path, .. } if *path == Path::new("/etc/blocker/child")
    ));
    assert!(world.log.events().is_empty());
}

/// Catches a removal that guesses a unit to stop for a path with no file name: such an absent
/// path is removed from the record with no stop and no write of a link, and a reload is due
/// because its kind is a unit.
#[test]
fn an_absent_path_without_a_file_name_is_dropped_with_no_stop() {
    let world = World::new("remove-nameless");
    let mut record = world.record();
    record
        .paths
        .insert(PathBuf::from("/ghostdir/.."), ghost_entry(PathKind::Unit));
    Store::new(&world.fs, world.roots.state_dir())
        .save(&record)
        .expect("save");
    world.log.clear();

    let removal = world
        .placer()
        .remove_all(&service("ghost"))
        .expect("the removal");

    assert_eq!(
        removal,
        Removal {
            stopped: Vec::new(),
            removed: vec![PathBuf::from("/ghostdir/..")],
            left_alone: Vec::new(),
            reloaded: true,
        }
    );
    assert_eq!(world.unit_calls(), [UnitCall::DaemonReload]);
    assert!(
        world
            .writes()
            .iter()
            .all(|write| !matches!(write, FsWrite::Remove(_)))
    );
    assert!(world.record().paths.is_empty());
}

/// Catches a removal that proceeds on a record it cannot read: a corrupt `placed.json` is
/// `PlacedError::Corrupt`, with no unit call.
#[test]
fn a_corrupt_record_stops_the_removal_before_any_unit_call() {
    let world = World::new("remove-corrupt");
    fs::write(world.host("/var/lib/hostkeeper/placed.json"), b"not json").expect("write");

    let err = world
        .placer()
        .remove_all(&service("tierstore"))
        .expect_err("the record is corrupt");

    assert!(matches!(err, PlacedError::Corrupt { .. }));
    assert!(world.log.events().is_empty());
}

/// Catches `unlink_unlisted` removing paths on a desired set it could not compute: a unit file
/// that cannot be read is `PlacedError::Lint`, and nothing is stopped or written.
#[test]
fn unlink_unlisted_refuses_a_version_whose_unit_cannot_be_read() {
    let world = World::new("remove-unreadable");
    let manifest = install(&world, "tierstore", "1.5.0", &Spec::tierstore());
    fs::remove_file(
        world
            .version_dir("tierstore", "1.5.0")
            .join("units/tierstore@.service"),
    )
    .expect("remove the unit");
    world.log.clear();

    let err = world
        .placer()
        .unlink_unlisted(&manifest)
        .expect_err("the unit is unreadable");

    assert!(matches!(
        &err,
        PlacedError::Lint(LintError::Unreadable { file, .. }) if file == "tierstore@.service"
    ));
    assert!(world.log.events().is_empty());
}

/// Catches a `.wants` link written anywhere but beside the units, or pointing at the wrong file:
/// the link for an instance points at its template, resolves into the version directory, and is
/// recorded as a placed `wants_link`; `enable_run_units` reports the write once and then none.
#[test]
fn run_unit_enable_writes_wantedby_links_and_records_them() {
    let (world, manifest) = unenabled_world("enable-links");
    let link = wants("tierstore@9301.service");
    assert!(world.read_link(&link).is_none());

    assert!(world.placer().enable_run_units(&manifest).expect("enable"));

    assert_eq!(
        world.read_link(&link),
        Some(unit_path("tierstore@.service"))
    );
    assert_eq!(
        world.resolve(&link),
        Some(PathBuf::from(
            "/var/lib/hostkeeper/packages/tierstore/1.5.0/units/tierstore@.service"
        ))
    );
    let record = world.record();
    let entry = record.paths.get(&link).expect("the wants entry");
    assert_eq!(entry.kind, PathKind::WantsLink);
    assert_eq!(entry.state, PathState::Placed);
    assert_eq!(entry.service, "tierstore");
    assert_eq!(entry.version, "1.5.0");
    assert_eq!(entry.link_target, unit_path("tierstore@.service"));

    world.log.clear();
    assert!(!world.placer().enable_run_units(&manifest).expect("again"));
    assert!(world.log.events().is_empty());
}

/// Catches `disable` that stops units, leaves a link or its entry, or reloads per link: it removes
/// the wants link and its entry, keeps the unit and file links, stops nothing and reloads once.
#[test]
fn disable_removes_the_wants_links_and_reloads_once_without_stopping() {
    let world = tierstore_world("disable");
    world.log.clear();

    let removal = world
        .placer()
        .disable(&service("tierstore"))
        .expect("disable");

    assert_eq!(
        removal,
        Removal {
            stopped: Vec::new(),
            removed: vec![wants("tierstore@9301.service")],
            left_alone: Vec::new(),
            reloaded: true,
        }
    );
    assert_eq!(world.unit_calls(), [UnitCall::DaemonReload]);
    assert!(world.read_link(wants("tierstore@9301.service")).is_none());
    assert!(world.read_link(unit_path("tierstore@.service")).is_some());
    assert!(world.read_link(CONF).is_some());
    let kinds: Vec<PathKind> = world.record().paths.values().map(|e| e.kind).collect();
    assert_eq!(kinds.len(), 2);
    assert!(!kinds.contains(&PathKind::WantsLink));

    world.log.clear();
    let again = world
        .placer()
        .disable(&service("tierstore"))
        .expect("again");
    assert_eq!(again, Removal::default());
    assert!(world.log.events().is_empty());
}

/// Catches `disable` deleting a wants path that is not the recorded link, or reloading when no
/// link went: the regular file stays, is reported, its entry is dropped, and no reload runs.
#[test]
fn disable_leaves_a_wants_path_that_is_not_the_recorded_link() {
    let world = tierstore_world("disable-foreign");
    let link = world.host(wants("tierstore@9301.service"));
    fs::remove_file(&link).expect("remove the link");
    fs::write(&link, b"kept\n").expect("write a file");
    world.log.clear();

    let removal = world
        .placer()
        .disable(&service("tierstore"))
        .expect("disable");

    assert_eq!(
        removal,
        Removal {
            stopped: Vec::new(),
            removed: Vec::new(),
            left_alone: vec![wants("tierstore@9301.service")],
            reloaded: false,
        }
    );
    assert_eq!(fs::read(link).expect("the file"), b"kept\n");
    assert!(world.unit_calls().is_empty());
    assert!(
        !world
            .record()
            .paths
            .contains_key(&wants("tierstore@9301.service"))
    );
}

/// Catches a failed reload after a disable being dropped: the links and entries are already gone
/// and the error is `PlacedError::Units`.
#[test]
fn disable_reports_a_failed_reload_after_the_record_is_saved() {
    let world = tierstore_world("disable-reload-fails");
    let failing = FakeUnitControl::failing(world.log.clone());
    let placer = Placer::new(world.roots.clone(), &world.fs, &failing);

    let err = placer
        .disable(&service("tierstore"))
        .expect_err("the reload fails");

    assert!(matches!(err, PlacedError::Units(_)));
    assert!(world.read_link(wants("tierstore@9301.service")).is_none());
    assert!(
        !world
            .record()
            .paths
            .contains_key(&wants("tierstore@9301.service"))
    );
}

/// Catches a plain run unit linked to a template or to the wrong file: a `run` unit that is a
/// unit file itself has its wants link point at its own placed unit file.
#[test]
fn a_plain_run_unit_links_to_its_own_unit_file() {
    let world = World::new("enable-plain");
    let spec = Spec::new()
        .unit("tierstore-cache.service", &exec())
        .run("tierstore-cache.service");
    install(&world, "tierstore", "1.5.0", &spec);

    assert_eq!(
        world.read_link(wants("tierstore-cache.service")),
        Some(unit_path("tierstore-cache.service"))
    );
    let record = world.record();
    let entry = record
        .paths
        .get(&wants("tierstore-cache.service"))
        .expect("the wants entry");
    assert_eq!(entry.kind, PathKind::WantsLink);
    assert_eq!(entry.link_target, unit_path("tierstore-cache.service"));
}

/// Catches a unit with several `WantedBy=` targets getting one link only: each target has its own
/// `.wants` directory and link.
#[test]
fn each_wantedby_target_gets_its_own_link() {
    let world = World::new("enable-two-targets");
    let text = format!(
        "[Unit]\nDescription=x\n\n[Service]\nExecStart=/bin/true\n\n[Install]\nWantedBy={} extra.service\n",
        support::TARGET
    );
    let spec = Spec::new()
        .unit("tierstore-cache.service", &text)
        .run("tierstore-cache.service");
    install(&world, "tierstore", "1.5.0", &spec);

    let target = Some(unit_path("tierstore-cache.service"));
    assert_eq!(world.read_link(wants("tierstore-cache.service")), target);
    assert_eq!(
        world.read_link("/etc/systemd/system/extra.service.wants/tierstore-cache.service"),
        target
    );
}

/// Catches a run unit with no `[Install]` being enabled silently: it is `LintError::NoWantedBy`
/// naming the unit and its file, and nothing is written.
#[test]
fn a_run_unit_without_install_is_refused_by_enable() {
    let world = World::new("enable-no-install");
    let spec = Spec::new()
        .unit("tierstore@.service", &unit_without_install("/bin/true"))
        .run("tierstore@9301.service");
    let manifest = world.package("tierstore", "1.5.0", &spec);

    let err = world
        .placer()
        .enable_run_units(&manifest)
        .expect_err("no WantedBy");

    assert!(matches!(
        &err,
        PlacedError::Lint(LintError::NoWantedBy { unit, file })
            if unit == "tierstore@9301.service" && file == "tierstore@.service"
    ));
    assert_eq!(err.reason(), "manifest_mismatch");
    assert!(world.log.events().is_empty());
}

/// Catches a `WantedBy=` naming the same target twice producing two writes of one path: the
/// duplicate claim is refused as `ClaimError::Duplicate`; a target that is not a unit name is
/// `LintError::BadTarget`.
#[test]
fn a_repeated_or_malformed_wantedby_target_is_refused() {
    let world = World::new("enable-bad-targets");
    let text = |targets: &str| {
        format!(
            "[Unit]\nDescription=x\n\n[Service]\nExecStart=/bin/true\n\n[Install]\nWantedBy={targets}\n"
        )
    };
    let twice = format!("{} {}", support::TARGET, support::TARGET);
    let spec = Spec::new()
        .unit("tierstore-cache.service", &text(&twice))
        .run("tierstore-cache.service");
    let manifest = world.package("tierstore", "1.5.0", &spec);
    let err = world
        .placer()
        .enable_run_units(&manifest)
        .expect_err("a duplicate");
    assert!(matches!(
        &err,
        PlacedError::Claim(ClaimError::Duplicate { path }) if *path == wants("tierstore-cache.service")
    ));

    let spec = Spec::new()
        .unit("tierstore-cache.service", &text(".hidden"))
        .run("tierstore-cache.service");
    let manifest = world.package("tierstore", "1.5.1", &spec);
    let err = world
        .placer()
        .enable_run_units(&manifest)
        .expect_err("a bad target");
    assert!(matches!(
        &err,
        PlacedError::Lint(LintError::BadTarget { file, target })
            if file == "tierstore-cache.service" && target == ".hidden"
    ));
    assert!(world.log.events().is_empty());
}

/// Catches an enable that overwrites what it does not own: a regular file where the wants link
/// belongs is `PlacedError::PathConflict`, and keeps its bytes.
#[test]
fn enable_refuses_a_wants_path_held_by_a_file() {
    let (world, manifest) = unenabled_world("enable-conflict");
    let link = world.host(wants("tierstore@9301.service"));
    fs::create_dir_all(link.parent().expect("a parent")).expect("create the directory");
    fs::write(&link, b"mine\n").expect("write a file");

    let err = world
        .placer()
        .enable_run_units(&manifest)
        .expect_err("a conflict");

    assert!(matches!(
        &err,
        PlacedError::PathConflict { path, .. } if *path == wants("tierstore@9301.service")
    ));
    assert_eq!(fs::read(link).expect("the file"), b"mine\n");
}

/// Catches a failing write at any step of enabling being swallowed: the n-th write failing, for
/// every n of begin, link and commit, is an error, and one write more than needed succeeds.
#[test]
fn a_failing_write_at_any_step_of_enable_is_an_error() {
    let total = {
        let (world, manifest) = unenabled_world("enable-count");
        let counting = FaultFs::new(world.root(), world.log.clone());
        world
            .placer_on(&counting)
            .enable_run_units(&manifest)
            .expect("enable");
        counting.writes()
    };
    assert!(total > 8);
    for n in 1..=total {
        let (world, manifest) = unenabled_world(&format!("enable-fail-{n}"));
        let failing = world.failing_at(n);
        let err = world
            .placer_on(&failing)
            .enable_run_units(&manifest)
            .expect_err("the write fails");
        assert!(matches!(err, PlacedError::Io { .. }), "write {n}: {err}");
    }
    let (world, manifest) = unenabled_world("enable-fail-none");
    let failing = world.failing_at(total + 1);
    assert!(
        world
            .placer_on(&failing)
            .enable_run_units(&manifest)
            .expect("enable")
    );
}
