//! The coordinator's typed orders end to end, in process: the real coordinator's heartbeat with
//! typed orders (`Coordinator::heartbeat_with_orders`, what its gRPC route answers), the real
//! heartbeat loop over the daemon (`hostkeeper_daemon::links`), and the real executor with its
//! step runner. The fakes are the network transport (a link that calls the coordinator as the
//! host `host-1` of the role `daemon`), the service manager and the package source.
//!
//! The coordinator orders an `install` and an `abort` only to a daemon that announced
//! `install-v1` and `abort-v1`; the loop announces what [`ANNOUNCED`] lists, so these tests also
//! prove the daemon's announcement reaches the coordinator's gates.

mod support;

use std::path::Path;
use std::sync::{Arc, Mutex};

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_coordinator::operator::Caller;
use hostkeeper_coordinator::service::Coordinator;
use hostkeeper_daemon::testing::spawn;
use hostkeeper_hook::Step;
use hostkeeper_link::testing::ManualDispatch;
use hostkeeper_link::{
    ANNOUNCED, BeatOutcome, DEFAULT_HEARTBEAT_SECS, Feature, Order, PackageDigest, Sleeper, Target,
    Tick, Verb,
};
use hostkeeper_observe::ActiveState;
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_proto::hostkeeper::operator::v1::{AbortRequest, GateReason};
use hostkeeper_steps::Settled;
use support::coordinated::{
    HOOK_LINES, Host, convergence_of, coordinator_of, hk_on_host, hostkeeper_at, hostkeeper_daemon,
    on_host, order_and_try, set_desired_of,
};
use support::selfupdate::{DEB_UNIT, DEB_WANTS, HK, NEW, OLD, SelfRig};
use support::{Rig, Scratch, d};

/// The service of the scenarios and the version the coordinator wants.
const SERVICE: &str = "web";
const TARGET: &str = "2.0.0";

/// The real coordinator on the state files of `dir`: `web` runs on the role `daemon` and is wanted
/// at [`TARGET`], whose package digest CI registered as [`d`], unless a coordinator before it on
/// `dir` left another state; the verdicts it acknowledges are kept in the verdicts file beside
/// the state file, as `serve` keeps them; it orders from its start (no quiet period), on a clock
/// that stays at second 1000.
fn coordinator(dir: &Path) -> Arc<Coordinator> {
    coordinator_of(dir, SERVICE, TARGET)
}

/// CI sets `web` to `version`, with the package digest [`d`].
fn set_desired(coordinator: &Coordinator, version: &str) {
    set_desired_of(coordinator, SERVICE, version);
}

/// Where the coordinator says `web` stands: `converged`, `in_progress`, `blocked` or `halted`.
fn convergence(coordinator: &Coordinator) -> String {
    convergence_of(coordinator, SERVICE)["state"]
        .as_str()
        .expect("a state")
        .to_owned()
}

/// Catches a daemon the coordinator never orders a first install (it did not announce
/// `install-v1`), a loop that drops an install order or refuses it for its service not being
/// installed, and an install that does not reach the executor's install path: the host, which
/// does not run `web`, is ordered its install at the coordinator's first answer; the real
/// executor runs the hook's four steps; the next heartbeat lists `web` at the version with the
/// install's verdict, and the coordinator reads `web` converged and orders nothing more.
#[test]
fn an_install_the_coordinator_orders_installs_the_service_and_converges() {
    let rig = Rig::new("typed-install");
    rig.stage(SERVICE, TARGET);
    for (step, line) in HOOK_LINES {
        rig.launcher.replies(SERVICE, step, &[(0, line)]);
    }
    let dir = Scratch::new("typed-install-coordinator");
    let coordinator = coordinator(dir.path());
    let mut host = Host::new(&rig, &coordinator);

    host.beat();
    let ordered = host.ordered();
    let install = ordered[0].clone().expect("an order");
    assert!(
        matches!(&install.verb, Verb::Install(target)
            if target.service.as_str() == SERVICE && target.version.as_str() == TARGET),
        "{install:?}"
    );
    assert!(host.dispatch.run_next());
    assert_eq!(
        rig.launcher.steps(SERVICE),
        [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup]
    );

    host.beat();
    host.beat();
    assert_eq!(host.ordered()[1..], [None, None]);
    assert_eq!(convergence(&coordinator), "converged");
    assert!(ANNOUNCED.contains(&Feature::InstallV1));
}

