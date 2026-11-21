//! The alerts of the maintenance runs, derived from the ops state alone: `maintenance_halted` for a
//! host whose turn is stored halted, `maintenance_not_back` for one stored not back, and none for
//! any other step. The coordinator's note that a rebooting host did not come back, and the
//! `silent_host` it spares, are in `alerts_not_back_tests.rs`.

use std::time::Duration;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use crate::alerts::{Alert, At, EnrollmentFacts, Kind, Subject, Thresholds, derive};
use crate::fleet::{Fleet, QUIET_START};
use crate::host_groups_support::UNIX;
use crate::maintenance_calls_tests::{host_at, run_of};
use crate::maintenance_runs::{Act, Kind as RunKind, Run, RunHost, Step};
use crate::ops::OpsState;
use crate::testutil::{kbf_services, secs};

/// What the message of every halted host says after the reason its daemon gave.
const HALTED_TAIL: &str = "; the host stays cordoned and no further host of its group starts in \
     the run until an operator acts with Maintenance.RetryHost or Maintenance.PutBack \
     (Maintenance.Cancel ends the run but leaves the host halted)";

/// A turn of `id` in `group` (empty: none) at `step`, since [`UNIX`], applying a Linux update.
fn turn(id: &str, group: &str, step: Step) -> RunHost {
    host_at(id, group, step, UNIX, &[RunKind::Linux])
}

/// A turn of `id` in `group` stored halted, with `reason` as its daemon worded it.
fn halted(id: &str, group: &str, reason: &str) -> RunHost {
    RunHost {
        reason: reason.to_owned(),
        ..turn(id, group, Step::Halted)
    }
}

/// The alerts of a coordinator that has heard no host, over `runs`.
fn derived(runs: Vec<Run>) -> Vec<Alert> {
    let ops = OpsState {
        maintenance_runs: runs,
        ..OpsState::default()
    };
    derive(
        &Fleet::new(Duration::ZERO, QUIET_START),
        &ops,
        &kbf_services(),
        &EnrollmentFacts::default(),
        At {
            now: secs(1_000),
            unix: UNIX,
        },
        &Thresholds::default(),
    )
}

/// The subject of an alert of `kind` about `host` in `group`.
fn subject(kind: Kind, host: &str, group: &str) -> Subject {
    Subject {
        host: host.to_owned(),
        group: group.to_owned(),
        ..Subject::of(kind)
    }
}

/// Proves: a host whose turn is stored halted raises one `maintenance_halted`, critical, about
/// that host in the group its turn took it in, whose message names the host, the run, the group
/// and the reason its daemon gave, says that the host stays cordoned and its group starts no
/// further host, and names what an operator does; a host at any other step of the same run raises
/// none, and two halted hosts raise one each.
///
/// Catches: an alert for a host that is merely in its turn, one that leaves out the daemon's
/// reason or the run, and two hosts of a run folded into one alert.
#[test]
fn a_halted_turn_raises_a_critical_alert_naming_the_host_the_run_the_group_and_the_reason() {
    let run = run_of(
        "run-1",
        vec![
            halted("h1", "alpha", "apply linux: exit 1"),
            turn("h2", "alpha", Step::Applying),
            turn("h3", "alpha", Step::Done),
            turn("h4", "alpha", Step::Waiting),
            turn("h5", "alpha", Step::Deferred),
        ],
    );
    assert_eq!(
        derived(vec![run.clone()]),
        vec![Alert {
            subject: subject(Kind::MaintenanceHalted, "h1", "alpha"),
            message: format!(
                "h1 halted in its turn of maintenance run run-1, in host group alpha: its daemon \
                 said \"apply linux: exit 1\"{HALTED_TAIL}"
            ),
        }]
    );
    assert_eq!(
        Kind::MaintenanceHalted.severity(),
        proto::AlertSeverity::Critical
    );

    let mut two = run;
    two.hosts.push(halted("h6", "beta", "reboot: exit 2"));
    let alerts = derived(vec![two]);
    let halts: Vec<(&str, &str)> = alerts
        .iter()
        .map(|alert| (alert.subject.host.as_str(), alert.subject.group.as_str()))
        .collect();
    assert_eq!(halts, vec![("h1", "alpha"), ("h6", "beta")]);
}

