//! The built-in host group `all` (`docs/design/hostgroups.md`, section 15): every enrolled host is
//! in it, and runs its packages, then those of the one specific group it may be in; a package both
//! list is run as the specific group says. The tests drive heartbeats of enrolled hosts, some in a
//! group and some in none.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::HostGroupsRefusal as Refusal;

use crate::group_decision_support::Farm;
use crate::host_groups_support::{SELF, body, move_of, put_of};
use crate::marks_support::operator;
use crate::orders::Order;

/// A farm that holds `all`, listing hostkeeper and `base`, and a group `web` listing `svc` (not
/// hostkeeper), with the desired versions `base` 1.0 in `all` and `svc` 2.0 in `web`; the host `n1`
/// is in no group and `n2` is in `web`.
async fn farm(name: &str) -> Farm {
    let farm = Farm::new(name);
    farm.world.hold_all();
    farm.world
        .put("all", &[SELF, "base"], Some(1))
        .expect("all is edited");
    farm.world.put("web", &["svc"], None).expect("web is put");
    farm.deploy("all", "base", "1.0", 1).await;
    farm.deploy("web", "svc", "2.0", 2).await;
    farm.lone("n1");
    farm.host("n2", "web");
    farm
}

/// The service and version of an install or update order, if `order` is one.
fn ordered(order: Option<Order>) -> Option<(String, String)> {
    match order? {
        Order::Install {
            service, version, ..
        }
        | Order::Update {
            service, version, ..
        } => Some((service, version.as_str().to_owned())),
        _ => None,
    }
}

fn pair(service: &str, version: &str) -> Option<(String, String)> {
    Some((service.to_owned(), version.to_owned()))
}

/// Proves: a host in no group is ordered the packages of `all`, and a host in `web` is ordered
/// those of `all` first and then those of `web`, each at the version of the group that lists it.
///
/// Catches: a host in a group that runs only its group's packages, or a host in no group that runs
/// nothing, or the packages of the specific group offered before those of `all`.
#[tokio::test]
async fn a_host_runs_the_packages_of_all_and_then_those_of_its_group() {
    let farm = farm("all-union").await;
    assert_eq!(ordered(farm.order("n1", &[])), pair("base", "1.0"));
    assert_eq!(ordered(farm.order("n1", &[("base", "1.0")])), None);
    assert_eq!(ordered(farm.order("n2", &[])), pair("base", "1.0"));
    assert_eq!(
        ordered(farm.order("n2", &[("base", "1.0")])),
        pair("svc", "2.0")
    );
    assert_eq!(
        ordered(farm.order("n2", &[("base", "1.0"), ("svc", "2.0")])),
        None
    );
    assert_eq!(
        ordered(farm.order("n1", &[("base", "1.0")])),
        None,
        "svc is not listed by all"
    );
}

/// Proves: a package both `all` and `web` list is installed once and run as `web` says for the
/// hosts of `web` (its desired version), and as `all` says for the other hosts; `all`'s version of
/// it is not offered to a host of `web`.
///
/// Catches: a host of `web` ordered `all`'s version, or the package listed twice.
#[tokio::test]
async fn a_package_both_list_is_run_as_the_specific_group_says() {
    let farm = farm("all-override").await;
    farm.world
        .put("all", &[SELF, "base", "svc"], Some(2))
        .expect("all lists svc");
    farm.deploy("all", "svc", "1.5", 3).await;
    let have = [("base", "1.0")];
    assert_eq!(ordered(farm.order("n1", &have)), pair("svc", "1.5"));
    assert_eq!(ordered(farm.order("n2", &have)), pair("svc", "2.0"));
    assert_eq!(
        farm.order("n2", &[("base", "1.0"), ("svc", "2.0")]),
        None,
        "installed once"
    );
}

