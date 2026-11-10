//! The update of hostkeeper itself through to the coordinator, in process: the real coordinator
//! (its heartbeat with typed orders, its verdicts file and its bake), the real heartbeat loop over
//! the daemon, the real executor, and the daemon's own observer over a fake service manager and
//! fake processes, with the fake of apt and dpkg answering the daemon's tool runs. The daemon runs
//! as the unit `hostkeeper.service` its package names, from the binary directory's `current`.
//! The coordinator orders hostkeeper's release, whose entry CI set ([`set_release`]).
//!
//! What these hold the daemon to: after its self-update, the binary that settles it reports the
//! update's verdict in its first heartbeat, in place of whatever older verdict the coordinator
//! still holds for the host, and reports its own service live, healthy and at the version it
//! runs, so that the coordinator bakes the host and reads hostkeeper converged.

mod support;

use std::fs;
use std::sync::Arc;

use hostkeeper_config::names::ServiceName;
use hostkeeper_daemon::Daemon;
use hostkeeper_daemon::link_adapters::{
    DaemonObserver, DaemonRecords, OWN_UNIT_NOT_LOADED, recorded,
};
use hostkeeper_daemon::own_package::OwnActivate;
use hostkeeper_link::{
    Entry, Health, Liveness, Order, OrderId, OrderRef, Outcome, PackageDigest, ServiceObserver,
    ServiceRecords, Target, Verb,
};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_steps::Settled;
use hostkeeper_steps::record::{Activated, RECORD_FILE};
use support::Scratch;
use support::coordinated::{Host, convergence_of, coordinator_over, hk_on_host, order_and_try};
use support::selfupdate::{DEB_UNIT_NAME, HK, LATER, NEW, OLD, SelfRig};
use support::typed::set_release;

/// The unit the daemon runs as: the one hostkeeper's package names.
fn own_unit() -> &'static str {
    DEB_UNIT_NAME
}

/// How many healthy beats at its version the rollout asks of a host before its stage is done.
const BAKE: &str = "bake_beats = 2";

/// The verdict the coordinator holds as the host's last update of hostkeeper: its target,
/// outcome, reason, order id and try.
fn last_update(
    coordinator: &hostkeeper_coordinator::service::Coordinator,
) -> (String, i32, String, String, Option<u64>) {
    let last = hk_on_host(coordinator)
        .last_update
        .expect("a last update of hostkeeper");
    (
        last.target,
        last.outcome,
        last.reason,
        last.order_id.unwrap_or_default(),
        last.order_try,
    )
}

/// hostkeeper at `version` as an update or an install of the release published on `s` names it:
/// the digest the repository pinned, and the entry CI set.
fn hostkeeper_at(s: &SelfRig, version: &str) -> Target {
    let entry = s.entry_of(HK, version);
    Target {
        service: hk(),
        version: hostkeeper_config::names::Version::new(version).expect("a version"),
        digest: PackageDigest::new(&entry.digest).expect("a digest"),
        index_digest: None,
        entry: Some(Entry::new(entry)),
    }
}

/// The start of the daemon `daemon` as far as the self-update goes: its settlement, and, once the
/// new binary confirmed itself, the `activate` of its package, which ends the update.
fn settled(daemon: &Daemon) -> Settled {
    let settled = daemon.settle_self_update().expect("settled");
    if settled == Settled::Confirmed {
        assert_eq!(
            daemon.activate_own_package(),
            Some(OwnActivate::Ended(Activated::Succeeded { reason: None }))
        );
    }
    settled
}

/// The order of the answer `n` (from 0).
fn order_of(host: &Host, n: usize) -> Order {
    host.ordered()[n].clone().expect("an order")
}

