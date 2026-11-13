//! `Services.SetRepository` ([`crate::service_repository`]): the repository of a package is
//! coordinator state, read back by `Services.List`, kept by a restart, held in use against
//! `Repositories.Remove`, and the one a release of the package must name channels of.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use serde_json::json;

use crate::group_decision_support::{Farm, policy, stages};
use crate::marks_support::{INSTALLER, beat_of, operator, runs, who};
use crate::operator::Caller;
use crate::ops::AuditEntry;
use crate::repositories_support::{
    credential_in, private, repository, repository_entry, targets_of,
};
use crate::repository_push::{FEATURE, PRIVATE_FEATURE};
use crate::service::SetError;
use crate::service::ci_calls::Setter;
use crate::store::{audit_failure, store_failure};
use crate::testutil::id;
use crate::write_record::WriteAsk;

type Refusal = proto::ServicesRefusal;

/// Puts the public repository `name` (suite `testing`) in the registry.
fn put(farm: &Farm, name: &str) {
    farm.world
        .coordinator()
        .repositories_put(
            &operator(),
            proto::PutRepositoryRequest {
                repository: Some(repository(
                    name,
                    &format!("https://{name}.example.com/apt"),
                    &["testing"],
                    3,
                )),
                reason: "for a test".to_owned(),
            },
        )
        .expect("put");
}

/// `Services.SetRepository` of `service` to `repository` with `reason`, as the operator.
fn set_as(
    farm: &Farm,
    caller: &Caller,
    service: &str,
    repository: &str,
    reason: &str,
) -> Result<proto::SetRepositoryResponse, Refusal> {
    farm.world.coordinator().set_repository(
        caller,
        proto::SetRepositoryRequest {
            service: service.to_owned(),
            repository: repository.to_owned(),
            reason: reason.to_owned(),
        },
    )
}

/// [`set_as`] the operator.
fn set(
    farm: &Farm,
    service: &str,
    repository: &str,
) -> Result<proto::SetRepositoryResponse, Refusal> {
    set_as(farm, &operator(), service, repository, "moved")
}

/// The repository `Services.List` shows for `service`.
fn shown(farm: &Farm, service: &str) -> String {
    farm.world
        .coordinator()
        .list_services(&operator(), proto::ListServicesRequest {})
        .expect("the list")
        .services
        .into_iter()
        .find(|entry| entry.service == service)
        .unwrap_or_else(|| panic!("{service} is listed"))
        .repository
}

/// The services `Repositories.List` shows the repository `name` as named by.
fn naming(farm: &Farm, name: &str) -> Vec<String> {
    farm.world
        .coordinator()
        .repositories_list(&operator(), &proto::ListRepositoriesRequest {})
        .expect("the list")
        .repositories
        .into_iter()
        .find(|kept| kept.repository.as_ref().is_some_and(|r| r.name == name))
        .unwrap_or_else(|| panic!("{name} is held"))
        .services
}

/// The audit entry `seq` of a `set_repository` of `service` by the operator, with `reason`.
fn audited(seq: u64, service: &str, reason: &str) -> AuditEntry {
    AuditEntry {
        seq,
        by: "operator".to_owned(),
        action: "set_repository".to_owned(),
        service: Some(service.to_owned()),
        node_id: None,
        host_group: None,
        reason: Some(reason.to_owned()),
        subject: None,
        source: None,
    }
}

/// Proves: the registry's repository `acme` set for `web` is what `Services.List` shows for it
/// (`db` stays `komira`), the answer carries the audit entry `set_repository` naming the service
/// with the reason `repository acme: moved`, and the ring holds that one entry.
///
/// Catches: a repository that is written but still listed as the default, one set for the wrong
/// service, or an entry that does not name the service or the repository.
#[test]
fn a_set_repository_is_read_back_by_the_service_list() {
    let farm = Farm::new("sr-read-back");
    put(&farm, "acme");
    let audited_before = farm.world.audit().len() as u64;
    assert_eq!(shown(&farm, "web"), "komira");

    let done = set(&farm, "web", "acme").expect("set");

    assert_eq!(shown(&farm, "web"), "acme");
    assert_eq!(shown(&farm, "db"), "komira");
    let want = audited(audited_before + 1, "web", "repository acme: moved");
    assert_eq!(farm.world.audit_after(audited_before as usize), [want]);
    let entry = done.entry.expect("an entry");
    assert_eq!(
        (entry.service.as_deref(), entry.reason.as_deref()),
        (Some("web"), Some("repository acme: moved"))
    );
}

