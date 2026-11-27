//! A host silent mid-update ([`crate::silent`]): its last word of an update running holds its
//! update slot and its service's rollout place for less than [`SILENT_UPDATE_AFTER`] of silence,
//! and nothing from then on; a failure it last reported holds what it held; the alert that names
//! it; and a host that comes back. Every time is explicit; the fleets are built beat by beat, and
//! the asking host beats just before each question, as a live host does.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use hostkeeper_wire::wire::{Heartbeat, Outcome};

use crate::alerts::{Alert, At, EnrollmentFacts, Kind, Said, Subject, Thresholds, derive};
use crate::decide::{Decision, Gate};
use crate::desired::Desired;
use crate::fleet::{Fleet, QUIET_START};
use crate::live::{Why, why_live};
use crate::ops::OpsState;
use crate::services::Services;
use crate::silent::{SILENT_UPDATE_AFTER, silent_updates};
use crate::testutil::{beat, decide_why, id, last, role, secs, unhealthy, updating, version};

/// `store` and `web` both run on daemons, with the default plan. One daemon may update each at a
/// time (`max_updating`), and none needs to stay up.
const SERVICES: &str = "[[service]]\nname = \"store\"\nroles = [\"daemon\"]\n\
     [[service]]\nname = \"web\"\nroles = [\"daemon\"]\n";

/// When the silent host last beat.
const LAST: u64 = 100;

/// The bound, in seconds.
const BOUND: u64 = SILENT_UPDATE_AFTER.as_secs();

fn want(target: &str) -> Desired {
    let text = format!(
        r#"{{"version":"{target}","max_updating":{{"daemon":1}},"min_up":{{"storage":0,"frontend":0}}}}"#
    );
    Desired::parse(text.as_bytes()).expect("parsed")
}

struct Farm {
    services: Services,
    all: BTreeMap<String, Desired>,
    fleet: Fleet,
}

impl Farm {
    /// Both services want 2.0.
    fn new() -> Self {
        Self {
            services: Services::parse(SERVICES).expect("services"),
            all: [("store", "2.0"), ("web", "2.0")]
                .into_iter()
                .map(|(s, v)| (s.to_owned(), want(v)))
                .collect(),
            fleet: Fleet::new(Duration::ZERO, QUIET_START),
        }
    }

    /// The farm after `d1` last said `said` of `store` at [`LAST`], and nothing since.
    fn after(said: Heartbeat) -> Self {
        let mut farm = Self::new();
        farm.beat("store", "d1", said, LAST);
        farm
    }

    fn beat(&mut self, service: &str, host: &str, b: Heartbeat, at: u64) {
        self.fleet
            .record_in(service, id(host), role("daemon"), b, secs(at));
    }

    /// What the host `host`, which beats 1.0 of `service` just before, is told at `at`.
    fn ask(&mut self, service: &str, host: &str, at: u64) -> Decision {
        self.beat(service, host, beat("1.0"), at);
        decide_why(
            &self.services,
            &self.all,
            service,
            &self.fleet,
            &id(host),
            secs(at),
        )
    }

    /// Why `service`'s rollout is live at `at`, if it is.
    fn live(&self, service: &str, at: u64) -> Option<Why> {
        let def = self.services.get(service).expect("defined");
        why_live(def, self.all.get(service), &self.fleet, secs(at)).map(|live| live.why)
    }

    /// Whether `d1` holds a slot of `store` at `at`.
    fn d1_holds(&self, at: u64) -> bool {
        let host = self.fleet.get_in("store", &id("d1")).expect("heard");
        self.fleet.holds_slot("store", &id("d1"), host, secs(at))
    }
}

fn go() -> Decision {
    Decision::Update {
        version: version("2.0"),
    }
}

fn pending(running: &str) -> Heartbeat {
    let mut b = beat(running);
    b.promotion_pending = true;
    b
}

/// The bound is the one the PR states: an hour.
/// Catches: the bound changed without its reasoning (`SILENT_UPDATE_AFTER`'s doc) changing.
#[test]
fn the_bound_is_an_hour() {
    assert_eq!(BOUND, 3600);
}

