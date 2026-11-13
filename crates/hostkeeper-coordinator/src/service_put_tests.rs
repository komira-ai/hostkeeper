//! `Services.Put` and `Services.Remove` ([`crate::service_put`]): a package is registered before
//! any host group lists it, shown by `Services.List`, kept by a restart, held in use by a group
//! that lists it, and taken out again.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use crate::group_decision_support::Farm;
use crate::marks_support::operator;
use crate::operator::Caller;
use crate::ops::AuditEntry;
use crate::repositories_support::repository;

type Refusal = proto::ServicesRefusal;

/// Puts the public repository `name` (suite `testing`) in the registry.
fn registry_has(farm: &Farm, name: &str) {
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

/// `Services.Put` of `service` in `repository` with `reason`, as `caller`.
fn put_as(
    farm: &Farm,
    caller: &Caller,
    service: &str,
    repository: &str,
    reason: &str,
) -> Result<proto::PutServiceResponse, Refusal> {
    farm.world.coordinator().put_service(
        caller,
        proto::PutServiceRequest {
            service: service.to_owned(),
            repository: repository.to_owned(),
            reason: reason.to_owned(),
        },
    )
}

/// [`put_as`] the operator.
fn put(farm: &Farm, service: &str, repository: &str) -> Result<proto::PutServiceResponse, Refusal> {
    put_as(farm, &operator(), service, repository, "registered")
}

/// `Services.Remove` of `service` with `reason`, as `caller`.
fn remove_as(
    farm: &Farm,
    caller: &Caller,
    service: &str,
    reason: &str,
) -> Result<proto::RemoveServiceResponse, Refusal> {
    farm.world.coordinator().remove_service(
        caller,
        proto::RemoveServiceRequest {
            service: service.to_owned(),
            reason: reason.to_owned(),
        },
    )
}

/// [`remove_as`] the operator.
fn remove(farm: &Farm, service: &str) -> Result<proto::RemoveServiceResponse, Refusal> {
    remove_as(farm, &operator(), service, "gone")
}

/// What `Services.List` says of `service`, if it lists it.
fn listed(farm: &Farm, service: &str) -> Option<proto::ServiceEntry> {
    farm.world
        .coordinator()
        .list_services(&operator(), proto::ListServicesRequest {})
        .expect("the list")
        .services
        .into_iter()
        .find(|entry| entry.service == service)
}

/// The audit entry `seq` of `action` on `service` by the operator, with `reason`.
fn audited(seq: u64, action: &str, service: &str, reason: &str) -> AuditEntry {
    AuditEntry {
        seq,
        by: "operator".to_owned(),
        action: action.to_owned(),
        service: Some(service.to_owned()),
        node_id: None,
        host_group: None,
        reason: Some(reason.to_owned()),
        subject: None,
        source: None,
    }
}

/// Proves: a package no host group lists and the rollout file does not define is not listed;
/// after `Put` in the registry's repository `acme` it is listed with that repository and no host
/// group, the answer carries the audit entry `service_put` naming it and its repository, and a
/// group that lists it afterwards shows it with the same repository.
///
/// Catches: a registered package that the list leaves out, one shown with the default repository,
/// or an entry that names neither the package nor the repository.
#[test]
fn a_put_package_is_listed_with_its_repository_before_a_group_lists_it() {
    let farm = Farm::new("sp-listed");
    registry_has(&farm, "acme");
    farm.group("alpha", &["cfg-alpha"], None);
    assert!(listed(&farm, "js-agent").is_none());
    let before = farm.world.audit().len() as u64;

    let done = put(&farm, "js-agent", "acme").expect("put");

    let shown = listed(&farm, "js-agent").expect("listed");
    assert_eq!(
        (shown.repository.as_str(), shown.host_groups.is_empty()),
        ("acme", true)
    );
    let want = audited(
        before + 1,
        "service_put",
        "js-agent",
        "repository acme: registered",
    );
    assert_eq!(farm.world.audit_after(before as usize), [want]);
    let entry = done.entry.expect("an entry");
    assert_eq!(
        (entry.service.as_deref(), entry.reason.as_deref()),
        (Some("js-agent"), Some("repository acme: registered"))
    );
    farm.group("beta", &["js-agent"], None);
    let shown = listed(&farm, "js-agent").expect("listed");
    assert_eq!(
        (shown.repository.as_str(), shown.host_groups.as_slice()),
        ("acme", ["beta".to_owned()].as_slice())
    );
}

/// Proves: an empty repository registers the package in the default repository, `komira`, which
/// the registry must hold: without it the call is refused `repository_unknown`.
///
/// Catches: an empty repository stored as an empty name, or a default that is not checked against
/// the registry.
#[test]
fn an_empty_repository_is_the_default_one_and_must_be_held() {
    let farm = Farm::new("sp-default");
    farm.group("alpha", &["cfg-alpha"], None);
    assert_eq!(put(&farm, "tierstore", ""), Err(Refusal::RepositoryUnknown));
    assert!(listed(&farm, "tierstore").is_none());

    registry_has(&farm, "komira");
    put(&farm, "tierstore", "").expect("put");

    assert_eq!(
        listed(&farm, "tierstore").expect("listed").repository,
        "komira"
    );
}

/// Proves: a package a group lists takes a `Put` too, which sets its repository, and registering
/// one that is registered sets its repository again.
///
/// Catches: a `Put` that refuses a package a group already lists, or that keeps the old
/// repository.
#[test]
fn a_put_sets_the_repository_of_a_package_already_registered() {
    let farm = Farm::new("sp-again");
    registry_has(&farm, "acme");
    registry_has(&farm, "komira");
    farm.group("alpha", &["cfg-alpha"], None);

    put(&farm, "cfg-alpha", "acme").expect("put a listed package");
    assert_eq!(
        listed(&farm, "cfg-alpha").expect("listed").repository,
        "acme"
    );
    put(&farm, "tierstore", "acme").expect("put");
    put(&farm, "tierstore", "komira").expect("put again");

    assert_eq!(
        listed(&farm, "tierstore").expect("listed").repository,
        "komira"
    );
}

/// Proves: `Put` is refused, and changes nothing, for a caller that is not the operator (no
/// bearer `unauthorized`, a CI token `forbidden`), a reason that is empty, has a control character
/// or is over 256 bytes, a package name that breaks the grammar (upper case, a space, empty, over
/// 64 bytes), a repository name that does (`invalid_request`) and one the registry lacks
/// (`repository_unknown`).
///
/// Catches: CI or an anonymous caller that registers a package, or a bad field that is stored.
#[test]
fn a_put_that_breaks_a_rule_is_refused_and_changes_nothing() {
    let farm = Farm::new("sp-put-refusals");
    registry_has(&farm, "acme");
    farm.group("alpha", &["cfg-alpha"], None);
    let before = farm.world.state();

    assert_eq!(
        put_as(&farm, &Caller::Anonymous, "tierstore", "acme", "why"),
        Err(Refusal::Unauthorized)
    );
    assert_eq!(
        put_as(&farm, &Caller::Ci, "tierstore", "acme", "why"),
        Err(Refusal::Forbidden)
    );
    let long = "x".repeat(257);
    for reason in ["", "  ", "two\nlines", long.as_str()] {
        assert_eq!(
            put_as(&farm, &operator(), "tierstore", "acme", reason),
            Err(Refusal::InvalidRequest),
            "reason {reason:?}"
        );
    }
    for name in ["Tier", "tier store", "", &"a".repeat(65)] {
        assert_eq!(
            put(&farm, name, "acme"),
            Err(Refusal::InvalidRequest),
            "package {name:?}"
        );
    }
    for name in ["Acme", "acme!", &"a".repeat(33)] {
        assert_eq!(
            put(&farm, "tierstore", name),
            Err(Refusal::InvalidRequest),
            "repository {name:?}"
        );
    }
    assert_eq!(
        put(&farm, "tierstore", "other"),
        Err(Refusal::RepositoryUnknown)
    );

    assert_eq!(farm.world.state(), before);
}

/// Proves: a registered package that no group lists is taken out by `Remove`: `Services.List` no
/// longer shows it, the ops state holds no entry for it, the audit entry is `service_remove`
/// naming it with the operator's reason, and a second `Remove` of it is `unknown_service`.
///
/// Catches: a remove that leaves the package listed or an empty marks entry behind, or one that
/// writes no audit entry.
#[test]
fn a_package_no_group_lists_is_removed() {
    let farm = Farm::new("sp-remove");
    registry_has(&farm, "acme");
    farm.group("alpha", &["cfg-alpha"], None);
    put(&farm, "js-agent", "acme").expect("put");
    let before = farm.world.audit().len() as u64;

    let done = remove(&farm, "js-agent").expect("removed");

    assert!(listed(&farm, "js-agent").is_none());
    assert!(!farm.world.ops().services.contains_key("js-agent"));
    let want = audited(before + 1, "service_remove", "js-agent", "gone");
    assert_eq!(farm.world.audit_after(before as usize), [want]);
    assert_eq!(done.entry.expect("an entry").action, "service_remove");
    assert_eq!(remove(&farm, "js-agent"), Err(Refusal::UnknownService));
}

/// Proves: `Remove` of a package a host group lists is refused `in_use` and changes nothing, so
/// does one the rollout file defines (`web`), whether or not the operator registered it; once the
/// group stops listing it the remove is taken.
///
/// Catches: a package removed from under the group that deploys it.
#[test]
fn a_package_a_group_lists_is_in_use() {
    let farm = Farm::new("sp-in-use");
    registry_has(&farm, "acme");
    farm.group("alpha", &["cfg-alpha"], None);
    put(&farm, "cfg-alpha", "acme").expect("put");
    put(&farm, "web", "acme").expect("put");
    let before = farm.world.state();

    assert_eq!(remove(&farm, "cfg-alpha"), Err(Refusal::InUse));
    assert_eq!(remove(&farm, "web"), Err(Refusal::InUse));
    assert_eq!(farm.world.state(), before);

    farm.world
        .put("alpha", &["hostkeeper"], Some(1))
        .expect("alpha drops cfg-alpha");
    remove(&farm, "cfg-alpha").expect("removed once no group lists it");
    assert!(listed(&farm, "cfg-alpha").is_none());
}

/// Proves: `Remove` is refused, and changes nothing, for a caller that is not the operator, a bad
/// reason, a name that breaks the grammar (`invalid_request`) and a package nothing registers
/// (`unknown_service`), and a package whose other marks stand (here a service of the ops state
/// with no repository) is not one it takes.
///
/// Catches: CI or an anonymous caller that removes a package, or a package registered only by
/// marks that are not a repository.
#[test]
fn a_remove_that_breaks_a_rule_is_refused_and_changes_nothing() {
    let farm = Farm::new("sp-remove-refusals");
    farm.group("alpha", &["cfg-alpha"], None);
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
    let before = farm.world.state();

    assert_eq!(
        remove_as(&farm, &Caller::Anonymous, "tierstore", "why"),
        Err(Refusal::Unauthorized)
    );
    assert_eq!(
        remove_as(&farm, &Caller::Ci, "tierstore", "why"),
        Err(Refusal::Forbidden)
    );
    assert_eq!(
        remove_as(&farm, &operator(), "tierstore", ""),
        Err(Refusal::InvalidRequest)
    );
    assert_eq!(remove(&farm, "Tier Store"), Err(Refusal::InvalidRequest));
    assert_eq!(remove(&farm, "tierstore"), Err(Refusal::UnknownService));
    assert_eq!(remove(&farm, "gone"), Err(Refusal::UnknownService));

    assert_eq!(farm.world.state(), before);
}

/// Proves: a coordinator started over the state the first one committed lists the package the
/// first registered, and not one it removed.
///
/// Catches: a registration that lives in memory only and is gone after a restart.
#[test]
fn a_restart_keeps_the_registered_packages() {
    let farm = Farm::new("sp-restart");
    registry_has(&farm, "acme");
    farm.group("alpha", &["cfg-alpha"], None);
    put(&farm, "js-agent", "acme").expect("put");
    put(&farm, "tierstore", "acme").expect("put");
    remove(&farm, "tierstore").expect("removed");

    let again = Farm::over("sp-restart-again", farm.world.fake.clone());

    assert_eq!(
        listed(&again, "js-agent").expect("listed").repository,
        "acme"
    );
    assert!(listed(&again, "tierstore").is_none());
}