/// Proves: in a coordinator that holds a host group, a package only the group lists (the rollout
/// file does not define `cfg-alpha`) takes a repository, `Services.List` shows it with the group
/// that lists it, and a package neither the file nor a group names is `unknown_service`.
///
/// Catches: a config package no rollout file defines that cannot be given a repository, or one
/// that is shown as the default.
#[test]
fn a_package_only_a_group_lists_takes_a_repository() {
    let farm = Farm::new("sr-group-package");
    put(&farm, "acme");
    farm.group("alpha", &["cfg-alpha"], None);

    set(&farm, "cfg-alpha", "acme").expect("set");

    let listed = farm
        .world
        .coordinator()
        .list_services(&operator(), proto::ListServicesRequest {})
        .expect("the list")
        .services
        .into_iter()
        .find(|entry| entry.service == "cfg-alpha")
        .expect("listed");
    assert_eq!(
        (listed.repository.as_str(), listed.host_groups.as_slice()),
        ("acme", ["alpha".to_owned()].as_slice())
    );
    assert_eq!(set(&farm, "nope", "acme"), Err(Refusal::UnknownService));
}

/// Proves: a repository the registry does not hold is refused `repository_unknown`, and nothing
/// changes: not the repository shown, not the ops state, the desired state or the audit ring.
///
/// Catches: a package pointed at a repository no host could be sent, or a refused call that still
/// writes its audit entry.
#[test]
fn a_repository_the_registry_lacks_is_refused_and_changes_nothing() {
    let farm = Farm::new("sr-unknown-repository");
    let before = farm.world.state();

    assert_eq!(set(&farm, "web", "acme"), Err(Refusal::RepositoryUnknown));

    assert_eq!(farm.world.state(), before);
    assert_eq!(shown(&farm, "web"), "komira");
}

/// Proves: a caller that is not the operator is refused before the request is read (no bearer
/// `unauthorized`, a CI token `forbidden`), a reason that is empty, has a control character or is
/// over 256 bytes is `invalid_request`, so is a repository name that breaks the registry's grammar,
/// and a service no one manages is `unknown_service`; each leaves the whole state as it was.
///
/// Catches: CI or an anonymous caller that can repoint a package, or a bad field that is stored.
#[test]
fn a_set_that_breaks_a_rule_is_refused_and_changes_nothing() {
    let farm = Farm::new("sr-refusals");
    put(&farm, "acme");
    let before = farm.world.state();

    assert_eq!(
        set_as(&farm, &Caller::Anonymous, "web", "acme", "why"),
        Err(Refusal::Unauthorized)
    );
    assert_eq!(
        set_as(&farm, &Caller::Ci, "web", "acme", "why"),
        Err(Refusal::Forbidden)
    );
    let long = "x".repeat(257);
    for reason in ["", "  ", "two\nlines", long.as_str()] {
        assert_eq!(
            set_as(&farm, &operator(), "web", "acme", reason),
            Err(Refusal::InvalidRequest),
            "reason {reason:?}"
        );
    }
    for name in ["Acme", "acme!", "acme x", &"a".repeat(33)] {
        assert_eq!(
            set_as(&farm, &operator(), "web", name, "why"),
            Err(Refusal::InvalidRequest),
            "repository {name:?}"
        );
    }
    assert_eq!(
        set_as(&farm, &operator(), "nope", "acme", "why"),
        Err(Refusal::UnknownService)
    );

    assert_eq!(farm.world.state(), before);
}

