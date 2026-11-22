//! A Linux maintenance update end to end (docs/design/maintenance-updates.md, section 8, and the
//! one-button Linux update the build delivers first): the real coordinator schedules the run and
//! orders each host's turn from its heartbeats, and on each host the real daemon runs the turn
//! from its run record (stage, drain, apply with its reboot, check, put back) through the real
//! maintenance hook over a fake Linux host, across the host's reboot, a daemon that stops in the
//! middle of a turn and a coordinator that restarts (`support_linux`). The hook's `prepare` is the
//! real hook's too, over the real `reboot` kind; the `linux` kind's stage is answered there. Each
//! test ends with exact equality on what the fake host went through.

mod support;
mod support_linux;
mod support_maintain;

use hostkeeper_daemon::maintenance_run::Driven;
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_proto::hostkeeper::operator::v1 as op;
use hostkeeper_steps::maintenance::{Ending, Kind};
use op::MaintenanceHostStep as Step;
use support_linux::{
    Due, KERNEL_PACKAGE, NEW_KERNEL, Node, OLD_KERNEL, Ordered, SERVICE, World, after_drained,
    first_offer, kernel_turn, one_at_a_time,
};

/// The group of every test.
const GROUP: &str = "linux";

/// The largest time a `Schedule` names.
const MAX_SECS: u64 = i64::MAX as u64;

/// The `updates_due` notice of [`GROUP`], alone.
fn updates_due() -> Vec<(op::AlertKind, String, String)> {
    vec![(op::AlertKind::UpdatesDue, GROUP.to_owned(), String::new())]
}

/// No maintenance alert.
fn no_alerts() -> Vec<(op::AlertKind, String, String)> {
    Vec::new()
}

/// `host` and `step`, as [`World::steps`] lists them.
fn at(host: &str, step: Step) -> (String, Step) {
    (host.to_owned(), step)
}

/// The two hosts of [`GROUP`] for the test `test`: `h1`, with `due` due, and `h2`, with nothing
/// due, which keeps serving while `h1` takes its turn; each heard with its report.
fn pair(test: &str, due: Due) -> (std::sync::Arc<World>, Node, Node) {
    let world = World::new(test);
    world.group(GROUP, &["h1", "h2"], one_at_a_time());
    let mut h1 = Node::new(&world, test, "h1", GROUP, due);
    let mut h2 = Node::new(&world, test, "h2", GROUP, Due::Nothing);
    h1.report();
    h2.report();
    (world, h1, h2)
}

/// `node`'s turn, which waits for the reboot it asked for: it reports that it reboots, reboots,
/// its new daemon's start ends the turn done, and three heartbeats report that end and then what
/// the next look found. No answer orders anything.
fn reboot_to_done(node: &mut Node) {
    assert!(node.machine.reboot_asked(), "the turn asked for a reboot");
    assert_eq!(node.beat(), None);
    assert_eq!(node.reboot(), Some(Driven::Ended(Ending::Done)));
    for _ in 0..3 {
        assert_eq!(node.beat(), None);
    }
}

