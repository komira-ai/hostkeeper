//! The sessions and the login limiter over their rows (`login_rows`, migration `0025`): the
//! answers of the in-memory tables (`crate::session`) on the same scripts, the writes a lookup
//! makes and does not make, and the answers when the database is lost.
//!
//! The clock is a number each test sets; nothing sleeps.

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::TestDatabase;
use crate::enrollment::Random;
use crate::login_support::Seconds;
use crate::password::HashSlots;
use crate::session::{
    Failure, LimiterSettings, LoginLimiter, Refusal, SessionError, SessionSettings, SessionView,
    Sessions,
};
use crate::store::postgres::Postgres;
use crate::store::postgres::login_rows::PgLoginRows;

pub(super) const START: u64 = 1_800_000_000;

/// Random bytes that never repeat, from `first` on.
pub(super) fn counting_from(first: u64) -> Random {
    let next = AtomicU64::new(first);
    Box::new(move |buffer| {
        let n = next.fetch_add(1, Ordering::SeqCst).to_le_bytes();
        for (i, byte) in buffer.iter_mut().enumerate() {
            *byte = n[i % 8].wrapping_add(u8::try_from(i).unwrap_or(0));
        }
        true
    })
}

pub(super) fn sessions_of(idle_secs: u64, max_secs: u64, max_count: usize) -> SessionSettings {
    SessionSettings {
        idle_secs,
        max_secs,
        max_count,
    }
}

pub(super) fn limits(max_failures: u32, lockout_secs: u64, max_tracked: usize) -> LimiterSettings {
    LimiterSettings {
        max_failures,
        lockout_secs,
        max_tracked,
    }
}

fn slots() -> HashSlots {
    HashSlots::new(NonZeroUsize::new(2).expect("two"))
}

/// A database, one store on it and a clock the test sets, which the rows judge by.
struct Rig {
    db: TestDatabase,
    store: Postgres,
    now: Arc<AtomicU64>,
}

impl Rig {
    fn new() -> Rig {
        let db = TestDatabase::new();
        let (store, _) = db.open("login rows").expect("the store opens");
        Rig {
            db,
            store,
            now: Arc::new(AtomicU64::new(START)),
        }
    }

    fn set(&self, now: u64) {
        self.now.store(now, Ordering::SeqCst);
    }

    /// The rows over the rig's store under the rig's clock.
    fn rows(
        &self,
        sessions: SessionSettings,
        limiter: LimiterSettings,
        touch_secs: u64,
    ) -> Arc<PgLoginRows> {
        let now = self.now.clone();
        Arc::new(
            PgLoginRows::new(self.store.clone(), sessions, limiter, touch_secs)
                .with_clock(Arc::new(move || now.load(Ordering::SeqCst))),
        )
    }

    /// An empty table of sessions in memory and one over the rows, both judged by the rig's
    /// clock and drawing the same random bytes, so that they make the same ids.
    fn sessions(&self, settings: SessionSettings, touch_secs: u64) -> Pair {
        self.db.run("DELETE FROM sessions").expect("emptied");
        let rows = self.rows(settings, LimiterSettings::default(), touch_secs);
        let clock = || Arc::new(Seconds(self.now.clone()));
        Pair {
            mem: Sessions::new(settings, clock(), counting_from(1)),
            pg: Sessions::new(settings, clock(), counting_from(1)).over(rows),
        }
    }

    /// An empty limiter in memory and one over the rows.
    fn limiter(&self, settings: LimiterSettings) -> Counts {
        self.db.run("DELETE FROM login_failures").expect("emptied");
        let rows = self.rows(SessionSettings::default(), settings, 0);
        let clock = || Arc::new(Seconds(self.now.clone()));
        Counts {
            mem: LoginLimiter::new(settings, clock(), slots()),
            pg: LoginLimiter::new(settings, clock(), slots()).over(rows),
        }
    }

