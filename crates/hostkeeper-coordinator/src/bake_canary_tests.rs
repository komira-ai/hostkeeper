//! `bake_beats` and `canary`: a host that has updated holds its slot until it has beaten
//! healthy enough times, `promotion_pending` holds the slot and is not up, and a canary
//! gate lets the first hosts go alone. Every time is explicit; the fleets are built beat by
//! beat.

use std::collections::BTreeMap;
use std::time::Duration;

use hostkeeper_wire::wire::Reply;

use crate::converge::{State, converge_service};
use crate::decide::{Decision, Gate, Skip, decide_in, decide_why};
use crate::desired::Desired;
use crate::fleet::Fleet;
use crate::identity::Identity;
use crate::services::Services;
use crate::testutil::{LEGACY, Rig, beat, desired, id, last, role, unhealthy, updating, version};

pub(crate) fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

/// `kbf` replaced by a service of daemons with the definition lines `extra` (`bake_beats`,
/// `canary`) in its table.
pub(crate) fn definition(extra: &str) -> String {
    format!("[[service]]\nname = \"kbf\"\n{extra}\nroles = [\"daemon\"]\n")
}

pub(crate) fn services(extra: &str) -> Services {
    Services::parse(&definition(extra)).expect("services")
}

pub(crate) fn all(want: &Desired) -> BTreeMap<String, Desired> {
    BTreeMap::from([(LEGACY.to_owned(), want.clone())])
}

pub(crate) fn why(s: &Services, f: &Fleet, host: &str, now: u64) -> Decision {
    let want = desired("2.0", 0, 0);
    let decision = decide_why(s, &all(&want), LEGACY, f, &id(host), secs(now));
    let reply = decide_in(s, &all(&want), LEGACY, f, &id(host), secs(now));
    match &decision {
        Decision::Update { version } => assert_eq!(
            reply,
            Reply::Update {
                version: version.clone()
            }
        ),
        Decision::Install { .. }
        | Decision::Uninstall { .. }
        | Decision::Gate(_)
        | Decision::Skip(_) => assert_eq!(reply, Reply::Nothing),
    }
    decision
}

pub(crate) fn go() -> Decision {
    Decision::Update {
        version: version("2.0"),
    }
}

pub(crate) fn gate(why: Gate) -> Decision {
    Decision::Gate(why)
}

pub(crate) fn skip(why: Skip) -> Decision {
    Decision::Skip(why)
}

pub(crate) fn beats(f: &mut Fleet, host: &str, b: &hostkeeper_wire::wire::Heartbeat, at: u64) {
    f.record_in(LEGACY, id(host), role("daemon"), b.clone(), secs(at));
}

/// With `bake_beats` absent, and with it zero, a host at the desired version holds no slot:
/// the next host is ordered at once, as before, and the definition is the same one.
#[test]
fn bake_zero_changes_nothing() {
    assert_eq!(
        services("bake_beats = 0"),
        services(""),
        "zero is the default"
    );
    for extra in ["", "bake_beats = 0"] {
        let s = services(extra);
        let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
        beats(&mut f, "a", &beat("2.0"), 100);
        beats(&mut f, "b", &beat("1.0"), 100);
        assert_eq!(why(&s, &f, "b", 100), go());
    }
}

/// With `bake_beats = 3` and one slot, the host that updated holds the slot through its
/// first two healthy beats at the new version and frees it at the third.
#[test]
fn an_updated_host_holds_its_slot_until_it_has_beaten_healthy_bake_beats_times() {
    let s = services("bake_beats = 3");
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    beats(&mut f, "b", &beat("1.0"), 100);
    for (n, at) in [100, 115, 130].into_iter().enumerate() {
        beats(&mut f, "a", &beat("2.0"), at);
        beats(&mut f, "b", &beat("1.0"), at);
        let expect = if n < 2 { gate(Gate::Baking) } else { go() };
        assert_eq!(why(&s, &f, "b", at), expect, "after beat {}", n + 1);
    }
}

