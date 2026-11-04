//! Orders end to end through `Daemon::order`: decoding, the daemon's own checks, the locks, the
//! try rules, the halt, and the step runner's verdicts, all on fakes of the daemon's ports.

mod support;

use std::fs;
use std::num::NonZeroU64;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_daemon::packages::PackageError;
use hostkeeper_daemon::testing::{FakeClock, manifest};
use hostkeeper_daemon::verb::{
    DIGEST_RULE, Forced, Hooked, NO_TRY, NO_VERB, PackageDigest, SERVICE_RULE, VERSION_RULE,
};
use hostkeeper_daemon::verbs::update::declared_params;
use hostkeeper_daemon::{Refusal, Reply, Verb};
use hostkeeper_hook::{Class, Phase, Step, Verdict};
use hostkeeper_proto::hostkeeper::agent::v1::{
    Abort, ClearHalt, Force, Install, Order, Uninstall, order,
};
use hostkeeper_steps::record::{Loaded, RECORD_FILE};
use hostkeeper_steps::{RecordStore, tree_digest};
use support::{Rig, d, digest, force_rollback, update};

fn succeeded() -> Reply {
    Reply::Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
        resent: false,
    }
}

fn resent(verdict: Verdict) -> Reply {
    Reply::Finished {
        verdict,
        reason: None,
        resent: true,
    }
}

fn name(text: &str) -> ServiceName {
    ServiceName::new(text).expect("a service name")
}

/// The step record of `service`, which must be there.
fn record_of(rig: &Rig, service: &str) -> Box<hostkeeper_steps::Record> {
    match RecordStore::new(&rig.state.join(service))
        .load()
        .expect("load")
    {
        Loaded::Current(rec) => rec,
        other => panic!("no current record of {service}: {other:?}"),
    }
}

fn no_record(rig: &Rig, service: &str) -> bool {
    !rig.state.join(service).join(RECORD_FILE).exists()
}

/// Catches a verb that is not built doing something anyway (or panicking): each answers
/// `not_implemented`, takes no lock, asks for no package and writes nothing, whether it comes as
/// an order or as a verb (`clear_record` comes only from the admin socket).
#[test]
fn unimplemented_verbs_are_refused_not_implemented_and_touch_nothing() {
    let rig = Rig::new("unimpl");
    let daemon = rig.daemon();
    let web = name("web");
    let hooked = Hooked {
        service: web.clone(),
        version: Version::new("2.0.0").expect("a version"),
        digest: PackageDigest::new(&d()).expect("a digest"),
        order_try: NonZeroU64::MIN,
    };
    let verbs = [
        ("install", Verb::Install(hooked.clone())),
        ("uninstall", Verb::Uninstall(web.clone())),
        (
            "force",
            Verb::Force(Forced {
                package: hooked.clone(),
                halt_generation: 1,
            }),
        ),
        ("abort", Verb::Abort(web.clone())),
        (
            "clear_halt",
            Verb::ClearHalt {
                service: web.clone(),
                halt_generation: 1,
            },
        ),
        ("clear_record", Verb::ClearRecord(web.clone())),
    ];
    for (verb_name, verb) in verbs {
        let detail = format!("`{verb_name}` is not implemented in this daemon yet (a later task)");
        assert_eq!(
            daemon.execute(verb),
            Reply::Refused {
                refusal: Refusal::NotImplemented,
                detail
            },
            "{verb_name}"
        );
    }
    let (service, version, package_digest) = ("web".to_owned(), "2.0.0".to_owned(), d());
    let orders = [
        order::Verb::Install(Install {
            service: service.clone(),
            version: version.clone(),
            package_digest: package_digest.clone(),
        }),
        order::Verb::Uninstall(Uninstall {
            service: service.clone(),
        }),
        order::Verb::Force(Force {
            service: service.clone(),
            version,
            package_digest,
            halt_generation: 2,
        }),
        order::Verb::Abort(Abort {
            service: service.clone(),
        }),
        order::Verb::ClearHalt(ClearHalt {
            service,
            halt_generation: 2,
        }),
    ];
    for verb in orders {
        let order = Order {
            order_id: "o".to_owned(),
            r#try: 1,
            verb: Some(verb.clone()),
        };
        let reply = daemon.order(&order);
        assert_eq!(reply.refusal(), Some(Refusal::NotImplemented), "{verb:?}");
    }
    assert!(!rig.state.exists());
    assert!(rig.store.asked().is_empty());
    assert!(rig.launcher.runs().is_empty());
    assert_eq!(daemon.ctx().locks.holder(&web), None);
}

