//! The daemon as the executor and the held halts of the heartbeat loop (`hostkeeper_link`): the
//! loop's order reaches `Daemon::order` as the order it names, the daemon's reply is the verdict
//! the loop reports, and the halt generations the loop raises are the ones the executor reads.

mod support;

use std::fs;
use std::num::NonZeroU64;
use std::sync::Arc;

use hostkeeper_config::names::Version;
use hostkeeper_daemon::link_adapters::{
    DaemonHalts, DaemonOrders, MAX_DETAIL, order_result, wire_order,
};
use hostkeeper_daemon::packages::PackageError;
use hostkeeper_daemon::testing::spawn;
use hostkeeper_daemon::verb::{Forced, Hooked, NO_TRY, PackageDigest as WireDigest, Unhooked};
use hostkeeper_daemon::{Refusal, Reply, Verb as Decoded};
use hostkeeper_hook::{Class, Phase, Step, Verdict};
use hostkeeper_link::{
    HeldHalts, Order, OrderId, OrderResult, OrderSink, Outcome, PackageDigest, Reason, Target, Verb,
};
use hostkeeper_steps::runner::DEFAULT_RETRY_SECS;
use support::force::{WEB, link_of, succeeded, web};
use support::{Rig, d, force, name};

fn target(service: &str, version: &str) -> Target {
    Target {
        service: name(service),
        version: Version::new(version).expect("a version"),
        digest: PackageDigest::new(&d()).expect("a package digest"),
    }
}

fn order_id(text: &str) -> OrderId {
    OrderId::new(text).expect("an order id")
}

/// An update of `service` to `version` as the loop carries it.
fn update_order(service: &str, version: &str, order_try: u64) -> Order {
    Order {
        id: order_id(&format!("order-{service}-{version}")),
        order_try,
        verb: Verb::Update(target(service, version)),
    }
}

/// A forced update of `service` to `version` as the loop carries it.
fn force_order(service: &str, version: &str, order_try: u64) -> Order {
    Order {
        id: order_id(&format!("force-{service}-{version}")),
        order_try,
        verb: Verb::Force(target(service, version)),
    }
}

/// The version the loop gives the executor with `order`, which names one: its own.
fn version_of(order: &Order) -> Version {
    order
        .target()
        .expect("an order that names a version")
        .version
        .clone()
}

fn token(text: &str) -> Reason {
    Reason::new(text).expect("a reason token")
}

/// The verdict of `order` that reports nothing but how it ended: no phase, class, reason or
/// detail.
fn plain(order: &Order, outcome: Outcome) -> OrderResult {
    OrderResult {
        order: order.reference(),
        target: version_of(order),
        outcome,
        phase: None,
        class: None,
        reason: None,
        forced: order.is_forced(),
        detail: None,
    }
}

