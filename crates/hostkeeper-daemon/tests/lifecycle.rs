//! Where a service is in its life on the host (design section 6.1): the lifecycle the daemon
//! reports, the orders it refuses for it, and what a `placed.json` it cannot read does to each.

mod support;

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use hostkeeper_daemon::checks::{force_allowed, installed_rule};
use hostkeeper_daemon::installed::Lifecycle;
use hostkeeper_daemon::testing::spawn;
use hostkeeper_daemon::verb::HookedVerb;
use hostkeeper_daemon::{Outcome, Refusal, Reply};
use hostkeeper_hook::{Class, Phase, Step, Verdict};
use hostkeeper_placed::error::{ClaimError, Owner};
use hostkeeper_placed::{PlacedError, Tombstone};
use hostkeeper_proto::hostkeeper::agent::v1::{Force, Order, order};
use support::{Rig, alerting, d, install, lines, name, snapshot, uninstall};

fn force(service: &str) -> Order {
    Order {
        order_id: format!("force-{service}"),
        r#try: 1,
        verb: Some(order::Verb::Force(Force {
            service: service.to_owned(),
            version: "2.0.0".to_owned(),
            package_digest: d(),
            index_digest: String::new(),
            halt_generation: 1,
        })),
    }
}

/// `service` installed at 1.0.0 from a package that places, with an `uninstall` bound.
fn installed(rig: &Rig, daemon: &hostkeeper_daemon::Daemon, service: &str) {
    rig.placed(service, "1.0.0", Some(300));
    let reply = daemon.order(&install(service, "1.0.0", &d(), 1));
    assert!(
        matches!(
            reply,
            Reply::Finished {
                verdict: Verdict::Succeeded { .. },
                ..
            }
        ),
        "{reply:?}"
    );
}

/// Catches the lifecycle rules applied to the wrong verb or the wrong state: a tombstone refuses
/// every hooked order; `install` needs no release installed and the others one; a resumed order
/// passes whatever `current` says; `force` is refused while an install or an uninstall is under
/// way.
#[test]
fn the_lifecycle_rules_refuse_by_where_the_service_is() {
    use HookedVerb::{ForceRollback, Install, Update};
    let rows = [
        (Install, false, false, false, Ok(())),
        (Install, true, false, false, Err(Refusal::AlreadyInstalled)),
        (Install, true, true, false, Ok(())),
        (Update, true, false, false, Ok(())),
        (Update, false, false, false, Err(Refusal::NotInstalled)),
        (
            ForceRollback,
            false,
            false,
            false,
            Err(Refusal::NotInstalled),
        ),
        (Update, false, true, false, Ok(())),
        (Update, true, false, true, Err(Refusal::Uninstalling)),
        (Install, false, true, true, Err(Refusal::Uninstalling)),
    ];
    for (verb, installed, resuming, tombstoned, want) in rows {
        assert_eq!(
            installed_rule(verb, installed, resuming, tombstoned),
            want,
            "{verb:?} installed={installed} resuming={resuming} tombstoned={tombstoned}"
        );
    }
    let forces = [
        (Lifecycle::Running, Ok(())),
        (Lifecycle::Absent, Ok(())),
        (Lifecycle::Installing, Err(Refusal::Installing)),
        (Lifecycle::Removing, Err(Refusal::Uninstalling)),
    ];
    for (lifecycle, want) in forces {
        assert_eq!(force_allowed(lifecycle), want, "{lifecycle:?}");
    }
    let lives = [
        ((true, true, true), Lifecycle::Removing),
        ((true, false, false), Lifecycle::Removing),
        ((false, true, true), Lifecycle::Running),
        ((false, false, true), Lifecycle::Installing),
        ((false, false, false), Lifecycle::Absent),
    ];
    for ((tombstoned, installed, busy), want) in lives {
        assert_eq!(Lifecycle::of(tombstoned, installed, busy), want);
    }
}

/// Catches a failure of linking reported with another token: a path held by another is
/// `path_conflict`, a claim or unit that breaks a rule of linking `manifest_mismatch`, a
/// tombstone `uninstalling`, and a failure of the host `link_failed`.
#[test]
fn each_failure_of_linking_has_its_token() {
    let rows = [
        (
            PlacedError::PathConflict {
                path: PathBuf::from("/etc/x"),
                owner: Owner::File,
            },
            Refusal::PathConflict,
        ),
        (
            PlacedError::Claim(ClaimError::EmptyPath),
            Refusal::ManifestMismatch,
        ),
        (
            PlacedError::Tombstoned {
                service: "web".to_owned(),
            },
            Refusal::Uninstalling,
        ),
        (
            PlacedError::Io {
                op: "write",
                path: PathBuf::from("/var/lib/hostkeeper/placed.json"),
                source: std::io::Error::other("full"),
            },
            Refusal::LinkFailed,
        ),
    ];
    for (error, refusal) in rows {
        assert_eq!(Refusal::from(&error), refusal, "{error}");
    }
}

