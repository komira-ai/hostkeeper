//! The run state of a host's maintenance turn in the heartbeat: the report that carries the run
//! record's state (`RunReported`), the heartbeat loop that hands a `maintain` order to the daemon
//! and then reports its turn, the loop's runner of the order (`DaemonMaintain`), and the
//! command-line wiring of the maintenance source (`cli::maintenance_source`). Every step runs
//! through the rig's `ScriptedLauncher`; no program runs and no clock is read.

mod support;
mod support_maintain;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use hostkeeper_daemon::Reply;
use hostkeeper_daemon::cli::DEFAULT_PACKAGE_LOCK_WAIT_SECS;
use hostkeeper_daemon::cli::maintenance_source;
use hostkeeper_daemon::executor::Daemon;
use hostkeeper_daemon::link_adapters::DaemonMaintain;
use hostkeeper_daemon::links::Links;
use hostkeeper_daemon::maintenance_apt::Apt;
use hostkeeper_daemon::maintenance_collect::{Collector, LinuxScope};
use hostkeeper_daemon::maintenance_state::{LookAgain, RunReported, run_state};
use hostkeeper_daemon::testing::RecordedWaits;
use hostkeeper_hook::Verdict;
use hostkeeper_launchd::{FakeLaunchctl, LaunchctlOutput, PlatformKind};
use hostkeeper_link::testing::{
    FakeCerts, FakeClock, FakeLink, ManualDispatch, RecordingLog, ScriptedSleeper, identity,
};
use hostkeeper_link::{
    BeatOutcome, CertState, DEFAULT_HEARTBEAT_SECS, Dispatch, Feature, Heartbeater, Job,
    MaintainOrder, MaintainSink, MaintenanceSource, OrderId, Reply as LinkReply, StopFlag,
};
use hostkeeper_placed::InstalledEntry;
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_steps::maintenance::{
    Kind, MAINTENANCE_RECORD_FILE, MaintenanceRecord, MaintenanceStore, RunStep,
};
use support::{Rig, alerting, lines};
use support_maintain::{HOOK, ORDER, RUN, launched, record, terms};

/// `apt-get --simulate -qq dist-upgrade` on an Ubuntu 24.04 host.
const UBUNTU: &str = include_str!("fixtures/maintenance/apt/ubuntu_2404.txt");

/// The time of the last look the fake facts report.
const LOOKED: u64 = 1_700_000_000;

/// Facts that report a fixed look and count how often they are asked to look again.
struct Facts {
    looks: Arc<AtomicUsize>,
}

impl MaintenanceSource for Facts {
    fn report(&self) -> pb::MaintenanceReport {
        facts_report()
    }
}

impl LookAgain for Facts {
    fn look_again(&self) {
        self.looks.fetch_add(1, Ordering::SeqCst);
    }
}

/// What the fake facts report, with no run.
fn facts_report() -> pb::MaintenanceReport {
    pb::MaintenanceReport {
        looked_unix: LOOKED,
        ..pb::MaintenanceReport::default()
    }
}

/// A source over fake facts and the record beside `rig`'s state directory, and the count of
/// looks it asked the facts for.
fn source(rig: &Rig) -> (RunReported<Facts>, Arc<AtomicUsize>) {
    let looks = Arc::new(AtomicUsize::new(0));
    let facts = Facts {
        looks: Arc::clone(&looks),
    };
    (RunReported::new(facts, &rig.state, Vec::new()), looks)
}

/// Writes `record` as the rig's run record.
fn save(rig: &Rig, record: &MaintenanceRecord) {
    let store = MaintenanceStore::beside(&rig.state);
    std::fs::create_dir_all(store.dir()).expect("the record's directory");
    store.save(record).expect("the record is written");
}

/// The record of a new turn of `order_id` of the kind `kinds`: at `stage`, nothing launched.
fn fresh(order_id: &str, kinds: &[Kind]) -> MaintenanceRecord {
    MaintenanceRecord::new(RUN, order_id, 1, kinds, Vec::new()).expect("a valid record")
}

/// The record of the turn of `order_id` that is done: every kind applied.
fn done(order_id: &str, kinds: &[Kind]) -> MaintenanceRecord {
    let mut record = fresh(order_id, kinds);
    record.step = RunStep::Done;
    record.kinds.iter_mut().for_each(|kind| kind.applied = true);
    record
}