/// An unhealthy beat, an updating beat or a pending promotion each start the count again:
/// two healthy beats before and two after are not three in a row.
#[test]
fn an_unhealthy_or_updating_beat_resets_the_bake() {
    let mut pending = beat("2.0");
    pending.promotion_pending = true;
    let breaks = [
        unhealthy(beat("2.0")),
        updating(beat("2.0"), "2.0"),
        pending,
    ];
    for broken in breaks {
        let s = services("bake_beats = 3");
        let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
        let mut at = 100;
        for b in [beat("2.0"), beat("2.0"), broken, beat("2.0"), beat("2.0")] {
            beats(&mut f, "a", &b, at);
            at += 15;
        }
        beats(&mut f, "b", &beat("1.0"), at);
        assert_eq!(why(&s, &f, "b", at), gate(Gate::Baking), "two beats since");
        beats(&mut f, "a", &beat("2.0"), at);
        assert_eq!(why(&s, &f, "b", at), go(), "three beats since");
    }
}

/// A baking host that is silent for the down limit does not hold its slot: the fleet
/// is not stalled by a host that cannot finish baking.
#[test]
fn a_baking_host_that_went_silent_frees_its_slot() {
    let s = services("bake_beats = 3");
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    beats(&mut f, "a", &beat("2.0"), 100);
    beats(&mut f, "b", &beat("1.0"), 100);
    assert_eq!(why(&s, &f, "b", 100), gate(Gate::Baking));
    beats(&mut f, "b", &beat("1.0"), 145);
    assert_eq!(why(&s, &f, "b", 145), go());
}

/// A slot held by a host that is updating is a slot held, not a bake: the reason says so.
#[test]
fn a_slot_held_by_an_update_is_no_slot_not_baking() {
    let s = services("bake_beats = 3");
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    beats(&mut f, "a", &updating(beat("1.0"), "2.0"), 100);
    beats(&mut f, "b", &beat("1.0"), 100);
    assert_eq!(why(&s, &f, "b", 100), gate(Gate::NoSlot));
}

/// A pending promotion holds the slot, and the host is not up: the next host waits while it
/// is pending and goes when it clears.
#[test]
fn promotion_pending_holds_the_slot_and_is_not_up_until_it_clears() {
    let mut pending = beat("2.0");
    pending.promotion_pending = true;

    let slot = services("");
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    beats(&mut f, "a", &pending, 100);
    beats(&mut f, "b", &beat("1.0"), 100);
    assert_eq!(why(&slot, &f, "b", 100), gate(Gate::NoSlot));
    let a = f.get_in(LEGACY, &id("a")).expect("a");
    assert!(!f.is_up(LEGACY, &id("a"), a, secs(100)), "a is not up");
    beats(&mut f, "a", &beat("2.0"), 115);
    beats(&mut f, "b", &beat("1.0"), 115);
    assert_eq!(why(&slot, &f, "b", 115), go());
}

/// A pending promotion, or a bake not yet complete, keeps the service from converging.
#[test]
fn a_pending_or_baking_host_keeps_the_service_in_progress() {
    let s = services("bake_beats = 2");
    let def = s.get(LEGACY).expect("kbf");
    let want = desired("2.0", 0, 0);
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    beats(&mut f, "a", &beat("2.0"), 100);
    let state = |f: &Fleet, at| converge_service(def, Some(&want), f, secs(at)).state;
    assert_eq!(state(&f, 100), State::InProgress, "one beat of two");
    beats(&mut f, "a", &beat("2.0"), 115);
    assert_eq!(state(&f, 115), State::Converged);
    let mut pending = beat("2.0");
    pending.promotion_pending = true;
    beats(&mut f, "a", &pending, 130);
    assert_eq!(state(&f, 130), State::InProgress, "pending");
}

pub(crate) fn started(f: &mut Fleet, at: u64) {
    for host in ["a", "b", "c"] {
        beats(f, host, &beat("1.0"), at);
    }
}