/// Catches a force that cuts into an install or an uninstall, or changes anything while it is
/// refused: a force of a tombstoned service is `uninstalling`, and of one whose first install is
/// under way `installing`, each with nothing on disk changed.
#[test]
fn force_is_refused_while_a_service_is_tombstoned() {
    let rig = Rig::new("force-refused");
    let daemon = Arc::new(rig.daemon());
    installed(&rig, &daemon, "web");
    rig.launcher.additive_exits("web", &[1]);
    assert!(matches!(
        daemon.order(&uninstall("web", 1)),
        Reply::Finished {
            verdict: Verdict::Failed { .. },
            ..
        }
    ));
    rig.placed("new", "1.0.0", None);
    let gate = rig.launcher.hold("new", Step::Prepare);
    let installing = {
        let daemon = Arc::clone(&daemon);
        spawn("the install", move || {
            daemon.order(&install("new", "1.0.0", &d(), 1))
        })
    };
    gate.entered(&installing)
        .expect("the install's prepare runs");
    let before = snapshot(rig.scratch.path());
    let rows = [("web", Refusal::Uninstalling), ("new", Refusal::Installing)];
    for (service, refusal) in rows {
        let reply = daemon.order(&force(service));
        assert_eq!(reply.refusal(), Some(refusal), "{service}: {reply:?}");
    }
    let Reply::Refused { detail, .. } = daemon.order(&force("web")) else {
        panic!("refused");
    };
    assert_eq!(detail, "force of web: it is Removing");
    assert_eq!(snapshot(rig.scratch.path()), before);
    gate.release();
    assert!(matches!(
        installing.join().expect("the install"),
        Reply::Finished { .. }
    ));
}

