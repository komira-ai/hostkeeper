//! `Placer::desired` and `Placer::relink_to`: the paths one manifest places, and the relink of a
//! service to one manifest, which links what it lists, removes what the record holds beyond it,
//! and runs `daemon-reload` once. Each case builds a scratch host and breaks one thing.

mod support;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use hostkeeper_config::manifest::{FileEntry, Manifest};
use hostkeeper_placed::testing::{FakeUnitControl, UnitCall};
use hostkeeper_placed::{ClaimError, LintError, PathState, PlacedError, Placer, Roots, Tombstone};
use support::{Spec, TARGET, WANTS, World, install, unit_text, unit_without_install};

const PACKAGES: &str = "/var/lib/hostkeeper/packages";
const CONF: &str = "/etc/tierstore/tierstore.conf";
const EXTRA: &str = "/etc/tierstore/extra.conf";
const EXEC: &str = "/var/lib/hostkeeper/packages/tierstore/current/bin/tierstore";

/// tierstore with one more file.
fn with_extra() -> Spec {
    Spec::tierstore().file("files/extra.conf", EXTRA)
}

/// tierstore with one more unit, which is not a `run` unit and has no `[Install]`.
fn with_main_unit() -> Spec {
    Spec::tierstore().unit("tierstore.service", &unit_without_install(EXEC))
}

fn wants_path() -> String {
    format!("{WANTS}/tierstore@9301.service")
}

fn package_link(world: &World, name: &str) -> Option<PathBuf> {
    world.read_link(format!("{PACKAGES}/tierstore/{name}"))
}

