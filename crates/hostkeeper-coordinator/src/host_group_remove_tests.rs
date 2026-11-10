//! `HostGroups.Remove` ([`crate::operator_host_groups`]): the group, its desired versions and its
//! marks go, with the audit entry; a group a host is in or moving into, or an inventory record
//! names, stays; each refusal changes nothing.

use hostkeeper_proto::hostkeeper::operator::v1::HostGroupsRefusal as Refusal;

use crate::host_groups::GroupName;
use crate::host_groups_support::{REASON, SELF, World, operator_entry, remove_of};
use crate::inventory::InventoryRecord;
use crate::marks_support::operator;
use crate::operator::{Caller, entry_proto};
use crate::ops::{AuditEntry, Write};
use crate::testutil::version;

/// Proves: `Remove` of a group drops it with its desired versions and its marks (those of every
/// package, minimum versions included), leaves another group's, and answers the audit entry
/// `host_group_remove` about the group with the operator's reason; `Get` of it is then
/// `unknown_host_group`.
///
/// Catches: marks of a removed group that come back when a group of its name is made again.
#[test]
fn a_remove_drops_the_group_its_desired_versions_and_its_marks() {
    let world = World::new("hg-remove-drops");
    world.put("alpha", &[SELF, "svc"], None).expect("put");
    world.put("beta", &[SELF, "svc"], None).expect("put");
    for group in ["alpha", "beta"] {
        world.seed_desired(group, "svc", "1.0");
        world.seed_marks(group, "svc", |marks| {
            marks.min_version = Some(version("0.5"));
            marks.last_good = Some(version("1.0"));
        });
    }
    let audited = world.audit().len() as u64;

    let removed = world
        .coordinator()
        .host_groups_remove(&operator(), remove_of("alpha"))
        .expect("removed");

    let wanted = operator_entry(
        audited + 1,
        "host_group_remove",
        Some("alpha"),
        None,
        REASON,
    );
    assert_eq!(removed.entry, Some(entry_proto(&wanted)));
    assert_eq!(world.audit().pop(), Some(wanted));
    let alpha: GroupName = "alpha".parse().expect("a name");
    let beta: GroupName = "beta".parse().expect("a name");
    assert_eq!(world.coordinator().desired.host_group(&alpha), None);
    assert!(world.coordinator().desired.host_group(&beta).is_some());
    let ops = world.ops();
    assert!(!ops.host_groups.0.contains_key(&alpha));
    assert_eq!(
        ops.group_marks(&beta, "svc").last_good,
        Some(version("1.0"))
    );
    let again = world
        .coordinator()
        .host_groups_remove(&operator(), remove_of("alpha"))
        .expect_err("gone");
    assert_eq!(again, Refusal::UnknownHostGroup);
    world
        .put("alpha", &[SELF, "svc"], None)
        .expect("made again");
    assert_eq!(world.ops().group_marks(&alpha, "svc"), Default::default());
}

/// Proves: a group a host is in, a host moving into it and an inventory record that names it is
/// not removed (`not_empty`), a host that moved on or was retired no longer holds it, and once
/// nothing names it the same call removes it.
///
/// Catches: a group removed from under a host, which would then be placed by nothing.
#[test]
fn a_group_a_host_or_a_record_names_is_not_empty() {
    let world = World::new("hg-remove-not-empty");
    for host in ["d1", "d2"] {
        world.enroll(host);
    }
    world.put("alpha", &[SELF], None).expect("put");
    world.put("beta", &[SELF], None).expect("put");
    world.move_to("d1", "alpha").expect("moved");
    world.move_to("d2", "alpha").expect("moved");
    world.move_to("d2", "beta").expect("moved again");
    let remove = |group: &str| {
        world
            .coordinator()
            .host_groups_remove(&operator(), remove_of(group))
    };
    let before = world.state();
    assert_eq!(remove("alpha").expect_err("d1 is in it"), Refusal::NotEmpty);
    assert_eq!(
        remove("beta").expect_err("d2 moves into it"),
        Refusal::NotEmpty
    );
    assert_eq!(world.state(), before);

    world.move_to("d1", "beta").expect("moved");
    world
        .coordinator()
        .ops()
        .change(
            Write::BeforeMemory,
            AuditEntry::new("test", "seed"),
            |state| {
                state.inventory.push(InventoryRecord {
                    host_id: "d3".to_owned(),
                    role: "storage".to_owned(),
                    host_group: "alpha".to_owned(),
                    data_macs: Vec::new(),
                    bmc_mac: String::new(),
                    serial: String::new(),
                    notes: String::new(),
                    by: "alice".to_owned(),
                    at_unix: 1,
                    approve_until_unix: 0,
                    used: None,
                    platform: None,
                });
            },
        )
        .expect("seeded");
    let before = world.state();
    assert_eq!(
        remove("alpha").expect_err("a record names it"),
        Refusal::NotEmpty
    );
    assert_eq!(world.state(), before);
    world
        .coordinator()
        .ops()
        .change(
            Write::BeforeMemory,
            AuditEntry::new("test", "seed"),
            |state| {
                state.inventory.clear();
            },
        )
        .expect("seeded");
    remove("alpha").expect("nothing names it now");

    world
        .enrollment
        .enrollment
        .store()
        .deny(&hostkeeper_enrollment::names::HostId::parse("d1").expect("a host id"))
        .expect("denied");
    world
        .enrollment
        .enrollment
        .store()
        .deny(&hostkeeper_enrollment::names::HostId::parse("d2").expect("a host id"))
        .expect("denied");
    remove("beta").expect("a retired host holds no group");
}

/// Proves: every refusal of `Remove` is its token and changes nothing: CI's token `forbidden`
/// and none `unauthorized`; a name that breaks the grammar and a reason that is empty, over 256
/// bytes or holds a control character `invalid_request`; a group not held `unknown_host_group`;
/// a store that fails `store_failed`, and one whose audit line cannot be kept
/// `audit_unavailable`.
///
/// Catches: a refusal that removes the group all the same.
#[test]
fn every_refusal_of_a_remove_changes_nothing() {
    let world = World::new("hg-remove-refusals");
    world.put("alpha", &[SELF], None).expect("put");
    let before = world.state();
    let coordinator = world.coordinator();
    let remove = |caller: &Caller, group: &str, reason: &str| {
        let mut request = remove_of(group);
        request.reason = reason.to_owned();
        coordinator
            .host_groups_remove(caller, request)
            .expect_err("refused")
    };
    assert_eq!(remove(&Caller::Ci, "alpha", REASON), Refusal::Forbidden);
    assert_eq!(
        remove(&Caller::Anonymous, "alpha", REASON),
        Refusal::Unauthorized
    );
    for group in ["", "Alpha", "a--b"] {
        assert_eq!(
            remove(&operator(), group, REASON),
            Refusal::InvalidRequest,
            "{group:?}"
        );
    }
    let long = "x".repeat(257);
    for reason in ["", long.as_str(), "a\u{7}b"] {
        assert_eq!(
            remove(&operator(), "alpha", reason),
            Refusal::InvalidRequest,
            "{reason:?}"
        );
    }
    assert_eq!(
        remove(&operator(), "beta", REASON),
        Refusal::UnknownHostGroup
    );
    for (failure, refusal) in [
        (
            crate::store::store_failure as fn() -> _,
            Refusal::StoreFailed,
        ),
        (crate::store::audit_failure, Refusal::AuditUnavailable),
    ] {
        world.fake.refuse(move |_| Some(failure()));
        assert_eq!(remove(&operator(), "alpha", REASON), refusal);
    }
    world.fake.heal();
    assert_eq!(world.state(), before);
}
