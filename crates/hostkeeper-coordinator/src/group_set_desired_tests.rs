//! `Releases.SetDesired` for a coordinator that holds a host group ([`crate::service_groups`]):
//! the group named, the groups that list the package when none is named, the refusals that name
//! the group, the audit entries, the write record, and the frozen `POST /v1/desired` as the form
//! that names no group.

use serde_json::{Value, json};

use crate::desired::DesiredError;
use crate::group_decision_support::Farm;
use crate::host_groups::GroupName;
use crate::marks_support::digest;
use crate::ops::{AuditEntry, Halt, HaltOrigin};
use crate::releases_tests::targets;
use crate::service::ci_calls::Setter;
use crate::service::{Set, SetError};
use crate::store::{audit_failure, store_failure};
use crate::testutil::{ci_token, version};
use crate::write_record::{WriteAsk, WriteId, WriteRecord};

type Answer = Result<(Set, Option<WriteRecord>), SetError>;

fn group(name: &str) -> GroupName {
    name.parse().expect("a group name")
}

/// The operator's set of `body` in the group `named` (none: empty).
async fn sets(farm: &Farm, body: &Value, ask: &WriteAsk, named: &str) -> Answer {
    farm.world
        .coordinator()
        .set_desired_in(
            &Setter::Operator("operator"),
            body.to_string().as_bytes(),
            ask,
            None,
            named,
        )
        .await
}

fn two(farm: &Farm) {
    farm.group("alpha", &["web"], None);
    farm.group("beta", &["web"], None);
}

fn deployments(farm: &Farm) -> Vec<AuditEntry> {
    farm.world
        .audit()
        .into_iter()
        .filter(|entry| entry.action == "set_desired")
        .collect()
}

/// The audit entry `seq` of a set of `web` in `group`, by `by`, with `reason`.
fn deployment(seq: u64, by: &str, group: &str, reason: &str) -> AuditEntry {
    AuditEntry {
        seq,
        by: by.to_owned(),
        action: "set_desired".to_owned(),
        service: Some("web".to_owned()),
        node_id: None,
        host_group: Some(group.to_owned()),
        reason: Some(reason.to_owned()),
        subject: None,
        source: None,
    }
}

/// Proves: a set that names a group sets the package in that group alone: one deployment, one
/// audit entry `set_desired` naming the group, the group's desired record written once, the other
/// group untouched; the answer is the group's state and its write record.
///
/// Catches: a named set that reaches every group that lists the package, or an entry that does
/// not name the group.
#[tokio::test]
async fn a_set_that_names_a_group_sets_that_group_alone() {
    let farm = Farm::new("gs-named");
    two(&farm);
    let audited = farm.world.audit().len() as u64;

    let (set, record) = sets(
        &farm,
        &json!({"service": "web", "version": "2.0"}),
        &WriteAsk::default(),
        "alpha",
    )
    .await
    .expect("set");

    assert_eq!(farm.wanted("alpha", "web"), Some(version("2.0")));
    assert_eq!(farm.wanted("beta", "web"), None);
    assert_eq!(set.desired.version, version("2.0"));
    assert!(set.dropped_roles.is_empty());
    let record = record.expect("a record");
    assert_eq!(
        (record.generation, record.replaced, record.write_id),
        (1, None, None)
    );
    assert_eq!(
        farm.world.audit_after(audited as usize),
        vec![deployment(audited + 1, "operator", "alpha", "2.0")]
    );
}

