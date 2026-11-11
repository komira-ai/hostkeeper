//! The ruling end to end: a store that placed by roles and a rollout file whose package comes
//! from a private repository is migrated (`--apply`, on files and on Postgres); a coordinator
//! built on the migrated store as `serve` builds it (places by groups only, defines no service of
//! its own) shows that repository, judges a release by it, and keeps `Services.SetRepository`,
//! `Repositories` and `Audit.List` consistent with what the migration wrote.

use std::sync::Arc;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use serde_json::json;

use crate::audit_file::UnixClock;
use crate::desired::DesiredStore;
use crate::lock::FileLock;
use crate::marks_support::operator;
use crate::migrate::migrate;
use crate::migrate_support::NOW;
use crate::migrate_tests::{Disk, clock};
use crate::ops::OpsStore;
use crate::repositories_support::{repository_entry, targets_of};
use crate::service::ci_calls::Setter;
use crate::service::{Coordinator, SetError};
use crate::services::Services;
use crate::store::postgres::transfer::{Moved, read_only};
use crate::store::{Fake, Shared};
use crate::testutil::Rig;
use crate::transfer::read_files;
use crate::verdict_file::VerdictFile;
use crate::write_record::WriteAsk;

/// A coordinator over what `moved` holds, built as `serve` builds one: by groups only, defining
/// no service, past its quiet start. Its changes go to `fake`.
fn serve_built(name: &str, moved: &Moved, fake: &Fake) -> (Rig, Coordinator) {
    let loaded = moved.loaded.clone();
    let backend: Shared = Arc::new(fake.clone());
    let list = format!(
        "{}services = [\"web\"]\n",
        hostkeeper_oidc::testkit::config_text(hostkeeper_oidc::testkit::STATIC_ISSUER)
    );
    let unix: UnixClock = Arc::new(|| NOW);
    let rig = Rig::new(name);
    let coordinator = Rig::start_over(
        DesiredStore::over(Arc::clone(&backend), loaded.desired).by_groups_only(),
        &rig.clock,
        &list,
    )
    .with_services(Services::default())
    .with_legacy_service(None)
    .with_ops(OpsStore::over(Arc::clone(&backend), loaded.ops).with_unix(unix))
    .with_verdicts(VerdictFile::over(backend, loaded.verdicts));
    rig.at(2_000);
    (rig, coordinator)
}

/// The repository `Services.List` shows for `service`.
fn shown(coordinator: &Coordinator, service: &str) -> Option<String> {
    coordinator
        .list_services(&operator(), proto::ListServicesRequest {})
        .expect("the list")
        .services
        .into_iter()
        .find(|entry| entry.service == service)
        .map(|entry| entry.repository)
}

/// The operator's set of `kbf-config` at `at` in the group `web` with a release whose one entry
/// is the repository channel `channel`.
async fn deploy(coordinator: &Coordinator, channel: &str, at: &str) -> Result<(), SetError> {
    let body = json!({"service": "kbf-config", "version": at});
    coordinator
        .set_desired_in(
            &Setter::Operator("operator"),
            body.to_string().as_bytes(),
            &WriteAsk::default(),
            Some(targets_of(vec![repository_entry("kbf-config", channel)])),
            "web",
        )
        .await
        .map(drop)
}

fn remove(
    coordinator: &Coordinator,
    name: &str,
) -> Result<proto::RemoveRepositoryResponse, proto::RepositoriesRefusal> {
    coordinator.repositories_remove(
        &operator(),
        proto::RemoveRepositoryRequest {
            name: name.to_owned(),
            reason: "gone".to_owned(),
        },
    )
}

/// What every coordinator built on a migrated store answers, whatever the store.
async fn answers_as_the_ruling_says(name: &str, moved: &Moved) {
    let fake = Fake::new();
    let (_rig, coordinator) = serve_built(name, moved, &fake);

    assert_eq!(
        shown(&coordinator, "kbf-config").as_deref(),
        Some("private")
    );
    assert_eq!(shown(&coordinator, "kbf").as_deref(), Some("komira"));
    let refused = deploy(&coordinator, "komira-testing", "1.0").await;
    assert!(
        matches!(refused, Err(SetError::RepositoryUnknown)),
        "{refused:?}"
    );
    deploy(&coordinator, "private-testing", "1.0")
        .await
        .expect("a release naming the private channel is accepted");
    assert_eq!(
        remove(&coordinator, "private").map(drop),
        Err(proto::RepositoriesRefusal::InUse)
    );
    let set = |service: &str, repository: &str| {
        coordinator.set_repository(
            &operator(),
            proto::SetRepositoryRequest {
                service: service.to_owned(),
                repository: repository.to_owned(),
                reason: "after the migration".to_owned(),
            },
        )
    };
    assert_eq!(
        set("ghost", "private").map(drop),
        Err(proto::ServicesRefusal::UnknownService)
    );
    let written = set("kbf-config", "").expect("cleared");
    assert_eq!(written.entry.expect("an entry").action, "set_repository");
    assert_eq!(shown(&coordinator, "kbf-config").as_deref(), Some("komira"));
    remove(&coordinator, "private").expect("taken once no package names it");
    assert_eq!(
        fake.audit()
            .iter()
            .map(|line| line.action.as_str())
            .collect::<Vec<_>>(),
        ["set_desired", "set_repository", "repository_remove"]
    );
}

