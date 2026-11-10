//! What makes the plan of `host-groups migrate` an error: each is said, and names the host, role,
//! group, service or serial it is about.

use std::collections::{BTreeMap, BTreeSet};

use crate::assignment::Entry;
use crate::host_groups::{GroupName, HostGroup, Policy};
use crate::migrate_plan::verify;
use crate::migrate_support::{Store, desired};
use crate::ops::OpsState;

/// Two services on `web`, each placed by the role.
const WEB: &str = "[[service]]\nname = \"kbf\"\nroles = [\"web\"]\n";

/// Proves: a host whose role is empty, and one whose role is not a group name, are errors naming
/// the host, and the plan still says the groups of the others.
///
/// Catches: a host left in no group that the plan does not mention.
#[test]
fn a_host_whose_role_no_group_takes_is_an_error_naming_it() {
    let plan = Store::of(&[("w1", "web"), ("w2", ""), ("w3", "Not_A_Role")]).plan(WEB);
    assert_eq!(
        plan.errors,
        [
            "host w2: has no role, so no group takes it",
            "host w3: role \"Not_A_Role\" is not a group name",
        ]
    );
    assert!(plan.lines[0].starts_with("group web (from role web): 1 hosts"));
}

/// Proves: a role that records name and that is not a group name is an error naming the record.
///
/// Catches: a record whose role the new coordinator would refuse at its first read.
#[test]
fn a_record_that_names_a_bad_role_is_an_error_naming_it() {
    use crate::inventory::InventoryRecord;
    use hostkeeper_enrollment::enroll::records::pb;
    let mut store = Store::of(&[("w1", "web")]);
    store.moved.loaded.ops.inventory = vec![InventoryRecord {
        host_id: "n1".to_owned(),
        role: "Bad Role".to_owned(),
        host_group: String::new(),
        data_macs: Vec::new(),
        bmc_mac: String::new(),
        serial: String::new(),
        notes: String::new(),
        by: "alice".to_owned(),
        at_unix: 1,
        approve_until_unix: 0,
        used: None,
        platform: None,
    }];
    store.moved.records.requests = vec![pb::Request {
        request_id: vec![9; 32],
        state: pb::RequestState::Approved as i32,
        approved: Some(pb::Approval {
            host_id: "n2".to_owned(),
            role: "also bad".to_owned(),
            ..pb::Approval::default()
        }),
        ..pb::Request::default()
    }];
    let plan = store.plan(WEB);
    assert_eq!(
        plan.errors,
        [
            "inventory record n1: role \"Bad Role\" is not a group name",
            "approval of n2: role \"also bad\" is not a group name",
        ]
    );
}

/// Proves: a derived group name over 32 bytes is an error naming the role, and the group is not
/// made.
///
/// Catches: a `<role>-2` that is no group name, written into the state the coordinator then
/// refuses to read.
#[test]
fn a_derived_name_over_32_bytes_is_an_error_naming_the_role() {
    let role = "r".repeat(31);
    let mut store = Store::of(&[("a", &role), ("b", &role)]);
    store.entry("kbf", "b", Entry::Exclude);
    let plan = store.plan(&format!(
        "[[service]]\nname = \"kbf\"\nroles = [\"{role}\"]\n"
    ));
    assert_eq!(
        plan.errors,
        [format!(
            "role {role}: the group name {role}-2 is not a group name (over 32 bytes)"
        )]
    );
    assert_eq!(plan.after.loaded.desired.host_groups.0.len(), 1);
}

/// Proves: a derived group name that is already the name of a role (of a host, here) is an error
/// naming the role, as is one a role entry names.
///
/// Catches: a `daemon-2` that a script naming the role `daemon-2` would then mean something else
/// by.
#[test]
fn a_derived_name_that_is_a_role_is_an_error_naming_the_role() {
    let mut store = Store::of(&[("a", "daemon"), ("b", "daemon"), ("c", "daemon-2")]);
    store.entry("kbf", "b", Entry::Exclude);
    let rollout = "[[service]]\nname = \"kbf\"\nroles = [\"daemon\", \"daemon-2\"]\n";
    let plan = store.plan(rollout);
    assert_eq!(
        plan.errors,
        ["role daemon: the group name daemon-2 is the name of a role"]
    );
    let mut store = Store::of(&[("a", "daemon"), ("b", "daemon")]);
    store.entry("kbf", "b", Entry::Exclude);
    store.assignments("kbf", |a| {
        a.roles.insert("daemon-2".to_owned(), Entry::Include);
    });
    store.moved.loaded.ops.holds = vec![crate::holds::HoldMark {
        service: "kbf".to_owned(),
        scope: crate::holds::HoldScope::Role("daemon-2".to_owned()),
        reason: "r".to_owned(),
        by: "alice".to_owned(),
        at_unix: 1,
    }];
    let plan = store.plan("[[service]]\nname = \"kbf\"\nroles = [\"daemon\"]\n");
    assert_eq!(
        plan.errors,
        ["role daemon: the group name daemon-2 is the name of a role"]
    );
}

