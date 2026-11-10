//! The prune after a confirmed switch (design sections 4.7, 5.6 step 18 and 8.7): an update that
//! ends `Succeeded` and a force that moved `current` prune the service's package directory to
//! `current` and `previous`, under the service's lock alone, keeping what the records still use;
//! nothing else prunes.

mod support;
mod support_apt;
mod support_host;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::thread;

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_daemon::locks::{Package, ServiceGuard};
use hostkeeper_daemon::prune::{self, ForceLockPoint, LayoutPrune, Prune};
use hostkeeper_daemon::testing::{RecordingPrune, ScriptedLauncher, manifest, spawn};
use hostkeeper_daemon::verb::{HookedVerb, PackageDigest};
use hostkeeper_daemon::{Daemon, Placement, Ports, Reply};
use hostkeeper_hook::{Class, Phase, Step, Verdict};
use hostkeeper_placed::testing::FaultFs;
use hostkeeper_steps::record::{RECORD_FILE, Record, State};
use hostkeeper_steps::{
    ForcePhase, ForceRecord, ForceVerdict, Layout, PRUNE_PREFIX, Protected, PruneReport, PruneStep,
    RecordStore, Stamp, tree_digest,
};
use support::force::{WEB, failed, link_of, succeeded};
use support::{OWN, PENDING_LINE, Rig, alerting, config, lines, name, roots, snapshot};
use support_host::NativeHost;

/// A version, from a literal.
fn version(text: &str) -> Version {
    Version::new(text).expect("a version")
}

/// What a service's lock is held for: an order of `verb` for `version`.
fn package(verb: HookedVerb, version_text: &str) -> Package {
    Package {
        verb,
        version: version(version_text),
        digest: PackageDigest::new(&support::d()).expect("a digest"),
    }
}

/// A daemon on the host, linking through `fs` and pruning with `prune`.
fn daemon_on(farm: &NativeHost, fs: Arc<FaultFs>, prune: Arc<dyn Prune>) -> Daemon {
    let ports = Ports {
        placement: Placement { roots: roots(), fs },
        prune,
        ..farm.ports()
    };
    Daemon::new(config(), farm.rig().dirs(), ports, OWN)
}

/// A daemon on the host whose prune is the host's, behind a recorder.
fn pruning(farm: &NativeHost) -> (Daemon, Arc<RecordingPrune>) {
    let pruner = RecordingPrune::new();
    let daemon = daemon_on(farm, farm.rig().placed_fs.clone(), pruner.clone());
    (daemon, pruner)
}

/// `version` of `web`, with the one unit it runs, published to order next.
fn web(farm: &NativeHost, version_text: &str) {
    farm.publish(WEB, version_text);
}

/// `daemon` forces `web` to `version_text` with `order_try`, and that runs to `Succeeded`; then
/// the log of placed writes and unit calls is cleared.
fn forced(farm: &NativeHost, daemon: &Daemon, version_text: &str, order_try: u64) {
    assert_eq!(
        daemon.order(&farm.force(WEB, version_text, order_try, 0)),
        succeeded(),
        "the force of {version_text}"
    );
    farm.rig().placed_log.clear();
}

/// Everything in `service`'s package directory, by name.
fn on_disk(rig: &Rig, service: &str) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(rig.packages.join(service))
        .expect("the package directory")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

/// The version directories of `web`, by name.
fn versions(rig: &Rig) -> Vec<String> {
    on_disk(rig, WEB)
        .into_iter()
        .filter(|name| !name.starts_with('.'))
        .filter(|name| {
            fs::symlink_metadata(rig.packages.join(WEB).join(name)).is_ok_and(|meta| meta.is_dir())
        })
        .collect()
}

/// What a row of the design's table reads: `current`, `previous`, and the versions on disk.
fn row(rig: &Rig) -> (String, String, Vec<String>) {
    (
        link_of(rig, WEB, "current").unwrap_or_default(),
        link_of(rig, WEB, "previous").unwrap_or_default(),
        versions(rig),
    )
}

