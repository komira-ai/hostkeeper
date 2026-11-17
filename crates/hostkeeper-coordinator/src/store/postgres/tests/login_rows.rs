//! The sessions and the login limiter over a server (`login_rows`): the answers of the in-memory
//! table (`crate::session`) on the same script, a cookie made by one coordinator answered by
//! another, and counts that two coordinators make together.
//!
//! The clock is a number each test passes in; nothing sleeps.

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};

use hostkeeper_enrollment::validity::UnixClock;
use sha2::{Digest as _, Sha256};

use super::TestDatabase;
use crate::enrollment::Random;
use crate::password::HashSlots;
use crate::session::{
    Failure, LimiterSettings, LoginLimiter, SessionSettings, SessionView, Sessions,
};
use crate::store::postgres::login_rows::{
    LimiterRules, Live, NewSession, SessionRules, clear_must_change, create, end, end_user, failed,
    held, is_locked, lookup, succeeded, tracked,
};
use crate::store::postgres::session::Session;
use crate::store::postgres::{Postgres, TIMEOUTS};

const START: u64 = 1_800_000_000;

struct Clock(AtomicU64);

impl UnixClock for Clock {
    fn now_unix(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

fn counting() -> Random {
    let next = AtomicU64::new(1);
    Box::new(move |buffer| {
        let n = next.fetch_add(1, Ordering::SeqCst).to_le_bytes();
        for (i, byte) in buffer.iter_mut().enumerate() {
            *byte = n[i % 8].wrapping_add(u8::try_from(i).unwrap_or(0));
        }
        true
    })
}

fn rig(n: usize) -> (TestDatabase, Postgres, Vec<Session>) {
    let db = TestDatabase::new();
    let (store, _) = db.open("login rows").expect("the store opens");
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

fn hash(id: &str) -> Vec<u8> {
    Sha256::digest(id.as_bytes()).to_vec()
}

fn at(now: u64) -> i64 {
    i64::try_from(now).expect("small")
}

fn live_of(view: &SessionView) -> Live {
    Live {
        username: view.username.clone(),
        csrf: view.csrf().to_owned(),
        must_change: view.must_change,
        must_change_until: view.must_change_until.map(at),
    }
}

fn rules(idle: u64, max: u64, count: usize) -> (SessionSettings, SessionRules) {
    (
        SessionSettings {
            idle_secs: idle,
            max_secs: max,
            max_count: count,
        },
        SessionRules {
            idle_secs: at(idle),
            max_secs: at(max),
            max_count: i64::try_from(count).expect("small"),
        },
    )
}

/// Proves: on one script (makes, lookups inside and past the idle and the absolute deadline, a
/// clock that steps back, the cap with the least recently used dropped, a change of password, an
/// end, every session of a user ended but one) the rows answer what the in-memory table answers,
/// each step.
///
/// Catches: a deadline off by one second, an eviction of the wrong session, a lookup that does not
/// push the idle deadline out, and an expired session that is answered.
#[test]
fn the_rows_answer_as_the_in_memory_sessions_do() {
    let (_db, _store, mut sessions) = rig(1);
    let pg = &mut sessions[0];
    let clock = Arc::new(Clock(AtomicU64::new(START)));
    let (settings, rules) = rules(100, 1000, 3);
    let table = Sessions::new(settings, clock.clone(), counting());
    let set = |now: u64| clock.0.store(now, Ordering::SeqCst);
    let mut ids: Vec<String> = Vec::new();

    let make =
        |pg: &mut Session, ids: &mut Vec<String>, user: &str, must: Option<u64>, now: u64| {
            let (id, view) = table.create(user, must.is_some(), must).expect("made");
            create(
                pg,
                &rules,
                &NewSession {
                    id_sha256: &hash(id.as_str()),
                    username: user,
                    csrf: view.csrf(),
                    must_change: view.must_change,
                    must_change_until: view.must_change_until.map(at),
                },
                at(now),
            )
            .expect("made in rows");
            ids.push(id.as_str().to_owned());
        };
    let both = |pg: &mut Session, ids: &[String], n: usize, now: u64, what: &str| {
        let want = table.lookup(&ids[n]).map(|v| live_of(&v));
        let got = lookup(pg, &rules, &hash(&ids[n]), at(now)).expect("looked up");
        assert_eq!(got, want, "{what}");
    };

    make(pg, &mut ids, "ann", Some(START + 500), START);
    set(START + 50);
    make(pg, &mut ids, "bob", None, START + 50);
    set(START + 60);
    make(pg, &mut ids, "ann", None, START + 60);
    both(pg, &ids, 0, START + 60, "within idle");
    set(START + 70);
    // The cap is 3: a fourth drops the least recently used, which is `bob` (50) and not `ann` (60).
    make(pg, &mut ids, "cy", None, START + 70);
    assert_eq!(
        held(pg).expect("held"),
        i64::try_from(table.len()).expect("small")
    );
    for (n, name) in [(0, "ann 0"), (1, "bob evicted"), (2, "ann 2"), (3, "cy")] {
        both(pg, &ids, n, START + 70, name);
    }
    // The lookups pushed ann 0, ann 2 and cy to 70; idle is 100.
    set(START + 169);
    both(pg, &ids, 2, START + 169, "one second inside idle");
    set(START + 170);
    both(pg, &ids, 2, START + 170, "at the idle deadline");
    both(pg, &ids, 0, START + 170, "forgotten with it, or ended");
    set(START + 60);
    both(
        pg,
        &ids,
        3,
        START + 60,
        "a clock that stepped back keeps a session",
    );
    // An absolute deadline of 1000 holds however used.
    for t in (START + 160..START + 1000).step_by(90) {
        set(t);
        both(pg, &ids, 3, t, "kept alive by use");
    }
    set(START + 1000);
    both(pg, &ids, 3, START + 1000, "past the absolute deadline");

    // The change of password, the end, and the end of a user's others.
    set(START + 1010);
    make(pg, &mut ids, "ann", Some(START + 2000), START + 1010);
    make(pg, &mut ids, "ann", None, START + 1010);
    make(pg, &mut ids, "dee", None, START + 1010);
    let n = ids.len() - 3;
    let want = table.clear_must_change(&ids[n]).map(|v| live_of(&v));
    let got = clear_must_change(pg, &rules, &hash(&ids[n]), at(START + 1010)).expect("cleared");
    assert_eq!(got, want);
    assert!(got.is_some());
    set(START + 1200);
    assert_eq!(
        clear_must_change(pg, &rules, &hash(&ids[n]), at(START + 1200)).expect("cleared"),
        None,
        "an expired session is not changed"
    );
    assert!(table.clear_must_change(&ids[n]).is_none());
    set(START + 1011);
    assert_eq!(
        clear_must_change(pg, &rules, &hash("no such id"), at(START + 1011)).expect("cleared"),
        None
    );
    let kept = hash(&ids[n + 1]);
    assert_eq!(
        usize::try_from(end_user(pg, "ann", Some(&kept)).expect("ended")).expect("small"),
        table.end_user("ann", Some(&ids[n + 1])),
    );
    both(pg, &ids, n + 1, START + 1011, "the kept one");
    both(pg, &ids, n, START + 1011, "an ended one");
    assert_eq!(
        end(pg, &hash(&ids[n + 2])).expect("ended"),
        table.end(&ids[n + 2])
    );
    assert_eq!(
        end(pg, &hash(&ids[n + 2])).expect("ended"),
        table.end(&ids[n + 2])
    );
    assert_eq!(
        usize::try_from(end_user(pg, "ann", None).expect("ended")).expect("small"),
        table.end_user("ann", None)
    );
    assert_eq!(
        held(pg).expect("held"),
        i64::try_from(table.len()).expect("small")
    );
}

/// Proves: a session one coordinator made is answered by another with the same user, token and
/// deadlines, and the cookie's id is not in the table (only its SHA-256); the other coordinator's
/// end of it ends it for the first.
///
/// Catches: sessions that live in one coordinator, so a deploy signs everyone out.
#[test]
fn a_session_made_by_one_coordinator_is_answered_by_another() {
    let (db, _store, mut sessions) = rig(2);
    let (_, rules) = rules(100, 1000, 8);
    let mut b = sessions.pop().expect("a session");
    let mut a = sessions.pop().expect("a session");
    let id = "cookie-value";
    create(
        &mut a,
        &rules,
        &NewSession {
            id_sha256: &hash(id),
            username: "ann",
            csrf: "tok",
            must_change: true,
            must_change_until: Some(5000),
        },
        1000,
    )
    .expect("made");
    assert_eq!(
        lookup(&mut b, &rules, &hash(id), 1010).expect("looked up"),
        Some(Live {
            username: "ann".into(),
            csrf: "tok".into(),
            must_change: true,
            must_change_until: Some(5000)
        })
    );
    assert!(
        db.rows(
            "SELECT count(*) FROM sessions WHERE convert_from(id_sha256, 'LATIN1') = 'cookie-value'"
        )[0][0]
            .as_deref()
            == Some("0")
    );
    assert!(end(&mut b, &hash(id)).expect("ended"));
    assert_eq!(
        lookup(&mut a, &rules, &hash(id), 1011).expect("looked up"),
        None
    );
}

/// Proves: coordinators making sessions together past the cap leave exactly the cap's number
/// held, never more.
///
/// Catches: a cap that two logins at once both pass.
#[test]
fn sessions_made_together_never_pass_the_cap() {
    for round in 0..4 {
        let (_db, _store, sessions) = rig(3);
        let (_, rules) = rules(100, 1000, 4);
        let start = Barrier::new(3);
        let mut sessions = sessions;
        std::thread::scope(|scope| {
            for (who, session) in sessions.iter_mut().enumerate() {
                let (start, rules) = (&start, &rules);
                scope.spawn(move || {
                    start.wait();
                    for n in 0..6 {
                        create(
                            session,
                            rules,
                            &NewSession {
                                id_sha256: &hash(&format!("{who}-{n}")),
                                username: "u",
                                csrf: "c",
                                must_change: false,
                                must_change_until: None,
                            },
                            1000 + i64::from(n),
                        )
                        .expect("made");
                    }
                });
            }
        });
        assert_eq!(held(&mut sessions[0]).expect("held"), 4, "round {round}");
    }
}

/// Proves: on one script (failures to the limit, a failure while locked, the lock's end, a success,
/// a name forgotten after a lockout, names pressed out past the number tracked) the rows give what
/// the in-memory limiter gives, as the outcome of each failure, whether each name is locked, and
/// how many names are held.
///
/// Catches: a count that is off by one, a lock that moves when a failure arrives while locked, and
/// the wrong name dropped to make room.
#[test]
fn the_rows_count_as_the_in_memory_limiter_does() {
    let (_db, _store, mut sessions) = rig(1);
    let pg = &mut sessions[0];
    let clock = Arc::new(Clock(AtomicU64::new(START)));
    let limiter = LoginLimiter::new(
        LimiterSettings {
            max_failures: 3,
            lockout_secs: 100,
            max_tracked: 3,
        },
        clock.clone(),
        HashSlots::new(NonZeroUsize::new(1).expect("one")),
    );
    let rules = LimiterRules {
        max_failures: 3,
        lockout_secs: 100,
        max_tracked: 3,
    };
    let script: [(u64, &str, bool); 24] = [
        (0, "ann", false),
        (1, "ann", false),
        (2, "ann", false),
        (3, "ann", false),
        (50, "ann", false),
        (102, "ann", false),
        (103, "ann", false),
        (104, "ann", false),
        (105, "ann", true),
        (106, "bob", false),
        (107, "cy", false),
        (108, "dee", false),
        (109, "eve", false),
        (110, "eve", false),
        (111, "eve", false),
        (112, "fay", false),
        (113, "gus", false),
        (400, "gus", false),
        (401, "hal", false),
        (402, "hal", false),
        (403, "hal", false),
        (404, "ivy", false),
        (405, "ann", false),
        (406, "ann", true),
    ];
    for (step, (t, name, succeed)) in script.into_iter().enumerate() {
        let now = START + t;
        clock.0.store(now, Ordering::SeqCst);
        if succeed {
            limiter.succeeded(name);
            succeeded(pg, name).expect("cleared");
        } else {
            assert_eq!(
                failed(pg, &rules, name, at(now)).expect("counted"),
                limiter.failed(name),
                "step {step}: {name} at {t}"
            );
        }
        for who in ["ann", "bob", "cy", "dee", "eve", "fay", "gus", "hal", "ivy"] {
            assert_eq!(
                is_locked(pg, who, at(now)).expect("asked"),
                limiter.is_locked(who),
                "step {step}: {who} locked at {t}"
            );
        }
        assert_eq!(
            tracked(pg).expect("tracked"),
            i64::try_from(limiter.tracked()).expect("small"),
            "step {step}: names held"
        );
    }
}

/// Proves: two coordinators counting the failures of one name at once count them all: of the
/// twenty failures exactly one is the first of the run and exactly one locks the name, and the
/// stored count is twenty.
///
/// Catches: failures read and written back by two coordinators, so that some are lost and a name
/// is locked late or not at all.
#[test]
fn failures_counted_together_are_all_counted_and_lock_once() {
    for round in 0..4 {
        let (db, _store, mut sessions) = rig(2);
        let rules = LimiterRules {
            max_failures: 5,
            lockout_secs: 100,
            max_tracked: 10,
        };
        let start = Barrier::new(2);
        let outcomes: Vec<Vec<Failure>> = std::thread::scope(|scope| {
            let handles: Vec<_> = sessions
                .iter_mut()
                .map(|session| {
                    let (start, rules) = (&start, &rules);
                    scope.spawn(move || {
                        start.wait();
                        (0..10)
                            .map(|n| failed(session, rules, "ann", 1000 + n).expect("counted"))
                            .collect()
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("joins"))
                .collect()
        });
        let all: Vec<Failure> = outcomes.into_iter().flatten().collect();
        assert_eq!(
            all.iter().filter(|f| **f == Failure::First).count(),
            1,
            "round {round}"
        );
        assert_eq!(
            all.iter().filter(|f| **f == Failure::Locked).count(),
            1,
            "round {round}"
        );
        assert_eq!(
            db.rows("SELECT failures FROM login_failures WHERE name = 'ann'"),
            vec![vec![Some("20".to_owned())]]
        );
    }
}
