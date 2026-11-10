//! `Releases.SetDesired` by a CI token whose allow-list entry names `host_groups`
//! ([`crate::service_groups`]): the groups it may deploy to, the set that names a group, the set
//! that names none, the coordinator that holds no group, and the reads it is not limited in.

use serde_json::json;

use crate::group_decision_support::Farm;
use crate::ops::AuditEntry;
use crate::service::ci_calls::Setter;
use crate::service::{Coordinator, Set, SetError};
use crate::testutil::{LEGACY, Rig, ci_token, version};
use crate::write_record::{WriteAsk, WriteRecord};

type Answer = Result<(Set, Option<WriteRecord>), SetError>;

/// CI's set of `web` at `version` in the group `named` (none: empty), with a token no one has
/// used; `if_generation` is the precondition asked, if any.
async fn ci_sets(
    coordinator: &Coordinator,
    version: &str,
    named: &str,
    if_generation: Option<u64>,
) -> Answer {
    let verified = coordinator.ci(Some(&ci_token())).await.expect("a token");
    let body = json!({"service": "web", "version": version}).to_string();
    let ask = WriteAsk {
        if_generation,
        ..WriteAsk::default()
    };
    coordinator
        .set_desired_in(&Setter::Ci(&verified), body.as_bytes(), &ask, None, named)
        .await
}

/// A farm whose token may deploy to the groups `permitted` (the entry's `host_groups` line), with
/// the groups alpha, beta and gamma, each listing `web`.
fn farm(name: &str, permitted: &str) -> Farm {
    let farm = Farm::limited(name, &format!("host_groups = {permitted}\n"));
    for group in ["alpha", "beta", "gamma"] {
        farm.group(group, &["web"], None);
    }
    farm
}

fn set_entries(farm: &Farm) -> Vec<AuditEntry> {
    farm.world
        .audit()
        .into_iter()
        .filter(|entry| entry.action == "set_desired")
        .collect()
}

fn ci_entry(group: &str) -> (String, Option<String>, Option<String>) {
    (
        "ci".to_owned(),
        Some("web".to_owned()),
        Some(group.to_owned()),
    )
}

fn who(entries: &[AuditEntry]) -> Vec<(String, Option<String>, Option<String>)> {
    entries
        .iter()
        .map(|e| (e.by.clone(), e.service.clone(), e.host_group.clone()))
        .collect()
}

/// Proves: a token limited to `["alpha"]` that names alpha sets it: alpha holds the version, one
/// audit entry by `ci` names alpha and the workflow, and the groups it may not deploy to are
/// untouched.
#[tokio::test]
async fn a_limited_token_sets_a_group_it_may_deploy_to() {
    let farm = farm("gci-permitted", "[\"alpha\"]");

    ci_sets(farm.world.coordinator(), "2.0", "alpha", None)
        .await
        .expect("set");

    assert_eq!(farm.wanted("alpha", "web"), Some(version("2.0")));
    assert_eq!(farm.wanted("beta", "web"), None);
    let entries = set_entries(&farm);
    assert_eq!(who(&entries), vec![ci_entry("alpha")]);
    assert_eq!(
        entries[0].reason.as_deref(),
        Some(format!("2.0 set by {}", hostkeeper_oidc::testkit::WORKFLOW).as_str())
    );
}

/// Proves: a limited token that names a group outside its list is `forbidden` and writes nothing,
/// whether the group exists and lists the package (beta) or does not exist at all, and an unknown
/// group inside its list is still `not_in_host_group`: the list is judged before the group is
/// looked up.
#[tokio::test]
async fn a_limited_token_naming_another_group_is_forbidden_before_the_group_is_looked_up() {
    let farm = farm("gci-other", "[\"alpha\", \"absent\"]");
    let audited = farm.world.audit().len();

    for named in ["beta", "missing", "Not A Name"] {
        let refused = ci_sets(farm.world.coordinator(), "2.0", named, None)
            .await
            .expect_err(named);
        assert!(matches!(refused, SetError::Forbidden), "{named}");
    }
    let refused = ci_sets(farm.world.coordinator(), "2.0", "absent", None)
        .await
        .expect_err("absent");
    assert!(matches!(refused, SetError::NotInHostGroup));

    for group in ["alpha", "beta", "gamma"] {
        assert_eq!(farm.wanted(group, "web"), None, "{group}");
    }
    assert_eq!(farm.world.audit().len(), audited);
}

