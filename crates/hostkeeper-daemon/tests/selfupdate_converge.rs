//! The update of hostkeeper itself through to the coordinator, in process: the real coordinator
//! (its heartbeat with typed orders, its verdicts file and its bake), the real heartbeat loop over
//! the daemon, the real executor, and the daemon's own observer over a fake service manager and
//! fake processes. The host is shaped as a Mac: hostkeeper's package names no `[units] run` (the
//! installer pkg places the daemon's launchd job, not the package), and the daemon runs as the
//! job `dev.hostkeeper.hostkeeper` from the binary directory's `current`.
//!
//! What these hold the daemon to: after its self-update, the binary that settles it reports the
//! update's verdict in its first heartbeat, in place of whatever older verdict the coordinator
//! still holds for the host, and reports its own service live, healthy and at the version it
//! runs, so that the coordinator bakes the host and reads hostkeeper converged.

mod support;

use std::fs;
use std::sync::Arc;

use hostkeeper_config::names::ServiceName;
use hostkeeper_daemon::link_adapters::recorded;
use hostkeeper_daemon::selfupdate::{ProbeError, SettledOrder};
use hostkeeper_hook::Verdict;
use hostkeeper_launchd::Label;
use hostkeeper_link::{Order, Verb};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_steps::Settled;
use hostkeeper_steps::record::RECORD_FILE;
use support::Scratch;
use support::coordinated::{
    HOOK_LINES, Host, convergence_of, coordinator_with, hk_on_host, hostkeeper_at, order_and_try,
    set_desired_of,
};
use support::selfupdate::{HK, LATER, NEW, OLD, SelfRig};