/// Catches an uninstall that is not reported while it is unfinished, or reported with the wrong
/// verdict: a service whose uninstall failed is `removing`, with the uninstall's verdict, its
/// step and its try as `last`, in the library's report and on the wire; a tombstone with nothing
/// else of its service left is listed and `removing`; a service that runs a release is
/// `running`, one being installed `installing`, one with only a staged version `absent`.
#[test]
fn status_reports_removing_while_an_uninstall_is_unfinished() {
    let rig = Rig::new("status-removing");
    let daemon = Arc::new(rig.daemon());
    installed(&rig, &daemon, "web");
    rig.launcher
        .additive_replies("web", &[(1, r#"{"class":"environment","reason":"drain"}"#)]);
    assert!(matches!(
        daemon.order(&uninstall("web", 7)),
        Reply::Finished {
            verdict: Verdict::Failed { .. },
            ..
        }
    ));
    rig.running("api", "1.0.0");
    rig.stage("db", "1.0.0");
    let mut record = rig.placed_record();
    record
        .uninstalling
        .insert("ghost".to_owned(), Tombstone::default());
    rig.save_placed(&record);
    rig.placed("new", "1.0.0", None);
    let gate = rig.launcher.hold("new", Step::Prepare);
    let installing = {
        let daemon = Arc::clone(&daemon);
        spawn("the install", move || {
            daemon.order(&install("new", "1.0.0", &d(), 1))
        })
    };
    gate.entered(&installing)
        .expect("the install's prepare runs");

    let status = daemon.status().expect("status");
    let lives: Vec<(String, Lifecycle)> = status
        .services
        .iter()
        .map(|s| (s.service.to_string(), s.lifecycle))
        .collect();
    assert_eq!(
        lives,
        [
            ("api".to_owned(), Lifecycle::Running),
            ("db".to_owned(), Lifecycle::Absent),
            ("ghost".to_owned(), Lifecycle::Removing),
            ("new".to_owned(), Lifecycle::Installing),
            ("web".to_owned(), Lifecycle::Removing),
        ]
    );
    let web = &status.services[4];
    let last = web.uninstall.as_ref().expect("the uninstall's verdict");
    assert_eq!(last.order_try, 7);
    assert_eq!(
        last.finished.verdict,
        Verdict::Failed {
            phase: Phase::BeforeCommit,
            class: Class::Environment
        }
    );

    assert_eq!(last.finished.reason.as_deref(), Some("drain"));
    // A service that runs a release reports its last order from its step record.
    assert_eq!(
        (
            status.services[0].last_result(),
            &status.services[0].uninstall
        ),
        (None, &None),
        "api ran no order through this daemon"
    );
    gate.release();
    installing.join().expect("the install");

    // Once the uninstall is done, the service is no longer listed.
    assert!(matches!(
        daemon.order(&uninstall("web", 8)),
        Reply::Finished {
            verdict: Verdict::Succeeded { .. },
            ..
        }
    ));
    let names: Vec<String> = daemon
        .status()
        .expect("status")
        .services
        .iter()
        .map(|s| s.service.to_string())
        .collect();
    assert!(!names.contains(&"web".to_owned()), "{names:?}");
}

/// Catches a `placed.json` that cannot be read taken as one without tombstones by anything that
/// changes the host: an uninstall and a force are refused `link_failed`, an install or update is
/// refused the same, the start resumes no uninstall and alerts, and `Status` still answers.
#[test]
fn a_placed_json_that_cannot_be_read_holds_every_change() {
    let rig = Rig::new("placed-garbled");
    rig.running("web", "1.0.0");
    rig.add("api", "2.0.0");
    fs::write(rig.host("/placed/placed.json"), "{").expect("garbled");
    let (daemon, alerts) = alerting(rig.daemon());
    for order in [
        uninstall("web", 1),
        force("web"),
        support::update("api", "2.0.0", &d(), 1),
    ] {
        let reply = daemon.order(&order);
        assert_eq!(reply.refusal(), Some(Refusal::LinkFailed), "{reply:?}");
    }
    assert!(daemon.status().is_ok());
    fs::create_dir_all(&rig.state).expect("the state directory");
    let report = daemon.boot_pass().expect("the start pass");
    assert!(report.boot_id_written);
    assert!(
        lines(&alerts)
            .iter()
            .any(|line| line.contains("alert=start_incomplete") & line.contains("placed.json")),
        "{:?}",
        lines(&alerts)
    );
    assert!(rig.launcher.runs().is_empty() & rig.launcher.additive_runs().is_empty());
}

/// Catches a tombstone whose uninstall resumes into a release it cannot run: a tombstone that
/// has not run its hook, for a service whose `current` is gone, or whose manifest has no
/// `uninstall` bound, is interrupted with nothing removed, and stays.
#[test]
fn a_tombstone_whose_release_cannot_run_the_hook_is_interrupted() {
    let rig = Rig::new("tombstone-unrunnable");
    let daemon = rig.daemon();
    installed(&rig, &daemon, "web");
    rig.placed("api", "1.0.0", None);
    assert!(matches!(
        daemon.order(&install("api", "1.0.0", &d(), 1)),
        Reply::Finished { .. }
    ));
    installed(&rig, &daemon, "db");
    fs::remove_file(rig.packages.join("web/current")).expect("current gone");
    fs::remove_file(rig.packages.join("db/1.0.0.seal")).expect("the seal gone");
    let mut record = rig.placed_record();
    for service in ["web", "api", "db"] {
        record
            .uninstalling
            .insert(service.to_owned(), Tombstone::default());
    }
    rig.save_placed(&record);
    let rows = [
        ("web", Refusal::ReleaseUnreadable),
        ("api", Refusal::UninstallUnsupported),
        ("db", Refusal::ReleaseUnreadable),
    ];
    for (service, refusal) in rows {
        let reply = daemon.order(&uninstall(service, 1));
        assert_eq!(reply.refusal(), Some(refusal), "{service}: {reply:?}");
        assert!(matches!(reply, Reply::Interrupted { .. }), "{reply:?}");
        assert!(rig.packages.join(service).join("1.0.0").exists());
    }
    assert_eq!(rig.placed_record().uninstalling.len(), 3);
    assert!(rig.launcher.additive_runs().is_empty());
    // The start resumes neither, and says why.
    let report = daemon.boot_pass().expect("the start pass");
    for service in ["web", "api", "db"] {
        assert!(
            matches!(
                &report.services[service].outcome,
                Outcome::Interrupted { .. }
            ),
            "{service}"
        );
    }
    assert_eq!(
        daemon.ctx().lifecycle(&name("web")).expect("the lifecycle"),
        Lifecycle::Removing
    );
}