/// The version of `web` that `Hosts.Get` answers installed on `host-1`; none when it lists
/// none.
fn listed_version(coordinator: &Coordinator) -> Option<String> {
    on_host(coordinator, SERVICE).and_then(|service| service.installed_version)
}

/// The heartbeat loop's waits: in the first, the install the first heartbeat ordered runs to its
/// end; the second stops the loop. Before each wait it notes what `Hosts.Get` lists of `web`.
struct InstallInTheWait {
    dispatch: Arc<ManualDispatch>,
    coordinator: Arc<Coordinator>,
    /// Each wait asked, in seconds, and the version `Hosts.Get` listed as it began.
    waits: Mutex<Vec<(u64, Option<String>)>>,
}

impl Sleeper for InstallInTheWait {
    fn sleep(&self, secs: u64) -> Tick {
        let mut waits = self.waits.lock().expect("the log is not poisoned");
        waits.push((secs, listed_version(&self.coordinator)));
        if waits.len() == 1 {
            assert!(self.dispatch.run_next(), "the install runs");
            Tick::Beat
        } else {
            Tick::Stop
        }
    }
}

/// Catches a host whose new service the coordinator lists only a heartbeat period after its
/// install ended: the coordinator orders the install at the first heartbeat, the real executor
/// runs it to its end within the first second of the loop's wait, and by the next wait
/// `Hosts.Get` lists `web` installed at the target, with fewer seconds waited in between than
/// one heartbeat period, on the loop's own sleeper and not the wall clock.
#[test]
fn hosts_get_lists_an_installed_service_without_another_heartbeat_period() {
    let rig = Rig::new("typed-install-listed");
    rig.stage(SERVICE, TARGET);
    for (step, line) in HOOK_LINES {
        rig.launcher.replies(SERVICE, step, &[(0, line)]);
    }
    let dir = Scratch::new("typed-install-listed-coordinator");
    let coordinator = coordinator(dir.path());
    let host = Host::new(&rig, &coordinator);
    let sleeper = InstallInTheWait {
        dispatch: Arc::clone(&host.dispatch),
        coordinator: Arc::clone(&coordinator),
        waits: Mutex::new(Vec::new()),
    };
    let mut seen = Vec::new();
    host.heartbeater
        .run(&sleeper, &mut |outcome| seen.push(outcome.clone()));

    assert_eq!(seen, [BeatOutcome::Sent, BeatOutcome::Sent]);
    assert_eq!(
        rig.launcher.steps(SERVICE),
        [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup]
    );
    let ordered = host
        .ordered
        .lock()
        .expect("the log is not poisoned")
        .clone();
    assert!(
        matches!(&ordered[0], Some(Order { verb: Verb::Install(target), .. })
            if target.service.as_str() == SERVICE && target.version.as_str() == TARGET),
        "{ordered:?}"
    );
    let waits = sleeper
        .waits
        .lock()
        .expect("the log is not poisoned")
        .clone();
    let listed: Vec<Option<&str>> = waits.iter().map(|(_, at)| at.as_deref()).collect();
    assert_eq!(listed, [None, Some(TARGET)], "{waits:?}");
    let waited = waits[0].0;
    assert!(
        waited < DEFAULT_HEARTBEAT_SECS.get(),
        "listed after {waited} seconds of a {DEFAULT_HEARTBEAT_SECS}-second period"
    );
    assert_eq!(listed_version(&coordinator).as_deref(), Some(TARGET));
}