fn strings(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

/// `web` forced to 1.0.0, then to 2.0.0, each fetched just before: `current` 2.0.0, `previous`
/// 1.0.0.
fn two_forced(farm: &NativeHost, daemon: &Daemon) {
    web(farm, "1.0.0");
    forced(farm, daemon, "1.0.0", 1);
    web(farm, "2.0.0");
    forced(farm, daemon, "2.0.0", 2);
}

/// A version of `web` staged and never switched to, sealed.
fn stale(rig: &Rig, version_text: &str) {
    rig.add_with(WEB, version_text, &manifest(WEB, version_text));
}

/// The leftovers of a prune that stopped, in `web`'s package directory.
fn leftovers(rig: &Rig) -> usize {
    on_disk(rig, WEB)
        .iter()
        .filter(|name| name.starts_with(PRUNE_PREFIX))
        .count()
}

/// Whether `reply` is an order that ran to `Deferred`.
fn is_deferred(reply: &Reply) -> bool {
    matches!(
        reply,
        Reply::Finished {
            verdict: Verdict::Deferred { .. },
            resent: false,
            ..
        }
    )
}

/// The bytes of each of `files`.
fn bytes_of(files: &[PathBuf]) -> Vec<Vec<u8>> {
    files
        .iter()
        .map(|path| fs::read(path).expect("a file"))
        .collect()
}

/// Catches a confirmed switch that leaves a third version, or deletes one of the two: three
/// versions switched to in turn, each switch followed by the confirmed-switch call under the
/// service's lock, leave exactly the second and third with their seals and the two links.
#[test]
fn a_confirmed_switch_leaves_exactly_current_and_previous() {
    let rig = Rig::new("prune-confirmed");
    let ports = Ports {
        prune: Arc::new(LayoutPrune),
        ..rig.ports()
    };
    let daemon = Daemon::new(config(), rig.dirs(), ports, OWN);
    let ctx = daemon.ctx();
    let service = name(WEB);
    let layout = Layout::package(&rig.packages.join(WEB));
    for (n, text) in ["1.0.0", "2.0.0", "3.0.0"].into_iter().enumerate() {
        stale(&rig, text);
        layout.switch(text).expect("the switch");
        let guard = ctx
            .locks
            .take(&service, package(HookedVerb::Update, text))
            .expect("the service's lock");
        let report = prune::confirmed_switch(ctx, &guard, &service, &version(text));
        let report = report.expect("a report");
        assert_eq!(report.deleted.len(), usize::from(n == 2), "{text}");
    }
    assert_eq!(
        on_disk(&rig, WEB),
        [
            "2.0.0",
            "2.0.0.seal",
            "3.0.0",
            "3.0.0.seal",
            "current",
            "previous"
        ]
    );
    assert_eq!(row(&rig).0, "3.0.0");
    assert_eq!(row(&rig).1, "2.0.0");
}

/// Catches any row of the design's table going wrong: forces to A, B and C give (C, B, B and C
/// on disk); a force back to B, which is on the host, gives (B, C, both kept) and asks the
/// repository for nothing; a force to A, fetched again, gives (A, B), C and its seal deleted.
#[test]
fn the_design_table_rows_hold_in_sequence() {
    let farm = NativeHost::new("prune-table");
    let rig = farm.rig();
    let (daemon, _) = pruning(&farm);
    two_forced(&farm, &daemon);
    web(&farm, "3.0.0");
    forced(&farm, &daemon, "3.0.0", 3);
    assert_eq!(
        row(rig),
        ("3.0.0".into(), "2.0.0".into(), strings(&["2.0.0", "3.0.0"]))
    );
    let seen = farm.apt.host.calls().len();
    forced(&farm, &daemon, "2.0.0", 4);
    assert_eq!(
        row(rig),
        ("2.0.0".into(), "3.0.0".into(), strings(&["2.0.0", "3.0.0"]))
    );
    let tools = support_apt::seen::labels_since(&farm.apt, seen);
    assert!(
        !tools
            .iter()
            .any(|label| label == "apt-get update" || label == "apt-get download"),
        "nothing was fetched: {tools:?}"
    );
    // 1.0.0 was deleted by the prune: the force stages it from the repository again.
    let seen = farm.apt.host.calls().len();
    forced(&farm, &daemon, "1.0.0", 5);
    let tools = support_apt::seen::labels_since(&farm.apt, seen);
    assert!(tools.contains(&"apt-get download".to_owned()), "{tools:?}");
    assert_eq!(
        row(rig),
        ("1.0.0".into(), "2.0.0".into(), strings(&["1.0.0", "2.0.0"]))
    );
    assert!(!rig.packages.join(WEB).join("3.0.0.seal").exists());
}

/// Catches a prune that keeps an old `previous`, or deletes the version rolled back from: after
/// C and a rollback to B, `previous` names C and C stays; a further switch to D leaves D and B,
/// `previous` being B, the version left, and C is deleted.
#[test]
fn a_switch_after_a_rollback_keeps_the_version_rolled_back_from() {
    let farm = NativeHost::new("prune-after-rollback");
    let rig = farm.rig();
    let (daemon, _) = pruning(&farm);
    two_forced(&farm, &daemon);
    web(&farm, "3.0.0");
    forced(&farm, &daemon, "3.0.0", 3);
    forced(&farm, &daemon, "2.0.0", 4);
    assert_eq!(
        row(rig),
        ("2.0.0".into(), "3.0.0".into(), strings(&["2.0.0", "3.0.0"]))
    );
    web(&farm, "4.0.0");
    forced(&farm, &daemon, "4.0.0", 5);
    assert_eq!(
        row(rig),
        ("4.0.0".into(), "2.0.0".into(), strings(&["2.0.0", "4.0.0"]))
    );
}

/// A force lock point that, between a force's switch and its prune, tries a second prune of
/// `web` from another thread, and notes what it found and where `current` was.
#[derive(Default)]
struct Racer {
    daemon: OnceLock<Weak<Daemon>>,
    packages: PathBuf,
    seen: Mutex<Vec<(&'static str, String)>>,
}

impl ForceLockPoint for Racer {
    fn after_switch_before_prune(&self) {
        let daemon = self
            .daemon
            .get()
            .and_then(Weak::upgrade)
            .expect("the daemon");
        let tried = thread::spawn(move || second_prune(&daemon))
            .join()
            .expect("the second prune");
        let current = fs::read_link(self.packages.join(WEB).join("current"))
            .expect("current")
            .to_string_lossy()
            .into_owned();
        self.seen.lock().expect("the notes").push((tried, current));
    }
}

/// A prune of `web` by another order: it needs the service's lock, which it never waits for.
fn second_prune(daemon: &Daemon) -> &'static str {
    let ctx = daemon.ctx();
    let service = name(WEB);
    match ctx
        .locks
        .take(&service, package(HookedVerb::Update, "9.0.0"))
    {
        Ok(guard) => {
            prune::confirmed_switch(ctx, &guard, &service, &version("9.0.0"));
            "pruned"
        }
        Err(held) => {
            assert!(held.is_force(), "{held:?}");
            "locked"
        }
    }
}

/// A daemon whose force lock point is a [`Racer`].
fn racing(farm: &NativeHost) -> (Arc<Daemon>, Arc<Racer>) {
    let racer = Arc::new(Racer {
        packages: farm.rig().packages.clone(),
        ..Racer::default()
    });
    let ports = Ports {
        prune: Arc::new(LayoutPrune),
        force_lock_point: racer.clone(),
        ..farm.ports()
    };
    let daemon = Arc::new(Daemon::new(config(), farm.rig().dirs(), ports, OWN));
    racer.daemon.set(Arc::downgrade(&daemon)).expect("set once");
    (daemon, racer)
}

/// Catches a prune that takes no lock, the wrong lock, or a keep set made before the switch: a
/// second prune tried between each force's switch and its prune cannot have the service's lock,
/// which the force holds; `current` already names the target there; and the force's own prune
/// keeps the target and deletes the version two back. Once the force is over, the second prune
/// has the lock. A force to a version an update staged and never switched to keeps that tree.
#[test]
fn a_prune_racing_a_force_never_removes_the_target() {
    let farm = NativeHost::new("prune-race");
    let rig = farm.rig();
    let (daemon, racer) = racing(&farm);
    two_forced(&farm, &daemon);
    web(&farm, "3.0.0");
    forced(&farm, &daemon, "3.0.0", 3);
    let seen = racer.seen.lock().expect("the notes").clone();
    assert_eq!(
        seen,
        [
            ("locked", "1.0.0".to_owned()),
            ("locked", "2.0.0".to_owned()),
            ("locked", "3.0.0".to_owned()),
        ]
    );
    assert_eq!(versions(rig), ["2.0.0", "3.0.0"]);
    assert_eq!(second_prune(&daemon), "pruned");

    let farm = NativeHost::new("prune-race-staged");
    let rig = farm.rig();
    let (daemon, racer) = racing(&farm);
    two_forced(&farm, &daemon);
    web(&farm, "3.0.0");
    rig.launcher.exits(WEB, Step::Prepare, &[75]);
    let reply = daemon.order(&farm.update(WEB, "3.0.0", 3));
    assert!(is_deferred(&reply), "{reply:?}");
    assert_eq!(versions(rig), ["1.0.0", "2.0.0", "3.0.0"]);
    forced(&farm, &daemon, "3.0.0", 4);
    assert_eq!(versions(rig), ["2.0.0", "3.0.0"]);
    assert_eq!(link_of(rig, WEB, "current").as_deref(), Some("3.0.0"));
    let last = racer.seen.lock().expect("the notes").last().cloned();
    assert_eq!(last, Some(("locked", "3.0.0".to_owned())));
}

/// `web` at 1.0.0 to 4.0.0, sealed, `current` 3.0.0 and `previous` 2.0.0, and the record of a
/// finished update to 3.0.0 that replaced 2.0.0; returns that record. 1.0.0 to 3.0.0 are
/// installed in turn by orders of a daemon that prunes nothing, and 4.0.0 is a version staged and
/// never switched to.
fn recorded(farm: &NativeHost) -> Record {
    let rig = farm.rig();
    for text in ["1.0.0", "2.0.0", "3.0.0"] {
        web(farm, text);
    }
    let daemon = farm.daemon();
    assert_eq!(daemon.order(&farm.install(WEB, "1.0.0", 1)), succeeded());
    assert_eq!(daemon.order(&farm.update(WEB, "2.0.0", 2)), succeeded());
    assert_eq!(daemon.order(&farm.update(WEB, "3.0.0", 3)), succeeded());
    stale(rig, "4.0.0");
    rig.record(WEB).expect("the record")
}

fn save(rig: &Rig, service: &str, rec: &Record) {
    RecordStore::new(&rig.state.join(service))
        .save(rec)
        .expect("save the record");
}

/// The confirmed switch of `web` to 3.0.0 on `daemon`, under its lock.
fn confirm(daemon: &Daemon) -> Option<PruneReport> {
    let ctx = daemon.ctx();
    let service = name(WEB);
    let guard = ctx
        .locks
        .take(&service, package(HookedVerb::Update, "3.0.0"))
        .expect("the service's lock");
    prune::confirmed_switch(ctx, &guard, &service, &version("3.0.0"))
}

/// Catches a prune that deletes the release an unfinished update may still return to, or keeps
/// it for good: a running record whose `previous_digest` is the tree of 4.0.0 (neither `current`
/// nor `previous`) keeps 4.0.0; the same record closed lets it go, which shows the record, not a
/// blanket keep, protected it; and a running record with no previous release keeps nothing
/// extra. A seal that does not read names no release.
#[test]
fn prune_never_removes_the_previous_release_of_an_unfinished_record() {
    let farm = NativeHost::new("prune-unfinished");
    let rig = farm.rig();
    let mut rec = recorded(&farm);
    let daemon = daemon_on(&farm, rig.placed_fs.clone(), Arc::new(LayoutPrune));
    let digest = tree_digest(&rig.packages.join(WEB).join("4.0.0")).expect("a digest");
    fs::write(rig.packages.join(WEB).join("5.0.0.seal"), "not a seal").expect("a bad seal");
    rec.previous_digest = Some(digest);
    rec.state = State::Running {
        started: Stamp::now(&*rig.clock),
        deadline_secs: 100,
        unbounded: false,
    };
    save(rig, WEB, &rec);
    let report = confirm(&daemon).expect("a report");
    assert_eq!(report.deleted, ["1.0.0", "5.0.0"].map(String::from).into());
    assert_eq!(versions(rig), ["2.0.0", "3.0.0", "4.0.0"]);

    rec.state = State::Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
    };
    save(rig, WEB, &rec);
    confirm(&daemon).expect("a report");
    assert_eq!(versions(rig), ["2.0.0", "3.0.0"]);

    stale(rig, "4.0.0");
    rec.previous_digest = None;
    rec.state = State::Pending;
    save(rig, WEB, &rec);
    confirm(&daemon).expect("a report");
    assert_eq!(versions(rig), ["2.0.0", "3.0.0"]);
}