/// Proves: a set that names no group is one deployment to each group that lists the package, in one
/// change: both groups hold the version, one audit entry for each group in the order of their names,
/// the answer is the first group's state and carries no write record; a group that does not list the
/// package is left alone.
///
/// Catches: the set of a package reaching a group that does not list it, or one entry for the two.
#[tokio::test]
async fn a_set_that_names_no_group_deploys_to_each_group_that_lists_the_package() {
    let farm = Farm::new("gs-fan-out");
    two(&farm);
    farm.group("gamma", &["db"], None);
    let audited = farm.world.audit().len() as u64;

    let (set, record) = sets(
        &farm,
        &json!({"service": "web", "version": "2.0"}),
        &WriteAsk::default(),
        "",
    )
    .await
    .expect("set");

    for name in ["alpha", "beta"] {
        assert_eq!(farm.wanted(name, "web"), Some(version("2.0")), "{name}");
    }
    assert_eq!(farm.wanted("gamma", "web"), None);
    assert_eq!(set.desired.version, version("2.0"));
    assert_eq!(record, None);
    assert_eq!(
        farm.world.audit_after(audited as usize),
        vec![
            deployment(audited + 1, "operator", "alpha", "2.0"),
            deployment(audited + 2, "operator", "beta", "2.0"),
        ]
    );
}

/// Proves: when exactly one group lists the package a set that names no group is the deployment to
/// that group and answers its write record, and `if_generation` is read against that group's
/// generation of the package: a stale one is refused `generation_conflict`, naming both numbers,
/// and the right one is taken, raising the generation by one and remembering the version replaced
/// and the writer's id.
///
/// Catches: a precondition read against the package's generation, which no group has.
#[tokio::test]
async fn if_generation_is_the_generation_of_the_one_group_that_lists_the_package() {
    let farm = Farm::new("gs-generation");
    farm.group("alpha", &["web"], None);
    farm.group("beta", &["db"], None);
    let body = json!({"service": "web", "version": "2.0"});
    sets(&farm, &body, &WriteAsk::default(), "")
        .await
        .expect("first");
    let stale = WriteAsk {
        if_generation: Some(5),
        write_id: None,
    };

    let refused = sets(
        &farm,
        &json!({"service": "web", "version": "3.0"}),
        &stale,
        "",
    )
    .await
    .expect_err("refused");
    assert!(matches!(
        refused,
        SetError::Invalid(DesiredError::GenerationConflict { asked: 5, now: 1 })
    ));
    assert_eq!(farm.wanted("alpha", "web"), Some(version("2.0")));

    let ask = WriteAsk {
        if_generation: Some(1),
        write_id: Some(WriteId::parse("run-7").expect("a write id")),
    };
    let (_, record) = sets(
        &farm,
        &json!({"service": "web", "version": "3.0"}),
        &ask,
        "",
    )
    .await
    .expect("taken");
    let record = record.expect("a record");
    assert_eq!(record.generation, 2);
    assert_eq!(
        record.replaced.map(|replaced| replaced.version),
        Some(version("2.0"))
    );
    assert_eq!(
        record.write_id,
        Some(WriteId::parse("run-7").expect("a write id"))
    );
}

/// Proves: `if_generation` with no group is refused `ambiguous_host_group` when two groups list the
/// package, and nothing is written; the same precondition naming a group is read against that
/// group.
///
/// Catches: one generation compared for two groups.
#[tokio::test]
async fn if_generation_with_no_group_is_ambiguous_for_two_groups() {
    let farm = Farm::new("gs-ambiguous");
    two(&farm);
    let before = farm.world.state();
    let ask = WriteAsk {
        if_generation: Some(0),
        write_id: None,
    };

    let refused = sets(
        &farm,
        &json!({"service": "web", "version": "2.0"}),
        &ask,
        "",
    )
    .await
    .expect_err("refused");

    assert!(matches!(refused, SetError::AmbiguousHostGroup));
    assert_eq!(refused.reason(), Some("ambiguous_host_group"));
    assert_eq!(farm.world.state(), before);
    sets(
        &farm,
        &json!({"service": "web", "version": "2.0"}),
        &ask,
        "beta",
    )
    .await
    .expect("named");
}

