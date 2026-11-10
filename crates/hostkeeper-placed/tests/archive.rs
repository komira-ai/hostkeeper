//! The paths an archive version placed: their removal when a native version is installed over
//! it ([`Placer::take_away_archive_paths`]), and the start's repair of a dangling unit link
//! ([`Placer::remove_dangling_unit_links`]).

mod support;

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;

use hostkeeper_placed::testing::{Event, FakeUnitControl, FaultFs, FsWrite, UnitCall};
use hostkeeper_placed::{ArchiveKind, ArchivePath, PlacedError, PlacedRecord, Placer, TakenAway};
use serde_json::{Value, json};
use support::World;

const PLACED: &str = "/var/lib/hostkeeper/placed.json";
const PACKAGES: &str = "/var/lib/hostkeeper/packages";
const UNIT: &str = "/etc/systemd/system/hostkeeper.service";
const WANTS: &str = "/etc/systemd/system/multi-user.target.wants/hostkeeper.service";
const DEB_UNIT: &str = "/usr/lib/systemd/system/hostkeeper.service";

/// Where the archive version of `service` linked its unit `file`.
fn unit_target(service: &str, file: &str) -> String {
    format!("{PACKAGES}/{service}/current/units/{file}")
}

/// One entry of `paths` as the release before the native installers wrote it.
fn entry(service: &str, kind: &str, link_target: &str) -> Value {
    json!({
        "service": service,
        "version": "0.1.240",
        "kind": kind,
        "state": "placed",
        "link_target": link_target,
    })
}

/// Writes `placed.json` with `paths`, hostkeeper recorded as installed by `apt`, a tombstone
/// and a key of another writer.
fn seed(world: &World, paths: Value) {
    let record = json!({
        "schema": 1,
        "paths": paths,
        "reload_pending": false,
        "uninstalling": {"old": {}},
        "installed": {"hostkeeper": {"installer": "apt", "package": "hostkeeper"}},
        "newer_key": {"kept": true},
    });
    fs::write(
        world.host(PLACED),
        serde_json::to_vec_pretty(&record).expect("json"),
    )
    .expect("write placed.json");
}

/// `placed.json` as JSON.
fn on_disk(world: &World) -> Value {
    serde_json::from_slice(&world.placed_bytes().expect("placed.json is there")).expect("json")
}

/// Makes a link at the host path `link` whose target is `target`, written as it is.
fn link(world: &World, target: &str, at: &str) {
    let disk = world.host(at);
    fs::create_dir_all(disk.parent().expect("a parent")).expect("make the directory");
    symlink(target, disk).expect("make the link");
}

/// Writes a regular file at the host path `at`.
fn file(world: &World, at: &str, text: &str) {
    let disk = world.host(at);
    fs::create_dir_all(disk.parent().expect("a parent")).expect("make the directory");
    fs::write(disk, text).expect("write the file");
}

/// Whether anything, a dangling link included, is at the host path `at`.
fn there(world: &World, at: &str) -> bool {
    fs::symlink_metadata(world.host(at)).is_ok()
}

fn paths(list: &[&str]) -> Vec<PathBuf> {
    list.iter().map(PathBuf::from).collect()
}

/// Catches the move leaving the archive's unit link in `/etc/systemd/system/` once `current`
/// names the native version: the dangling link hides the package's unit, systemd finds none, and
/// the daemon's own unit never loads again. The link is removed and `daemon-reload` runs after
/// the removal; the record keeps the entry until it is forgotten.
#[test]
fn a_dangling_unit_link_the_record_lists_is_removed_then_systemd_reloads() {
    let world = World::new("archive-dangling-unit");
    let target = unit_target("hostkeeper", "hostkeeper.service");
    seed(
        &world,
        json!({ UNIT: entry("hostkeeper", "unit", &target) }),
    );
    link(&world, &target, UNIT);
    world.log.clear();

    let taken = world
        .placer()
        .take_away_archive_paths("hostkeeper")
        .expect("taken away");

    assert_eq!(
        taken,
        TakenAway {
            removed: paths(&[UNIT]),
            links: vec![(PathBuf::from(UNIT), PathBuf::from(&target))],
            left_alone: Vec::new(),
            units: vec!["hostkeeper.service".to_owned()],
            reloaded: true,
        }
    );
    assert!(!there(&world, UNIT));
    assert_eq!(on_disk(&world)["paths"][UNIT]["kind"], "unit");
    let removed = world
        .log
        .events()
        .iter()
        .position(|e| *e == Event::Fs(FsWrite::Remove(PathBuf::from(UNIT))));
    let reloaded = world
        .log
        .events()
        .iter()
        .position(|e| *e == Event::Unit(UnitCall::DaemonReload));
    assert!(
        removed.is_some() & (removed < reloaded),
        "{:?}",
        world.log.events()
    );
    assert_eq!(world.unit_calls(), [UnitCall::DaemonReload]);

    assert!(
        world
            .placer()
            .forget_archive_paths("hostkeeper")
            .expect("forgotten")
    );
    assert_eq!(on_disk(&world)["paths"], json!({}));
}

