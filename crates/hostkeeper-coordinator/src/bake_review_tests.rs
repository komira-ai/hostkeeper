//! Bake and canary, second pass: what a gap in the heartbeats does to a bake, what a silent
//! or pending host holds, and what keeps the other hosts waiting while a canary goes.

use std::time::Duration;

use hostkeeper_wire::wire::{DeferredReport, Token};

use crate::bake_canary_tests::{
    all, beats, definition, gate, go, one_daemon_at_a_time, secs, services, skip, why, why_under,
};
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

/// An unhealthy host at the desired version has not baked: it is no longer counted up, and the
/// next host waits on it. It is the stage that waits, not a slot: with the default plan the
/// second host says `stage_wait` while the host is not up and `baking` once it beats healthy;
/// with one stage and one daemon at a time the unhealthy host holds no update slot, so the
/// second host is not held by `max_updating`.
#[test]
fn an_unhealthy_host_at_the_desired_version_holds_baking() {
    let s = services("bake_beats = 3");
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    beats(&mut f, "a", &unhealthy(beat("2.0")), 100);
    beats(&mut f, "b", &beat("1.0"), 100);
    assert_eq!(why(&s, &f, "b", 100), gate(Gate::StageWait));
    let one_stage = services("bake_beats = 3\nstages = [100]");
    let one = one_daemon_at_a_time();
    assert_eq!(why_under(&one, &one_stage, &f, "b", 100), go());

    beats(&mut f, "a", &beat("2.0"), 115);
    beats(&mut f, "b", &beat("1.0"), 115);
    assert_eq!(why(&s, &f, "b", 115), gate(Gate::Baking));
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
    let one = one_daemon_at_a_time();
    assert_eq!(why_under(&one, &s, &f, "b", 1_000), gate(Gate::NoSlot));
    assert_eq!(
        why(&s, &f, "b", 1_000),
        go(),
        "the default for the daemon role is two at a time"
    );
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

/// A service that cannot start its rollout yet holds every host of it: another service has an
/// update live (a host of it updating), and every host of the canary service says
/// `rollout_elsewhere`, whichever would be first to ask. When the other update ends, the first
/// host to ask goes and the rest wait for the canary.
#[test]
fn a_service_that_cannot_start_yet_holds_all_its_hosts_on_rollout_elsewhere() {
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
    for host in ["a", "b", "c"] {
        assert_eq!(
            why(&s, &f, host, 100),
            gate(Gate::RolloutElsewhere),
            "{host}"
        );
    }
    f.record_in("other", id("x"), role("daemon"), beat("1.0"), secs(115));
    for host in ["a", "b", "c"] {
        beats(&mut f, host, &beat("1.0"), 115);
    }
    assert_eq!(why(&s, &f, "a", 115), go());
    f.order(LEGACY, id("a"), version("2.0"), secs(115));
    assert_eq!(why(&s, &f, "b", 115), gate(Gate::StageWait));
    assert_eq!(why(&s, &f, "c", 115), gate(Gate::StageWait));
}

/// A host that declined the update takes no place in the canary stage while its wait lasts:
/// the next host to ask is the canary, the deferring host says `deferred`, and when the wait
/// is over it is the first candidate again.
#[test]
fn a_deferring_host_does_not_take_the_canary_place() {
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
    f.order(LEGACY, id("b"), version("2.0"), secs(100));
    assert_eq!(why(&s, &f, "c", 100), gate(Gate::StageWait));
    let mut again = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    again.record_in(LEGACY, id("a"), role("daemon"), declined, secs(100));
    for host in ["a", "b", "c"] {
        beats(&mut again, host, &beat("1.0"), 5_000);
    }
    assert_eq!(why(&s, &again, "a", 5_000), go());
    again.order(LEGACY, id("a"), version("2.0"), secs(5_000));
    assert_eq!(why(&s, &again, "b", 5_000), gate(Gate::StageWait));
}
