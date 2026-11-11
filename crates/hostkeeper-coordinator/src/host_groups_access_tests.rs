//! The pieces the `HostGroups` calls are built from ([`crate::host_groups`],
//! [`crate::desired`]): the rules of a list of packages, the keys a policy differs in, the marks
//! helpers of the operations state, and the host groups of the desired state store.

use std::convert::Infallible;

use hostkeeper_wire::version::Version;

use crate::desired::DesiredStore;
use crate::host_groups::{
    GroupMarks, GroupName, HostGroup, HostGroups, ListError, Policy, check_packages,
};
use crate::host_groups_support::{SELF, World};
use crate::ops::OpsState;
use crate::reconcile::Rollback;

fn list(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}

fn name(text: &str) -> GroupName {
    text.parse().expect("a group name")
}

/// Proves: a list is refused for the first rule it breaks, in the order: its count (none, or over
/// 64), a name that is no package's, a name twice, `hostkeeper` missing; 64 packages with
/// `hostkeeper` among them are a list.
///
/// Catches: a list of 65 taken, an empty one taken for want of a check, or a duplicate that
/// passes because the first of its two is valid.
#[test]
fn a_list_of_packages_is_refused_for_the_first_rule_it_breaks() {
    let many = |n: usize| -> Vec<String> {
        std::iter::once(SELF.to_owned())
            .chain((1..n).map(|i| format!("p{i}")))
            .collect()
    };
    assert_eq!(check_packages(&many(64)), Ok(()));
    assert_eq!(check_packages(&many(65)), Err(ListError::Count(65)));
    assert_eq!(check_packages(&[]), Err(ListError::Count(0)));
    assert_eq!(
        check_packages(&list(&["bad name", "hostkeeper"])),
        Err(ListError::Package("bad name".to_owned()))
    );
    assert_eq!(
        check_packages(&list(&["a", "hostkeeper", "a"])),
        Err(ListError::Twice("a".to_owned()))
    );
    assert_eq!(
        check_packages(&list(&["a", "b"])),
        Err(ListError::HostkeeperNotListed)
    );
}

/// Proves: two policies differ in the keys that differ, in the order of the policy, and a policy
/// differs from itself in none.
#[test]
fn the_keys_two_policies_differ_in_are_named_in_the_policys_order() {
    let base = Policy::default();
    assert_eq!(base.changed_keys(&base), Vec::<&str>::new());
    let other = Policy {
        min_up: 1,
        rollback: Rollback::Automatic,
        bake_beats: 2,
        live_rollouts: Some(3),
        ..Policy::default()
    };
    assert_eq!(
        other.changed_keys(&base),
        ["bake_beats", "rollback", "min_up", "live_rollouts"]
    );
}

/// A group of `hostkeeper` and `svc`.
fn group() -> HostGroup {
    HostGroup {
        packages: list(&[SELF, "svc"]),
        policy: Policy::default(),
        generation: 1,
        by: "alice".to_owned(),
        at_unix: 1,
        desired: std::collections::BTreeMap::new(),
    }
}

/// Proves: the marks of a package in a group read as the default for a pair with none; dropping
/// the packages a group no longer lists takes their marks but their minimum versions and leaves
/// the marks of the packages kept; dropping the marks of a group not held changes nothing;
/// dropping a group takes all its marks.
///
/// Catches: a `Put` that forgets the minimum version `SetMinVersion` set, or one that drops the
/// halt of a package it still lists.
#[test]
fn the_marks_of_a_group_are_read_dropped_by_package_and_dropped_whole() {
    let web = name("web");
    let mut state = OpsState::default();
    assert_eq!(state.group_marks(&web, "svc"), GroupMarks::default());
    let v = |text: &str| Version::parse(text).expect("a version");
    let marks = state.host_groups.0.entry(web.clone()).or_default();
    marks.insert(
        "svc".to_owned(),
        GroupMarks {
            min_version: Some(v("1.0")),
            last_good: Some(v("1.5")),
            force_generation: 2,
            ..GroupMarks::default()
        },
    );
    marks.insert(
        "cfg".to_owned(),
        GroupMarks {
            last_good: Some(v("2.0")),
            ..GroupMarks::default()
        },
    );
    marks.insert(
        "kept".to_owned(),
        GroupMarks {
            last_good: Some(v("3.0")),
            ..GroupMarks::default()
        },
    );
    state.drop_group_packages(&name("db"), &list(&[]));
    assert_eq!(state.host_groups.0[&web].len(), 3);

    state.drop_group_packages(&web, &list(&["kept"]));
    assert_eq!(
        state.group_marks(&web, "svc"),
        GroupMarks {
            min_version: Some(v("1.0")),
            ..GroupMarks::default()
        }
    );
    assert_eq!(state.group_marks(&web, "cfg"), GroupMarks::default());
    assert_eq!(state.group_marks(&web, "kept").last_good, Some(v("3.0")));
    assert!(!state.host_groups.0[&web].contains_key("cfg"));

    state.drop_group(&web);
    assert!(state.host_groups.0.is_empty());
}

/// Proves: the store reads no group until one is staged and committed, then answers the groups,
/// one group, and that it is in group mode; a staged edit that refuses stages nothing.
///
/// Catches: a mode that is on with no group, or a staging that changes memory before its commit.
#[test]
fn the_store_answers_the_groups_one_group_and_the_mode() {
    let world = World::new("groups-access-store");
    let store: &DesiredStore = &world.coordinator().desired;
    assert!(!store.in_group_mode());
    assert_eq!(store.host_groups(), HostGroups::default());
    assert_eq!(store.host_group(&name("web")), None);

    let refused = store.stage_host_groups(|_| Err::<(HostGroups, ()), _>("no"));
    assert_eq!(refused.err(), Some("no"));
    assert!(!store.in_group_mode());

    world.put("web", &[SELF, "svc"], None).expect("put");
    assert!(store.in_group_mode());
    let held = store.host_group(&name("web")).expect("the group");
    assert_eq!(held.packages, list(&[SELF, "svc"]));
    assert_eq!(store.host_groups().0.len(), 1);

    let staged = store
        .stage_host_groups(|groups| Ok::<_, Infallible>((HostGroups::default(), groups.0.len())))
        .expect("staged");
    assert_eq!(staged.1, 1);
    assert!(store.in_group_mode(), "a staged edit is not in memory");
    drop(staged);
    assert!(group().check().is_ok());
}