/// Catches a prune that looks at `current` and `previous` only: a force record with no verdict
/// whose target is 4.0.0 keeps it; with the force's verdict written, 4.0.0 goes.
#[test]
fn prune_never_removes_the_target_of_a_switch_under_way() {
    let farm = NativeHost::new("prune-under-way");
    let rig = farm.rig();
    let mut rec = recorded(&farm);
    let daemon = daemon_on(&farm, rig.placed_fs.clone(), Arc::new(LayoutPrune));
    let mut journal = ForceRecord {
        target_version: "4.0.0".to_owned(),
        target_digest: support::d(),
        from_version: Some("3.0.0".to_owned()),
        phase: ForcePhase::UnitsStopped,
        order_try: 2,
        halt_generation: 0,
        order_id: None,
        verdict: None,
    };
    rec.force = Some(journal.clone());
    save(rig, WEB, &rec);
    confirm(&daemon).expect("a report");
    assert_eq!(versions(rig), ["2.0.0", "3.0.0", "4.0.0"]);
    journal.verdict = Some(ForceVerdict::Succeeded);
    rec.force = Some(journal);
    save(rig, WEB, &rec);
    confirm(&daemon).expect("a report");
    assert_eq!(versions(rig), ["2.0.0", "3.0.0"]);
}

