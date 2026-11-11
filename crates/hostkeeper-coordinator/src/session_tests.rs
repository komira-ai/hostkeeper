//! The session table and the login limiter, on a clock the test sets and random bytes the test
//! counts.

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hostkeeper_enrollment::validity::UnixClock;

use crate::enrollment::Random;
use crate::password::HashSlots;
use crate::session::{
    Failure, LimiterSettings, LoginLimiter, Refusal, SessionError, SessionSettings, Sessions,
};

const START: u64 = 1_800_000_000;

struct Clock(AtomicU64);

impl UnixClock for Clock {
    fn now_unix(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

fn clock() -> Arc<Clock> {
    Arc::new(Clock(AtomicU64::new(START)))
}

fn set(clock: &Clock, now: u64) {
    clock.0.store(now, Ordering::SeqCst);
}

/// Random bytes that differ at each call and never repeat.
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

fn settings(idle_secs: u64, max_secs: u64, max_count: usize) -> SessionSettings {
    SessionSettings {
        idle_secs,
        max_secs,
        max_count,
    }
}

fn sessions(settings: SessionSettings, clock: &Arc<Clock>) -> Sessions {
    Sessions::new(settings, clock.clone(), counting())
}

#[test]
fn the_defaults_are_the_numbers_of_the_design() {
    assert_eq!(settings(1800, 43200, 256), SessionSettings::default());
    assert_eq!(
        LimiterSettings {
            max_failures: 5,
            lockout_secs: 900,
            max_tracked: 4096
        },
        LimiterSettings::default()
    );
}

#[test]
fn a_session_is_32_random_bytes_and_a_csrf_token_of_its_own() {
    let clock = clock();
    let sessions = sessions(SessionSettings::default(), &clock);
    let (id, view) = sessions.create("admin", false, None).unwrap();
    assert_eq!(id.as_str().len(), 43);
    assert_eq!(view.csrf().len(), 43);
    assert_ne!(id.as_str(), view.csrf());
    assert!(
        id.as_str()
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    );
    let (other, other_view) = sessions.create("admin", false, None).unwrap();
    assert_ne!(id, other);
    assert_ne!(view.csrf(), other_view.csrf());
    assert_eq!(sessions.len(), 2);
    assert!(!sessions.is_empty());
}

#[test]
fn a_cookie_finds_its_session_and_no_other_value_does() {
    let clock = clock();
    let sessions = sessions(SessionSettings::default(), &clock);
    let (id, view) = sessions.create("admin", true, Some(START + 3600)).unwrap();
    assert_eq!(sessions.lookup(id.as_str()), Some(view.clone()));
    assert_eq!(view.username, "admin");
    assert!(view.must_change);
    assert_eq!(view.must_change_until, Some(START + 3600));
    assert_eq!(sessions.lookup("not-a-session"), None);
    assert_eq!(sessions.lookup(""), None);
    assert_eq!(sessions.lookup(view.csrf()), None);
}

#[test]
fn the_csrf_token_matches_only_itself() {
    let clock = clock();
    let sessions = sessions(SessionSettings::default(), &clock);
    let (_, view) = sessions.create("admin", false, None).unwrap();
    let token = view.csrf().to_owned();
    assert!(view.csrf_matches(&token));
    assert!(!view.csrf_matches(""));
    assert!(!view.csrf_matches(&token[..token.len() - 1]));
    assert!(!view.csrf_matches(&format!("{token}x")));
    let mut changed = token.clone();
    changed.replace_range(0..1, if token.starts_with('A') { "B" } else { "A" });
    assert!(!view.csrf_matches(&changed));
}

#[test]
fn only_a_hash_of_the_id_is_kept_and_no_debug_text_shows_a_secret() {
    let clock = clock();
    let sessions = sessions(SessionSettings::default(), &clock);
    let (id, view) = sessions.create("admin", false, None).unwrap();
    let text = format!("{id:?} {view:?} {sessions:?}");
    assert!(!text.contains(id.as_str()));
    assert!(!text.contains(view.csrf()));
    assert_eq!(format!("{id:?}"), "SessionId(<redacted>)");
    assert_eq!(
        format!("{view:?}"),
        "SessionView { username: \"admin\", must_change: false, must_change_until: None, csrf: \"<redacted>\" }"
    );
    assert_eq!(
        format!("{sessions:?}"),
        "Sessions { settings: SessionSettings { idle_secs: 1800, max_secs: 43200, max_count: 256 }, .. }"
    );
}

#[test]
fn a_session_ends_at_its_idle_deadline_and_each_use_pushes_it_out() {
    let clock = clock();
    let sessions = sessions(settings(100, 10_000, 8), &clock);
    let (id, _) = sessions.create("admin", false, None).unwrap();
    set(&clock, START + 99);
    assert!(sessions.lookup(id.as_str()).is_some());
    set(&clock, START + 198);
    assert!(
        sessions.lookup(id.as_str()).is_some(),
        "99 s after the last use"
    );
    set(&clock, START + 298);
    assert!(
        sessions.lookup(id.as_str()).is_none(),
        "100 s after the last use"
    );
    assert!(sessions.is_empty(), "an expired session is forgotten");
}

#[test]
fn a_session_ends_at_its_absolute_deadline_however_used() {
    let clock = clock();
    let sessions = sessions(settings(100, 250, 8), &clock);
    let (id, _) = sessions.create("admin", false, None).unwrap();
    for step in [50, 100, 150, 200, 249] {
        set(&clock, START + step);
        assert!(sessions.lookup(id.as_str()).is_some(), "at {step}");
    }
    set(&clock, START + 250);
    assert!(sessions.lookup(id.as_str()).is_none());
}

#[test]
fn a_clock_that_steps_back_keeps_a_session_and_never_ends_it() {
    let clock = clock();
    let sessions = sessions(settings(100, 1000, 8), &clock);
    let (id, _) = sessions.create("admin", false, None).unwrap();
    set(&clock, START + 90);
    assert!(sessions.lookup(id.as_str()).is_some());
    set(&clock, START - 5000);
    assert!(sessions.lookup(id.as_str()).is_some());
    set(&clock, START + 189);
    assert!(
        sessions.lookup(id.as_str()).is_some(),
        "the use at START + 90 still counts"
    );
    set(&clock, START + 290);
    assert!(sessions.lookup(id.as_str()).is_none());
}

#[test]
fn a_session_made_while_the_clock_is_behind_has_a_deadline_from_that_clock() {
    let clock = clock();
    let sessions = sessions(settings(100, 1000, 8), &clock);
    set(&clock, START - 5000);
    let (id, _) = sessions.create("admin", false, None).unwrap();
    set(&clock, START - 5000 + 99);
    assert!(sessions.lookup(id.as_str()).is_some());
    set(&clock, START - 5000 + 199);
    assert!(sessions.lookup(id.as_str()).is_none());
}

#[test]
fn past_the_cap_the_session_least_recently_used_is_dropped() {
    let clock = clock();
    let sessions = sessions(settings(1000, 100_000, 3), &clock);
    let (first, _) = sessions.create("a", false, None).unwrap();
    set(&clock, START + 1);
    let (second, _) = sessions.create("b", false, None).unwrap();
    set(&clock, START + 2);
    let (third, _) = sessions.create("c", false, None).unwrap();
    set(&clock, START + 3);
    assert!(
        sessions.lookup(first.as_str()).is_some(),
        "the first is used again"
    );
    set(&clock, START + 4);
    let (fourth, _) = sessions.create("d", false, None).unwrap();
    assert_eq!(sessions.len(), 3);
    assert!(
        sessions.lookup(second.as_str()).is_none(),
        "the idlest went"
    );
    for kept in [&first, &third, &fourth] {
        assert!(sessions.lookup(kept.as_str()).is_some());
    }
}

#[test]
fn of_two_sessions_idle_alike_the_older_one_is_dropped() {
    let clock = clock();
    let sessions = sessions(settings(1000, 100_000, 2), &clock);
    let (older, _) = sessions.create("a", false, None).unwrap();
    let (newer, _) = sessions.create("b", false, None).unwrap();
    let (latest, _) = sessions.create("c", false, None).unwrap();
    assert!(sessions.lookup(older.as_str()).is_none());
    assert!(sessions.lookup(newer.as_str()).is_some());
    assert!(sessions.lookup(latest.as_str()).is_some());
}

#[test]
fn expired_sessions_go_before_a_live_one_is_dropped() {
    let clock = clock();
    let sessions = sessions(settings(100, 100_000, 2), &clock);
    let (stale, _) = sessions.create("a", false, None).unwrap();
    set(&clock, START + 60);
    let (live, _) = sessions.create("b", false, None).unwrap();
    set(&clock, START + 120);
    let (newest, _) = sessions.create("c", false, None).unwrap();
    assert_eq!(sessions.len(), 2);
    assert!(sessions.lookup(stale.as_str()).is_none());
    assert!(sessions.lookup(live.as_str()).is_some());
    assert!(sessions.lookup(newest.as_str()).is_some());
}

#[test]
fn a_cap_of_zero_holds_one_session() {
    let clock = clock();
    let sessions = sessions(settings(1000, 1000, 0), &clock);
    let (first, _) = sessions.create("a", false, None).unwrap();
    let (second, _) = sessions.create("a", false, None).unwrap();
    assert_eq!(sessions.len(), 1);
    assert!(sessions.lookup(first.as_str()).is_none());
    assert!(sessions.lookup(second.as_str()).is_some());
}

#[test]
fn a_session_ended_by_its_cookie_is_gone_and_a_second_end_finds_none() {
    let clock = clock();
    let sessions = sessions(SessionSettings::default(), &clock);
    let (id, _) = sessions.create("admin", false, None).unwrap();
    let (other, _) = sessions.create("admin", false, None).unwrap();
    assert!(sessions.end(id.as_str()));
    assert!(!sessions.end(id.as_str()));
    assert!(!sessions.end("not-a-session"));
    assert!(sessions.lookup(id.as_str()).is_none());
    assert!(sessions.lookup(other.as_str()).is_some());
}

#[test]
fn ending_a_user_ends_all_their_sessions_and_no_one_elses() {
    let clock = clock();
    let sessions = sessions(SessionSettings::default(), &clock);
    let (one, _) = sessions.create("ann", false, None).unwrap();
    let (two, _) = sessions.create("ann", false, None).unwrap();
    let (bob, _) = sessions.create("bob", false, None).unwrap();
    assert_eq!(sessions.end_user("ann", None), 2);
    assert_eq!(sessions.end_user("ann", None), 0);
    assert_eq!(sessions.end_user("nobody", None), 0);
    assert!(sessions.lookup(one.as_str()).is_none());
    assert!(sessions.lookup(two.as_str()).is_none());
    assert!(sessions.lookup(bob.as_str()).is_some());
}

#[test]
fn ending_a_user_can_spare_the_session_that_asked() {
    let clock = clock();
    let sessions = sessions(SessionSettings::default(), &clock);
    let (one, _) = sessions.create("ann", false, None).unwrap();
    let (two, _) = sessions.create("ann", false, None).unwrap();
    let (bob, _) = sessions.create("bob", false, None).unwrap();
    assert_eq!(sessions.end_user("ann", Some(two.as_str())), 1);
    assert!(sessions.lookup(one.as_str()).is_none());
    assert!(sessions.lookup(two.as_str()).is_some());
    assert_eq!(
        sessions.end_user("ann", Some(bob.as_str())),
        1,
        "sparing another user's session spares nothing of ann's"
    );
    assert!(sessions.lookup(bob.as_str()).is_some());
}

#[test]
fn a_session_that_cannot_draw_random_bytes_is_not_made() {
    let clock = clock();
    let calls = Arc::new(AtomicU64::new(0));
    let seen = calls.clone();
    let random: Random = Box::new(move |buffer| {
        buffer.fill(7);
        seen.fetch_add(1, Ordering::SeqCst) < 1
    });
    let sessions = Sessions::new(SessionSettings::default(), clock, random);
    assert_eq!(
        sessions.create("admin", false, None).unwrap_err(),
        SessionError::NoRandom,
        "the id came, the csrf token did not"
    );
    assert!(sessions.is_empty());
    let none: Random = Box::new(|_| false);
    let sessions = Sessions::new(SessionSettings::default(), self::clock(), none);
    assert_eq!(
        sessions.create("admin", false, None).unwrap_err(),
        SessionError::NoRandom
    );
    assert_eq!(
        SessionError::NoRandom.to_string(),
        "no random bytes for a session id"
    );
    assert!(sessions.is_empty());
}

fn limiter(
    max_failures: u32,
    lockout_secs: u64,
    hashes: usize,
    clock: &Arc<Clock>,
) -> LoginLimiter {
    LoginLimiter::new(
        LimiterSettings {
            max_failures,
            lockout_secs,
            max_tracked: 4096,
        },
        clock.clone(),
        HashSlots::new(NonZeroUsize::new(hashes).unwrap()),
    )
}

#[test]
fn the_fifth_failure_locks_the_name_and_the_lock_ends_by_the_clock() {
    let clock = clock();
    let limiter = limiter(5, 900, 2, &clock);
    for _ in 0..4 {
        assert!(!limiter.is_locked("admin"));
        let _ = limiter.failed("admin");
    }
    assert!(limiter.admit("admin").is_ok());
    assert_eq!(limiter.failed("admin"), Failure::Locked);
    assert!(limiter.is_locked("admin"));
    assert_eq!(limiter.admit("admin").unwrap_err(), Refusal::Locked);
    set(&clock, START + 899);
    assert!(limiter.is_locked("admin"));
    set(&clock, START + 900);
    assert!(!limiter.is_locked("admin"));
    assert!(limiter.admit("admin").is_ok());
}

#[test]
fn only_the_first_failure_and_the_lock_are_for_the_audit() {
    let clock = clock();
    let limiter = limiter(4, 900, 2, &clock);
    assert_eq!(limiter.failed("admin"), Failure::First);
    assert_eq!(limiter.failed("admin"), Failure::Counted);
    assert_eq!(limiter.failed("admin"), Failure::Counted);
    assert_eq!(limiter.failed("admin"), Failure::Locked);
}

#[test]
fn a_limit_of_one_locks_at_the_first_failure_and_a_limit_of_zero_means_one() {
    for max in [1, 0] {
        let clock = clock();
        let limiter = limiter(max, 900, 2, &clock);
        assert_eq!(limiter.failed("admin"), Failure::Locked);
        assert!(limiter.is_locked("admin"));
    }
}

#[test]
fn a_success_clears_the_count_and_the_lock() {
    let clock = clock();
    let limiter = limiter(3, 900, 2, &clock);
    let _ = limiter.failed("admin");
    let _ = limiter.failed("admin");
    limiter.succeeded("admin");
    assert_eq!(limiter.tracked(), 0);
    assert_eq!(
        limiter.failed("admin"),
        Failure::First,
        "the run starts over"
    );
    assert_eq!(limiter.failed("admin"), Failure::Counted);
    assert_eq!(limiter.failed("admin"), Failure::Locked);
    limiter.succeeded("admin");
    assert!(!limiter.is_locked("admin"));
    assert!(limiter.admit("admin").is_ok());
    limiter.succeeded("nobody");
}

#[test]
fn a_lock_is_of_one_name_alone() {
    let clock = clock();
    let limiter = limiter(2, 900, 2, &clock);
    let _ = limiter.failed("ann");
    assert_eq!(limiter.failed("ann"), Failure::Locked);
    assert_eq!(limiter.failed("bob"), Failure::First);
    assert!(limiter.is_locked("ann"));
    assert!(!limiter.is_locked("bob"));
    assert!(limiter.admit("bob").is_ok());
}

#[test]
fn a_name_nobody_owns_is_counted_and_locked_like_any_other() {
    let clock = clock();
    let limiter = limiter(2, 900, 2, &clock);
    let _ = limiter.failed("no-such-user");
    assert_eq!(limiter.failed("no-such-user"), Failure::Locked);
    assert_eq!(
        limiter.admit("no-such-user").unwrap_err(),
        Refusal::Locked,
        "a lock does not say which names are users"
    );
}

#[test]
fn a_failure_that_lands_during_a_lock_does_not_move_it() {
    let clock = clock();
    let limiter = limiter(2, 900, 2, &clock);
    let _ = limiter.failed("admin");
    assert_eq!(limiter.failed("admin"), Failure::Locked);
    set(&clock, START + 500);
    assert_eq!(
        limiter.failed("admin"),
        Failure::Counted,
        "a hash that was already running"
    );
    set(&clock, START + 900);
    assert!(
        !limiter.is_locked("admin"),
        "the lock still ends at its own time"
    );
    assert_eq!(
        limiter.failed("admin"),
        Failure::First,
        "after a lock the run starts over"
    );
}

#[test]
fn a_run_of_failures_is_forgotten_a_lockout_length_after_its_last() {
    let clock = clock();
    let limiter = limiter(3, 900, 2, &clock);
    let _ = limiter.failed("ann");
    let _ = limiter.failed("ann");
    set(&clock, START + 899);
    assert_eq!(limiter.failed("ann"), Failure::Locked, "still one run");
    set(&clock, START + 2000);
    let _ = limiter.failed("bob");
    assert_eq!(
        limiter.tracked(),
        1,
        "ann's lock is over and her count is gone"
    );
    let _ = limiter.failed("carol");
    set(&clock, START + 2000 + 900);
    let _ = limiter.failed("dave");
    assert_eq!(limiter.tracked(), 1, "bob and carol are forgotten too");
}

#[test]
fn a_lock_is_kept_while_it_lasts_however_old_its_last_failure() {
    let clock = clock();
    let limiter = limiter(1, 5000, 2, &clock);
    let _ = limiter.failed("ann");
    set(&clock, START + 4000);
    let _ = limiter.failed("bob");
    assert!(
        limiter.is_locked("ann"),
        "past a lockout of 900 it would have been dropped"
    );
    assert_eq!(limiter.tracked(), 2);
}

#[test]
fn a_clock_that_steps_back_does_not_end_a_lock_early_or_forget_a_run() {
    let clock = clock();
    let limiter = limiter(2, 900, 2, &clock);
    let _ = limiter.failed("ann");
    assert_eq!(limiter.failed("ann"), Failure::Locked);
    set(&clock, START - 1000);
    assert!(limiter.is_locked("ann"));
    let _ = limiter.failed("bob");
    assert!(limiter.is_locked("ann"));
    assert_eq!(limiter.tracked(), 2);
}

#[test]
fn a_free_hash_slot_is_given_and_a_full_cap_is_busy_until_one_is_given_back() {
    let clock = clock();
    let limiter = limiter(5, 900, 2, &clock);
    let one = limiter.admit("ann").unwrap();
    let two = limiter.admit("bob").unwrap();
    assert_eq!(limiter.admit("carol").unwrap_err(), Refusal::Busy);
    drop(one);
    let three = limiter.admit("carol").unwrap();
    assert_eq!(limiter.admit("dave").unwrap_err(), Refusal::Busy);
    drop((two, three));
    assert!(limiter.admit("dave").is_ok());
}

#[test]
fn a_locked_name_takes_no_hash_slot() {
    let clock = clock();
    let limiter = limiter(1, 900, 1, &clock);
    let _ = limiter.failed("ann");
    assert_eq!(limiter.admit("ann").unwrap_err(), Refusal::Locked);
    assert!(
        limiter.admit("bob").is_ok(),
        "the only slot was never taken"
    );
}

#[test]
fn the_refusals_and_the_limiter_say_no_name_and_no_count() {
    assert_eq!(Refusal::Locked.to_string(), "the user is locked");
    assert_eq!(Refusal::Busy.to_string(), "no password hash slot is free");
    let clock = clock();
    let limiter = limiter(5, 900, 2, &clock);
    let _ = limiter.failed("ann");
    assert_eq!(
        format!("{limiter:?}"),
        "LoginLimiter { settings: LimiterSettings { max_failures: 5, lockout_secs: 900, max_tracked: 4096 }, .. }"
    );
}

fn tracking(max_tracked: usize, clock: &Arc<Clock>) -> LoginLimiter {
    LoginLimiter::new(
        LimiterSettings {
            max_failures: 2,
            lockout_secs: 900,
            max_tracked,
        },
        clock.clone(),
        HashSlots::new(NonZeroUsize::new(1).unwrap()),
    )
}

#[test]
fn past_the_tracking_cap_the_oldest_unlocked_name_is_dropped() {
    let clock = clock();
    let limiter = tracking(2, &clock);
    let _ = limiter.failed("a");
    set(&clock, START + 1);
    let _ = limiter.failed("b");
    set(&clock, START + 2);
    let _ = limiter.failed("c");
    assert_eq!(limiter.tracked(), 2);
    assert_eq!(limiter.failed("a"), Failure::First, "a was dropped");
    assert_eq!(limiter.tracked(), 2);
}

#[test]
fn a_locked_name_outlasts_a_flood_of_unlocked_ones() {
    let clock = clock();
    let limiter = tracking(2, &clock);
    let _ = limiter.failed("a");
    assert_eq!(limiter.failed("a"), Failure::Locked);
    for (i, name) in ["b", "c", "d", "e"].into_iter().enumerate() {
        set(&clock, START + 1 + u64::try_from(i).unwrap());
        let _ = limiter.failed(name);
    }
    assert_eq!(limiter.tracked(), 2);
    assert!(limiter.is_locked("a"));
}

#[test]
fn when_every_tracked_name_is_locked_the_oldest_is_dropped_for_a_new_one() {
    let clock = clock();
    let limiter = tracking(2, &clock);
    for (i, name) in ["a", "b"].into_iter().enumerate() {
        set(&clock, START + u64::try_from(i).unwrap());
        let _ = limiter.failed(name);
        assert_eq!(limiter.failed(name), Failure::Locked);
    }
    set(&clock, START + 5);
    let _ = limiter.failed("c");
    assert_eq!(limiter.tracked(), 2);
    assert!(!limiter.is_locked("a"));
    assert!(limiter.is_locked("b"));
}

#[test]
fn a_tracking_cap_of_zero_holds_one_name() {
    let clock = clock();
    let limiter = tracking(0, &clock);
    let _ = limiter.failed("a");
    let _ = limiter.failed("b");
    assert_eq!(limiter.tracked(), 1);
}

#[test]
fn a_session_that_chose_a_password_no_longer_must_change_it() {
    let clock = clock();
    let sessions = sessions(SessionSettings::default(), &clock);
    let (id, view) = sessions.create("admin", true, Some(START + 3600)).unwrap();
    assert!(view.must_change);
    let cleared = sessions.clear_must_change(id.as_str()).unwrap();
    assert_eq!(cleared.username, "admin");
    assert!(!cleared.must_change);
    assert_eq!(cleared.must_change_until, None);
    assert_eq!(cleared.csrf(), view.csrf(), "the session is the same one");
    assert_eq!(sessions.lookup(id.as_str()), Some(cleared));
}

#[test]
fn clearing_must_change_of_an_unknown_or_ended_or_expired_session_finds_none() {
    let clock = clock();
    let sessions = sessions(settings(100, 1000, 8), &clock);
    assert!(sessions.clear_must_change("nobody").is_none());
    let (ended, _) = sessions.create("admin", true, None).unwrap();
    assert!(sessions.end(ended.as_str()));
    assert!(sessions.clear_must_change(ended.as_str()).is_none());
    let (idle, _) = sessions.create("admin", true, None).unwrap();
    set(&clock, START + 100);
    assert!(sessions.clear_must_change(idle.as_str()).is_none());
}