/// Catches the farm's stalled rollout: a host failed hostkeeper's update to [`NEW`]
/// (`self_binary_missing`), and the coordinator kept that verdict as the host's. Ordered
/// [`LATER`], the daemon of [`OLD`] ran the self-update and exited before its `Deferred` reached
/// a heartbeat, and the binary of [`LATER`] settled it `Succeeded` but reported no verdict, so
/// the coordinator kept reading the failure as the host's last update, and the host never baked
/// and the rollout never converged. Now the first heartbeat of the binary of
/// [`LATER`] carries the success of the order it settled, under that order's id and try, which
/// the coordinator holds in place of the failure; hostkeeper is reported healthy at [`LATER`],
/// confirmed by its running executable, and the coordinator reads it converged once the host
/// has beaten healthy twice.
#[test]
fn a_self_update_reports_its_success_over_an_older_failure_bakes_and_converges() {
    let s = SelfRig::recorded("converge-mac");
    s.release(LATER);
    let dir = Scratch::new("converge-mac-coordinator");
    let coordinator = coordinator_over(dir.path(), HK, BAKE);
    // A release that holds no hostkeeper binary: the update fails before the commit.
    set_release(&coordinator, HK, NEW, &s.release_without_binary(NEW));
    s.runs(own_unit(), OLD, 100);
    let daemon = Arc::new(s.daemon_running_as(OLD, own_unit()));
    let mut host = Host::of(Arc::clone(&daemon), &coordinator, None);

    host.beat();
    assert_eq!(
        order_and_try(&host, 0),
        (Verb::Update(hostkeeper_at(&s, NEW)), 1)
    );
    assert!(host.dispatch.run_next());
    host.beat();
    let failed = order_of(&host, 0);
    assert_eq!(
        last_update(&coordinator),
        (
            NEW.to_owned(),
            pb::Outcome::Failed as i32,
            "self_binary_missing".to_owned(),
            failed.id.as_str().to_owned(),
            Some(1)
        )
    );
    assert_eq!(hk_on_host(&coordinator).health, pb::Health::Healthy as i32);

    set_release(&coordinator, HK, LATER, &s.entry_of(HK, LATER));
    host.beat();
    let ordered = order_of(&host, 2);
    assert_eq!(ordered.verb, Verb::Update(hostkeeper_at(&s, LATER)));
    assert!(host.dispatch.run_next());
    assert_eq!(s.layout().next().expect("next").as_deref(), Some(LATER));
    assert!(daemon.ctx().restart().requested());
    // The daemon exits before its `Deferred` reaches a heartbeat.
    drop(host);

    s.runs(own_unit(), LATER, 101);
    let daemon = Arc::new(s.daemon_running_as(LATER, own_unit()));
    assert_eq!(settled(&daemon), Settled::Confirmed);
    let mut host = Host::of(Arc::clone(&daemon), &coordinator, None);
    host.beat();
    assert_eq!(
        last_update(&coordinator),
        (
            LATER.to_owned(),
            pb::Outcome::Succeeded as i32,
            String::new(),
            ordered.id.as_str().to_owned(),
            Some(ordered.order_try)
        )
    );
    let hk = hk_on_host(&coordinator);
    assert_eq!(
        (hk.health, hk.up, hk.installed_version.as_deref()),
        (pb::Health::Healthy as i32, true, Some(LATER)),
        "{hk:?}"
    );
    // One healthy beat of the two the rollout asks.
    let convergence = convergence_of(&coordinator, HK);
    assert_eq!(convergence["state"], "in_progress", "{convergence}");

    host.beat();
    let convergence = convergence_of(&coordinator, HK);
    assert_eq!(convergence["state"], "converged", "{convergence}");
    assert_eq!(host.ordered(), [None, None]);
    // Acknowledged, the success stands: no beat brings the older failure back.
    host.beat();
    assert_eq!(last_update(&coordinator).1, pb::Outcome::Succeeded as i32);

    let status = daemon.status().expect("status");
    let own = status
        .services
        .iter()
        .find(|service| service.service.as_str() == HK)
        .expect("hostkeeper in the status");
    let seen = own.observation.as_ref().expect("observed");
    assert_eq!(
        (seen.state.clone(), seen.reason_token()),
        (hostkeeper_observe::ServiceState::Live, None)
    );
}