/// Catches a unit kept because it is a regular file, or because its link still resolves: a unit
/// file of the archive's at that path hides the package's whatever it is.
#[test]
fn a_regular_unit_file_and_a_unit_link_that_resolves_are_removed_too() {
    let world = World::new("archive-regular-unit");
    let live = "/etc/systemd/system/hello-a.service";
    seed(
        &world,
        json!({
            UNIT: entry("hostkeeper", "unit", &unit_target("hostkeeper", "hostkeeper.service")),
            live: entry("hostkeeper", "unit", &unit_target("hostkeeper", "hello-a.service")),
        }),
    );
    file(&world, UNIT, "[Service]\n");
    file(&world, "/opt/unit/hello-a.service", "[Service]\n");
    link(&world, "/opt/unit/hello-a.service", live);

    let taken = world
        .placer()
        .take_away_archive_paths("hostkeeper")
        .expect("taken away");

    assert_eq!(taken.removed, paths(&[live, UNIT]));
    assert_eq!(taken.units, ["hello-a.service", "hostkeeper.service"]);
    assert!(!there(&world, UNIT) & !there(&world, live));
    assert!(there(&world, "/opt/unit/hello-a.service"));
}

/// Catches the removal of a `WantedBy=` link or a file that is not the archive's: the deb's own
/// `systemctl enable` link and a conffile stay, and the archive's own links of both kinds go.
#[test]
fn wants_links_and_files_go_only_when_they_are_the_archives_links() {
    let world = World::new("archive-wants-files");
    let conf_ours = "/etc/hostkeeper/archive.conf";
    let conf_theirs = "/etc/hostkeeper/hostkeeper.toml";
    let wants_other = "/etc/systemd/system/timers.target.wants/hostkeeper.service";
    seed(
        &world,
        json!({
            WANTS: entry("hostkeeper", "wants_link", UNIT),
            wants_other: entry("hostkeeper", "wants_link", UNIT),
            conf_ours: entry("hostkeeper", "file", &format!("{PACKAGES}/hostkeeper/current/files/a.conf")),
            conf_theirs: entry("hostkeeper", "file", &format!("{PACKAGES}/hostkeeper/current/files/t.toml")),
        }),
    );
    // The archive's `WantedBy=` link, to the unit path it recorded.
    link(&world, UNIT, WANTS);
    // The deb's `systemctl enable` link at another `WantedBy=` path the record lists.
    link(&world, DEB_UNIT, wants_other);
    // The archive's file link, written relative, into the packages directory.
    link(
        &world,
        "../../var/lib/hostkeeper/packages/hostkeeper/current/files/a.conf",
        conf_ours,
    );
    // The package's conffile, a regular file.
    file(&world, conf_theirs, "[daemon]\n");

    let taken = world
        .placer()
        .take_away_archive_paths("hostkeeper")
        .expect("taken away");

    assert_eq!(
        taken,
        TakenAway {
            removed: paths(&[conf_ours, WANTS]),
            links: vec![
                (
                    PathBuf::from(conf_ours),
                    PathBuf::from(
                        "../../var/lib/hostkeeper/packages/hostkeeper/current/files/a.conf"
                    ),
                ),
                (PathBuf::from(WANTS), PathBuf::from(UNIT)),
            ],
            left_alone: paths(&[conf_theirs, wants_other]),
            units: Vec::new(),
            reloaded: true,
        }
    );
    assert_eq!(world.read_link(wants_other), Some(PathBuf::from(DEB_UNIT)));
    assert_eq!(
        fs::read_to_string(world.host(conf_theirs)).expect("kept"),
        "[daemon]\n"
    );
}

