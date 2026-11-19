//! The coordinator has no built-in host group (`docs/design/hostgroups.md`, "Removing the built-in
//! group all"): a host is in one group or in none, a host in none is ordered no package, every
//! group lists `hostkeeper`, and `all` is a name like any other.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::HostGroupsRefusal as Refusal;
use serde_json::json;

use crate::group_decision_support::Farm;
use crate::host_groups_support::{SELF, World, remove_of};
use crate::marks_support::operator;
use crate::orders::Order;
use crate::service::SetError;
use crate::service::ci_calls::Setter;
use crate::write_record::WriteAsk;

const GROUP: &str = "web";

/// The package an order installs or updates, if it is one.
fn package_of(order: Option<Order>) -> Option<String> {
    match order {
        Some(Order::Install { service, .. }) => Some(service),
        _ => None,
    }
}

/// Proves: a host in no group is answered its heartbeat and ordered nothing, whatever it runs:
/// not an install of a package a group lists and wants, not an update of `hostkeeper` to a version
/// a group wants, and not an uninstall of a package it runs that no group lists; a host of
/// the group `web` beside it is ordered.
///
/// Catches: an unassigned host ordered what a group lists, as if it were in a group of every
/// host, or its agent updated or a package uninstalled by an order no group gave it.
#[tokio::test]
async fn a_host_in_no_group_is_ordered_nothing_hostkeeper_included() {
    let farm = Farm::new("no-all-unassigned");
    farm.group(GROUP, &["tierstore"], None);
    farm.deploy(GROUP, SELF, "2.0", 1).await;
    farm.deploy(GROUP, "tierstore", "1.0", 2).await;
    farm.lone("u1");
    farm.host("w1", GROUP);

    for runs in [
        &[][..],
        &[("tierstore", "0.9")][..],
        &[("tierstore", "1.0"), ("legacy", "1.0")][..],
    ] {
        assert_eq!(farm.order("u1", runs), None, "{runs:?}");
    }
    assert!(
        farm.order("w1", &[]).is_some(),
        "a host of the group is ordered what the group wants"
    );
    assert!(
        farm.world.intents().iter().all(|(_, host, _)| host != "u1"),
        "no uninstall is recorded for it"
    );
}

/// Proves: a `Put` of any group that does not list `hostkeeper` is refused `hostkeeper_not_listed`
/// and writes nothing, the group named `all` included; the same `Put` with `hostkeeper` listed is
/// taken.
///
/// Catches: a group that can be made without the agent, whose hosts would lose it at their next
/// uninstall, or a name that is let off the rule.
#[test]
fn a_put_without_hostkeeper_is_refused_for_every_name() {
    let world = World::new("no-all-put");
    let before = world.state();
    for name in [GROUP, "all", "all-in-one"] {
        assert_eq!(
            world.put(name, &["tierstore"], None).expect_err("refused"),
            Refusal::HostkeeperNotListed,
            "{name}"
        );
        assert_eq!(
            world.put(name, &[], None).expect_err("refused"),
            Refusal::HostkeeperNotListed,
            "{name}"
        );
    }
    assert_eq!(world.state(), before);
    world.put("all", &[SELF], None).expect("a group named all");
}

/// Proves: `all` is an ordinary group name: a group made under it holds no host until one is
/// moved into it, `List` counts no host in it for the hosts of other groups and for those in none,
/// the packages it lists run on no host outside it, and `Remove` takes it away like any other
/// empty group.
///
/// Catches: `all` still read as the group of every host, or a group named so that cannot be
/// removed.
#[test]
fn a_group_named_all_is_a_group_like_any_other() {
    let world = World::new("no-all-ordinary");
    world.put(GROUP, &[SELF, "tierstore"], None).expect("put");
    world.put("all", &[SELF, "base"], None).expect("put");
    world.enroll("u1");
    world.enroll("w1");
    world.move_to("w1", GROUP).expect("moved");

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
    let counts: Vec<(&str, u32)> = listed
        .host_groups
        .iter()
        .map(|state| {
            (
                state.group.as_ref().expect("a body").name.as_str(),
                state.hosts,
            )
        })
        .collect();
    assert_eq!(counts, [("all", 0), (GROUP, 1)]);
    assert_eq!(
        world
            .coordinator()
            .hosts_in(&"all".parse().expect("a name")),
        Ok(Vec::new())
    );

    world
        .coordinator()
        .host_groups_remove(&operator(), remove_of("all"))
        .expect("removed");
    assert_eq!(
        world
            .coordinator()
            .host_groups_remove(&operator(), remove_of("all"))
            .expect_err("gone"),
        Refusal::UnknownHostGroup
    );
}

/// Proves: a set of a desired version that names the group `all`, when no group of that name is
/// held, is refused as a group that does not list the package (`not_in_host_group`) and changes
/// nothing, as a set naming any other missing group is; a set that names no group still reaches
/// each group that lists the package.
///
/// Catches: `all` left a special target that reaches every host, or answered otherwise than a
/// group that is not there.
#[tokio::test]
async fn a_set_naming_a_missing_group_all_is_refused_like_any_missing_group() {
    let farm = Farm::new("no-all-set");
    farm.group("alpha", &["web"], None);
    farm.group("beta", &["web"], None);
    let before = farm.world.state();
    let body = json!({"service": "web", "version": "2.0"});
    let set = |named: &'static str| {
        let farm = &farm;
        let body = &body;
        async move {
            farm.world
                .coordinator()
                .set_desired_in(
                    &Setter::Operator(&crate::tokens::Operator::named(
                        "operator".parse().expect("a token name"),
                    )),
                    body.to_string().as_bytes(),
                    &WriteAsk::default(),
                    None,
                    named,
                )
                .await
        }
    };

    for named in ["all", "nowhere"] {
        let refused = set(named).await.expect_err("refused");
        assert!(matches!(refused, SetError::NotInHostGroup), "{named}");
        assert_eq!(refused.reason(), Some("not_in_host_group"), "{named}");
    }
    assert_eq!(farm.world.state(), before);

    set("").await.expect("set");
    assert_eq!(
        farm.wanted("alpha", "web").map(|v| v.to_string()),
        Some("2.0".to_owned())
    );
    assert_eq!(
        farm.wanted("beta", "web").map(|v| v.to_string()),
        Some("2.0".to_owned())
    );
}

/// Proves: a host in a group whose name starts with `all` (`all-in-one`) is ordered the packages
/// the group lists, as a host of any group is: the install of its package.
///
/// Catches: a name that starts with the old built-in group's read as it, or a group that orders
/// nothing to its hosts.
#[tokio::test]
async fn a_host_in_all_in_one_still_gets_its_packages() {
    let farm = Farm::new("no-all-in-one");
    farm.group("all-in-one", &["tierstore"], None);
    farm.deploy("all-in-one", "tierstore", "1.0", 1).await;
    farm.host("a1", "all-in-one");

    assert_eq!(
        package_of(farm.order("a1", &[])).as_deref(),
        Some("tierstore")
    );
}