/// Catches a malformed order that reaches the package store or a lock: every row is refused
/// `invalid_order` with a text that names its field, before anything is asked or written.
#[test]
fn an_unspecified_or_malformed_order_is_refused_naming_the_field() {
    let rig = Rig::new("malformed");
    let daemon = rig.daemon();
    let ok = d();
    let long_version = "1".repeat(129);
    let long_service = "s".repeat(65);
    let upper = "AB".repeat(32);
    let short = "a".repeat(63);
    let with_verb = |verb: order::Verb| Order {
        order_id: "o".to_owned(),
        r#try: 1,
        verb: Some(verb),
    };
    let rows: Vec<(Order, &str, &str)> = vec![
        (
            Order {
                order_id: "o".to_owned(),
                r#try: 1,
                verb: None,
            },
            "verb",
            NO_VERB,
        ),
        (update("web", "2.0.0", &ok, 0), "try", NO_TRY),
        (
            update("we b", "2.0.0", &ok, 1),
            "update.service",
            SERVICE_RULE,
        ),
        (update("", "2.0.0", &ok, 1), "update.service", SERVICE_RULE),
        (
            update(&long_service, "2.0.0", &ok, 1),
            "update.service",
            SERVICE_RULE,
        ),
        (
            update("web", ".2.0", &ok, 1),
            "update.version",
            VERSION_RULE,
        ),
        (
            update("web", &long_version, &ok, 1),
            "update.version",
            VERSION_RULE,
        ),
        (update("web", "", &ok, 1), "update.version", VERSION_RULE),
        (
            update("web", "2.0/0", &ok, 1),
            "update.version",
            VERSION_RULE,
        ),
        (
            update("web", "2.0.0", &short, 1),
            "update.package_digest",
            DIGEST_RULE,
        ),
        (
            update("web", "2.0.0", &upper, 1),
            "update.package_digest",
            DIGEST_RULE,
        ),
        (
            update("web", "2.0.0", "", 1),
            "update.package_digest",
            DIGEST_RULE,
        ),
        (
            force_rollback("w/b", "1.0.0", &ok, 1),
            "force_rollback.service",
            SERVICE_RULE,
        ),
        (
            with_verb(order::Verb::Install(Install {
                service: "web".to_owned(),
                version: "2.0.0".to_owned(),
                package_digest: short.clone(),
            })),
            "install.package_digest",
            DIGEST_RULE,
        ),
        (
            with_verb(order::Verb::Force(Force {
                service: "web".to_owned(),
                version: ".1".to_owned(),
                package_digest: ok.clone(),
                halt_generation: 1,
            })),
            "force.version",
            VERSION_RULE,
        ),
        (
            with_verb(order::Verb::Uninstall(Uninstall {
                service: String::new(),
            })),
            "uninstall.service",
            SERVICE_RULE,
        ),
        (
            with_verb(order::Verb::Abort(Abort {
                service: "a b".to_owned(),
            })),
            "abort.service",
            SERVICE_RULE,
        ),
        (
            with_verb(order::Verb::ClearHalt(ClearHalt {
                service: "x.y".to_owned(),
                halt_generation: 1,
            })),
            "clear_halt.service",
            SERVICE_RULE,
        ),
    ];
    for (order, field, rule) in rows {
        assert_eq!(
            daemon.order(&order),
            Reply::Refused {
                refusal: Refusal::InvalidOrder,
                detail: format!("the order's {field} {rule}"),
            },
            "{order:?}"
        );
    }
    assert!(rig.store.asked().is_empty());
    assert!(!rig.state.exists());
}

/// Catches the daemon trusting the order or the manifest alone: a package whose manifest names
/// another service, or another version, than the order is refused `manifest_mismatch`, and
/// nothing runs and no record is written.
#[test]
fn manifest_name_or_version_that_differs_from_the_order_is_refused() {
    let rig = Rig::new("mismatch");
    let daemon = rig.daemon();
    rig.store.add("web", "2.0.0", &manifest("api", "2.0.0"));
    rig.store.add("web", "2.1.0", &manifest("web", "2.2.0"));
    for (version, expected, found) in [("2.0.0", "web", "api"), ("2.1.0", "2.1.0", "2.2.0")] {
        let reply = daemon.order(&update("web", version, &d(), 1));
        assert_eq!(
            reply.refusal(),
            Some(Refusal::ManifestMismatch),
            "{reply:?}"
        );
        let Reply::Refused { detail, .. } = reply else {
            panic!("not refused");
        };
        assert!(
            detail.contains(expected) & detail.contains(found),
            "{detail}"
        );
    }
    assert!(rig.launcher.runs().is_empty());
    assert!(no_record(&rig, "web"));
}

