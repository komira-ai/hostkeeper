//! `uninstall`: the sequence of design section 4.7 in its order, what ends it early and what is
//! then left, the refusals before anything changes, and what it never touches.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use hostkeeper_daemon::halt::HaltStore;
use hostkeeper_daemon::testing::{AdditiveFault, Signal, spawn};
use hostkeeper_daemon::{Daemon, Refusal, Reply};
use hostkeeper_hook::{Class, Phase, Verdict};
use hostkeeper_placed::testing::{Event, FakeUnitControl, FsWrite, UnitCall};
use hostkeeper_placed::{Entry, PathKind, PathState};
use support::{Rig, TARGET, d, install, name, snapshot, uninstall, update};

fn finished(verdict: Verdict, reason: Option<&str>) -> Reply {
    Reply::Finished {
        verdict,
        reason: reason.map(str::to_owned),
        resent: false,
    }
}

fn succeeded() -> Reply {
    finished(
        Verdict::Succeeded {
            promotion_pending: false,
        },
        None,
    )
}

/// `service` installed at `version` from a package that places a unit, its `WantedBy=` link and
/// a file, with an `uninstall` bound of 300 seconds.
fn installed(rig: &Rig, daemon: &Daemon, service: &str, version: &str) {
    rig.placed(service, version, Some(300));
    assert_eq!(
        daemon.order(&install(service, version, &d(), 1)),
        succeeded()
    );
}

/// Whether nothing of `service` is left: no package directory, no step record, no placed path
/// on disk or in `placed.json`, no tombstone.
fn gone(rig: &Rig, service: &str) -> bool {
    let record = rig.placed_record();
    let placed = rig
        .placed_paths(service)
        .iter()
        .any(|path| rig.there(path) | record.paths.contains_key(path));
    !rig.packages.join(service).exists()
        & !rig.state.join(service).join("step-record.json").exists()
        & !placed
        & record.of_service(service).next().is_none()
        & !record.uninstalling.contains_key(service)
}

/// The events of the rig's placement log as a test reads them: each write of `placed.json` as
/// `placed.json`, each removal as `remove <host path without its leading />` (a directory with
/// a `/`), each stop and reload, each mark; the temporary files, fsyncs, starts and queries of
/// units are left out.
fn effects(rig: &Rig) -> Vec<String> {
    let below = |path: &Path| path.strip_prefix("/").unwrap_or(path).display().to_string();
    rig.placed_log
        .events()
        .into_iter()
        .filter_map(|event| match event {
            Event::Fs(FsWrite::Rename { to, .. }) if to.ends_with("placed.json") => {
                Some("placed.json".to_owned())
            }
            Event::Fs(FsWrite::Remove(path)) => Some(format!("remove {}", below(&path))),
            Event::Fs(FsWrite::RemoveDir(path)) => Some(format!("remove {}/", below(&path))),
            Event::Unit(UnitCall::Stop(unit)) => Some(format!("stop {unit}")),
            Event::Unit(UnitCall::DaemonReload) => Some("daemon-reload".to_owned()),
            Event::Mark(mark) => Some(mark),
            Event::Fs(_) | Event::Unit(_) => None,
        })
        .collect()
}