    fn column(&self, sql: &str) -> Vec<String> {
        self.db
            .rows(sql)
            .into_iter()
            .map(|row| row[0].clone().expect("a value"))
            .collect()
    }

    fn last_used(&self) -> Vec<String> {
        self.column("SELECT last_used_unix FROM sessions ORDER BY serial")
    }

    fn stop(self) {
        self.store.stop();
    }
}

/// The two tables of sessions, asked the same at each step.
struct Pair {
    mem: Sessions,
    pg: Sessions,
}

impl Pair {
    /// A session made in both, the same in both.
    fn make(&self, user: &str, must: Option<u64>) -> String {
        let (a, view_a) = self.mem.create(user, must.is_some(), must).expect("made");
        let (b, view_b) = self.pg.create(user, must.is_some(), must).expect("made");
        assert_eq!(a, b);
        assert_eq!(view_a, view_b);
        assert_eq!(self.mem.len(), self.pg.len());
        a.as_str().to_owned()
    }

    /// A lookup, answered the same by both.
    fn look(&self, id: &str) -> Option<SessionView> {
        let (a, b) = (self.mem.lookup(id), self.pg.lookup(id));
        assert_eq!(a, b, "lookup");
        assert_eq!(self.mem.len(), self.pg.len(), "held after a lookup");
        b
    }

    /// Whether each of `ids` lives, asked of both without using it.
    fn live(&self, ids: &[&str]) -> Vec<bool> {
        ids.iter()
            .map(|id| {
                let key = Sessions::key_of(id);
                let (a, b) = (self.mem.is_live(&key), self.pg.is_live(&key));
                assert_eq!(a, b, "is_live");
                b
            })
            .collect()
    }

    fn touch(&self, id: &str) -> bool {
        let key = Sessions::key_of(id);
        let (a, b) = (self.mem.touch(&key), self.pg.touch(&key));
        assert_eq!(a, b, "touch");
        assert_eq!(self.mem.len(), self.pg.len(), "held after a touch");
        b
    }

    fn clear(&self, id: &str) -> Option<SessionView> {
        let (a, b) = (
            self.mem.clear_must_change(id),
            self.pg.clear_must_change(id),
        );
        assert_eq!(a, b, "clear_must_change");
        b
    }

    fn end(&self, id: &str) -> bool {
        let (a, b) = (self.mem.end(id), self.pg.end(id));
        assert_eq!(a, b, "end");
        b.expect("the rows are up")
    }

    fn end_user(&self, user: &str, keep: Option<&str>) -> usize {
        let (a, b) = (self.mem.end_user(user, keep), self.pg.end_user(user, keep));
        assert_eq!(a, b, "end_user");
        b.expect("the rows are up")
    }

    fn len(&self) -> usize {
        assert_eq!(self.mem.len(), self.pg.len());
        self.pg.len()
    }
}

/// The two limiters, asked the same at each step.
struct Counts {
    mem: LoginLimiter,
    pg: LoginLimiter,
}

impl Counts {
    fn fail(&self, name: &str) -> Failure {
        let (a, b) = (self.mem.failed(name), self.pg.failed(name));
        assert_eq!(a, b, "failed {name}");
        assert_eq!(self.mem.tracked(), self.pg.tracked(), "tracked");
        b
    }

    fn locked(&self, name: &str) -> bool {
        let (a, b) = (self.mem.is_locked(name), self.pg.is_locked(name));
        assert_eq!(a, b, "is_locked {name}");
        b
    }

    fn succeed(&self, name: &str) {
        self.mem.succeeded(name);
        self.pg.succeeded(name);
        assert_eq!(self.mem.tracked(), self.pg.tracked(), "tracked");
    }

    fn tracked(&self) -> usize {
        assert_eq!(self.mem.tracked(), self.pg.tracked());
        self.pg.tracked()
    }
}