/// Proves: taking a package out of `all` uninstalls it from the hosts that run it and whose group
/// does not list it, and from none that run it as their group's package; moving a host between
/// groups keeps what `all` lists and uninstalls the rest of the old group's packages.
///
/// Catches: an uninstall of a package a host still runs because `all` lists it, or a package of
/// `all` that stays on a host after `all` drops it.
#[tokio::test]
async fn a_change_uninstalls_only_what_a_host_no_longer_runs() {
    let farm = farm("all-uninstall").await;
    farm.world
        .put("all", &[SELF, "base", "svc"], Some(2))
        .expect("all lists svc");
    farm.world
        .put("web", &["svc", "extra"], Some(1))
        .expect("web lists svc and extra");
    for host in ["n1", "n2"] {
        farm.tell(host, &[("base", "1.0"), ("svc", "2.0"), ("extra", "1.0")]);
    }
    farm.world
        .put("all", &[SELF, "base"], Some(3))
        .expect("all drops svc");
    let pending = |host: &str| -> Vec<String> {
        farm.world
            .ops()
            .assignments
            .iter()
            .filter(|(_, entries)| entries.uninstall.contains_key(host))
            .map(|(package, _)| package.clone())
            .collect()
    };
    assert_eq!(pending("n1"), ["svc"], "n1 has no group that lists svc");
    assert_eq!(pending("n2"), Vec::<String>::new(), "web lists svc");

    farm.world.move_to("n2", "web").expect_err("same group");
    farm.world
        .put("db", &["pg"], None)
        .expect("a group db is put");
    farm.world.move_to("n2", "db").expect("moved");
    assert_eq!(pending("n2"), ["extra", "svc"]);
}

/// Proves: `all` cannot be removed, created by a `Put` that names no group of it, or be the group
/// a host is moved into, each refused `reserved_host_group`; `Put` of `all` that replaces it is
/// taken, and must list hostkeeper; a specific group may list none, or no hostkeeper.
///
/// Catches: an `all` an operator can delete or rename, which every host's placement depends on.
#[test]
fn all_is_reserved_and_keeps_hostkeeper() {
    let world = crate::host_groups_support::World::new("all-reserved");
    let coordinator = world.coordinator();
    let new = |name: &str, packages: &[&str], generation| {
        coordinator.host_groups_put(&operator(), put_of(body(name, packages, None), generation))
    };
    assert_eq!(
        new("all", &[SELF], None).expect_err("absent"),
        Refusal::ReservedHostGroup
    );
    world.hold_all();
    assert_eq!(
        new("all", &["svc"], Some(1)).expect_err("no hostkeeper"),
        Refusal::HostkeeperNotListed
    );
    new("all", &[SELF, "base"], Some(1)).expect("replaced");
    assert!(new("empty", &[], None).is_ok());
    assert!(new("plain", &["svc"], None).is_ok());
    let remove = coordinator.host_groups_remove(
        &operator(),
        proto::RemoveHostGroupRequest {
            host_group: "all".to_owned(),
            reason: "no".to_owned(),
        },
    );
    assert_eq!(remove.expect_err("refused"), Refusal::ReservedHostGroup);
    world.enroll("n1");
    assert_eq!(
        coordinator
            .host_groups_move(&operator(), move_of("n1", "all"))
            .expect_err("refused"),
        Refusal::ReservedHostGroup
    );
}

/// Proves: `List` counts every enrolled host in `all` and the hosts of a specific group in it
/// alone, and `Get` of `all` pages every host.
///
/// Catches: an `all` shown with no host, which would read as a group that targets none.
#[tokio::test]
async fn all_counts_every_host() {
    let farm = farm("all-counts").await;
    let coordinator = farm.world.coordinator();
    let listed = coordinator
        .host_groups_list(&operator(), &proto::ListHostGroupsRequest::default())
        .expect("listed");
    let counts: Vec<(String, u32)> = listed
        .host_groups
        .iter()
        .map(|state| (state.group.clone().expect("body").name, state.hosts))
        .collect();
    assert_eq!(counts, [("all".to_owned(), 2), ("web".to_owned(), 1)]);
    let got = coordinator
        .host_groups_get(
            &operator(),
            &proto::GetHostGroupRequest {
                host_group: "all".to_owned(),
                limit: 0,
                page_token: String::new(),
            },
        )
        .expect("got");
    assert_eq!(got.node_ids, ["n1", "n2"]);
}