/// Catches a desired set that misses a kind of path: tierstore places its template, its file
/// and the `WantedBy=` link of its run unit.
#[test]
fn desired_lists_units_files_and_wants_links() {
    let world = World::new("desired");
    let manifest = world.package("tierstore", "1.5.0", &Spec::tierstore());
    let desired = world.placer().desired(&manifest).expect("desired");

    let expected: BTreeSet<PathBuf> = [
        "/etc/systemd/system/tierstore@.service".to_owned(),
        CONF.to_owned(),
        wants_path(),
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect();
    assert_eq!(desired.paths(), expected);
}

/// Catches a bad claim or a unit without `WantedBy=` passing `desired`: a `[[files]]` path
/// under a refused prefix is a claim error, a run unit with no `[Install]` is a lint error, and
/// a file claiming the path of a `WantedBy=` link is a duplicate (with a unit directory that
/// is not itself refused).
#[test]
fn desired_refuses_what_claims_and_lint_refuse() {
    let world = World::new("desired-refused");
    let placer = world.placer();

    let mut manifest = world.package("tierstore", "1.5.0", &Spec::tierstore());
    manifest.files.push(FileEntry {
        source: "files/tierstore.conf".to_owned(),
        path: "/usr/bin/x".to_owned(),
    });
    assert!(matches!(
        placer.desired(&manifest),
        Err(PlacedError::Claim(ClaimError::Refused { .. }))
    ));

    let spec = Spec::new()
        .unit("tierstore.service", &unit_without_install(EXEC))
        .run("tierstore.service");
    let bare = world.package("tierstore", "1.6.0", &spec);
    assert!(matches!(
        placer.desired(&bare),
        Err(PlacedError::Lint(LintError::NoWantedBy { .. }))
    ));

    let roots = Roots::new(
        Path::new("/var/lib/hostkeeper"),
        Path::new("/etc/units"),
        Path::new(PACKAGES),
    );
    let placer = Placer::new(roots, &world.fs, &world.units);
    let spec = Spec::new()
        .unit("tierstore.service", &unit_text(EXEC))
        .run("tierstore.service");
    let mut manifest = world.package("tierstore", "1.7.0", &spec);
    let wants = format!("/etc/units/{TARGET}.wants/tierstore.service");
    manifest.files.push(FileEntry {
        source: "files/tierstore.conf".to_owned(),
        path: wants.clone(),
    });
    match placer.desired(&manifest) {
        Err(PlacedError::Claim(ClaimError::Duplicate { path })) => {
            assert_eq!(path, Path::new(&wants));
        }
        other => panic!("expected a duplicate claim, got {other:?}"),
    }
}

/// Catches `relink_to` leaving what the older version does not list: from 1.5.0 back to the
/// manifest of 1.4.2, which lists fewer files, the unlisted file and unit links are removed and
/// dropped from the record, the kept ones stay, the unit is stopped and `daemon-reload` runs
/// once; a second `relink_to` returns false and calls nothing.
#[test]
fn relink_to_removes_what_the_older_manifest_does_not_list() {
    let world = World::new("relink-back");
    let rich = with_main_unit().file("files/extra.conf", EXTRA);
    install(&world, "tierstore", "1.5.0", &rich);
    let older = world.package("tierstore", "1.4.2", &Spec::tierstore());
    let placer = world.placer();
    world.log.clear();

    assert!(placer.relink_to(&older).expect("relink"));
    assert_eq!(
        world.unit_calls(),
        [
            UnitCall::Stop("tierstore.service".to_owned()),
            UnitCall::DaemonReload
        ]
    );
    for gone in [EXTRA, "/etc/systemd/system/tierstore.service"] {
        assert!(fs::symlink_metadata(world.host(gone)).is_err(), "{gone}");
    }
    let record = world.record();
    assert_eq!(record.paths.len(), 3);
    for kept in [
        CONF.to_owned(),
        "/etc/systemd/system/tierstore@.service".to_owned(),
        wants_path(),
    ] {
        let entry = &record.paths[Path::new(&kept)];
        assert_eq!(entry.version, "1.4.2", "{kept}");
        assert_eq!(entry.state, PathState::Placed, "{kept}");
        assert!(fs::symlink_metadata(world.host(&kept)).is_ok(), "{kept}");
    }
    assert_eq!(package_link(&world, "current"), Some("1.5.0".into()));

    world.log.clear();
    assert!(!placer.relink_to(&older).expect("the repeat"));
    assert!(world.unit_calls().is_empty());
    assert!(world.writes().is_empty());
}

/// Catches a missing reload when only links changed: a manifest that adds a file makes
/// `relink_to` link it and run `daemon-reload` once; a repeat changes nothing.
#[test]
fn relink_to_reloads_once_when_only_links_changed() {
    let world = World::new("relink-add");
    install(&world, "tierstore", "1.4.2", &Spec::tierstore());
    let newer = world.package("tierstore", "1.5.0", &with_extra());
    let placer = world.placer();
    world.log.clear();

    assert!(placer.relink_to(&newer).expect("relink"));
    assert_eq!(world.unit_calls(), [UnitCall::DaemonReload]);
    assert_eq!(
        world.read_link(EXTRA),
        Some(PathBuf::from(format!(
            "{PACKAGES}/tierstore/current/files/extra.conf"
        )))
    );
    let record = world.record();
    let entry = &record.paths[Path::new(EXTRA)];
    assert_eq!(entry.state, PathState::Placed);
    assert_eq!(entry.version, "1.5.0");

    world.log.clear();
    assert!(!placer.relink_to(&newer).expect("the repeat"));
    assert!(world.unit_calls().is_empty());
}

/// Catches a second reload when links were made and a unit was removed: one `relink_to` that
/// adds a file and drops a unit runs `daemon-reload` once, after the stop.
#[test]
fn relink_to_reloads_once_when_it_links_and_removes() {
    let world = World::new("relink-both");
    install(&world, "tierstore", "1.4.2", &with_main_unit());
    let newer = world.package("tierstore", "1.5.0", &with_extra());
    world.log.clear();

    assert!(world.placer().relink_to(&newer).expect("relink"));
    assert_eq!(
        world.unit_calls(),
        [
            UnitCall::Stop("tierstore.service".to_owned()),
            UnitCall::DaemonReload
        ]
    );
    assert!(world.read_link(EXTRA).is_some());
}

/// Catches errors of `relink_to` lost on the way: a tombstone refuses it, a failing service
/// manager is `Units` after the links were made, and a bad unit is a lint error before
/// anything is written.
#[test]
fn relink_to_reports_each_refusal() {
    let world = World::new("relink-refused");
    install(&world, "tierstore", "1.4.2", &Spec::tierstore());
    let newer = world.package("tierstore", "1.5.0", &with_extra());

    let failing = FakeUnitControl::failing(world.log.clone());
    let placer = Placer::new(world.roots.clone(), &world.fs, &failing);
    let refusal = placer.relink_to(&newer).expect_err("the reload fails");
    assert!(matches!(refusal, PlacedError::Units(_)));
    assert!(world.read_link(EXTRA).is_some());

    let spec = Spec::new()
        .unit("tierstore.service", &unit_without_install(EXEC))
        .run("tierstore.service");
    let bare = world.package("tierstore", "1.6.0", &spec);
    world.log.clear();
    assert!(matches!(
        world.placer().relink_to(&bare),
        Err(PlacedError::Lint(LintError::NoWantedBy { .. }))
    ));
    assert!(world.writes().is_empty());

    let mut record = world.record();
    record
        .uninstalling
        .insert("tierstore".to_owned(), Tombstone::default());
    world.placer().store().save(&record).expect("save");
    assert!(matches!(
        world.placer().relink_to(&newer),
        Err(PlacedError::Tombstoned { .. })
    ));
}

/// A host with 1.4.2 installed (with a second unit) and the manifest of 1.5.0 (with one more
/// file and without that unit).
fn relink_scenario(name: &str) -> (World, Manifest) {
    let world = World::new(name);
    install(&world, "tierstore", "1.4.2", &with_main_unit());
    let newer = world.package("tierstore", "1.5.0", &with_extra());
    (world, newer)
}

/// Catches a write of `relink_to` whose failure is swallowed: with each of its writes failing in
/// turn, `relink_to` is an error; with none failing it succeeds.
#[test]
fn relink_to_fails_when_any_of_its_writes_fails() {
    let (world, newer) = relink_scenario("relink-count");
    let before = world.fs.writes();
    assert!(world.placer().relink_to(&newer).expect("relink"));
    let total = world.fs.writes() - before;
    assert!(
        total >= 12,
        "begin, link, commit and removal write: {total}"
    );

    for n in 1..=total {
        let (world, newer) = relink_scenario(&format!("relink-fault-{n}"));
        let failing = world.failing_at(n);
        let result = world.placer_on(&failing).relink_to(&newer);
        assert!(result.is_err(), "write {n} of {total} must fail the relink");
    }

    let (world, newer) = relink_scenario("relink-past");
    let failing = world.failing_at(total + 1);
    world
        .placer_on(&failing)
        .relink_to(&newer)
        .expect("no write fails");
}