/// Proves: on one script the rows answer, step by step, what the in-memory table answers: the idle
/// deadline (alive one second before it, over at it, every use pushing it out), the absolute
/// deadline (alive one second before, over at it, however used), a clock that steps back (a
/// session is kept and its last use is not moved to the past), the cap (a tie in time goes to the
/// session made first, the least recently used goes first, expired ones go before a live one), a
/// change of password (also of an expired session, which it leaves in place), an end, the end of
/// every session of a user with and without one kept, and the answers by the handle (`is_live`
/// asks, `touch` uses).
///
/// Catches: a deadline off by one second, a clock stepped back that ends a session or writes a past
/// time, an eviction of the wrong session, a lookup that does not push the idle deadline out, an
/// expired session that is answered, and an `end_user` that ends the kept session or another
/// user's.
#[test]
fn the_rows_answer_as_the_in_memory_sessions_do() {
    let rig = Rig::new();

    // The idle deadline: used at +99 and +198, over 100 seconds after that.
    let p = rig.sessions(sessions_of(100, 1000, 10), 0);
    let a = p.make("alice", None);
    rig.set(START + 99);
    assert!(p.look(&a).is_some());
    assert_eq!(rig.last_used(), [(START + 99).to_string()]);
    rig.set(START + 198);
    assert!(p.look(&a).is_some());
    rig.set(START + 297);
    assert!(p.live(&[&a])[0], "99 seconds idle");
    rig.set(START + 298);
    assert!(
        !p.live(&[&a])[0],
        "asking at the deadline: 100 seconds idle"
    );
    assert_eq!(p.len(), 1, "asking forgets nothing");
    assert_eq!(p.look(&a), None);
    assert_eq!(p.len(), 0, "the lookup forgot it");
    assert_eq!(p.look("not a session"), None);

    // The absolute deadline, with an idle one that never comes first.
    let p = rig.sessions(sessions_of(5000, 1000, 10), 0);
    rig.set(START);
    let b = p.make("bob", None);
    rig.set(START + 999);
    assert!(p.look(&b).is_some());
    rig.set(START + 1000);
    assert_eq!(p.look(&b), None, "1000 seconds old, used a second ago");
    assert_eq!(p.len(), 0);

    // A clock that steps back.
    let p = rig.sessions(sessions_of(100, 1000, 10), 0);
    rig.set(START);
    let c = p.make("carol", None);
    rig.set(START - 500);
    assert!(p.look(&c).is_some());
    assert_eq!(
        rig.last_used(),
        [START.to_string()],
        "not moved to the past"
    );
    let behind = p.make("dave", None);
    rig.set(START - 1000);
    assert_eq!(p.live(&[&c, &behind]), [true, true]);
    rig.set(START + 99);
    assert_eq!(p.live(&[&c, &behind]), [true, false]);
    rig.set(START + 100);
    assert_eq!(p.live(&[&c, &behind]), [false, false]);

    // The cap of three: a tie goes to the first made.
    let p = rig.sessions(sessions_of(100, 1000, 3), 0);
    rig.set(START);
    let (x, y, z) = (p.make("u", None), p.make("u", None), p.make("u", None));
    let w = p.make("u", None);
    assert_eq!(p.len(), 3);
    assert_eq!(p.live(&[&x, &y, &z, &w]), [false, true, true, true]);
    // The least recently used goes, not the oldest.
    rig.set(START + 10);
    assert!(p.look(&y).is_some());
    let v = p.make("u", None);
    assert_eq!(p.live(&[&y, &z, &w, &v]), [true, false, true, true]);
    // The expired ones go before a live one is dropped.
    rig.set(START + 109);
    assert_eq!(p.live(&[&y, &w, &v]), [true, false, true]);
    let u = p.make("u", None);
    assert_eq!(p.len(), 3, "w expired and went, so no live one was dropped");
    assert_eq!(p.live(&[&y, &v, &u]), [true, true, true]);
    rig.set(START + 111);
    let t = p.make("u", None);
    assert_eq!(p.len(), 2, "y and v expired and went, u and t are held");
    assert_eq!(p.live(&[&y, &v, &u, &t]), [false, false, true, true]);
    // Equal in use, the older goes first, though made after.
    let p = rig.sessions(sessions_of(1000, 5000, 2), 0);
    rig.set(START);
    let old = p.make("u", None);
    rig.set(START + 5);
    let young = p.make("u", None);
    rig.set(START + 7);
    assert!(p.look(&old).is_some());
    assert!(p.look(&young).is_some());
    let next = p.make("u", None);
    assert_eq!(p.live(&[&old, &young, &next]), [false, true, true]);

    // A change of password, an end, and the end of a user's sessions.
    let p = rig.sessions(sessions_of(100, 1000, 10), 0);
    rig.set(START);
    let m = p.make("erin", Some(START + 3600));
    let seen = p.look(&m).expect("live");
    assert!(seen.must_change);
    assert_eq!(seen.must_change_until, Some(START + 3600));
    let cleared = p.clear(&m).expect("live");
    assert!(!cleared.must_change && cleared.must_change_until.is_none());
    assert_eq!(p.clear(&m), Some(cleared), "again, the same");
    assert_eq!(p.clear("not a session"), None);
    let late = p.make("erin", Some(START + 3600));
    rig.set(START + 100);
    assert_eq!(p.clear(&late), None, "expired");
    assert_eq!(p.len(), 2, "left for a lookup to forget");
    assert!(!p.touch(&late), "a touch forgets it");
    assert_eq!(p.len(), 1);
    assert!(!p.touch("not a session"));
    rig.set(START);
    assert!(p.touch(&m));
    assert!(p.end(&m));
    assert!(!p.end(&m));
    assert_eq!(p.len(), 0);
    let ids: Vec<String> = ["frank", "frank", "frank", "grace"]
        .iter()
        .map(|user| p.make(user, None))
        .collect();
    assert_eq!(p.end_user("frank", Some(&ids[1])), 2);
    assert_eq!(
        p.live(&[&ids[0], &ids[1], &ids[2], &ids[3]]),
        [false, true, false, true]
    );
    assert_eq!(p.end_user("frank", None), 1);
    assert_eq!(p.end_user("frank", None), 0);
    assert_eq!(p.end_user("nobody", Some(&ids[3])), 0);
    assert_eq!(p.len(), 1);
    rig.stop();
}

