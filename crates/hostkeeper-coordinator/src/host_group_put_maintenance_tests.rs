//! The `maintenance` table of a group's rollout policy through `HostGroups.Put`
//! ([`crate::operator_host_groups`]): read back whole by `Get` and `List`, every key named when it
//! was left out, each key at its bounds, `xcode_default` held to `xcode_keep`, and the audit entry
//! of a change to the table.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::HostGroupsRefusal as Refusal;

use crate::host_groups_support::{REASON, SELF, World, operator_entry, remove_of};
use crate::marks_support::operator;

/// A change of a maintenance table, for a table of cases.
type Change = fn(&mut proto::MaintenancePolicy);

/// The table with every key named at its default.
fn defaults() -> proto::MaintenancePolicy {
    proto::MaintenancePolicy {
        xcode_keep: 0,
        xcode_default: Some("latest".to_owned()),
        macos_major: false,
        linux_scope: i32::from(proto::LinuxScope::Security),
        batch: Some(1),
        min_serving: Some(1),
        drain_deadline_secs: 0,
        return_secs: 0,
    }
}

/// A table with every key set away from its default.
fn every_key() -> proto::MaintenancePolicy {
    proto::MaintenancePolicy {
        xcode_keep: 2,
        xcode_default: Some("26.1".to_owned()),
        macos_major: true,
        linux_scope: i32::from(proto::LinuxScope::All),
        batch: Some(3),
        min_serving: Some(0),
        drain_deadline_secs: 7200,
        return_secs: 900,
    }
}

fn policy_with(maintenance: Option<proto::MaintenancePolicy>) -> proto::RolloutPolicy {
    proto::RolloutPolicy {
        maintenance,
        ..proto::RolloutPolicy::default()
    }
}

/// The table `Get` answers for `group`.
fn got(world: &World, group: &str) -> proto::MaintenancePolicy {
    world
        .coordinator()
        .host_groups_get(
            &operator(),
            &proto::GetHostGroupRequest {
                host_group: group.to_owned(),
                ..proto::GetHostGroupRequest::default()
            },
        )
        .expect("got")
        .host_group
        .expect("the group")
        .group
        .expect("the body")
        .policy
        .expect("the policy")
        .maintenance
        .expect("the maintenance table")
}

/// Proves: a maintenance table put with every key set away from its default is answered by `Get`
/// and by `List` exactly as it was put.
///
/// Catches: a key that is stored and not answered, or one lost between the request and the store.
#[test]
fn a_maintenance_table_is_read_back_as_it_was_put() {
    let world = World::new("hg-put-maintenance-readback");
    world
        .put_with("alpha", &[SELF], Some(policy_with(Some(every_key()))), None)
        .expect("put");

    assert_eq!(got(&world, "alpha"), every_key());
    let listed = world
        .coordinator()
        .host_groups_list(
            &operator(),
            &proto::ListHostGroupsRequest {
                limit: 0,
                page_token: String::new(),
            },
        )
        .expect("listed");
    let policy = listed.host_groups[0]
        .group
        .as_ref()
        .expect("a body")
        .policy
        .as_ref()
        .expect("the policy");
    assert_eq!(policy.maintenance, Some(every_key()));
}

/// Proves: a group put with no policy, one with a policy that has no maintenance table and one
/// with a table that names nothing are each answered with every maintenance key named at its
/// default.
///
/// Catches: a default the answer leaves out, or one that is not the design's.
#[test]
fn a_maintenance_table_left_out_is_answered_with_every_key_at_its_default() {
    let world = World::new("hg-put-maintenance-defaults");
    world.put("none", &[SELF], None).expect("put");
    world
        .put_with("no-table", &[SELF], Some(policy_with(None)), None)
        .expect("put");
    world
        .put_with(
            "empty-table",
            &[SELF],
            Some(policy_with(Some(proto::MaintenancePolicy::default()))),
            None,
        )
        .expect("put");

    for group in ["none", "no-table", "empty-table"] {
        assert_eq!(got(&world, group), defaults(), "{group}");
    }
}

