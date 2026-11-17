//! Periodic jobs claimed per job over a server (`periodic`): of several coordinators that find a
//! job due, one gets it for the period.
//!
//! The clock is a number each test passes in; coordinators are sessions started together by a
//! barrier. Nothing sleeps.

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Barrier};

use super::TestDatabase;
use crate::store::postgres::periodic::{claim, seed};
use crate::store::postgres::session::Session;
use crate::store::postgres::{Postgres, TIMEOUTS};

fn rig(n: usize) -> (TestDatabase, Postgres, Vec<Session>) {
    let db = TestDatabase::new();
    let (store, _) = db.open("periodic").expect("the store opens");
    let sessions = (0..n)
        .map(|_| {
            Session::connect(
                &db.dsn().config(TIMEOUTS),
                Arc::new(AtomicU64::new(0)),
                false,
            )
            .expect("a session")
        })
        .collect();
    (db, store, sessions)
}

/// Proves: of four coordinators claiming at once a job that is due, exactly one gets it, in every
/// one of many rounds; the job is not due again until its period has passed, and is then claimed
/// by exactly one again; a job that is not due is never claimed; `last_instance` names the one that
/// claimed it.
///
/// Catches: two coordinators that both run a job per period, and a job whose time did not move.
#[test]
fn of_several_coordinators_one_claims_a_due_job_for_the_period() {
    let (db, _store, mut sessions) = rig(5);
    let mut setup = sessions.pop().expect("a session");
    seed(&mut setup, "alerts", 15, 1000).expect("seeded");
    seed(&mut setup, "prune", 600, 5000).expect("seeded");
    for round in 0..10_i64 {
        let now = 1000 + round * 15;
        let start = Barrier::new(sessions.len());
        let claimed: Vec<Vec<String>> = std::thread::scope(|scope| {
            let handles: Vec<_> = sessions
                .iter_mut()
                .enumerate()
                .map(|(who, session)| {
                    let start = &start;
                    scope.spawn(move || {
                        start.wait();
                        claim(session, now, &format!("i{who}"), &["alerts", "prune"])
                            .expect("claims")
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("joins"))
                .collect()
        });
        let winners: Vec<&Vec<String>> = claimed.iter().filter(|c| !c.is_empty()).collect();
        assert_eq!(winners.len(), 1, "round {round}: {claimed:?}");
        assert_eq!(winners[0], &vec!["alerts".to_owned()]);
        assert_eq!(
            claim(&mut setup, now + 14, "late", &["alerts", "prune"]).expect("claims"),
            Vec::<String>::new(),
            "not due again inside the period"
        );
    }
    let last = db.rows("SELECT last_run_unix, next_run_unix, last_instance FROM periodic_jobs WHERE name = 'alerts'");
    assert_eq!(last[0][0], Some("1135".to_owned()));
    assert_eq!(last[0][1], Some("1150".to_owned()));
    assert!(last[0][2].as_deref().is_some_and(|i| i.starts_with('i')));
    assert_eq!(
        db.rows("SELECT last_run_unix FROM periodic_jobs WHERE name = 'prune'"),
        vec![vec![None]],
        "a job that was never due was never claimed"
    );
}

/// Proves: jobs due together are all claimed by one claim; a job seeded again with another period
/// keeps its due time and runs at the new period from its next claim, and a seed of a job that
/// exists does not make it due again.
///
/// Catches: a restart that seeds every job as due at once, and a period change that is ignored.
#[test]
fn jobs_due_together_are_claimed_together_and_a_seed_keeps_the_due_time() {
    let (db, _store, mut sessions) = rig(1);
    let s = &mut sessions[0];
    seed(s, "a", 10, 100).expect("seeded");
    seed(s, "b", 20, 100).expect("seeded");
    let mut got = claim(s, 100, "i0", &["a", "b"]).expect("claims");
    got.sort();
    assert_eq!(got, vec!["a".to_owned(), "b".to_owned()]);
    seed(s, "a", 30, 0).expect("seeded again");
    assert_eq!(
        claim(s, 105, "i0", &["a", "b"]).expect("claims"),
        Vec::<String>::new()
    );
    assert_eq!(
        claim(s, 110, "i0", &["a", "b"]).expect("claims"),
        vec!["a".to_owned()]
    );
    assert_eq!(
        db.rows("SELECT name, next_run_unix FROM periodic_jobs ORDER BY name"),
        vec![
            vec![Some("a".into()), Some("140".into())],
            vec![Some("b".into()), Some("120".into())]
        ]
    );
}

/// Proves: the coordinators that find the periodic job `expire_requests` due through [`PgClaims`]
/// (the claims the coordinator's tick is given) are told yes once between them for the period,
/// whichever of them asks first, and not again in the period; a job that was never seeded is
/// nobody's; a coordinator whose rows cannot be reached is told no.
///
/// Catches: coordinators that each run the job every tick, and a claim that is lost when the
/// coordinator that took it does not ask again.
#[test]
fn coordinators_over_one_database_are_told_yes_once_a_period() {
    use crate::audit_file::wall_clock;
    use crate::store::Claims;
    use crate::store::postgres::periodic::PgClaims;

    let db = TestDatabase::new();
    let (first, second) = {
        let (a, b) = (db.dsn(), db.dsn());
        super::off_runtime(move || {
            (
                TestDatabase::open_dsn(&a, "first").expect("opens").0,
                TestDatabase::open_dsn(&b, "second").expect("opens").0,
            )
        })
    };
    let jobs = [("expire_requests", 3600)];
    let one = PgClaims::start(first.clone(), "one", &jobs, wall_clock()).expect("seeded");
    let two = PgClaims::start(second, "two", &jobs, wall_clock()).expect("seeded");
    let barrier = Barrier::new(2);
    let told: Vec<bool> = std::thread::scope(|scope| {
        let ask = |claims: Arc<PgClaims>| {
            let barrier = &barrier;
            scope.spawn(move || {
                barrier.wait();
                claims.claimed("expire_requests")
            })
        };
        let (a, b) = (ask(Arc::clone(&one)), ask(Arc::clone(&two)));
        vec![a.join().expect("asked"), b.join().expect("asked")]
    });
    assert_eq!(told.iter().filter(|yes| **yes).count(), 1, "{told:?}");
    assert!(!one.claimed("expire_requests") && !two.claimed("expire_requests"));
    assert!(!one.claimed("never_seeded"));
    first.stop();
    assert!(!one.claimed("expire_requests"), "a claim not made is no");
}