/// Proves: a package the named group does not list, a group that does not exist, a name that is
/// no group's and a package no group lists are each `not_in_host_group` (the last two with the
/// group's name unreadable `invalid`), and none of them writes anything.
///
/// Catches: a set of a package into a group that does not run it, which would roll it to the
/// group's hosts.
#[tokio::test]
async fn a_package_the_group_does_not_list_is_refused() {
    let farm = Farm::new("gs-not-in");
    farm.group("alpha", &["web"], None);
    farm.group("beta", &["db"], None);
    let before = farm.world.state();
    let web = json!({"service": "web", "version": "2.0"});

    for (body, named) in [
        (&web, "beta"),
        (&web, "nowhere"),
        (&json!({"service": "queue", "version": "2.0"}), ""),
        (&json!({"service": "queue", "version": "2.0"}), "alpha"),
    ] {
        let refused = sets(&farm, body, &WriteAsk::default(), named)
            .await
            .expect_err("refused");
        assert!(matches!(refused, SetError::NotInHostGroup), "{named}");
        assert_eq!(refused.reason(), Some("not_in_host_group"));
    }
    let refused = sets(&farm, &web, &WriteAsk::default(), "Not A Group")
        .await
        .expect_err("refused");
    assert!(matches!(
        refused,
        SetError::Invalid(DesiredError::Malformed(_))
    ));
    assert_eq!(farm.world.state(), before);
}

/// Proves: the limits of the desired state are a group's policy: a set that carries a
/// `max_updating` or a `min_up` that names a role is refused `limits_in_host_group` and writes
/// nothing, and one whose maps are empty is taken.
///
/// Catches: a limit accepted and lost, which would make a policy of a number the policy ignores.
#[tokio::test]
async fn limits_in_a_group_set_are_refused() {
    let farm = Farm::new("gs-limits");
    two(&farm);
    let before = farm.world.state();

    for limits in [
        json!({"max_updating": {"alpha": 2}}),
        json!({"min_up": {"alpha": 1}}),
    ] {
        let mut body = json!({"service": "web", "version": "2.0"});
        body.as_object_mut()
            .expect("an object")
            .extend(limits.as_object().cloned().expect("an object"));
        let refused = sets(&farm, &body, &WriteAsk::default(), "alpha")
            .await
            .expect_err("refused");
        assert!(matches!(refused, SetError::LimitsInHostGroup));
        assert_eq!(refused.reason(), Some("limits_in_host_group"));
    }
    assert_eq!(farm.world.state(), before);

    let empty = json!({"service": "web", "version": "2.0", "max_updating": {}, "min_up": {}});
    sets(&farm, &empty, &WriteAsk::default(), "alpha")
        .await
        .expect("taken");
}

/// Proves: a refusal for any group writes nothing for any group: with a minimum version of the
/// package in `beta` above the version, a set that names no group is refused `below_min_version`
/// though `alpha` would take it, and neither group nor the audit changes; naming `alpha` alone sets it.
///
/// Catches: a fan-out that writes the groups it can before it finds the one it cannot.
#[tokio::test]
async fn a_refusal_for_one_group_changes_no_group() {
    let farm = Farm::new("gs-atomic");
    two(&farm);
    farm.world.seed_marks("beta", "web", |marks| {
        marks.min_version = Some(version("3.0"))
    });
    let before = farm.world.state();
    let body = json!({"service": "web", "version": "2.0"});

    let refused = sets(&farm, &body, &WriteAsk::default(), "")
        .await
        .expect_err("refused");

    assert!(matches!(refused, SetError::BelowMinVersion));
    assert_eq!(farm.world.state(), before);
    sets(&farm, &body, &WriteAsk::default(), "alpha")
        .await
        .expect("alpha alone");
    assert_eq!(farm.wanted("alpha", "web"), Some(version("2.0")));
}