/// With a canary of one, the host with the smallest id alone is ordered. It is not
/// followed until it has baked: not while ordered, updating, or baking; when its second
/// healthy beat comes the others go.
#[test]
fn with_a_canary_of_one_nobody_else_is_ordered_until_it_has_baked() {
    let s = services("bake_beats = 2\ncanary = { count = 1 }");
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    started(&mut f, 100);
    assert_eq!(why(&s, &f, "a", 100), go());
    assert_eq!(why(&s, &f, "b", 100), gate(Gate::CanaryWait));
    assert_eq!(why(&s, &f, "c", 100), gate(Gate::CanaryWait));
    f.order(LEGACY, id("a"), version("2.0"), secs(100));
    assert_eq!(
        why(&s, &f, "a", 100),
        go(),
        "its own order does not hold it"
    );
    assert_eq!(why(&s, &f, "b", 100), gate(Gate::CanaryWait), "ordered");
    beats(&mut f, "a", &updating(beat("1.0"), "2.0"), 115);
    started_others(&mut f, 115);
    assert_eq!(why(&s, &f, "b", 115), gate(Gate::CanaryWait), "updating");
    beats(&mut f, "a", &beat("2.0"), 130);
    started_others(&mut f, 130);
    assert_eq!(why(&s, &f, "b", 130), gate(Gate::CanaryWait), "one beat");
    beats(&mut f, "a", &beat("2.0"), 145);
    started_others(&mut f, 145);
    assert_eq!(why(&s, &f, "b", 145), go(), "baked");
    assert_eq!(why(&s, &f, "c", 145), go(), "baked");
}

pub(crate) fn started_others(f: &mut Fleet, at: u64) {
    for host in ["b", "c"] {
        beats(f, host, &beat("1.0"), at);
    }
}

/// A canary at the desired version whose promotion is pending has not baked: the gate stays
/// closed, whatever the bake count says.
#[test]
fn a_canary_with_a_pending_promotion_keeps_the_gate_closed() {
    let s = services("canary = { count = 1 }");
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    started(&mut f, 100);
    let mut pending = beat("2.0");
    pending.promotion_pending = true;
    beats(&mut f, "a", &pending, 115);
    started_others(&mut f, 115);
    assert_eq!(why(&s, &f, "b", 115), gate(Gate::CanaryWait));
    beats(&mut f, "a", &beat("2.0"), 130);
    started_others(&mut f, 130);
    assert_eq!(why(&s, &f, "b", 130), go());
}

/// A canary of two lets two hosts go; the third waits until both have baked.
#[test]
fn a_canary_of_two_orders_two_hosts() {
    let s = services("canary = { count = 2 }");
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    started(&mut f, 100);
    assert_eq!(why(&s, &f, "a", 100), go());
    assert_eq!(why(&s, &f, "b", 100), go());
    assert_eq!(why(&s, &f, "c", 100), gate(Gate::CanaryWait));
    beats(&mut f, "a", &beat("2.0"), 115);
    started_others(&mut f, 115);
    assert_eq!(why(&s, &f, "c", 115), gate(Gate::CanaryWait), "one baked");
    beats(&mut f, "b", &beat("2.0"), 115);
    assert_eq!(why(&s, &f, "c", 115), go(), "two baked");
}

/// A canary that failed keeps its place, so nobody else is ordered and the service reads
/// as blocked.
#[test]
fn a_failed_canary_blocks_the_service_and_orders_nobody_else() {
    let s = services("canary = { count = 1 }");
    let def = s.get(LEGACY).expect("kbf");
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    started(&mut f, 100);
    let failed = last(beat("1.0"), "2.0", hostkeeper_wire::wire::Outcome::Failed);
    beats(&mut f, "a", &failed, 115);
    started_others(&mut f, 115);
    assert_eq!(why(&s, &f, "b", 115), gate(Gate::CanaryWait));
    assert_eq!(why(&s, &f, "c", 115), gate(Gate::CanaryWait));
    let want = desired("2.0", 0, 0);
    let state = converge_service(def, Some(&want), &f, secs(115)).state;
    assert_eq!(state, State::Blocked);
}

/// A canary that went silent before it was ever ordered is not a canary: the next host is.
#[test]
fn a_canary_that_is_down_and_never_started_is_replaced_by_the_next_host() {
    let s = services("canary = { count = 1 }");
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    started(&mut f, 100);
    beats(&mut f, "b", &beat("1.0"), 200);
    beats(&mut f, "c", &beat("1.0"), 200);
    assert_eq!(why(&s, &f, "b", 200), go());
    assert_eq!(why(&s, &f, "c", 200), gate(Gate::CanaryWait));
}

