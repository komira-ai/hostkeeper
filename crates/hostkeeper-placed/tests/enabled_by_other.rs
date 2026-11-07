//! A `WantedBy=` link that something other than the daemon wrote: the `systemctl enable` of a
//! distribution package that installed the same unit (the deb of hostkeeper enables
//! `hostkeeper.service` this way). A link to a unit file of the same name that `placed.json` does
//! not list is left as it is, never claimed, never a `path_conflict` and never removed; anything
//! else at the path is still refused.

mod support;

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use hostkeeper_placed::{BootView, Entry, Owner, PathKind, PathState, PlacedError};
use support::{Spec, World, service, version};

const UNIT_DIR: &str = "/etc/systemd/system";
/// The run instance of [`Spec::tierstore`], and the template it is an instance of.
const INSTANCE: &str = "tierstore@9301.service";
const TEMPLATE: &str = "tierstore@.service";
/// Where a distribution package puts its unit files.
const VENDOR_UNIT: &str = "/usr/lib/systemd/system/tierstore@.service";

fn wants() -> PathBuf {
    Path::new(support::WANTS).join(INSTANCE)
}

/// A world with tierstore 1.5.0 staged, nothing of it placed, and `target` linked at the
/// `WantedBy=` path of its run instance, as `systemctl enable` leaves it.
fn enabled_with(name: &str, target: &str) -> World {
    let world = World::new(name);
    world.package("tierstore", "1.5.0", &Spec::tierstore());
    let link = world.host(wants());
    fs::create_dir_all(link.parent().expect("the wants directory")).expect("mkdir");
    symlink(target, link).expect("the enable link");
    world
}

/// Places tierstore 1.5.0 as a first install's switch does: `current`, then every path it
/// places, committed.
fn place(world: &World) -> Result<bool, PlacedError> {
    let manifest = world.load("tierstore", "1.5.0");
    let placer = world.placer();
    placer.switch_current(&service("tierstore"), &version("1.5.0"))?;
    placer.relink_to(&manifest)
}

/// Catches an install refused `path_conflict` over the link a distribution package's enable
/// wrote for the same unit: the link is not among the claims, the check passes, placing the
/// release links the unit file and leaves that link pointing where it did, and `placed.json`
/// never lists it, so removing every path of the service leaves it too.
#[test]
fn a_link_a_distribution_package_enabled_for_the_same_unit_is_left_and_not_claimed() {
    let world = enabled_with("enabled-vendor", VENDOR_UNIT);
    let manifest = world.load("tierstore", "1.5.0");
    let placer = world.placer();
    let claims = placer.desired(&manifest).expect("the claims");
    assert!(
        claims.iter().all(|claim| claim.path != wants()),
        "{:?}",
        claims.paths()
    );
    placer.check(&claims).expect("no conflict");

    place(&world).expect("placed");
    assert_eq!(world.read_link(wants()), Some(PathBuf::from(VENDOR_UNIT)));
    assert_eq!(
        world.read_link(Path::new(UNIT_DIR).join(TEMPLATE)),
        Some(PathBuf::from(
            "/var/lib/hostkeeper/packages/tierstore/current/units/tierstore@.service"
        ))
    );
    assert!(!world.record().paths.contains_key(&wants()));

    let removal = world
        .placer()
        .remove_all(&service("tierstore"))
        .expect("the removal");
    assert!(!removal.removed.contains(&wants()), "{removal:?}");
    assert_eq!(world.read_link(wants()), Some(PathBuf::from(VENDOR_UNIT)));
}

/// Catches a link compared by more than the unit's name: a relative link (the boot image's
/// overlay enables the unit with one) to a unit file of the same name is left the same way.
#[test]
fn a_relative_link_to_a_unit_file_of_the_same_name_is_left_too() {
    let world = enabled_with("enabled-relative", "../tierstore@.service");
    place(&world).expect("placed");
    assert_eq!(
        world.read_link(wants()),
        Some(PathBuf::from("../tierstore@.service"))
    );
    assert!(!world.record().paths.contains_key(&wants()));
}

/// Catches the rule taken for "any link": a link at the `WantedBy=` path to a unit file of
/// another name is still claimed and refused `path_conflict`, naming the link, and nothing is
/// placed.
#[test]
fn a_link_to_a_unit_file_of_another_name_is_still_a_conflict() {
    let world = enabled_with("enabled-other", "/usr/lib/systemd/system/other.service");
    let err = place(&world).expect_err("refused");
    assert!(
        matches!(&err, PlacedError::PathConflict { path, owner: Owner::Link(target) }
            if *path == wants() && target == Path::new("/usr/lib/systemd/system/other.service")),
        "{err:?}"
    );
    assert_eq!(
        world.read_link(wants()),
        Some(PathBuf::from("/usr/lib/systemd/system/other.service"))
    );
}

/// Catches the rule reaching past links: a regular file at the `WantedBy=` path is refused
/// `path_conflict` and keeps its bytes.
#[test]
fn a_file_at_the_wants_path_is_still_a_conflict() {
    let world = World::new("enabled-file");
    world.package("tierstore", "1.5.0", &Spec::tierstore());
    let link = world.host(wants());
    fs::create_dir_all(link.parent().expect("the wants directory")).expect("mkdir");
    fs::write(&link, b"not a link\n").expect("a file");
    let manifest = world.load("tierstore", "1.5.0");
    let placer = world.placer();
    let err = placer
        .desired(&manifest)
        .and_then(|claims| placer.check(&claims))
        .expect_err("refused");
    assert!(
        matches!(&err, PlacedError::PathConflict { path, owner: Owner::File } if *path == wants()),
        "{err:?}"
    );
    assert_eq!(fs::read(link).expect("the file"), b"not a link\n");
}

/// Catches the rule taking another service's link: a link of the same unit name that
/// `placed.json` lists for another service is claimed and refused, naming that service.
#[test]
fn a_link_the_record_lists_for_another_service_is_still_a_conflict() {
    let world = enabled_with("enabled-listed", VENDOR_UNIT);
    let mut record = world.record();
    record.paths.insert(
        wants(),
        Entry::new(
            "ghost",
            "1.0.0",
            PathKind::WantsLink,
            PathState::Placed,
            Path::new(VENDOR_UNIT),
        ),
    );
    let placer = world.placer();
    let lock = placer.lock().expect("the lock");
    placer.store().save(&record, &lock).expect("save");
    drop(lock);
    let manifest = world.load("tierstore", "1.5.0");
    let err = placer
        .desired(&manifest)
        .and_then(|claims| placer.check(&claims))
        .expect_err("refused");
    assert!(
        matches!(&err, PlacedError::PathConflict { owner: Owner::Service(owner), .. } if owner == "ghost"),
        "{err:?}"
    );
}

/// Catches the start pass judging the link apart from the switch: reconciling a host whose
/// service runs with the distribution package's link in place leaves the link and reports no
/// conflict.
#[test]
fn the_start_pass_leaves_the_link_and_reports_no_conflict() {
    let world = enabled_with("enabled-reconcile", VENDOR_UNIT);
    place(&world).expect("placed");
    let report = world
        .placer()
        .reconcile(&BootView {
            intents: vec![],
            running: support::RUNNING,
        })
        .expect("reconcile");
    assert!(report.conflicts.is_empty(), "{report:?}");
    assert_eq!(world.read_link(wants()), Some(PathBuf::from(VENDOR_UNIT)));
}
