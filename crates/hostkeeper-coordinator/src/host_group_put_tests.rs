//! `HostGroups.Put` ([`crate::operator_host_groups`]): the group it makes, what a replacement
//! keeps and drops, the audit entry and its reason, the uninstalls it orders, each refusal with
//! nothing changed, and a restart that keeps the group. The bounds of the policy's keys are in
//! `host_group_put_policy_tests`.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::HostGroupsRefusal as Refusal;

use crate::host_groups::{GroupName, HostGroup};
use crate::host_groups_support::{
    REASON, SELF, UNIX, World, body, cancelled, coordinator_on, operator_entry, put_of,
};
use crate::marks_support::{NEW_DAEMON, ask, beat_of, operator};
use crate::operator::{Caller, entry_proto};
use crate::store::{audit_failure, store_failure};
use crate::testutil::version;

fn name(text: &str) -> GroupName {
    text.parse().expect("a group name")
}

fn held(world: &World, group: &str) -> HostGroup {
    world
        .coordinator()
        .desired
        .host_group(&name(group))
        .expect("the group")
}

/// Proves: a `Put` of a group that does not exist makes it at generation 1, by the operator's
/// token, at the time the audit reads, listing its packages in the order given with the policy
/// it names, and answers generation 1 and the audit entry `host_group_put` about the group,
/// whose reason says it is new, names the packages and the keys of the policy that are not their
/// default, then the operator's reason; `Get` reads the same back.
///
/// Catches: a group that is not made, a package order that is not kept, an entry with no group
/// or a reason that does not say what the call did.
#[test]
fn a_put_makes_the_group_and_audits_what_it_made() {
    let world = World::new("hg-put-creates");
    let policy = proto::RolloutPolicy {
        max_updating: Some(2),
        min_up: 1,
        ..proto::RolloutPolicy::default()
    };
    let put = world
        .put_with("alpha", &[SELF, "svc", "cfg"], Some(policy), None)
        .expect("put");
    let wanted = operator_entry(
        1,
        "host_group_put",
        Some("alpha"),
        None,
        &format!("new group; packages +hostkeeper +svc +cfg; policy max_updating min_up: {REASON}"),
    );
    assert_eq!(put.generation, 1);
    assert_eq!(put.entry, Some(entry_proto(&wanted)));
    assert_eq!(world.audit(), vec![wanted]);

    let group = held(&world, "alpha");
    assert_eq!(
        (group.packages, group.generation, group.by, group.at_unix),
        (
            vec![SELF.to_owned(), "svc".to_owned(), "cfg".to_owned()],
            1,
            "operator".to_owned(),
            UNIX
        )
    );
    assert_eq!((group.policy.max_updating, group.policy.min_up), (2, 1));
    assert!(group.desired.is_empty());
    let got = world
        .coordinator()
        .host_groups_get(
            &operator(),
            &proto::GetHostGroupRequest {
                host_group: "alpha".to_owned(),
                ..proto::GetHostGroupRequest::default()
            },
        )
        .expect("got");
    assert_eq!(got.host_group.expect("the group").generation, 1);
}

/// Proves: the reason of the entry of each `Put` of one group says what changed, in order: the
/// packages added (`+`, in the new order) and taken out (`-`, in the old), `reordered` for packages
/// kept in another order, the keys of the policy that changed, `no change` for a body the group
/// already has; a group made with the default policy names no key; and each `Put` raises the
/// generation by one.
///
/// Catches: a reason that says nothing of a reorder or of a policy change, one that lists a key
/// that did not change, or a generation that does not move on a body that changes nothing.
#[test]
fn the_reason_of_each_put_says_what_changed() {
    let world = World::new("hg-put-reasons");
    let min_up = |n| proto::RolloutPolicy {
        min_up: n,
        ..proto::RolloutPolicy::default()
    };
    let steps: [(&[&str], Option<proto::RolloutPolicy>, &str); 6] = [
        (
            &[SELF, "svc", "cfg"],
            None,
            "new group; packages +hostkeeper +svc +cfg",
        ),
        (
            &[SELF, "svc"],
            Some(min_up(1)),
            "packages -cfg; policy min_up",
        ),
        (&["svc", SELF], Some(min_up(1)), "reordered"),
        (&["svc", SELF], Some(min_up(1)), "no change"),
        (&["svc", SELF, "db"], None, "packages +db; policy min_up"),
        (&[SELF, "other"], None, "packages +other -svc -db"),
    ];
    let mut generation = None;
    for (n, (packages, policy, said)) in steps.into_iter().enumerate() {
        let put = world
            .put_with("alpha", packages, policy, generation)
            .expect("put");
        generation = Some(put.generation);
        assert_eq!(put.generation, n as u64 + 1);
        let entry = world.audit().pop().expect("an entry");
        assert_eq!(
            entry,
            operator_entry(
                n as u64 + 1,
                "host_group_put",
                Some("alpha"),
                None,
                &format!("{said}: {REASON}")
            )
        );
    }
}