/// Proves: an empty repository clears the setting: `Services.List` shows `komira` again, the
/// service's marks no longer hold an entry for it, and the audit entry reads `repository komira
/// (the default): back`; clearing a service that has none set still writes its entry, and so does
/// setting the same repository twice.
///
/// Catches: a clear that leaves the old repository in force, an empty marks entry left behind, or
/// a repeated call that writes nothing where `SetMinVersion` writes its entry.
#[test]
fn an_empty_repository_clears_the_setting_and_a_repeat_writes_its_entry() {
    let farm = Farm::new("sr-clear");
    put(&farm, "acme");
    let audited_before = farm.world.audit().len();

    set(&farm, "web", "acme").expect("set");
    set(&farm, "web", "acme").expect("set again");
    assert_eq!(
        farm.world.ops().services["web"].repository.as_deref(),
        Some("acme")
    );
    set_as(&farm, &operator(), "web", "", "back").expect("cleared");
    assert_eq!(shown(&farm, "web"), "komira");
    assert!(!farm.world.ops().services.contains_key("web"));
    set_as(&farm, &operator(), "db", "", "again").expect("cleared with none set");

    let reasons: Vec<Option<String>> = farm
        .world
        .audit_after(audited_before)
        .into_iter()
        .map(|entry| entry.reason)
        .collect();
    let want = [
        "repository acme: moved",
        "repository acme: moved",
        "repository komira (the default): back",
        "repository komira (the default): again",
    ];
    assert_eq!(
        reasons,
        want.iter()
            .map(|r| Some((*r).to_owned()))
            .collect::<Vec<_>>()
    );
}

/// Proves: a coordinator started over the state the first one committed shows the repository the
/// first one set, and a cleared one stays cleared.
///
/// Catches: a repository that lives in memory only and goes back to the default at a restart.
#[test]
fn a_restart_keeps_the_repository() {
    let farm = Farm::new("sr-restart");
    put(&farm, "acme");
    set(&farm, "web", "acme").expect("set");
    set(&farm, "db", "acme").expect("set");
    set_as(&farm, &operator(), "db", "", "back").expect("cleared");

    let again = Farm::over("sr-restart-again", farm.world.fake.clone());

    assert_eq!(shown(&again, "web"), "acme");
    assert_eq!(shown(&again, "db"), "komira");
}

/// Proves: `Repositories.Remove` of a repository a service names is refused `in_use` and the
/// registry lists the service as naming it; once the service is cleared the remove is taken. The
/// default repository (`komira`) is named by a package a host group lists, which the rollout file
/// does not define; it is refused `in_use` like any other.
///
/// Catches: a repository removed from under a package that comes from it, or a package of a host
/// group that is not counted as coming from the default repository.
#[test]
fn a_repository_a_service_names_is_in_use() {
    let farm = Farm::new("sr-in-use");
    put(&farm, "acme");
    farm.group("alpha", &["cfg-alpha"], None);
    set(&farm, "web", "acme").expect("set");
    // A service that has other marks but no repository set names no repository by them.
    farm.world
        .coordinator()
        .ops()
        .change(
            crate::ops::Write::BeforeMemory,
            AuditEntry::new("test", "seed"),
            |state| {
                state
                    .services
                    .entry("gone".to_owned())
                    .or_default()
                    .halt_generation = 1
            },
        )
        .expect("seeded");
    let remove = |name: &str| {
        farm.world.coordinator().repositories_remove(
            &operator(),
            proto::RemoveRepositoryRequest {
                name: name.to_owned(),
                reason: "gone".to_owned(),
            },
        )
    };

    assert_eq!(naming(&farm, "acme"), ["web"]);
    assert_eq!(naming(&farm, "komira"), ["cfg-alpha", "db", "hostkeeper"]);
    assert_eq!(
        remove("acme").map(drop),
        Err(proto::RepositoriesRefusal::InUse)
    );
    // `komira` is an entry like another: services name it, so it is in use.
    assert_eq!(
        remove("komira").map(drop),
        Err(proto::RepositoriesRefusal::InUse)
    );

    set_as(&farm, &operator(), "web", "", "back").expect("cleared");
    remove("acme").expect("taken once no service names it");
}