/// Catches a staged tree deleted when the update that staged it fails to switch, or kept for
/// good: an update that defers after staging 3.0.0 leaves it whole and sealed, a retry of the
/// update finds it so (the package store answers from the host for a sealed version) and defers
/// again, neither prunes, and the next confirmed switch, a force to 4.0.0, deletes it.
#[test]
fn a_staged_but_unswitched_tree_survives_until_the_next_confirmed_switch() {
    let farm = NativeHost::new("prune-staged");
    let rig = farm.rig();
    let (daemon, pruner) = pruning(&farm);
    two_forced(&farm, &daemon);
    web(&farm, "3.0.0");
    let layout = Layout::package(&rig.packages.join(WEB));
    rig.launcher.exits(WEB, Step::Prepare, &[75, 75]);
    let reply = daemon.order(&farm.update(WEB, "3.0.0", 3));
    assert!(is_deferred(&reply), "{reply:?}");
    layout.check_tree("3.0.0").expect("3.0.0 whole and sealed");
    let reply = daemon.order(&farm.update(WEB, "3.0.0", 4));
    assert!(is_deferred(&reply), "{reply:?}");
    layout.check_tree("3.0.0").expect("3.0.0 whole and sealed");
    assert_eq!(pruner.calls(), [WEB, WEB]);
    web(&farm, "4.0.0");
    forced(&farm, &daemon, "4.0.0", 5);
    assert_eq!(versions(rig), ["2.0.0", "4.0.0"]);
    assert_eq!(pruner.calls(), [WEB, WEB, WEB]);
}