/// Proves: a `Put` that replaces a group keeps its desired version of each package it still lists
/// and the marks of those packages, drops both for a package it no longer lists but the minimum
/// version of that package, and leaves another group's alone; the group's `by` and time are the
/// replacing call's.
///
/// Catches: a replacement that forgets a desired version (so the group stops converging), one that
/// keeps a halt of a package no longer in the group, or one that resets a minimum version the
/// operator set.
#[test]
fn a_replacement_keeps_what_belongs_to_the_packages_it_lists() {
    let world = World::new("hg-put-replaces");
    world
        .put("alpha", &[SELF, "svc", "cfg"], None)
        .expect("put");
    world.put("beta", &[SELF, "cfg"], None).expect("put");
    for (group, package, wanted) in [
        ("alpha", "svc", "1.0"),
        ("alpha", "cfg", "2.0"),
        ("beta", "cfg", "3.0"),
    ] {
        world.seed_desired(group, package, wanted);
        world.seed_marks(group, package, |marks| {
            marks.last_good = Some(version(wanted));
            marks.force_generation = 4;
        });
    }
    world.seed_marks("alpha", "cfg", |marks| {
        marks.min_version = Some(version("1.5"));
    });

    world.put("alpha", &[SELF, "svc"], Some(1)).expect("put");

    let group = held(&world, "alpha");
    assert_eq!(group.generation, 2);
    assert_eq!(group.desired.keys().collect::<Vec<_>>(), ["svc"]);
    assert_eq!(group.desired["svc"].version, version("1.0"));
    let ops = world.ops();
    let alpha = name("alpha");
    assert_eq!(
        ops.group_marks(&alpha, "svc").last_good,
        Some(version("1.0"))
    );
    assert_eq!(ops.group_marks(&alpha, "svc").force_generation, 4);
    let dropped = ops.group_marks(&alpha, "cfg");
    assert_eq!(dropped.last_good, None);
    assert_eq!(dropped.force_generation, 0);
    assert_eq!(dropped.min_version, Some(version("1.5")));
    let beta = name("beta");
    assert_eq!(
        ops.group_marks(&beta, "cfg").last_good,
        Some(version("3.0"))
    );
    assert_eq!(held(&world, "beta").desired["cfg"].version, version("3.0"));
}

/// Proves: a `Put` that takes packages out of a group orders each to be uninstalled from the
/// hosts of the group whose last heartbeat lists it, one intent each, the package listed last of
/// those taken out first, their generations above the highest any package has been given; a host
/// of another group, a host in none, a host that did not list the package and a package the
/// group never listed get none; and the entry is the only one written.
///
/// Catches: an uninstall ordered of a package the host never ran or one a hand installed, a host
/// that is not in the group asked to uninstall, or a config package uninstalled after its service.
#[test]
fn a_put_orders_the_removed_packages_uninstalled_in_reverse_list_order() {
    let world = World::new("hg-put-intents");
    for host in ["d1", "d2", "d3", "d4"] {
        world.enroll(host);
    }
    world
        .put("alpha", &[SELF, "svc", "cfg"], None)
        .expect("put");
    world.put("beta", &[SELF, "svc", "cfg"], None).expect("put");
    world.move_to("d1", "alpha").expect("moved");
    world.move_to("d2", "alpha").expect("moved");
    world.move_to("d4", "beta").expect("moved");
    world.beat("d1", &[SELF, "svc", "cfg", "extra"]);
    world.beat("d2", &[SELF, "svc"]);
    world.beat("d3", &[SELF, "svc", "cfg"]);
    world.beat("d4", &[SELF, "svc", "cfg"]);
    world
        .coordinator()
        .ops()
        .change(
            crate::ops::Write::BeforeMemory,
            crate::ops::AuditEntry::new("test", "seed"),
            |state| {
                state
                    .assignments
                    .entry("web".to_owned())
                    .or_default()
                    .uninstall_generation = 5;
            },
        )
        .expect("seeded");
    let audited = world.audit().len();

    world.put("alpha", &[SELF], Some(1)).expect("put");

    assert_eq!(
        world.intents(),
        [
            ("cfg".to_owned(), "d1".to_owned(), 6),
            ("svc".to_owned(), "d1".to_owned(), 7),
            ("svc".to_owned(), "d2".to_owned(), 7),
        ]
    );
    assert_eq!(world.ops().assignments["cfg"].uninstall_generation, 6);
    assert_eq!(world.ops().assignments["svc"].uninstall_generation, 7);
    assert_eq!(world.audit_after(audited).len(), 1);
}

