//! `install`: the update path with no previous version, the package placed and `current`
//! pointed at it immediately before `cutover`, both taken off the host again before `rollback`,
//! and the lifecycle rule of the hooked orders (design sections 4.1, 4.7, 5.7 and 6.1).

mod support;

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::thread;

use hostkeeper_daemon::installed::Lifecycle;
use hostkeeper_daemon::{Refusal, Reply};
use hostkeeper_hook::{Class, Params, Phase, Step, Verdict};
use hostkeeper_placed::testing::{Event, FaultFs, UnitCall};
use hostkeeper_placed::{PathState, Tombstone};
use support::{Rig, d, force_rollback, install, name, update};

fn succeeded() -> Reply {
    Reply::Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
        resent: false,
    }
}

/// The version `link` (`current`, `previous`) of `service` names, `None` when there is no link.
fn link(rig: &Rig, service: &str, link: &str) -> Option<String> {
    fs::read_link(rig.packages.join(service).join(link))
        .ok()
        .map(|target| target.to_string_lossy().into_owned())
}

/// Catches an install that needs a release to replace, or that tells the hook of one: the hook
/// is given no `updater.previous_*` key, the step record names no previous release, and on
/// `Succeeded` the service runs the target: `current` names it, there is no `previous`, its unit,
/// `WantedBy=` link and file are placed into `current` and recorded `placed`, and `daemon-reload`
/// ran.
#[test]
fn install_runs_the_update_path_with_no_previous_version() {
    let rig = Rig::new("install-first");
    rig.placed("web", "1.0.0", Some(300));
    let daemon = Arc::new(rig.daemon());
    let gate = rig.launcher.hold("web", Step::Cutover);
    let running = {
        let daemon = Arc::clone(&daemon);
        thread::spawn(move || daemon.order(&install("web", "1.0.0", &d(), 1)))
    };
    gate.entered();
    let params = Params::read(&rig.state.join("web").join("params.toml")).expect("the params file");
    assert_eq!(params.previous_release(), None);
    assert_eq!(params.previous_version(), None);
    assert_eq!(params.target_version(), Some("1.0.0"));
    // The switch came before `cutover`: `current` names the target while it runs.
    assert_eq!(link(&rig, "web", "current").as_deref(), Some("1.0.0"));
    gate.release();
    assert_eq!(running.join().expect("the order"), succeeded());

    assert_eq!(
        rig.launcher.steps("web"),
        [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup]
    );
    let rec = rig.record("web").expect("the step record");
    assert_eq!(rec.previous_digest, None);
    assert_eq!(link(&rig, "web", "current").as_deref(), Some("1.0.0"));
    assert_eq!(link(&rig, "web", "previous"), None);
    let [unit, wants, conf] = rig.placed_paths("web");
    // Each link is written as the host names its target: into `current`.
    let current = Path::new("/packages/web/current");
    assert_eq!(
        fs::read_link(rig.host(&unit)).expect("the unit link"),
        current.join("units/web.service")
    );
    assert_eq!(
        fs::read_link(rig.host(&wants)).expect("the WantedBy link"),
        unit
    );
    assert_eq!(
        fs::read_link(rig.host(&conf)).expect("the file link"),
        current.join("files/web.conf")
    );
    let record = rig.placed_record();
    for path in [&unit, &wants, &conf] {
        let entry = &record.paths[path];
        assert_eq!(
            (entry.service.as_str(), entry.version.as_str(), entry.state),
            ("web", "1.0.0", PathState::Placed)
        );
    }
    assert!(!record.reload_pending);
    assert!(
        rig.placed_log
            .events()
            .contains(&Event::Unit(UnitCall::DaemonReload))
    );
    assert_eq!(
        daemon.ctx().lifecycle(&name("web")).expect("lifecycle"),
        Lifecycle::Running
    );
}