/// Catches a prune wired into the generic end of an order: updates that end `Failed`,
/// `Deferred` and halted prune nothing and leave the package directory as it was, and an update
/// closed `forced_over` by a force prunes nothing itself (the force then prunes once).
#[test]
fn a_failed_or_deferred_update_prunes_nothing() {
    let farm = NativeHost::new("prune-not-confirmed");
    let rig = farm.rig();
    let (daemon, pruner) = pruning(&farm);
    two_forced(&farm, &daemon);
    stale(rig, "0.9.0");
    web(&farm, "3.0.0");
    let before = snapshot(&rig.packages);
    // The update stages 3.0.0 and nothing else changes: every other tree and seal is as it was.
    let staged = rig.packages.join(WEB);
    let without_3 = |snapshot: Vec<(PathBuf, String)>| -> Vec<(PathBuf, String)> {
        snapshot
            .into_iter()
            .filter(|(path, _)| {
                path.strip_prefix(&staged).map_or(true, |rel| {
                    !(rel.starts_with("3.0.0") || rel == Path::new("3.0.0.seal"))
                })
            })
            .collect()
    };
    // A deferred update is run again by the same order; a failed one is final for its package.
    let ends = [(75, 3), (1, 4), (76, 5)];
    for (code, order_try) in ends {
        rig.launcher.exits(WEB, Step::Prepare, &[code]);
        let reply = daemon.order(&farm.update(WEB, "3.0.0", order_try));
        let verdict = match reply {
            Reply::Finished { verdict, .. } => verdict,
            other => panic!("{other:?}"),
        };
        let ended = matches!(
            (code, &verdict),
            (1, Verdict::Failed { .. }) | (75, Verdict::Deferred { .. }) | (76, Verdict::Halt)
        );
        assert!(ended, "{code}: {verdict:?}");
        assert_eq!(pruner.calls(), [WEB, WEB], "{code}");
        assert_eq!(without_3(snapshot(&rig.packages)), before, "{code}");
        assert_eq!(
            versions(rig),
            ["0.9.0", "1.0.0", "2.0.0", "3.0.0"],
            "{code}"
        );
    }

    let farm = NativeHost::new("prune-forced-over");
    let rig = farm.rig();
    for version_text in ["0.9.0", "1.0.0", "2.0.0"] {
        web(&farm, version_text);
    }
    let (daemon, pruner) = pruning(&farm);
    fs::create_dir_all(&rig.state).expect("the state dir");
    fs::set_permissions(&rig.state, fs::Permissions::from_mode(0o700)).expect("its mode");
    daemon.boot_pass().expect("the start pass");
    // `web` runs 0.9.0; the prune of the install that put it there is not the update's.
    assert_eq!(daemon.order(&farm.install(WEB, "0.9.0", 1)), succeeded());
    let installed = pruner.calls();
    let daemon = Arc::new(daemon);
    let gate = rig.launcher.hold(WEB, Step::Cutover);
    let stops = rig.launcher.stops_seen();
    let update = farm.update(WEB, "1.0.0", 2);
    let updating = {
        let daemon = Arc::clone(&daemon);
        spawn("the update", move || daemon.order(&update))
    };
    gate.entered(&updating).expect("the update's cutover runs");
    let waiting = rig.clock.park_next_sleep();
    let force = farm.force(WEB, "2.0.0", 3, 0);
    let forcing = {
        let daemon = Arc::clone(&daemon);
        spawn("the force", move || daemon.order(&force))
    };
    assert_eq!(
        stops
            .wait(&forcing)
            .expect("the force's stop of the cutover"),
        "updater-web-cutover-2"
    );
    waiting
        .entered(&forcing)
        .expect("the force waits for the update");
    gate.release();
    assert_eq!(
        updating.join().expect("the update"),
        failed(Phase::BeforeCommit, Class::Environment, "forced_over")
    );
    assert_eq!(pruner.calls(), installed);
    waiting.release();
    assert_eq!(forcing.join().expect("the force"), succeeded());
    assert_eq!(pruner.calls(), [installed, vec![WEB.to_owned()]].concat());
}

