//! One rollout live at a time across services (`stages.rs`, `elsewhere`, `--live-rollouts`
//! default 1): when a slot one service holds is released, as another service sees it. Each
//! holder keeps its service's rollout live until its own service's rule releases it (its
//! desired version, its `bake_beats`, the order's age, a deferral, a healthy beat), and holders
//! of several services and hosts release the fleet only when the last of them does; until then
//! the services that are not live are held with `rollout_elsewhere`. A host takes part in a plan
//! only while it is live, so the asking host beats before each question. Every time is
//! explicit; the fleets are built beat by beat.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use hostkeeper_wire::wire::{DeferredReport, Health, Heartbeat, Outcome, Token};

use crate::decide::{Decision, Gate, decide_why};
use crate::desired::Desired;
use crate::fleet::{Fleet, Marks};
use crate::services::Services;
use crate::testutil::{beat, id, last, role, secs, unhealthy, updating, version};

/// `a` bakes for two beats and wants 2.0; `b` bakes for three and wants 5.0; `c` does not bake
/// and wants 2.0; `third` has no desired state; `broken` wants 2.0 and is the service of hosts
/// that failed it or promote it. All but `third` and `broken` run on daemons and storage hosts,
/// those two on daemons. All have the default plan.
const SERVICES: &str = "[[service]]\nname = \"a\"\nbake_beats = 2\nroles = [\"daemon\", \"storage\"]\n\
     [[service]]\nname = \"b\"\nbake_beats = 3\nroles = [\"daemon\", \"storage\"]\n\
     [[service]]\nname = \"c\"\nroles = [\"daemon\", \"storage\"]\n\
     [[service]]\nname = \"third\"\nroles = [\"daemon\"]\n\
     [[service]]\nname = \"broken\"\nroles = [\"daemon\"]\n";