/// The job a Mac's daemon runs as.
fn mac_job() -> String {
    Label::daemon().to_string()
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

/// The order of the answer `n` (from 0).
fn order_of(host: &Host, n: usize) -> Order {
    host.ordered()[n].clone().expect("an order")
}

/// Catches the farm's stalled Mac rollout: a Mac failed hostkeeper's update to [`NEW`]
/// (`protocol_unreadable`), and the coordinator kept that verdict as the host's. Ordered
/// [`LATER`], the daemon of [`OLD`] ran the self-update and exited before its `Deferred` reached
/// a heartbeat, and the binary of [`LATER`] settled it `Succeeded` but reported no verdict, so
/// the coordinator kept reading the failure as the host's last update; and the daemon observed
/// its own service as `unmonitored` (the package names no unit) and so unhealthy, so the host
/// never baked and the rollout never converged. Now the first heartbeat of the binary of
/// [`LATER`] carries the success of the order it settled, under that order's id and try, which
/// the coordinator holds in place of the failure; hostkeeper is reported healthy at [`LATER`],
/// confirmed by its running executable, and the coordinator reads it converged once the host
/// has beaten healthy twice.
#[test]
fn a_mac_self_update_reports_its_success_over_an_older_failure_bakes_and_converges() {
    let s = SelfRig::new("converge-mac");
    s.release(LATER);
    // Sealed, as a release the host fetched is: its version is confirmed against the binary.
    s.rig.store.seal(HK, LATER, &support::d());
    for (step, line) in HOOK_LINES {
        s.rig.launcher.replies(HK, step, &[(0, line), (0, line)]);
    }
    let dir = Scratch::new("converge-mac-coordinator");
    let coordinator = coordinator_with(dir.path(), HK, NEW, BAKE);
    s.probe.set(|| Err(ProbeError::Exit(None)));
    s.runs(&mac_job(), OLD, 100);
    let daemon = Arc::new(s.daemon_running_as(OLD, &mac_job()));
    let mut host = Host::of(Arc::clone(&daemon), &coordinator, None);

    host.beat();
    assert_eq!(
        order_and_try(&host, 0),
        (Verb::Update(hostkeeper_at(NEW)), 1)
    );
    assert!(host.dispatch.run_next());
    host.beat();
    let failed = order_of(&host, 0);
    assert_eq!(
        last_update(&coordinator),
        (
            NEW.to_owned(),
            pb::Outcome::Failed as i32,
            "protocol_unreadable".to_owned(),
            failed.id.as_str().to_owned(),
            Some(1)
        )
    );
    assert_eq!(hk_on_host(&coordinator).health, pb::Health::Healthy as i32);

    set_desired_of(&coordinator, HK, LATER);
    s.probe.set(|| Ok(1));
    host.beat();
    let ordered = order_of(&host, 2);
    assert_eq!(ordered.verb, Verb::Update(hostkeeper_at(LATER)));
    assert!(host.dispatch.run_next());
    assert_eq!(s.current().as_deref(), Some(LATER));
    assert!(daemon.ctx().restart().requested());
    // The daemon exits before its `Deferred` reaches a heartbeat.
    drop(host);

    s.runs(&mac_job(), LATER, 101);
    let daemon = Arc::new(s.daemon_running_as(LATER, &mac_job()));
    assert_eq!(
        daemon.settle_self_update().expect("settled"),
        Settled::Confirmed
    );
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

/// Catches the self-update of a Mac the pkg installed (no package of hostkeeper, so it is
/// ordered an `install`) whose success never reaches the coordinator: the binary of [`NEW`]
/// settles the install `Succeeded` and its first heartbeat carries that verdict, under the
/// install's id and try 1, and the coordinator reads hostkeeper converged.
#[test]
fn a_mac_the_pkg_installed_reports_the_install_of_its_release_and_converges() {
    let s = SelfRig::bootstrapped("converge-pkg");
    for (step, line) in HOOK_LINES {
        s.rig.launcher.replies(HK, step, &[(0, line)]);
    }
    let dir = Scratch::new("converge-pkg-coordinator");
    let coordinator = coordinator_with(dir.path(), HK, NEW, BAKE);
    s.runs(&mac_job(), OLD, 100);
    let mut host = Host::of(
        Arc::new(s.daemon_running_as(OLD, &mac_job())),
        &coordinator,
        None,
    );
    host.beat();
    assert_eq!(
        order_and_try(&host, 0),
        (Verb::Install(hostkeeper_at(NEW)), 1)
    );
    assert!(host.dispatch.run_next());
    let install = order_of(&host, 0);
    drop(host);

    s.runs(&mac_job(), NEW, 101);
    let daemon = Arc::new(s.daemon_running_as(NEW, &mac_job()));
    assert_eq!(
        daemon.settle_self_update().expect("settled"),
        Settled::Confirmed
    );
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

/// Catches a fallback the coordinator never hears of: the binary that starts after the switch is
/// the previous one (the new one fell back), which settles the update `Failed` after the commit,
/// of class release, `self_update_fallback`, and its first heartbeat carries that verdict under
/// the update's id and try.
#[test]
fn a_self_update_that_fell_back_reports_the_fallback_under_its_order() {
    let s = SelfRig::new("converge-fallback");
    for (step, line) in HOOK_LINES {
        s.rig.launcher.replies(HK, step, &[(0, line)]);
    }
    let dir = Scratch::new("converge-fallback-coordinator");
    let coordinator = coordinator_with(dir.path(), HK, NEW, BAKE);
    s.runs(&mac_job(), OLD, 100);
    let mut host = Host::of(
        Arc::new(s.daemon_running_as(OLD, &mac_job())),
        &coordinator,
        None,
    );
    host.beat();
    assert!(host.dispatch.run_next());
    let update = order_of(&host, 0);
    drop(host);

    s.layout()
        .flip_back()
        .expect("the fallback puts the previous binary back");
    let daemon = Arc::new(s.daemon_running_as(OLD, &mac_job()));
    assert_eq!(
        daemon.settle_self_update().expect("settled"),
        Settled::FellBack
    );
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
        )
    );
    let reported = hk_on_host(&coordinator).last_update.expect("the verdict");
    assert_eq!(
        (reported.phase, reported.class),
        (
            pb::FailurePhase::AfterCommit as i32,
            pb::FailureClass::Release as i32
        )
    );
}

/// Catches a verdict reported where none can be acknowledged, or for another service: a wait for
/// the restart that names no order (as a daemon from before the id was kept writes it, or as an
/// order with no id leaves it) is settled and reports no verdict; a settled order is reported for `hostkeeper` only, never for another
/// service; and a start that settles nothing keeps none.
#[test]
fn only_a_settled_wait_that_names_its_order_is_reported_and_only_for_hostkeeper() {
    let s = SelfRig::new("converge-no-id");
    for (step, line) in HOOK_LINES {
        s.rig.launcher.replies(HK, step, &[(0, line)]);
    }
    s.rig.running("web", "1.0.0");
    let old = s.daemon_running_as(OLD, &mac_job());
    let reply = old.order(&support::update(HK, NEW, &support::d(), 3));
    assert_eq!(reply.refusal(), None, "{reply:?}");
    let path = s.rig.state.join(HK).join(RECORD_FILE);
    let text = fs::read_to_string(&path).expect("the record");
    let id = "\"order_id\": \"order-hostkeeper-0.2.0\"";
    assert!(text.contains(id), "{text}");
    let unnamed = text.replace(&format!(",\n      {id}"), "");
    assert!(!unnamed.contains("order_id"), "{unnamed}");
    fs::write(&path, unnamed).expect("the record of an older daemon");

    let new = s.daemon_running_as(NEW, &mac_job());
    assert_eq!(
        new.settle_self_update().expect("settled"),
        Settled::Confirmed
    );
    assert_eq!(new.ctx().settled_order(), None);
    let listed = recorded(new.ctx(), ServiceName::new(HK).expect("a name")).expect("listed");
    assert_eq!(listed.last_result, None);

    // An order that carries no id keeps none in its wait, and its verdict is reported by no
    // binary either.
    let s = SelfRig::new("converge-empty-id");
    for (step, line) in HOOK_LINES {
        s.rig.launcher.replies(HK, step, &[(0, line)]);
    }
    let mut order = support::update(HK, NEW, &support::d(), 3);
    order.order_id = String::new();
    s.daemon_running_as(OLD, &mac_job()).order(&order);
    let wait = s
        .rig
        .record(HK)
        .and_then(|rec| rec.progress.awaiting_restart);
    assert_eq!(
        wait.map(|wait| (wait.version, wait.order_id)),
        Some((NEW.to_owned(), None))
    );
    let new = s.daemon_running_as(NEW, &mac_job());
    assert_eq!(
        new.settle_self_update().expect("settled"),
        Settled::Confirmed
    );
    assert_eq!(new.ctx().settled_order(), None);

    // The same update with its id kept: reported for hostkeeper, and for no other service.
    let s = SelfRig::new("converge-named");
    for (step, line) in HOOK_LINES {
        s.rig.launcher.replies(HK, step, &[(0, line)]);
    }
    s.rig.running("web", "1.0.0");
    let old = s.daemon_running_as(OLD, &mac_job());
    assert_eq!(old.ctx().settled_order(), None);
    old.order(&support::update(HK, NEW, &support::d(), 3));
    let new = s.daemon_running_as(NEW, &mac_job());
    assert_eq!(
        new.settle_self_update().expect("settled"),
        Settled::Confirmed
    );
    assert_eq!(
        new.ctx().settled_order(),
        Some(SettledOrder {
            order_id: "order-hostkeeper-0.2.0".to_owned(),
            order_try: 3,
            target: NEW.to_owned(),
            verdict: Verdict::Succeeded {
                promotion_pending: false
            },
            reason: None,
        })
    );
    let web = recorded(new.ctx(), ServiceName::new("web").expect("a name")).expect("listed");
    assert_eq!(web.last_result, None);
    let hk = recorded(new.ctx(), ServiceName::new(HK).expect("a name")).expect("listed");
    let result = hk.last_result.expect("the settled verdict");
    assert_eq!(
        (
            result.order.id.as_str(),
            result.order.order_try,
            result.target.as_str(),
            result.outcome,
            result.forced
        ),
        (
            "order-hostkeeper-0.2.0",
            3,
            NEW,
            hostkeeper_link::Outcome::Succeeded,
            false
        )
    );
    // A second start of the same binary settles nothing and keeps nothing to report.
    let again = s.daemon_running_as(NEW, &mac_job());
    assert_eq!(
        again.settle_self_update().expect("settled"),
        Settled::NotPending
    );
    assert_eq!(again.ctx().settled_order(), None);
}