/// Catches a failed first install that leaves the service half installed, or one held as a
/// halt when its hook declines: a `verify` that fails rolls back with `current` removed, every
/// placed link removed and dropped from `placed.json`, the version directory kept as a staged
/// version and the service absent; a `prepare` that fails had switched nothing and leaves the
/// same; a `rollback` that declines with `no_previous_release` ends `Failed` with that reason,
/// the service is absent, and a second order of the next try is accepted and installs it.
#[test]
fn a_failed_first_install_rolls_back_or_declines_with_no_previous_release() {
    let rig = Rig::new("install-fails");
    rig.placed("web", "1.0.0", None);
    rig.placed("api", "1.0.0", None);
    rig.placed("db", "1.0.0", None);
    rig.launcher.exits("web", Step::Verify, &[1]);
    rig.launcher.exits("api", Step::Prepare, &[1]);
    rig.launcher.exits("db", Step::Verify, &[1]);
    rig.launcher.replies(
        "db",
        Step::Rollback,
        &[(1, r#"{"reason":"no_previous_release"}"#)],
    );
    let daemon = rig.daemon();

    let failed = daemon.order(&install("web", "1.0.0", &d(), 1));
    assert_eq!(
        failed,
        Reply::Finished {
            verdict: Verdict::Failed {
                phase: Phase::GateRelease,
                class: Class::Release
            },
            reason: None,
            resent: false
        }
    );
    let failed = daemon.order(&install("api", "1.0.0", &d(), 1));
    assert!(
        matches!(
            &failed,
            Reply::Finished {
                verdict: Verdict::Failed {
                    phase: Phase::BeforeCommit,
                    ..
                },
                ..
            }
        ),
        "{failed:?}"
    );
    assert_eq!(rig.launcher.steps("api"), [Step::Prepare, Step::Rollback]);
    let declined = daemon.order(&install("db", "1.0.0", &d(), 1));
    assert_eq!(
        declined,
        Reply::Finished {
            verdict: Verdict::Failed {
                phase: Phase::BeforeCommit,
                class: Class::Release
            },
            reason: Some("no_previous_release".to_owned()),
            resent: false
        }
    );

    let record = rig.placed_record();
    for service in ["web", "api", "db"] {
        assert_eq!(link(&rig, service, "current"), None, "{service}");
        for path in rig.placed_paths(service) {
            assert!(!rig.there(&path), "{}", path.display());
            assert!(!record.paths.contains_key(&path), "{}", path.display());
        }
        assert!(
            rig.packages
                .join(service)
                .join("1.0.0/manifest.toml")
                .exists()
        );
        assert_eq!(
            daemon.ctx().lifecycle(&name(service)).expect("lifecycle"),
            Lifecycle::Absent,
            "{service}"
        );
    }

    // The service is absent: the next try of the order is accepted, and installs it.
    assert_eq!(daemon.order(&install("db", "1.0.0", &d(), 2)), succeeded());
    assert_eq!(link(&rig, "db", "current").as_deref(), Some("1.0.0"));
}

/// Catches an order of the wrong kind for where the service is: an `install` of a service that
/// runs a release is `already_installed`, an `update` or `force_rollback` of one that runs none
/// is `not_installed`, and every hooked order of a service whose tombstone stands is
/// `uninstalling`; each is refused before the package is asked for, and writes nothing. An
/// install that stopped after its switch (`current` names the target) is resumed by its resend,
/// not refused.
#[test]
fn install_of_an_installed_service_and_update_of_an_absent_one_are_refused() {
    let rig = Rig::new("install-refused");
    rig.running("web", "1.0.0");
    rig.stage("api", "1.0.0");
    rig.running("db", "1.0.0");
    let mut record = rig.placed_record();
    record
        .uninstalling
        .insert("db".to_owned(), Tombstone::default());
    rig.save_placed(&record);
    let daemon = rig.daemon();
    let rows = [
        (install("web", "2.0.0", &d(), 1), Refusal::AlreadyInstalled),
        (update("api", "1.0.0", &d(), 1), Refusal::NotInstalled),
        (
            force_rollback("api", "1.0.0", &d(), 1),
            Refusal::NotInstalled,
        ),
        (update("db", "2.0.0", &d(), 1), Refusal::Uninstalling),
        (install("db", "2.0.0", &d(), 1), Refusal::Uninstalling),
    ];
    for (order, refusal) in rows {
        let reply = daemon.order(&order);
        assert_eq!(reply.refusal(), Some(refusal), "{reply:?}");
    }
    let Reply::Refused { detail, .. } = daemon.order(&update("db", "2.0.0", &d(), 1)) else {
        panic!("refused");
    };
    assert_eq!(
        detail,
        "update of db: its installed release is 1.0.0, and it is being uninstalled"
    );
    assert!(rig.store.asked().is_empty());
    assert!(!rig.state.exists());

    // An install the daemon lost after its switch is resumed by its resend.
    let rig = Rig::new("install-resumed");
    rig.placed("web", "1.0.0", None);
    rig.launcher.lose("web", Step::Verify);
    let daemon = rig.daemon();
    let lost = daemon.order(&install("web", "1.0.0", &d(), 1));
    assert_eq!(lost.refusal(), Some(Refusal::LauncherFailed), "{lost:?}");
    assert_eq!(link(&rig, "web", "current").as_deref(), Some("1.0.0"));
    assert_eq!(daemon.order(&install("web", "1.0.0", &d(), 1)), succeeded());
}

/// Catches a first install that places over what is not its own, or links what breaks a rule of
/// linking: a package that claims a file another service placed is `path_conflict`, and one
/// whose unit starts a file of a version directory rather than of `current` is
/// `manifest_mismatch`, each before any step runs or anything is written.
#[test]
fn an_install_that_would_place_over_another_is_refused_path_conflict() {
    let rig = Rig::new("install-conflict");
    rig.placed("web", "1.0.0", None);
    let daemon = rig.daemon();
    assert_eq!(daemon.order(&install("web", "1.0.0", &d(), 1)), succeeded());

    // `api` claims the file `web` placed.
    let dir = rig.placed("api", "1.0.0", None);
    let manifest = fs::read_to_string(dir.join("manifest.toml")).expect("the manifest");
    let conf = rig.conf_path("api").display().to_string();
    let web_conf = rig.conf_path("web").display().to_string();
    fs::write(
        dir.join("manifest.toml"),
        manifest.replace(&conf, &web_conf),
    )
    .expect("write");
    rig.store.seal("api", "1.0.0", &d());
    let reply = daemon.order(&install("api", "1.0.0", &d(), 1));
    assert_eq!(reply.refusal(), Some(Refusal::PathConflict), "{reply:?}");

    // `db`'s unit starts a file of its version directory.
    let dir = rig.placed("db", "1.0.0", None);
    let unit = dir.join("units/db.service");
    let text = fs::read_to_string(&unit).expect("the unit");
    fs::write(&unit, text.replace("/db/current/", "/db/1.0.0/")).expect("write");
    rig.store.seal("db", "1.0.0", &d());
    let reply = daemon.order(&install("db", "1.0.0", &d(), 1));
    assert_eq!(
        reply.refusal(),
        Some(Refusal::ManifestMismatch),
        "{reply:?}"
    );

    assert!(rig.launcher.steps("api").is_empty() & rig.launcher.steps("db").is_empty());
    assert!(rig.record("api").is_none() & rig.record("db").is_none());
    assert_eq!(link(&rig, "api", "current"), None);
}

/// Catches a `cutover` run on a host the daemon could not switch, or an install lost when it
/// cannot: a `placed.json` that cannot be written stops the install with `link_failed` before
/// `cutover`, with nothing placed and `current` not set; the resend on a host that can write it
/// switches and finishes.
#[test]
fn an_install_whose_switch_fails_stops_before_cutover_and_resumes() {
    let rig = Rig::new("install-switch-fails");
    rig.placed("web", "1.0.0", None);
    let failing = Arc::new(FaultFs::failing_at(
        rig.scratch.path(),
        1,
        rig.placed_log.clone(),
    ));
    let daemon = rig.daemon_with(failing, rig.units.clone());
    let reply = daemon.order(&install("web", "1.0.0", &d(), 1));
    assert_eq!(reply.refusal(), Some(Refusal::LinkFailed), "{reply:?}");
    assert!(matches!(reply, Reply::Interrupted { .. }), "{reply:?}");
    assert_eq!(rig.launcher.steps("web"), [Step::Prepare]);
    assert_eq!(link(&rig, "web", "current"), None);
    assert!(rig.placed_paths("web").iter().all(|path| !rig.there(path)));

    let daemon = rig.daemon();
    assert_eq!(daemon.order(&install("web", "1.0.0", &d(), 1)), succeeded());
    assert_eq!(
        rig.launcher.steps("web"),
        [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup]
    );
    assert_eq!(link(&rig, "web", "current").as_deref(), Some("1.0.0"));
}