/// A prune that notes how many times `verify` of `web` had run when it was called, then prunes
/// as the host does.
struct Probe {
    launcher: Arc<ScriptedLauncher>,
    verifies: Mutex<Vec<usize>>,
}

impl Prune for Probe {
    fn prune(
        &self,
        layout: &Layout,
        protect: &Protected,
        held: &ServiceGuard<'_>,
    ) -> std::io::Result<PruneReport> {
        let verifies = self.launcher.steps(WEB);
        let verifies = verifies.iter().filter(|step| **step == Step::Verify);
        self.verifies
            .lock()
            .expect("the notes")
            .push(verifies.count());
        LayoutPrune.prune(layout, protect, held)
    }
}

/// Catches a prune that waits for the promotion: an update whose first `verify` says
/// `promotion_pending` is pruned right after that `verify`, before the watcher's, once; the
/// watcher then runs to `Succeeded`, and the update's target is kept with the release it
/// replaced, now `previous` (design section 4.7: 1.0.0 goes when 3.0.0 is confirmed).
#[test]
fn a_promotion_pending_success_counts_as_confirmed() {
    let farm = NativeHost::new("prune-promotion");
    let rig = farm.rig();
    let probe = Arc::new(Probe {
        launcher: rig.launcher.clone(),
        verifies: Mutex::default(),
    });
    let daemon = daemon_on(&farm, rig.placed_fs.clone(), Arc::new(LayoutPrune));
    two_forced(&farm, &daemon);
    stale(rig, "0.9.0");
    web(&farm, "3.0.0");
    let daemon = daemon_on(&farm, rig.placed_fs.clone(), probe.clone());
    rig.launcher.replies(
        WEB,
        Step::Verify,
        &[(0, PENDING_LINE), (0, PENDING_LINE), (0, "")],
    );
    assert_eq!(daemon.order(&farm.update(WEB, "3.0.0", 3)), succeeded());
    assert_eq!(*probe.verifies.lock().expect("the notes"), [1]);
    let ran = rig.launcher.steps(WEB);
    assert_eq!(ran.iter().filter(|step| **step == Step::Verify).count(), 3);
    assert_eq!(versions(rig), ["2.0.0", "3.0.0"]);
}

/// Catches a prune used to make room: a package that cannot be staged, by an update or by a
/// force (the repository cannot be reached), leaves every tree and seal as it was and prunes
/// nothing.
#[test]
fn a_version_that_cannot_be_staged_prunes_nothing() {
    let farm = NativeHost::new("prune-unstageable");
    let rig = farm.rig();
    let (daemon, pruner) = pruning(&farm);
    two_forced(&farm, &daemon);
    stale(rig, "0.9.0");
    let before = snapshot(&rig.packages);
    web(&farm, "3.0.0");
    farm.apt
        .host
        .set_health(support_apt::URI, support_apt::Health::Unreachable);
    let fetch_failed = |reply: &Reply| {
        matches!(
            reply,
            Reply::Finished {
                verdict: Verdict::Deferred { .. },
                reason: Some(reason),
                ..
            } if reason == "fetch_failed"
        )
    };
    let reply = daemon.order(&farm.update(WEB, "3.0.0", 3));
    assert!(fetch_failed(&reply), "{reply:?}");
    let reply = daemon.order(&farm.force(WEB, "3.0.0", 4, 0));
    assert!(fetch_failed(&reply), "{reply:?}");
    assert_eq!(snapshot(&rig.packages), before);
    assert_eq!(pruner.calls(), [WEB, WEB]);
}

