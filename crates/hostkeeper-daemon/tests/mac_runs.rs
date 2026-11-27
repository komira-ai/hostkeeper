//! The `runs` ring of a Mac (`hostkeeper_daemon::mac_runs`): launchd's counts of a service's jobs
//! turned into the restarts of the last ten minutes, sample by sample at the test's Unix seconds
//! (the first observation counting none, a restart older than the window dropped, a count that
//! fell or a clock stepped back starting the job again, the samples' bound, jobs and services
//! forgotten), whether each observation is one the state file must be written for, and the ring
//! kept in the state file across a restart of the daemon. Nothing reads a clock or a host's files
//! but a scratch state directory.

mod support;

use std::sync::Arc;

use hostkeeper_daemon::health_state::{self, Read};
use hostkeeper_daemon::host_health::{BootFacts, HealthKeeper};
use hostkeeper_daemon::links::counts_runs;
use hostkeeper_daemon::mac_runs::{MAX_RUN_SAMPLES, RECENT_SECS, RunSample, RunsRing};
use hostkeeper_daemon::shutdown_probe::{GoingDown, ShutdownProbe};
use hostkeeper_link::Platform;
use hostkeeper_link::testing::RecordingLog;
use hostkeeper_observe::{RunUnitRestarts, UnitRestarts};
use hostkeeper_proto::hostkeeper::agent::v1::ServiceRestarts;
use support::Scratch;

/// The second the tests begin at.
const NOW: u64 = 1_795_662_000;

const JOB: &str = "dev.hostkeeper.web-main";
const OTHER: &str = "dev.hostkeeper.web-standby";

fn unit(label: &str, count: u32, result: &str, exit_status: Option<i32>) -> RunUnitRestarts {
    RunUnitRestarts {
        unit: label.to_owned(),
        restarts: UnitRestarts {
            count,
            result: result.to_owned(),
            exit_status,
        },
    }
}

/// [`JOB`] alone, at `count`, its last run ended by exit code 1.
fn job(count: u32) -> Vec<RunUnitRestarts> {
    vec![unit(JOB, count, "exit-code", Some(1))]
}

fn restarts(count: u32, recent: u32) -> Option<ServiceRestarts> {
    Some(ServiceRestarts {
        count,
        recent,
        last_result: "exit-code".to_owned(),
        exit_status: Some(1),
    })
}

fn samples(ring: &RunsRing, service: &str, label: &str) -> Vec<RunSample> {
    ring.services
        .get(service)
        .and_then(|jobs| jobs.get(label))
        .cloned()
        .unwrap_or_default()
}

/// Proves a job's restarts: none at its first observation; each rise since counted; a count that
/// does not change keeps no sample (nothing to write); a rise more than ten minutes old dropped,
/// the count at the window's start kept as its base.
///
/// Catches: a Mac's crash loop never counted, the whole of launchd's count sent as recent, the
/// state file written at every heartbeat, or an old restart counted for good.
#[test]
fn a_jobs_rises_in_the_last_ten_minutes_are_its_recent_restarts() {
    let mut ring = RunsRing::default();
    assert_eq!(ring.record("web", &job(3), NOW), (restarts(3, 0), true));
    assert_eq!(
        ring.record("web", &job(3), NOW + 60),
        (restarts(3, 0), false)
    );
    assert_eq!(
        ring.record("web", &job(5), NOW + 120),
        (restarts(5, 2), true)
    );
    assert_eq!(
        ring.record("web", &job(8), NOW + 300),
        (restarts(8, 5), true)
    );
    assert_eq!(
        samples(&ring, "web", JOB),
        vec![
            RunSample { at: NOW, count: 3 },
            RunSample {
                at: NOW + 120,
                count: 5
            },
            RunSample {
                at: NOW + 300,
                count: 8
            },
        ]
    );

    let after = NOW + 120 + RECENT_SECS;
    assert_eq!(ring.record("web", &job(8), after), (restarts(8, 3), false));
    assert_eq!(
        samples(&ring, "web", JOB),
        vec![
            RunSample {
                at: NOW + 120,
                count: 5
            },
            RunSample {
                at: NOW + 300,
                count: 8
            },
        ],
        "the count when the window began is its base"
    );
    assert_eq!(
        ring.record("web", &job(8), NOW + 300 + RECENT_SECS),
        (restarts(8, 0), false)
    );
}

/// Proves a count that fell (launchd's count started again by a `bootout` and `bootstrap`, as a
/// deploy does) and a time before the last sample's each start the job again, counting none.
///
/// Catches: a deploy's reload counted as a crash, or a clock stepped back counting a rise.
#[test]
fn a_count_that_fell_or_a_clock_stepped_back_starts_again() {
    let mut ring = RunsRing::default();
    ring.record("web", &job(10), NOW);
    assert_eq!(
        ring.record("web", &job(14), NOW + 60),
        (restarts(14, 4), true)
    );
    assert_eq!(
        ring.record("web", &job(0), NOW + 120),
        (restarts(0, 0), true)
    );
    assert_eq!(
        samples(&ring, "web", JOB),
        vec![RunSample {
            at: NOW + 120,
            count: 0
        }]
    );
    assert_eq!(
        ring.record("web", &job(2), NOW + 180),
        (restarts(2, 2), true)
    );
    assert_eq!(
        ring.record("web", &job(3), NOW + 30),
        (restarts(3, 0), true)
    );
    assert_eq!(
        samples(&ring, "web", JOB),
        vec![RunSample {
            at: NOW + 30,
            count: 3
        }]
    );
}