/// Catches the self-update of a host the deb installed (no package of hostkeeper recorded, so it
/// is ordered an `install`) whose success never reaches the coordinator: the binary of [`NEW`]
/// settles the install `Succeeded` and its first heartbeat carries that verdict, under the
/// install's id and try 1, and the coordinator reads hostkeeper converged.
#[test]
fn a_host_the_deb_installed_reports_the_install_of_its_release_and_converges() {
    let s = SelfRig::bootstrapped("converge-pkg");
    let dir = Scratch::new("converge-pkg-coordinator");
    let coordinator = coordinator_over(dir.path(), HK, BAKE);
    set_release(&coordinator, HK, NEW, &s.entry_of(HK, NEW));
    s.runs(own_unit(), OLD, 100);
    let mut host = Host::of(
        Arc::new(s.daemon_running_as(OLD, own_unit())),
        &coordinator,
        None,
    );
    host.beat();
    assert_eq!(
        order_and_try(&host, 0),
        (Verb::Install(hostkeeper_at(&s, NEW)), 1)
    );
    assert!(host.dispatch.run_next());
    let install = order_of(&host, 0);
    drop(host);

    s.runs(own_unit(), NEW, 101);
    let daemon = Arc::new(s.daemon_running_as(NEW, own_unit()));
    assert_eq!(settled(&daemon), Settled::Confirmed);
    let mut host = Host::of(daemon, &coordinator, None);
    host.beat();
    host.beat();
    assert_eq!(
        last_update(&coordinator),
        (
            NEW.to_owned(),
            pb::Outcome::Succeeded as i32,
            String::new(),
            install.id.as_str().to_owned(),
            Some(1)
        )
    );
    let convergence = convergence_of(&coordinator, HK);
    assert_eq!(convergence["state"], "converged", "{convergence}");
}

/// Catches the daemon whose own unit the service manager no longer loads (a unit link an earlier
/// move left hides the package's) heartbeating its own service absent: the coordinator read
/// that as no package installed and ordered the install of the release that runs, which the
/// daemon refused `already_installed`. Through the real heartbeat loop and coordinator, a host
/// at hostkeeper's desired release whose unit becomes not found sends its own service
/// unhealthy at the version it runs, with the detail `own_unit_not_loaded` from the daemon's
/// observer, and is ordered nothing.
#[test]
fn a_daemon_whose_own_unit_is_not_found_reports_it_unhealthy_and_is_ordered_nothing() {
    let s = SelfRig::bootstrapped("converge-own-unit-gone");
    let dir = Scratch::new("converge-own-unit-gone-coordinator");
    let coordinator = coordinator_over(dir.path(), HK, BAKE);
    set_release(&coordinator, HK, NEW, &s.entry_of(HK, NEW));
    s.runs(own_unit(), OLD, 100);
    let mut host = Host::of(
        Arc::new(s.daemon_running_as(OLD, own_unit())),
        &coordinator,
        None,
    );
    host.beat();
    assert!(host.dispatch.run_next());
    drop(host);
    s.runs(own_unit(), NEW, 101);
    let daemon = Arc::new(s.daemon_running_as(NEW, own_unit()));
    assert_eq!(settled(&daemon), Settled::Confirmed);
    let mut host = Host::of(Arc::clone(&daemon), &coordinator, None);
    host.beat();
    host.beat();
    assert_eq!(convergence_of(&coordinator, HK)["state"], "converged");

    s.rig
        .manager
        .set(own_unit(), hostkeeper_observe::ActiveState::NotFound, None);
    let observed = DaemonObserver::new(Arc::clone(&daemon)).observe(&hk());
    assert_eq!(
        (
            observed.liveness,
            observed.health,
            observed.health_detail.as_deref()
        ),
        (
            Liveness::Running,
            Health::Unhealthy,
            Some(OWN_UNIT_NOT_LOADED)
        )
    );
    let from = host.ordered().len();
    host.beat();
    host.beat();

    let sent = host.sent.lock().expect("the log is not poisoned").clone();
    let own = sent
        .last()
        .expect("a heartbeat")
        .services
        .iter()
        .find(|service| service.name == HK)
        .cloned()
        .expect("hostkeeper in the heartbeat");
    assert_eq!(
        (own.health, own.version.as_str()),
        (pb::Health::Unhealthy as i32, NEW)
    );
    assert_eq!(host.ordered()[from..], [None, None]);
    let on_host = hk_on_host(&coordinator);
    assert_eq!(on_host.health, pb::Health::Unhealthy as i32, "{on_host:?}");
    assert_eq!(
        on_host.last_update.map(|last| last.reason),
        Some(String::new())
    );
}