/// Catches an abort the coordinator never sends (the daemon did not announce `abort-v1`), an
/// abort the loop answers `busy` because the update it is for runs, one run on the loop's own
/// thread, and an abort whose update goes on or reports no halt: an update of `web` holds in
/// `prepare` until its unit is stopped; an operator's `Hosts.Abort` is answered at the next
/// heartbeat; the loop hands it to the executor beside the update, which stops the step and ends
/// the update in the halt phase (`aborted_by_operator`) with no later step run; the abort's
/// answer is logged, and the next heartbeat reports the update's halt, which halts `web` on the
/// coordinator.
#[test]
fn an_abort_the_coordinator_queues_stops_the_live_update_and_its_halt_is_reported() {
    let rig = Rig::new("typed-abort");
    rig.add(SERVICE, TARGET);
    for (step, line) in HOOK_LINES {
        rig.launcher.replies(SERVICE, step, &[(0, line)]);
    }
    let gate = rig.launcher.hold_until_stopped(SERVICE, Step::Prepare);
    let dir = Scratch::new("typed-abort-coordinator");
    let coordinator = coordinator(dir.path());
    let mut host = Host::new(&rig, &coordinator);

    host.beat();
    let update = host.ordered()[0].clone().expect("an order");
    assert!(matches!(update.verb, Verb::Update(_)), "{update:?}");
    let worker = {
        let dispatch = Arc::clone(&host.dispatch);
        spawn("the update's worker", move || dispatch.run_next())
    };
    gate.entered(&worker).expect("the held step is entered");

    let operator = Caller::Operator("operator".parse().expect("a token name"));
    let queued = coordinator
        .abort(
            &operator,
            AbortRequest {
                node_id: "host-1".to_owned(),
                service: SERVICE.to_owned(),
                reason: "stuck in prepare".to_owned(),
            },
        )
        .expect("queued");
    assert!(queued.queued);
    host.beat();
    let abort = host.ordered()[1].clone().expect("an order");
    assert_eq!(
        abort.verb,
        Verb::Abort(ServiceName::new(SERVICE).expect("a service"))
    );
    assert!(host.dispatch.run_next());
    assert!(worker.join().expect("the update's worker"));
    assert_eq!(rig.launcher.steps(SERVICE), [Step::Prepare]);
    assert_eq!(
        host.log.lines(),
        [format!(
            "order {} try 1 of {SERVICE} answered failed: aborted_by_operator",
            abort.id.as_str()
        )]
    );

    host.beat();
    assert_eq!(host.ordered()[2], None);
    assert_eq!(convergence(&coordinator), "halted");
}

/// The next version CI sets after the install.
const NEXT: &str = "3.0.0";

/// `web` at `version`, as an update or an install of the package [`d`] names it.
fn web_at(version: &str) -> Target {
    Target {
        service: ServiceName::new(SERVICE).expect("a service"),
        version: Version::new(version).expect("a version"),
        digest: PackageDigest::new(&d()).expect("a digest"),
        index_digest: None,
    }
}