/// Proves: on one script the rows count as the in-memory limiter does: the first failure, the
/// failures up to the limit, the lock (alive one second before its end, over at it), a failure
/// while locked counted without moving the lock, a lock ended by the clock that starts a new run, a
/// success that clears a count and a lock, a name forgotten a lockout after its last failure (and
/// not one second before), and past the cap on names the unlocked name with the oldest last
/// failure dropped first, a locked one only when all are, ties by name.
///
/// Catches: a lock that a late failure moves, a count that survives a success, a run that is not
/// forgotten, and an eviction of a locked name while an unlocked one is held.
#[test]
fn the_rows_count_as_the_in_memory_limiter_does() {
    let rig = Rig::new();

    let c = rig.limiter(limits(3, 100, 10));
    rig.set(START);
    assert_eq!(c.fail("amy"), Failure::First);
    rig.set(START + 1);
    assert_eq!(c.fail("amy"), Failure::Counted);
    assert!(!c.locked("amy"));
    rig.set(START + 2);
    assert_eq!(c.fail("amy"), Failure::Locked);
    assert!(c.locked("amy"));
    assert!(!c.locked("ben"), "a lock is of one name");
    rig.set(START + 50);
    assert_eq!(c.fail("amy"), Failure::Counted, "while locked");
    rig.set(START + 101);
    assert!(
        c.locked("amy"),
        "the lock of the third failure, not moved by the fourth"
    );
    rig.set(START + 102);
    assert!(!c.locked("amy"));
    assert_eq!(c.fail("amy"), Failure::First, "a new run");
    assert_eq!(
        rig.column("SELECT failures FROM login_failures WHERE name = 'amy'"),
        ["1"]
    );
    // A success clears the count and the lock.
    c.succeed("amy");
    assert_eq!(c.tracked(), 0);
    assert_eq!(c.fail("amy"), Failure::First);
    assert_eq!(c.fail("amy"), Failure::Counted);
    assert_eq!(c.fail("amy"), Failure::Locked);
    c.succeed("amy");
    assert!(!c.locked("amy"));
    c.succeed("nobody");

    // A run is forgotten a lockout after its last failure.
    let c = rig.limiter(limits(3, 100, 10));
    rig.set(START);
    assert_eq!(c.fail("old"), Failure::First);
    rig.set(START + 99);
    assert_eq!(c.fail("newer"), Failure::First);
    assert_eq!(c.tracked(), 2, "99 seconds is not forgotten");
    rig.set(START + 100);
    assert_eq!(c.fail("newest"), Failure::First);
    assert_eq!(c.tracked(), 2);
    assert_eq!(
        rig.column("SELECT name FROM login_failures ORDER BY name"),
        ["newer", "newest"]
    );
    assert_eq!(c.fail("old"), Failure::First, "forgotten, so a first");

    // A lock is kept while it lasts, whatever the age of its last failure.
    let c = rig.limiter(limits(1, 100, 10));
    rig.set(START);
    assert_eq!(c.fail("held"), Failure::Locked);
    rig.set(START + 99);
    assert_eq!(c.fail("other"), Failure::Locked);
    assert!(c.locked("held"));
    assert_eq!(c.tracked(), 2);

    // The cap on names: the unlocked with the oldest last failure goes first.
    let c = rig.limiter(limits(3, 1000, 3));
    rig.set(START);
    assert_eq!(c.fail("x"), Failure::First);
    for (second, expect) in [
        (1, Failure::First),
        (2, Failure::Counted),
        (3, Failure::Locked),
    ] {
        rig.set(START + second);
        assert_eq!(c.fail("y"), expect);
    }
    rig.set(START + 4);
    assert_eq!(c.fail("z"), Failure::First);
    rig.set(START + 5);
    assert_eq!(c.fail("w"), Failure::First);
    assert_eq!(
        rig.column("SELECT name FROM login_failures ORDER BY name"),
        ["w", "y", "z"],
        "x, the oldest unlocked, went"
    );
    rig.set(START + 6);
    assert_eq!(c.fail("v"), Failure::First);
    assert_eq!(
        rig.column("SELECT name FROM login_failures ORDER BY name"),
        ["v", "w", "y"],
        "z, the oldest unlocked of the rest, went; y is locked"
    );
    assert!(c.locked("y"));
    // A name already held makes no room.
    assert_eq!(c.fail("v"), Failure::Counted);
    assert_eq!(c.tracked(), 3);

    // All locked: the oldest goes, ties by name.
    let c = rig.limiter(limits(1, 1000, 2));
    rig.set(START);
    for name in ["q", "p"] {
        assert_eq!(c.fail(name), Failure::Locked);
    }
    assert_eq!(c.fail("r"), Failure::Locked);
    assert_eq!(
        rig.column("SELECT name FROM login_failures ORDER BY name"),
        ["q", "r"],
        "p and q tie on the time, p first by name"
    );
    rig.set(START + 1);
    assert_eq!(c.fail("s"), Failure::Locked);
    assert_eq!(
        rig.column("SELECT name FROM login_failures ORDER BY name"),
        ["r", "s"],
        "q and r tie, q first by name"
    );
    rig.stop();
}