/// Proves: a repository left set on a package that no host group lists and the coordinator does
/// not define (a group put without it) still holds the repository in use, and `SetRepository`
/// takes the package to clear it, after which the repository can be removed; a package with no
/// repository set that nothing lists is still `unknown_service`.
///
/// Catches: a repository that can never be removed because the only call that clears it refuses
/// the package it is set on.
#[test]
fn a_repository_left_on_a_package_no_group_lists_can_be_cleared() {
    let farm = Farm::new("sr-left");
    put(&farm, "acme");
    farm.world
        .coordinator()
        .ops()
        .change(
            crate::ops::Write::BeforeMemory,
            AuditEntry::new("test", "seed"),
            |state| {
                state
                    .services
                    .entry("left".to_owned())
                    .or_default()
                    .repository = Some("acme".to_owned());
            },
        )
        .expect("seeded");
    let remove = || {
        farm.world.coordinator().repositories_remove(
            &operator(),
            proto::RemoveRepositoryRequest {
                name: "acme".to_owned(),
                reason: "gone".to_owned(),
            },
        )
    };

    assert_eq!(naming(&farm, "acme"), ["left"]);
    assert_eq!(remove().map(drop), Err(proto::RepositoriesRefusal::InUse));
    assert_eq!(set(&farm, "unset", "acme"), Err(Refusal::UnknownService));

    set_as(&farm, &operator(), "left", "", "the package is gone").expect("cleared");
    assert!(
        !farm
            .world
            .coordinator()
            .ops()
            .state()
            .services
            .contains_key("left")
    );
    assert_eq!(set(&farm, "left", "acme"), Err(Refusal::UnknownService));
    remove().expect("taken once no service names it");
}

/// Proves: `Repositories.List` names a repository the registry lacks when a package a host group
/// lists comes from it, and does not name the default `komira`, which the registry holds.
///
/// Catches: a group's package that comes from a repository no one can see is missing, or the
/// repository the registry holds listed as lacking.
#[test]
fn a_group_package_from_a_repository_the_registry_lacks_is_listed_unregistered() {
    let farm = Farm::new("sr-unregistered");
    farm.group("alpha", &["cfg-alpha"], None);
    let listed = || {
        farm.world
            .coordinator()
            .repositories_list(&operator(), &proto::ListRepositoriesRequest {})
            .expect("the list")
            .unregistered
    };
    assert!(listed().is_empty(), "the registry holds komira");
    farm.world
        .coordinator()
        .ops()
        .change(
            crate::ops::Write::BeforeMemory,
            AuditEntry::new("test", "seed"),
            |state| {
                state
                    .services
                    .entry("cfg-alpha".to_owned())
                    .or_default()
                    .repository = Some("elsewhere".to_owned());
            },
        )
        .expect("seeded");
    assert_eq!(listed(), ["elsewhere"]);
}

/// Proves: a store that cannot write refuses the call `store_failed` and one that cannot append
/// the audit line `audit_unavailable`, each leaving the state as it was; the same call is taken
/// once the store is healthy.
///
/// Catches: a repository that stands in memory while its commit failed.
#[test]
fn a_failing_store_refuses_the_call_and_changes_nothing() {
    let farm = Farm::new("sr-store-failure");
    put(&farm, "acme");
    let before = farm.world.state();

    for (failure, refusal) in [
        (store_failure as fn() -> _, Refusal::StoreFailed),
        (audit_failure, Refusal::AuditUnavailable),
    ] {
        farm.world.fake.refuse(move |_| Some(failure()));
        assert_eq!(set(&farm, "web", "acme"), Err(refusal));
        assert_eq!(farm.world.state(), before);
    }
    farm.world.fake.heal();
    set(&farm, "web", "acme").expect("healed");
    assert_eq!(shown(&farm, "web"), "acme");
}

/// The operator's set of the config package in `alpha` at `at` with a release whose one entry is
/// the repository channel `channel`.
async fn deploy_from(farm: &Farm, channel: &str, at: &str) -> Result<(), SetError> {
    let body = json!({"service": "cfg-alpha", "version": at});
    farm.world
        .coordinator()
        .set_desired_in(
            &Setter::Operator("operator"),
            body.to_string().as_bytes(),
            &WriteAsk::default(),
            Some(targets_of(vec![repository_entry("cfg-alpha", channel)])),
            "alpha",
        )
        .await
        .map(drop)
}

