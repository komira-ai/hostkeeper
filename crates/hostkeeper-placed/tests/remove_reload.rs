//! Removal when the unit manager or the disk changes under it, and the reload the record owes:
//! a unit that keeps one `WantedBy=` link is not stopped, a path replaced by a stop is left
//! alone, a failed reload leaves `placed.json` owing it, and `disable` with nothing to disable
//! does nothing.

mod support;

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use hostkeeper_placed::testing::UnitCall;
use hostkeeper_placed::{PlacedError, Placer, Removal, UnitControl, UnitControlError};
use support::{Spec, World, install, service};

const UNIT_DIR: &str = "/etc/systemd/system";
const CONF: &str = "/etc/tierstore/tierstore.conf";
const EXTRA_WANTS: &str = "/etc/systemd/system/extra.service.wants";

fn wants(unit: &str) -> PathBuf {
    Path::new(support::WANTS).join(unit)
}

fn unit_path(unit: &str) -> PathBuf {
    Path::new(UNIT_DIR).join(unit)
}

/// tierstore 1.5.0 installed under a fresh scratch root, with the log of the install forgotten.
fn tierstore_world(name: &str) -> World {
    let world = World::new(name);
    install(&world, "tierstore", "1.5.0", &Spec::tierstore());
    world.log.clear();
    world
}

/// A plain unit `tierstore-cache.service`, in `run`, wanted by `targets`.
fn cache_spec(targets: &str) -> Spec {
    let text = format!(
        "[Unit]\nDescription=x\n\n[Service]\nExecStart=/bin/true\n\n[Install]\nWantedBy={targets}\n"
    );
    Spec::new()
        .unit("tierstore-cache.service", &text)
        .run("tierstore-cache.service")
}

/// A unit manager whose stop replaces the file at `disk` with a regular file, as a service that
/// rewrites its own configuration while it shuts down would, and remembers the inode of it.
struct Swapper {
    disk: PathBuf,
    inode: Mutex<Option<u64>>,
}

impl Swapper {
    fn new(disk: PathBuf) -> Swapper {
        Swapper {
            disk,
            inode: Mutex::new(None),
        }
    }