/// The run state the report gives for `order_id`.
fn state(
    order_id: &str,
    step: pb::MaintenanceStep,
    applied: &[pb::MaintenanceKind],
    reason: &str,
) -> pb::MaintenanceRunState {
    pb::MaintenanceRunState {
        run_id: RUN.to_owned(),
        order_id: order_id.to_owned(),
        step: step.into(),
        applied: applied.iter().map(|kind| i32::from(*kind)).collect(),
        halted_reason: reason.to_owned(),
    }
}

/// The facts' report with `run`.
fn with_run(run: pb::MaintenanceRunState) -> pb::MaintenanceReport {
    pb::MaintenanceReport {
        run: Some(run),
        ..facts_report()
    }
}

/// Proves: with no run record on the host, the report is the facts' report exactly, with no run,
/// and the facts are not asked to look again.
///
/// Catches: a report that invents a run state for a host that holds no turn, or a look asked for
/// when no turn ended.
#[test]
fn a_host_with_no_record_reports_the_facts_and_no_run() {
    let rig = Rig::new("report-no-record");
    let (reported, looks) = source(&rig);
    assert_eq!(reported.report(), facts_report());
    assert_eq!(facts_report().run, None);
    assert_eq!(looks.load(Ordering::SeqCst), 0);
}

/// Proves: a run record that does not read leaves the run out of the report, which is otherwise
/// the facts' report whole.
///
/// Catches: a report that fails, or that reports a run, for a record it cannot read.
#[test]
fn a_record_that_does_not_read_leaves_the_run_out() {
    let rig = Rig::new("report-corrupt");
    let store = MaintenanceStore::beside(&rig.state);
    std::fs::create_dir_all(store.dir()).expect("the record's directory");
    std::fs::write(store.dir().join(MAINTENANCE_RECORD_FILE), "not json").expect("a bad record");
    let (reported, looks) = source(&rig);
    assert_eq!(reported.report(), facts_report());
    assert_eq!(looks.load(Ordering::SeqCst), 0);
}

/// Proves: with a record, the report is the facts' report with the run state of the record
/// (`waiting` for a turn not launched, `staging` once its first step runs), read afresh at each
/// report.
///
/// Catches: a record read once and kept, so that a turn that moved on is reported at the step it
/// was first seen at.
#[test]
fn the_report_carries_the_run_state_of_the_record_at_every_report() {
    let rig = Rig::new("report-run-state");
    let (reported, looks) = source(&rig);

    let mut turn = fresh(ORDER, &[Kind::Linux]);
    save(&rig, &turn);
    let waiting = state(ORDER, pb::MaintenanceStep::Waiting, &[], "");
    assert_eq!(run_state(&turn), waiting);
    assert_eq!(reported.report(), with_run(waiting));

    turn.running = true;
    save(&rig, &turn);
    let staging = state(ORDER, pb::MaintenanceStep::Staging, &[], "");
    assert_eq!(reported.report(), with_run(staging));
    assert_eq!(looks.load(Ordering::SeqCst), 0);
}

/// Proves: the kinds a record holds applied are reported in the order they were applied, and a
/// record of a turn that is done reports `done` with no reason.
///
/// Catches: a done turn reported with the kinds it did not apply, or in another order.
#[test]
fn a_done_turn_reports_the_kinds_it_applied_in_order() {
    let rig = Rig::new("report-applied");
    let (reported, _) = source(&rig);
    save(&rig, &done(ORDER, &[Kind::Reboot, Kind::Linux]));
    let expected = state(
        ORDER,
        pb::MaintenanceStep::Done,
        &[pb::MaintenanceKind::Linux, pb::MaintenanceKind::Reboot],
        "",
    );
    assert_eq!(reported.report(), with_run(expected));
}

/// Proves: the facts are asked to look again once when the record first reads done, not at the
/// next report of the same turn, and once more for the turn of another order id.
///
/// Catches: a look asked at every report of a finished turn (a look at every beat), or never
/// asked, so that what the update made due or not due is not seen until the hour is out.
#[test]
fn the_facts_look_again_once_per_ended_turn() {
    let rig = Rig::new("report-look-done");
    let (reported, looks) = source(&rig);

    save(&rig, &done(ORDER, &[Kind::Linux]));
    let first = state(
        ORDER,
        pb::MaintenanceStep::Done,
        &[pb::MaintenanceKind::Linux],
        "",
    );
    assert_eq!(reported.report(), with_run(first.clone()));
    assert_eq!(looks.load(Ordering::SeqCst), 1);
    assert_eq!(reported.report(), with_run(first));
    assert_eq!(looks.load(Ordering::SeqCst), 1);

    save(&rig, &done("order-2", &[Kind::Linux]));
    let second = state(
        "order-2",
        pb::MaintenanceStep::Done,
        &[pb::MaintenanceKind::Linux],
        "",
    );
    assert_eq!(reported.report(), with_run(second.clone()));
    assert_eq!(looks.load(Ordering::SeqCst), 2);
    assert_eq!(reported.report(), with_run(second));
    assert_eq!(looks.load(Ordering::SeqCst), 2);
}

