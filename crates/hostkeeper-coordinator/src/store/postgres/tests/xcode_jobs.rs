//! The job `xcode-feed` over a database: of two coordinators that find it due one reads the list
//! and the other does not, the list is in the table `xcode_releases`, a read that fails is due
//! again a day later, and a coordinator with no feed address is never given the job.
//!
//! The jobs' clock is a number the test moves (`PgJobs::with_clock`), and the origin is served in
//! process: nothing sleeps and no socket is opened.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use clap::Parser;
use hostkeeper_oidc::Dial;

use super::TestDatabase;
use super::periodic::{clock, jobs_of};
use crate::config::DatabaseFlags;
use crate::jobs::{Jobs, run_claimed};
use crate::repository_origin::RepositoryOrigin;
use crate::repository_origin_support::{FakeOrigin, Then, response};
use crate::service::Coordinator;
use crate::store::Postgres;
use crate::store::postgres::alert_rows::PgAlerts;
use crate::store::postgres::xcode_rows::PgReleases;
use crate::testutil::Rig;
use crate::xcode_feed::FeedUrl;

/// A copy of the 12 newest entries of the Xcode Releases site's `data.json`.
const FIXTURE: &[u8] = include_bytes!("../../../../tests/fixtures/xcode/data.json");

/// The address of the list at the fake origin.
const FEED: &str = "https://origin.test/data.json";

/// The time the jobs are first due, unix seconds.
const START: u64 = 5_000_000;

/// A day, the period of the job `xcode-feed`.
const DAY: u64 = 86_400;

/// The periods of the jobs a coordinator with (`feed`) or without a feed address claims, as the
/// flags' defaults give them.
fn periods_of(feed: bool) -> Vec<(String, i64)> {
    #[derive(Debug, Parser)]
    struct Flags {
        #[command(flatten)]
        database: DatabaseFlags,
    }
    Flags::try_parse_from(["serve"])
        .expect("no flag is needed")
        .database
        .job_periods(feed)
}

/// A coordinator over the database, claiming jobs.
struct Node {
    store: Postgres,
    coordinator: Arc<Coordinator>,
    jobs: Arc<Jobs>,
    /// Keeps the files of the node's coordinator.
    _rig: Rig,
}

/// A coordinator named `test` on `db`, reading the list at `FEED` through `dial` when `feed`.
fn node(
    test: &str,
    db: &TestDatabase,
    fake: &FakeOrigin,
    dial: Dial,
    feed: bool,
    at: &crate::audit_file::UnixClock,
) -> Node {
    let (store, loaded) = db.open(test).expect("the store opens");
    let rig = Rig::new(test);
    let marker = rig.dir.path().join("halt.marker");
    let origin = RepositoryOrigin::new(fake.roots())
        .expect("the origin client")
        .with_dial(dial);
    let coordinator = super::coordinator::start_on_postgres(&rig, &store, loaded, &marker)
        .with_repository_origin(origin)
        .with_xcode_feed(feed.then(|| FeedUrl::parse(FEED).expect("a feed address")))
        .with_known_xcode_releases(Arc::new(PgReleases::new(store.clone())));
    let pg = jobs_of(db, &store, periods_of(feed), at);
    let coordinator = coordinator.with_jobs(
        Jobs::new(pg, Duration::from_secs(3600)),
        PgAlerts::new(store.clone()),
    );
    let jobs = coordinator.jobs.clone().expect("a claiming node");
    Node {
        store,
        coordinator: Arc::new(coordinator),
        jobs,
        _rig: rig,
    }
}

impl Node {
    /// One tick at the second `at` of the clock `now`: the jobs it ran.
    async fn tick(&self, now: &AtomicU64, at: u64) -> Vec<String> {
        now.store(at, Ordering::SeqCst);
        run_claimed(&self.coordinator, &self.jobs).await
    }
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| (*name).to_owned()).collect()
}

/// The releases of `db`'s table: version, build, channel, macOS, of the channel `release`, newest
/// version first.
fn kept_releases(db: &TestDatabase) -> Vec<Vec<Option<String>>> {
    db.rows(
        "SELECT version, build, channel, requires_macos FROM xcode_releases \
         WHERE channel = 'release' ORDER BY string_to_array(version, '.')::int[] DESC",
    )
}

fn count(db: &TestDatabase) -> Vec<Vec<Option<String>>> {
    db.rows("SELECT count(*) FROM xcode_releases")
}

