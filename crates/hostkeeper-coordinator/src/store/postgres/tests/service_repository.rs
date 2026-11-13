//! A package's repository in the database: `Services.SetRepository` through a coordinator over the
//! store, and the state a store opened again loads.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::ServicesRefusal as Refusal;

use super::TestDatabase;
use super::coordinator::Farm;
use super::devices::with_session;
use crate::marks_support::{WEB, operator};
use crate::repositories_support::repository;
use crate::store::postgres::migrate;

/// `Services.SetRepository` of `service` to `repository`, as the operator.
fn set(farm: &Farm, service: &str, repository: &str) -> Result<(), Refusal> {
    farm.rig
        .coordinator
        .set_repository(
            &operator(),
            proto::SetRepositoryRequest {
                service: service.to_owned(),
                repository: repository.to_owned(),
                reason: "moved".to_owned(),
            },
        )
        .map(drop)
}

/// Proves: a repository set through the coordinator is in the database row of the service, a store
/// opened again loads it as the service's marks.
///
/// Catches: a repository the database does not keep, so that a restart sends the service back to
/// the default.
#[test]
fn a_repository_set_survives_the_store_being_opened_again() {
    let farm = Farm::plain("pg-set-repository");
    farm.rig
        .coordinator
        .repositories_put(
            &operator(),
            proto::PutRepositoryRequest {
                repository: Some(repository(
                    "acme",
                    "https://acme.example.com/apt",
                    &["testing"],
                    3,
                )),
                reason: "for a test".to_owned(),
            },
        )
        .expect("put");

    set(&farm, WEB, "acme").expect("set");
    assert_eq!(
        farm.database
            .rows("SELECT repository FROM services WHERE service = 'web'"),
        vec![vec![Some("acme".to_owned())]]
    );
    farm.store.stop();
    let (store, loaded) = farm.database.open("again").expect("opened again");
    assert_eq!(loaded.ops.services[WEB].repository.as_deref(), Some("acme"));
    store.stop();
}

/// Proves: this release starts on a database the release before (0.1.252, migrations `0001` to
/// `0008`) migrated and wrote a service, a host and an audit line into, as that release writes them
/// (naming the columns it knows): the start runs `0009` and the migrations after it, `0010` to `0012` (the rows
/// of `0001` to `0008` in `schema_version` are as they were, `0009` to `0012` are added, the table
/// now has the column), the service
/// loads with no repository, and the host and the audit line are intact.
///
/// Catches: a start that refuses or fails on the database of the release before, one that runs a
/// migration again, or a service row read as naming a repository it never set.
#[test]
fn this_release_starts_on_a_database_the_release_before_migrated_and_runs_0009_and_0010_only() {
    const COLUMN: &str = "SELECT count(*) FROM information_schema.columns WHERE \
         table_schema = 'hostkeeper' AND table_name = 'services' AND column_name = 'repository'";
    let db = TestDatabase::new();
    let ran = with_session(&db, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..8]).expect("the release before's set");
        migrate::apply(session, &before).expect("the release before migrates")
    });
    assert_eq!(ran, (1..=8).collect::<Vec<i32>>());
    db.run(
        "INSERT INTO services (service, halt_generation) VALUES ('web', 2); \
         INSERT INTO enrollment_hosts (host_id, key_hash, machine_id, role, services, duplicate, \
         first_heartbeat_unix, first_serving_unix) \
         VALUES ('web-1', '\\x07', 'm-1', 'web', '{}', false, NULL, NULL); \
         INSERT INTO audit (seq, at_unix, by_name, action, service, node_id, reason) \
         VALUES (1, 1800000000, 'op', 'exclude', 'web', 'web-1', 'disk')",
    )
    .expect("the release before's rows");
    assert_eq!(db.rows(COLUMN), vec![vec![Some("0".to_owned())]]);
    let migrated = "SELECT version, name, applied_unix FROM schema_version ORDER BY version";
    let before = db.rows(migrated);
    assert_eq!(before.len(), 8);

    let (store, loaded) = db.open("this release").expect("this release starts");
    assert_eq!(loaded.ops.services[WEB].repository, None);
    assert_eq!(loaded.ops.services[WEB].halt_generation, 2);
    assert_eq!(loaded.ops.audit.len(), 1);
    store.stop();

    let after = db.rows(migrated);
    assert_eq!(after.len(), 17, "{after:?}");
    assert_eq!(after[..8], before[..], "0001 to 0008 were not run again");
    assert_eq!(
        after[8][..2],
        [Some("9".to_owned()), Some("service_repository".to_owned())]
    );
    assert_eq!(
        after[9][..2],
        [Some("10".to_owned()), Some("accounts".to_owned())]
    );
    assert_eq!(
        after[10][..2],
        [Some("11".to_owned()), Some("mdm".to_owned())]
    );
    assert_eq!(
        after[11][..2],
        [Some("12".to_owned()), Some("mdm_setup".to_owned())]
    );
    assert_eq!(
        after[12][..2],
        [Some("13".to_owned()), Some("host_group_all".to_owned())]
    );
    assert_eq!(
        after[13][..2],
        [Some("14".to_owned()), Some("trusted_publishers".to_owned())]
    );
    assert_eq!(
        after[14][..2],
        [Some("15".to_owned()), Some("lab_layout".to_owned())]
    );
    assert_eq!(db.rows(COLUMN), vec![vec![Some("1".to_owned())]]);
    assert_eq!(
        db.rows("SELECT service, halt_generation, repository FROM services"),
        vec![vec![Some("web".to_owned()), Some("2".to_owned()), None]]
    );
    assert_eq!(
        db.rows("SELECT host_id FROM enrollment_hosts"),
        vec![vec![Some("web-1".to_owned())]]
    );
    assert_eq!(
        db.rows("SELECT seq FROM audit"),
        vec![vec![Some("1".to_owned())]]
    );
}