/// Proves: a turn that ended deferred is reported `deferred` with the record's reason, and asks
/// the facts to look again once, as a done turn does.
///
/// Catches: only a done turn asking for a look, though a deferred turn put the host back as it
/// was and a drained service may have changed what is due.
#[test]
fn a_deferred_turn_reports_its_reason_and_asks_for_one_look() {
    let rig = Rig::new("report-look-deferred");
    let (reported, looks) = source(&rig);
    let mut turn = done(ORDER, &[Kind::Linux]);
    turn.kinds.iter_mut().for_each(|kind| kind.applied = false);
    turn.deferred = Some("stage: exit 75, disk_full: no room for the download".to_owned());
    save(&rig, &turn);
    let expected = with_run(state(
        ORDER,
        pb::MaintenanceStep::Deferred,
        &[],
        "stage: exit 75, disk_full: no room for the download",
    ));
    assert_eq!(reported.report(), expected);
    assert_eq!(reported.report(), expected);
    assert_eq!(looks.load(Ordering::SeqCst), 1);
}

/// Proves: a turn that is running, one that waits to be launched and one that is halted never
/// ask the facts to look again, however often they are reported.
///
/// Catches: a look asked of a host that is out of service or in the middle of a turn, where what
/// is installed is not settled.
#[test]
fn a_turn_that_has_not_ended_or_that_halted_asks_for_no_look() {
    let rig = Rig::new("report-no-look");
    let (reported, looks) = source(&rig);

    let mut turn = fresh(ORDER, &[Kind::Linux]);
    save(&rig, &turn);
    reported.report();
    turn.running = true;
    save(&rig, &turn);
    reported.report();
    turn.running = false;
    turn.halted = Some("apply linux: exit 1".to_owned());
    save(&rig, &turn);
    let halted = with_run(state(
        ORDER,
        pb::MaintenanceStep::Halted,
        &[],
        "apply linux: exit 1",
    ));
    assert_eq!(reported.report(), halted);
    assert_eq!(reported.report(), halted);
    assert_eq!(looks.load(Ordering::SeqCst), 0);
}

/// A heartbeat loop over a daemon on `rig` whose maintenance source is [`RunReported`] over fake
/// facts, its coordinator, the dispatcher that holds the loop's jobs, and the count of looks.
struct Host {
    heartbeater: Heartbeater,
    link: Arc<FakeLink>,
    dispatch: Arc<ManualDispatch>,
    looks: Arc<AtomicUsize>,
}

fn host(rig: &Rig) -> Host {
    let daemon = Arc::new(rig.daemon());
    let link = Arc::new(FakeLink::default());
    let dispatch = Arc::new(ManualDispatch::default());
    let (reported, looks) = source(rig);
    let maintenance: Arc<dyn MaintenanceSource> = Arc::new(reported);
    let links = Links {
        enroller: Box::new(|| Err("no enrollment here".to_owned())),
        link: link.clone(),
        certs: Arc::new(FakeCerts::new(CertState::Enrolled(identity("host-1")))),
        clock: Arc::new(FakeClock::new(2_000_000_000, 7)),
        log: Arc::new(RecordingLog::default()),
        sleeper: Box::new(ScriptedSleeper::new(0)),
        period: DEFAULT_HEARTBEAT_SECS,
        dispatch: dispatch.clone(),
        stop: StopFlag::default(),
        enroll_sleeper: Box::new(ScriptedSleeper::new(0)),
        renewal: hostkeeper_enroll::RenewalState::default(),
        watchdog: Box::new(hostkeeper_daemon::NoWatchdog),
        console: None,
        identity: None,
        remote_login_probe: None,
        full_disk_access: None,
        operator_keys: None,
        prerequisites: None,
        maintenance: Some(maintenance),
        bmc: None,
        serial_console: None,
        health: None,
    };
    Host {
        heartbeater: links.heartbeater(&daemon),
        link,
        dispatch,
        looks,
    }
}

/// The reply that advertises `maintenance-v1` and carries the order [`ORDER`], try 1, of a run of
/// the Linux kind.
fn orders_a_run() -> LinkReply {
    LinkReply {
        features: vec![Feature::MaintenanceV1],
        maintain: Some(MaintainOrder {
            id: OrderId::new(ORDER).expect("an order id"),
            order_try: 1,
            maintain: terms(&[pb::MaintenanceKind::Linux], None),
        }),
        ..LinkReply::default()
    }
}