/// Proves: a version marked bad is refused in every group, and a package CI set before with another
/// package digest is `digest_conflict` whichever group it is set in: the first group takes the
/// digest, the second is refused the same version with another.
///
/// Catches: the registry of a package kept per group, which would let one version be two builds.
#[tokio::test]
async fn bad_versions_and_digests_are_the_packages_in_every_group() {
    let farm = Farm::new("gs-package-level");
    two(&farm);
    let with = |n: u8| json!({"service": "web", "version": "2.0", "release_digest": digest(n)});
    sets(&farm, &with(1), &WriteAsk::default(), "alpha")
        .await
        .expect("alpha");

    let refused = sets(&farm, &with(2), &WriteAsk::default(), "beta")
        .await
        .expect_err("refused");
    assert!(matches!(refused, SetError::DigestConflict));
    sets(&farm, &with(1), &WriteAsk::default(), "beta")
        .await
        .expect("beta, same digest");
    assert_eq!(
        farm.world
            .coordinator()
            .desired
            .group_desired(&group("beta"), "web")
            .and_then(|desired| desired.release_digest)
            .map(|digest| digest.to_string()),
        Some(digest(1))
    );

    farm.world
        .coordinator()
        .ops()
        .change(
            crate::ops::Write::BeforeMemory,
            AuditEntry::new("test", "seed"),
            |state| {
                state
                    .services
                    .entry("web".to_owned())
                    .or_default()
                    .bad
                    .push(version("3.0"));
            },
        )
        .expect("seeded");
    for named in ["alpha", "beta"] {
        let refused = sets(
            &farm,
            &json!({"service": "web", "version": "3.0"}),
            &WriteAsk::default(),
            named,
        )
        .await
        .expect_err("refused");
        assert!(matches!(refused, SetError::BadVersion), "{named}");
    }
}

/// Proves: a set with targets keeps the release beside the groups' versions and audits it as a set
/// with targets is, a version set before as another release is refused `release_exists` in any
/// group, and the targets are judged before any group changes.
///
/// Catches: the release of a version kept per group.
#[tokio::test]
async fn a_set_with_targets_keeps_one_release_for_the_package() {
    let farm = Farm::new("gs-targets");
    farm.group("alpha", &["db"], None);
    farm.group("beta", &["db"], None);
    let body = json!({"service": "db", "version": "2.0"});
    let set = |named: &'static str, targets| {
        let farm = &farm;
        let body = &body;
        async move {
            farm.world
                .coordinator()
                .set_desired_in(
                    &Setter::Operator("operator"),
                    body.to_string().as_bytes(),
                    &WriteAsk::default(),
                    Some(targets),
                    named,
                )
                .await
        }
    };

    set("alpha", targets("db")).await.expect("alpha");

    assert!(
        farm.world
            .coordinator()
            .desired
            .release("db", "2.0")
            .is_some()
    );
    assert_eq!(
        farm.world
            .audit()
            .into_iter()
            .filter(|entry| entry.action == "set_desired")
            .map(|entry| entry.host_group)
            .collect::<Vec<_>>(),
        [Some("alpha".to_owned())]
    );
    let other = crate::releases_tests::archive_targets("db");
    let refused = set("beta", other).await.expect_err("refused");
    assert!(matches!(refused, SetError::ReleaseExists));
    assert_eq!(farm.wanted("beta", "db"), None);
    set("beta", targets("db"))
        .await
        .expect("beta, same release");
}

/// Proves: CI's set of a package with no group, the form the frozen `POST /v1/desired` takes, is
/// one deployment to each group that lists it, answers the state's own keys, and is audited by `ci`
/// for each group with the workflow that set it, as no set of CI was audited without targets
/// before; the allow-list still names the package, and a package the token may not set is
/// `forbidden`.
///
/// Catches: CI's set unaudited per group, or the allow-list skipped by the group form.
#[tokio::test]
async fn ci_sets_each_group_that_lists_the_package_and_is_audited() {
    let farm = Farm::new("gs-ci");
    two(&farm);
    let audited = farm.world.audit().len() as u64;
    let body = json!({"service": "web", "version": "2.0"}).to_string();

    let set = farm
        .world
        .coordinator()
        .set_desired_noting(Some(&ci_token()), body.as_bytes())
        .await
        .expect("set");

    assert_eq!(set.desired.version, version("2.0"));
    for name in ["alpha", "beta"] {
        assert_eq!(farm.wanted(name, "web"), Some(version("2.0")), "{name}");
    }
    let reason = format!("2.0 set by {}", hostkeeper_oidc::testkit::WORKFLOW);
    assert_eq!(
        farm.world.audit_after(audited as usize),
        vec![
            deployment(audited + 1, "ci", "alpha", &reason),
            deployment(audited + 2, "ci", "beta", &reason),
        ]
    );
    let refused = farm
        .world
        .coordinator()
        .set_desired_noting(
            Some(&ci_token()),
            json!({"service": "queue", "version": "2.0"})
                .to_string()
                .as_bytes(),
        )
        .await
        .expect_err("refused");
    assert!(matches!(refused, SetError::Forbidden));
}