/// Catches a package that needs a newer daemon being run: `min_hostkeeper` above this daemon's
/// own version is `daemon_too_old`, and nothing runs.
#[test]
fn min_hostkeeper_above_the_running_daemon_is_refused() {
    let rig = Rig::new("too-old");
    let daemon = rig.daemon();
    let text = manifest("web", "2.0.0")
        .replace("min_hostkeeper = \"0.1.0\"", "min_hostkeeper = \"0.2.0\"");
    rig.store.add("web", "2.0.0", &text);
    let reply = daemon.order(&update("web", "2.0.0", &d(), 1));
    assert_eq!(reply.refusal(), Some(Refusal::DaemonTooOld), "{reply:?}");
    assert!(rig.launcher.runs().is_empty());
    assert!(no_record(&rig, "web"));
}

/// Catches a downgrade rule that ignores the verb, or compares versions as strings: with
/// `1.10.0` running, an `update` to `1.9.0` is `below_running` (as text it would sort above),
/// and a `force_rollback` of the same package runs.
#[test]
fn an_update_below_the_running_version_is_refused_but_force_rollback_is_accepted() {
    let rig = Rig::new("below");
    let daemon = rig.daemon();
    rig.running("web", "1.10.0");
    rig.add("web", "1.9.0");
    assert_eq!(
        daemon.order(&update("web", "1.9.0", &d(), 1)),
        Reply::Refused {
            refusal: Refusal::BelowRunning,
            detail: "1.9.0 is below 1.10.0".to_owned(),
        }
    );
    assert!(rig.store.asked().is_empty());
    assert_eq!(
        daemon.order(&force_rollback("web", "1.9.0", &d(), 2)),
        succeeded()
    );
    assert_eq!(rig.launcher.steps("web")[0], Step::Prepare);
}

/// Catches orders that queue behind a busy service, or a lock that is global: while one order
/// of a service runs, another package's order of that service is `busy` at once, and another
/// service's order is served (it gets its package while the first holds the step lock, then
/// waits for the step lock, and both end).
#[test]
fn a_second_order_while_one_runs_is_busy() {
    let rig = Rig::new("busy");
    rig.add("alpha", "2.0.0");
    rig.add("alpha", "3.0.0");
    rig.add("beta", "2.0.0");
    let gate = rig.launcher.hold("alpha", Step::Prepare);
    let daemon = Arc::new(rig.daemon());
    let first = {
        let daemon = Arc::clone(&daemon);
        thread::spawn(move || daemon.order(&update("alpha", "2.0.0", &d(), 1)))
    };
    gate.entered();
    let other = daemon.order(&update("alpha", "3.0.0", &d(), 2));
    assert_eq!(
        other,
        Reply::Refused {
            refusal: Refusal::Busy,
            detail: "alpha is locked by an order to update it to 2.0.0".to_owned(),
        }
    );
    let (events, seen) = mpsc::channel();
    rig.store.notify(events.clone());
    let second = {
        let daemon = Arc::clone(&daemon);
        thread::spawn(move || {
            let reply = daemon.order(&update("beta", "2.0.0", &d(), 1));
            events
                .send(format!("beta replied {reply:?}"))
                .unwrap_or_default();
            reply
        })
    };
    assert_eq!(seen.recv().expect("an event"), "beta 2.0.0");
    gate.release();
    assert_eq!(first.join().expect("the first order"), succeeded());
    assert_eq!(second.join().expect("the second order"), succeeded());
    assert_eq!(rig.launcher.most_running(), 1);
}

