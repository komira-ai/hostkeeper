//! A set with repository entries against the registry of package repositories
//! (`Coordinator::set_desired_with`, packages-repositories.md section 3): the entry's repository
//! must be `<name>-<suite>` of the repository the service names, which the registry must hold; a
//! set without a repository entry is not asked about the registry.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use serde_json::json;

use crate::marks_support::{WEB, operator, rig};
use crate::releases::Targets;
use crate::releases_tests::{archive_targets, entry};
use crate::repositories_support::{komira, repository, repository_entry, targets_of};
use crate::service::SetError;
use crate::service::ci_calls::Setter;
use crate::testutil::Rig;
use crate::write_record::WriteAsk;

/// The operator `alice`'s set of `web` at `version` with `targets`.
async fn set(rig: &Rig, version: &str, targets: Option<Targets>) -> Result<(), SetError> {
    let body = json!({"service": WEB, "version": version, "min_up": {"storage": 0, "frontend": 0}});
    rig.coordinator
        .set_desired_with(
            &Setter::Operator("alice"),
            body.to_string().as_bytes(),
            &WriteAsk::default(),
            targets,
        )
        .await
        .map(drop)
}

fn put(rig: &Rig, repository: hostkeeper_proto::hostkeeper::release::v1::Repository) {
    rig.coordinator
        .repositories_put(
            &operator(),
            proto::PutRepositoryRequest {
                repository: Some(repository),
                reason: "for a test".to_owned(),
            },
        )
        .expect("put");
}

fn files(rig: &Rig) -> Vec<Option<Vec<u8>>> {
    ["desired.json", "desired.json.releases", "desired.json.ops"]
        .iter()
        .map(|name| std::fs::read(rig.dir.path().join(name)).ok())
        .collect()
}

/// Proves: a set with a repository entry is refused `repository_unknown`, with the state, the
/// releases and the ops file as they were, when the registry lacks the repository the service
/// names; once `other` is put, entries naming `other-testing` and `other-stable` are taken. (The
/// built-in `komira` is never lacking: its channels are taken with no repository put.)
///
/// Catches: a release kept for a repository no host can be sent.
#[tokio::test]
async fn a_set_with_a_repository_entry_needs_the_repository_in_the_registry() {
    let rig = rig("release-set-repository-absent", "repository = \"other\"");
    let before = files(&rig);
    let refused = set(
        &rig,
        "2.0",
        Some(targets_of(vec![repository_entry(WEB, "other-testing")])),
    )
    .await
    .expect_err("refused");
    assert!(matches!(refused, SetError::RepositoryUnknown), "{refused}");
    assert_eq!(refused.reason(), Some("repository_unknown"));
    assert_eq!(files(&rig), before);

    put(
        &rig,
        repository("other", "https://o.example.com/", &["testing", "stable"], 2),
    );
    for suite in ["testing", "stable"] {
        let name = format!("other-{suite}");
        let targets = Some(targets_of(vec![repository_entry(WEB, &name)]));
        let version = if suite == "testing" { "2.0" } else { "2.1" };
        set(&rig, version, targets).await.expect("taken");
    }
}

/// Proves: the built-in `komira` is the repository of a service that names none: a set naming its
/// channels is taken with no repository put, and one naming a channel it does not have is refused.
///
/// Catches: a built-in repository the sets do not see, which would make every package's first
/// release wait for an operator's `Put`.
#[tokio::test]
async fn the_builtin_komira_takes_entries_with_no_repository_put() {
    let rig = rig("release-set-repository-builtin", "");
    for (version, channel) in [("2.0", "komira-testing"), ("2.1", "komira-stable")] {
        let targets = Some(targets_of(vec![repository_entry(WEB, channel)]));
        set(&rig, version, targets).await.expect("taken");
    }
    let refused = set(
        &rig,
        "2.2",
        Some(targets_of(vec![repository_entry(WEB, "komira-edge")])),
    )
    .await
    .expect_err("refused");
    assert!(matches!(refused, SetError::RepositoryUnknown), "{refused}");
}

/// Proves: an entry whose `repository` is not a channel of the repository the service names is
/// refused `repository_unknown`: another suite of it, another repository's channel though that
/// one is registered, and any entry of a list whose first entry is fine; and a service that names
/// another repository is judged against that one.
///
/// Catches: an entry naming a channel the host will not have, which its package could not be
/// installed from.
#[tokio::test]
async fn an_entry_must_name_a_channel_of_the_repository_the_service_names() {
    let rig = rig("release-set-repository-channel", "repository = \"other\"");
    put(&rig, komira(1));
    put(
        &rig,
        repository("other", "https://o.example.com/", &["stable"], 2),
    );
    let refused = |entries| async {
        let outcome = set(&rig, "2.0", Some(targets_of(entries))).await;
        assert!(
            matches!(outcome, Err(SetError::RepositoryUnknown)),
            "{outcome:?}"
        );
    };
    // `web` names `other`, whose only suite is `stable`.
    refused(vec![repository_entry(WEB, "komira-testing")]).await;
    refused(vec![repository_entry(WEB, "other-testing")]).await;
    refused(vec![
        repository_entry(WEB, "other"),
        entry(WEB, "tarball", 'c', 3_000),
    ])
    .await;
    set(
        &rig,
        "2.0",
        Some(targets_of(vec![
            repository_entry(WEB, "other-stable"),
            entry(WEB, "tarball", 'c', 3_000),
        ])),
    )
    .await
    .expect("a channel of the repository it names");
}

/// Proves: a set without targets, and a set whose targets hold artifact entries alone, are taken
/// while the registry is empty.
///
/// Catches: a set of an artifact-only service (or a Mac's) refused for a registry it does not use.
#[tokio::test]
async fn a_set_without_a_repository_entry_is_not_asked_about_the_registry() {
    let rig = rig("release-set-repository-none", "");
    set(&rig, "1.0", None).await.expect("a plain set");
    set(&rig, "2.0", Some(archive_targets(WEB)))
        .await
        .expect("artifact entries");
    let listed = rig
        .coordinator
        .repositories_list(&operator(), &proto::ListRepositoriesRequest {})
        .expect("listed");
    assert_eq!(listed.generation, 0);
}