/// Proves: with `--session-touch-secs` 30 a use less than 30 seconds after the last write writes
/// nothing (the row's `last_used_unix` stays), a use 30 seconds after writes the second of the
/// use, a use of a session by its handle is the same, asking by the handle writes nothing at all,
/// and the idle deadline is judged by the stored second.
///
/// Catches: a write at every use, which makes every request of the console a write to the shared
/// database, and a touch interval that never writes, which lets a used session expire.
#[test]
fn a_use_inside_the_touch_interval_writes_nothing_and_one_past_it_moves_last_used() {
    let rig = Rig::new();
    let p = rig.sessions(sessions_of(1000, 5000, 10), 30);
    let (a, _) = p.pg.create("alice", false, None).expect("made");
    let a = a.as_str().to_owned();
    assert_eq!(rig.last_used(), [START.to_string()]);
    for (second, stored) in [(29, 0), (30, 30), (59, 30), (60, 60), (89, 60)] {
        rig.set(START + second);
        assert!(p.pg.lookup(&a).is_some());
        assert_eq!(
            rig.last_used(),
            [(START + stored).to_string()],
            "at +{second}"
        );
    }
    // By the handle: a use inside the interval writes nothing, one past it writes.
    rig.set(START + 89);
    assert!(p.pg.touch(&Sessions::key_of(&a)));
    assert_eq!(rig.last_used(), [(START + 60).to_string()]);
    rig.set(START + 90);
    assert!(p.pg.touch(&Sessions::key_of(&a)));
    assert_eq!(rig.last_used(), [(START + 90).to_string()]);
    // Asking by the handle is no use at all.
    rig.set(START + 500);
    assert_eq!([p.pg.is_live(&Sessions::key_of(&a))], [true]);
    assert_eq!(rig.last_used(), [(START + 90).to_string()]);

    // Used inside the interval only, the deadline is that of the stored second.
    let p = rig.sessions(sessions_of(1000, 5000, 10), 30);
    rig.set(START);
    let (b, _) = p.pg.create("bob", false, None).expect("made");
    let b = b.as_str().to_owned();
    rig.set(START + 29);
    assert!(p.pg.lookup(&b).is_some());
    assert_eq!(rig.last_used(), [START.to_string()]);
    rig.set(START + 999);
    assert!(p.pg.is_live(&Sessions::key_of(&b)));
    rig.set(START + 1000);
    assert_eq!(
        p.pg.lookup(&b),
        None,
        "1000 seconds after the stored second"
    );
    rig.stop();
}