/// Proves: the Update button on a host with a due kernel update and a drain-capable service runs
/// the whole turn from one order: the stage, the service's drain, the install of the kernel
/// update, the reboot it needs, the check in the new boot, the service's resume, done; the host
/// runs the new kernel and is back in service, its next report shows nothing due and the
/// group's `updates_due` notice clears. The host was given one order, once; the other host of
/// the group went through nothing.
///
/// Catches: a turn that resumes the service before the check, or checks before the reboot; an
/// install or a reboot repeated after the reboot; an order given again; a report that still
/// lists the update, or a notice that stands, after the turn is done.
#[test]
fn the_update_button_drains_installs_reboots_checks_and_puts_back_a_host() {
    let (world, mut h1, h2) = pair("linux-e2e-update", Due::Kernel);
    assert_eq!(h1.due(), [pb::MaintenanceKind::Linux]);
    assert_eq!(world.alerts(), updates_due());

    let run = world.update(&["h1"]);
    let order = first_offer(&run, &[pb::MaintenanceKind::Linux]);
    assert_eq!(h1.beat(), Some(order.clone()));
    assert!(h1.machine.cordoned(), "the service is out of service");
    reboot_to_done(&mut h1);

    assert_eq!(h1.machine.effects(), kernel_turn());
    assert_eq!(h1.machine.running(), NEW_KERNEL);
    assert!(!h1.machine.cordoned(), "the service is back in service");
    assert_eq!(world.steps(&run), [at("h1", Step::Done)]);
    assert_eq!(h1.due(), Vec::<pb::MaintenanceKind>::new());
    assert_eq!(world.alerts(), no_alerts());
    assert_eq!(h1.orders(), [order]);
    assert_eq!(h2.machine.effects(), Vec::<String>::new());
}

/// Proves: a run over the three hosts of a group of three with batch 1 starts no host before its
/// window opens, and in the window takes one host at a time in node id order: after every
/// heartbeat and every reboot, at most one host is between staging and done, and the next host
/// starts only once the one before reported done. Each host is ordered once and goes through
/// the whole turn.
///
/// Catches: a host started before the window; two hosts of a group of batch 1 in their turn at
/// once; a host started while the one before still reboots or checks.
#[test]
fn a_batch_of_one_takes_one_host_of_the_group_at_a_time_inside_the_window() {
    let test = "linux-e2e-batch";
    let world = World::new(test);
    world.group(GROUP, &["h1", "h2", "h3"], one_at_a_time());
    let mut nodes: Vec<Node> = ["h1", "h2", "h3"]
        .iter()
        .map(|host| Node::new(&world, test, host, GROUP, Due::Kernel))
        .collect();
    for node in &mut nodes {
        node.report();
    }

    let later = world.schedule(op::ScheduleMaintenanceRequest {
        host_group: GROUP.to_owned(),
        window_start_unix: MAX_SECS - 1,
        window_end_unix: MAX_SECS,
        take_last_host: true,
        ..op::ScheduleMaintenanceRequest::default()
    });
    for node in &mut nodes {
        assert_eq!(node.beat(), None, "the window is not open");
    }
    assert_eq!(
        world.steps(&later),
        [
            at("h1", Step::Waiting),
            at("h2", Step::Waiting),
            at("h3", Step::Waiting),
        ]
    );
    world.cancel(&later);

    let run = world.schedule(op::ScheduleMaintenanceRequest {
        host_group: GROUP.to_owned(),
        window_start_unix: 1,
        window_end_unix: MAX_SECS,
        take_last_host: true,
        ..op::ScheduleMaintenanceRequest::default()
    });
    let mut seen = vec![world.in_turn(&run)];
    let mut started = Vec::new();
    for _ in 0..8 {
        for node in &mut nodes {
            if let Some(order) = node.beat() {
                started.push((node.name.clone(), order));
            }
            seen.push(world.in_turn(&run));
            if node.machine.reboot_asked() {
                assert_eq!(node.beat(), None);
                seen.push(world.in_turn(&run));
                assert_eq!(node.reboot(), Some(Driven::Ended(Ending::Done)));
                seen.push(world.in_turn(&run));
            }
        }
    }

    seen.dedup();
    let none: Vec<String> = Vec::new();
    let only = |host: &str| vec![host.to_owned()];
    assert_eq!(
        seen,
        [
            none.clone(),
            only("h1"),
            none.clone(),
            only("h2"),
            none.clone(),
            only("h3"),
            none,
        ]
    );
    let order = first_offer(&run, &[pb::MaintenanceKind::Linux]);
    assert_eq!(
        started,
        [
            ("h1".to_owned(), order.clone()),
            ("h2".to_owned(), order.clone()),
            ("h3".to_owned(), order),
        ]
    );
    assert_eq!(
        world.steps(&run),
        [
            at("h1", Step::Done),
            at("h2", Step::Done),
            at("h3", Step::Done),
        ]
    );
    for node in &nodes {
        assert_eq!(node.machine.effects(), kernel_turn(), "{}", node.name);
    }
    assert_eq!(world.alerts(), no_alerts());
}