/// Proves: a config package set to a private repository of the registry deploys to its group: a
/// set with a release whose entry names that repository's channel for the group
/// (`acme-alpha`) is refused `repository_unknown` before the package has the repository, and
/// after `SetRepository` is accepted and the group's host is ordered to install the package; an
/// entry naming the default repository's channel is then refused.
///
/// Catches: a package of a private repository that no release can name (every package taken for
/// `komira`), or a repository set that does not reach the check a deployment is judged by or the
/// install order of the host.
#[tokio::test]
async fn a_config_package_set_to_a_private_repository_deploys_to_its_group() {
    let farm = Farm::new("sr-deploy");
    let wide = policy(|p| {
        p.max_updating = Some(5);
        p.stages = stages(&[100]);
    });
    farm.group("alpha", &["cfg-alpha"], Some(wide));
    farm.host("a1", "alpha");
    let credential = credential_in(farm.world.rig.dir.path(), "acme-credential", "Bearer x");
    farm.world
        .coordinator()
        .repositories_put(
            &operator(),
            proto::PutRepositoryRequest {
                repository: Some(private(
                    "acme",
                    "https://acme.example.com/apt",
                    &["alpha"],
                    4,
                    &credential,
                )),
                reason: "for a test".to_owned(),
            },
        )
        .expect("put");

    let refused = deploy_from(&farm, "acme-alpha", "1.0").await;
    assert!(
        matches!(refused, Err(SetError::RepositoryUnknown)),
        "{refused:?}"
    );

    set(&farm, "cfg-alpha", "acme").expect("set");
    deploy_from(&farm, "acme-alpha", "1.0")
        .await
        .expect("accepted");
    assert_eq!(
        deploy_from(&farm, "komira-testing", "2.0")
            .await
            .map_err(|e| e.to_string()),
        Err(SetError::RepositoryUnknown.to_string())
    );
    let mut features = INSTALLER.to_vec();
    features.push(PRIVATE_FEATURE);
    let ordered = farm.order_on("a1", beat_of(&features, vec![runs("hostkeeper", "1.0")]));
    let ordered = ordered.expect("an order");
    assert!(
        matches!(&ordered, crate::orders::Order::Install { service, .. } if service == "cfg-alpha"),
        "{ordered:?}"
    );
}

/// Proves: in a coordinator that places by role, a set of `web` with a release whose entry names
/// the channel `acme-testing` is refused `repository_unknown` while `web` comes from `komira`, and
/// accepted once `SetRepository` gave it `acme`.
///
/// Catches: the rollout file's package judged by the repository its definition names rather than
/// the one an operator set.
#[tokio::test]
async fn a_set_of_a_role_placed_service_is_judged_by_the_repository_it_was_given() {
    let farm = Farm::new("sr-role-mode");
    put(&farm, "acme");
    let release = || Some(targets_of(vec![repository_entry("web", "acme-testing")]));
    let body = json!({"service": "web", "version": "1.0"}).to_string();
    let deploy = || async {
        farm.world
            .coordinator()
            .set_desired_with(
                &Setter::Operator("operator"),
                body.as_bytes(),
                &WriteAsk::default(),
                release(),
            )
            .await
    };

    let refused = deploy().await.map(drop).map_err(|e| e.to_string());
    assert_eq!(refused, Err(SetError::RepositoryUnknown.to_string()));
    set(&farm, "web", "acme").expect("set");
    deploy().await.expect("accepted");
}

/// Proves: a host of a group whose packages were given the repository `acme` is sent the entry of
/// `acme` and no longer the entry of `komira`, which it was sent while they came from the default.
///
/// Catches: the repositories a host is sent read from the package's definition instead of the
/// repository an operator set.
#[test]
fn a_host_is_sent_the_repository_its_groups_packages_were_given() {
    let farm = Farm::new("sr-push");
    put(&farm, "acme");
    farm.group("alpha", &["cfg-alpha"], None);
    farm.host("a1", "alpha");
    crate::marks_support::ask(
        &farm.world.rig,
        "a1",
        "storage",
        beat_of(&["heartbeat-v2", FEATURE], vec![]),
    );
    farm.world.coordinator().hear_repositories(
        &who("a1", "storage"),
        &hostkeeper_proto::hostkeeper::agent::v1::HeartbeatRequest {
            features: vec![FEATURE.to_owned()],
            ..Default::default()
        },
    );
    let names = || -> Vec<String> {
        farm.world
            .coordinator()
            .repository_set(&id("a1"))
            .map(|set| set.entries.into_iter().map(|e| e.name).collect())
            .unwrap_or_default()
    };
    assert_eq!(names(), ["komira"]);

    set(&farm, "cfg-alpha", "acme").expect("set");
    set(&farm, "hostkeeper", "acme").expect("set");

    assert_eq!(names(), ["acme"]);
}