/// Catches a resend that runs the order a second time: the same order while it runs is
/// answered `Running`, and once it ended with its recorded verdict; the hook ran once.
#[test]
fn a_second_order_for_the_same_package_is_a_resend_not_a_second_run() {
    let rig = Rig::new("resend");
    rig.add("web", "2.0.0");
    let gate = rig.launcher.hold("web", Step::Prepare);
    let daemon = Arc::new(rig.daemon());
    let first = {
        let daemon = Arc::clone(&daemon);
        thread::spawn(move || daemon.order(&update("web", "2.0.0", &d(), 1)))
    };
    gate.entered();
    assert_eq!(
        daemon.order(&update("web", "2.0.0", &d(), 1)),
        Reply::Running
    );
    gate.release();
    assert_eq!(first.join().expect("the order"), succeeded());
    let steps = [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup];
    assert_eq!(rig.launcher.steps("web"), steps);
    let ended = Verdict::Succeeded {
        promotion_pending: false,
    };
    assert_eq!(
        daemon.order(&update("web", "2.0.0", &d(), 1)),
        resent(ended)
    );
    assert_eq!(rig.launcher.steps("web"), steps);
}

/// Catches a daemon that forgets its verdicts across a restart: after the daemon that ran an
/// order is gone, a new one on the same directories answers the same order (same try) from the
/// step record, and runs nothing.
#[test]
fn resend_after_daemon_death_returns_the_recorded_verdict() {
    let rig = Rig::new("death");
    rig.add("web", "2.0.0");
    rig.launcher.exits("web", Step::Prepare, &[1]);
    let first = rig.daemon();
    let failed = Verdict::Failed {
        phase: Phase::BeforeCommit,
        class: Class::Release,
    };
    let reply = first.order(&update("web", "2.0.0", &d(), 4));
    assert_eq!(
        reply,
        Reply::Finished {
            verdict: failed.clone(),
            reason: None,
            resent: false
        }
    );
    drop(first);
    let runs = rig.launcher.runs().len();
    let second = rig.daemon();
    assert_eq!(
        second.order(&update("web", "2.0.0", &d(), 4)),
        resent(failed)
    );
    assert_eq!(rig.launcher.runs().len(), runs);
    assert_eq!(record_of(&rig, "web").order_try, Some(4));
}

/// Catches a same-try order with another version being run or answered from the record.
#[test]
fn same_try_with_a_different_version_is_try_conflict() {
    let rig = Rig::new("conflict-v");
    rig.add("web", "2.0.0");
    rig.add("web", "2.1.0");
    let daemon = rig.daemon();
    assert_eq!(daemon.order(&update("web", "2.0.0", &d(), 3)), succeeded());
    assert_eq!(
        daemon.order(&update("web", "2.1.0", &d(), 3)),
        Reply::Refused {
            refusal: Refusal::TryConflict,
            detail: "update 2.1.0 try 3 of web; the step record holds try 3".to_owned(),
        }
    );
    assert_eq!(rig.store.asked().len(), 1);
}

/// Catches a same-try order with another digest being taken for a resend.
#[test]
fn same_try_with_a_different_digest_is_try_conflict() {
    let rig = Rig::new("conflict-d");
    rig.add("web", "2.0.0");
    let daemon = rig.daemon();
    assert_eq!(daemon.order(&update("web", "2.0.0", &d(), 3)), succeeded());
    let reply = daemon.order(&update("web", "2.0.0", &digest("cd"), 3));
    assert_eq!(reply.refusal(), Some(Refusal::TryConflict));
    assert_eq!(rig.store.asked().len(), 1);
}

/// Catches a lower try being run (a stale resend from before a coordinator restart), for
/// another package and for the same one.
#[test]
fn a_lower_try_is_stale_try() {
    let rig = Rig::new("stale");
    rig.add("web", "2.0.0");
    rig.add("web", "2.1.0");
    let daemon = rig.daemon();
    assert_eq!(daemon.order(&update("web", "2.0.0", &d(), 5)), succeeded());
    for version in ["2.1.0", "2.0.0"] {
        let reply = daemon.order(&update("web", version, &d(), 4));
        assert_eq!(reply.refusal(), Some(Refusal::StaleTry), "{version}");
    }
    assert_eq!(rig.store.asked().len(), 1);
}

/// Catches a higher try that is not run once the last order ended: it runs, and the record
/// keeps its try.
#[test]
fn a_higher_try_runs_once_the_last_order_ended() {
    let rig = Rig::new("higher");
    rig.add("web", "2.0.0");
    rig.add("web", "2.1.0");
    let daemon = rig.daemon();
    assert_eq!(daemon.order(&update("web", "2.0.0", &d(), 1)), succeeded());
    assert_eq!(daemon.order(&update("web", "2.1.0", &d(), 2)), succeeded());
    let rec = record_of(&rig, "web");
    assert_eq!(
        (rec.target_version.as_str(), rec.order_try),
        ("2.1.0", Some(2))
    );
    assert_eq!(rig.launcher.steps("web").len(), 8);
}