/// Proves: an install that fails halts the host's turn at the apply: no resume and no rollback
/// run, the host stays out of service, nothing reboots, the run shows the host halted with the
/// daemon's reason, `maintenance_halted` stands for it and no other host of the group starts.
/// Put back runs the drained service's resume (the update left as far as it got), the alert
/// clears, and the group goes on: the next host takes its whole turn.
///
/// Catches: a halt that resumes the service or rolls anything back; a halted group that starts
/// its next host; a Put back that installs or reboots, or that leaves the group stopped.
#[test]
fn a_failed_apply_halts_the_host_cordoned_until_put_back_and_the_group_goes_on() {
    let test = "linux-e2e-halt";
    let world = World::new(test);
    world.group(GROUP, &["h1", "h2", "h3"], one_at_a_time());
    let mut h1 = Node::new(&world, test, "h1", GROUP, Due::Kernel);
    let mut h2 = Node::new(&world, test, "h2", GROUP, Due::Kernel);
    let mut h3 = Node::new(&world, test, "h3", GROUP, Due::Nothing);
    h1.machine.fail_install();
    for node in [&mut h1, &mut h2, &mut h3] {
        node.report();
    }

    let run = world.schedule(op::ScheduleMaintenanceRequest {
        host_group: GROUP.to_owned(),
        ..op::ScheduleMaintenanceRequest::default()
    });
    let order = first_offer(&run, &[pb::MaintenanceKind::Linux]);
    assert_eq!(h1.beat(), Some(order.clone()));
    let halted_at = vec![
        "stage".to_owned(),
        format!("drain {SERVICE}"),
        format!("install {KERNEL_PACKAGE}"),
    ];
    assert_eq!(h1.machine.effects(), halted_at);
    assert!(h1.machine.cordoned(), "a halted host stays out of service");
    assert!(!h1.machine.reboot_asked());
    let why = h1
        .record()
        .and_then(|record| record.halted)
        .expect("the turn halted");
    assert!(
        why.starts_with("apply linux: exit 1, install_failed: "),
        "{why}"
    );

    assert_eq!(h1.beat(), None);
    for _ in 0..2 {
        assert_eq!(h2.beat(), None, "a halted group starts no host");
        assert_eq!(h3.beat(), None);
    }
    let hosts: Vec<_> = world
        .hosts(&run)
        .iter()
        .map(|host| (host.host_id.clone(), host.step(), host.reason.clone()))
        .collect();
    assert_eq!(
        hosts,
        [
            ("h1".to_owned(), Step::Halted, why),
            ("h2".to_owned(), Step::Waiting, String::new()),
        ]
    );
    let mut alerts = updates_due();
    alerts.push((
        op::AlertKind::MaintenanceHalted,
        GROUP.to_owned(),
        "h1".to_owned(),
    ));
    assert_eq!(world.alerts(), alerts);
    assert_eq!(h1.machine.effects(), halted_at, "nothing more ran");

    world.put_back(&run, "h1");
    let put_back = Ordered {
        order_try: 2,
        action: pb::MaintainAction::PutBack,
        ..order.clone()
    };
    assert_eq!(h1.beat(), Some(put_back.clone()));
    assert_eq!(h1.beat(), None);
    let mut resumed = halted_at.clone();
    resumed.push(format!("resume {SERVICE}"));
    assert_eq!(h1.machine.effects(), resumed);
    assert!(!h1.machine.cordoned(), "put back in service");
    assert_eq!(h1.machine.running(), OLD_KERNEL);
    assert_eq!(world.alerts(), updates_due(), "h1's update is still due");

    assert_eq!(h2.beat(), Some(order.clone()));
    reboot_to_done(&mut h2);
    assert_eq!(h2.machine.effects(), kernel_turn());
    assert_eq!(
        world.steps(&run),
        [at("h1", Step::Done), at("h2", Step::Done)]
    );
    assert_eq!(h1.orders(), [order.clone(), put_back]);
    assert_eq!(h2.orders(), [order]);
    assert_eq!(h3.machine.effects(), Vec::<String>::new());
}