/// Proves: the release before (migrations `0001` to `0008`) is accepted on a database this release
/// migrated, because `0009` says `-- older-releases: may-run`: its runner runs nothing and answers
/// no error, and the database keeps `0001` to `0012` and the repository a service was given.
///
/// Catches: a `0009` marked `refuse`, which would stop a rollback to 0.1.252 that the column alone
/// does not need.
#[test]
fn the_release_before_runs_on_a_database_this_release_migrated() {
    let db = TestDatabase::new();
    let (store, _) = db.open("this release").expect("the store opens");
    store.stop();
    db.run("INSERT INTO services (service, halt_generation, repository) VALUES ('web', 1, 'acme')")
        .expect("a repository set");
    let ran = with_session(&db, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..8]).expect("the release before's set");
        migrate::apply(session, &before).expect("the release before is accepted")
    });
    assert_eq!(ran, Vec::<i32>::new());
    assert_eq!(
        db.rows("SELECT version FROM schema_version ORDER BY version"),
        (1..=17)
            .map(|n| vec![Some(n.to_string())])
            .collect::<Vec<_>>()
    );
    assert_eq!(
        db.rows("SELECT repository FROM services WHERE service = 'web'"),
        vec![vec![Some("acme".to_owned())]]
    );
}

/// Proves: the release before (migrations `0001` to `0013`) accepts a database this release
/// migrated and wrote a package's trusted publishers into, because `0014` says `may-run`: its
/// runner runs nothing and answers, the versions are as they were, its own write of the service's
/// row (naming the columns it knows) leaves the publishers, and this release reads them back, an
/// empty list as empty and a package with none as none.
///
/// Catches: a `0014` marked `refuse`, which would stop a rollback the column alone does not need,
/// and a write of the release before that takes the list away.
#[test]
fn the_release_before_runs_on_a_database_with_trusted_publishers_and_leaves_them() {
    let db = TestDatabase::new();
    let (store, _) = db.open("this release").expect("the store opens");
    store.stop();
    db.run(
        "INSERT INTO services (service, halt_generation, trusted_publishers) VALUES \
         ('web', 1, '[{\"repository\": \"acme/web\", \"workflow\": \".github/workflows/r.yml\", \
         \"ref\": \"refs/heads/main\"}]'), ('gone', 1, '[]'), ('plain', 1, NULL)",
    )
    .expect("publishers written");
    let ran = with_session(&db, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..13]).expect("the release before's set");
        migrate::apply(session, &before).expect("the release before is accepted")
    });
    assert_eq!(ran, Vec::<i32>::new());
    db.run("UPDATE services SET halt_generation = 2 WHERE service = 'web'")
        .expect("the release before's write");
    let (store, loaded) = db.open("again").expect("this release again");
    store.stop();
    let web = &loaded.ops.services["web"];
    assert_eq!(web.halt_generation, 2);
    let held = web.trusted_publishers.as_ref().expect("held");
    assert_eq!(held[0].repository, "acme/web");
    assert_eq!(held[0].git_ref, "refs/heads/main");
    assert_eq!(
        loaded.ops.services["gone"].trusted_publishers,
        Some(Vec::new())
    );
    assert_eq!(loaded.ops.services["plain"].trusted_publishers, None);
}
