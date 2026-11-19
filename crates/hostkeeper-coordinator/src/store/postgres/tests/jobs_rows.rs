//! The coordinator's periodic work over a database (`jobs`; `docs/design/coordinator-multi-instance.md`,
//! section 9): a tick claims the jobs that are due and runs the ones it got.
//!
//! The jobs' clock is a number each test moves (`PgJobs::with_clock`); a test that waits for the
//! background loop waits on the row it writes, yielding between looks. Nothing sleeps.

use std::sync::Arc;
use std::sync::PoisonError;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use hostkeeper_enrollment::enroll::{Claims, NewRequest};
use hostkeeper_enrollment::fs::RealFs;

use super::periodic::{clock, jobs_of, periods};
use super::{LOG, TestDatabase, logged, logging};
use crate::enrollment::tests::support::{Files, FixedClock, HostKey, NOW};
use crate::enrollment::{Enrollment, StoreAt, os_random, setup};
use crate::jobs::{Jobs, periodic, run_claimed};
use crate::marks_support::{WEB, operator, set};
use crate::service::Coordinator;
use crate::store::Postgres;
use crate::store::postgres::alert_rows::PgAlerts;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;

/// A coordinator over a database whose `web` is halted at a version of the test's own.
struct Node {
    db: TestDatabase,
    store: Postgres,
    coordinator: Arc<Coordinator>,
    jobs: Option<Arc<Jobs>>,
    now: Arc<AtomicU64>,
    enrollment: Option<Arc<Enrollment>>,
    version: String,
    /// Keeps the files of the test's enrollment.
    _dir: hostkeeper_testkit::Scratch,
}

/// How a node is made.
struct Plan {
    version: &'static str,
    enrolled: bool,
    /// The jobs it claims, if it claims any.
    periods: Option<Vec<(String, i64)>>,
    at: u64,
}

impl Plan {
    fn claiming(version: &'static str) -> Plan {
        Plan {
            version,
            enrolled: false,
            periods: Some(periods()),
            at: 5_000_000,
        }
    }
}

async fn node(test: &str, plan: Plan) -> Node {
    logging();
    let db = TestDatabase::new();
    let (store, loaded) = db.open("jobs").expect("the store opens");
    let mut rig = crate::testutil::Rig::new(test);
    let marker = rig.dir.path().join("halt.marker");
    let mut coordinator = super::coordinator::start_on_postgres(&rig, &store, loaded, &marker);
    let mut enrollment = None;
    if plan.enrolled {
        let records = Arc::new(crate::store::postgres::PgRecords::over(&store));
        let files = Files::in_dir(&rig.dir);
        let started = Arc::new(
            setup(
                Arc::new(RealFs),
                &crate::enrollment::Setup {
                    store: StoreAt::Records(records),
                    ..files.setup(true)
                },
                FixedClock::at(NOW),
                os_random(),
            )
            .expect("enrollment starts over the database"),
        );
        coordinator = coordinator.with_enrollment(Arc::clone(&started));
        enrollment = Some(started);
    }
    rig.coordinator = coordinator;
    set(&rig, WEB, plan.version, 1).await;
    rig.coordinator
        .halt(
            &operator(),
            proto::HaltRequest {
                service: WEB.to_owned(),
                reason: "a bad disk".to_owned(),
            },
        )
        .expect("halted");
    let crate::testutil::Rig {
        mut coordinator,
        dir,
        ..
    } = rig;
    let (now, at) = clock(plan.at);
    if let Some(periods) = plan.periods {
        let pg = jobs_of(&db, &store, periods, &at);
        coordinator = coordinator.with_jobs(
            Jobs::new(pg, Duration::from_secs(3600)),
            PgAlerts::new(store.clone()),
        );
    }
    let jobs = coordinator.jobs.clone();
    Node {
        db,
        store,
        coordinator: Arc::new(coordinator),
        jobs,
        now,
        enrollment,
        version: plan.version.to_owned(),
        _dir: dir,
    }
}

impl Node {
    /// One tick at the second `at`: the jobs it ran.
    async fn tick(&self, at: u64) -> Vec<String> {
        self.now.store(at, Ordering::SeqCst);
        let jobs = self.jobs.as_ref().expect("a claiming node");
        run_claimed(&self.coordinator, jobs).await
    }

    /// The halts of this test noted in the alert rows: service.
    fn alert_rows(&self) -> Vec<Vec<Option<String>>> {
        self.db.rows(&format!(
            "SELECT kind, service FROM alerts WHERE version = '{}' ORDER BY service",
            self.version
        ))
    }

    /// How many lines the process logged for this test's halt alert.
    fn alert_lines(&self) -> usize {
        let log = String::from_utf8_lossy(&LOG.lock().unwrap_or_else(PoisonError::into_inner))
            .into_owned();
        let marker = format!("version={} ", self.version);
        log.lines()
            .filter(|line| line.contains("alert=\"halt\"") && line.contains(&marker))
            .count()
    }