fn want(target: &str) -> Desired {
    let text = format!(r#"{{"version":"{target}","min_up":{{"storage":0,"frontend":0}}}}"#);
    Desired::parse(text.as_bytes()).expect("parsed")
}

/// The desired states: every service of [`SERVICES`] but `third`.
fn all() -> BTreeMap<String, Desired> {
    [("a", "2.0"), ("b", "5.0"), ("c", "2.0"), ("broken", "2.0")]
        .into_iter()
        .map(|(s, v)| (s.to_owned(), want(v)))
        .collect()
}

struct Farm {
    services: Services,
    all: BTreeMap<String, Desired>,
    fleet: Fleet,
}

impl Farm {
    fn new() -> Self {
        Self {
            services: Services::parse(SERVICES).expect("services"),
            all: all(),
            fleet: Fleet::new(Duration::ZERO, crate::fleet::QUIET_START),
        }
    }

    fn daemon(&mut self, service: &str, host: &str, b: Heartbeat, at: u64) {
        self.fleet
            .record_in(service, id(host), role("daemon"), b, secs(at));
    }

    fn storage(&mut self, service: &str, host: &str, b: Heartbeat, at: u64) {
        self.fleet
            .record_in(service, id(host), role("storage"), b, secs(at));
    }

    fn why(&self, service: &str, host: &str, at: u64) -> Decision {
        decide_why(
            &self.services,
            &self.all,
            service,
            &self.fleet,
            &id(host),
            secs(at),
        )
    }

    /// The decision, noted as an order when it is one, as the coordinator does.
    fn ask(&mut self, service: &str, host: &str, at: u64) -> Decision {
        let decision = self.why(service, host, at);
        if let Decision::Update { version } = &decision {
            self.fleet
                .order(service, id(host), version.clone(), secs(at));
        }
        decision
    }
}

fn go(target: &str) -> Decision {
    Decision::Update {
        version: version(target),
    }
}

fn gate(why: Gate) -> Decision {
    Decision::Gate(why)
}

fn pending(running: &str) -> Heartbeat {
    let mut b = beat(running);
    b.promotion_pending = true;
    b
}

fn failed_unhealthy(target: &str) -> Heartbeat {
    unhealthy(last(beat("1.0"), target, Outcome::Failed))
}

/// Behaviour: a host whose promotion of `b` is pending holds `b`'s rollout live, so `c` on
/// itself and `a` on a storage host are held with `rollout_elsewhere`, and still are after 60 s
/// of silence. Once its beats show the promotion done, it bakes `b` (three beats, the silence
/// restarting the count): both stay held, and both may be ordered after the third healthy beat.
/// Catches a pending promotion counted only for its own service, or only on its own host.
#[test]
fn a_pending_promotion_holds_other_services_on_its_host_and_on_another_role() {
    let elsewhere = gate(Gate::RolloutElsewhere);
    let mut f = Farm::new();
    f.daemon("b", "d1", pending("5.0"), 100);
    f.daemon("c", "d1", beat("1.0"), 100);
    f.storage("a", "s1", beat("1.0"), 100);
    for at in [100, 160] {
        f.daemon("c", "d1", beat("1.0"), at);
        f.storage("a", "s1", beat("1.0"), at);
        assert_eq!(f.why("c", "d1", at), elsewhere, "c on d1 at {at}");
        assert_eq!(f.why("a", "s1", at), elsewhere, "a on s1 at {at}");
    }
    for (beat_no, at) in [(1, 160), (2, 175)] {
        f.daemon("b", "d1", beat("5.0"), at);
        f.daemon("c", "d1", beat("1.0"), at);
        f.storage("a", "s1", beat("1.0"), at);
        assert_eq!(f.why("c", "d1", at), elsewhere, "beat {beat_no}");
        assert_eq!(f.why("a", "s1", at), elsewhere, "beat {beat_no}");
    }
    f.daemon("b", "d1", beat("5.0"), 190);
    f.daemon("c", "d1", beat("1.0"), 190);
    f.storage("a", "s1", beat("1.0"), 190);
    assert_eq!(f.why("c", "d1", 190), go("2.0"));
    assert_eq!(f.why("a", "s1", 190), go("2.0"));
}

/// Behaviour: `d1` updated `b` (5.0, three beats to bake) and `d2` waits for `a` (2.0, two
/// beats). The holder's version and count decide: `d2` waits through two healthy beats and is
/// free after the third; an unhealthy beat in between starts the count again. While `d1` is
/// updating, a new desired version of `b` does not free the slot; once `d1` runs 5.0 and `b`
/// wants 6.0, `d1` is no longer baking and frees it. Catches the asking service's version or
/// `bake_beats` used for another service's bake, and a bake that survives an unhealthy beat.
#[test]
fn a_bake_is_counted_with_its_own_services_version_and_beats() {
    let elsewhere = gate(Gate::RolloutElsewhere);
    let mut f = Farm::new();
    f.daemon("b", "d1", updating(beat("1.0"), "5.0"), 100);
    f.daemon("a", "d2", beat("1.0"), 100);
    f.all.insert("b".to_owned(), want("6.0"));
    assert_eq!(f.why("a", "d2", 100), elsewhere, "updating to 5.0");
    f.all.insert("b".to_owned(), want("5.0"));

    f.daemon("b", "d1", beat("5.0"), 115);
    f.daemon("b", "d1", beat("5.0"), 130);
    assert_eq!(f.why("a", "d2", 130), elsewhere, "two of three");
    f.daemon("b", "d1", unhealthy(beat("5.0")), 145);
    f.daemon("a", "d2", beat("1.0"), 145);
    assert_eq!(f.why("a", "d2", 145), elsewhere, "unhealthy");
    for at in [160, 175] {
        f.daemon("b", "d1", beat("5.0"), at);
        f.daemon("a", "d2", beat("1.0"), at);
        assert_eq!(f.why("a", "d2", at), elsewhere, "again at {at}");
    }
    f.daemon("b", "d1", beat("5.0"), 190);
    f.daemon("a", "d2", beat("1.0"), 190);
    assert_eq!(f.why("a", "d2", 190), go("2.0"), "baked");

    let mut moved = Farm::new();
    moved.daemon("b", "d1", beat("5.0"), 100);
    moved.daemon("a", "d2", beat("1.0"), 100);
    assert_eq!(moved.why("a", "d2", 100), elsewhere);
    moved.all.insert("b".to_owned(), want("6.0"));
    assert_eq!(moved.why("a", "d2", 100), go("2.0"), "b wants 6.0 now");
}

/// Behaviour: an order of `c` to `d1` at 100 s holds `a` on `d2` (`rollout_elsewhere`) at 219 s
/// and frees it at 220 s; `d1` asking again at 160 s is told again without its order being held
/// longer. `d2` beats before each question: a host that is silent is no host of the plan.
/// Catches an order of one service not holding another, and a repeated order that extends the
/// hold.
#[test]
fn an_order_of_one_service_holds_another_until_it_ages_out() {
    let mut f = Farm::new();
    f.daemon("c", "d1", beat("1.0"), 100);
    f.daemon("a", "d2", beat("1.0"), 100);
    assert_eq!(f.ask("c", "d1", 100), go("2.0"));
    f.daemon("c", "d1", beat("1.0"), 160);
    assert_eq!(f.ask("c", "d1", 160), go("2.0"), "told again");
    f.daemon("a", "d2", beat("1.0"), 219);
    assert_eq!(f.why("a", "d2", 219), gate(Gate::RolloutElsewhere));
    f.daemon("a", "d2", beat("1.0"), 220);
    assert_eq!(f.why("a", "d2", 220), go("2.0"));
}

/// Behaviour: `d1` declines the order of `c` (a deferral of its target), which frees the slot
/// at once for `a` on `d2`; a deferral of another version does not. Catches a deferral that
/// keeps holding another service for the order's whole age.
#[test]
fn a_deferral_of_the_ordered_version_frees_the_slot_for_another_service() {
    let deferred = |target: &str| {
        let mut b = beat("1.0");
        b.deferred = Some(DeferredReport {
            target: version(target),
            reason: Token::parse("drain_bound").expect("a token"),
            retry_after_secs: 300,
        });
        b
    };
    let mut f = Farm::new();
    f.daemon("c", "d1", beat("1.0"), 100);
    f.daemon("a", "d2", beat("1.0"), 100);
    assert_eq!(f.ask("c", "d1", 100), go("2.0"));
    f.daemon("c", "d1", deferred("1.5"), 110);
    assert_eq!(
        f.why("a", "d2", 110),
        gate(Gate::RolloutElsewhere),
        "another version"
    );
    f.daemon("c", "d1", deferred("2.0"), 115);
    assert_eq!(f.why("a", "d2", 115), go("2.0"));
}

/// Behaviour: a host silent past the down limit still holds the slot while its last word was
/// updating or failed and unhealthy, and stops holding it while its last word was a bake: a
/// bake needs live beats. The silent baker holds at 144 s and frees at 145 s (45 s after its
/// last beat). The asking host `d2` beats just before each question. Catches liveness applied
/// to an update or a failure, and a silent bake that holds the fleet for ever.
#[test]
fn a_silent_holder_keeps_the_slot_unless_it_was_only_baking() {
    for (holder, b) in [
        ("updating", updating(beat("1.0"), "2.0")),
        ("failed and unhealthy", failed_unhealthy("2.0")),
    ] {
        let mut f = Farm::new();
        f.daemon("c", "d1", b, 100);
        f.daemon("a", "d2", beat("1.0"), 1_000);
        assert_eq!(
            f.why("a", "d2", 1_000),
            gate(Gate::RolloutElsewhere),
            "{holder}"
        );
    }
    let mut f = Farm::new();
    f.daemon("b", "d1", beat("5.0"), 100);
    f.daemon("a", "d2", beat("1.0"), 144);
    assert_eq!(f.why("a", "d2", 144), gate(Gate::RolloutElsewhere));
    f.daemon("a", "d2", beat("1.0"), 145);
    assert_eq!(f.why("a", "d2", 145), go("2.0"));
}

/// Behaviour: holders of services the coordinator does not order keep the rollout live all the
/// same: a host updating `third`, which has no desired state, and a host whose update of
/// `broken` failed and is unhealthy, each hold `c` on another host (`rollout_elsewhere`) until
/// they are done or healthy. A failure that stands keeps `broken`'s own rollout going while a
/// host is left to roll (`stage_failed` for it, `rollout_elsewhere` for `c`) until an operator
/// acknowledges it. Catches the count taken only over services that can be ordered.
#[test]
fn a_service_with_no_desired_state_or_a_failed_host_still_holds_its_slot() {
    let elsewhere = gate(Gate::RolloutElsewhere);
    let mut f = Farm::new();
    f.daemon("third", "d1", updating(beat("1.0"), "2.0"), 100);
    f.daemon("c", "d2", beat("1.0"), 100);
    assert_eq!(f.why("c", "d2", 100), elsewhere, "third updating");
    f.daemon("third", "d1", beat("2.0"), 115);
    assert_eq!(f.why("c", "d2", 115), go("2.0"), "third done");

    let mut f = Farm::new();
    f.daemon("broken", "d1", failed_unhealthy("2.0"), 100);
    f.daemon("c", "d2", beat("1.0"), 100);
    assert_eq!(f.why("c", "d2", 100), elsewhere, "broken failed");
    let mut healthy = failed_unhealthy("2.0");
    healthy.health = Health::Healthy;
    f.daemon("broken", "d1", healthy, 115);
    assert_eq!(f.why("c", "d2", 115), go("2.0"), "broken healthy");

    f.daemon("broken", "d3", beat("1.0"), 115);
    assert_eq!(f.why("c", "d2", 115), elsewhere, "broken has d3 to roll");
    assert_eq!(
        f.why("broken", "d3", 115),
        gate(Gate::StageFailed),
        "its own service's failed host"
    );
    let acked = BTreeSet::from([("d1".to_owned(), "2.0".to_owned())]);
    f.fleet.set_marks(Marks {
        acked: BTreeMap::from([("broken".to_owned(), acked)]),
        ..Marks::default()
    });
    assert_eq!(f.why("c", "d2", 115), go("2.0"), "broken acknowledged");
    assert_eq!(f.why("broken", "d3", 115), go("2.0"), "d3 takes the stage");
}

/// Behaviour: a fleet that a coordinator allowing several rollouts left with several holders,
/// of three services: `a` has `d1` updating and `d3` ordered and then baking, `b` has `s1`
/// failed and unhealthy and `d3` baking, `broken` has `d2` with a promotion pending. `c` on `d4`
/// is held with `rollout_elsewhere` while any holder is left, bakes included, and is ordered
/// only after the last bake ends. Catches a hold that lifts when the first holder or the first
/// host is done, and a host's bake of one service hidden by its slot for another.
#[test]
fn several_holders_release_the_fleet_only_when_the_last_one_does() {
    let elsewhere = gate(Gate::RolloutElsewhere);
    let mut f = Farm::new();
    f.daemon("a", "d1", updating(beat("1.0"), "2.0"), 100);
    f.storage("b", "s1", failed_unhealthy("5.0"), 100);
    f.daemon("broken", "d2", pending("2.0"), 100);
    f.daemon("a", "d3", beat("1.0"), 100);
    f.fleet.order("a", id("d3"), version("2.0"), secs(100));
    f.daemon("b", "d3", beat("5.0"), 100);
    f.daemon("c", "d4", beat("1.0"), 100);
    assert_eq!(f.why("c", "d4", 100), elsewhere, "all four");

    f.daemon("a", "d1", beat("2.0"), 110);
    assert_eq!(f.why("c", "d4", 110), elsewhere, "d1 done");
    let mut healthy = failed_unhealthy("5.0");
    healthy.health = Health::Healthy;
    f.storage("b", "s1", healthy, 110);
    assert_eq!(f.why("c", "d4", 110), elsewhere, "s1 healthy");
    f.daemon("broken", "d2", beat("2.0"), 110);
    assert_eq!(f.why("c", "d4", 110), elsewhere, "d3's order");
    f.daemon("b", "d3", beat("5.0"), 110);
    f.daemon("a", "d3", updating(beat("1.0"), "2.0"), 110);
    assert_eq!(f.why("c", "d4", 110), elsewhere, "d3 updating");
    f.daemon("a", "d3", beat("2.0"), 120);
    assert_eq!(f.why("c", "d4", 120), elsewhere, "only bakes left");

    f.daemon("a", "d1", beat("2.0"), 125);
    f.daemon("a", "d3", beat("2.0"), 125);
    assert_eq!(f.why("c", "d4", 125), elsewhere, "d3 bakes b");
    f.daemon("b", "d3", beat("5.0"), 130);
    assert_eq!(f.why("c", "d4", 130), go("2.0"), "the last bake ended");
}