    fn inode(&self) -> Option<u64> {
        *self.inode.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl UnitControl for Swapper {
    fn daemon_reload(&self) -> Result<(), UnitControlError> {
        Ok(())
    }

    fn stop_unit(&self, _unit: &str) -> Result<(), UnitControlError> {
        fs::remove_file(&self.disk).expect("remove the placed link");
        fs::write(&self.disk, b"mine\n").expect("write a file");
        let ino = fs::metadata(&self.disk).expect("metadata").ino();
        *self.inode.lock().unwrap_or_else(PoisonError::into_inner) = Some(ino);
        Ok(())
    }
}

/// A unit manager that stops units and refuses to reload.
struct ReloadFails;

impl UnitControl for ReloadFails {
    fn daemon_reload(&self) -> Result<(), UnitControlError> {
        Err(UnitControlError {
            action: "systemctl daemon-reload".to_owned(),
            why: "refused".to_owned(),
        })
    }

    fn stop_unit(&self, _unit: &str) -> Result<(), UnitControlError> {
        Ok(())
    }
}

/// Catches a unit that loses one of two `WantedBy=` targets being stopped as if it were gone:
/// only the link of the dropped target is removed, the unit file and the other link stay, nothing
/// is stopped, and `daemon-reload` runs once.
#[test]
fn a_run_unit_that_drops_one_wantedby_target_is_not_stopped() {
    let world = World::new("remove-one-target");
    install(
        &world,
        "tierstore",
        "1.4.2",
        &cache_spec(&format!("{} extra.service", support::TARGET)),
    );
    let manifest = install(&world, "tierstore", "1.5.0", &cache_spec(support::TARGET));
    let dropped = Path::new(EXTRA_WANTS).join("tierstore-cache.service");
    assert!(world.read_link(&dropped).is_some());
    world.log.clear();

    let removal = world
        .placer()
        .unlink_unlisted(&manifest)
        .expect("the removal");

    assert_eq!(
        removal,
        Removal {
            stopped: Vec::new(),
            removed: vec![dropped.clone()],
            left_alone: Vec::new(),
            reloaded: true,
        }
    );
    let reloads: Vec<UnitCall> = world.unit_calls();
    assert_eq!(reloads, [UnitCall::DaemonReload]);
    assert!(world.read_link(&dropped).is_none());
    assert_eq!(
        world.read_link(wants("tierstore-cache.service")),
        Some(unit_path("tierstore-cache.service"))
    );
    assert!(
        world
            .read_link(unit_path("tierstore-cache.service"))
            .is_some()
    );
    assert!(!world.record().paths.contains_key(&dropped));
}

/// Catches a removal that deletes a file which took the place of a placed configuration link
/// while the units were being stopped: the path is inspected again after the stops, reported in
/// `left_alone`, and its bytes and inode survive.
#[test]
fn a_config_link_replaced_during_the_stop_is_left_alone() {
    let world = tierstore_world("remove-swap-conf");
    let swapper = Swapper::new(world.host(CONF));
    let placer = Placer::new(world.roots.clone(), &world.fs, &swapper);

    let removal = placer
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
    assert_eq!(fs::read(world.host(CONF)).expect("the file"), b"mine\n");
    let after = fs::metadata(world.host(CONF)).expect("metadata").ino();
    assert_eq!(swapper.inode(), Some(after));
    assert!(world.record().paths.is_empty());
}

/// Catches a removal that deletes a file which took the place of a placed unit link while the
/// units were being stopped: the unit path is reported in `left_alone`, the other links go, and
/// the file keeps its bytes and inode.
#[test]
fn a_unit_link_replaced_during_the_stop_is_left_alone() {
    let world = tierstore_world("remove-swap-unit");
    let unit = unit_path("tierstore@.service");
    let swapper = Swapper::new(world.host(&unit));
    let placer = Placer::new(world.roots.clone(), &world.fs, &swapper);

    let removal = placer
        .remove_all(&service("tierstore"))
        .expect("the removal");

    assert_eq!(
        removal,
        Removal {
            stopped: vec!["tierstore@9301.service".to_owned()],
            removed: vec![wants("tierstore@9301.service"), PathBuf::from(CONF)],
            left_alone: vec![unit.clone()],
            reloaded: true,
        }
    );
    assert_eq!(fs::read(world.host(&unit)).expect("the file"), b"mine\n");
    let after = fs::metadata(world.host(&unit)).expect("metadata").ino();
    assert_eq!(swapper.inode(), Some(after));
    assert!(world.read_link(CONF).is_none());
    assert!(world.record().paths.is_empty());
}

/// Catches a failed reload after a removal that forgets it is owed: the removal is saved, the
/// error is `PlacedError::Units`, `placed.json` still owes the reload, and a later
/// `Placer::reload` with a working manager runs it and clears the debt.
#[test]
fn a_failed_reload_in_remove_all_leaves_the_reload_owed_until_a_reload_succeeds() {
    let world = tierstore_world("remove-reload-owed");
    world.placer().reload().expect("reload");
    assert!(!world.record().reload_pending);
    let refusing = ReloadFails;
    let placer = Placer::new(world.roots.clone(), &world.fs, &refusing);

    let err = placer
        .remove_all(&service("tierstore"))
        .expect_err("the reload fails");

    assert!(matches!(err, PlacedError::Units(_)));
    let record = world.record();
    assert!(record.paths.is_empty());
    assert!(record.reload_pending);

    world.log.clear();
    world.placer().reload().expect("reload");

    assert_eq!(world.unit_calls(), [UnitCall::DaemonReload]);
    assert!(!world.record().reload_pending);
    assert!(world.record().paths.is_empty());
}

/// Catches a `disable` that reloads or writes when the service has no `WantedBy=` link: a
/// service that places only a file gets an empty result, no write and no unit call.
#[test]
fn disable_with_no_wants_links_writes_nothing_and_does_not_reload() {
    let world = World::new("disable-none");
    install(
        &world,
        "filesvc",
        "1.0.0",
        &Spec::new().file("files/filesvc.conf", "/etc/filesvc/filesvc.conf"),
    );
    let before = world.record();
    world.log.clear();

    let removal = world
        .placer()
        .disable(&service("filesvc"))
        .expect("disable");

    assert_eq!(removal, Removal::default());
    assert!(world.log.events().is_empty());
    assert_eq!(world.record(), before);
}