/// Catches a reload with no unit gone, and a unit already gone left out of the units to check:
/// only an archive file link went, so systemd is not asked to reload, and the unit the record
/// lists is still one the package has to answer to; forgetting drops both entries.
#[test]
fn a_file_alone_and_a_path_already_gone_reload_nothing() {
    let world = World::new("archive-files-only");
    let conf = "/etc/hostkeeper/archive.conf";
    let target = format!("{PACKAGES}/hostkeeper/current/files/a.conf");
    seed(
        &world,
        json!({
            conf: entry("hostkeeper", "file", &target),
            UNIT: entry("hostkeeper", "unit", &unit_target("hostkeeper", "hostkeeper.service")),
        }),
    );
    link(&world, &target, conf);

    let taken = world
        .placer()
        .take_away_archive_paths("hostkeeper")
        .expect("taken away");

    assert_eq!(
        taken,
        TakenAway {
            removed: paths(&[conf]),
            links: vec![(PathBuf::from(conf), PathBuf::from(&target))],
            units: vec!["hostkeeper.service".to_owned()],
            ..TakenAway::default()
        }
    );
    assert_eq!(world.unit_calls(), []);
    assert!(
        world
            .placer()
            .forget_archive_paths("hostkeeper")
            .expect("forgotten")
    );
    assert_eq!(on_disk(&world)["paths"], json!({}));
}

/// Catches a directory at a unit path deleted or read as the archive's: it is left, and it is no
/// unit to check.
#[test]
fn a_directory_at_a_unit_path_is_left_alone() {
    let world = World::new("archive-dir-unit");
    seed(
        &world,
        json!({ UNIT: entry("hostkeeper", "unit", &unit_target("hostkeeper", "hostkeeper.service")) }),
    );
    fs::create_dir_all(world.host(UNIT)).expect("a directory");

    let taken = world
        .placer()
        .take_away_archive_paths("hostkeeper")
        .expect("taken away");

    assert_eq!(taken.left_alone, paths(&[UNIT]));
    assert_eq!(taken.units, Vec::<String>::new());
    assert!(world.host(UNIT).is_dir());
    assert_eq!(world.unit_calls(), []);
}

/// Catches the removal reaching past the service: another service's paths, an entry of a kind
/// this daemon does not know, an entry with no service, and every other key of the record stay
/// as they were.
#[test]
fn only_the_services_known_entries_are_touched() {
    let world = World::new("archive-others");
    let web = "/etc/systemd/system/web.service";
    let odd = "/etc/systemd/system/odd.service";
    let nameless = "/etc/systemd/system/nameless.service";
    let web_entry = entry("web", "unit", &unit_target("web", "web.service"));
    let odd_entry = entry(
        "hostkeeper",
        "socket",
        &unit_target("hostkeeper", "odd.service"),
    );
    let nameless_entry = json!({"kind": "unit"});
    seed(
        &world,
        json!({
            UNIT: entry("hostkeeper", "unit", &unit_target("hostkeeper", "hostkeeper.service")),
            web: web_entry.clone(),
            odd: odd_entry.clone(),
            nameless: nameless_entry.clone(),
        }),
    );
    for path in [UNIT, web, odd, nameless] {
        link(&world, &unit_target("x", "gone.service"), path);
    }
    let before = on_disk(&world);

    let taken = world
        .placer()
        .take_away_archive_paths("hostkeeper")
        .expect("taken away");

    assert_eq!(taken.removed, paths(&[UNIT]));
    assert!(there(&world, web) & there(&world, odd) & there(&world, nameless));
    assert!(
        world
            .placer()
            .forget_archive_paths("hostkeeper")
            .expect("forgotten")
    );
    let after = on_disk(&world);
    assert_eq!(
        after["paths"],
        json!({ web: web_entry, odd: odd_entry, nameless: nameless_entry })
    );
    for key in [
        "installed",
        "uninstalling",
        "newer_key",
        "schema",
        "reload_pending",
    ] {
        assert_eq!(after[key], before[key], "{key}");
    }
    assert_eq!(
        world.record().archive_paths("web"),
        [ArchivePath {
            path: PathBuf::from(web),
            kind: ArchiveKind::Unit,
            link_target: Some(PathBuf::from(unit_target("web", "web.service"))),
        }]
    );
}