/// Proves: a set of another version lifts the halt the updater raised in the group it sets, and
/// ends the force of another version that stands there, each audited by the setter naming the
/// group, in the same change as the set; the halt of another group, a hook's halt and the
/// force of the version set stay.
///
/// Catches: a lift of the package's halt in place of the group's, or a force that goes on ordering
/// a version the group no longer wants.
#[tokio::test]
async fn a_set_lifts_the_halt_and_ends_the_force_of_the_group_it_sets() {
    let farm = Farm::new("gs-lift");
    two(&farm);
    let halt = |origin| Halt {
        node_id: "a1".to_owned(),
        check: "artifact_changed".to_owned(),
        target: version("1.5"),
        report: None,
        origin,
    };
    farm.world.seed_marks("alpha", "web", |marks| {
        marks.halt = Some(halt(HaltOrigin::Updater));
        marks.halt_acked = vec![crate::ops::HaltAck {
            node_id: "a1".to_owned(),
            target: version("1.2"),
            report: None,
        }];
        marks.last_good = Some(version("1.0"));
        marks.force = Some(crate::force::ForceRecord {
            version: version("1.5"),
            generation: 1,
            reason: "for the tests".to_owned(),
            by: "operator".to_owned(),
            digest: crate::registry::Digest::parse(&digest(1)).expect("a digest"),
            release_index: None,
            halt_generation: 0,
            targets: crate::force::ForceTargets::Fleet,
            include_held: false,
            hosts: vec!["a1".to_owned()],
        });
    });
    farm.world.seed_marks("beta", "web", |marks| {
        marks.halt = Some(halt(HaltOrigin::Updater))
    });
    farm.group("gamma", &["db"], None);
    farm.world.seed_marks("gamma", "db", |marks| {
        marks.last_good = Some(version("1.0"))
    });
    let audited = farm.world.audit().len();

    sets(
        &farm,
        &json!({"service": "web", "version": "2.0"}),
        &WriteAsk::default(),
        "alpha",
    )
    .await
    .expect("set");

    let ops = farm.world.ops();
    let alpha = ops.group_marks(&group("alpha"), "web");
    assert!(alpha.halt.is_none() && alpha.force.is_none());
    assert_eq!(alpha.halt_generation, 1);
    assert!(ops.group_marks(&group("beta"), "web").halt.is_some());
    let actions: Vec<(String, Option<String>)> = farm
        .world
        .audit_after(audited)
        .into_iter()
        .map(|entry| (entry.action, entry.host_group))
        .collect();
    assert_eq!(
        actions,
        [
            ("set_desired".to_owned(), Some("alpha".to_owned())),
            ("force_ended".to_owned(), Some("alpha".to_owned())),
            ("lift_halt".to_owned(), Some("alpha".to_owned())),
        ]
    );
}

