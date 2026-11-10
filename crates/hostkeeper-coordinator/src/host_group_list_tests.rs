//! `HostGroups.List` and `HostGroups.Get` ([`crate::operator_host_groups`]): what they answer,
//! the pages, and each refusal. They change nothing.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::HostGroupsRefusal as Refusal;

use crate::host_groups_support::{SELF, World, coordinator_on};
use crate::marks_support::operator;
use crate::operator::Caller;
use crate::operator_host_groups::{MAX_PAGE, page_size};
use crate::store::Fake;
use crate::testutil::Rig;

fn list_of(limit: u32, page_token: &str) -> proto::ListHostGroupsRequest {
    proto::ListHostGroupsRequest {
        limit,
        page_token: page_token.to_owned(),
    }
}

fn get_of(group: &str, limit: u32, page_token: &str) -> proto::GetHostGroupRequest {
    proto::GetHostGroupRequest {
        host_group: group.to_owned(),
        limit,
        page_token: page_token.to_owned(),
    }
}

fn names_of(listed: &proto::ListHostGroupsResponse) -> Vec<&str> {
    listed
        .host_groups
        .iter()
        .map(|state| state.group.as_ref().expect("a body").name.as_str())
        .collect()
}

/// Proves: a request for 0 groups or hosts is a request for the most, a request for more than
/// the most gets the most, and any other number is itself.
///
/// Catches: a page of nothing for the `limit` of 0 that means "the default".
#[test]
fn a_page_is_the_limit_asked_capped_and_zero_is_the_cap() {
    assert_eq!(MAX_PAGE, 256);
    assert_eq!(page_size(0), 256);
    assert_eq!(page_size(1), 1);
    assert_eq!(page_size(256), 256);
    assert_eq!(page_size(257), 256);
    assert_eq!(page_size(u32::MAX), 256);
}

/// Proves: `List` answers each group by name with its body, policy, generation, the number of
/// enrolled hosts in it (those moving into it among them, a denied host and a host in another
/// group not), and per package its desired version where one is set; it answers neither a
/// minimum version nor a convergence, which `Get` does.
///
/// Catches: a count of hosts that includes a retired host or leaves out one that is moving, a
/// desired version attached to the wrong package, or a `List` that answers `Get`'s fields.
#[test]
fn list_answers_each_group_with_its_host_count_and_desired_versions() {
    let world = World::new("hg-list-answers");
    for host in ["d1", "d2", "d3", "d4"] {
        world.enroll(host);
    }
    world.put("beta", &[SELF, "svc"], None).expect("put");
    world
        .put("alpha", &[SELF, "svc", "cfg"], None)
        .expect("put");
    world.seed_desired("alpha", "svc", "1.2");
    world.move_to("d1", "alpha").expect("moved");
    world.move_to("d2", "alpha").expect("moved");
    world.move_to("d3", "alpha").expect("moved");
    world.move_to("d4", "beta").expect("moved");
    world
        .enrollment
        .enrollment
        .store()
        .deny(&hostkeeper_enrollment::names::HostId::parse("d3").expect("a host id"))
        .expect("denied");
    let before = world.state();

    let listed = world
        .coordinator()
        .host_groups_list(&operator(), &list_of(0, ""))
        .expect("listed");
    assert_eq!(names_of(&listed), ["alpha", "beta"]);
    assert_eq!(listed.next_page_token, "");
    let alpha = &listed.host_groups[0];
    assert_eq!((alpha.generation, alpha.hosts), (1, 2));
    assert_eq!(listed.host_groups[1].hosts, 1);
    assert_eq!(
        alpha.packages,
        vec![
            proto::GroupPackage {
                package: SELF.to_owned(),
                ..proto::GroupPackage::default()
            },
            proto::GroupPackage {
                package: "svc".to_owned(),
                desired_version: Some("1.2".to_owned()),
                ..proto::GroupPackage::default()
            },
            proto::GroupPackage {
                package: "cfg".to_owned(),
                ..proto::GroupPackage::default()
            },
        ]
    );
    assert_eq!(world.state(), before);
}