/// Behaviour: a host whose last word of `store` is an update running, and that then goes silent,
/// keeps `store`'s rollout live, and `web` on another host waits `rollout_elsewhere`, until it has
/// been silent for exactly the bound: from then on `store` holds no place and `web` is ordered.
/// Catches: a silent mid-update host that holds the farm's one live rollout for ever (the order
/// of the other service never comes), and a bound off by a second either way.
#[test]
fn another_service_is_ordered_once_a_host_silent_mid_update_reaches_the_bound() {
    let mut farm = Farm::after(updating(beat("1.0"), "2.0"));
    let edge = LAST + BOUND;
    assert_eq!(farm.live("store", edge - 1), Some(Why::Updating));
    assert_eq!(
        farm.ask("web", "d2", edge - 1),
        Decision::Gate(Gate::RolloutElsewhere),
        "one second inside the bound"
    );
    assert_eq!(farm.live("store", edge), None);
    assert_eq!(farm.ask("web", "d2", edge), go(), "at the bound");
}

/// Behaviour: a pending promotion holds the same way: the rollout is live, and `web` waits,
/// until the bound; then neither.
/// Catches: the bound applied to `updating` and not to `promotion_pending`.
#[test]
fn a_pending_promotion_of_a_silent_host_holds_until_the_bound() {
    let mut farm = Farm::after(pending("2.0"));
    let edge = LAST + BOUND;
    assert_eq!(farm.live("store", edge - 1), Some(Why::PromotionPending));
    assert_eq!(
        farm.ask("web", "d2", edge - 1),
        Decision::Gate(Gate::RolloutElsewhere)
    );
    assert_eq!(farm.live("store", edge), None);
    assert_eq!(farm.ask("web", "d2", edge), go());
}

/// Behaviour: within its own service, a silent mid-update host takes the role's one update slot
/// (`max_updating` 1), so another daemon of `store` gets `no_slot`, until the bound; from then on
/// the slot is free and that daemon is ordered. `Fleet::holds_slot` says the same.
/// Catches: the place freed but the slot kept, which holds the service's own fix behind the
/// silent host.
#[test]
fn a_silent_mid_update_host_frees_its_slot_at_the_bound() {
    let mut farm = Farm::after(updating(beat("1.0"), "2.0"));
    let edge = LAST + BOUND;
    assert!(farm.d1_holds(edge - 1));
    assert_eq!(
        farm.ask("store", "d2", edge - 1),
        Decision::Gate(Gate::NoSlot)
    );
    assert!(!farm.d1_holds(edge));
    assert_eq!(farm.ask("store", "d2", edge), go());
}

/// Behaviour: a host whose last word is a failure, unhealthy, keeps the slot of its own service
/// past the bound: an operator frees it. It holds no other service's rollout, before the bound
/// or after.
/// Catches: the bound freeing a failed host's slot, which would let the plan pile failures on.
#[test]
fn a_silent_failed_host_keeps_its_own_slot_past_the_bound() {
    let mut farm = Farm::after(unhealthy(last(beat("1.0"), "2.0", Outcome::Failed)));
    let edge = LAST + BOUND;
    assert!(farm.d1_holds(edge));
    assert!(farm.d1_holds(edge + 10 * BOUND));
    assert_eq!(farm.ask("web", "d2", edge - 1), go());
}

/// Behaviour: a host that comes back after the bound is read from its first beat: still
/// updating, it holds `store`'s rollout and slot again from that beat on; at the version, healthy
/// and done, it holds neither, and bakes.
/// Catches: a returning host whose update is ignored for good, and one whose old silence is still
/// held against a beat that says it is done.
#[test]
fn a_host_that_comes_back_is_read_from_its_first_beat() {
    let edge = LAST + BOUND;
    let mut farm = Farm::after(updating(beat("1.0"), "2.0"));
    assert_eq!(farm.live("store", edge), None);
    farm.beat("store", "d1", updating(beat("1.0"), "2.0"), edge + 5);
    assert_eq!(farm.live("store", edge + 5), Some(Why::Updating));
    assert!(farm.d1_holds(edge + 5));
    assert_eq!(
        farm.ask("web", "d2", edge + 5),
        Decision::Gate(Gate::RolloutElsewhere)
    );

    let mut farm = Farm::after(updating(beat("1.0"), "2.0"));
    farm.beat("store", "d1", beat("2.0"), edge + 5);
    assert!(!farm.d1_holds(edge + 5));
    assert_ne!(farm.live("store", edge + 5), Some(Why::Updating));
}

