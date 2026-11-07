//! The decision names the rule that held. One test per rule, each with a fleet where only
//! that rule fails, so a reply of nothing can be told from every other nothing; a gate and a
//! skip are told apart.

use std::collections::BTreeMap;
use std::time::Duration;

use hostkeeper_wire::wire::{Outcome, Reply};

use crate::decide::{Decision, Gate, Skip, Why, decide_in, decide_why};
use crate::desired::Desired;
use crate::fleet::Fleet;
use crate::services::Services;
use crate::testutil::{
    KBF_ENTRY, LEGACY, beat, desired, fleet, id, kbf_services, last, role, secs, updating, version,
};

const NOW: Duration = Duration::from_secs(1_000);

const TWO: &str = "[[service]]\nname = \"server\"\nroles = [\"daemon\"]\n\n[[service]]\nname = \"agent\"\nroles = [\"daemon\"]\n";

fn gate(why: Gate) -> Decision {
    Decision::Gate(why)
}

fn skip(why: Skip) -> Decision {
    Decision::Skip(why)
}

fn update(target: &str) -> Decision {
    Decision::Update {
        version: version(target),
    }
}

/// The decision, asserted to carry the reply `decide_in` gives for the same question.
fn why(
    services: &Services,
    all: &BTreeMap<String, Desired>,
    service: &str,
    f: &Fleet,
    host: &str,
    now: Duration,
) -> Decision {
    let decision = decide_why(services, all, service, f, &id(host), now);
    let reply = decide_in(services, all, service, f, &id(host), now);
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

/// `kbf`'s decision for `host` under `d` at `now`.
fn kbf(d: &Desired, f: &Fleet, host: &str, now: Duration) -> Decision {
    let all = BTreeMap::from([(LEGACY.to_owned(), d.clone())]);
    why(&kbf_services(), &all, LEGACY, f, host, now)
}

fn two(want: &Desired) -> (Services, BTreeMap<String, Desired>) {
    let all = ["server", "agent"]
        .map(|name| (name.to_owned(), want.clone()))
        .into();
    (Services::parse(TWO).expect("services"), all)
}

/// A host behind the desired version with every rule satisfied is ordered, and the decision
/// carries the desired version.
#[test]
fn a_host_that_passes_every_rule_is_ordered() {
    let f = fleet(NOW, &[("d1", role("daemon"), beat("1.0"))]);
    assert_eq!(kbf(&desired("2.0", 1, 1), &f, "d1", NOW), update("2.0"));
}

/// A service no definition names is not one the host is a member of, even with a desired state:
/// the coordinator orders only the services it manages and assigns.
#[test]
fn a_service_without_a_definition_holds_as_not_a_member() {
    let f = fleet(NOW, &[("d1", role("daemon"), beat("1.0"))]);
    let all = BTreeMap::from([("ghost".to_owned(), desired("2.0", 1, 1))]);
    let services = kbf_services();
    assert_eq!(
        why(&services, &all, "ghost", &f, "d1", NOW),
        skip(Skip::NotAMember)
    );
}

/// A defined service with no desired state holds with `NoDesired`.
#[test]
fn a_service_without_a_desired_state_holds_as_no_desired() {
    let f = fleet(NOW, &[("d1", role("daemon"), beat("1.0"))]);
    let services = kbf_services();
    assert_eq!(
        why(&services, &BTreeMap::new(), LEGACY, &f, "d1", NOW),
        skip(Skip::NoDesired)
    );
}

/// A host the coordinator has not heard from holds with `NotAMember`.
#[test]
fn a_host_that_has_not_heartbeated_holds_as_not_a_member() {
    let f = fleet(NOW, &[("d1", role("daemon"), beat("1.0"))]);
    assert_eq!(
        kbf(&desired("2.0", 1, 1), &f, "d2", NOW),
        skip(Skip::NotAMember)
    );
}

/// A host whose role the definition does not run on holds with `NotAMember`: a storage host
/// asked about a service that runs on daemons.
#[test]
fn a_host_whose_role_the_service_does_not_run_on_holds_as_not_a_member() {
    let want = desired("2.0", 0, 0);
    let (services, all) = two(&want);
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    f.record_in("server", id("s1"), role("storage"), beat("1.0"), NOW);
    assert_eq!(
        why(&services, &all, "server", &f, "s1", NOW),
        skip(Skip::NotAMember)
    );
}

/// Within thirty seconds of the coordinator starting the hold is `Quiet`; at thirty seconds
/// the same host is ordered.
#[test]
fn the_first_thirty_seconds_hold_as_quiet() {
    let d = desired("2.0", 1, 1);
    let mut f = Fleet::new(secs(1_000), crate::fleet::QUIET_START);
    f.record_in(LEGACY, id("d1"), role("daemon"), beat("1.0"), secs(1_029));
    assert_eq!(kbf(&d, &f, "d1", secs(1_000)), gate(Gate::Quiet));
    assert_eq!(kbf(&d, &f, "d1", secs(1_029)), gate(Gate::Quiet));
    assert_eq!(kbf(&d, &f, "d1", secs(1_030)), update("2.0"));
}

/// A host in the middle of an update holds with `Updating`.
#[test]
fn a_host_that_is_updating_holds_as_updating() {
    let f = fleet(NOW, &[("d1", role("daemon"), updating(beat("1.0"), "2.0"))]);
    assert_eq!(
        kbf(&desired("2.0", 1, 1), &f, "d1", NOW),
        gate(Gate::Updating)
    );
}

/// A host with an order outstanding for another service holds with `BusyElsewhere`, and
/// the service that ordered it still answers with the order.
#[test]
fn a_host_busy_with_another_service_holds_as_busy_elsewhere() {
    let want = desired("2.0", 0, 0);
    let (services, all) = two(&want);
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    f.record_in("server", id("h"), role("daemon"), beat("1.0"), NOW);
    f.record_in("agent", id("h"), role("daemon"), beat("1.0"), NOW);
    f.order("server", id("h"), version("2.0"), NOW);
    assert_eq!(
        why(&services, &all, "agent", &f, "h", NOW),
        gate(Gate::BusyElsewhere)
    );
    assert_eq!(why(&services, &all, "server", &f, "h", NOW), update("2.0"));
}

/// A host that runs the desired version holds with `UpToDate`.
#[test]
fn a_host_at_the_desired_version_holds_as_up_to_date() {
    let f = fleet(NOW, &[("d1", role("daemon"), beat("2.0"))]);
    assert_eq!(
        kbf(&desired("2.0", 1, 1), &f, "d1", NOW),
        skip(Skip::UpToDate)
    );
}

/// A host that failed an update to the desired version holds with `FailedHere`; a different
/// desired version lifts it.
#[test]
fn a_host_that_failed_this_version_holds_as_failed_here() {
    let f = fleet(
        NOW,
        &[(
            "d1",
            role("daemon"),
            last(beat("1.0"), "2.0", Outcome::Failed),
        )],
    );
    assert_eq!(
        kbf(&desired("2.0", 1, 1), &f, "d1", NOW),
        skip(Skip::FailedHere)
    );
    assert_eq!(kbf(&desired("3.0", 1, 1), &f, "d1", NOW), update("3.0"));
}

/// A storage host asked while another storage host updates, in a plan of one stage, holds with
/// `NoSlot` (the storage limit is 1); with the default plan, where the first stage holds one of
/// the two hosts, the same host holds with `StageWait`.
#[test]
fn a_role_with_no_free_slot_holds_as_no_slot() {
    let f = fleet(
        NOW,
        &[
            ("s1", role("storage"), updating(beat("1.0"), "2.0")),
            ("s2", role("storage"), beat("1.0")),
        ],
    );
    let d = desired("2.0", 0, 0);
    let whole = Services::parse(&format!("{KBF_ENTRY}\nstages = [100]\n")).expect("services");
    let all = BTreeMap::from([(LEGACY.to_owned(), d.clone())]);
    assert_eq!(why(&whole, &all, LEGACY, &f, "s2", NOW), gate(Gate::NoSlot));
    assert_eq!(kbf(&d, &f, "s2", NOW), gate(Gate::StageWait));
}

/// The first failing rule is the one named: a host that is both up to date and in a role
/// with no slot is `UpToDate`, and an updating host in a quiet start is `Quiet`.
#[test]
fn the_first_failing_rule_is_the_one_named() {
    let f = fleet(
        NOW,
        &[
            ("s1", role("storage"), updating(beat("1.0"), "2.0")),
            ("s2", role("storage"), beat("2.0")),
        ],
    );
    let d = desired("2.0", 0, 0);
    assert_eq!(kbf(&d, &f, "s2", NOW), skip(Skip::UpToDate));
    let mut quiet = Fleet::new(secs(1_000), crate::fleet::QUIET_START);
    quiet.record_in(
        LEGACY,
        id("s1"),
        role("storage"),
        updating(beat("1.0"), "2.0"),
        secs(1_000),
    );
    assert_eq!(kbf(&d, &quiet, "s1", secs(1_000)), gate(Gate::Quiet));
}

/// A host that would be ordered says no reason; a gate and a skip say theirs, each in its own
/// words, through [`Decision::why`] (the status's `hold`).
///
/// Catches: a decision whose reason is dropped, or a gate and a skip reported as one another.
#[test]
fn the_decision_says_why_it_holds_for_the_status() {
    assert_eq!(update("2.0").why(), None);
    assert_eq!(gate(Gate::NoSlot).why(), Some(Why::Gate(Gate::NoSlot)));
    assert_eq!(skip(Skip::UpToDate).why(), Some(Why::Skip(Skip::UpToDate)));
}