/// Proves: `List` pages by name: a page of 2 of 3 groups names the last of them as the token of
/// the next, whose page holds the third and no token; a page that holds exactly the groups left
/// names no token; the token of the last name of the last page lists nothing; a request for 1
/// walks all three one at a time.
///
/// Catches: a page that repeats the group of its token, skips one, or names a token past the end.
#[test]
fn list_pages_by_name_with_the_last_name_as_the_token() {
    let world = World::new("hg-list-pages");
    for group in ["c-group", "a-group", "b-group"] {
        world.put(group, &[SELF], None).expect("put");
    }
    let coordinator = world.coordinator();
    let page = |limit, token: &str| {
        coordinator
            .host_groups_list(&operator(), &list_of(limit, token))
            .expect("listed")
    };
    let first = page(2, "");
    assert_eq!(names_of(&first), ["a-group", "b-group"]);
    assert_eq!(first.next_page_token, "b-group");
    let second = page(2, &first.next_page_token);
    assert_eq!(names_of(&second), ["c-group"]);
    assert_eq!(second.next_page_token, "");
    let exact = page(3, "");
    assert_eq!(exact.host_groups.len(), 3);
    assert_eq!(exact.next_page_token, "");
    assert_eq!(page(5, "c-group").host_groups.len(), 0);
    let mut walked = Vec::new();
    let mut token = String::new();
    loop {
        let one = page(1, &token);
        walked.extend(names_of(&one).into_iter().map(str::to_owned));
        token = one.next_page_token;
        if token.is_empty() {
            break;
        }
    }
    assert_eq!(walked, ["a-group", "b-group", "c-group"]);
    assert_eq!(page(0, "zzz").host_groups.len(), 0);
}

/// Proves: `List` with no group held answers no group, and a coordinator without enrollment
/// counts no host in a group it holds.
#[test]
fn list_answers_nothing_for_no_group_and_no_hosts_without_enrollment() {
    let world = World::new("hg-list-empty");
    let listed = world
        .coordinator()
        .host_groups_list(&operator(), &list_of(0, ""))
        .expect("listed");
    assert_eq!(listed, proto::ListHostGroupsResponse::default());

    let fake = Fake::new();
    let mut rig = Rig::new("hg-list-no-enrollment");
    rig.coordinator = coordinator_on(&fake, &rig.clock, None);
    rig.at(2_000);
    rig.coordinator
        .host_groups_put(
            &operator(),
            crate::host_groups_support::put_of(
                crate::host_groups_support::body("alpha", &[SELF], None),
                None,
            ),
        )
        .expect("put");
    let listed = rig
        .coordinator
        .host_groups_list(&operator(), &list_of(0, ""))
        .expect("listed");
    assert_eq!(listed.host_groups[0].hosts, 0);
}

/// Proves: `List` and `Get` refuse a CI token `forbidden` and no token `unauthorized`, a page
/// token that is no group name (for `List`) or no node id (for `Get`) and a name that is no group
/// name `invalid_request`, a group not held `unknown_host_group`; each changes nothing.
#[test]
fn list_and_get_refuse_a_caller_a_bad_token_and_an_unknown_group() {
    let world = World::new("hg-list-refusals");
    world.put("alpha", &[SELF], None).expect("put");
    let before = world.state();
    let coordinator = world.coordinator();
    for (caller, refusal) in [
        (Caller::Ci, Refusal::Forbidden),
        (Caller::Anonymous, Refusal::Unauthorized),
    ] {
        assert_eq!(
            coordinator
                .host_groups_list(&caller, &list_of(0, ""))
                .expect_err("refused"),
            refusal
        );
        assert_eq!(
            coordinator
                .host_groups_get(&caller, &get_of("alpha", 0, ""))
                .expect_err("refused"),
            refusal
        );
    }
    for token in ["Bad", "a--b", "has space"] {
        assert_eq!(
            coordinator
                .host_groups_list(&operator(), &list_of(0, token))
                .expect_err("refused"),
            Refusal::InvalidRequest,
            "{token}"
        );
    }
    for (group, token) in [("Alpha", ""), ("", ""), ("alpha", "-bad-"), ("alpha", "UP")] {
        assert_eq!(
            coordinator
                .host_groups_get(&operator(), &get_of(group, 0, token))
                .expect_err("refused"),
            Refusal::InvalidRequest,
            "{group:?} {token:?}"
        );
    }
    assert_eq!(
        coordinator
            .host_groups_get(&operator(), &get_of("beta", 0, ""))
            .expect_err("refused"),
        Refusal::UnknownHostGroup
    );
    assert_eq!(world.state(), before);
}