/// Proves: a limited token that names no group deploys to each of its groups that lists the
/// package and to no other: alpha and gamma hold the version, beta keeps what it had, and there is
/// one audit entry by `ci` for each of the two, in name order.
#[tokio::test]
async fn a_limited_token_naming_no_group_deploys_to_its_groups_that_list_the_package() {
    let farm = farm("gci-fan-out", "[\"gamma\", \"alpha\"]");
    farm.world.seed_desired("beta", "web", "1.0");

    ci_sets(farm.world.coordinator(), "2.0", "", None)
        .await
        .expect("set");

    assert_eq!(farm.wanted("alpha", "web"), Some(version("2.0")));
    assert_eq!(farm.wanted("gamma", "web"), Some(version("2.0")));
    assert_eq!(farm.wanted("beta", "web"), Some(version("1.0")));
    assert_eq!(
        who(&set_entries(&farm)),
        vec![ci_entry("alpha"), ci_entry("gamma")]
    );
}

/// Proves: a limited token that names no group is `forbidden` when only groups it may not deploy
/// to list the package, writing nothing; and is `not_in_host_group` when no group lists the
/// package at all.
#[tokio::test]
async fn a_limited_token_naming_no_group_is_forbidden_only_when_another_group_lists_the_package() {
    let farm = Farm::limited("gci-none-permitted", "host_groups = [\"alpha\"]\n");
    farm.group("alpha", &["db"], None);
    farm.group("beta", &["web"], None);
    farm.group("gamma", &["web"], None);
    let audited = farm.world.audit().len();

    let refused = ci_sets(farm.world.coordinator(), "2.0", "", None)
        .await
        .expect_err("only beta and gamma list web");
    assert!(matches!(refused, SetError::Forbidden));
    for group in ["alpha", "beta", "gamma"] {
        assert_eq!(farm.wanted(group, "web"), None, "{group}");
    }
    assert_eq!(farm.world.audit().len(), audited);

    let none = Farm::limited("gci-no-lister", "host_groups = [\"alpha\"]\n");
    none.group("alpha", &["db"], None);
    let refused = ci_sets(none.world.coordinator(), "2.0", "", None)
        .await
        .expect_err("no group lists web");
    assert!(matches!(refused, SetError::NotInHostGroup));
}

/// Proves: `if_generation` with no group counts the groups the token may deploy to that list the
/// package: two of them is `ambiguous_host_group`, one (of three that list it) is accepted and
/// writes that group.
#[tokio::test]
async fn if_generation_without_a_group_counts_the_groups_the_token_may_deploy_to() {
    let two = farm("gci-ambiguous", "[\"alpha\", \"beta\"]");
    let refused = ci_sets(two.world.coordinator(), "2.0", "", Some(0))
        .await
        .expect_err("two permitted groups list web");
    assert!(matches!(refused, SetError::AmbiguousHostGroup));
    assert_eq!(two.wanted("alpha", "web"), None);

    let one = farm("gci-one-counted", "[\"beta\"]");
    ci_sets(one.world.coordinator(), "2.0", "", Some(0))
        .await
        .expect("one permitted group lists web");
    assert_eq!(one.wanted("beta", "web"), Some(version("2.0")));
    assert_eq!(one.wanted("alpha", "web"), None);
}