/// Proves: after `migrate --apply` on a file store whose rollout file gave `kbf-config` the
/// private repository, a coordinator built on the store as `serve` builds it (by groups only, no
/// service of its own) lists the package from `private` and `kbf` from `komira`; a set of the
/// package in the migrated group with a release naming `private-testing` is accepted and one
/// naming `komira-testing` is refused `repository_unknown`; `Repositories.Remove` of `private` is
/// refused in use while the package names it and allowed after `SetRepository` cleared it; a
/// package no group lists is refused `unknown_service`.
///
/// Catches: a migration whose repository lives where the coordinator does not read it (the whole
/// package taken for `komira` once the rollout file is gone), or readers that take the empty
/// `Services` of `serve` for the answer.
#[tokio::test]
async fn a_migrated_file_store_keeps_the_private_repository_of_its_package() {
    let disk = Disk::new("roundtrip-files");
    migrate(&disk.flags(true), &FileLock, &clock()).expect("applied");
    let moved = read_files(&disk.paths(), None, NOW).expect("the store reads");

    answers_as_the_ruling_says("roundtrip-files-coordinator", &moved).await;
}

/// Proves: the same as the file store's round trip, over a Postgres store that was imported from
/// the files and migrated with `--apply`.
///
/// Catches: a database migration that writes the repository to a column the store's reader does
/// not map, or loses it between the transaction and the next start.
#[test]
fn a_migrated_database_keeps_the_private_repository_of_its_package() {
    let served = crate::migrate_pg_tests::Served::new("roundtrip-database");
    migrate(&served.flags(true), &FileLock, &clock()).expect("applied");
    let moved = read_only(&served.database.dsn(), NOW).expect("the database reads");

    // The database's driver blocks, so it is not used inside the runtime: the store is read
    // first, and the coordinator's answers are awaited after.
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a runtime")
        .block_on(answers_as_the_ruling_says(
            "roundtrip-database-coordinator",
            &moved,
        ));
}

/// Proves: `Audit.List` over the migrated file store's audit file lists the migration's
/// `set_repository` line for `kbf-config` by action and by service, as the entry a later
/// `Services.SetRepository` of the same package writes (same action, service set, no host group,
/// a reason that begins `repository private: `), and the migration's `set_min_version` lines
/// carry the group they set, as `Services.SetMinVersion` writes them.
///
/// Catches: migration lines in a shape the audit list does not return by their action or service,
/// or one a later call of the same name writes differently.
#[test]
fn the_audit_list_reads_the_migration_lines_as_the_calls_write_them() {
    let disk = Disk::new("roundtrip-audit");
    migrate(&disk.flags(true), &FileLock, &clock()).expect("applied");
    let fake = Fake::new();
    let moved = read_files(&disk.paths(), None, NOW).expect("the store reads");
    let (rig, coordinator) = serve_built("roundtrip-audit-coordinator", &moved, &fake);
    let coordinator = coordinator.with_ops(
        OpsStore::open(&disk.paths().ops_file())
            .expect("the migrated ops file")
            .with_unix(Arc::new(|| NOW)),
    );
    let list = |action: &str, service: Option<&str>| {
        coordinator
            .audit_list(
                &operator(),
                proto::ListAuditRequest {
                    action: Some(action.to_owned()),
                    service: service.map(str::to_owned),
                    ..proto::ListAuditRequest::default()
                },
            )
            .expect("a page")
            .entries
    };

    let migrated = list("set_repository", Some("kbf-config"));
    assert_eq!(migrated.len(), 1);
    assert_eq!(migrated[0].by_name.as_deref(), Some("migration"));
    assert_eq!(migrated[0].host_group, None);
    assert_eq!(
        migrated[0].reason.as_deref(),
        Some("repository private: from the rollout file")
    );
    assert!(list("set_repository", Some("kbf")).is_empty());
    let minimums = list("set_min_version", None);
    assert_eq!(minimums.len(), 2);
    assert!(
        minimums
            .iter()
            .all(|entry| entry.service.as_deref() == Some("kbf") && entry.host_group.is_some())
    );

    let later = coordinator
        .set_repository(
            &operator(),
            proto::SetRepositoryRequest {
                service: "kbf-config".to_owned(),
                repository: "private".to_owned(),
                reason: "again".to_owned(),
            },
        )
        .expect("set")
        .entry
        .expect("an entry");
    assert_eq!(later.action, migrated[0].action);
    assert_eq!(later.service, migrated[0].service);
    assert_eq!(later.host_group, migrated[0].host_group);
    assert_eq!(later.reason.as_deref(), Some("repository private: again"));
    assert_eq!(list("set_repository", Some("kbf-config")).len(), 2);
    drop(rig);
}