/// Proves: a `Put` that lists a package again cancels the uninstall intents of the group's hosts
/// for it, one `uninstall_cancelled` entry by the coordinator for each after the `Put`'s own, and
/// leaves the intents of the packages still out; a host that settles an intent by reporting the
/// package gone is audited `uninstalled`, as an `Unassign`'s is.
///
/// Catches: a package put back that is uninstalled from the hosts all the same.
#[test]
fn a_put_that_lists_a_package_again_cancels_its_intents() {
    let world = World::new("hg-put-cancels");
    for host in ["d1", "d2"] {
        world.enroll(host);
    }
    world
        .put("alpha", &[SELF, "svc", "cfg"], None)
        .expect("put");
    world.move_to("d1", "alpha").expect("moved");
    world.move_to("d2", "alpha").expect("moved");
    world.beat("d1", &[SELF, "svc", "cfg"]);
    world.beat("d2", &[SELF, "svc", "cfg"]);
    world.put("alpha", &[SELF], Some(1)).expect("put");
    assert_eq!(world.intents().len(), 4);
    let audited = world.audit().len();

    world.put("alpha", &[SELF, "svc"], Some(2)).expect("put");

    assert_eq!(
        world.audit_after(audited),
        vec![
            operator_entry(
                audited as u64 + 1,
                "host_group_put",
                Some("alpha"),
                None,
                &format!("packages +svc: {REASON}")
            ),
            cancelled(audited as u64 + 2, "svc", "d1"),
            cancelled(audited as u64 + 3, "svc", "d2"),
        ]
    );
    assert_eq!(
        world.intents(),
        [
            ("cfg".to_owned(), "d1".to_owned(), 1),
            ("cfg".to_owned(), "d2".to_owned(), 1),
        ]
    );

    let gone = beat_of(NEW_DAEMON, vec![crate::marks_support::runs(SELF, "1.0")]);
    ask(&world.rig, "d2", "storage", gone);
    let last = world.audit().pop().expect("an entry");
    assert_eq!(
        (last.by.as_str(), last.action.as_str()),
        ("coordinator", "uninstalled")
    );
    assert_eq!(
        (last.service.as_deref(), last.node_id.as_deref()),
        (Some("cfg"), Some("d2"))
    );
}