/// Catches a prune that takes the step lock: with the host's step lock held by another
/// service's running `prepare`, a force of `web` that moves `current` still runs its prune to
/// the end; the other service's update then finishes.
#[test]
fn prune_does_not_take_the_step_lock() {
    let farm = NativeHost::new("prune-step-lock");
    let rig = farm.rig();
    let (daemon, pruner) = pruning(&farm);
    let daemon = Arc::new(daemon);
    two_forced(&farm, &daemon);
    stale(rig, "0.9.0");
    for version_text in ["0.9.0", "1.0.0"] {
        farm.publish("db", version_text);
    }
    assert_eq!(daemon.order(&farm.install("db", "0.9.0", 1)), succeeded());
    let gate = rig.launcher.hold("db", Step::Prepare);
    let update = farm.update("db", "1.0.0", 2);
    let updating = {
        let daemon = Arc::clone(&daemon);
        spawn("the update", move || daemon.order(&update))
    };
    gate.entered(&updating).expect("db's prepare runs");
    web(&farm, "3.0.0");
    forced(&farm, &daemon, "3.0.0", 3);
    assert_eq!(versions(rig), ["2.0.0", "3.0.0"]);
    let pruned_web = pruner.calls().iter().filter(|call| *call == WEB).count();
    assert_eq!(pruned_web, 3);
    gate.release();
    assert_eq!(updating.join().expect("the update"), succeeded());
}

/// Catches a removal error that fails the update, goes unreported, or is never tried again: the
/// removals of the trees of 0.9.0 and of 1.0.0 (no longer `previous` once the update switched
/// `current` to 3.0.0) are refused, the update is still `Succeeded`, the alert names both
/// versions, and the next confirmed switch removes what was left.
#[test]
fn a_failed_removal_is_reported_and_does_not_fail_the_update() {
    let farm = NativeHost::new("prune-removal-fails");
    let rig = farm.rig();
    let (daemon, pruner) = pruning(&farm);
    two_forced(&farm, &daemon);
    stale(rig, "0.9.0");
    web(&farm, "3.0.0");
    let (daemon, alerts) = alerting(daemon);
    pruner.fault_next(|step| matches!(step, PruneStep::Removing { .. }));
    assert_eq!(daemon.order(&farm.update(WEB, "3.0.0", 3)), succeeded());
    assert_eq!(
        lines(&alerts),
        [
            "hostkeeper daemon: level=error alert=prune_failed service=web: the old versions of web could not be pruned: 0.9.0: the removal was refused; 1.0.0: the removal was refused"
        ]
    );
    assert_eq!(leftovers(rig), 2);
    assert_eq!(versions(rig), ["2.0.0", "3.0.0"]);

    web(&farm, "4.0.0");
    assert_eq!(daemon.order(&farm.update(WEB, "4.0.0", 4)), succeeded());
    assert_eq!(leftovers(rig), 0);
    assert_eq!(versions(rig), ["3.0.0", "4.0.0"]);
    assert_eq!(lines(&alerts).len(), 1);
}

/// Catches a prune that writes a file another release reads: the step record, the seals of the
/// versions kept and `placed.json` are the same bytes after a prune as before it. The prune was
/// told to keep the version switched to.
#[test]
fn prune_adds_no_persisted_field() {
    let farm = NativeHost::new("prune-no-field");
    let rig = farm.rig();
    let (daemon, pruner) = pruning(&farm);
    two_forced(&farm, &daemon);
    stale(rig, "0.9.0");
    let dir = rig.packages.join(WEB);
    let files = [
        rig.state.join(WEB).join(RECORD_FILE),
        dir.join("1.0.0.seal"),
        dir.join("2.0.0.seal"),
        rig.scratch.path().join("placed/placed.json"),
    ];
    let before = bytes_of(&files);
    let ctx = daemon.ctx();
    let service = name(WEB);
    let guard = ctx
        .locks
        .take(&service, package(HookedVerb::Force, "2.0.0"))
        .expect("the service's lock");
    let report = prune::confirmed_switch(ctx, &guard, &service, &version("2.0.0"));
    assert_eq!(
        report.map(|report| report.deleted),
        Some(["0.9.0".to_owned()].into())
    );
    assert_eq!(bytes_of(&files), before);
    let told = pruner.protected().last().cloned().expect("a call");
    assert!(told.switch_targets.contains("2.0.0"), "{told:?}");
}

