//! The uninstall intents a change of a host group writes and cancels
//! ([`crate::group_uninstall`]): who is asked to uninstall what, the generations that order a
//! host's uninstalls, and the intents a change takes back.

use std::collections::BTreeMap;
use std::time::Duration;

use hostkeeper_wire::Platform;
use hostkeeper_wire::wire::Health;

use crate::assignment::ServiceAssignments;
use crate::fleet::{Fleet, Node, QUIET_START};
use crate::group_uninstall::{Placed, Wanted, cancel_uninstalls, lost_by, order_uninstalls};
use crate::host_groups::{HostGroup, HostGroups, Policy};
use crate::ops::{AuditEntry, OpsState};
use crate::testutil::id;

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|n| (*n).to_owned()).collect()
}

/// A fleet of the hosts `hosts`, each last heard listing `services`.
fn fleet_of(hosts: &[(&str, &[&str])]) -> Fleet {
    let mut fleet = Fleet::new(Duration::ZERO, QUIET_START);
    for (host, services) in hosts {
        let node = Node {
            role: "web".to_owned(),
            features: Vec::new(),
            agent: None,
            platform: Platform::LinuxX86_64,
            seen: Duration::from_secs(5),
            announced: services
                .iter()
                .map(|name| ((*name).to_owned(), Health::Healthy))
                .collect(),
            sent_unix: None,
            cert_not_after_unix: None,
            last_renew_error: None,
            trusted_root: crate::trusted_root::TrustedRootSaid::default(),
            skewed_beats: 0,
        };
        fleet.note_node(id(host), node, 0);
    }
    fleet
}

/// A group `name` listing `packages`, as the groups of a coordinator hold it.
fn group_of(packages: &[&str]) -> HostGroup {
    HostGroup {
        packages: names(packages),
        policy: Policy::default(),
        generation: 1,
        by: "test".to_owned(),
        at_unix: 0,
        desired: BTreeMap::new(),
    }
}

/// The groups `held`, each a name and its list.
fn groups_of(held: &[(&str, &[&str])]) -> HostGroups {
    HostGroups(
        held.iter()
            .map(|(name, packages)| ((*name).parse().expect("a name"), group_of(packages)))
            .collect(),
    )
}

/// Proves: of the packages a host runs before a change and not after, in the reverse of the order
/// it was offered them, each is paired with the hosts among those named whose last heartbeat lists
/// it, in the order of the hosts; a package no named host lists is left out, a host the fleet never
/// heard from lists nothing, and a package that `all` lists is kept by every host.
///
/// Catches: an uninstall ordered of a host that never listed the package (a hand-installed one
/// is never touched, and a host that has none has nothing to uninstall), or of a package a host
/// still runs because `all` lists it.
#[test]
fn only_the_packages_a_host_listed_and_no_longer_runs_are_paired_with_it() {
    let fleet = fleet_of(&[
        ("d1", &["svc", "cfg", "base"]),
        ("d2", &["svc", "base"]),
        ("d3", &["other"]),
    ]);
    let before = groups_of(&[("all", &["base"]), ("web", &["cfg", "svc", "never"])]);
    let after = groups_of(&[("all", &["base"]), ("web", &[])]);
    let web = || Some("web".parse().expect("a name"));
    let members: Vec<Placed> = ["d1", "d2", "d3", "unheard"]
        .iter()
        .map(|host| (id(host), web(), web()))
        .collect();
    assert_eq!(
        lost_by(&fleet, &before, &after, &members),
        vec![
            ("svc".to_owned(), names(&["d1", "d2"])),
            ("cfg".to_owned(), names(&["d1"])),
        ]
    );
    // The host that moves from `web` to no specific group keeps what `all` lists, and loses the rest.
    let moved = [(id("d1"), web(), None)];
    assert_eq!(
        lost_by(&fleet, &before, &before, &moved),
        vec![
            ("svc".to_owned(), names(&["d1"])),
            ("cfg".to_owned(), names(&["d1"])),
        ]
    );
}

/// Proves: the intents are written one generation above the highest any package has been given,
/// the first package of the order the lowest and each next one more, each package's own counter
/// raised to its generation, and every one of its hosts given that generation.
///
/// Catches: two packages given one order id, or an intent a later `Unassign` cannot be told
/// from, or a package listed after the one it configures uninstalled after it.
#[test]
fn each_package_is_given_a_generation_above_every_one_given_before() {
    let mut state = OpsState::default();
    state.assignments.insert(
        "old".to_owned(),
        ServiceAssignments {
            uninstall_generation: 7,
            ..ServiceAssignments::default()
        },
    );
    let wanted: Vec<Wanted> = vec![
        ("cfg".to_owned(), names(&["d1"])),
        ("svc".to_owned(), names(&["d1", "d2"])),
    ];
    order_uninstalls(&mut state, &wanted);
    assert_eq!(state.assignments["cfg"].uninstall_generation, 8);
    assert_eq!(state.assignments["svc"].uninstall_generation, 9);
    assert_eq!(
        state.assignments["cfg"].uninstall,
        BTreeMap::from([("d1".to_owned(), 8)])
    );
    assert_eq!(
        state.assignments["svc"].uninstall,
        BTreeMap::from([("d1".to_owned(), 9), ("d2".to_owned(), 9)])
    );
    assert_eq!(state.assignments["old"].uninstall_generation, 7);

    let mut fresh = OpsState::default();
    order_uninstalls(&mut fresh, &wanted[..1]);
    assert_eq!(fresh.assignments["cfg"].uninstall_generation, 1);
    order_uninstalls(&mut fresh, &[]);
    assert_eq!(fresh.assignments.len(), 1);
}

/// Proves: cancelling drops the intents of the hosts named for the packages named, answers one
/// `uninstall_cancelled` entry by the coordinator for each (naming the group of the change),
/// packages first and hosts in order, keeps the counter of a package whose last intent went, and touches neither a host not named,
/// a package not named, nor a package that has no entry.
///
/// Catches: a package put back in a group that is still uninstalled from its hosts, or a
/// cancelled intent that leaves no audit entry.
#[test]
fn cancelling_drops_the_named_intents_and_answers_one_entry_each() {
    let mut state = OpsState::default();
    order_uninstalls(
        &mut state,
        &[
            ("cfg".to_owned(), names(&["d1", "d2"])),
            ("svc".to_owned(), names(&["d1"])),
        ],
    );
    let entries = cancel_uninstalls(
        &mut state,
        &"alpha".parse().expect("a group name"),
        &names(&["svc", "cfg", "none"]),
        &names(&["d1", "d3"]),
    );
    let entry = |service: &str, node: &str| {
        let mut entry = AuditEntry::new("coordinator", "uninstall_cancelled");
        entry.service = Some(service.to_owned());
        entry.node_id = Some(node.to_owned());
        entry.host_group = Some("alpha".to_owned());
        entry
    };
    assert_eq!(entries, vec![entry("svc", "d1"), entry("cfg", "d1")]);
    assert_eq!(
        state.assignments["cfg"].uninstall,
        BTreeMap::from([("d2".to_owned(), 1)])
    );
    assert_eq!(
        state.assignments["svc"],
        ServiceAssignments {
            uninstall_generation: 2,
            ..ServiceAssignments::default()
        }
    );
}