/// Proves: a group of more than 64 packages is an error naming the group.
///
/// Catches: a group the coordinator refuses to read, written by the migration.
#[test]
fn more_than_64_packages_is_an_error_naming_the_group() {
    let defs = (0..65)
        .map(|n| crate::services::ServiceDef::new(&format!("p{n:02}"), &["web"]))
        .collect();
    let rollout = crate::services::Services::from_defs(defs);
    let plan = Store::of(&[("w1", "web")]).plan_services(&rollout, None);
    assert_eq!(plan.errors, ["group web: lists 66 packages, not 1 to 64"]);
}

/// Proves: a policy key out of its range, here a `max_updating` the desired state holds over the
/// bound, is an error naming the group and the key.
///
/// Catches: a policy the coordinator refuses when it reads the group back.
#[test]
fn a_policy_out_of_range_is_an_error_naming_the_group_and_key() {
    let mut store = Store::of(&[("w1", "web")]);
    let mut state = desired("1.0.0");
    state.max_updating.set("web", 20_000);
    store.want("kbf", state);
    let plan = store.plan(WEB);
    assert_eq!(
        plan.errors,
        ["group web: policy max_updating: is over 10000"]
    );
}

/// Proves: more holds than the ops state keeps once a role's holds are copied is an error.
///
/// Catches: an ops file the coordinator refuses to read.
#[test]
fn more_holds_than_the_state_keeps_is_an_error() {
    let mut store = Store::of(&[("a", "web"), ("b", "web")]);
    store.entry("kbf", "b", Entry::Exclude);
    store.moved.loaded.ops.holds = (0..crate::holds::MAX_HOLDS)
        .map(|n| crate::holds::HoldMark {
            service: "kbf".to_owned(),
            scope: crate::holds::HoldScope::Role("web".to_owned()),
            reason: format!("hold {n}"),
            by: "alice".to_owned(),
            at_unix: 1,
        })
        .collect();
    let plan = store.plan(WEB);
    assert_eq!(plan.errors, ["the holds would be over 4096"]);
}

/// Proves: a store with no host and no record that names a role is an error: there is no group to
/// make.
///
/// Catches: a migration that succeeds and writes nothing, after which the coordinator places no
/// host.
#[test]
fn a_store_with_nothing_to_group_is_an_error() {
    let plan = Store::default().plan(WEB);
    assert_eq!(
        plan.errors,
        ["no host and no record names a role: there is no group to make"]
    );
}

/// The groups of one group `name` listing `packages`.
fn groups(name: &str, packages: &[&str]) -> BTreeMap<GroupName, HostGroup> {
    BTreeMap::from([(
        name.parse().expect("a name"),
        HostGroup {
            packages: packages.iter().map(|p| (*p).to_owned()).collect(),
            policy: Policy::default(),
            generation: 1,
            by: "migration".to_owned(),
            at_unix: 1,
            desired: BTreeMap::new(),
        },
    )])
}

/// Proves: the check that every host's placement is unchanged finds a package placed today and not
/// after (and the reverse) and names the host, the package and the group, and a host whose group
/// was not made.
///
/// Catches: a check that cannot fail, which would pass any plan.
#[test]
fn the_check_of_every_hosts_placement_names_a_difference() {
    let set =
        |names: &[&str]| -> BTreeSet<String> { names.iter().map(|n| (*n).to_owned()).collect() };
    let names = set(&["hello", "hostkeeper", "kbf"]);
    let hosts = vec![(
        "w1".to_owned(),
        "web".parse::<GroupName>().expect("a name"),
        set(&["hostkeeper", "kbf"]),
    )];
    let ops = OpsState::default();
    assert_eq!(
        verify(&groups("web", &["hostkeeper", "kbf"]), &ops, &hosts, &names),
        Vec::<String>::new()
    );
    assert_eq!(
        verify(
            &groups("web", &["hostkeeper", "hello"]),
            &ops,
            &hosts,
            &names
        ),
        [
            "host w1: hello is not placed today and placed after the migration (group web)",
            "host w1: kbf is placed today and not placed after the migration (group web)",
        ]
    );
    assert_eq!(
        verify(&groups("other", &["hostkeeper"]), &ops, &hosts, &names),
        ["host w1: its group web is not made"]
    );
}