/// Catches a write or a reload where there is nothing to take away: a record with no `paths`
/// key, one whose `paths` is not a map, and one that lists none of the service's.
#[test]
fn a_record_with_nothing_of_the_service_is_not_written() {
    for (name, paths) in [
        ("none", None),
        ("not-a-map", Some(json!(["/etc/systemd/system/x.service"]))),
        (
            "others",
            Some(json!({"/etc/systemd/system/web.service": entry("web", "unit", "/x")})),
        ),
    ] {
        let world = World::new(&format!("archive-nothing-{name}"));
        if let Some(paths) = paths {
            seed(&world, paths);
        }
        let before = world.placed_bytes();
        world.log.clear();

        let taken = world
            .placer()
            .take_away_archive_paths("hostkeeper")
            .expect("nothing to do");

        assert_eq!(taken, TakenAway::default(), "{name}");
        let forgot = world
            .placer()
            .forget_archive_paths("hostkeeper")
            .expect("nothing to forget");
        assert!(!forgot, "{name}");
        assert_eq!(world.writes(), [], "{name}");
        assert_eq!(world.unit_calls(), [], "{name}");
        assert_eq!(world.placed_bytes(), before, "{name}");
    }
    assert_eq!(PlacedRecord::default().archive_paths("hostkeeper"), []);
}

/// Catches a failed removal reported as done, or the record written without the path the
/// removal did not take: the error is the removal's, and the record still lists the path.
#[test]
fn a_removal_that_fails_leaves_the_record_listing_the_path() {
    let world = World::new("archive-remove-fails");
    let target = unit_target("hostkeeper", "hostkeeper.service");
    seed(
        &world,
        json!({ UNIT: entry("hostkeeper", "unit", &target) }),
    );
    link(&world, &target, UNIT);
    let fs = world.failing_at(1);

    let error = world
        .placer_on(&fs)
        .take_away_archive_paths("hostkeeper")
        .expect_err("the removal fails");

    assert!(
        matches!(&error, PlacedError::Io { op: "remove", path, .. } if path == &PathBuf::from(UNIT)),
        "{error}"
    );
    assert!(there(&world, UNIT));
    assert_eq!(world.record().archive_paths("hostkeeper").len(), 1);
    assert_eq!(world.unit_calls(), []);
}

/// Catches a failed `daemon-reload` reported as done: the error says so, after the link went;
/// the record still lists it, so a repeat still names the unit to check.
#[test]
fn a_reload_that_fails_is_an_error_after_the_removal() {
    let world = World::new("archive-reload-fails");
    let target = unit_target("hostkeeper", "hostkeeper.service");
    seed(
        &world,
        json!({ UNIT: entry("hostkeeper", "unit", &target) }),
    );
    link(&world, &target, UNIT);
    let units = FakeUnitControl::new(world.log.clone());
    units.fail_reload();

    let error = Placer::new(world.roots.clone(), &world.fs, &units)
        .take_away_archive_paths("hostkeeper")
        .expect_err("the reload fails");

    assert!(matches!(error, PlacedError::Units(_)), "{error}");
    assert!(!there(&world, UNIT));
    let again = world
        .placer()
        .take_away_archive_paths("hostkeeper")
        .expect("the repeat");
    assert_eq!(
        again,
        TakenAway {
            units: vec!["hostkeeper.service".to_owned()],
            ..TakenAway::default()
        }
    );
}

/// Catches a record whose write fails read as forgotten: the error is the write's, and the
/// record still lists the path.
#[test]
fn a_forget_whose_write_fails_keeps_the_entries() {
    let world = World::new("archive-forget-fails");
    seed(
        &world,
        json!({ UNIT: entry("hostkeeper", "unit", &unit_target("hostkeeper", "hostkeeper.service")) }),
    );
    let fs = world.failing_at(1);

    let error = world
        .placer_on(&fs)
        .forget_archive_paths("hostkeeper")
        .expect_err("the write fails");

    assert!(
        matches!(error, PlacedError::Io { op: "write", .. }),
        "{error}"
    );
    assert_eq!(world.record().archive_paths("hostkeeper").len(), 1);
}

fn native(services: &[&str]) -> BTreeSet<String> {
    services.iter().map(|s| (*s).to_owned()).collect()
}