/// Catches the update after an install refused `try_conflict` (komira-ai/hostkeeper#115): the
/// coordinator numbered tries per order id, so the update of a service the host had just
/// installed went at try 1, the try the daemon's step record held for the install, and the daemon
/// refused it for its other package. The coordinator installs `web` at [`TARGET`] (try 1); CI sets
/// [`NEXT`]; the coordinator orders the update at try 2; the real executor runs all four steps of
/// it, and the coordinator reads `web` converged at [`NEXT`] and orders nothing more.
#[test]
fn an_update_after_an_install_runs_and_converges() {
    let rig = Rig::new("typed-install-update");
    rig.stage(SERVICE, TARGET);
    rig.stage(SERVICE, NEXT);
    for (step, line) in HOOK_LINES {
        rig.launcher.replies(SERVICE, step, &[(0, line), (0, line)]);
    }
    let dir = Scratch::new("typed-install-update-coordinator");
    let coordinator = coordinator(dir.path());
    let mut host = Host::new(&rig, &coordinator);

    host.beat();
    assert_eq!(order_and_try(&host, 0), (Verb::Install(web_at(TARGET)), 1));
    assert!(host.dispatch.run_next());
    host.beat();
    assert_eq!(host.ordered()[1], None);
    assert_eq!(convergence(&coordinator), "converged");

    set_desired(&coordinator, NEXT);
    host.beat();
    assert_eq!(order_and_try(&host, 2), (Verb::Update(web_at(NEXT)), 2));
    assert!(host.dispatch.run_next());
    assert_eq!(
        rig.launcher.steps(SERVICE),
        [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup].repeat(2)
    );
    host.beat();
    host.beat();
    assert_eq!(host.ordered()[3..], [None, None]);
    assert_eq!(convergence(&coordinator), "converged");
}

/// Catches a host ordered from try 1 again after its daemon restarts: the daemon installs `web`
/// (try 1) and reports the install's verdict, which the coordinator stores and acknowledges; the
/// daemon restarts, and its loop, which no longer sends that verdict, lists `web` with none. The
/// coordinator reads the host as still reporting the verdict it acknowledged (design 8.2), so it
/// orders the next update one past it, at try 2, which the restarted daemon runs to convergence.
/// At try 1 the daemon, whose step record holds the install's try 1, would refuse it
/// `try_conflict`.
#[test]
fn a_restarted_daemon_is_ordered_one_past_the_try_it_reported() {
    let rig = Rig::new("typed-restart-update");
    rig.stage(SERVICE, TARGET);
    rig.stage(SERVICE, NEXT);
    for (step, line) in HOOK_LINES {
        rig.launcher.replies(SERVICE, step, &[(0, line), (0, line)]);
    }
    let dir = Scratch::new("typed-restart-update-coordinator");
    let coordinator = coordinator(dir.path());
    let mut host = Host::new(&rig, &coordinator);
    host.beat();
    assert_eq!(order_and_try(&host, 0), (Verb::Install(web_at(TARGET)), 1));
    assert!(host.dispatch.run_next());
    host.beat();
    assert_eq!(host.ordered()[1], None);
    assert_eq!(convergence(&coordinator), "converged");
    drop(host);

    set_desired(&coordinator, NEXT);
    let mut host = Host::new(&rig, &coordinator);
    host.beat();
    assert_eq!(order_and_try(&host, 0), (Verb::Update(web_at(NEXT)), 2));
    assert!(host.dispatch.run_next());
    assert_eq!(
        rig.launcher.steps(SERVICE),
        [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup].repeat(2)
    );
    host.beat();
    assert_eq!(host.ordered()[1], None);
    assert_eq!(convergence(&coordinator), "converged");
}

/// The version CI sets after [`NEXT`], once both the daemon and the coordinator restarted.
const LAST: &str = "4.0.0";