/// Catches a mapping that changes a field of the order on its way to the daemon: an update is
/// decoded by the daemon as an `update` and a forced update as a `force`, each with the order's
/// id, try, service, version and digest as they were, and a `force` with no halt generation.
#[test]
fn a_link_order_is_decoded_by_the_daemon_as_the_verb_and_target_it_names() {
    let hooked = |order_try: u64| Hooked {
        service: name("web"),
        version: Version::new("2.0.0").expect("a version"),
        digest: WireDigest::new(&d()).expect("a digest"),
        order_try: NonZeroU64::new(order_try).expect("a try from 1"),
    };
    let update = wire_order(&update_order("web", "2.0.0", 3));
    assert_eq!(
        (update.order_id.as_str(), update.r#try),
        ("order-web-2.0.0", 3)
    );
    assert_eq!(Decoded::try_from(&update), Ok(Decoded::Update(hooked(3))));
    let forced = wire_order(&force_order("web", "2.0.0", 4));
    assert_eq!(
        (forced.order_id.as_str(), forced.r#try),
        ("force-web-2.0.0", 4)
    );
    assert_eq!(
        Decoded::try_from(&forced),
        Ok(Decoded::Force(Forced {
            package: hooked(4),
            halt_generation: 0,
            order_id: Some("force-web-2.0.0".to_owned()),
        }))
    );
}

/// Catches an install, an uninstall or an abort of the loop that the daemon decodes as another
/// verb, or with a field changed: each is decoded as the verb it names, with the order's
/// service, try, and for an install its version and digest.
#[test]
fn an_install_uninstall_and_abort_are_decoded_as_the_verbs_they_name() {
    let order = |id: &str, verb: Verb| Order {
        id: order_id(id),
        order_try: 2,
        verb,
    };
    let install = wire_order(&order("install-1", Verb::Install(target("web", "2.0.0"))));
    assert_eq!((install.order_id.as_str(), install.r#try), ("install-1", 2));
    assert_eq!(
        Decoded::try_from(&install),
        Ok(Decoded::Install(Hooked {
            service: name("web"),
            version: Version::new("2.0.0").expect("a version"),
            digest: WireDigest::new(&d()).expect("a digest"),
            order_try: NonZeroU64::new(2).expect("a try from 1"),
        }))
    );
    let uninstall = wire_order(&order("uninstall-1", Verb::Uninstall(name("web"))));
    assert_eq!(
        Decoded::try_from(&uninstall),
        Ok(Decoded::Uninstall(Unhooked {
            service: name("web"),
            order_try: NonZeroU64::new(2).expect("a try from 1"),
        }))
    );
    let abort = wire_order(&order("abort-1", Verb::Abort(name("web"))));
    assert_eq!(abort.order_id, "abort-1");
    assert_eq!(Decoded::try_from(&abort), Ok(Decoded::Abort(name("web"))));
}

/// Catches an uninstall's verdict that names no version or one of its own making: the verdict of
/// an order that names none is at the version the loop gave, whatever the daemon answered.
#[test]
fn an_uninstalls_verdict_names_the_version_the_loop_gave() {
    let removal = Order {
        id: order_id("uninstall-1"),
        order_try: 1,
        verb: Verb::Uninstall(name("web")),
    };
    let at = Version::new("1.4.0").expect("a version");
    let done = Reply::Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
        resent: false,
    };
    let result = order_result(&removal, &at, &done);
    assert_eq!(result.target, at);
    assert_eq!(result.outcome, Outcome::Succeeded);
    assert_eq!(
        order_result(&removal, &at, &Reply::Running),
        OrderResult::busy(&removal, at)
    );
}

/// Catches an order that does not reach `Daemon::order`, or a reply that is not the verdict of
/// the order: an update through the adapter makes the daemon fetch the package and run the five
/// steps, writes the order's try and digest in the step record, and is reported `succeeded` with
/// the order's id, try and version; the same order again is answered from the record, not run
/// twice.
#[test]
fn an_order_in_the_reply_reaches_daemon_order_and_its_reply_is_the_next_last_update() {
    let rig = Rig::new("link-update");
    rig.add("web", "2.0.0");
    let orders = DaemonOrders::new(Arc::new(rig.daemon()));
    let order = update_order("web", "2.0.0", 3);
    let result = orders.execute(&order, &version_of(&order));
    assert_eq!(result, plain(&order, Outcome::Succeeded));
    assert_eq!(result.order.id.as_str(), "order-web-2.0.0");
    assert_eq!(result.order.order_try, 3);
    assert_eq!(rig.store.asked(), [("web".to_owned(), "2.0.0".to_owned())]);
    let steps = [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup];
    assert_eq!(rig.launcher.steps("web"), steps);
    let rec = rig.record("web").expect("the step record");
    assert_eq!(
        (
            rec.service.as_str(),
            rec.target_version.as_str(),
            rec.tarball_digest.as_str(),
            rec.order_try
        ),
        ("web", "2.0.0", d().as_str(), Some(3))
    );
    assert_eq!(orders.execute(&order, &version_of(&order)), result);
    assert_eq!(rig.launcher.steps("web"), steps);
}

/// Catches a refusal that loses its token, its text or the order's try, and a resend of the
/// running order that is taken for a new one: while `alpha`'s `prepare` runs, another order of
/// it is reported `failed` with the reason `busy` and the daemon's text, under its own id and
/// try; the running order again is answered `busy` with no text, as the loop answers an order
/// that arrives while another runs; and the first order still ends `succeeded`.
#[test]
fn a_busy_order_and_a_running_one_are_reported_failed_busy_under_their_own_id_and_try() {
    let rig = Rig::new("link-busy");
    rig.add("alpha", "2.0.0");
    rig.add("alpha", "3.0.0");
    let gate = rig.launcher.hold("alpha", Step::Prepare);
    let orders = Arc::new(DaemonOrders::new(Arc::new(rig.daemon())));
    let first = update_order("alpha", "2.0.0", 1);
    let running = {
        let (orders, first) = (Arc::clone(&orders), first.clone());
        spawn("the order", move || {
            orders.execute(&first, &version_of(&first))
        })
    };
    gate.entered(&running).expect("the held step is entered");
    let other = update_order("alpha", "3.0.0", 2);
    assert_eq!(
        orders.execute(&other, &version_of(&other)),
        OrderResult {
            reason: Some(token("busy")),
            detail: Some("alpha is locked by an order to update it to 2.0.0".to_owned()),
            ..plain(&other, Outcome::Failed)
        }
    );
    assert_eq!(
        orders.execute(&first, &version_of(&first)),
        OrderResult::busy(&first, version_of(&first))
    );
    gate.release();
    assert_eq!(
        running.join().expect("the first order"),
        plain(&first, Outcome::Succeeded)
    );
}

/// Catches a standing halt reported as a plain failure, a halt verdict that loses its phase, or a
/// refusal re-reporting the halt: a `prepare` that exits 76 is reported `failed` in the phase
/// `halt` with no class; the next order of the service is refused `halted`, reported `failed` in
/// no phase with the reason `halted`, the daemon's text and its own id and try.
#[test]
fn a_halt_is_reported_in_the_halt_phase_and_the_orders_it_refuses_are_not() {
    let rig = Rig::new("link-halted");
    rig.running("web", "1.0.0");
    rig.add("web", "2.0.0");
    rig.add("web", "2.1.0");
    rig.launcher.exits("web", Step::Prepare, &[76]);
    let orders = DaemonOrders::new(Arc::new(rig.daemon()));
    let first = update_order("web", "2.0.0", 1);
    assert_eq!(
        orders.execute(&first, &version_of(&first)),
        OrderResult {
            phase: Some(Phase::Halt),
            ..plain(&first, Outcome::Failed)
        }
    );
    let second = update_order("web", "2.1.0", 2);
    assert_eq!(
        orders.execute(&second, &version_of(&second)),
        OrderResult {
            reason: Some(token("halted")),
            detail: Some("update 2.1.0 try 2 of web; the step record holds try 1".to_owned()),
            ..plain(&second, Outcome::Failed)
        }
    );
    assert_eq!(rig.launcher.steps("web"), [Step::Prepare]);
}

/// Catches a verdict reported in a shape of its own: a `prepare` that exits 75 is reported
/// `deferred` with the runner's default retry time and no phase, class or reason, and one that
/// exits 1 is reported `failed` before the commit, blamed on the release.
#[test]
fn a_deferral_and_a_failure_keep_their_outcome_phase_and_class() {
    let rig = Rig::new("link-verdicts");
    rig.add("alpha", "2.0.0");
    rig.add("beta", "2.0.0");
    rig.launcher.exits("alpha", Step::Prepare, &[75]);
    rig.launcher.exits("beta", Step::Prepare, &[1]);
    let orders = DaemonOrders::new(Arc::new(rig.daemon()));
    let deferred = update_order("alpha", "2.0.0", 1);
    assert_eq!(
        orders.execute(&deferred, &version_of(&deferred)),
        plain(
            &deferred,
            Outcome::Deferred {
                retry_after_secs: DEFAULT_RETRY_SECS
            }
        )
    );
    let failed = update_order("beta", "2.0.0", 1);
    assert_eq!(
        orders.execute(&failed, &version_of(&failed)),
        OrderResult {
            phase: Some(Phase::BeforeCommit),
            class: Some(Class::Release),
            ..plain(&failed, Outcome::Failed)
        }
    );
}

/// Catches a step the launcher lost being reported as a verdict of the order, or as no failure:
/// the daemon's interrupted reply is `failed` with the reason `launcher_failed` and the
/// daemon's text, in no phase and of no class.
#[test]
fn an_order_the_runner_stopped_without_a_verdict_is_reported_failed_with_its_token() {
    let rig = Rig::new("link-interrupted");
    rig.add("web", "2.0.0");
    rig.launcher.lose("web", Step::Prepare);
    let orders = DaemonOrders::new(Arc::new(rig.daemon()));
    let order = update_order("web", "2.0.0", 1);
    let result = orders.execute(&order, &version_of(&order));
    let detail = result.detail.clone().expect("the daemon's text");
    assert!(!detail.is_empty());
    assert_eq!(
        result,
        OrderResult {
            reason: Some(token("launcher_failed")),
            detail: Some(detail),
            ..plain(&order, Outcome::Failed)
        }
    );
}

/// Catches an order with a try of 0 reaching a lock or the disk, or being dropped: the daemon
/// refuses it as malformed, naming the try, and the adapter reports it `failed` with the reason
/// `invalid_order` and that text.
#[test]
fn an_order_the_daemon_cannot_decode_is_reported_failed_invalid_order() {
    let rig = Rig::new("link-invalid");
    let orders = DaemonOrders::new(Arc::new(rig.daemon()));
    let order = update_order("web", "2.0.0", 0);
    assert_eq!(
        orders.execute(&order, &version_of(&order)),
        OrderResult {
            reason: Some(token("invalid_order")),
            detail: Some(format!("the order's try {NO_TRY}")),
            ..plain(&order, Outcome::Failed)
        }
    );
    assert!(!rig.state.exists());
}

/// Catches a forced order run as a hooked update, or its verdict not marked forced: a forced
/// update through the adapter runs no hook step, switches `current`, and the daemon's force
/// record holds the order's id, try, version and digest with a halt generation of 0; the verdict
/// is `succeeded` and forced.
#[test]
fn a_forced_order_reaches_the_daemon_as_a_force_and_its_verdict_is_forced() {
    let rig = Rig::new("link-force");
    web(&rig, "2.0.0");
    let (daemon, _) = rig.booted();
    let orders = DaemonOrders::new(Arc::new(daemon));
    let order = force_order(WEB, "2.0.0", 4);
    let result = orders.execute(&order, &version_of(&order));
    assert_eq!(result, plain(&order, Outcome::Succeeded));
    assert!(result.forced);
    let force = rig
        .record(WEB)
        .and_then(|rec| rec.force)
        .expect("the force record");
    assert_eq!(
        (
            force.order_id.as_deref(),
            force.order_try,
            force.target_version.as_str(),
            force.target_digest.as_str(),
            force.halt_generation
        ),
        (Some("force-web-2.0.0"), 4, "2.0.0", d().as_str(), 0)
    );
    assert_eq!(link_of(&rig, WEB, "current").as_deref(), Some("2.0.0"));
    assert!(rig.launcher.runs().is_empty());
}

/// Catches a failed force reported as a hooked failure: a package the store refuses fails the
/// force before the commit, blamed on the release, with the store's reason `digest_mismatch`;
/// the verdict is forced.
#[test]
fn a_forced_order_that_fails_before_the_switch_is_reported_failed_and_forced() {
    let rig = Rig::new("link-force-failed");
    rig.store.fail(
        WEB,
        "2.0.0",
        PackageError::DigestMismatch("other bytes".to_owned()),
    );
    let orders = DaemonOrders::new(Arc::new(rig.daemon()));
    let order = force_order(WEB, "2.0.0", 1);
    let result = orders.execute(&order, &version_of(&order));
    assert!(result.forced);
    assert_eq!(
        result,
        OrderResult {
            phase: Some(Phase::BeforeCommit),
            class: Some(Class::Release),
            reason: Some(token("digest_mismatch")),
            ..plain(&order, Outcome::Failed)
        }
    );
}

/// Catches a refusal reported under another token, or with another phase: every refusal of the
/// daemon, refused or interrupted, is reported `failed` with its own token as the reason and its
/// text as the detail, in no phase, whether the order is hooked or forced.
#[test]
fn every_refusal_is_reported_under_its_own_token() {
    for order in [
        update_order("web", "2.0.0", 2),
        force_order("web", "2.0.0", 2),
    ] {
        for refusal in Refusal::ALL {
            let replies = [
                Reply::refused(refusal, "why"),
                Reply::Interrupted {
                    refusal,
                    detail: "why".to_owned(),
                },
            ];
            for reply in replies {
                let expected = OrderResult {
                    reason: Some(token(refusal.token())),
                    detail: Some("why".to_owned()),
                    ..plain(&order, Outcome::Failed)
                };
                assert_eq!(
                    order_result(&order, &version_of(&order), &reply),
                    expected,
                    "{reply:?}"
                );
            }
        }
    }
}

/// Catches a verdict mapped to another outcome, phase, class or retry time, or one that depends
/// on whether the reply was a resend: each verdict of the hook contract, with and without a
/// reason, hooked and forced, resent or not, is the outcome, phase, class, reason and retry time
/// the loop reports; a deferral with no retry time gets the runner's default, and a halt is
/// `failed` in the phase `halt` with no class.
#[test]
fn a_finished_order_is_reported_as_its_verdict() {
    let rows = [
        (
            Verdict::Succeeded {
                promotion_pending: false,
            },
            None,
            (Outcome::Succeeded, None, None),
        ),
        (
            Verdict::Succeeded {
                promotion_pending: true,
            },
            None,
            (Outcome::Succeeded, None, None),
        ),
        (
            Verdict::Failed {
                phase: Phase::GateRelease,
                class: Class::Environment,
            },
            Some("disk_full"),
            (
                Outcome::Failed,
                Some(Phase::GateRelease),
                Some(Class::Environment),
            ),
        ),
        (
            Verdict::Failed {
                phase: Phase::AfterCommit,
                class: Class::Release,
            },
            None,
            (
                Outcome::Failed,
                Some(Phase::AfterCommit),
                Some(Class::Release),
            ),
        ),
        (
            Verdict::Deferred {
                retry_after_secs: Some(300),
            },
            Some("drain_bound"),
            (
                Outcome::Deferred {
                    retry_after_secs: 300,
                },
                None,
                None,
            ),
        ),
        (
            Verdict::Deferred {
                retry_after_secs: None,
            },
            None,
            (
                Outcome::Deferred {
                    retry_after_secs: DEFAULT_RETRY_SECS,
                },
                None,
                None,
            ),
        ),
        (
            Verdict::Halt,
            Some("state_invalid"),
            (Outcome::Failed, Some(Phase::Halt), None),
        ),
    ];
    for order in [
        update_order("web", "2.0.0", 1),
        force_order("web", "2.0.0", 1),
    ] {
        for (verdict, reason, (outcome, phase, class)) in &rows {
            for resent in [false, true] {
                let reply = Reply::Finished {
                    verdict: verdict.clone(),
                    reason: reason.map(str::to_owned),
                    resent,
                };
                let expected = OrderResult {
                    phase: *phase,
                    class: *class,
                    reason: reason.map(token),
                    ..plain(&order, *outcome)
                };
                assert_eq!(
                    order_result(&order, &version_of(&order), &reply),
                    expected,
                    "{reply:?}"
                );
            }
        }
    }
}

/// Catches a reason that is not a token being reported, or panicking the loop, or vanishing
/// without a trace: the reasons the grammar refuses (spaces, capitals, empty, over 64 bytes) are
/// left out of the verdict, which keeps its outcome, phase and class, and the detail says which
/// text was refused, by the grammar's own words.
#[test]
fn a_reason_that_is_not_a_token_is_left_out_and_said_in_the_detail() {
    let long = "a".repeat(65);
    for text in ["Not A Token", "", "has space", "UPPER", long.as_str()] {
        let order = update_order("web", "2.0.0", 1);
        let reply = Reply::Finished {
            verdict: Verdict::Failed {
                phase: Phase::BeforeCommit,
                class: Class::Release,
            },
            reason: Some(text.to_owned()),
            resent: false,
        };
        assert_eq!(
            order_result(&order, &version_of(&order), &reply),
            OrderResult {
                phase: Some(Phase::BeforeCommit),
                class: Some(Class::Release),
                detail: Some(format!(
                    "{text:?} is not a reason: 1 to 64 bytes of a-z, 0-9 and `_`"
                )),
                ..plain(&order, Outcome::Failed)
            }
        );
    }
}

/// Catches a detail that is sent whole or cut inside a character: the daemon's text is kept up
/// to 512 bytes and cut at a character boundary, whether it is a refusal's text or the note of a
/// refused reason; one of exactly 512 bytes is kept.
#[test]
fn a_detail_is_cut_to_512_bytes_on_a_character_boundary() {
    assert_eq!(MAX_DETAIL, 512);
    let order = update_order("web", "2.0.0", 1);
    let refused = |detail: String| {
        order_result(
            &order,
            &version_of(&order),
            &Reply::Refused {
                refusal: Refusal::StoreFailed,
                detail,
            },
        )
        .detail
    };
    assert_eq!(refused("é".repeat(300)), Some("é".repeat(256)));
    assert_eq!(
        refused(format!("{}é", "a".repeat(511))),
        Some("a".repeat(511))
    );
    assert_eq!(refused("a".repeat(512)), Some("a".repeat(512)));
    assert_eq!(refused(String::new()), Some(String::new()));
    let reply = Reply::Finished {
        verdict: Verdict::Halt,
        reason: Some("A".repeat(1000)),
        resent: false,
    };
    let note = order_result(&order, &version_of(&order), &reply)
        .detail
        .expect("the note");
    assert_eq!(note.len(), MAX_DETAIL);
    assert!(note.starts_with("\"AAAA"), "{note}");
}

/// Catches a resend of the running order being reported as a failure of its own, or losing the
/// forced mark: the reply `Running` is the loop's `busy` answer for the order, hooked or forced.
#[test]
fn a_running_order_is_answered_busy() {
    for order in [
        update_order("web", "2.0.0", 1),
        force_order("web", "2.0.0", 1),
    ] {
        let busy = order_result(&order, &version_of(&order), &Reply::Running);
        assert_eq!(busy, OrderResult::busy(&order, version_of(&order)));
        assert_eq!(busy.reason, Some(token("busy")));
        assert_eq!(busy.forced, order.is_forced());
        assert_eq!(busy.detail, None);
    }
}

/// Catches a halt generation raised into a store of its own, a lower one lowering the hold or
/// failing, and a forced order unable to read what the loop raised: a generation raised through
/// the adapter is what the daemon's store and the adapter read back; a lower one is `Ok` and
/// changes nothing (the file included); a force whose generation is below it lifts nothing and
/// is not refused for it; and a force above it lifts the halt and raises the hold, which the
/// adapter reads back.
#[test]
fn a_halt_generation_in_a_reply_is_the_one_a_force_reads() {
    let rig = Rig::new("link-halts");
    rig.add(WEB, "1.0.0");
    rig.add_units(WEB, "3.0.0", &[], &[], &[]);
    rig.add_units(WEB, "4.0.0", &[], &[], &[]);
    rig.launcher.exits(WEB, Step::Prepare, &[76]);
    let (daemon, _) = rig.booted();
    let daemon = Arc::new(daemon);
    let orders = DaemonOrders::new(Arc::clone(&daemon));
    let halts = DaemonHalts::new(Arc::clone(&daemon));
    let service = name(WEB);
    let halted = update_order(WEB, "1.0.0", 1);
    assert_eq!(
        orders.execute(&halted, &version_of(&halted)),
        OrderResult {
            phase: Some(Phase::Halt),
            ..plain(&halted, Outcome::Failed)
        }
    );
    assert_eq!(halts.held(&service), 0);

    halts.raise(&service, 5).expect("raise to 5");
    assert_eq!(daemon.ctx().halts.get(&service).ok(), Some(5));
    assert_eq!(halts.held(&service), 5);

    halts
        .raise(&service, 2)
        .expect("a lower generation is not an error");
    halts.raise(&service, 5).expect("the same generation again");
    assert_eq!(daemon.ctx().halts.get(&service).ok(), Some(5));
    assert_eq!(halts.held(&service), 5);
    let file = rig.state.join(WEB).join("held-halt");
    assert_eq!(fs::read_to_string(file).expect("the file"), "5\n");

    assert_eq!(daemon.order(&force(WEB, "3.0.0", &d(), 2, 4)), succeeded());
    assert_eq!(halts.held(&service), 5);
    assert_eq!(daemon.order(&force(WEB, "4.0.0", &d(), 3, 6)), succeeded());
    assert_eq!(daemon.ctx().halts.get(&service).ok(), Some(6));
    assert_eq!(halts.held(&service), 6);
}

/// Catches a store that cannot be read being read as no halt, or a raise on it that lifts or
/// hides the fault: a held-halt file of garbage or a directory in its place holds every
/// generation (so the loop raises nothing above it) and refuses a raise with the store's own
/// text, leaving the file as it was; a service directory that cannot be made refuses a raise
/// with the store's text; and a service with no file holds 0.
#[test]
fn a_store_that_cannot_be_read_or_written_holds_every_generation_and_refuses_a_raise() {
    let rig = Rig::new("link-halts-bad");
    let halts = DaemonHalts::new(Arc::new(rig.daemon()));
    let garbage = rig.state.join("web");
    fs::create_dir_all(&garbage).expect("the service's directory");
    fs::write(garbage.join("held-halt"), "seven\n").expect("the garbage");
    assert_eq!(halts.held(&name("web")), u64::MAX);
    let error = halts.raise(&name("web"), 9).expect_err("a garbage file");
    assert_eq!(
        error.to_string(),
        "the held halt generation of web is not a number: \"seven\\n\""
    );
    assert_eq!(
        fs::read_to_string(garbage.join("held-halt")).expect("the file"),
        "seven\n"
    );

    fs::create_dir_all(rig.state.join("api").join("held-halt")).expect("a directory in the way");
    assert_eq!(halts.held(&name("api")), u64::MAX);
    let error = halts.raise(&name("api"), 1).expect_err("a directory");
    assert!(
        error
            .to_string()
            .starts_with("the held halt generation of api cannot be read or written: "),
        "{error}"
    );

    std::os::unix::fs::symlink("nowhere", rig.state.join("db")).expect("a dangling link");
    assert_eq!(halts.held(&name("db")), 0);
    let error = halts.raise(&name("db"), 1).expect_err("no directory");
    assert!(
        error
            .to_string()
            .starts_with("the held halt generation of db cannot be read or written: "),
        "{error}"
    );
    assert_eq!(halts.held(&name("other")), 0);
}