/// Catches a step of the uninstall run out of the order of design section 4.7, for example the
/// paths dropped or the package removed before the hook ran: the tombstone is written first; the
/// `run` unit is stopped; the hook's `uninstall` step runs, and its success is recorded; the
/// units are disabled, the placed links removed, the paths dropped from `placed.json` in the
/// write that owes the `daemon-reload`, the reload runs; then, last, `current`, the seal, the
/// version directory and the step record go, and the tombstone with them.
#[test]
fn uninstall_runs_the_sequence_in_the_documented_order() {
    let rig = Rig::new("uninstall-order");
    let daemon = rig.daemon();
    installed(&rig, &daemon, "web", "1.0.0");
    let marks = rig.placed_log.clone();
    rig.launcher.during_additive(move |call| {
        marks.push(Event::Mark(format!("hook {}", call.step)));
    });
    rig.placed_log.clear();

    assert_eq!(daemon.order(&uninstall("web", 4)), succeeded());

    let wants = format!("units/{TARGET}.wants/web.service");
    let conf = rig
        .conf_path("web")
        .strip_prefix("/")
        .expect("absolute")
        .display()
        .to_string();
    let version = "packages/web/1.0.0";
    let want = [
        // 1. The tombstone.
        "placed.json".to_owned(),
        // 2. The `run` unit stopped.
        "stop web.service".to_owned(),
        // 3. The hook's `uninstall` step, and its success recorded.
        "hook uninstall".to_owned(),
        "placed.json".to_owned(),
        // 4. and 5. Disabled (the unit stopped again before its links go), unlinked, the paths
        // dropped in the write that owes the reload, `daemon-reload`, the reload no longer owed.
        "stop web.service".to_owned(),
        format!("remove {wants}"),
        format!("remove {conf}"),
        "remove units/web.service".to_owned(),
        "placed.json".to_owned(),
        "daemon-reload".to_owned(),
        "placed.json".to_owned(),
        // 6. Last: `current`, the seal, the version directory, the package directory; then the
        // step record and its directory (the daemon's own, not through the placement's
        // filesystem); then the tombstone goes.
        "remove packages/web/current".to_owned(),
        "remove packages/web/1.0.0.seal".to_owned(),
        format!("remove {version}/bin/hook"),
        format!("remove {version}/bin/"),
        format!("remove {version}/files/web.conf"),
        format!("remove {version}/files/"),
        format!("remove {version}/manifest.toml"),
        format!("remove {version}/units/web.service"),
        format!("remove {version}/units/"),
        format!("remove {version}/"),
        "remove packages/web/".to_owned(),
        "placed.json".to_owned(),
    ];
    assert_eq!(effects(&rig), want);
    assert!(!rig.state.join("web").exists());
    assert!(gone(&rig, "web"));
    let ran = rig.launcher.additive_runs();
    assert_eq!(ran.len(), 1);
    assert_eq!(
        (
            ran[0].step.as_str(),
            ran[0].unit.as_str(),
            ran[0].timeout_secs
        ),
        ("uninstall", "updater-web-uninstall-1", 300)
    );
    assert_eq!(ran[0].hook_args, ["update-hook"]);
    assert_eq!(ran[0].executable, rig.packages.join("web/1.0.0/bin/hook"));
    let params = hostkeeper_hook::Params::parse(&ran[0].params).expect("the params file");
    assert_eq!(params.target_version(), Some("1.0.0"));
    assert_eq!(
        params.staged_release(),
        Some(rig.packages.join("web/1.0.0"))
    );
    assert!(
        rig.launcher
            .stops()
            .contains(&"updater-web-uninstall-1".to_owned())
    );
}

/// Catches an uninstall that goes on past a unit that will not stop, or removes anything when
/// it does not: the order ends `Failed`, class environment, `unit_stuck`; the tombstone stands;
/// the hook is not run; every file, link, seal and directory of the service is still there; the
/// next order, once the unit stops, finishes the uninstall.
#[test]
fn a_stuck_unit_ends_failed_unit_stuck_with_nothing_removed() {
    let rig = Rig::new("uninstall-stuck");
    installed(&rig, &rig.daemon(), "web", "1.0.0");
    let before = snapshot(rig.scratch.path());
    let stuck = Arc::new(FakeUnitControl::new(rig.placed_log.clone()));
    stuck.fail_stop("web.service");
    let daemon = rig.daemon_with(rig.placed_fs.clone(), stuck);
    assert_eq!(
        daemon.order(&uninstall("web", 1)),
        finished(
            Verdict::Failed {
                phase: Phase::BeforeCommit,
                class: Class::Environment
            },
            Some("unit_stuck")
        )
    );
    assert!(rig.launcher.additive_runs().is_empty());
    assert!(rig.placed_record().uninstalling.contains_key("web"));
    // Only `placed.json` changed: it holds the tombstone.
    let placed = rig.host("/placed/placed.json");
    let changed: Vec<PathBuf> = snapshot(rig.scratch.path())
        .into_iter()
        .filter(|entry| !before.contains(entry))
        .map(|(path, _)| path)
        .collect();
    assert_eq!(changed, [placed]);

    assert_eq!(rig.daemon().order(&uninstall("web", 2)), succeeded());
    assert!(gone(&rig, "web"));
}