/// Proves: every refusal of `Put` is its token and changes nothing (what the store holds, the
/// audit and every host's group are as they were): CI's token `forbidden` and none
/// `unauthorized`; a reason that is empty, over 256 bytes or holds a control character, and a
/// body that is absent, `invalid_request`; a name that breaks the grammar `invalid_host_group`; an
/// empty list or one without `hostkeeper` `hostkeeper_not_listed`; 65 packages `too_many_packages`
/// where 64 are taken; a name that is no package's or one listed twice `invalid_package`; and a
/// replacement without `if_generation`, with a stale one, or a new group with one,
/// `generation_mismatch`.
///
/// Catches: a refusal that leaves a half-written group, or one whose token is another's.
#[test]
fn every_refusal_of_a_put_changes_nothing() {
    let world = World::new("hg-put-refusals");
    world.put("alpha", &[SELF, "svc"], None).expect("put");
    let before = world.state();
    let coordinator = world.coordinator();
    let put = |caller: &Caller, request: proto::PutHostGroupRequest| {
        coordinator
            .host_groups_put(caller, request)
            .expect_err("refused")
    };
    let with_reason = |reason: &str| proto::PutHostGroupRequest {
        reason: reason.to_owned(),
        ..put_of(body("beta", &[SELF], None), None)
    };
    let new = |name: &str, packages: &[&str]| put_of(body(name, packages, None), None);

    assert_eq!(put(&Caller::Ci, new("beta", &[SELF])), Refusal::Forbidden);
    assert_eq!(
        put(&Caller::Anonymous, new("beta", &[SELF])),
        Refusal::Unauthorized
    );
    let long = "x".repeat(257);
    for reason in ["", "  ", long.as_str(), "bad\nreason"] {
        assert_eq!(
            put(&operator(), with_reason(reason)),
            Refusal::InvalidRequest,
            "{reason:?}"
        );
    }
    let no_body = proto::PutHostGroupRequest {
        host_group: None,
        ..new("beta", &[SELF])
    };
    assert_eq!(put(&operator(), no_body), Refusal::InvalidRequest);
    let wide = "a".repeat(33);
    for bad in ["", "Beta", "a--b", "1a", "b-", wide.as_str()] {
        assert_eq!(
            put(&operator(), new(bad, &[SELF])),
            Refusal::InvalidHostGroup,
            "{bad:?}"
        );
    }
    for packages in [&[][..], &["svc"][..]] {
        assert_eq!(
            put(&operator(), new("beta", packages)),
            Refusal::HostkeeperNotListed,
            "{packages:?}"
        );
    }
    let sixty_five: Vec<String> = std::iter::once(SELF.to_owned())
        .chain((1..65).map(|n| format!("p{n}")))
        .collect();
    fn refs(list: &[String]) -> Vec<&str> {
        list.iter().map(String::as_str).collect()
    }
    assert_eq!(
        put(&operator(), new("beta", &refs(&sixty_five))),
        Refusal::TooManyPackages
    );
    let overlong = "p".repeat(256);
    for packages in [
        vec![SELF, "bad name"],
        vec![SELF, ""],
        vec![SELF, overlong.as_str()],
        vec![SELF, "svc", "svc"],
    ] {
        assert_eq!(
            put(&operator(), new("beta", &packages)),
            Refusal::InvalidPackage,
            "{packages:?}"
        );
    }
    for generation in [None, Some(7), Some(0)] {
        assert_eq!(
            put(
                &operator(),
                put_of(body("alpha", &[SELF], None), generation)
            ),
            Refusal::GenerationMismatch,
            "{generation:?}"
        );
    }
    assert_eq!(
        put(&operator(), put_of(body("beta", &[SELF], None), Some(1))),
        Refusal::GenerationMismatch
    );
    assert_eq!(world.state(), before);

    let sixty_four = &sixty_five[..64];
    world
        .put("beta", &refs(sixty_four), None)
        .expect("64 packages are a list");
}

/// Proves: a commit that fails is `store_failed` and one whose audit line cannot be kept
/// `audit_unavailable`, each leaving the groups, the audit and the hosts as they were; once the
/// store takes commits again the same `Put` lands.
///
/// Catches: a group in memory that the store does not hold.
#[test]
fn a_commit_that_fails_changes_nothing_and_a_resend_lands() {
    let world = World::new("hg-put-commit-fails");
    world.put("alpha", &[SELF], None).expect("put");
    let before = world.state();
    for (failure, refusal) in [
        (store_failure as fn() -> _, Refusal::StoreFailed),
        (audit_failure, Refusal::AuditUnavailable),
    ] {
        world.fake.refuse(move |_| Some(failure()));
        assert_eq!(
            world
                .put("alpha", &[SELF, "svc"], Some(1))
                .expect_err("refused"),
            refusal
        );
        assert_eq!(world.state(), before);
        assert_eq!(held(&world, "alpha").generation, 1);
    }
    world.fake.heal();
    assert_eq!(
        world
            .put("alpha", &[SELF, "svc"], Some(1))
            .expect("put")
            .generation,
        2
    );
}

/// Proves: a coordinator started again on what the store holds has the group, its generation and
/// its policy, and its next `Put` is the generation after.
///
/// Catches: a group kept in memory only.
#[test]
fn a_restart_keeps_the_group() {
    let world = World::new("hg-put-restart");
    let policy = proto::RolloutPolicy {
        live_rollouts: Some(3),
        ..proto::RolloutPolicy::default()
    };
    world
        .put_with("alpha", &[SELF, "svc"], Some(policy), None)
        .expect("put");
    let again = coordinator_on(&world.fake, &world.rig.clock, None);
    let kept = again.desired.host_group(&name("alpha")).expect("kept");
    assert_eq!(kept, held(&world, "alpha"));
    let next = again
        .host_groups_put(
            &operator(),
            put_of(body("alpha", &[SELF, "svc", "cfg"], None), Some(1)),
        )
        .expect("put");
    assert_eq!(next.generation, 2);
}