/// Proves, through the daemon's own heartbeat loop: a `maintain` order of a reply that advertises
/// `maintenance-v1` is held as a job and runs the host's turn on the daemon (the hook's three
/// steps launched, the record done); the next heartbeat announces `maintenance-v1` and carries the
/// facts' report with the run `done` of the order, and the facts were asked to look again once.
/// The heartbeat before the reply carried no maintenance report and did not announce the feature.
///
/// Catches: a `maintain` sink that is not wired when the maintenance source is, a report that does
/// not carry the run, or a loop that announces the feature before the coordinator advertised it.
#[test]
fn a_maintain_order_of_a_reply_runs_the_turn_and_the_next_beat_reports_it_done() {
    let rig = Rig::new("report-beat-done");
    let mut host = host(&rig);
    host.link.answer(Ok(orders_a_run()));

    assert_eq!(host.heartbeater.beat_once(), BeatOutcome::Sent);
    assert_eq!(host.dispatch.pending(), 1);
    assert_eq!(launched(&rig), Vec::<String>::new());
    let first = host.link.beats().remove(0);
    assert!(!first.features.contains(&Feature::MaintenanceV1));
    assert_eq!(first.maintenance, None);

    assert!(host.dispatch.run_next());
    assert_eq!(
        launched(&rig),
        [
            format!("{HOOK} prepare"),
            format!("{HOOK} cutover"),
            format!("{HOOK} verify"),
        ]
    );
    assert_eq!(
        record(&rig).map(|held| held.step),
        Some(RunStep::Done),
        "the record of the turn is done"
    );

    assert_eq!(host.heartbeater.beat_once(), BeatOutcome::Sent);
    let second = host.link.beats().remove(1);
    assert!(second.features.contains(&Feature::MaintenanceV1));
    assert_eq!(
        second.maintenance,
        Some(with_run(state(
            ORDER,
            pb::MaintenanceStep::Done,
            &[pb::MaintenanceKind::Linux],
            ""
        )))
    );
    assert_eq!(host.looks.load(Ordering::SeqCst), 1);
}

/// Proves, through the heartbeat loop: a turn whose `cutover` fails halts where it is (no `verify`
/// is launched), and the next heartbeat reports `halted` with the record's reason, no kind applied,
/// and no look asked of the facts.
///
/// Catches: a halted turn reported as done or as not started, or a look asked for a host that
/// stays out of service.
#[test]
fn a_turn_that_halts_is_reported_halted_with_its_reason_by_the_next_beat() {
    let rig = Rig::new("report-beat-halted");
    rig.launcher.additive_exits(HOOK, &[0, 1]);
    let mut host = host(&rig);
    host.link.answer(Ok(orders_a_run()));

    assert_eq!(host.heartbeater.beat_once(), BeatOutcome::Sent);
    assert!(host.dispatch.run_next());
    assert_eq!(
        launched(&rig),
        [format!("{HOOK} prepare"), format!("{HOOK} cutover")]
    );

    assert_eq!(host.heartbeater.beat_once(), BeatOutcome::Sent);
    let beat = host.link.beats().remove(1);
    assert_eq!(
        beat.maintenance,
        Some(with_run(state(
            ORDER,
            pb::MaintenanceStep::Halted,
            &[],
            "apply linux: exit 1"
        )))
    );
    assert_eq!(host.looks.load(Ordering::SeqCst), 0);
}

/// Proves: the loop's runner of `maintain` orders runs the turn it is given on the daemon (the
/// hook's three steps launched, the record done) and logs the daemon's answer, `Finished` with
/// the verdict `Succeeded`, as an alert line; a resend of the same order and try starts nothing
/// and logs the answer again as `resent`.
///
/// Catches: a runner that drops the order, one that starts a finished turn again, one that
/// answers nothing to the log, or one that logs the same answer at every resend.
#[test]
fn the_runner_runs_the_turn_and_logs_the_answer_and_a_resend_starts_nothing() {
    let rig = Rig::new("report-runner");
    let (daemon, log) = alerting(rig.daemon());
    let runner = DaemonMaintain::new(Arc::new(daemon));
    let order = MaintainOrder {
        id: OrderId::new(ORDER).expect("an order id"),
        order_try: 1,
        maintain: terms(&[pb::MaintenanceKind::Linux], None),
    };
    let answer = |resent: bool| {
        format!(
            "hostkeeper daemon: maintain order {ORDER} try 1: {:?}",
            Reply::Finished {
                verdict: Verdict::Succeeded {
                    promotion_pending: false
                },
                reason: None,
                resent,
            }
        )
    };

    runner.maintain(&order);
    assert_eq!(
        launched(&rig),
        [
            format!("{HOOK} prepare"),
            format!("{HOOK} cutover"),
            format!("{HOOK} verify"),
        ]
    );
    assert_eq!(record(&rig).map(|held| held.step), Some(RunStep::Done));
    assert_eq!(lines(&log).last(), Some(&answer(false)));

    let said = lines(&log);
    runner.maintain(&order);
    assert_eq!(launched(&rig).len(), 3);
    // The answer from the record to a resend is not logged again at every heartbeat.
    assert_eq!(lines(&log), said);
    assert!(!said.contains(&answer(true)));
}