/// Proves: a set the store or the audit cannot write changes nothing, in memory or on disk, and is
/// taken once the store heals: the groups' versions, their records, the registry and the audit
/// are as they were after a refusal.
///
/// Catches: a desired version written before its audit line, or an audit line for a set that
/// failed.
#[tokio::test]
async fn a_set_the_store_cannot_write_changes_nothing() {
    let farm = Farm::new("gs-store");
    two(&farm);
    let before = farm.world.state();
    let body = json!({"service": "web", "version": "2.0", "release_digest": digest(3)});

    for failure in [store_failure as fn() -> _, audit_failure] {
        farm.world.fake.refuse(move |_| Some(failure()));
        let refused = sets(&farm, &body, &WriteAsk::default(), "")
            .await
            .expect_err("refused");
        assert!(matches!(refused, SetError::Ops(_)));
        assert_eq!(farm.world.state(), before);
        assert_eq!(farm.wanted("alpha", "web"), None);
    }
    farm.world.fake.heal();
    sets(&farm, &body, &WriteAsk::default(), "")
        .await
        .expect("healed");
    assert_eq!(deployments(&farm).len(), 2);
}

/// Proves: the body is read as a set of a service is: a body with no `service` is the legacy
/// service's, or malformed when the coordinator has none; a `service` that is not text and a body
/// that is not an object are malformed, each refused with nothing written.
#[tokio::test]
async fn a_body_that_names_no_package_is_malformed() {
    let farm = Farm::new("gs-body");
    farm.group("alpha", &[], None);
    let before = farm.world.state();
    for body in [
        json!({"version": "2.0"}),
        json!({"service": 7, "version": "2.0"}),
        json!([1]),
    ] {
        let refused = sets(&farm, &body, &WriteAsk::default(), "")
            .await
            .expect_err("refused");
        assert!(
            matches!(refused, SetError::Invalid(DesiredError::Malformed(_))),
            "{body}"
        );
    }
    assert_eq!(farm.world.state(), before);
}

/// Proves: the same version set again is one more write, which raises the group's generation and
/// keeps the version the last change of version replaced; a package or a group with no desired
/// version has no record.
///
/// Catches: a repeated set that forgets what the last change replaced, which a writer that lost
/// an answer reads its write by.
#[tokio::test]
async fn a_repeat_of_the_version_is_one_more_write_and_keeps_what_was_replaced() {
    let farm = Farm::new("gs-repeat");
    farm.group("alpha", &["web"], None);
    for at in ["1.0", "2.0", "2.0"] {
        sets(
            &farm,
            &json!({"service": "web", "version": at}),
            &WriteAsk::default(),
            "alpha",
        )
        .await
        .expect("set");
    }

    let desired = &farm.world.coordinator().desired;
    let record = desired
        .group_record(&group("alpha"), "web")
        .expect("a record");
    assert_eq!(record.generation, 3);
    assert_eq!(
        record.replaced.map(|replaced| replaced.version),
        Some(version("1.0"))
    );
    assert_eq!(desired.group_record(&group("alpha"), "none"), None);
    assert_eq!(desired.group_record(&group("nowhere"), "web"), None);
    assert_eq!(desired.group_desired(&group("nowhere"), "web"), None);
    assert!(desired.group_desireds(&group("nowhere")).is_empty());
}

/// Proves: what is not a set is refused as malformed with nothing written: a body that is no JSON,
/// and a version no scheme reads.
#[tokio::test]
async fn a_body_that_is_no_set_is_malformed() {
    let farm = Farm::new("gs-garbage");
    farm.group("alpha", &[], None);
    let before = farm.world.state();

    for body in [&b"not json"[..], br#"{"service":"web","version":"x y"}"#] {
        let refused = farm
            .world
            .coordinator()
            .set_desired_in(
                &Setter::Operator("operator"),
                body,
                &WriteAsk::default(),
                None,
                "alpha",
            )
            .await
            .expect_err("refused");
        assert!(
            matches!(refused, SetError::Invalid(DesiredError::Malformed(_))),
            "{refused}"
        );
    }
    assert_eq!(farm.world.state(), before);
}