/// Proves: `Get` answers the group as `List` does, with each package's minimum version as
/// `Services.SetMinVersion` set it, its convergence in the group (in progress while the enrolled
/// hosts have not been heard from), the count
/// of hosts, and a page of the node ids of the hosts in the group, those moving into it among
/// them, by node id, a host in another group and a denied host not.
///
/// Catches: a minimum version of another group's package, or hosts of other groups in the page.
#[test]
fn get_answers_the_group_with_minimum_versions_convergence_and_its_hosts() {
    let world = World::new("hg-get-answers");
    for host in ["d3", "d1", "d2", "d9"] {
        world.enroll(host);
    }
    world.put("alpha", &[SELF, "svc"], None).expect("put");
    world.put("beta", &[SELF, "svc"], None).expect("put");
    world.seed_desired("alpha", "svc", "2.0");
    world.seed_marks("alpha", "svc", |marks| {
        marks.min_version = Some(crate::testutil::version("1.5"));
    });
    world.seed_marks("beta", "svc", |marks| {
        marks.min_version = Some(crate::testutil::version("1.1"));
    });
    for host in ["d3", "d1", "d2"] {
        world.move_to(host, "alpha").expect("moved");
    }
    world.move_to("d9", "beta").expect("moved");
    let coordinator = world.coordinator();
    let got = coordinator
        .host_groups_get(&operator(), &get_of("alpha", 0, ""))
        .expect("got");
    assert_eq!(got.node_ids, ["d1", "d2", "d3"]);
    assert_eq!(got.next_page_token, "");
    let state = got.host_group.expect("the group");
    assert_eq!((state.generation, state.hosts), (1, 3));
    let waiting = |desired: Option<&str>| {
        Some(proto::Convergence {
            desired: desired.map(str::to_owned),
            state: proto::ConvergenceState::InProgress.into(),
            coordinator_commit: crate::BUILD_COMMIT.to_owned(),
            ..proto::Convergence::default()
        })
    };
    assert_eq!(
        state.packages,
        vec![
            proto::GroupPackage {
                package: SELF.to_owned(),
                convergence: waiting(None),
                ..proto::GroupPackage::default()
            },
            proto::GroupPackage {
                package: "svc".to_owned(),
                desired_version: Some("2.0".to_owned()),
                min_version: Some("1.5".to_owned()),
                convergence: waiting(Some("2.0")),
            },
        ]
    );
}

/// Proves: `Get` pages the hosts by node id: a page of 2 of 3 names the last as the token, the
/// next page holds the third and no token, the token of the last host lists none.
#[test]
fn get_pages_the_hosts_by_node_id() {
    let world = World::new("hg-get-pages");
    world.put("alpha", &[SELF], None).expect("put");
    for host in ["d1", "d2", "d3"] {
        world.enroll(host);
        world.move_to(host, "alpha").expect("moved");
    }
    let coordinator = world.coordinator();
    let first = coordinator
        .host_groups_get(&operator(), &get_of("alpha", 2, ""))
        .expect("got");
    assert_eq!(first.node_ids, ["d1", "d2"]);
    assert_eq!(first.next_page_token, "d2");
    let second = coordinator
        .host_groups_get(&operator(), &get_of("alpha", 2, "d2"))
        .expect("got");
    assert_eq!(second.node_ids, ["d3"]);
    assert_eq!(second.next_page_token, "");
    let past = coordinator
        .host_groups_get(&operator(), &get_of("alpha", 2, "d3"))
        .expect("got");
    assert_eq!(past.node_ids, Vec::<String>::new());
    assert_eq!(past.next_page_token, "");
    assert_eq!(past.host_group.expect("the group").hosts, 3);
}

/// Proves: when the records of the enrolled hosts cannot be read, `List`, `Get`, `Put` and
/// `Remove` are `store_failed` and change nothing, and the same calls answer once they can.
///
/// Catches: a count of hosts read as 0 from a failed read, which would let a group be removed
/// from under its hosts.
#[test]
fn unreadable_host_records_are_store_failed_for_each_call_that_reads_them() {
    let world = World::new("hg-list-records-fail");
    world.enroll("d1");
    world.put("alpha", &[SELF], None).expect("put");
    world.move_to("d1", "alpha").expect("moved");
    let before = world.state();
    let coordinator = world.coordinator();
    let fail = || world.records.fail_at(world.records.count() + 1);
    fail();
    assert_eq!(
        coordinator
            .host_groups_list(&operator(), &list_of(0, ""))
            .expect_err("refused"),
        Refusal::StoreFailed
    );
    fail();
    assert_eq!(
        coordinator
            .host_groups_get(&operator(), &get_of("alpha", 0, ""))
            .expect_err("refused"),
        Refusal::StoreFailed
    );
    fail();
    assert_eq!(
        world
            .put("alpha", &[SELF, "svc"], Some(1))
            .expect_err("refused"),
        Refusal::StoreFailed
    );
    fail();
    assert_eq!(
        coordinator
            .host_groups_remove(&operator(), crate::host_groups_support::remove_of("alpha"))
            .expect_err("refused"),
        Refusal::StoreFailed
    );
    world.records.fail_at(0);
    assert_eq!(world.state(), before);
    assert_eq!(
        coordinator
            .host_groups_get(&operator(), &get_of("alpha", 0, ""))
            .expect("got")
            .node_ids,
        ["d1"]
    );
}