/// Proves: a lookup, a touch or an end that finds an expired session deletes its row (the idle
/// deadline and the absolute one alike) and only its row, asking by the handle leaves it, and a
/// lookup of an unknown id changes nothing.
///
/// Catches: expired sessions that stay in the table for ever (they count against the cap), and a
/// delete of a session that has not expired.
#[test]
fn an_expired_session_is_forgotten_by_the_lookup_that_finds_it() {
    let rig = Rig::new();
    let rows = rig.rows(sessions_of(100, 300, 10), LimiterSettings::default(), 0);
    let sessions = Sessions::new(
        sessions_of(100, 300, 10),
        Arc::new(Seconds(rig.now.clone())),
        counting_from(1),
    )
    .over(rows);
    let (idle, _) = sessions.create("a", false, None).expect("made");
    let (kept, _) = sessions.create("b", false, None).expect("made");
    let (touched, _) = sessions.create("c", false, None).expect("made");
    rig.set(START + 99);
    assert!(sessions.lookup(kept.as_str()).is_some());
    assert!(sessions.touch(&Sessions::key_of(touched.as_str())));
    rig.set(START + 100);
    assert!(!sessions.is_live(&Sessions::key_of(idle.as_str())));
    assert_eq!(sessions.len(), 3, "asking forgets nothing");
    assert_eq!(sessions.lookup("unknown"), None);
    assert_eq!(sessions.len(), 3);
    assert_eq!(sessions.lookup(idle.as_str()), None);
    assert_eq!(sessions.len(), 2);
    assert_eq!(
        rig.column("SELECT username FROM sessions ORDER BY username"),
        ["b", "c"]
    );
    // The absolute deadline: b and c were used at +99, and are 300 seconds old at +300.
    rig.set(START + 190);
    assert!(sessions.lookup(kept.as_str()).is_some());
    rig.set(START + 280);
    assert!(sessions.lookup(kept.as_str()).is_some());
    rig.set(START + 300);
    assert!(!sessions.touch(&Sessions::key_of(touched.as_str())));
    assert_eq!(rig.column("SELECT username FROM sessions"), ["b"]);
    assert_eq!(sessions.lookup(kept.as_str()), None);
    assert!(sessions.is_empty());
    rig.stop();
}