    /// A session row that every rule of the jobs ends (created and last used at 0).
    fn old_session(&self, id: &str) {
        self.db
            .run(&format!(
                "INSERT INTO sessions (id_sha256, username, csrf, must_change, created_unix, \
                 last_used_unix) VALUES ('\\x{id}', 'u{id}', 'c', false, 0, 0)"
            ))
            .expect("a session");
    }

    fn sessions(&self) -> usize {
        self.db.rows("SELECT id_sha256 FROM sessions").len()
    }

    fn stop(self) {
        self.store.stop();
    }
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| (*name).to_owned()).collect()
}

/// Proves: a tick runs exactly the jobs it claimed: the first runs all three (the halt is noted in
/// the alert rows and logged once, the expired session deleted), a tick inside the periods runs
/// none (a session expired since stays, no alert is logged again), one after the short periods
/// runs the two short jobs and not `prune`, and one after the long period runs `prune` too.
///
/// Catches: a tick that runs jobs it did not claim, one that runs nothing it claimed, and a job
/// run again inside its period.
#[tokio::test]
async fn a_tick_runs_exactly_the_jobs_claimed_and_a_second_inside_the_periods_runs_none() {
    let node = node("jobs-tick", Plan::claiming("9.5001.1")).await;
    node.old_session("01");
    let t = 5_000_000;
    assert_eq!(
        node.tick(t).await,
        names(&["alerts", "prune", "request-expiry"])
    );
    assert_eq!(
        node.alert_rows(),
        vec![vec![Some("halt".to_owned()), Some("web".to_owned())]]
    );
    assert_eq!(node.alert_lines(), 1);
    assert_eq!(node.sessions(), 0, "prune deleted the expired session");

    node.old_session("02");
    assert_eq!(node.tick(t + 1).await, Vec::<String>::new());
    assert_eq!(node.sessions(), 1, "prune did not run again");
    assert_eq!(node.tick(t + 14).await, Vec::<String>::new());

    assert_eq!(
        node.tick(t + 15).await,
        names(&["alerts", "request-expiry"])
    );
    assert_eq!(node.sessions(), 1, "prune is not due before its hour");
    assert_eq!(
        node.alert_lines(),
        1,
        "the alert is logged once, not at each observation"
    );

    assert_eq!(
        node.tick(t + 3600).await,
        names(&["alerts", "prune", "request-expiry"])
    );
    assert_eq!(node.sessions(), 0);
    assert_eq!(node.alert_lines(), 1);
    node.stop();
}

/// Proves: the job `request-expiry` expires over the database the request that waited longer than
/// the request TTL and leaves the one posted since, and a tick that did not claim it expires
/// nothing.
///
/// Catches: a job that never reaches the enrollment store, one that expires a request still
/// inside its TTL, and one that runs before it is claimed.
#[tokio::test]
async fn request_expiry_expires_the_stale_request_when_it_is_claimed() {
    let node = node(
        "jobs-expiry",
        Plan {
            enrolled: true,
            ..Plan::claiming("9.5002.1")
        },
    )
    .await;
    let enrollment = Arc::clone(node.enrollment.as_ref().expect("an enrollment"));
    let post = |machine: &str, at: u64| {
        let key = HostKey::new();
        enrollment
            .store()
            .post(
                NewRequest {
                    csr_der: key.csr(),
                    claims: Claims {
                        machine_id: machine.to_owned(),
                        ..Claims::default()
                    },
                    image: "hostkeeper-image-1".to_owned(),
                    renewal_of: None,
                    mac_nonce_presented: false,
                },
                at,
            )
            .expect("posted")
            .request_id
    };
    let stale = post("m-stale", NOW - enrollment.request_ttl_secs() - 1);
    let fresh = post("m-fresh", NOW);
    let expired = || -> Vec<_> {
        enrollment
            .store()
            .expired()
            .expect("read")
            .into_iter()
            .map(|view| view.request_id)
            .collect()
    };
    assert_eq!(expired(), Vec::<_>::new());
    assert_eq!(
        node.tick(5_000_000).await,
        names(&["alerts", "prune", "request-expiry"])
    );
    assert_eq!(expired(), vec![stale]);
    assert_ne!(expired(), vec![fresh]);
    node.stop();
}

/// Proves: a coordinator with no enrollment store claims and runs `request-expiry` as a job that
/// does nothing, and the other jobs of the tick run.
///
/// Catches: a job that fails (or panics) for want of an enrollment store, and one that stops the
/// jobs after it.
#[tokio::test]
async fn request_expiry_without_an_enrollment_runs_as_nothing() {
    let node = node("jobs-no-enrollment", Plan::claiming("9.5003.1")).await;
    assert_eq!(
        node.tick(5_000_000).await,
        names(&["alerts", "prune", "request-expiry"])
    );
    assert_eq!(node.alert_rows().len(), 1, "the alerts job ran");
    assert!(!logged("no job is named request-expiry"));
    node.stop();
}

