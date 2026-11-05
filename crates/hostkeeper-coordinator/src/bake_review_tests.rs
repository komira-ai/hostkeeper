//! Bake and canary, second pass: what a gap in the heartbeats does to a bake, what a silent
//! or pending host holds, and what keeps the canary gate closed.

use std::time::Duration;

use hostkeeper_wire::wire::{DeferredReport, Token};

use crate::bake_canary_tests::{all, beats, definition, gate, go, secs, services, skip, why};
use crate::decide::{Decision, Gate, Skip, decide_why};
use crate::fleet::Fleet;
use crate::services::Services;
use crate::testutil::{LEGACY, beat, desired, id, role, unhealthy, updating, version};

/// A host that was silent for the down limit or more has not proven what it ran meanwhile:
/// when it comes back its count starts again, so a flapping host never finishes baking on
/// beats separated by gaps. Two beats, a gap of 85 s, then it needs three more.
#[test]
fn a_host_silent_past_the_down_limit_restarts_its_bake() {
    let s = services("bake_beats = 3");
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    beats(&mut f, "a", &beat("2.0"), 100);
    beats(&mut f, "a", &beat("2.0"), 115);
    for (at, expect) in [
        (200, gate(Gate::Baking)),
        (215, gate(Gate::Baking)),
        (230, go()),
    ] {
        beats(&mut f, "a", &beat("2.0"), at);
        beats(&mut f, "b", &beat("1.0"), at);
        assert_eq!(why(&s, &f, "b", at), expect, "at {at}");
    }
}

/// The gap that restarts a bake is the liveness limit itself: a beat 44 s after the last one
/// keeps the count, one 45 s after it starts the count again.
#[test]
fn a_gap_of_the_down_limit_restarts_the_bake_and_one_second_less_does_not() {
    let s = services("bake_beats = 2");
    for (gap, expect) in [(44, go()), (45, gate(Gate::Baking))] {
        let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
        beats(&mut f, "a", &beat("2.0"), 100);
        let at = 100 + gap;
        beats(&mut f, "a", &beat("2.0"), at);
        beats(&mut f, "b", &beat("1.0"), at);
        assert_eq!(why(&s, &f, "b", at), expect, "gap {gap}");
    }
}

/// An unhealthy host at the desired version holds the slot as a baking host: it is no longer
/// counted up, but it has not baked, and the next host waits on it.
#[test]
fn an_unhealthy_host_at_the_desired_version_holds_baking() {
    let s = services("bake_beats = 3");
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    beats(&mut f, "a", &unhealthy(beat("2.0")), 100);
    beats(&mut f, "b", &beat("1.0"), 100);
    assert_eq!(why(&s, &f, "b", 100), gate(Gate::Baking));
}

/// A host whose promotion is pending holds its slot also when it goes silent, as an updating
/// host and a failed one do: the coordinator has no bound of its own on a promotion, and a
/// dead host mid-promotion keeps the slot until it is fixed.
#[test]
fn a_silent_pending_host_holds_its_slot() {
    let s = services("");
    let mut pending = beat("2.0");
    pending.promotion_pending = true;
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    beats(&mut f, "a", &pending, 100);
    beats(&mut f, "b", &beat("1.0"), 1_000);
    assert_eq!(why(&s, &f, "b", 1_000), gate(Gate::NoSlot));
}

/// A pending promotion is for the version the host runs: a new desired version is ordered to
/// the host at its next beat, and the version it has is told nothing.
#[test]
fn a_pending_host_is_ordered_a_new_desired_version() {
    let s = services("");
    let mut pending = beat("2.0");
    pending.promotion_pending = true;
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    beats(&mut f, "a", &pending, 100);
    let ask = |want: &str| {
        decide_why(
            &s,
            &all(&desired(want, 0, 0)),
            LEGACY,
            &f,
            &id("a"),
            secs(100),
        )
    };
    assert_eq!(ask("2.0"), skip(Skip::UpToDate));
    assert_eq!(
        ask("3.0"),
        Decision::Update {
            version: version("3.0")
        }
    );
}

/// A canary count over the cap is refused, with the field named.
#[test]
fn a_canary_count_over_the_cap_is_refused() {
    let err = Services::parse(&definition("canary = { count = 10001 }"))
        .expect_err("refused")
        .to_string();
    assert!(err.contains("canary count is not 1 to 10000"), "{err}");
}

/// Intended: a canary that cannot go yet does not let anyone past it. The host chosen as the
/// canary is held by the one live update, here a host of another service updating, and every
/// other host says `canary_wait` meanwhile: the gate never opens for them because the canary is
/// behind a rule.
#[test]
fn a_canary_that_cannot_go_yet_holds_everyone_else_on_canary_wait() {
    let text = format!(
        "{}[[service]]\nname = \"other\"\nroles = [\"daemon\"]\n",
        definition("canary = { count = 1 }")
    );
    let s = Services::parse(&text).expect("services");
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    for host in ["a", "b", "c"] {
        beats(&mut f, host, &beat("1.0"), 100);
    }
    f.record_in(
        "other",
        id("x"),
        role("daemon"),
        updating(beat("1.0"), "2.0"),
        secs(100),
    );
    assert_eq!(why(&s, &f, "a", 100), gate(Gate::NoSlot));
    assert_eq!(why(&s, &f, "b", 100), gate(Gate::CanaryWait));
    assert_eq!(why(&s, &f, "c", 100), gate(Gate::CanaryWait));
}

/// A host that declined the update is no canary while its wait lasts: the next host is the
/// canary, the deferring host says `deferred`, and when the wait is over it is the first
/// candidate again.
#[test]
fn a_deferring_host_is_not_chosen_as_the_canary() {
    let s = services("canary = { count = 1 }");
    let mut declined = beat("1.0");
    declined.deferred = Some(DeferredReport {
        target: version("2.0"),
        reason: Token::parse("drain_bound").expect("a token"),
        retry_after_secs: 300,
    });
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    beats(&mut f, "a", &declined, 100);
    for host in ["b", "c"] {
        beats(&mut f, host, &beat("1.0"), 100);
    }
    assert_eq!(why(&s, &f, "a", 100), skip(Skip::Deferred));
    assert_eq!(why(&s, &f, "b", 100), go());
    assert_eq!(why(&s, &f, "c", 100), gate(Gate::CanaryWait));
    for host in ["a", "b", "c"] {
        beats(&mut f, host, &beat("1.0"), 5_000);
    }
    assert_eq!(why(&s, &f, "a", 5_000), go());
    assert_eq!(why(&s, &f, "b", 5_000), gate(Gate::CanaryWait));
}