/// Proves: `host_groups = []` names no group, so every set is `forbidden` with a group named, with
/// none named, and with the group existing; nothing is written.
#[tokio::test]
async fn an_entry_with_no_host_groups_forbids_every_set() {
    let farm = farm("gci-empty", "[]");
    let audited = farm.world.audit().len();

    for named in ["alpha", ""] {
        let refused = ci_sets(farm.world.coordinator(), "2.0", named, None)
            .await
            .expect_err(named);
        assert!(matches!(refused, SetError::Forbidden), "{named:?}");
    }
    assert_eq!(farm.wanted("alpha", "web"), None);
    assert_eq!(farm.world.audit().len(), audited);
}

/// Proves: an entry that names no `host_groups` is not limited: a set naming any group is made,
/// and a set naming none deploys to each group that lists the package.
#[tokio::test]
async fn an_entry_without_host_groups_deploys_to_any_group_that_lists_the_package() {
    let farm = Farm::new("gci-unlimited");
    for group in ["alpha", "beta"] {
        farm.group(group, &["web"], None);
    }

    ci_sets(farm.world.coordinator(), "2.0", "beta", None)
        .await
        .expect("a named group");
    ci_sets(farm.world.coordinator(), "3.0", "", None)
        .await
        .expect("each group");

    assert_eq!(farm.wanted("alpha", "web"), Some(version("3.0")));
    assert_eq!(farm.wanted("beta", "web"), Some(version("3.0")));
    assert_eq!(set_entries(&farm).len(), 3);
}

/// Proves: a coordinator that holds no host group refuses a limited token every set, with a group
/// named and with none, and writes nothing; the same coordinator still accepts the set of a token
/// whose entry names no host groups.
#[tokio::test]
async fn a_limited_token_is_forbidden_on_a_coordinator_that_holds_no_group() {
    let rig = Rig::new("gci-no-groups");
    let limited = format!(
        "{}host_groups = [\"alpha\"]\n",
        hostkeeper_oidc::testkit::config_text(hostkeeper_oidc::testkit::STATIC_ISSUER)
    );
    let coordinator = Rig::start_with_allow_list(&rig.dir, &rig.clock, &limited);
    let body = json!({"service": LEGACY, "version": "2.0"}).to_string();

    for named in ["", "alpha"] {
        let verified = coordinator.ci(Some(&ci_token())).await.expect("a token");
        let refused = coordinator
            .set_desired_in(
                &Setter::Ci(&verified),
                body.as_bytes(),
                &WriteAsk::default(),
                None,
                named,
            )
            .await
            .expect_err(named);
        assert!(matches!(refused, SetError::Forbidden), "{named:?}");
    }
    assert_eq!(coordinator.desired.get(LEGACY), None);

    let unlimited = Rig::start_with_allow_list(
        &rig.dir,
        &rig.clock,
        &hostkeeper_oidc::testkit::config_text(hostkeeper_oidc::testkit::STATIC_ISSUER),
    );
    let verified = unlimited.ci(Some(&ci_token())).await.expect("a token");
    unlimited
        .set_desired_in(
            &Setter::Ci(&verified),
            body.as_bytes(),
            &WriteAsk::default(),
            None,
            "",
        )
        .await
        .expect("an entry without host groups");
    assert_eq!(
        unlimited.desired.get(LEGACY).map(|d| d.version),
        Some(version("2.0"))
    );
}

/// Proves: the reads are not limited by `host_groups`: a limited token reads the convergence of a
/// group it may not deploy to, and of the package across groups.
#[tokio::test]
async fn a_limited_token_reads_the_groups_it_may_not_deploy_to() {
    let farm = farm("gci-reads", "[\"alpha\"]");
    farm.world.seed_desired("beta", "web", "1.0");
    let coordinator = farm.world.coordinator();

    let beta = coordinator
        .convergence_in("beta", "web")
        .expect("beta is read");
    assert_eq!(
        beta.desired,
        Some(version("1.0")),
        "beta's own desired version"
    );
    let across = coordinator
        .convergence(Some(&ci_token()), "web")
        .await
        .expect("the package is read");
    assert_eq!(
        across.desired,
        Some(version("1.0")),
        "the one group with a desired version of web is beta's, which the limited token reads"
    );
}