/// Catches a higher try of another package being run over an unfinished order (one the
/// launcher lost, left `running` in the record): it is `busy`; the same package with a higher
/// try resumes the unfinished order, stopping its unit by name first.
#[test]
fn a_higher_try_during_an_unfinished_different_package_is_busy() {
    let rig = Rig::new("unfinished");
    rig.add("web", "2.0.0");
    rig.add("web", "3.0.0");
    rig.launcher.lose("web", Step::Cutover);
    let daemon = rig.daemon();
    let lost = daemon.order(&update("web", "2.0.0", &d(), 1));
    assert_eq!(lost.refusal(), Some(Refusal::LauncherFailed), "{lost:?}");
    let reply = daemon.order(&update("web", "3.0.0", &d(), 2));
    assert_eq!(reply.refusal(), Some(Refusal::Busy), "{reply:?}");
    assert_eq!(daemon.order(&update("web", "2.0.0", &d(), 2)), succeeded());
    assert_eq!(rig.launcher.stops(), ["updater-web-cutover-1"]);
    assert_eq!(
        rig.launcher.steps("web"),
        [
            Step::Prepare,
            Step::Cutover,
            Step::Cutover,
            Step::Verify,
            Step::Cleanup
        ]
    );
}

/// Catches a halt that holds one version only, or every service: after an update exits 76,
/// every other order of that service is `halted` (another version, the same package with a
/// higher try, a `force_rollback`), its resend is answered with the halt, and another service
/// still runs.
#[test]
fn a_halt_record_holds_every_later_order_for_the_service_and_no_other() {
    let rig = Rig::new("halt");
    rig.running("alpha", "1.0.0");
    rig.add("alpha", "2.0.0");
    rig.add("alpha", "2.1.0");
    rig.add("beta", "2.0.0");
    rig.launcher.exits("alpha", Step::Prepare, &[76]);
    let daemon = rig.daemon();
    let halted = daemon.order(&update("alpha", "2.0.0", &d(), 1));
    assert_eq!(
        halted,
        Reply::Finished {
            verdict: Verdict::Halt,
            reason: None,
            resent: false
        }
    );
    let later = [
        update("alpha", "2.1.0", &d(), 2),
        update("alpha", "2.0.0", &d(), 2),
        force_rollback("alpha", "1.0.0", &d(), 3),
        update("alpha", "2.1.0", &d(), 1),
    ];
    for order in later {
        assert_eq!(
            daemon.order(&order).refusal(),
            Some(Refusal::Halted),
            "{order:?}"
        );
    }
    assert_eq!(
        daemon.order(&update("alpha", "2.0.0", &d(), 1)),
        resent(Verdict::Halt)
    );
    assert_eq!(rig.launcher.steps("alpha"), [Step::Prepare]);
    assert_eq!(daemon.order(&update("beta", "2.0.0", &d(), 1)), succeeded());
}

/// Catches the daemon running anything but the new package's hook, in another order, or
/// touching the release it replaces: `prepare` (given the version directory), `cutover`,
/// `verify`, `cleanup`, each the new package's `bin/hook`; the release it replaces keeps its
/// digest and is the record's previous release; the verdict is in the reply and the record.
#[test]
fn an_update_runs_the_five_steps_of_the_new_packages_hook_in_order() {
    let rig = Rig::new("five");
    let old = rig.running("web", "1.0.0");
    let new = rig.add("web", "2.0.0");
    let old_digest = tree_digest(&old).expect("the old tree");
    let daemon = rig.daemon();
    assert_eq!(daemon.order(&update("web", "2.0.0", &d(), 1)), succeeded());
    let runs = rig.launcher.runs();
    let steps: Vec<Step> = runs.iter().map(|ran| ran.step).collect();
    assert_eq!(
        steps,
        [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup]
    );
    for ran in &runs {
        assert_eq!(ran.executable, new.join("bin/hook"), "{ran:?}");
        let artifact = (ran.step == Step::Prepare).then(|| new.clone());
        assert_eq!(ran.artifact, artifact, "{ran:?}");
        assert_eq!(ran.unit, format!("updater-web-{}-1", ran.step));
    }
    assert_eq!(tree_digest(&old).expect("the old tree"), old_digest);
    let rec = record_of(&rig, "web");
    assert_eq!(rec.previous_digest, Some(old_digest));
    assert_eq!(rec.tarball_digest, d());
    let status = daemon.status().expect("status");
    let web = &status.services[0];
    assert_eq!(
        web.last_result().map(|f| f.verdict),
        Some(Verdict::Succeeded {
            promotion_pending: false
        })
    );
}

