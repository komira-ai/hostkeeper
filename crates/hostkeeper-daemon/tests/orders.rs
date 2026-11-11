//! Orders end to end through `Daemon::order`: decoding, the daemon's own checks, the locks, the
//! try rules, the halt, and the step runner's verdicts, all on fakes of the daemon's ports.

mod support;
mod support_apt;
mod support_host;

use std::fs;
use std::sync::Arc;

use hostkeeper_config::names::ServiceName;
use hostkeeper_daemon::testing::{FakeClock, manifest, spawn};
use hostkeeper_daemon::verb::{DIGEST_RULE, NO_TRY, NO_VERB, SERVICE_RULE, VERSION_RULE};
use hostkeeper_daemon::verbs::update::declared_params;
use hostkeeper_daemon::{Refusal, Reply};
use hostkeeper_hook::{Class, Phase, Step, Verdict};
use hostkeeper_proto::hostkeeper::agent::v1::{
    Abort, ClearHalt, ClearRecord, Force, Install, Order, Uninstall, order,
};
use hostkeeper_proto::hostkeeper::release::v1::Target;
use hostkeeper_steps::record::{Loaded, RECORD_FILE};
use hostkeeper_steps::{RecordStore, tree_digest};
use support::versions::hook_path;
use support::{d, digest, force_rollback, update};
use support_apt::with_entry;
use support_host::{NativeHost, pv};

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
fn record_of(rig: &NativeHost, service: &str) -> Box<hostkeeper_steps::Record> {
    match RecordStore::new(&rig.state.join(service))
        .load()
        .expect("load")
    {
        Loaded::Current(rec) => rec,
        other => panic!("no current record of {service}: {other:?}"),
    }
}

fn no_record(rig: &NativeHost, service: &str) -> bool {
    !rig.state.join(service).join(RECORD_FILE).exists()
}

/// Catches a malformed order that reaches the package manager or a lock: every row is refused
/// `invalid_order` with a text that names its field, before anything is asked or written.
#[test]
fn an_unspecified_or_malformed_order_is_refused_naming_the_field() {
    let rig = NativeHost::new("malformed");
    let daemon = rig.daemon();
    let ok = support::d();
    let long_version = "1".repeat(256);
    let long_service = "s".repeat(256);
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
                target: None,
                service: "web".to_owned(),
                version: "2.0.0".to_owned(),
                package_digest: short.clone(),
                index_digest: String::new(),
            })),
            "install.package_digest",
            DIGEST_RULE,
        ),
        (
            with_verb(order::Verb::Force(Force {
                target: None,
                service: "web".to_owned(),
                version: ".1".to_owned(),
                package_digest: ok.clone(),
                index_digest: String::new(),
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
                service: "x+y".to_owned(),
                halt_generation: 1,
            })),
            "clear_halt.service",
            SERVICE_RULE,
        ),
        (
            with_verb(order::Verb::ClearRecord(ClearRecord {
                service: "-web".to_owned(),
            })),
            "clear_record.service",
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
    assert!(rig.tools().is_empty());
    assert!(!rig.state.exists());
}

/// Catches a package that needs a newer daemon being run: `min_hostkeeper` above this daemon's
/// own version is `daemon_too_old`, and nothing runs, is staged or is written.
#[test]
fn min_hostkeeper_above_the_running_daemon_is_refused() {
    let rig = NativeHost::new("too-old");
    rig.seeded("web", support::BASE);
    let daemon = rig.daemon();
    let text = manifest("web", "2.0.0")
        .replace("min_hostkeeper = \"0.1.0\"", "min_hostkeeper = \"0.2.0\"");
    rig.manifest("web", "2.0.0", &text);
    let reply = daemon.order(&rig.update("web", "2.0.0", 1));
    assert_eq!(reply.refusal(), Some(Refusal::DaemonTooOld), "{reply:?}");
    assert!(rig.launcher.runs().is_empty());
    assert!(rig.tools().is_empty());
    assert!(no_record(&rig, "web"));
}