fn row(cells: [&str; 4]) -> Vec<Option<String>> {
    cells.iter().map(|cell| Some((*cell).to_owned())).collect()
}

/// Proves: two coordinators on one database with the feed configured, each ticking once at the
/// same second, read the list once between them (the one that claimed the job; the other runs
/// none), and the table holds the 12 releases of the list, three of them of the channel
/// `release`, each with its build and the macOS it requires.
///
/// Catches: a job run by every coordinator, a job that is claimed and not run, and a list that is
/// read and not kept in the table.
#[tokio::test]
async fn of_two_coordinators_one_reads_the_list_and_the_table_holds_it() {
    let db = TestDatabase::new();
    let fake = FakeOrigin::new();
    let dial = fake.dial_all(Then::Write(response("200 OK", FIXTURE)));
    let (now, at) = clock(START);
    let first = node("xcode-two-a", &db, &fake, dial.clone(), true, &at);
    let second = node("xcode-two-b", &db, &fake, dial, true, &at);
    assert_eq!(
        first.tick(&now, START).await,
        names(&["alerts", "prune", "request-expiry", "xcode-feed"])
    );
    assert_eq!(second.tick(&now, START).await, Vec::<String>::new());
    assert_eq!(fake.dials(), 1);
    assert_eq!(count(&db), vec![vec![Some("12".to_owned())]]);
    assert_eq!(
        kept_releases(&db),
        vec![
            row(["27.2", "27B52", "release", "26.6"]),
            row(["27.1", "27A9275", "release", "26.6"]),
            row(["27.0", "27A266a", "release", "26.6"]),
        ]
    );
    first.store.stop();
    second.store.stop();
}

/// Proves: a read that fails (the origin answers 500) is not claimed again before a day has
/// passed: a tick a second short of the day runs no `xcode-feed` and dials nothing, the tick at
/// the day runs it, and when the origin then answers the list the table holds it.
///
/// Catches: a failed read that is retried at every tick, one that is never retried, and a table
/// written by a read that failed.
#[tokio::test]
async fn a_read_that_fails_is_due_again_a_day_later() {
    let db = TestDatabase::new();
    let fake = FakeOrigin::new();
    let healthy = Arc::new(AtomicBool::new(false));
    let answering = Arc::clone(&healthy);
    let dial = fake.dial(Arc::new(move |_: &str| {
        if answering.load(Ordering::SeqCst) {
            Then::Write(response("200 OK", FIXTURE))
        } else {
            Then::Write(response("500 Internal Server Error", b""))
        }
    }));
    let (now, at) = clock(START);
    let node = node("xcode-retry", &db, &fake, dial, true, &at);

    assert!(
        node.tick(&now, START)
            .await
            .contains(&"xcode-feed".to_owned())
    );
    assert_eq!(fake.dials(), 1);
    assert_eq!(count(&db), vec![vec![Some("0".to_owned())]]);

    assert!(
        !node
            .tick(&now, START + DAY - 1)
            .await
            .contains(&"xcode-feed".to_owned())
    );
    assert_eq!(fake.dials(), 1);

    healthy.store(true, Ordering::SeqCst);
    assert!(
        node.tick(&now, START + DAY)
            .await
            .contains(&"xcode-feed".to_owned())
    );
    assert_eq!(fake.dials(), 2);
    assert_eq!(count(&db), vec![vec![Some("12".to_owned())]]);
    node.store.stop();
}

/// Proves: a coordinator with no feed address is never given the job `xcode-feed`: neither its
/// first tick nor the tick a day later runs it, and nothing is dialled.
///
/// Catches: a job that is seeded and claimed with no address, so that it reads nothing and logs
/// a failure each day.
#[tokio::test]
async fn without_a_feed_address_the_job_is_never_claimed() {
    let db = TestDatabase::new();
    let fake = FakeOrigin::new();
    let dial = fake.dial_all(Then::Write(response("200 OK", FIXTURE)));
    let (now, at) = clock(START);
    let node = node("xcode-off", &db, &fake, dial, false, &at);
    assert_eq!(
        node.tick(&now, START).await,
        names(&["alerts", "prune", "request-expiry"])
    );
    assert_eq!(
        node.tick(&now, START + DAY).await,
        names(&["alerts", "prune", "request-expiry"])
    );
    assert_eq!(fake.dials(), 0);
    assert_eq!(count(&db), vec![vec![Some("0".to_owned())]]);
    node.store.stop();
}
