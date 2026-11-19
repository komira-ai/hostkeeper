//! The periodic jobs claimed per job over a server (`periodic`; `docs/design/coordinator-multi-instance.md`,
//! section 9): of several coordinators that find a job due, one gets it for the period.
//!
//! The clock is a number each test passes in (`PgJobs::with_clock`); coordinators are stores of
//! one database whose claims start together at a barrier. Nothing sleeps.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};

use super::{TestDatabase, off_runtime};
use crate::audit_file::UnixClock;
use crate::session::{LimiterSettings, SessionSettings};
use crate::store::postgres::Postgres;
use crate::store::postgres::login_rows::PgLoginRows;
use crate::store::postgres::periodic::{PgJobs, Pruned, seed};

/// The message of a write refused at once because the store is lost.
const LOST: &str = "the writing session stopped";

/// The periods of the three jobs, as the flags' defaults give them.
pub(super) fn periods() -> Vec<(String, i64)> {
    vec![
        ("alerts".to_owned(), 15),
        ("request-expiry".to_owned(), 15),
        ("prune".to_owned(), 3600),
    ]
}

/// A clock a test moves.
pub(super) fn clock(at: u64) -> (Arc<AtomicU64>, UnixClock) {
    let now = Arc::new(AtomicU64::new(at));
    let read = Arc::clone(&now);
    (now, Arc::new(move || read.load(Ordering::SeqCst)))
}

/// The login rows' rules of these tests: a session idles out after 100 seconds and ends at 1000,
/// a name locks at 3 failures for 60 seconds.
pub(super) fn login_rows_of(store: &Postgres) -> PgLoginRows {
    PgLoginRows::new(
        store.clone(),
        SessionSettings {
            idle_secs: 100,
            max_secs: 1000,
            max_count: 10,
        },
        LimiterSettings {
            max_failures: 3,
            lockout_secs: 60,
            max_tracked: 10,
        },
        30,
    )
}

/// The jobs of `periods` on `store`, judged by `clock`. A start seeds by the database's own
/// clock, so the rows it made are dropped and made again at the test's second.
pub(super) fn jobs_of(
    db: &TestDatabase,
    store: &Postgres,
    periods: Vec<(String, i64)>,
    clock: &UnixClock,
) -> PgJobs {
    let jobs =
        PgJobs::start(store.clone(), periods, &login_rows_of(store)).expect("the jobs start");
    db.run("DELETE FROM periodic_jobs").expect("rows dropped");
    let jobs = jobs.with_clock(Arc::clone(clock));
    jobs.seed().expect("seeded at the test's second");
    jobs
}

/// `n` stores of one database (each a coordinator of its own), and the database.
fn stores(n: usize) -> (TestDatabase, Vec<Postgres>) {
    let db = TestDatabase::new();
    let stores = (0..n)
        .map(|i| db.open(&format!("claimer-{i}")).expect("the store opens").0)
        .collect();
    (db, stores)
}

fn stop(stores: &[Postgres]) {
    for store in stores {
        store.stop();
    }
}