/// Catches a fallback the coordinator never hears of, whichever way the wait was written: the
/// binary that starts after the hand-over is the old one (the new one failed its tries), settled
/// by the old binary at its start; or, for an update a release before the hand-over made the old
/// way (`current` switched first, the previous binary put back), settled by the previous binary
/// at its start; either way the record is `Failed` after the commit, of class release,
/// `self_update_fallback`, and the first heartbeat of the old binary carries that verdict under
/// the update's id and try.
#[test]
fn a_self_update_that_fell_back_reports_the_fallback_under_its_order() {
    for old_way in [true, false] {
        let tag = format!("converge-fallback-{old_way}");
        let s = SelfRig::recorded(&tag);
        let dir = Scratch::new(&format!("{tag}-coordinator"));
        let coordinator = coordinator_over(dir.path(), HK, BAKE);
        set_release(&coordinator, HK, NEW, &s.entry_of(HK, NEW));
        s.runs(own_unit(), OLD, 100);
        let mut host = Host::of(
            Arc::new(s.daemon_running_as(OLD, own_unit())),
            &coordinator,
            None,
        );
        host.beat();
        assert!(host.dispatch.run_next());
        let update = order_of(&host, 0);
        drop(host);
        if old_way {
            s.switch_the_old_way(NEW);
        }

        let daemon = Arc::new(s.daemon_running_as(OLD, own_unit()));
        if old_way {
            s.layout().switch(OLD).expect("the previous binary back");
        }
        let settled = settled(&daemon);
        assert_eq!(settled, Settled::FellBack, "{tag}");
        let mut host = Host::of(daemon, &coordinator, None);
        host.beat();
        assert_eq!(
            last_update(&coordinator),
            (
                NEW.to_owned(),
                pb::Outcome::Failed as i32,
                "self_update_fallback".to_owned(),
                update.id.as_str().to_owned(),
                Some(1)
            ),
            "{tag}"
        );
        let reported = hk_on_host(&coordinator).last_update.expect("the verdict");
        assert_eq!(
            (reported.phase, reported.class),
            (
                pb::FailurePhase::AfterCommit as i32,
                pb::FailureClass::Release as i32
            ),
            "{tag}"
        );
    }
}

/// Catches a settled verdict lost to a restart before the coordinator heard it, or sent again
/// after it was acknowledged (in place of a newer verdict the coordinator holds): the binary of
/// [`NEW`] settles the update and is restarted before any heartbeat; the start after it sends
/// the success under the update's id; once the coordinator acknowledged it, the record keeps it
/// no longer, and no later start lists it again.
#[test]
fn the_settled_verdict_is_sent_after_each_restart_until_it_is_acknowledged() {
    let s = SelfRig::recorded("converge-restart");
    let dir = Scratch::new("converge-restart-coordinator");
    let coordinator = coordinator_over(dir.path(), HK, BAKE);
    set_release(&coordinator, HK, NEW, &s.entry_of(HK, NEW));
    s.runs(own_unit(), OLD, 100);
    let mut host = Host::of(
        Arc::new(s.daemon_running_as(OLD, own_unit())),
        &coordinator,
        None,
    );
    host.beat();
    assert!(host.dispatch.run_next());
    let update = order_of(&host, 0);
    drop(host);

    s.runs(own_unit(), NEW, 101);
    let first = s.daemon_running_as(NEW, own_unit());
    assert_eq!(settled(&first), Settled::Confirmed);
    drop(first);
    let daemon = Arc::new(s.daemon_running_as(NEW, own_unit()));
    assert_eq!(settled(&daemon), Settled::NotPending);
    let listed = recorded(daemon.ctx(), hk()).expect("listed");
    let kept = listed.last_result.expect("the verdict the record keeps");
    assert_eq!(
        (kept.order.id.as_str(), kept.order.order_try, kept.outcome),
        (update.id.as_str(), 1, Outcome::Succeeded)
    );
    let mut host = Host::of(Arc::clone(&daemon), &coordinator, None);
    host.beat();
    assert_eq!(last_update(&coordinator).1, pb::Outcome::Succeeded as i32);
    let rec = s.rig.record(HK).expect("the record");
    assert_eq!(rec.progress.report_order_id, None);
    drop(host);

    let again = s.daemon_running_as(NEW, own_unit());
    let listed = recorded(again.ctx(), hk()).expect("listed");
    assert_eq!(listed.last_result, None);
}