/// Proves: the convergence of a package `all` lists counts, as one role named `all`, the hosts that
/// run it under `all` (those whose own group does not list it), and not a host of a group that
/// lists it; the group's own pair counts that host; a hold of the role `all` holds every host.
///
/// Catches: a host of `web` counted twice, once at `web`'s version and once at `all`'s, which would
/// keep `all`'s rollout from ever converging, or a pair of `all` counted by the host's own group.
#[tokio::test]
async fn a_package_of_all_is_counted_over_the_hosts_that_run_it_under_all() {
    let farm = farm("all-converge").await;
    farm.world
        .put("all", &[SELF, "base", "svc"], Some(2))
        .expect("all lists svc");
    farm.deploy("all", "svc", "1.5", 3).await;
    farm.tell("n1", &[("base", "1.0"), ("svc", "1.5")]);
    farm.tell("n2", &[("base", "1.0"), ("svc", "2.0")]);
    let coordinator = farm.world.coordinator();
    let counts = |group: &str| -> Vec<(String, usize)> {
        coordinator
            .convergence_in(group, "svc")
            .expect("converges")
            .roles
            .iter()
            .map(|role| (role.role.clone(), role.at_desired))
            .collect()
    };
    assert_eq!(counts("all"), [("all".to_owned(), 1)]);
    assert_eq!(counts("web"), [("web".to_owned(), 1)]);
}

/// Proves: a package that `all` lists and every host's own group lists too has no host under
/// `all`, so its convergence under `all` is that of a group with no hosts (converged), and so is
/// the convergence of a call that names no group, once the hosts of the groups are at the version;
/// a host in no group, which runs it under `all`, keeps `all` in progress until it is at the
/// version.
///
/// Catches: a deployment of hostkeeper to `all` and the groups that never converges, for a package
/// every host runs as its group says.
#[tokio::test]
async fn a_package_every_hosts_group_lists_converges_under_all_without_a_host() {
    let farm = farm("all-converge-none").await;
    farm.world
        .put("web", &[SELF, "svc", "base"], Some(1))
        .expect("web lists base");
    farm.deploy("", "base", "1.5", 3).await;
    let coordinator = farm.world.coordinator();
    let state = |group: &str| {
        coordinator
            .convergence_in(group, "base")
            .expect("converges")
            .state
    };
    farm.tell("n2", &[("base", "1.5"), ("svc", "2.0")]);
    assert_eq!(state("web"), crate::converge::State::Converged);
    assert_eq!(
        state("all"),
        crate::converge::State::InProgress,
        "n1, in no group, runs base under all and does not have it"
    );
    farm.tell("n1", &[("base", "1.5")]);
    assert_eq!(state("all"), crate::converge::State::Converged);
    assert_eq!(state(""), crate::converge::State::Converged);

    let alone = Farm::new("all-converge-grouped");
    alone.world.hold_all();
    alone
        .world
        .put("all", &[SELF, "base"], Some(1))
        .expect("all lists base");
    alone.world.put("web", &[SELF, "base"], None).expect("put");
    alone.deploy("", "base", "1.5", 3).await;
    alone.host("n2", "web");
    alone.tell("n2", &[("base", "1.5")]);
    let coordinator = alone.world.coordinator();
    for group in ["all", "web", ""] {
        assert_eq!(
            coordinator
                .convergence_in(group, "base")
                .expect("converges")
                .state,
            crate::converge::State::Converged,
            "{group:?}"
        );
    }
}