/// Catches a try numbered from what the beat carries now instead of the highest the host
/// reported: an acknowledged verdict is no longer sent, and after a restart of both the daemon
/// and the coordinator nothing but the coordinator's verdicts file remembers it. The coordinator
/// installs `web` at [`TARGET`] (try 1) and updates it to [`NEXT`] (try 2), each verdict
/// reported and acknowledged; the daemon and the coordinator both restart on their own files; CI
/// sets [`LAST`]; the restarted coordinator orders the update at try 3, which the restarted
/// daemon runs through all four steps, and `web` reads converged at [`LAST`]. At try 1 or 2 the
/// daemon, whose step record holds try 2, would refuse it `stale_try` or `try_conflict`.
#[test]
fn install_update_restart_update_runs_every_order_one_try_past_the_last() {
    let rig = Rig::new("typed-twice");
    for version in [TARGET, NEXT, LAST] {
        rig.stage(SERVICE, version);
    }
    for (step, line) in HOOK_LINES {
        rig.launcher
            .replies(SERVICE, step, &[(0, line), (0, line), (0, line)]);
    }
    let dir = Scratch::new("typed-twice-coordinator");
    let first = coordinator(dir.path());
    let mut host = Host::new(&rig, &first);

    host.beat();
    assert_eq!(order_and_try(&host, 0), (Verb::Install(web_at(TARGET)), 1));
    assert!(host.dispatch.run_next());
    host.beat();
    assert_eq!(host.ordered()[1], None);

    set_desired(&first, NEXT);
    host.beat();
    assert_eq!(order_and_try(&host, 2), (Verb::Update(web_at(NEXT)), 2));
    assert!(host.dispatch.run_next());
    host.beat();
    host.beat();
    assert_eq!(host.ordered()[3..], [None, None]);
    assert_eq!(convergence(&first), "converged");
    drop(host);
    drop(first);

    let second = coordinator(dir.path());
    let mut host = Host::new(&rig, &second);
    set_desired(&second, LAST);
    host.beat();
    assert_eq!(order_and_try(&host, 0), (Verb::Update(web_at(LAST)), 3));
    assert!(host.dispatch.run_next());
    assert_eq!(
        rig.launcher.steps(SERVICE),
        [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup].repeat(3)
    );
    host.beat();
    assert_eq!(host.ordered()[1], None);
    assert_eq!(convergence(&second), "converged");
}

/// Catches a fleet built by bootstrap that never updates hostkeeper: a daemon the deb or the pkg
/// installed lists no service `hostkeeper` in its heartbeat (no package of it is installed), so
/// the coordinator counted the host `missing`, and the install it ordered placed the package and
/// left the running binary alone, after which the heartbeat listed hostkeeper at the new version
/// and the coordinator read it converged while the old binary ran. The coordinator wants
/// hostkeeper at [`NEW`]; the host's daemon runs [`OLD`] and names itself in the heartbeat's
/// `agent`. The coordinator counts the host as one of hostkeeper's, behind, not missing; it
/// orders the install of the release (try 1), which the executor runs as the self-update: the
/// binary directory's `next` links [`NEW`] (`current` stays) and the daemon asks to exit. The binary of
/// [`NEW`] confirms the update at its start, and the coordinator reads hostkeeper converged and
/// orders nothing more.
#[test]
fn a_bootstrapped_daemon_is_installed_its_own_release_updates_itself_and_converges() {
    let s = SelfRig::bootstrapped("typed-selfupdate-bootstrap");
    for (step, line) in HOOK_LINES {
        s.rig.launcher.replies(HK, step, &[(0, line)]);
    }
    let dir = Scratch::new("typed-selfupdate-bootstrap-coordinator");
    let coordinator = coordinator_of(dir.path(), HK, NEW);
    let (daemon, settled) = hostkeeper_daemon(&s, OLD);
    assert_eq!(settled, Settled::NotPending);
    let mut host = Host::of(Arc::clone(&daemon), &coordinator, None);

    host.beat();
    assert_eq!(
        order_and_try(&host, 0),
        (Verb::Install(hostkeeper_at(NEW)), 1)
    );
    let convergence = convergence_of(&coordinator, HK);
    assert_eq!(convergence["state"], "in_progress", "{convergence}");
    // No host the convergence leaves out: the host is not missing.
    assert_eq!(
        convergence["hosts"],
        serde_json::Value::Null,
        "{convergence}"
    );
    assert!(host.dispatch.run_next());
    assert_eq!(s.current().as_deref(), Some(OLD));
    assert_eq!(s.layout().next().expect("next").as_deref(), Some(NEW));
    assert!(daemon.ctx().restart().requested());
    drop(host);

    let (daemon, settled) = hostkeeper_daemon(&s, NEW);
    assert_eq!(settled, Settled::Confirmed);
    let mut host = Host::of(daemon, &coordinator, None);
    host.beat();
    host.beat();
    assert_eq!(host.ordered(), [None, None]);
    let convergence = convergence_of(&coordinator, HK);
    assert_eq!(convergence["state"], "converged", "{convergence}");
    assert_eq!(
        s.hk_steps(),
        [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup]
    );
}