/// Catches a verdict reported where none can be acknowledged, or for another service, or an
/// acknowledgement that clears the wrong verdict: a wait for the restart that names no order (as
/// a daemon from before the id was kept writes it, or as an order with no id leaves it) is
/// settled and reports no verdict; a settled order is reported for `hostkeeper` only, never for
/// another service; and an acknowledgement of another order, another try or another service
/// leaves it to report.
#[test]
fn only_a_settled_wait_that_names_its_order_is_reported_and_only_for_hostkeeper() {
    let s = SelfRig::new("converge-no-id");
    let old = s.daemon_running_as(OLD, own_unit());
    let reply = old.order(&s.update_of(HK, NEW, 3));
    assert_eq!(reply.refusal(), None, "{reply:?}");
    let path = s.rig.state.join(HK).join(RECORD_FILE);
    let text = fs::read_to_string(&path).expect("the record");
    let id = "\"order_id\": \"order-hostkeeper-0.2.0\"";
    assert!(text.contains(id), "{text}");
    let unnamed = text.replace(&format!(",\n      {id}"), "");
    assert!(!unnamed.contains("order_id"), "{unnamed}");
    fs::write(&path, unnamed).expect("the record of an older daemon");
    let new = s.daemon_running_as(NEW, own_unit());
    assert_eq!(settled(&new), Settled::Confirmed);
    assert_eq!(recorded(new.ctx(), hk()).expect("listed").last_result, None);

    // An order that carries no id keeps none in its wait, and its verdict is reported by no
    // binary either.
    let s = SelfRig::new("converge-empty-id");
    let mut order = s.update_of(HK, NEW, 3);
    order.order_id = String::new();
    s.daemon_running_as(OLD, own_unit()).order(&order);
    let wait = s
        .rig
        .record(HK)
        .and_then(|rec| rec.progress.awaiting_restart);
    assert_eq!(
        wait.map(|wait| (wait.version, wait.order_id)),
        Some((NEW.to_owned(), None))
    );
    let new = s.daemon_running_as(NEW, own_unit());
    assert_eq!(settled(&new), Settled::Confirmed);
    assert_eq!(recorded(new.ctx(), hk()).expect("listed").last_result, None);

    // The same update with its id kept: reported for hostkeeper, and for no other service.
    let s = SelfRig::new("converge-named");
    s.rig.running("web", "1.0.0");
    s.daemon_running_as(OLD, own_unit())
        .order(&s.update_of(HK, NEW, 3));
    let new = Arc::new(s.daemon_running_as(NEW, own_unit()));
    assert_eq!(settled(&new), Settled::Confirmed);
    let web = ServiceName::new("web").expect("a name");
    assert_eq!(
        recorded(new.ctx(), web.clone())
            .expect("listed")
            .last_result,
        None
    );
    let result = recorded(new.ctx(), hk())
        .expect("listed")
        .last_result
        .expect("the settled verdict");
    assert_eq!(
        (
            result.order.id.as_str(),
            result.order.order_try,
            result.target.as_str(),
            result.outcome,
            result.forced
        ),
        ("order-hostkeeper-0.2.0", 3, NEW, Outcome::Succeeded, false)
    );
    let records = DaemonRecords::new(Arc::clone(&new));
    let other = |id: &str, order_try| OrderRef {
        id: OrderId::new(id).expect("an id"),
        order_try,
    };
    for (service, order) in [
        (hk(), other("order-hostkeeper-0.1.9", 3)),
        (hk(), other("order-hostkeeper-0.2.0", 2)),
        (web, other("order-hostkeeper-0.2.0", 3)),
    ] {
        records
            .acknowledged(&service, &order)
            .expect("nothing to write");
        assert_eq!(
            recorded(new.ctx(), hk()).expect("listed").last_result,
            Some(result.clone()),
            "{service} {order:?}"
        );
    }
    records.acknowledged(&hk(), &result.order).expect("written");
    assert_eq!(recorded(new.ctx(), hk()).expect("listed").last_result, None);
}

fn hk() -> ServiceName {
    ServiceName::new(HK).expect("a name")
}