/// The alerts of the farm `farm` at `at` seconds, `silent_while_updating` alone.
fn derived(farm: &Farm, at: u64) -> Vec<Alert> {
    derive(
        &farm.fleet,
        &OpsState::default(),
        &farm.services,
        &EnrollmentFacts::default(),
        At {
            now: secs(at),
            unix: 1_800_000_000 + at,
        },
        &Thresholds::default(),
    )
    .into_iter()
    .filter(|alert| alert.subject.kind == Kind::SilentWhileUpdating)
    .collect()
}

/// The alert of `d1` silent `silent` seconds while its last word was an update of `store` to
/// `target`.
fn alert(target: &str, silent: u64) -> Alert {
    Alert {
        subject: Subject {
            service: "store".to_owned(),
            host: "d1".to_owned(),
            version: target.to_owned(),
            ..Subject::of(Kind::SilentWhileUpdating)
        },
        message: format!(
            "d1 has not heartbeated for {silent} s and its last word was an update of store to \
             {target}: after 3600 s of silence that word holds no update slot and no rollout \
             place, so the other rollouts go on. The update may still be running on the host: \
             check the host before acting on it"
        ),
        said: Said::default(),
    }
}

/// Behaviour: `Alerts.List` names a host silent mid-update from the bound on, with the service,
/// the update's target and the whole message, and not a second before; a pending promotion is
/// named at the version the host runs.
/// Catches: a place freed with no one told, an alert raised before the place is freed, and an
/// alert that names the wrong version.
#[test]
fn the_alert_names_the_host_from_the_bound_on() {
    let edge = LAST + BOUND;
    let farm = Farm::after(updating(beat("1.0"), "2.0"));
    assert_eq!(derived(&farm, edge - 1), Vec::new());
    assert_eq!(derived(&farm, edge), vec![alert("2.0", BOUND)]);
    assert_eq!(derived(&farm, edge + 60), vec![alert("2.0", BOUND + 60)]);

    let farm = Farm::after(pending("2.0"));
    assert_eq!(derived(&farm, edge), vec![alert("2.0", BOUND)]);
}

/// Behaviour: no alert for a host silent past the bound whose last word is no update running (a
/// failure, or settled), nor for a host that came back, nor for a service not managed.
/// Catches: the alert raised for every silent host (`silent_host` says that), or kept after the
/// host is heard again.
#[test]
fn no_alert_without_an_update_running_or_once_the_host_is_back() {
    let edge = LAST + BOUND;
    let failed = Farm::after(unhealthy(last(beat("1.0"), "2.0", Outcome::Failed)));
    assert_eq!(derived(&failed, edge), Vec::new());
    let settled = Farm::after(beat("1.0"));
    assert_eq!(derived(&settled, edge), Vec::new());
    let mut back = Farm::after(updating(beat("1.0"), "2.0"));
    back.beat("store", "d1", updating(beat("1.0"), "2.0"), edge + 5);
    assert_eq!(derived(&back, edge + 5), Vec::new());
    let farm = Farm::after(updating(beat("1.0"), "2.0"));
    let others: BTreeSet<String> = ["web".to_owned()].into();
    assert_eq!(silent_updates(&farm.fleet, &others, secs(edge)), Vec::new());
}

/// Behaviour: the alert's kind is critical, with its own log token and wire value.
/// Catches: the kind wired to another's token or value.
#[test]
fn the_kind_is_critical_with_its_token_and_value() {
    use hostkeeper_proto::hostkeeper::operator::v1 as proto;
    let kind = Kind::SilentWhileUpdating;
    assert_eq!(
        (kind.log_token(), kind.severity(), kind.proto()),
        (
            "silent_while_updating",
            proto::AlertSeverity::Critical,
            proto::AlertKind::SilentWhileUpdating
        )
    );
}