/// Proves: a halted host whose daemon gave no reason, and one in no host group, are told so in
/// their messages, and the alert's group is empty.
///
/// Catches: a message that prints an empty quotation for a missing reason, or `in host group `
/// for a host the run took in no group.
#[test]
fn a_halted_turn_with_no_reason_and_no_group_says_so() {
    let alerts = derived(vec![run_of("run-2", vec![halted("h1", "", "")])]);
    assert_eq!(
        alerts,
        vec![Alert {
            subject: subject(Kind::MaintenanceHalted, "h1", ""),
            message: format!(
                "h1 halted in its turn of maintenance run run-2, in no host group: its daemon \
                 gave no reason{HALTED_TAIL}"
            ),
        }]
    );
}

/// Proves: a halted turn with an act queued for its daemon says that the retry, or the put back,
/// is sent to it, and one with none says nothing of it.
///
/// Catches: an alert that goes on telling the operator to act after the act is queued, or one
/// that names the other act.
#[test]
fn a_halted_turn_with_an_act_queued_says_the_act_is_sent_to_its_daemon() {
    let said = |queued: Option<Act>| {
        let held = RunHost {
            queued,
            ..halted("h1", "alpha", "x")
        };
        let [alert]: [Alert; 1] = derived(vec![run_of("run-3", vec![held])])
            .try_into()
            .expect("one alert");
        alert.message
    };
    let plain = format!(
        "h1 halted in its turn of maintenance run run-3, in host group alpha: its daemon said \
         \"x\"{HALTED_TAIL}"
    );
    assert_eq!(said(None), plain);
    assert_eq!(
        said(Some(Act::Retry)),
        format!("{plain}; a retry is sent to its daemon")
    );
    assert_eq!(
        said(Some(Act::PutBack)),
        format!("{plain}; a put back is sent to its daemon")
    );
}

/// Proves: a host whose turn is stored not back raises one `maintenance_not_back`, critical, whose
/// message names the host, the run, the group and when the step was stored, says that it stays
/// cordoned and its group starts no further host and that its turn goes on when it reports again;
/// a host rebooting raises none.
///
/// Catches: a not-back alert for a host that is only rebooting, one raised as a halt, and a
/// message that does not say the turn resumes when the host returns.
#[test]
fn a_turn_stored_not_back_raises_a_critical_alert_and_a_rebooting_one_none() {
    let run = run_of(
        "run-4",
        vec![
            turn("h1", "alpha", Step::NotBack),
            turn("h2", "alpha", Step::Rebooting),
        ],
    );
    assert_eq!(
        derived(vec![run]),
        vec![Alert {
            subject: subject(Kind::MaintenanceNotBack, "h1", "alpha"),
            message: format!(
                "h1 did not come back in its turn of maintenance run run-4, in host group \
                 alpha: it was rebooting and stayed silent past its group's return_secs, as \
                 noted at unix {UNIX}; it stays cordoned (its hold file keeps it so), no further \
                 host of its group starts in the run, and its turn goes on when it reports again"
            ),
        }]
    );
    assert_eq!(
        Kind::MaintenanceNotBack.severity(),
        proto::AlertSeverity::Critical
    );
}

/// Proves: a turn that has moved on from halted and from not back (checking, done, deferred, not
/// reached) raises neither alert: each stands only while the stored step is its own.
///
/// Catches: an alert that outlives the step that raised it.
#[test]
fn a_turn_that_moved_on_raises_neither_alert() {
    let steps = [
        Step::Waiting,
        Step::Staging,
        Step::Draining,
        Step::Applying,
        Step::Rebooting,
        Step::Checking,
        Step::PuttingBack,
        Step::Done,
        Step::Deferred,
        Step::NotReached,
        Step::Skipped,
    ];
    let hosts = steps
        .iter()
        .enumerate()
        .map(|(n, step)| turn(&format!("h{n}"), "alpha", *step))
        .collect();
    assert_eq!(derived(vec![run_of("run-5", hosts)]), Vec::new());
}