/// Catches a failed `prepare` not rolled back, or reported in another phase: `prepare` exits 1,
/// `rollback` runs, the verdict is failed before the commit.
#[test]
fn a_failed_prepare_reports_failed_before_commit_and_runs_rollback() {
    let rig = Rig::new("prep-fail");
    rig.running("web", "1.0.0");
    rig.add("web", "2.0.0");
    rig.launcher.exits("web", Step::Prepare, &[1]);
    let daemon = rig.daemon();
    assert_eq!(
        daemon.order(&update("web", "2.0.0", &d(), 1)),
        Reply::Finished {
            verdict: Verdict::Failed {
                phase: Phase::BeforeCommit,
                class: Class::Release
            },
            reason: None,
            resent: false
        }
    );
    assert_eq!(rig.launcher.steps("web"), [Step::Prepare, Step::Rollback]);
}

/// Catches a mapping of exit codes of the daemon's own: through the real step runner, a
/// `prepare` that exits 75 is rolled back and deferred, and one that exits 76 halts at once.
#[test]
fn exit_75_reports_deferred_and_exit_76_reports_halt() {
    let rig = Rig::new("exits");
    rig.add("alpha", "2.0.0");
    rig.add("beta", "2.0.0");
    rig.launcher.exits("alpha", Step::Prepare, &[75]);
    rig.launcher.exits("beta", Step::Prepare, &[76]);
    let daemon = rig.daemon();
    let deferred = Verdict::Deferred {
        retry_after_secs: None,
    };
    let reply = daemon.order(&update("alpha", "2.0.0", &d(), 1));
    assert_eq!(
        reply,
        Reply::Finished {
            verdict: deferred,
            reason: None,
            resent: false
        }
    );
    assert_eq!(rig.launcher.steps("alpha"), [Step::Prepare, Step::Rollback]);
    let reply = daemon.order(&update("beta", "2.0.0", &d(), 1));
    assert_eq!(
        reply,
        Reply::Finished {
            verdict: Verdict::Halt,
            reason: None,
            resent: false
        }
    );
    assert_eq!(rig.launcher.steps("beta"), [Step::Prepare]);
}

/// Catches a daemon that exits, or lets go of the service, when the dead-man cannot stop a step
/// at its bound: the order is interrupted `unstopped`, the service stays locked (another
/// package is `busy`, and so is a resend of the same one, which nothing runs any more), and
/// another service and the status still work.
#[test]
fn an_unstoppable_step_keeps_the_service_lock_and_the_daemon_serving() {
    let rig = Rig::new("unstopped");
    rig.add("alpha", "2.0.0");
    rig.add("alpha", "3.0.0");
    rig.add("beta", "2.0.0");
    rig.launcher.fail_stops("alpha");
    let stops = rig.launcher.stops_seen();
    let gate = rig.launcher.hold("alpha", Step::Prepare);
    let clock = FakeClock::frozen();
    let mut ports = rig.ports();
    ports.clock = clock.clone();
    ports.deadman_poll_secs = Some(1);
    let daemon = Arc::new(hostkeeper_daemon::Daemon::new(
        support::config(),
        rig.dirs(),
        ports,
        support::OWN,
    ));
    let first = {
        let daemon = Arc::clone(&daemon);
        thread::spawn(move || daemon.order(&update("alpha", "2.0.0", &d(), 1)))
    };
    gate.entered();
    // Past the bound of `prepare` (600 seconds): the dead-man tries to stop the step.
    clock.advance(601);
    assert_eq!(stops.recv().expect("a stop"), "updater-alpha-prepare-1");
    gate.release();
    let reply = first.join().expect("the order");
    assert_eq!(reply.refusal(), Some(Refusal::Unstopped), "{reply:?}");
    let alpha = name("alpha");
    assert!(daemon.ctx().locks.holder(&alpha).expect("held").stuck);
    let other = daemon.order(&update("alpha", "3.0.0", &d(), 2));
    assert_eq!(other.refusal(), Some(Refusal::Busy));
    let resend = daemon.order(&update("alpha", "2.0.0", &d(), 1));
    assert_eq!(resend.refusal(), Some(Refusal::Busy), "{resend:?}");
    assert_eq!(daemon.order(&update("beta", "2.0.0", &d(), 1)), succeeded());
    let status = daemon.status().expect("status");
    let names: Vec<_> = status
        .services
        .iter()
        .map(|s| s.service.to_string())
        .collect();
    assert_eq!(names, ["alpha", "beta"]);
    assert!(status.services[0].locked & status.services[0].step_running());
    assert!(!status.services[1].step_running());
}