/// Catches a host the move left with a dangling unit link staying broken across restarts: the
/// start removes a link into a native service's package directory whose target is gone, and
/// reloads, and leaves every other link and file of the unit directory alone.
#[test]
fn the_start_removes_a_dangling_link_into_a_native_services_package_dir() {
    let world = World::new("archive-repair");
    let resolving = "/etc/systemd/system/hello-a.service";
    let operator = "/etc/systemd/system/custom.service";
    let foreign = "/etc/systemd/system/web.service";
    let regular = "/etc/systemd/system/local.service";
    let relative = "/etc/systemd/system/hk-relative.service";
    // A version directory that holds the unit `hello-a.service`, which `current` names.
    let unit = format!("{PACKAGES}/hostkeeper/0.1.245/units/hello-a.service");
    file(&world, &unit, "[Service]\n");
    world.set_link("hostkeeper", "current", "0.1.245");
    link(
        &world,
        &unit_target("hostkeeper", "hostkeeper.service"),
        UNIT,
    );
    link(
        &world,
        &unit_target("hostkeeper", "hello-a.service"),
        resolving,
    );
    link(&world, "/opt/units/custom.service", operator);
    link(&world, &unit_target("web", "web.service"), foreign);
    file(&world, regular, "[Service]\n");
    link(
        &world,
        "../../../var/lib/hostkeeper/packages/hostkeeper/current/units/gone.service",
        relative,
    );
    world.log.clear();

    let removed = world
        .placer()
        .remove_dangling_unit_links(&native(&["hostkeeper"]))
        .expect("repaired");

    assert_eq!(removed, paths(&[relative, UNIT]));
    assert!(!there(&world, UNIT) & !there(&world, relative));
    for kept in [resolving, operator, foreign, regular] {
        assert!(there(&world, kept), "{kept}");
    }
    assert_eq!(world.unit_calls(), [UnitCall::DaemonReload]);
}

/// Catches a dangling link judged by a part of its target: a target whose directory is a file is
/// as gone as one whose file is missing, and a service whose name only starts another's is not
/// that service.
#[test]
fn a_target_below_a_file_is_gone_and_a_prefix_of_a_name_is_not_the_service() {
    let world = World::new("archive-repair-edges");
    let below_file = "/etc/systemd/system/a.service";
    let longer_name = "/etc/systemd/system/b.service";
    file(
        &world,
        &format!("{PACKAGES}/hostkeeper/current"),
        "not a directory",
    );
    link(&world, &unit_target("hostkeeper", "a.service"), below_file);
    link(
        &world,
        &unit_target("hostkeeper-pxe", "b.service"),
        longer_name,
    );

    let removed = world
        .placer()
        .remove_dangling_unit_links(&native(&["hostkeeper"]))
        .expect("repaired");

    assert_eq!(removed, paths(&[below_file]));
    assert!(there(&world, longer_name));
}

/// Catches a reload with nothing removed, and a missing unit directory read as an error.
#[test]
fn the_start_with_nothing_dangling_reloads_nothing() {
    let world = World::new("archive-repair-nothing");
    link(
        &world,
        &unit_target("web", "web.service"),
        "/etc/systemd/system/web.service",
    );
    world.log.clear();

    let removed = world
        .placer()
        .remove_dangling_unit_links(&native(&["hostkeeper"]))
        .expect("nothing to repair");
    assert_eq!(removed, Vec::<PathBuf>::new());
    assert_eq!(world.unit_calls(), []);
    assert_eq!(world.writes(), []);

    fs::remove_dir_all(world.host("/etc/systemd")).expect("remove the unit directory");
    assert_eq!(
        world
            .placer()
            .remove_dangling_unit_links(&native(&["hostkeeper"]))
            .expect("no unit directory"),
        Vec::<PathBuf>::new()
    );
}