/// Catches the self-update of a daemon the deb installed stopping after it placed the package:
/// the deb enabled `hostkeeper.service` with its own `WantedBy=` link, which `placed.json` does
/// not list, and the install of hostkeeper's release (which carries that unit, run) was refused
/// `path_conflict` before anything was written, and its verdict, of a service not listed, was
/// never sent. The install of [`NEW`] the coordinator orders now places the release around the
/// deb's link (left as it is), stages the binary and links `next` at it, and asks the daemon to exit; the
/// next heartbeat reports the wait for the restart; the binary of [`NEW`] confirms it at its
/// start, and the coordinator reads hostkeeper converged, its slot free, and orders nothing more.
#[test]
fn a_daemon_the_deb_enabled_installs_its_own_release_restarts_into_it_and_converges() {
    let s = SelfRig::deb("typed-selfupdate-deb");
    for (step, line) in HOOK_LINES {
        s.rig.launcher.replies(HK, step, &[(0, line)]);
    }
    let dir = Scratch::new("typed-selfupdate-deb-coordinator");
    let coordinator = coordinator_of(dir.path(), HK, NEW);
    let (daemon, _) = hostkeeper_daemon(&s, OLD);
    let mut host = Host::of(Arc::clone(&daemon), &coordinator, None);

    host.beat();
    assert_eq!(
        order_and_try(&host, 0),
        (Verb::Install(hostkeeper_at(NEW)), 1)
    );
    assert!(host.dispatch.run_next());
    assert_eq!(s.current().as_deref(), Some(OLD));
    assert_eq!(s.layout().next().expect("next").as_deref(), Some(NEW));
    assert!(daemon.ctx().restart().requested());
    let link = |path: &str| std::fs::read_link(s.rig.host(path)).ok();
    assert_eq!(link(DEB_WANTS), Some(DEB_UNIT.into()));
    assert_eq!(
        link("/units/hostkeeper.service"),
        Some("/packages/hostkeeper/current/units/hostkeeper.service".into())
    );
    host.beat();
    let reported = hk_on_host(&coordinator).last_update.expect("the verdict");
    assert_eq!(
        (reported.reason.as_str(), reported.order_try),
        ("awaiting_restart", Some(1))
    );
    drop(host);

    // The unit systemd started again runs the binary of [`NEW`].
    s.rig
        .manager
        .set("hostkeeper.service", ActiveState::Active, Some(4242));
    let (daemon, settled) = hostkeeper_daemon(&s, NEW);
    assert_eq!(settled, Settled::Confirmed);
    let mut host = Host::of(daemon, &coordinator, None);
    host.beat();
    host.beat();
    assert_eq!(host.ordered(), [None, None]);
    let convergence = convergence_of(&coordinator, HK);
    assert_eq!(convergence["state"], "converged", "{convergence}");
    let hk = hk_on_host(&coordinator);
    assert_eq!(
        (
            hk.holds_slot,
            hk.gate_reason,
            hk.installed_version.as_deref()
        ),
        (false, GateReason::UpToDate as i32, Some(NEW)),
        "{hk:?}"
    );
}

/// A release older than the daemon's oldest reader (0.1.163).
const BELOW: &str = "0.1.162";