/// Catches two steps running at once on the host: while a step of one service is held, an
/// order of another service gets its package (outside the step lock) and then waits; its first
/// step starts only after the held service's last step, and no two runs overlap.
#[test]
fn lock_order_is_service_then_step() {
    let rig = Rig::new("lock-order");
    rig.add("alpha", "2.0.0");
    rig.add("beta", "2.0.0");
    let alpha_gate = rig.launcher.hold("alpha", Step::Verify);
    let beta_gate = rig.launcher.hold("beta", Step::Prepare);
    let daemon = Arc::new(rig.daemon());
    let order = |service: &'static str| {
        let daemon = Arc::clone(&daemon);
        thread::spawn(move || daemon.order(&update(service, "2.0.0", &d(), 1)))
    };
    let alpha = order("alpha");
    alpha_gate.entered();
    let (events, seen) = mpsc::channel();
    rig.store.notify(events);
    let beta = order("beta");
    assert_eq!(
        seen.recv().expect("beta asks for its package"),
        "beta 2.0.0"
    );
    alpha_gate.release();
    beta_gate.entered();
    let services: Vec<String> = rig
        .launcher
        .runs()
        .into_iter()
        .map(|ran| ran.service)
        .collect();
    assert_eq!(services, ["alpha", "alpha", "alpha", "alpha", "beta"]);
    beta_gate.release();
    assert_eq!(alpha.join().expect("alpha"), succeeded());
    assert_eq!(beta.join().expect("beta"), succeeded());
    assert_eq!(rig.launcher.most_running(), 1);
}

/// Catches a package failure that writes a step record or becomes a free string: each error of
/// the package store reaches the reply as its token, with its text, and no record is written.
#[test]
fn package_errors_map_to_closed_reasons() {
    let rig = Rig::new("pkg-errors");
    let daemon = rig.daemon();
    let rows = [
        (
            PackageError::NotFound("none".to_owned()),
            Refusal::PackageNotFound,
        ),
        (
            PackageError::DigestMismatch("cd".to_owned()),
            Refusal::DigestMismatch,
        ),
        (PackageError::NoSpace("full".to_owned()), Refusal::NoSpace),
        (
            PackageError::Other("eof".to_owned()),
            Refusal::PackageFailed,
        ),
    ];
    for (n, (error, refusal)) in rows.into_iter().enumerate() {
        let version = format!("2.0.{n}");
        rig.store.fail("web", &version, error.clone());
        let reply = daemon.order(&update("web", &version, &d(), 1));
        assert_eq!(
            reply,
            Reply::Refused {
                refusal,
                detail: error.to_string()
            }
        );
    }
    assert!(no_record(&rig, "web"));
    assert!(rig.launcher.runs().is_empty());
}