/// Proves: every key of the maintenance table is held to its range: `xcode_keep` 0 to 4, `batch`
/// 1 to 10000, `min_serving` 0 to 10000, `return_secs` 0 or 60 to 86400, a `linux_scope` the proto
/// defines, an `xcode_default` that is `latest` or a version of 1 to 3 numbers of 1 to 4 digits; the
/// last good value of each is taken, the first past it is `invalid_policy` and changes nothing, and
/// `drain_deadline_secs` takes any number.
///
/// Catches: a bound that is off by one in either direction, or a key that is not checked at all.
#[test]
fn every_key_of_the_maintenance_table_is_held_to_its_range() {
    let cases: Vec<(&str, Change, bool)> = vec![
        ("xcode_keep 4", |m| m.xcode_keep = 4, true),
        ("xcode_keep 5", |m| m.xcode_keep = 5, false),
        ("batch 1", |m| m.batch = Some(1), true),
        ("batch 10000", |m| m.batch = Some(10_000), true),
        ("batch 0", |m| m.batch = Some(0), false),
        ("batch 10001", |m| m.batch = Some(10_001), false),
        ("min_serving 0", |m| m.min_serving = Some(0), true),
        ("min_serving 10000", |m| m.min_serving = Some(10_000), true),
        ("min_serving 10001", |m| m.min_serving = Some(10_001), false),
        ("return_secs 60", |m| m.return_secs = 60, true),
        ("return_secs 86400", |m| m.return_secs = 86_400, true),
        ("return_secs 59", |m| m.return_secs = 59, false),
        ("return_secs 86401", |m| m.return_secs = 86_401, false),
        (
            "drain deadline of u64::MAX",
            |m| m.drain_deadline_secs = u64::MAX,
            true,
        ),
        (
            "linux_scope all",
            |m| m.linux_scope = proto::LinuxScope::All.into(),
            true,
        ),
        ("linux_scope 9", |m| m.linux_scope = 9, false),
        (
            "xcode_default of nothing",
            |m| m.xcode_default = Some(String::new()),
            false,
        ),
        (
            "xcode_default 26.x",
            |m| {
                m.xcode_keep = 1;
                m.xcode_default = Some("26.x".to_owned());
            },
            false,
        ),
        (
            "xcode_default of four numbers",
            |m| {
                m.xcode_keep = 1;
                m.xcode_default = Some("27.2.1.9999".to_owned());
            },
            false,
        ),
        (
            "xcode_default of five digits",
            |m| {
                m.xcode_keep = 1;
                m.xcode_default = Some("12345".to_owned());
            },
            false,
        ),
        (
            "xcode_default of three numbers",
            |m| {
                m.xcode_keep = 1;
                m.xcode_default = Some("26.1.1".to_owned());
            },
            true,
        ),
    ];
    let world = World::new("hg-put-maintenance-bounds");
    let before = world.state();
    for (label, change, accepted) in cases {
        let mut table = defaults();
        change(&mut table);
        let put = world.put_with("alpha", &[SELF], Some(policy_with(Some(table))), None);
        if accepted {
            assert!(put.is_ok(), "{label}: {put:?}");
            world
                .coordinator()
                .host_groups_remove(&operator(), remove_of("alpha"))
                .expect("removed");
        } else {
            assert_eq!(put.expect_err(label), Refusal::InvalidPolicy, "{label}");
        }
    }
    assert_eq!(world.state().0, before.0);
}

/// Proves: an `xcode_default` that names a version is refused when `xcode_keep` is 0, since the
/// group keeps no Xcode, and taken when `xcode_keep` is 1; `latest` is taken with `xcode_keep` 0.
///
/// Catches: a version that no kept Xcode can be, taken without a word.
#[test]
fn an_xcode_default_version_needs_an_xcode_the_group_keeps() {
    let world = World::new("hg-put-maintenance-xcode");
    let table = |keep: u32, default: &str| proto::MaintenancePolicy {
        xcode_keep: keep,
        xcode_default: Some(default.to_owned()),
        ..proto::MaintenancePolicy::default()
    };
    let before = world.state();

    let put = world.put_with(
        "alpha",
        &[SELF],
        Some(policy_with(Some(table(0, "26.1")))),
        None,
    );
    assert_eq!(put.expect_err("refused"), Refusal::InvalidPolicy);
    assert_eq!(world.state().0, before.0);

    world
        .put_with(
            "alpha",
            &[SELF],
            Some(policy_with(Some(table(1, "26.1")))),
            None,
        )
        .expect("a version with an Xcode kept");
    assert_eq!(got(&world, "alpha").xcode_default, Some("26.1".to_owned()));

    world
        .put_with(
            "beta",
            &[SELF],
            Some(policy_with(Some(table(0, "latest")))),
            None,
        )
        .expect("latest with no Xcode kept");
    assert_eq!(got(&world, "beta").xcode_default, Some("latest".to_owned()));
}

/// Proves: the audit entry of a `Put` that changes only the maintenance table names `policy
/// maintenance`, then the operator's reason, and a `Put` that changes nothing in it says `no
/// change`.
///
/// Catches: a change of the table that the audit trail does not name.
#[test]
fn a_change_of_the_maintenance_table_is_named_in_the_audit_reason() {
    let world = World::new("hg-put-maintenance-audit");
    world
        .put_with("alpha", &[SELF], Some(policy_with(None)), None)
        .expect("put");

    world
        .put_with(
            "alpha",
            &[SELF],
            Some(policy_with(Some(every_key()))),
            Some(1),
        )
        .expect("put");
    assert_eq!(
        world.audit().pop().expect("an entry"),
        operator_entry(
            2,
            "host_group_put",
            Some("alpha"),
            None,
            &format!("policy maintenance: {REASON}"),
        )
    );

    world
        .put_with(
            "alpha",
            &[SELF],
            Some(policy_with(Some(every_key()))),
            Some(2),
        )
        .expect("put");
    assert_eq!(
        world.audit().pop().expect("an entry"),
        operator_entry(
            3,
            "host_group_put",
            Some("alpha"),
            None,
            &format!("no change: {REASON}"),
        )
    );
}

/// Proves: a `Put` that replaces a group and leaves the maintenance table out resets the table to
/// its defaults, as a `Put` replaces the group whole.
///
/// Catches: a table that outlives the policy that named it.
#[test]
fn a_put_that_leaves_the_maintenance_table_out_resets_it() {
    let world = World::new("hg-put-maintenance-reset");
    world
        .put_with("alpha", &[SELF], Some(policy_with(Some(every_key()))), None)
        .expect("put");
    assert_eq!(got(&world, "alpha"), every_key());

    world.put("alpha", &[SELF], Some(1)).expect("put");

    assert_eq!(got(&world, "alpha"), defaults());
    assert_eq!(
        world.audit().pop().expect("an entry"),
        operator_entry(
            2,
            "host_group_put",
            Some("alpha"),
            None,
            &format!("policy maintenance: {REASON}"),
        )
    );
}