/// Proves: a limiter and a table of sessions over rows that the database cannot reach fail closed:
/// no session is made (the error says the rows are unavailable and why), none is found, touched,
/// changed or ended, no user's are ended, the held count is 0 (and the table empty), every name is
/// locked (a login is refused as locked) while the failure is counted as one that was counted, a
/// success clears nothing and no name is tracked; the rows the lost calls did not change are there
/// when a store is started again.
///
/// Catches: a lost database that signs everybody in, lets a guesser go on guessing, or panics, and
/// a call that deletes rows it could not judge.
#[test]
fn a_lost_database_fails_the_login_closed() {
    super::logging();
    let rig = Rig::new();
    let (settings, limiter) = (sessions_of(1000, 5000, 10), limits(3, 1000, 10));
    let rows = rig.rows(settings, limiter, 0);
    let clock = || Arc::new(Seconds(rig.now.clone()));
    let sessions = Sessions::new(settings, clock(), counting_from(1)).over(rows.clone());
    let counts = LoginLimiter::new(limiter, clock(), slots()).over(rows);
    let (id, view) = sessions
        .create("alice", true, Some(START + 10))
        .expect("made");
    assert_eq!(counts.failed("mallory"), Failure::First);
    assert!(!counts.is_locked("mallory"));

    rig.store.stop();
    let refused = sessions.create("bob", false, None).expect_err("no rows");
    assert_eq!(
        refused,
        SessionError::Unavailable("the writing session stopped".to_owned())
    );
    assert_eq!(
        refused.to_string(),
        "the session rows are unavailable: the writing session stopped"
    );
    let key = Sessions::key_of(id.as_str());
    assert_eq!(sessions.lookup(id.as_str()), None);
    assert!(!sessions.is_live(&key));
    assert!(!sessions.touch(&key));
    assert_eq!(sessions.clear_must_change(id.as_str()), None);
    assert_eq!(
        sessions.end(id.as_str()),
        Err(SessionError::Unavailable(
            "the writing session stopped".to_owned()
        ))
    );
    assert_eq!(
        sessions.end_user("alice", None),
        Err(SessionError::Unavailable(
            "the writing session stopped".to_owned()
        ))
    );
    assert_eq!(sessions.len(), 0);
    assert!(sessions.is_empty());

    assert!(counts.is_locked("anyone"));
    assert!(counts.is_locked("mallory"));
    assert_eq!(counts.admit("anyone").map(|_| ()), Err(Refusal::Locked));
    assert_eq!(counts.failed("mallory"), Failure::Counted);
    counts.succeeded("mallory");
    assert_eq!(counts.tracked(), 0);
    assert!(super::logged("the login rows could not be reached"));

    // What the lost calls could not judge is as it was.
    assert_eq!(
        rig.column("SELECT username || ':' || must_change FROM sessions"),
        ["alice:true"]
    );
    assert_eq!(
        rig.column("SELECT name || ':' || failures FROM login_failures"),
        ["mallory:1"]
    );
    let (second, _) = rig.db.open("second").expect("a store again");
    let rows = Arc::new(PgLoginRows::new(second.clone(), settings, limiter, 0));
    let again = Sessions::new(settings, clock(), counting_from(1)).over(rows);
    assert_eq!(again.lookup(id.as_str()), Some(view));
    second.stop();
}