/// Catches an order run over a step record the daemon cannot read, or one a newer daemon wrote
/// (which must never be changed), or a record path it cannot read at all: each is refused with
/// its token and nothing runs.
#[test]
fn a_record_that_cannot_be_read_stops_the_services_orders() {
    let rig = Rig::new("records");
    let daemon = rig.daemon();
    let rows = [
        ("corrupt", Refusal::RecordUnreadable),
        ("newer", Refusal::RecordNewer),
        ("dir", Refusal::StoreFailed),
    ];
    for (service, refusal) in rows {
        rig.add(service, "2.0.0");
        let dir = rig.state.join(service);
        fs::create_dir_all(&dir).expect("the service dir");
        let path = dir.join(RECORD_FILE);
        match service {
            "corrupt" => fs::write(&path, "{").expect("write"),
            "newer" => fs::write(&path, r#"{"schema": 2}"#).expect("write"),
            _ => fs::create_dir(&path).expect("a directory"),
        }
        let reply = daemon.order(&update(service, "2.0.0", &d(), 1));
        assert_eq!(reply.refusal(), Some(refusal), "{service}: {reply:?}");
    }
    assert!(rig.store.asked().is_empty());
    assert!(rig.launcher.runs().is_empty());
}

/// Catches the wrong release taken as the one an order replaces: when `current` already names
/// the target (the daemon switched before it stopped), the release replaced is `previous`'s;
/// when `current` names a version whose directory is gone, the order is refused
/// `release_unreadable` before anything runs.
#[test]
fn the_release_an_order_replaces_is_current_or_previous() {
    let rig = Rig::new("previous");
    let old = rig.add("web", "1.0.0");
    rig.running("web", "2.0.0");
    rig.store.link("web", "previous", "1.0.0");
    let daemon = rig.daemon();
    assert_eq!(daemon.order(&update("web", "2.0.0", &d(), 1)), succeeded());
    let rec = record_of(&rig, "web");
    assert_eq!(
        rec.previous_digest,
        Some(tree_digest(&old).expect("digest"))
    );

    rig.add("api", "2.0.0");
    rig.store.link("api", "current", "1.5.0");
    let reply = daemon.order(&update("api", "2.0.0", &d(), 1));
    assert_eq!(
        reply.refusal(),
        Some(Refusal::ReleaseUnreadable),
        "{reply:?}"
    );
    assert!(no_record(&rig, "api"));
}

/// Catches an order run while the service's state directory cannot be made: something that is
/// not a directory (a dangling link) is where it goes, the order is refused `store_failed`, and
/// nothing runs.
#[test]
fn a_state_directory_that_cannot_be_written_refuses_the_order() {
    let rig = Rig::new("readonly");
    rig.add("web", "2.0.0");
    fs::create_dir_all(&rig.state).expect("the state dir");
    std::os::unix::fs::symlink("nowhere", rig.state.join("web")).expect("a dangling link");
    let reply = rig.daemon().order(&update("web", "2.0.0", &d(), 1));
    assert_eq!(reply.refusal(), Some(Refusal::StoreFailed), "{reply:?}");
    assert!(rig.launcher.runs().is_empty());
}

/// Catches a package the store cannot seal (its directory is not there) reaching the runner:
/// it is `package_failed`, and nothing is written.
#[test]
fn a_package_the_store_cannot_seal_is_package_failed() {
    let rig = Rig::new("no-seal");
    let reply = rig.daemon().order(&update("web", "9.9.9", &d(), 1));
    assert_eq!(reply.refusal(), Some(Refusal::PackageFailed), "{reply:?}");
    assert_eq!(rig.store.asked(), [("web".to_owned(), "9.9.9".to_owned())]);
    assert!(no_record(&rig, "web"));
}

/// Catches a manifest table the hook's params refuse being handed to the hook: it is
/// `manifest_mismatch`; a table of scalars passes as it is.
#[test]
fn declared_params_refuse_what_a_params_file_may_not_hold() {
    let reserved: toml::Table = "\"updater.target_version\" = \"1\"".parse().expect("toml");
    let refused = declared_params(&reserved).expect_err("reserved");
    assert_eq!(refused.refusal(), Some(Refusal::ManifestMismatch));
    let plain: toml::Table = "bound = 900".parse().expect("toml");
    let params = declared_params(&plain).expect("plain");
    assert_eq!(params.get_u64("bound"), Some(900));
}

/// Catches a held halt generation that cannot be read being taken as 0: the service's orders
/// are refused `halt_unreadable`, and another service's run.
#[test]
fn a_held_halt_that_cannot_be_read_stops_that_services_orders() {
    let rig = Rig::new("halt-garbage");
    rig.add("web", "2.0.0");
    rig.add("api", "2.0.0");
    fs::create_dir_all(rig.state.join("web")).expect("the service dir");
    fs::write(rig.state.join("web").join("held-halt"), "seven\n").expect("write");
    let daemon = rig.daemon();
    let reply = daemon.order(&update("web", "2.0.0", &d(), 1));
    assert_eq!(
        reply,
        Reply::Refused {
            refusal: Refusal::HaltUnreadable,
            detail: "the held halt generation of web is not a number: \"seven\\n\"".to_owned(),
        }
    );
    assert_eq!(daemon.order(&update("api", "2.0.0", &d(), 1)), succeeded());
}