/// Proves: the refusals of a set's release hold for a group: targets beside a package digest are
/// refused, a repository entry names a repository the registry must hold, and a release index is
/// audited as the set's release.
///
/// Catches: a release judged for the service alone, which would let a group take a release no host
/// can be sent.
#[tokio::test]
async fn the_refusals_of_a_release_hold_in_a_group() {
    use crate::repositories_support::{repository_entry, targets_of};

    let farm = Farm::new("gs-release");
    farm.group("alpha", &["web"], None);
    let before = farm.world.state();
    let operator = Setter::Operator("operator");
    let set = |body: Value, targets| {
        let farm = &farm;
        let operator = &operator;
        async move {
            farm.world
                .coordinator()
                .set_desired_in(
                    operator,
                    body.to_string().as_bytes(),
                    &WriteAsk::default(),
                    targets,
                    "alpha",
                )
                .await
        }
    };

    let both = json!({"service": "web", "version": "2.0", "release_digest": digest(1)});
    let refused = set(both, Some(targets("web"))).await.expect_err("refused");
    assert!(
        matches!(refused, SetError::Invalid(DesiredError::IndexInvalid(_))),
        "{refused}"
    );
    let plain = json!({"service": "web", "version": "2.0"});
    let entries = targets_of(vec![repository_entry("web", "komira-testing")]);
    let refused = set(plain, Some(entries)).await.expect_err("refused");
    assert!(matches!(refused, SetError::RepositoryUnknown), "{refused}");
    assert_eq!(farm.world.state(), before);

    let text = crate::index_set_support::index_text("web", "2.0", &[("linux-x86_64", 1)]);
    let sha = crate::index_set_support::sha(&text);
    let named = json!({
        "service": "web",
        "version": "2.0",
        "release_index": text,
        "release_index_digest": sha,
    });
    set(named, None).await.expect("a release index");
    assert_eq!(
        deployments(&farm)
            .into_iter()
            .map(|entry| entry.reason)
            .collect::<Vec<_>>(),
        [Some(format!("2.0 with release index {sha}"))]
    );
}

/// Proves: a coordinator that holds no group refuses a set that names one `not_in_host_group`,
/// whatever the adapter above it checked.
#[tokio::test]
async fn a_set_naming_a_group_is_refused_where_no_group_is_held() {
    let rig = crate::marks_support::rig("gs-no-groups", "");

    let refused = rig
        .coordinator
        .set_desired_in(
            &Setter::Operator("operator"),
            json!({"service": "web", "version": "2.0"})
                .to_string()
                .as_bytes(),
            &WriteAsk::default(),
            None,
            "alpha",
        )
        .await
        .expect_err("refused");

    assert!(matches!(refused, SetError::NotInHostGroup));
}

/// Proves: a set with targets of a package that already has the most releases drops the oldest
/// release that no group's last good version keeps, in the same change, and keeps the release just
/// set.
///
/// Catches: the registry's cap dropping a version a group may roll back to, or growing without bound.
#[tokio::test]
async fn a_set_past_the_cap_keeps_the_release_a_group_marked_good() {
    use std::sync::Arc;

    use crate::desired::Held;
    use crate::registry::MAX_REGISTRY;
    use crate::releases::{Release, Targets};
    use crate::store::Fake;

    let mut held = Held::default();
    let old = Arc::new(Release::new(Targets::new(), None));
    held.releases.insert(
        "web".to_owned(),
        (0..MAX_REGISTRY)
            .map(|n| (format!("1.{n}"), Arc::clone(&old)))
            .collect(),
    );
    let farm = Farm::over("gs-cap", Fake::holding_desired(held));
    farm.group("alpha", &["web"], None);
    farm.world.seed_marks("alpha", "web", |marks| {
        marks.last_good = Some(version("1.0"))
    });

    farm.world
        .coordinator()
        .set_desired_in(
            &Setter::Operator("operator"),
            json!({"service": "web", "version": "2.0"})
                .to_string()
                .as_bytes(),
            &WriteAsk::default(),
            Some(targets("web")),
            "alpha",
        )
        .await
        .expect("set");

    let kept = |at: &str| farm.world.coordinator().desired.release("web", at);
    assert!(kept("1.0").is_some(), "kept by alpha's last good");
    assert!(kept("1.1").is_none(), "the oldest no group keeps");
    assert!(kept("1.2").is_some());
    assert!(kept("2.0").is_some());
}