/// Proves: an order the daemon refuses (an action that is not given) is logged by the runner with
/// the refusal and its detail, nothing is launched, and no record is written.
///
/// Catches: a refusal the runner swallows, so that a coordinator's order that cannot run leaves no
/// line on the host to say why.
#[test]
fn a_refused_order_is_logged_with_its_refusal_and_runs_nothing() {
    let rig = Rig::new("report-refused");
    let (daemon, log) = alerting(rig.daemon());
    let runner = DaemonMaintain::new(Arc::new(daemon));
    let order = MaintainOrder {
        id: OrderId::new(ORDER).expect("an order id"),
        order_try: 1,
        maintain: pb::Maintain {
            action: pb::MaintainAction::Unspecified.into(),
            ..terms(&[pb::MaintenanceKind::Linux], None)
        },
    };
    runner.maintain(&order);
    let detail = format!(
        "the order's maintain.action {}",
        hostkeeper_daemon::verb::ACTION_RULE
    );
    assert_eq!(
        lines(&log),
        [format!(
            "hostkeeper daemon: maintain order {ORDER} try 1: Refused {{ refusal: InvalidOrder, detail: {detail:?} }}"
        )]
    );
    assert_eq!(launched(&rig), Vec::<String>::new());
    assert_eq!(record(&rig), None);
}

/// A dispatcher that runs each job where it is started.
struct Inline;

impl Dispatch for Inline {
    fn start(&self, job: Job) {
        job();
    }
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|arg| (*arg).to_owned()).collect()
}

/// Proves: the maintenance source the command line wires (`cli::maintenance_source`) reports the
/// run of the record beside the daemon's state directory: with no record the run is absent, and
/// once the record of a done turn is saved the next report carries its run state, with the
/// facts of the look beside it.
///
/// Catches: a source made over the bare facts, so that the wired daemon never reports its turn.
#[test]
fn the_wired_source_reports_the_run_of_the_record() {
    let rig = Rig::new("report-wired");
    let mut placed = rig.placed_record();
    let mut entry = InstalledEntry::new("apt");
    entry.package = Some("openssl".to_owned());
    placed.installed.insert("tls".to_owned(), entry);
    rig.save_placed(&placed);
    let daemon = Arc::new(Daemon::new(
        support::config(),
        rig.dirs(),
        rig.ports(),
        support::OWN,
    ));

    let apt_get = FakeLaunchctl::new();
    apt_get.always(&args(&["update", "-qq"]), Ok(LaunchctlOutput::success("")));
    apt_get.always(
        &args(&["--simulate", "-qq", "dist-upgrade"]),
        Ok(LaunchctlOutput::success(UBUNTU)),
    );
    let collectors: Vec<Box<dyn Collector>> = vec![Box::new(Apt::new(
        Box::new(apt_get.clone()),
        Box::new(apt_get),
        RecordedWaits::bounded(DEFAULT_PACKAGE_LOCK_WAIT_SECS).0,
    ))];
    let source = maintenance_source(
        &daemon,
        PlatformKind::Linux,
        LinuxScope::Security,
        collectors,
        Arc::new(FakeClock::new(2_000_000_000, 0)),
        Arc::new(Inline),
        Arc::new(RecordingLog::default()),
    )
    .expect("a host with a collector has a maintenance source");

    source.report();
    let before = source.report();
    assert_eq!(before.run, None);
    assert_eq!(before.look_failed, "");

    let turn = done(ORDER, &[Kind::Linux]);
    save(&rig, &turn);
    let after = source.report();
    assert_eq!(
        after.run,
        Some(state(
            ORDER,
            pb::MaintenanceStep::Done,
            &[pb::MaintenanceKind::Linux],
            ""
        ))
    );
    assert_eq!(after.run, Some(run_state(&turn)));
    assert_eq!(after.host, before.host);
}