/// Proves: numbers past what the columns hold are the most they hold and a stored value below zero
/// reads as 0: a deadline of `u64::MAX` seconds never ends a session, a `must_change_until` of
/// `u64::MAX` is stored as the largest second the column holds (and read back as that), and a row
/// whose deadline was set below zero by hand reads as the second 0.
///
/// Catches: a conversion that wraps a huge setting to a negative one (every session expired at
/// once) or panics, and a negative column that panics the lookup.
#[test]
fn numbers_past_the_columns_saturate_and_a_negative_one_reads_as_zero() {
    let rig = Rig::new();
    let settings = sessions_of(u64::MAX, u64::MAX, usize::MAX);
    let rows = rig.rows(settings, LimiterSettings::default(), 0);
    let sessions = Sessions::new(
        settings,
        Arc::new(Seconds(rig.now.clone())),
        counting_from(1),
    )
    .over(rows);
    let (id, view) = sessions.create("a", true, Some(u64::MAX)).expect("made");
    assert_eq!(view.must_change_until, Some(u64::MAX));
    rig.set(START + 1_000_000);
    let seen = sessions.lookup(id.as_str()).expect("never expires");
    assert_eq!(seen.must_change_until, Some(i64::MAX as u64));
    rig.db
        .run("UPDATE sessions SET must_change_until = -5")
        .expect("set");
    let seen = sessions.lookup(id.as_str()).expect("live");
    assert_eq!(seen.must_change_until, Some(0));
    rig.stop();
}

/// Makes the writes of `DELETE` or `UPDATE` (`events`) on the sessions fail, as a database that
/// reads but refuses a write does.
fn refuse(rig: &Rig, events: &str) {
    rig.db
        .run(&format!(
            "CREATE FUNCTION refuse() RETURNS trigger LANGUAGE plpgsql AS \
             $$BEGIN RAISE EXCEPTION 'refused by the test'; END$$; \
             CREATE TRIGGER refuse BEFORE {events} ON sessions FOR EACH ROW EXECUTE FUNCTION refuse()"
        ))
        .expect("the trigger is made");
}

/// The SHA-256 of a session id, as the rows key it.
fn key_of(id: &str) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(id.as_bytes()).into()
}

/// Proves: when the database reads but refuses writes, a lookup of an expired session whose
/// forgetting is refused answers the write's error (the server's text of the refusal) and the
/// session table answers none for it (it fails closed, and the row stays); a lookup of a live
/// session that is due a touch whose update is refused answers the error too, and the table answers
/// none; with `push` false neither write is tried.
///
/// Catches: a lookup that answers a session it could not forget or touch, and one that swallows
/// the write's failure.
#[test]
fn a_lookup_whose_forget_or_touch_is_refused_fails_closed() {
    super::logging();
    let rig = Rig::new();
    let settings = sessions_of(100, 1000, 10);
    let rows = rig.rows(settings, LimiterSettings::default(), 10);
    let sessions = Sessions::new(
        settings,
        Arc::new(Seconds(rig.now.clone())),
        counting_from(1),
    )
    .over(rows.clone());
    let (id, _) = sessions.create("alice", false, None).expect("made");
    let key = key_of(id.as_str());
    refuse(&rig, "DELETE OR UPDATE");
    let text = |why: Result<_, String>| why.map(|found: Option<_>| found.is_some());

    // Live and due (20 seconds since its use, touched every 10): the update is refused.
    rig.set(START + 20);
    let refused = "ERROR: refused by the test (SQLSTATE P0001)";
    assert_eq!(text(rows.lookup(&key, true)), Err(refused.to_owned()));
    assert_eq!(sessions.lookup(id.as_str()), None);
    assert_eq!(text(rows.lookup(&key, false)), Ok(true));
    assert_eq!(rig.last_used(), [START.to_string()]);

    // Expired: the delete is refused.
    rig.set(START + 100);
    assert_eq!(text(rows.lookup(&key, true)), Err(refused.to_owned()));
    assert_eq!(sessions.lookup(id.as_str()), None);
    assert_eq!(text(rows.lookup(&key, false)), Ok(false));
    assert_eq!(rig.column("SELECT username FROM sessions"), ["alice"]);
    rig.stop();
}