/// Catches the repair's failures passed over: a unit directory that cannot be listed, a removal
/// that fails and a reload that fails are each an error naming what failed.
#[test]
fn the_repairs_failures_are_errors() {
    let world = World::new("archive-repair-fails");
    link(
        &world,
        &unit_target("hostkeeper", "hostkeeper.service"),
        UNIT,
    );

    let fs = world.failing_at(1);
    let error = world
        .placer_on(&fs)
        .remove_dangling_unit_links(&native(&["hostkeeper"]))
        .expect_err("the removal fails");
    assert!(
        matches!(error, PlacedError::Io { op: "remove", .. }),
        "{error}"
    );
    assert!(there(&world, UNIT));

    let units = FakeUnitControl::new(world.log.clone());
    units.fail_reload();
    let error = Placer::new(world.roots.clone(), &world.fs, &units)
        .remove_dangling_unit_links(&native(&["hostkeeper"]))
        .expect_err("the reload fails");
    assert!(matches!(error, PlacedError::Units(_)), "{error}");
    assert!(!there(&world, UNIT));

    // The fourth read is the inspection of the link's target, after the list, the inspection
    // of the link and its read.
    link(
        &world,
        &unit_target("hostkeeper", "hostkeeper.service"),
        UNIT,
    );
    let reads = FaultFs::new(world.root(), world.log.clone()).failing_read(4, true);
    let error = world
        .placer_on(&reads)
        .remove_dangling_unit_links(&native(&["hostkeeper"]))
        .expect_err("the target cannot be inspected");
    assert!(
        matches!(&error, PlacedError::Io { op: "inspect", path, .. }
            if path == &PathBuf::from(unit_target("hostkeeper", "hostkeeper.service"))),
        "{error}"
    );
    assert!(there(&world, UNIT));

    fs::remove_dir_all(world.host("/etc/systemd/system")).expect("remove the unit directory");
    file(&world, "/etc/systemd/system", "not a directory");
    let error = world
        .placer()
        .remove_dangling_unit_links(&native(&["hostkeeper"]))
        .expect_err("the unit directory is a file");
    assert!(
        matches!(error, PlacedError::Io { op: "list", .. }),
        "{error}"
    );
}

/// Catches an install that failed after the take-away leaving the service with no unit systemd
/// knows: each link removed is put back as it was written, a path something else took meanwhile
/// is left as it is, and systemd reloads once.
#[test]
fn the_links_taken_away_are_put_back_where_the_path_is_free() {
    let world = World::new("archive-restore");
    let target = unit_target("hostkeeper", "hostkeeper.service");
    seed(
        &world,
        json!({
            UNIT: entry("hostkeeper", "unit", &target),
            WANTS: entry("hostkeeper", "wants_link", UNIT),
        }),
    );
    link(&world, &target, UNIT);
    link(&world, UNIT, WANTS);
    let taken = world
        .placer()
        .take_away_archive_paths("hostkeeper")
        .expect("taken away");
    file(&world, UNIT, "[Service]\n");
    world.log.clear();

    let restored = world
        .placer()
        .restore_archive_links(&taken)
        .expect("put back");

    assert_eq!(restored, paths(&[WANTS]));
    assert_eq!(world.read_link(WANTS), Some(PathBuf::from(UNIT)));
    assert_eq!(
        fs::read_to_string(world.host(UNIT)).expect("the file is kept"),
        "[Service]\n"
    );
    assert_eq!(world.unit_calls(), [UnitCall::DaemonReload]);
}

/// Catches a reload with nothing put back, and a failure to put a link back passed over.
#[test]
fn a_restore_with_nothing_to_do_reloads_nothing_and_its_failures_are_errors() {
    let world = World::new("archive-restore-edges");
    world.log.clear();
    assert_eq!(
        world
            .placer()
            .restore_archive_links(&TakenAway::default())
            .expect("nothing"),
        Vec::<PathBuf>::new()
    );
    assert_eq!(world.unit_calls(), []);

    let taken = TakenAway {
        links: vec![(PathBuf::from(UNIT), PathBuf::from("/x"))],
        ..TakenAway::default()
    };
    let fs = world.failing_at(1);
    let error = world
        .placer_on(&fs)
        .restore_archive_links(&taken)
        .expect_err("the link fails");
    assert!(
        matches!(error, PlacedError::Io { op: "link", .. }),
        "{error}"
    );

    let units = FakeUnitControl::new(world.log.clone());
    units.fail_reload();
    let error = Placer::new(world.roots.clone(), &world.fs, &units)
        .restore_archive_links(&taken)
        .expect_err("the reload fails");
    assert!(matches!(error, PlacedError::Units(_)), "{error}");
    assert_eq!(world.read_link(UNIT), Some(PathBuf::from("/x")));
}

/// Catches a unit entry whose path names no file read as a unit: it is no unit to check.
#[test]
fn a_unit_entry_with_no_file_name_names_no_unit() {
    let world = World::new("archive-no-name");
    seed(&world, json!({ "/": entry("hostkeeper", "unit", "/x") }));

    let taken = world
        .placer()
        .take_away_archive_paths("hostkeeper")
        .expect("taken away");

    assert_eq!(taken.units, Vec::<String>::new());
    assert_eq!(taken.left_alone, paths(&["/"]));
}