/// A canary that started and then went silent still counts as started: no one takes its
/// place.
#[test]
fn one_that_started_and_went_down_still_counts() {
    let s = services("canary = { count = 1 }");
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    beats(&mut f, "a", &updating(beat("1.0"), "2.0"), 100);
    beats(&mut f, "b", &beat("1.0"), 200);
    beats(&mut f, "c", &beat("1.0"), 200);
    assert_eq!(why(&s, &f, "b", 200), gate(Gate::CanaryWait));
    assert_eq!(why(&s, &f, "c", 200), gate(Gate::CanaryWait));
}

/// A canary is chosen by node id whatever the roles: the smallest id alone is ordered, in any
/// role, and a host of another role waits for the canary to bake like every other.
#[test]
fn a_canary_is_chosen_by_node_id_whatever_the_role() {
    let text = "[[service]]\nname = \"kbf\"\ncanary = { count = 1 }\nroles = [\"frontend\", \"storage\"]\n";
    let s = Services::parse(text).expect("services");
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    f.record_in(LEGACY, id("a1"), role("frontend"), beat("1.0"), secs(100));
    f.record_in(LEGACY, id("s1"), role("storage"), beat("1.0"), secs(100));
    assert_eq!(why(&s, &f, "a1", 100), go());
    assert_eq!(why(&s, &f, "s1", 100), gate(Gate::CanaryWait));
}

/// A restart forgets the count: a host that had baked two of three beats starts again at
/// one, and the next host waits until it has beaten three times since the restart.
#[tokio::test]
async fn a_restart_mid_bake_starts_the_count_again() {
    let mut rig = Rig::new("bake-restart");
    let s = || services("bake_beats = 3");
    rig.coordinator = Rig::start(&rig.dir, &rig.clock).with_services(s());
    rig.set(r#"{"version":"2.0","min_up":{"storage":0,"frontend":0}}"#)
        .await;
    let who = |name: &str| Identity {
        host_id: id(name),
        role: role("daemon"),
    };
    rig.at(2_000);
    for at in [2_000, 2_015] {
        rig.at(at);
        rig.coordinator
            .heartbeat(who("a"), beat("2.0"))
            .expect("a heartbeat");
    }
    let asked = |rig: &Rig| {
        rig.coordinator
            .heartbeat(who("b"), beat("1.0"))
            .expect("a heartbeat")
    };
    assert_eq!(asked(&rig), Reply::Nothing, "two of three");
    rig.at(3_000);
    rig.coordinator = Rig::start(&rig.dir, &rig.clock).with_services(s());
    rig.at(3_031);
    rig.coordinator
        .heartbeat(who("a"), beat("2.0"))
        .expect("a heartbeat");
    assert_eq!(
        asked(&rig),
        Reply::Nothing,
        "one of three since the restart"
    );
    for at in [3_046, 3_061] {
        rig.at(at);
        rig.coordinator
            .heartbeat(who("a"), beat("2.0"))
            .expect("a heartbeat");
    }
    assert_eq!(
        asked(&rig),
        Reply::Update {
            version: version("2.0")
        }
    );
}

/// The fields are read from a definition and refused where they are not usable: over the
/// cap, a canary of none, and the roles and labels form of the canary (a later change).
#[test]
fn the_definition_reads_bake_and_canary_and_refuses_the_rest() {
    let s = services("bake_beats = 100\ncanary = { count = 2 }");
    let kbf = s.get(LEGACY).expect("kbf");
    assert_eq!(kbf.bake_beats, 100);
    assert_eq!(kbf.canary.map(|c| c.count), Some(2));
    let refused = |extra: &str| {
        Services::parse(&definition(extra))
            .expect_err("refused")
            .to_string()
    };
    assert!(refused("bake_beats = 101").contains("bake_beats"));
    assert!(refused("bake_beats = 101").contains("100"));
    assert!(refused("canary = { count = 0 }").contains("canary"));
    assert!(refused("canary = { roles = [\"daemon\"] }").contains("unknown field `roles`"));
    assert!(refused("canary = { labels = { a = \"b\" } }").contains("unknown field `labels`"));
}