/// Catches an exit of the `uninstall` step read as another verdict, or a bound that is not the
/// manifest's: 0 completes; 1 leaves the service stopped and tombstoned, with its files; 75 is
/// `Deferred` with the hook's wait, the tombstone kept; 76 halts, and the next order is refused
/// `halted` and runs nothing; a run killed at its bound (the clock past it) is a failure. Each
/// run is bounded by the manifest's `uninstall` bound.
#[test]
fn uninstall_hook_exit_codes_map_to_verdicts() {
    let rig = Rig::new("uninstall-exits");
    let daemon = rig.daemon();
    for service in ["ok", "fails", "defers", "halts", "killed"] {
        installed(&rig, &daemon, service, "1.0.0");
    }
    rig.launcher.additive_replies(
        "fails",
        &[(1, r#"{"class":"environment","reason":"drain"}"#)],
    );
    rig.launcher
        .additive_replies("defers", &[(75, r#"{"retry_after_secs":120}"#)]);
    rig.launcher.additive_exits("halts", &[76]);
    rig.launcher.additive_fault("killed", AdditiveFault::Killed);
    let clock = rig.clock.clone();
    rig.launcher.during_additive(move |call| {
        if call.service == "killed" {
            clock.advance(call.timeout_secs + 1);
        }
    });

    // A release whose step record is gone runs its step in generation 0.
    fs::remove_file(rig.state.join("ok/step-record.json")).expect("the record gone");
    assert_eq!(daemon.order(&uninstall("ok", 1)), succeeded());
    assert!(gone(&rig, "ok"));
    assert_eq!(
        rig.launcher.additive_runs()[0].unit,
        "updater-ok-uninstall-0"
    );
    assert_eq!(
        daemon.order(&uninstall("fails", 1)),
        finished(
            Verdict::Failed {
                phase: Phase::BeforeCommit,
                class: Class::Environment
            },
            Some("drain")
        )
    );
    assert_eq!(
        daemon.order(&uninstall("defers", 1)),
        finished(
            Verdict::Deferred {
                retry_after_secs: Some(120)
            },
            None
        )
    );
    assert_eq!(
        daemon.order(&uninstall("halts", 1)),
        finished(Verdict::Halt, None)
    );
    assert_eq!(
        daemon.order(&uninstall("killed", 1)),
        finished(
            Verdict::Failed {
                phase: Phase::BeforeCommit,
                class: Class::Release
            },
            None
        )
    );
    let record = rig.placed_record();
    for service in ["fails", "defers", "halts", "killed"] {
        let tombstone = &record.uninstalling[service];
        assert!(!tombstone.hook_done, "{service}");
        assert_eq!(tombstone.halted, service == "halts", "{service}");
        assert!(
            rig.packages.join(service).join("1.0.0").exists(),
            "{service}"
        );
        assert!(
            rig.placed_paths(service).iter().all(|p| rig.there(p)),
            "{service}"
        );
    }
    let bounds: Vec<u64> = rig
        .launcher
        .additive_runs()
        .iter()
        .map(|ran| ran.timeout_secs)
        .collect();
    assert_eq!(bounds, [300; 5]);

    let held = daemon.order(&uninstall("halts", 2));
    assert_eq!(held.refusal(), Some(Refusal::Halted), "{held:?}");
    assert_eq!(rig.launcher.additive_runs().len(), 5);
}

/// Catches an uninstall begun that cannot finish, or that changes something before it refuses:
/// a manifest without an `uninstall` bound is `uninstall_unsupported`, as is the service
/// `hostkeeper`; an unfinished order in the step record is `busy`, a halt in it `halted`, a held
/// halt generation that does not read `halt_unreadable`, and a release that is not the tree its
/// seal names `release_unreadable`. In each case `placed.json` holds no tombstone, nothing was
/// stopped and no hook ran.
#[test]
fn uninstall_unsupported_is_refused_before_anything_changes() {
    let rig = Rig::new("uninstall-refused");
    let daemon = rig.daemon();
    rig.placed("web", "1.0.0", None);
    assert_eq!(daemon.order(&install("web", "1.0.0", &d(), 1)), succeeded());
    for service in ["lost", "halted", "garbled", "altered", "unsealed"] {
        installed(&rig, &daemon, service, "1.0.0");
    }
    // hostkeeper's install is its self-update, which this package (no binary) cannot run: its
    // package is put in place as an install leaves it.
    rig.placed("hostkeeper", "1.0.0", Some(300));
    rig.store.link("hostkeeper", "current", "1.0.0");
    rig.launcher.lose("lost", hostkeeper_hook::Step::Verify);
    rig.placed("lost", "2.0.0", Some(300));
    let lost = daemon.order(&update("lost", "2.0.0", &d(), 2));
    assert_eq!(lost.refusal(), Some(Refusal::LauncherFailed), "{lost:?}");
    rig.launcher
        .exits("halted", hostkeeper_hook::Step::Prepare, &[76]);
    rig.placed("halted", "2.0.0", Some(300));
    assert_eq!(
        daemon.order(&update("halted", "2.0.0", &d(), 2)),
        finished(Verdict::Halt, None)
    );
    fs::write(rig.state.join("garbled").join("held-halt"), "x\n").expect("a garbled halt");
    fs::write(rig.packages.join("altered/1.0.0/bin/hook"), "#!changed\n").expect("altered");
    fs::remove_file(rig.packages.join("unsealed/1.0.0.seal")).expect("the seal gone");
    rig.placed_log.clear();
    let before = snapshot(rig.scratch.path());

    let rows = [
        ("web", Refusal::UninstallUnsupported),
        ("hostkeeper", Refusal::UninstallUnsupported),
        ("lost", Refusal::Busy),
        ("halted", Refusal::Halted),
        ("garbled", Refusal::HaltUnreadable),
        ("altered", Refusal::ReleaseUnreadable),
        ("unsealed", Refusal::ReleaseUnreadable),
    ];
    for (service, refusal) in rows {
        let reply = daemon.order(&uninstall(service, 1));
        assert_eq!(reply.refusal(), Some(refusal), "{service}: {reply:?}");
        assert!(
            matches!(reply, Reply::Refused { .. }),
            "{service}: {reply:?}"
        );
    }
    let Reply::Refused { detail, .. } = daemon.order(&uninstall("web", 1)) else {
        panic!("refused");
    };
    assert_eq!(
        detail,
        "the manifest of web 1.0.0 has no [hook.timeout_secs] uninstall"
    );
    assert!(rig.placed_record().uninstalling.is_empty());
    assert!(effects(&rig).is_empty());
    assert!(rig.launcher.additive_runs().is_empty());
    assert_eq!(snapshot(rig.scratch.path()), before);
}

/// Catches an uninstall that touches the service's own data: a sentinel under the fake
/// `/var/lib/<service>` and one under `/etc/<service>` survive the uninstall byte for byte, and
/// the recording filesystem logs no operation below either.
#[test]
fn uninstall_never_touches_the_services_data_directory() {
    let rig = Rig::new("uninstall-data");
    let daemon = rig.daemon();
    installed(&rig, &daemon, "web", "1.0.0");
    let data = rig.scratch.path().join("var/lib/web");
    let config = rig.scratch.path().join("etc/web");
    for dir in [&data, &config] {
        fs::create_dir_all(dir).expect("the service's directory");
        fs::write(dir.join("sentinel"), "the service's own\n").expect("a sentinel");
    }
    rig.placed_log.clear();
    assert_eq!(daemon.order(&uninstall("web", 1)), succeeded());
    for dir in [&data, &config] {
        assert_eq!(
            fs::read_to_string(dir.join("sentinel")).expect("still there"),
            "the service's own\n"
        );
    }
    let (data_host, config_host) = (Path::new("/var/lib/web"), Path::new("/etc/web"));
    let touched: Vec<Event> = rig
        .placed_log
        .events()
        .into_iter()
        .filter(|event| {
            let paths: Vec<&Path> = match event {
                Event::Fs(
                    FsWrite::Write(p)
                    | FsWrite::SyncFile(p)
                    | FsWrite::SyncDir(p)
                    | FsWrite::Remove(p)
                    | FsWrite::RemoveDir(p)
                    | FsWrite::CreateDirs(p),
                ) => vec![p.as_path()],
                Event::Fs(FsWrite::Rename { from, to }) => vec![from.as_path(), to.as_path()],
                Event::Fs(FsWrite::Symlink { target, link }) => {
                    vec![target.as_path(), link.as_path()]
                }
                Event::Unit(_) | Event::Mark(_) => Vec::new(),
            };
            paths
                .iter()
                .any(|p| p.starts_with(data_host) | p.starts_with(config_host))
        })
        .collect();
    assert_eq!(touched, Vec::<Event>::new());
}

/// Catches an uninstall that removes what is another service's: `api` and `web` both installed,
/// `web` uninstalled; every entry of `api` in `placed.json`, its links, its version directory,
/// its seal, its `current` and its step record are as they were. The held halt generation of
/// `web` stays (it is never lowered), and with it `web`'s state directory.
#[test]
fn uninstall_leaves_other_services_placed_paths_and_versions_alone() {
    let rig = Rig::new("uninstall-other");
    let daemon = rig.daemon();
    installed(&rig, &daemon, "web", "1.0.0");
    installed(&rig, &daemon, "api", "2.0.0");
    HaltStore::new(&rig.state)
        .raise(&name("web"), 4)
        .expect("a held halt");
    // What else a crash may leave in the state directory goes; the held halt stays.
    fs::write(rig.state.join("web/params.toml.tmp"), "x").expect("a stray file");
    let api_entries: Vec<(PathBuf, Entry)> = rig
        .placed_record()
        .of_service("api")
        .map(|(path, entry)| (path.clone(), entry.clone()))
        .collect();
    assert_eq!(api_entries.len(), 3);
    let api_dir = rig.packages.join("api");
    let api_before = snapshot(&api_dir);
    let record_before = fs::read(rig.state.join("api/step-record.json")).expect("api's record");

    assert_eq!(daemon.order(&uninstall("web", 1)), succeeded());

    let record = rig.placed_record();
    let api_after: Vec<(PathBuf, Entry)> = record
        .of_service("api")
        .map(|(path, entry)| (path.clone(), entry.clone()))
        .collect();
    assert_eq!(api_after, api_entries);
    assert!(
        api_after
            .iter()
            .all(|(_, entry)| entry.state == PathState::Placed)
    );
    assert!(
        api_after
            .iter()
            .any(|(_, entry)| entry.kind == PathKind::WantsLink)
    );
    for path in rig.placed_paths("api") {
        assert!(rig.there(&path), "{}", path.display());
    }
    assert_eq!(snapshot(&api_dir), api_before);
    assert_eq!(
        fs::read(rig.state.join("api/step-record.json")).expect("api's record"),
        record_before
    );
    assert!(gone(&rig, "web"));
    assert_eq!(
        fs::read_to_string(rig.state.join("web/held-halt")).expect("the held halt"),
        "4\n"
    );
    assert!(!rig.state.join("web/params.toml.tmp").exists());
}

/// Catches a tombstone that waits for an order to be finished, or that the start resumes from
/// its beginning: an uninstall whose hook failed leaves its tombstone; the daemon's next start
/// resumes it without any order, runs the hook again and finishes it, and the report carries its
/// verdict for the heartbeat; a halted uninstall is left halted by the start.
#[test]
fn boot_resumes_a_tombstoned_uninstall_without_an_order() {
    let rig = Rig::new("uninstall-boot");
    let daemon = rig.daemon();
    installed(&rig, &daemon, "web", "1.0.0");
    installed(&rig, &daemon, "api", "1.0.0");
    rig.launcher.additive_exits("web", &[1]);
    rig.launcher.additive_exits("api", &[76]);
    assert!(matches!(
        daemon.order(&uninstall("web", 1)),
        Reply::Finished {
            verdict: Verdict::Failed { .. },
            ..
        }
    ));
    assert_eq!(
        daemon.order(&uninstall("api", 1)),
        finished(Verdict::Halt, None)
    );
    drop(daemon);

    let (_restarted, report) = rig.booted();
    assert_eq!(
        report.services["web"].verdict(),
        Some(support::ok()),
        "{report:?}"
    );
    assert!(matches!(
        &report.services["api"].outcome,
        hostkeeper_daemon::Outcome::Interrupted {
            refusal: Refusal::Halted,
            ..
        }
    ));
    assert!(gone(&rig, "web"));
    assert!(rig.placed_record().uninstalling["api"].halted);
    let runs: Vec<String> = rig
        .launcher
        .additive_runs()
        .into_iter()
        .map(|ran| ran.service)
        .collect();
    assert_eq!(runs, ["web", "api", "web"]);
}

/// Catches a second uninstall run beside the first, another order run beside it, or a launcher
/// failure taken for the hook's verdict: a resend while the uninstall runs answers that it runs,
/// an update of the service is `busy` (`locked by an order to uninstall it`); a launcher that
/// lost the step's unit has it stopped and the uninstall interrupted with the tombstone kept, and
/// one whose unit cannot be stopped keeps the service locked, every later order of it `busy`.
#[test]
fn an_uninstall_holds_its_service_and_a_lost_step_is_interrupted() {
    let rig = Rig::new("uninstall-lock");
    let daemon = Arc::new(rig.daemon());
    installed(&rig, &daemon, "web", "1.0.0");
    let (entered, on_entered) = Signal::new();
    let (release, on_release) = mpsc::channel::<()>();
    let on_release = Mutex::new(on_release);
    rig.launcher.during_additive(move |_| {
        entered.send(());
        on_release
            .lock()
            .expect("the gate")
            .recv()
            .expect("released");
    });
    let running = {
        let daemon = Arc::clone(&daemon);
        spawn("the uninstall", move || daemon.order(&uninstall("web", 1)))
    };
    on_entered.wait(&running).expect("the hook runs");
    assert_eq!(daemon.order(&uninstall("web", 1)), Reply::Running);
    assert_eq!(
        daemon.order(&update("web", "2.0.0", &d(), 2)),
        Reply::Refused {
            refusal: Refusal::Busy,
            detail: "web is locked by an order to uninstall it".to_owned()
        }
    );
    release.send(()).expect("release");
    assert_eq!(running.join().expect("the order"), succeeded());

    let rig = Rig::new("uninstall-lost");
    let daemon = rig.daemon();
    installed(&rig, &daemon, "lost", "1.0.0");
    installed(&rig, &daemon, "stuck", "1.0.0");
    rig.launcher.additive_fault("lost", AdditiveFault::Lost);
    rig.launcher
        .additive_fault("stuck", AdditiveFault::LostUnstoppable);
    let lost = daemon.order(&uninstall("lost", 1));
    assert_eq!(lost.refusal(), Some(Refusal::LauncherFailed), "{lost:?}");
    assert!(rig.placed_record().uninstalling.contains_key("lost"));
    let stuck = daemon.order(&uninstall("stuck", 1));
    assert_eq!(stuck.refusal(), Some(Refusal::Unstopped), "{stuck:?}");
    let again = daemon.order(&uninstall("stuck", 2));
    assert_eq!(again.refusal(), Some(Refusal::Busy), "{again:?}");
    assert_eq!(daemon.order(&uninstall("lost", 2)), succeeded());
}

/// Catches an uninstall of a service with no release installed that fails, runs a hook, or
/// leaves what a failed install staged: with only a staged version and its step record left,
/// the tombstone is written as done with its hook, nothing is stopped or run, and the package
/// and the record go; with nothing of the service on the host, it is a success that writes
/// nothing.
#[test]
fn an_uninstall_of_a_service_with_nothing_installed_removes_what_is_left() {
    let rig = Rig::new("uninstall-absent");
    let daemon = rig.daemon();
    rig.placed("web", "1.0.0", Some(300));
    rig.launcher
        .exits("web", hostkeeper_hook::Step::Prepare, &[1]);
    assert!(matches!(
        daemon.order(&install("web", "1.0.0", &d(), 1)),
        Reply::Finished {
            verdict: Verdict::Failed { .. },
            ..
        }
    ));
    rig.placed_log.clear();
    assert_eq!(daemon.order(&uninstall("web", 1)), succeeded());
    assert!(gone(&rig, "web"));
    assert!(rig.launcher.additive_runs().is_empty());
    assert!(
        !effects(&rig)
            .iter()
            .any(|effect| effect.starts_with("stop "))
    );

    rig.placed_log.clear();
    assert_eq!(daemon.order(&uninstall("web", 2)), succeeded());
    assert_eq!(rig.placed_log.events(), Vec::<Event>::new());

    // Only a staged version, and no state directory at all.
    let rig = Rig::new("uninstall-staged");
    rig.stage("api", "1.0.0");
    assert_eq!(rig.daemon().order(&uninstall("api", 1)), succeeded());
    assert!(!rig.packages.join("api").exists());
    assert!(!rig.state.exists());
}

/// Catches an uninstall that removes a service by the steps of another installer: a service
/// `placed.json` records with an installer this daemon has no steps of (one a newer binary
/// installed, before a fallback) is stopped and its hook run, and then nothing of it is
/// removed: the uninstall is interrupted naming the installer, and its links, its package and
/// its tombstone stay for a daemon that has those steps.
#[test]
fn an_uninstall_of_a_service_of_an_installer_without_steps_removes_nothing() {
    let rig = Rig::new("uninstall-installer");
    let daemon = rig.daemon();
    installed(&rig, &daemon, "web", "1.0.0");
    let mut record = rig.placed_record();
    record.installed.insert(
        "web".to_owned(),
        hostkeeper_placed::InstalledEntry::new("oci-image"),
    );
    rig.save_placed(&record);
    let reply = daemon.order(&uninstall("web", 1));
    assert_eq!(
        reply,
        Reply::Interrupted {
            refusal: Refusal::LinkFailed,
            detail:
                "web is installed by the installer \"oci-image\", which this daemon has no steps of"
                    .to_owned(),
        }
    );
    let after = rig.placed_record();
    assert!(after.uninstalling["web"].hook_done);
    assert!(rig.placed_paths("web").iter().all(|path| rig.there(path)));
    assert!(rig.packages.join("web").join("1.0.0").exists());
}