/// Proves: a claimed job whose name no job has is reported failed (an error line names it) and
/// does not stop the jobs claimed with it: the alerts job, which sorts after it, still runs.
///
/// Catches: a periods list that panics or stops the tick at a name it cannot run, and a failure
/// that is not logged.
#[tokio::test]
async fn an_unknown_job_name_is_reported_failed_and_does_not_stop_the_others() {
    let node = node(
        "jobs-unknown",
        Plan {
            periods: Some(vec![
                ("alerts".to_owned(), 15),
                ("a-no-such-job-m8".to_owned(), 15),
            ]),
            ..Plan::claiming("9.5004.1")
        },
    )
    .await;
    assert_eq!(
        node.tick(5_000_000).await,
        names(&["a-no-such-job-m8", "alerts"])
    );
    assert!(logged("a periodic job failed"));
    assert!(logged("no job is named a-no-such-job-m8"));
    assert_eq!(node.alert_rows().len(), 1, "the alerts job ran after it");
    node.stop();
}

/// Proves: a tick whose claim fails (the store is lost) runs nothing, whatever is due: the halt is
/// noted in no row, and the failure is logged as a claim that could not be made.
///
/// Catches: a tick that runs the jobs it could not claim, so that every coordinator without a
/// database runs them, and a failed claim that is not logged.
#[tokio::test]
async fn a_claim_that_fails_runs_nothing() {
    let node = node("jobs-claim-fails", Plan::claiming("9.5005.1")).await;
    node.old_session("01");
    node.store.stop();
    assert_eq!(node.tick(5_000_000).await, Vec::<String>::new());
    assert_eq!(node.alert_rows(), Vec::<Vec<Option<String>>>::new());
    assert_eq!(node.sessions(), 1, "prune did not run");
    assert!(logged("the periodic jobs could not be claimed"));
}

/// Proves: only a coordinator given jobs claims them.
///
/// Catches: a coordinator over files that waits for claims no one makes, so its requests never
/// expire.
#[tokio::test]
async fn a_coordinator_claims_jobs_only_when_given_them() {
    let claiming = node("jobs-claims", Plan::claiming("9.5006.1")).await;
    assert!(claiming.coordinator.claims_jobs());
    claiming.stop();
    let alone = node(
        "jobs-claims-not",
        Plan {
            periods: None,
            ..Plan::claiming("9.5007.1")
        },
    )
    .await;
    assert!(!alone.coordinator.claims_jobs());
    alone.stop();
}

/// Proves: the periodic work of a coordinator without jobs (over files) is the alerts observed
/// every interval: its halt is logged by the loop, once the interval has passed.
///
/// Catches: a loop that starts no observation when it holds no jobs, so that nothing logs an
/// alert on a coordinator over files.
#[tokio::test(start_paused = true)]
async fn periodic_without_jobs_observes_the_alerts_every_interval() {
    let node = node(
        "jobs-files-arm",
        Plan {
            periods: None,
            ..Plan::claiming("9.5008.1")
        },
    )
    .await;
    assert_eq!(node.alert_lines(), 0);
    let running = tokio::spawn(periodic(Arc::clone(&node.coordinator)));
    tokio::task::yield_now().await;
    tokio::time::advance(crate::alerts::OBSERVE_EVERY).await;
    for _ in 0..1000 {
        if node.alert_lines() > 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    running.abort();
    assert_eq!(node.alert_lines(), 1);
    node.stop();
}

/// Proves: the periodic work of a coordinator with jobs claims at its first tick: the job rows
/// say this instance claimed them, and the alert is in the rows.
///
/// Catches: a loop that waits a whole tick before it first claims, and one that claims and runs
/// nothing.
#[tokio::test(start_paused = true)]
async fn periodic_with_jobs_claims_at_its_first_tick() {
    let node = node("jobs-claim-arm", Plan::claiming("9.5009.1")).await;
    let id = node.store.instance().id.clone();
    let running = tokio::spawn(periodic(Arc::clone(&node.coordinator)));
    // The progress event is the row itself: the loop ends when the first tick has claimed, with no
    // bound of time or count (a loop that never claims never ends the test).
    let rows = loop {
        let rows = node.db.rows(
            "SELECT last_instance FROM periodic_jobs WHERE name = 'alerts' AND last_instance IS NOT NULL",
        );
        if !rows.is_empty() {
            break rows;
        }
        tokio::task::yield_now().await;
    };
    running.abort();
    assert_eq!(rows, vec![vec![Some(id)]]);
    node.stop();
}