/// Catches a prune that guesses when it cannot read what protects a version: a step record of a
/// newer schema, a step record that does not parse, a `placed.json` that does not parse, and an
/// unfinished record of a service with no package directory each make the confirmed switch an
/// alert, with nothing deleted.
#[test]
fn a_record_that_cannot_be_read_prunes_nothing() {
    let farm = NativeHost::new("prune-unreadable");
    let rig = farm.rig();
    let rec = recorded(&farm);
    let (daemon, alerts) = alerting(daemon_on(
        &farm,
        rig.placed_fs.clone(),
        Arc::new(LayoutPrune),
    ));
    let record = rig.state.join(WEB).join(RECORD_FILE);
    let before = snapshot(&rig.packages);
    for text in [r#"{"schema":99}"#, "not a record"] {
        fs::write(record.as_path(), text).expect("the record");
        assert_eq!(confirm(&daemon), None, "{text}");
    }
    save(rig, WEB, &rec);
    fs::write(rig.scratch.path().join("placed/placed.json"), "not JSON").expect("placed.json");
    assert_eq!(confirm(&daemon), None);
    assert_eq!(snapshot(&rig.packages), before);

    let ghost = ServiceName::new("ghost").expect("a name");
    let mut unfinished = rec.clone();
    unfinished.service = "ghost".to_owned();
    unfinished.state = State::Pending;
    fs::create_dir_all(rig.state.join("ghost")).expect("its state directory");
    save(rig, "ghost", &unfinished);
    let ctx = daemon.ctx();
    let guard = ctx
        .locks
        .take(&ghost, package(HookedVerb::Update, "1.0.0"))
        .expect("the lock");
    assert_eq!(
        prune::confirmed_switch(ctx, &guard, &ghost, &version("1.0.0")),
        None
    );
    let alerted = lines(&alerts);
    assert_eq!(alerted.len(), 4, "{alerted:?}");
    assert_eq!(
        alerted[0],
        "hostkeeper daemon: level=error alert=prune_failed service=web: the old versions of web could not be pruned: the step record has schema 99, newer than this daemon"
    );
    assert!(
        alerted
            .iter()
            .all(|line| line.contains("alert=prune_failed"))
    );
}

/// Catches a prune that deletes the versions of a service whose installer it has no steps of: a
/// service `placed.json` records with an installer this daemon has no steps of (one a newer
/// binary installed, before a fallback, or `hostkeeper-archive`, which no daemon of this release
/// runs) has nothing pruned, and the confirmed switch is the alert naming the installer. A
/// service with no installer recorded has its old versions deleted.
#[test]
fn a_service_of_an_installer_without_steps_prunes_nothing() {
    let farm = NativeHost::new("prune-installer");
    let rig = farm.rig();
    recorded(&farm);
    let (daemon, alerts) = alerting(daemon_on(
        &farm,
        rig.placed_fs.clone(),
        Arc::new(LayoutPrune),
    ));
    let placed = rig.scratch.path().join("placed/placed.json");
    let before = snapshot(&rig.packages);
    let mut expected = Vec::new();
    for installer in ["oci-image", "hostkeeper-archive"] {
        fs::write(
            &placed,
            format!(r#"{{"schema":1,"installed":{{"web":{{"installer":"{installer}"}}}}}}"#),
        )
        .expect("placed.json");
        assert_eq!(confirm(&daemon), None, "{installer}");
        assert_eq!(snapshot(&rig.packages), before, "{installer}");
        expected.push(format!(
            "hostkeeper daemon: level=error alert=prune_failed service=web: the old versions of web could not be pruned: web is installed by the installer \"{installer}\", which this daemon has no steps of"
        ));
        assert_eq!(lines(&alerts), expected);
    }

    fs::write(&placed, r#"{"schema":1,"installed":{}}"#).expect("placed.json");
    let report = confirm(&daemon).expect("a report");
    assert_eq!(report.deleted, ["1.0.0", "4.0.0"].map(String::from).into());
    assert_eq!(versions(rig), ["2.0.0", "3.0.0"]);
    assert_eq!(lines(&alerts), expected, "nothing failed");
}