/// Catches a downgrade rule that ignores the verb, or compares versions as strings: with
/// `1.10.0` running, an `update` to `1.9.0` is `below_running` (as text it would sort above),
/// and a `force_rollback` of the same package runs.
#[test]
fn an_update_below_the_running_version_is_refused_but_force_rollback_is_accepted() {
    let rig = NativeHost::new("below");
    let daemon = rig.daemon();
    rig.seeded("web", "1.10.0");
    rig.add("web", "1.9.0");
    assert_eq!(
        daemon.order(&rig.update("web", "1.9.0", 1)),
        Reply::Refused {
            refusal: Refusal::BelowRunning,
            detail: "1.9.0 is below 1.10.0".to_owned(),
        }
    );
    assert!(rig.tools().is_empty());
    assert_eq!(
        daemon.order(&rig.force_rollback("web", "1.9.0", 2)),
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
    let rig = NativeHost::new("busy");
    rig.add("alpha", "2.0.0");
    rig.add("alpha", "3.0.0");
    rig.add("beta", "2.0.0");
    let gate = rig.launcher.hold("alpha", Step::Prepare);
    let daemon = Arc::new(rig.daemon());
    let first = {
        let (daemon, order) = (Arc::clone(&daemon), rig.update("alpha", "2.0.0", 1));
        spawn("alpha's order", move || daemon.order(&order))
    };
    gate.entered(&first).expect("alpha's prepare runs");
    let other = daemon.order(&rig.update("alpha", "3.0.0", 2));
    assert_eq!(
        other,
        Reply::Refused {
            refusal: Refusal::Busy,
            detail: "alpha is locked by an order to update it to 2.0.0".to_owned(),
        }
    );
    let asked = rig.asked();
    let second = {
        let (daemon, order) = (Arc::clone(&daemon), rig.update("beta", "2.0.0", 1));
        spawn("beta's order", move || daemon.order(&order))
    };
    assert_eq!(
        asked.wait(&second).expect("beta asks for its package"),
        format!("beta={}", pv("2.0.0"))
    );
    gate.release();
    assert_eq!(first.join().expect("the first order"), succeeded());
    assert_eq!(second.join().expect("the second order"), succeeded());
    assert_eq!(rig.launcher.most_running(), 1);
}

/// Catches a resend that runs the order a second time: the same order while it runs is
/// answered `Running`, and once it ended with its recorded verdict; the hook ran once.
#[test]
fn a_second_order_for_the_same_package_is_a_resend_not_a_second_run() {
    let rig = NativeHost::new("resend");
    rig.add("web", "2.0.0");
    let gate = rig.launcher.hold("web", Step::Prepare);
    let daemon = Arc::new(rig.daemon());
    let order = rig.update("web", "2.0.0", 1);
    let first = {
        let (daemon, order) = (Arc::clone(&daemon), order.clone());
        spawn("the order", move || daemon.order(&order))
    };
    gate.entered(&first).expect("the prepare runs");
    assert_eq!(daemon.order(&order), Reply::Running);
    gate.release();
    assert_eq!(first.join().expect("the order"), succeeded());
    let steps = [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup];
    assert_eq!(rig.launcher.steps("web"), steps);
    let ended = Verdict::Succeeded {
        promotion_pending: false,
    };
    assert_eq!(daemon.order(&order), resent(ended));
    assert_eq!(rig.launcher.steps("web"), steps);
}

/// Catches a daemon that forgets its verdicts across a restart: after the daemon that ran an
/// order is gone, a new one on the same directories answers the same order (same try) from the
/// step record, and runs nothing.
#[test]
fn resend_after_daemon_death_returns_the_recorded_verdict() {
    let rig = NativeHost::new("death");
    rig.add("web", "2.0.0");
    let order = rig.update("web", "2.0.0", 4);
    rig.launcher.exits("web", Step::Prepare, &[1]);
    let first = rig.daemon();
    let failed = Verdict::Failed {
        phase: Phase::BeforeCommit,
        class: Class::Release,
    };
    let reply = first.order(&order);
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
    assert_eq!(second.order(&order), resent(failed));
    assert_eq!(rig.launcher.runs().len(), runs);
    assert_eq!(record_of(&rig, "web").order_try, Some(4));
}

/// Catches a same-try order with another version being run or answered from the record.
#[test]
fn same_try_with_a_different_version_is_try_conflict() {
    let rig = NativeHost::new("conflict-v");
    rig.add("web", "2.0.0");
    rig.add("web", "2.1.0");
    let daemon = rig.daemon();
    assert_eq!(daemon.order(&rig.update("web", "2.0.0", 3)), succeeded());
    assert_eq!(
        daemon.order(&rig.update("web", "2.1.0", 3)),
        Reply::Refused {
            refusal: Refusal::TryConflict,
            detail: "update 2.1.0 try 3 of web; the step record holds try 3".to_owned(),
        }
    );
    assert_eq!(rig.downloads(), 1);
}

/// Catches a same-try order with another digest being taken for a resend.
#[test]
fn same_try_with_a_different_digest_is_try_conflict() {
    let rig = NativeHost::new("conflict-d");
    rig.add("web", "2.0.0");
    let daemon = rig.daemon();
    assert_eq!(daemon.order(&rig.update("web", "2.0.0", 3)), succeeded());
    let other = digest("cd");
    let entry = Target {
        digest: other.clone(),
        ..rig.entry("web", "2.0.0")
    };
    let order = with_entry(update("web", "2.0.0", &other, 3), &entry);
    let reply = daemon.order(&order);
    assert_eq!(reply.refusal(), Some(Refusal::TryConflict));
    assert_eq!(rig.downloads(), 1);
}

/// Catches a lower try being run (a stale resend from before a coordinator restart), for
/// another package and for the same one.
#[test]
fn a_lower_try_is_stale_try() {
    let rig = NativeHost::new("stale");
    rig.add("web", "2.0.0");
    rig.add("web", "2.1.0");
    let daemon = rig.daemon();
    assert_eq!(daemon.order(&rig.update("web", "2.0.0", 5)), succeeded());
    for version in ["2.1.0", "2.0.0"] {
        let reply = daemon.order(&rig.update("web", version, 4));
        assert_eq!(reply.refusal(), Some(Refusal::StaleTry), "{version}");
    }
    assert_eq!(rig.downloads(), 1);
}

/// Catches a higher try that is not run once the last order ended: it runs, and the record
/// keeps its try.
#[test]
fn a_higher_try_runs_once_the_last_order_ended() {
    let rig = NativeHost::new("higher");
    rig.add("web", "2.0.0");
    rig.add("web", "2.1.0");
    let daemon = rig.daemon();
    assert_eq!(daemon.order(&rig.update("web", "2.0.0", 1)), succeeded());
    assert_eq!(daemon.order(&rig.update("web", "2.1.0", 2)), succeeded());
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
    let rig = NativeHost::new("unfinished");
    rig.add("web", "2.0.0");
    rig.add("web", "3.0.0");
    rig.launcher.lose("web", Step::Cutover);
    let daemon = rig.daemon();
    let lost = daemon.order(&rig.update("web", "2.0.0", 1));
    assert_eq!(lost.refusal(), Some(Refusal::LauncherFailed), "{lost:?}");
    let reply = daemon.order(&rig.update("web", "3.0.0", 2));
    assert_eq!(reply.refusal(), Some(Refusal::Busy), "{reply:?}");
    assert_eq!(daemon.order(&rig.update("web", "2.0.0", 2)), succeeded());
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
    let rig = NativeHost::new("halt");
    rig.seeded("alpha", "1.0.0");
    rig.add("alpha", "2.0.0");
    rig.add("alpha", "2.1.0");
    rig.add("beta", "2.0.0");
    rig.launcher.exits("alpha", Step::Prepare, &[76]);
    let daemon = rig.daemon();
    let halted = daemon.order(&rig.update("alpha", "2.0.0", 1));
    assert_eq!(
        halted,
        Reply::Finished {
            verdict: Verdict::Halt,
            reason: None,
            resent: false
        }
    );
    let later = [
        rig.update("alpha", "2.1.0", 2),
        rig.update("alpha", "2.0.0", 2),
        rig.force_rollback("alpha", "1.0.0", 3),
        rig.update("alpha", "2.1.0", 1),
    ];
    for order in later {
        assert_eq!(
            daemon.order(&order).refusal(),
            Some(Refusal::Halted),
            "{order:?}"
        );
    }
    assert_eq!(
        daemon.order(&rig.update("alpha", "2.0.0", 1)),
        resent(Verdict::Halt)
    );
    assert_eq!(rig.launcher.steps("alpha"), [Step::Prepare]);
    assert_eq!(daemon.order(&rig.update("beta", "2.0.0", 1)), succeeded());
}

/// Catches the daemon running anything but the new package's hook, in another order, or
/// touching the release it replaces: `prepare` (given the version directory), `cutover`,
/// `verify`, `cleanup`, each the new package's hook program in its sealed tree; the release it
/// replaces keeps its digest and is the record's previous release; the verdict is in the reply
/// and the record.
#[test]
fn an_update_runs_the_five_steps_of_the_new_packages_hook_in_order() {
    let rig = NativeHost::new("five");
    let old = rig.seeded("web", "1.0.0");
    let new = rig.add("web", "2.0.0");
    let old_digest = tree_digest(&old).expect("the old tree");
    let daemon = rig.daemon();
    assert_eq!(daemon.order(&rig.update("web", "2.0.0", 1)), succeeded());
    let runs = rig.launcher.runs();
    let steps: Vec<Step> = runs.iter().map(|ran| ran.step).collect();
    assert_eq!(
        steps,
        [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup]
    );
    for ran in &runs {
        let program = new.join("tree").join(hook_path("web"));
        assert_eq!(ran.executable, program, "{ran:?}");
        let artifact = (ran.step == Step::Prepare).then(|| new.clone());
        assert_eq!(ran.artifact, artifact, "{ran:?}");
        assert_eq!(ran.unit, format!("updater-web-{}-1", ran.step));
    }
    assert_eq!(tree_digest(&old).expect("the old tree"), old_digest);
    let rec = record_of(&rig, "web");
    assert_eq!(rec.previous_digest, Some(old_digest));
    assert_eq!(rec.tarball_digest, rig.pin("web", "2.0.0"));
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
    let rig = NativeHost::new("prep-fail");
    rig.seeded("web", "1.0.0");
    rig.add("web", "2.0.0");
    rig.launcher.exits("web", Step::Prepare, &[1]);
    let daemon = rig.daemon();
    assert_eq!(
        daemon.order(&rig.update("web", "2.0.0", 1)),
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
    let rig = NativeHost::new("exits");
    rig.add("alpha", "2.0.0");
    rig.add("beta", "2.0.0");
    rig.launcher.exits("alpha", Step::Prepare, &[75]);
    rig.launcher.exits("beta", Step::Prepare, &[76]);
    let daemon = rig.daemon();
    let deferred = Verdict::Deferred {
        retry_after_secs: None,
    };
    let reply = daemon.order(&rig.update("alpha", "2.0.0", 1));
    assert_eq!(
        reply,
        Reply::Finished {
            verdict: deferred,
            reason: None,
            resent: false
        }
    );
    assert_eq!(rig.launcher.steps("alpha"), [Step::Prepare, Step::Rollback]);
    let reply = daemon.order(&rig.update("beta", "2.0.0", 1));
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
    let rig = NativeHost::new("unstopped");
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
        let (daemon, order) = (Arc::clone(&daemon), rig.update("alpha", "2.0.0", 1));
        spawn("alpha's order", move || daemon.order(&order))
    };
    gate.entered(&first).expect("alpha's prepare runs");
    // Past the bound of `prepare` (600 seconds): the dead-man tries to stop the step. It ticks,
    // then sleeps on the clock: by its second sleep from here, one whole tick has looked past it.
    clock.advance(601);
    let ticked = clock.sleeps(2);
    assert_eq!(
        stops.wait(&ticked).expect("the dead-man's stop"),
        "updater-alpha-prepare-1"
    );
    gate.release();
    let reply = first.join().expect("the order");
    assert_eq!(reply.refusal(), Some(Refusal::Unstopped), "{reply:?}");
    let alpha = name("alpha");
    assert!(daemon.ctx().locks.holder(&alpha).expect("held").stuck);
    let other = daemon.order(&rig.update("alpha", "3.0.0", 2));
    assert_eq!(other.refusal(), Some(Refusal::Busy));
    let resend = daemon.order(&rig.update("alpha", "2.0.0", 1));
    assert_eq!(resend.refusal(), Some(Refusal::Busy), "{resend:?}");
    assert_eq!(daemon.order(&rig.update("beta", "2.0.0", 1)), succeeded());
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
    let rig = NativeHost::new("lock-order");
    rig.add("alpha", "2.0.0");
    rig.add("beta", "2.0.0");
    let alpha_gate = rig.launcher.hold("alpha", Step::Verify);
    let beta_gate = rig.launcher.hold("beta", Step::Prepare);
    let daemon = Arc::new(rig.daemon());
    let order = |service: &'static str, what: &'static str| {
        let (daemon, order) = (Arc::clone(&daemon), rig.update(service, "2.0.0", 1));
        spawn(what, move || daemon.order(&order))
    };
    let alpha = order("alpha", "alpha's order");
    alpha_gate.entered(&alpha).expect("alpha's verify runs");
    let asked = rig.asked();
    let beta = order("beta", "beta's order");
    assert_eq!(
        asked.wait(&beta).expect("beta asks for its package"),
        format!("beta={}", pv("2.0.0"))
    );
    alpha_gate.release();
    beta_gate.entered(&beta).expect("beta's prepare runs");
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

/// Catches an order run over a step record the daemon cannot read, or one a newer daemon wrote
/// (which must never be changed), or a record path it cannot read at all: each is refused with
/// its token and nothing runs.
#[test]
fn a_record_that_cannot_be_read_stops_the_services_orders() {
    let rig = NativeHost::new("records");
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
        let reply = daemon.order(&rig.update(service, "2.0.0", 1));
        assert_eq!(reply.refusal(), Some(refusal), "{service}: {reply:?}");
    }
    assert!(rig.tools().is_empty());
    assert!(rig.launcher.runs().is_empty());
}

/// Catches the wrong release taken as the one an order replaces: when `current` already names
/// the target (the daemon switched before it stopped), the release replaced is `previous`'s;
/// when `current` names a version whose directory is gone, the order is refused
/// `release_unreadable` before anything runs.
#[test]
fn the_release_an_order_replaces_is_current_or_previous() {
    let rig = NativeHost::new("previous");
    let old = rig.seeded("web", "1.0.0");
    rig.seeded("web", "2.0.0");
    let daemon = rig.daemon();
    assert_eq!(daemon.order(&rig.update("web", "2.0.0", 1)), succeeded());
    let rec = record_of(&rig, "web");
    assert_eq!(
        rec.previous_digest,
        Some(tree_digest(&old).expect("digest"))
    );

    rig.add("api", "2.0.0");
    rig.store.link("api", "current", "1.5.0");
    let reply = daemon.order(&rig.update("api", "2.0.0", 1));
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
    let rig = NativeHost::new("readonly");
    rig.add("web", "2.0.0");
    fs::create_dir_all(&rig.state).expect("the state dir");
    std::os::unix::fs::symlink("nowhere", rig.state.join("web")).expect("a dangling link");
    let reply = rig.daemon().order(&rig.update("web", "2.0.0", 1));
    assert_eq!(reply.refusal(), Some(Refusal::StoreFailed), "{reply:?}");
    assert!(rig.launcher.runs().is_empty());
}

/// Catches a version the repository does not list reaching the runner: the order for it ends
/// `Deferred` with the reason `not_in_repository` (the stage's own verdict), and nothing is
/// written.
#[test]
fn a_version_the_repository_does_not_list_is_deferred_and_writes_no_record() {
    let rig = NativeHost::new("unlisted");
    rig.seeded("web", "1.0.0");
    let entry = Target {
        digest: d(),
        package_version: Some(pv("9.9.9")),
        ..rig.entry("web", "1.0.0")
    };
    let order = with_entry(update("web", "9.9.9", &d(), 1), &entry);
    let reply = rig.daemon().order(&order);
    let Reply::Finished {
        verdict: Verdict::Deferred { .. },
        reason,
        resent: false,
    } = &reply
    else {
        panic!("not deferred: {reply:?}");
    };
    assert_eq!(reason.as_deref(), Some("not_in_repository"));
    assert!(no_record(&rig, "web"));
    assert!(rig.launcher.runs().is_empty());
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
    let rig = NativeHost::new("halt-garbage");
    rig.add("web", "2.0.0");
    rig.add("api", "2.0.0");
    fs::create_dir_all(rig.state.join("web")).expect("the service dir");
    fs::write(rig.state.join("web").join("held-halt"), "seven\n").expect("write");
    let daemon = rig.daemon();
    let reply = daemon.order(&rig.update("web", "2.0.0", 1));
    assert_eq!(
        reply,
        Reply::Refused {
            refusal: Refusal::HaltUnreadable,
            detail: "the held halt generation of web is not a number: \"seven\\n\"".to_owned(),
        }
    );
    assert_eq!(daemon.order(&rig.update("api", "2.0.0", 1)), succeeded());
}