/// What each of `jobs` claimed when they all claimed at once.
fn claim_together(jobs: &[PgJobs]) -> Vec<Vec<String>> {
    let start = Barrier::new(jobs.len());
    std::thread::scope(|scope| {
        let handles: Vec<_> = jobs
            .iter()
            .map(|job| {
                let start = &start;
                scope.spawn(move || {
                    start.wait();
                    job.claim().expect("claims")
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("joins"))
            .collect()
    })
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| (*name).to_owned()).collect()
}

/// A row of `periodic_jobs`: next run, last run, last instance.
fn row(db: &TestDatabase, name: &str) -> Vec<Option<String>> {
    db.rows(&format!(
        "SELECT next_run_unix, last_run_unix, last_instance FROM periodic_jobs WHERE name = '{name}'"
    ))
    .into_iter()
    .next()
    .expect("the job has a row")
}

/// Proves: of three coordinators claiming at once, in every one of many periods, each due job
/// goes to exactly one of them (the three jobs never to the same claimer twice or to none), and
/// `last_instance` names the coordinator that got it.
///
/// Catches: two coordinators that both run a job in a period, a job no one gets, and a claim that
/// does not say who holds it.
#[test]
fn two_sessions_claim_the_same_due_job_and_one_runs() {
    let (db, stores) = stores(3);
    let (now, at) = clock(1000);
    let jobs: Vec<PgJobs> = stores
        .iter()
        .map(|store| jobs_of(&db, store, periods(), &at))
        .collect();
    let ids: Vec<String> = stores.iter().map(|s| s.instance().id.clone()).collect();
    for round in 0..20_u64 {
        now.store(1000 + round * 15, Ordering::SeqCst);
        let claimed = claim_together(&jobs);
        for job in ["alerts", "request-expiry"] {
            let winners: Vec<usize> = (0..claimed.len())
                .filter(|i| claimed[*i].iter().any(|name| name == job))
                .collect();
            assert_eq!(winners.len(), 1, "round {round}, {job}: {claimed:?}");
            assert_eq!(
                row(&db, job)[2].as_deref(),
                Some(ids[winners[0]].as_str()),
                "round {round}, {job}"
            );
        }
        let prune: usize = claimed
            .iter()
            .filter(|c| c.iter().any(|n| n == "prune"))
            .count();
        assert_eq!(prune, usize::from(round == 0), "round {round}: {claimed:?}");
    }
    stop(&stores);
}

/// Proves: with 1, 2 or 3 coordinators claiming, a job is claimed once at its due time, not again
/// for the rest of its period (to the last second of it), and once again when the period has
/// passed, the due time moving by the period from the claim.
///
/// Catches: a claim that does not move the due time on, one that moves it by the wrong period,
/// and a claim count that grows with the coordinators.
#[test]
fn periodic_job_runs_once_per_period_with_n_claimers() {
    for n in 1..=3 {
        let (db, stores) = stores(n);
        let (now, at) = clock(5000);
        let jobs: Vec<PgJobs> = stores
            .iter()
            .map(|store| jobs_of(&db, store, vec![("alerts".to_owned(), 15)], &at))
            .collect();
        let count = |claimed: Vec<Vec<String>>| claimed.into_iter().flatten().count();
        assert_eq!(count(claim_together(&jobs)), 1, "{n} claimers at t");
        assert_eq!(row(&db, "alerts")[0].as_deref(), Some("5015"));
        for inside in [5001, 5014] {
            now.store(inside, Ordering::SeqCst);
            assert_eq!(count(claim_together(&jobs)), 0, "{n} claimers at {inside}");
        }
        now.store(5015, Ordering::SeqCst);
        assert_eq!(count(claim_together(&jobs)), 1, "{n} claimers at t+period");
        let last = row(&db, "alerts");
        assert_eq!(last[0].as_deref(), Some("5030"));
        assert_eq!(last[1].as_deref(), Some("5015"));
        stop(&stores);
    }
}

/// Proves: a claim whose job is never run (the claimer died) costs one run: the job is not due
/// again for any claimer until one period later, and then any claimer gets it.
///
/// Catches: a claim held for good by a dead coordinator, and a job that is due again at once so
/// that a lost claim is run twice.
#[test]
fn claimer_death_loses_one_run_only() {
    let (db, stores) = stores(2);
    let (now, at) = clock(100);
    let dying = jobs_of(&db, &stores[0], periods(), &at);
    let other = jobs_of(&db, &stores[1], periods(), &at);
    assert_eq!(
        dying.claim().expect("claims"),
        names(&["alerts", "prune", "request-expiry"])
    );
    stores[0].stop();
    now.store(114, Ordering::SeqCst);
    assert_eq!(other.claim().expect("claims"), Vec::<String>::new());
    now.store(115, Ordering::SeqCst);
    assert_eq!(
        other.claim().expect("claims"),
        names(&["alerts", "request-expiry"])
    );
    assert_eq!(
        row(&db, "alerts")[2].as_deref(),
        Some(stores[1].instance().id.as_str())
    );
    stores[1].stop();
}

/// Proves: seeding again keeps the due time of a job that has a row (a job already claimed and not
/// due is not made due by a coordinator that starts later), and makes a row, due at once, for a job
/// that has none; the number of rows made is returned.
///
/// Catches: a start that makes every job due again and so runs it in every restart, and a new job
/// that is never run.
#[test]
fn a_seed_keeps_the_due_time_of_a_job_that_has_a_row() {
    let (db, stores) = stores(1);
    let (now, at) = clock(1000);
    let jobs = jobs_of(&db, &stores[0], vec![("alerts".to_owned(), 15)], &at);
    assert_eq!(jobs.claim().expect("claims"), names(&["alerts"]));
    now.store(1001, Ordering::SeqCst);
    jobs.seed().expect("seeded again");
    assert_eq!(row(&db, "alerts")[0].as_deref(), Some("1015"));
    assert_eq!(jobs.claim().expect("claims"), Vec::<String>::new());
    let made = stores[0]
        .write(|tx| seed(tx, &["alerts", "prune"], Some(2000)))
        .expect("seeds");
    assert_eq!(made, 1);
    assert_eq!(row(&db, "alerts")[0].as_deref(), Some("1015"));
    assert_eq!(
        row(&db, "prune"),
        vec![Some("2000".to_owned()), None, None],
        "made due at the second given, never run"
    );
    stop(&stores);
}

/// Proves: jobs that are due together are claimed together, in one statement, in name order, each
/// row naming the same claimer and second.
///
/// Catches: a claim that returns one job per tick, or an order that depends on the rows' storage.
#[test]
fn jobs_due_together_are_claimed_together() {
    let (db, stores) = stores(1);
    let (_now, at) = clock(7000);
    let jobs = jobs_of(&db, &stores[0], periods(), &at);
    assert_eq!(
        jobs.claim().expect("claims"),
        names(&["alerts", "prune", "request-expiry"])
    );
    let id = stores[0].instance().id.clone();
    for (job, next) in [
        ("alerts", "7015"),
        ("request-expiry", "7015"),
        ("prune", "10600"),
    ] {
        assert_eq!(
            row(&db, job),
            vec![
                Some(next.to_owned()),
                Some("7000".to_owned()),
                Some(id.clone())
            ],
            "{job}"
        );
    }
    stop(&stores);
}

/// Proves: a claimer claims only the jobs of its own list: a due job it does not name keeps its due
/// time and has no claimer, for a coordinator that does name it.
///
/// Catches: a claim that takes every due row and so loses the jobs a coordinator does not run.
#[test]
fn a_job_not_in_the_claimers_list_is_not_claimed() {
    let (db, stores) = stores(2);
    let (_now, at) = clock(300);
    let some = jobs_of(&db, &stores[1], vec![("alerts".to_owned(), 15)], &at);
    let _all = jobs_of(&db, &stores[0], periods(), &at);
    assert_eq!(some.claim().expect("claims"), names(&["alerts"]));
    for job in ["prune", "request-expiry"] {
        assert_eq!(
            row(&db, job),
            vec![Some("300".to_owned()), None, None],
            "{job} is untouched"
        );
    }
    stop(&stores);
}

/// Proves: the job `prune` deletes the sessions past their idle (at the second) or absolute
/// deadline, the failure counts the limiter forgot (not locked, last failure a lockout ago) and
/// the instances unseen for a day (not the one seen exactly a day ago) and the orders aged out
/// (`ORDER_AGE` or more since given, not one a second younger), counts each, and keeps live
/// sessions, locked and recent failures, instances seen inside the day and live orders.
///
/// Catches: a prune that deletes a live session, a lock, a running instance or an order that still
/// holds its slot, one that keeps what is past its deadline, and a boundary second judged the
/// wrong way.
#[test]
fn prune_deletes_expired_sessions_forgotten_failures_unseen_instances_and_aged_orders() {
    let (db, stores) = stores(1);
    let now = 10_000_000_u64;
    let (_now, at) = clock(now);
    let jobs = jobs_of(&db, &stores[0], periods(), &at);
    let n = now as i64;
    for (id, created, used) in [
        ("01", n - 10, n - 10),
        ("02", n - 500, n - 99),
        ("03", n - 500, n - 100),
        ("04", n - 1000, n - 1),
        ("05", n - 999, n - 1),
    ] {
        db.run(&format!(
            "INSERT INTO sessions (id_sha256, username, csrf, must_change, created_unix, \
             last_used_unix) VALUES ('\\x{id}', 'u{id}', 'c', false, {created}, {used})"
        ))
        .expect("a session");
    }
    for (name, failures, last, locked) in [
        ("recent", 1, n - 59, "NULL".to_owned()),
        ("forgotten", 1, n - 60, "NULL".to_owned()),
        ("locked-old", 3, n - 5000, (n + 1).to_string()),
        ("lock-ended", 3, n - 5000, n.to_string()),
    ] {
        db.run(&format!(
            "INSERT INTO login_failures (name, failures, last_failure_unix, locked_until_unix) \
             VALUES ('{name}', {failures}, {last}, {locked})"
        ))
        .expect("a failure");
    }
    for (id, seen) in [
        ("00000000-0000-0000-0000-000000000001", n - 86_400),
        ("00000000-0000-0000-0000-000000000002", n - 86_401),
        ("00000000-0000-0000-0000-000000000003", n),
    ] {
        db.run(&format!(
            "INSERT INTO instances (id, started_unix, seen_unix, release, listen) \
             VALUES ('{id}', 0, {seen}, 'r', 'l')"
        ))
        .expect("an instance");
    }
    for (host, at) in [("young", n - 119), ("aged", n - 120), ("old", n - 5000)] {
        db.run(&format!(
            "INSERT INTO fleet_orders (host_id, service, target, at_unix, try) \
             VALUES ('{host}', 'web', '1.0', {at}, 1)"
        ))
        .expect("an order");
    }
    let pruned = jobs.prune().expect("prunes");
    assert_eq!(
        pruned,
        Pruned {
            sessions: 2,
            login_failures: 2,
            instances: 1,
            fleet_orders: 2
        }
    );
    let column = |sql: &str| -> Vec<String> {
        db.rows(sql)
            .into_iter()
            .map(|row| row[0].clone().expect("a value"))
            .collect()
    };
    assert_eq!(
        column("SELECT username FROM sessions ORDER BY username"),
        ["u01", "u02", "u05"]
    );
    assert_eq!(
        column("SELECT name FROM login_failures ORDER BY name"),
        ["locked-old", "recent"]
    );
    assert_eq!(
        column("SELECT host_id FROM fleet_orders ORDER BY host_id"),
        ["young"],
        "an order is freed once ORDER_AGE has passed since it was given"
    );
    assert_eq!(
        column("SELECT id::text FROM instances WHERE id::text LIKE '00000000%' ORDER BY id"),
        [
            "00000000-0000-0000-0000-000000000001",
            "00000000-0000-0000-0000-000000000003"
        ]
    );
    assert_eq!(
        db.rows(&format!(
            "SELECT count(*) FROM instances WHERE id = '{}'",
            stores[0].instance().id
        )),
        vec![vec![Some("1".to_owned())]],
        "the running instance's own row stays"
    );
    assert_eq!(jobs.prune().expect("prunes"), Pruned::default());
    stop(&stores);
}

/// Proves: over a store whose session is lost, seeding, claiming and pruning each return the
/// store's error and claim nothing.
///
/// Catches: a claim that reports jobs it could not write, so that a lost coordinator runs jobs
/// another one also claims.
#[test]
fn a_lost_store_claims_nothing() {
    let (db, stores) = stores(1);
    let (_now, at) = clock(1);
    let jobs = jobs_of(&db, &stores[0], periods(), &at);
    stores[0].stop();
    off_runtime(move || {
        assert_eq!(jobs.claim(), Err(LOST.to_owned()));
        assert_eq!(jobs.seed(), Err(LOST.to_owned()));
        assert_eq!(jobs.prune(), Err(LOST.to_owned()));
    });
}