/// Proves: a daemon that stops in the middle of a turn (just after the drain) and a coordinator
/// that restarts, once while the turn is under way and once while the host reboots, neither
/// start the order again nor repeat a step: the new coordinator reads the turn from its store
/// and the host's report and orders nothing, and the new daemon's start resumes the turn from
/// its run record at the apply. The turn ends done with each step run once.
///
/// Catches: a coordinator that starts a host's turn again after a restart; a daemon that drains
/// again, or starts the turn over, after a restart; a turn lost across the restarts.
#[test]
fn restarts_of_the_coordinator_and_the_daemon_mid_run_resume_the_turn_once() {
    let (world, mut h1, _h2) = pair("linux-e2e-restart", Due::Kernel);
    h1.stop();
    let stop = after_drained(&h1, SERVICE);
    h1.start_crashing(stop);
    h1.report();

    let run = world.update(&["h1"]);
    let order = first_offer(&run, &[pb::MaintenanceKind::Linux]);
    assert_eq!(h1.beat(), Some(order.clone()));
    assert_eq!(
        h1.machine.effects(),
        ["stage".to_owned(), format!("drain {SERVICE}")]
    );
    assert_eq!(h1.beat(), None);

    world.restart();
    assert_eq!(h1.beat(), None, "the restarted coordinator starts nothing");
    assert_eq!(world.in_turn(&run), ["h1".to_owned()]);

    h1.stop();
    assert_eq!(h1.start(), Some(Driven::Waiting(Kind::Linux)));
    assert_eq!(h1.beat(), None);
    world.restart();
    reboot_to_done(&mut h1);

    assert_eq!(h1.machine.effects(), kernel_turn());
    assert_eq!(world.steps(&run), [at("h1", Step::Done)]);
    assert_eq!(h1.orders(), [order]);
    assert_eq!(h1.due(), Vec::<pb::MaintenanceKind>::new());
}

/// Proves: a host whose only due item is a restart (`run/reboot-required`, no package due, the
/// newest kernel already running) is ordered `reboot` alone; the real hook's `prepare` of it is
/// done, and its turn installs nothing: stage, drain, reboot, check in the new boot, resume, done,
/// the same kernel running and nothing due after.
///
/// Catches: a reboot-only run that runs the package manager's install, that ends without
/// rebooting, or whose stage fails `unsupported_kind` and defers the turn.
#[test]
fn a_reboot_only_run_installs_nothing_and_reboots() {
    let (world, mut h1, _h2) = pair("linux-e2e-reboot", Due::Reboot);
    assert_eq!(h1.due(), [pb::MaintenanceKind::Reboot]);

    let run = world.update(&["h1"]);
    let order = first_offer(&run, &[pb::MaintenanceKind::Reboot]);
    assert_eq!(h1.beat(), Some(order.clone()));
    reboot_to_done(&mut h1);

    assert_eq!(
        h1.machine.effects(),
        [
            "stage".to_owned(),
            format!("drain {SERVICE}"),
            "reboot".to_owned(),
            "boot boot-2".to_owned(),
            "check".to_owned(),
            format!("resume {SERVICE}"),
        ]
    );
    assert_eq!(h1.machine.running(), OLD_KERNEL);
    assert_eq!(world.steps(&run), [at("h1", Step::Done)]);
    assert_eq!(h1.due(), Vec::<pb::MaintenanceKind>::new());
    assert_eq!(h1.orders(), [order]);
}