/// Catches the verdict of hostkeeper's own install kept unsent while hostkeeper is not listed (a
/// daemon the deb installed lists no service `hostkeeper` until its package is placed), which
/// left the order holding the fleet's one update slot: a release older than this daemon's oldest
/// reader is refused before anything is written, and the next heartbeat carries
/// that verdict, which the coordinator records as the host's last update of hostkeeper, the slot
/// no longer held; once it is acknowledged, the coordinator does not order the same install
/// again (the host still lists hostkeeper absent, reporting that refusal).
#[test]
fn a_refused_install_of_hostkeeper_is_reported_in_the_next_heartbeat() {
    let s = SelfRig::deb("typed-selfupdate-refused");
    s.shipped_release(BELOW);
    let dir = Scratch::new("typed-selfupdate-refused-coordinator");
    let coordinator = coordinator_of(dir.path(), HK, BELOW);
    let (daemon, _) = hostkeeper_daemon(&s, OLD);
    let mut host = Host::of(daemon, &coordinator, None);

    host.beat();
    assert!(host.dispatch.run_next());
    assert_eq!(s.current().as_deref(), Some(OLD));
    assert_eq!(s.rig.record(HK), None);
    host.beat();
    let hk = hk_on_host(&coordinator);
    let reported = hk
        .last_update
        .clone()
        .expect("the verdict reached the coordinator");
    assert_eq!(
        (
            reported.target.as_str(),
            reported.outcome,
            reported.reason.as_str(),
            reported.order_try
        ),
        (
            BELOW,
            pb::Outcome::Failed as i32,
            "protocol_unsupported",
            Some(1)
        )
    );
    assert!(!hk.holds_slot, "{hk:?}");
    // Acknowledged, the refusal stands: the host is not ordered the same install again.
    host.beat();
    host.beat();
    assert_eq!(host.ordered()[1..], [None, None, None]);
    assert_eq!(hk_on_host(&coordinator).last_update, Some(reported));
    let id = host.ordered()[0].clone().expect("the install").id;
    assert_eq!(
        host.log.lines(),
        [format!(
            "order {} try 1 of hostkeeper, which is not installed, ended failed: \
             protocol_unsupported: it is sent with hostkeeper listed absent",
            id.as_str()
        )]
    );
}

/// Catches a host that installed hostkeeper's package losing the self-update it had: the daemon
/// runs [`OLD`] with its package installed, so its heartbeat lists hostkeeper. The coordinator
/// orders the update to [`NEW`] at its first answer, try 1, with no install before it; the
/// executor runs it as the self-update, the binary of [`NEW`] confirms it at its start, and the
/// coordinator reads hostkeeper converged and orders nothing more.
#[test]
fn a_daemon_with_its_own_package_updates_itself_and_converges() {
    let s = SelfRig::new("typed-selfupdate-installed");
    for (step, line) in HOOK_LINES {
        s.rig.launcher.replies(HK, step, &[(0, line)]);
    }
    let dir = Scratch::new("typed-selfupdate-installed-coordinator");
    let coordinator = coordinator_of(dir.path(), HK, NEW);
    let (daemon, _) = hostkeeper_daemon(&s, OLD);
    let mut host = Host::of(Arc::clone(&daemon), &coordinator, None);

    host.beat();
    assert_eq!(
        order_and_try(&host, 0),
        (Verb::Update(hostkeeper_at(NEW)), 1)
    );
    assert!(host.dispatch.run_next());
    assert_eq!(s.current().as_deref(), Some(OLD));
    assert_eq!(s.layout().next().expect("next").as_deref(), Some(NEW));
    assert!(daemon.ctx().restart().requested());
    drop(host);

    let (daemon, settled) = hostkeeper_daemon(&s, NEW);
    assert_eq!(settled, Settled::Confirmed);
    let mut host = Host::of(daemon, &coordinator, None);
    host.beat();
    host.beat();
    assert_eq!(host.ordered(), [None, None]);
    let convergence = convergence_of(&coordinator, HK);
    assert_eq!(convergence["state"], "converged", "{convergence}");
    assert_eq!(
        s.hk_steps(),
        [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup]
    );
}