/// Proves at most [`MAX_RUN_SAMPLES`] samples of a job are kept, the oldest dropped first, so a
/// job rising faster than that within the window still counts at least that many.
///
/// Catches: a ring that grows without bound in the state file.
#[test]
fn a_job_keeps_at_most_its_bound_of_samples() {
    let mut ring = RunsRing::default();
    let bound = u32::try_from(MAX_RUN_SAMPLES).expect("a small bound");
    for count in 0..=bound + 5 {
        ring.record("web", &job(count), NOW + u64::from(count));
    }
    let kept = samples(&ring, "web", JOB);
    assert_eq!(kept.len(), MAX_RUN_SAMPLES);
    assert_eq!(kept.first().map(|s| s.count), Some(6));
    assert_eq!(
        ring.record("web", &job(bound + 5), NOW + 100),
        (restarts(bound + 5, bound - 1), false)
    );
}

/// Proves the service's restarts sum its jobs (the first job whose result is not `success` gives
/// the result), a job that leaves the run set is forgotten (a write), and a service observed with
/// no counts has no restarts and is forgotten: a write when it was held, none when it was not.
///
/// Catches: the restarts of a job no longer run kept, or an unobserved service's old count
/// reported or held for good.
#[test]
fn jobs_and_services_no_longer_counted_are_forgotten() {
    let mut ring = RunsRing::default();
    let both = |main: u32, standby: u32| {
        vec![
            unit(JOB, main, "success", Some(0)),
            unit(OTHER, standby, "signal", None),
        ]
    };
    ring.record("web", &both(1, 4), NOW);
    assert_eq!(
        ring.record("web", &both(2, 6), NOW + 60),
        (
            Some(ServiceRestarts {
                count: 8,
                recent: 3,
                last_result: "signal".to_owned(),
                exit_status: None,
            }),
            true
        )
    );
    assert_eq!(
        ring.record("web", &job(2), NOW + 120),
        (restarts(2, 1), true),
        "the job's rise since its first count stays; the job gone is forgotten"
    );
    assert!(samples(&ring, "web", OTHER).is_empty());
    assert_eq!(ring.record("web", &[], NOW + 180), (None, true));
    assert!(ring.services.is_empty());
    assert_eq!(ring.record("web", &[], NOW + 240), (None, false));
    assert_eq!(ring.record("db", &job(1), NOW), (restarts(1, 0), true));
    assert_eq!(
        ring.services.keys().map(String::as_str).collect::<Vec<_>>(),
        ["db"]
    );
}

struct Never;

impl ShutdownProbe for Never {
    fn going_down(&self) -> GoingDown {
        GoingDown::No("a test".to_owned())
    }
}

fn keeper(dir: &Scratch) -> HealthKeeper {
    HealthKeeper::open(
        dir.path(),
        BootFacts {
            boot_id: Some("boot-1".to_owned()),
            boot_unix: NOW - 3600,
            machine_id: String::new(),
        },
        Box::new(Never),
        Arc::new(RecordingLog::default()),
    )
}

fn on_file(dir: &Scratch) -> RunsRing {
    match health_state::read(dir.path(), "") {
        Read::Read(file) => file.runs,
        other => panic!("the state file is not read: {other:?}"),
    }
}

/// Proves the keeper holds the ring in its state file: a restart of the daemon (a new keeper over
/// the same directory) counts a rise since a count the daemon before it saw, and the file holds
/// each sample kept; a service forgotten leaves the file.
///
/// Catches: a Mac daemon in a crash loop of its own never counting its restarts (each new process
/// starting an empty window), or the ring not written.
#[test]
fn the_ring_survives_a_restart_of_the_daemon() {
    let dir = Scratch::new("mac-runs-keeper");
    let first = keeper(&dir);
    assert_eq!(first.runs("hostkeeper", &job(1), NOW), restarts(1, 0));
    assert_eq!(
        on_file(&dir).services["hostkeeper"][JOB],
        vec![RunSample { at: NOW, count: 1 }]
    );
    drop(first);

    let second = keeper(&dir);
    assert_eq!(second.runs("hostkeeper", &job(4), NOW + 90), restarts(4, 3));
    assert_eq!(on_file(&dir).services["hostkeeper"][JOB].len(), 2);
    assert_eq!(second.runs("hostkeeper", &[], NOW + 120), None);
    assert!(on_file(&dir).services.is_empty());
}

/// Proves only a Mac's daemon counts its services' restarts in the ring.
///
/// Catches: a Linux daemon writing its state file at every restart of a unit, or a Mac's
/// restarts counted in memory only.
#[test]
fn only_a_mac_counts_runs() {
    assert!(counts_runs(Platform::DarwinArm64));
    assert!(!counts_runs(Platform::LinuxX86_64));
    assert!(!counts_runs(Platform::LinuxArm64));
}