/// Proves: the Update button on a host that reports both a `linux` item (a kernel update) and a
/// `reboot` item (`run/reboot-required`) orders `linux` and `reboot` together, and the real
/// hook's `prepare` of both is done, so the turn runs to done: stage, drain, install of the
/// kernel update, one reboot, the check in the new boot, resume. The host runs the new kernel,
/// its next report shows nothing due and the group's `updates_due` notice clears.
///
/// Catches: a `reboot` kind whose stage answers `unsupported_kind`, which defers the turn after
/// `stage` and leaves a host with a pending restart never updated by the Update button; or a run
/// of both kinds that reboots twice.
#[test]
fn the_update_button_runs_a_host_with_a_kernel_update_and_a_restart_due_to_done() {
    let (world, mut h1, _h2) = pair("linux-e2e-both", Due::KernelAndReboot);
    assert_eq!(
        h1.due(),
        [pb::MaintenanceKind::Reboot, pb::MaintenanceKind::Linux]
    );
    assert_eq!(world.alerts(), updates_due());

    let run = world.update(&["h1"]);
    let order = first_offer(
        &run,
        &[pb::MaintenanceKind::Linux, pb::MaintenanceKind::Reboot],
    );
    assert_eq!(h1.beat(), Some(order.clone()));
    reboot_to_done(&mut h1);

    assert_eq!(h1.machine.effects(), kernel_turn());
    assert_eq!(h1.machine.running(), NEW_KERNEL);
    assert!(!h1.machine.cordoned(), "the service is back in service");
    assert_eq!(world.steps(&run), [at("h1", Step::Done)]);
    assert_eq!(h1.due(), Vec::<pb::MaintenanceKind>::new());
    assert_eq!(world.alerts(), no_alerts());
    assert_eq!(h1.orders(), [order]);
}

/// Proves: a drain that reaches the run's drain deadline while work holds it puts the host back
/// unchanged and defers its turn: the drained service is resumed, nothing is installed or
/// rebooted, the host is back in service with its update still due, and the run shows it
/// deferred with the daemon's reason; it is not ordered again at its next heartbeat.
///
/// Catches: a drain deadline that halts the host or leaves it cordoned; an update installed after
/// a deferred drain; a deferred host ordered again at once.
#[test]
fn a_drain_deadline_puts_the_host_back_unchanged_and_defers_it() {
    let (world, mut h1, _h2) = pair("linux-e2e-deadline", Due::Kernel);
    h1.machine.keep_busy();

    let run = world.schedule(op::ScheduleMaintenanceRequest {
        host_ids: vec!["h1".to_owned()],
        drain_deadline_secs: Some(600),
        ..op::ScheduleMaintenanceRequest::default()
    });
    let order = first_offer(&run, &[pb::MaintenanceKind::Linux]);
    assert_eq!(h1.beat(), Some(order.clone()));

    let why = format!("drain {SERVICE}: exit 75");
    assert_eq!(
        h1.record().and_then(|record| record.deferred),
        Some(why.clone())
    );
    assert_eq!(h1.beat(), None);
    let hosts: Vec<_> = world
        .hosts(&run)
        .iter()
        .map(|host| (host.host_id.clone(), host.step(), host.reason.clone()))
        .collect();
    assert_eq!(hosts, [("h1".to_owned(), Step::Deferred, why)]);
    assert_eq!(h1.beat(), None, "a deferred host waits its backoff");

    assert_eq!(
        h1.machine.effects(),
        [
            "stage".to_owned(),
            format!("drain {SERVICE}"),
            format!("resume {SERVICE}"),
        ]
    );
    assert!(!h1.machine.cordoned(), "put back in service");
    assert!(!h1.machine.reboot_asked());
    assert_eq!(h1.machine.running(), OLD_KERNEL);
    assert_eq!(h1.due(), [pb::MaintenanceKind::Linux]);
    assert_eq!(world.alerts(), updates_due());
    assert_eq!(h1.orders(), [order]);
}
