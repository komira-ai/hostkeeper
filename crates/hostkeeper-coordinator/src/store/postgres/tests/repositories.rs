//! The registry of package repositories over the Postgres store (`docs/design/coordinator-storage.md`,
//! section 3.2; migration `0007`): the tables `repositories` and `repository_generation`, what a
//! commit writes to them, how a load reads them, and the release before this one on a database
//! this release migrated.

use std::sync::Arc;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use super::coordinator::start_on_postgres;
use super::{TestDatabase, off_runtime};
use crate::desired::DesiredStore;
use crate::marks_support::operator;
use crate::repositories_support::{komira, repository};
use crate::store::postgres::migrate;
use crate::store::postgres::rows::Tables;
use crate::store::postgres::session::Session;
use crate::store::postgres::state;
use crate::store::postgres::{Postgres, TIMEOUTS};
use crate::store::{Commit, Loaded, Shared};
use crate::testutil::Rig;

/// A desired store over the Postgres store `store`, holding what it loaded.
fn desired_store(store: &Postgres, loaded: Loaded) -> DesiredStore {
    let backend: Shared = Arc::new(store.clone());
    DesiredStore::over(backend, loaded.desired)
}

/// Commits the registry that `edit` makes of the one the store holds.
fn commit_registry(
    desired: &DesiredStore,
    edit: impl FnOnce(
        &crate::repositories::Registry,
    ) -> Result<crate::repositories::Registry, crate::repositories::PutError>,
) {
    let staged = desired.stage_repositories(edit).expect("staged");
    let part = staged.part().expect("a change");
    staged
        .backend()
        .commit(&Commit {
            desired: Some(part),
            ..Commit::default()
        })
        .expect("committed");
    staged.finish();
}

fn with_session<R: Send + 'static>(
    db: &TestDatabase,
    f: impl FnOnce(&mut Session) -> R + Send + 'static,
) -> R {
    let config = db.dsn().config(TIMEOUTS);
    off_runtime(move || {
        let mut session = Session::connect(
            &config,
            Arc::new(std::sync::atomic::AtomicU64::new(0)),
            false,
        )
        .expect("a session");
        f(&mut session)
    })
}

/// The names and generations of the rows of `repositories`, and the row of `repository_generation`.
type Table = Vec<Vec<Option<String>>>;

fn rows(db: &TestDatabase) -> (Table, Table) {
    (
        db.rows("SELECT name, (record->>'generation') FROM repositories ORDER BY name"),
        db.rows("SELECT id, generation FROM repository_generation"),
    )
}

fn pair(a: &str, b: &str) -> Vec<Option<String>> {
    vec![Some(a.to_owned()), Some(b.to_owned())]
}

/// Proves: a registry committed is one row of `repositories` per repository, keyed by its name,
/// whose `record` is the JSON the repositories file holds for it, and one row of
/// `repository_generation` holding the registry's generation; a database that holds no registry
/// has neither.
///
/// Catches: a repository written into a table an older coordinator reads, a generation that is not
/// kept, or a row for the empty registry.
#[test]
fn a_registry_is_a_row_per_repository_and_one_generation_row() {
    let db = TestDatabase::new();
    let (store, loaded) = db.open("coordinator").expect("the store opens");
    assert_eq!(rows(&db), (Vec::new(), Vec::new()));
    let desired = desired_store(&store, loaded);
    commit_registry(&desired, |held| held.put(komira(1)));
    commit_registry(&desired, |held| {
        held.put(repository("spare", "https://spare.example.com/", &["x"], 2))
    });
    assert_eq!(
        rows(&db),
        (
            vec![pair("komira", "1"), pair("spare", "2")],
            vec![pair("1", "2")]
        )
    );
    let stored = db.rows("SELECT record::text FROM repositories WHERE name = 'komira'");
    let json: serde_json::Value =
        serde_json::from_str(stored[0][0].as_deref().expect("a record")).expect("JSON");
    assert_eq!(
        json,
        crate::repositories::Kept {
            repository: komira(1),
            generation: 1
        }
        .to_json()
    );
    store.stop();
}

/// Proves: a commit writes only the rows that differ: a repository put over another leaves the
/// other's row as it was (a mark put into it by hand is still there), and a repository put again
/// replaces its own row.
///
/// Catches: a commit that rewrites every repository, keyrings included, at every change.
#[test]
fn a_commit_writes_only_the_rows_that_changed() {
    let db = TestDatabase::new();
    let (store, loaded) = db.open("coordinator").expect("the store opens");
    let desired = desired_store(&store, loaded);
    commit_registry(&desired, |held| held.put(komira(1)));
    db.run("UPDATE repositories SET record = jsonb_set(record, '{mark}', '1')")
        .expect("a mark");
    commit_registry(&desired, |held| {
        held.put(repository("spare", "https://spare.example.com/", &["x"], 2))
    });
    let marked = db.rows("SELECT record ? 'mark' FROM repositories ORDER BY name");
    assert_eq!(
        marked,
        vec![vec![Some("t".to_owned())], vec![Some("f".to_owned())]]
    );
    commit_registry(&desired, |held| held.put(komira(3)));
    assert_eq!(
        db.rows("SELECT record ? 'mark' FROM repositories WHERE name = 'komira'"),
        vec![vec![Some("f".to_owned())]]
    );
    store.stop();
}

/// Proves: a registry committed is loaded back equal, each repository's generation and the
/// registry's among it, by a store opened again; and once the last repository is removed the
/// generation row stays, so a registry emptied is read back at its generation and not at 0.
///
/// Catches: a generation that goes back to 0 with the last removal, which would let a host's
/// report of generation 2 stand for a set the coordinator no longer holds.
#[test]
fn a_registry_loads_back_equal_and_its_generation_survives_the_last_removal() {
    let db = TestDatabase::new();
    let (store, loaded) = db.open("first").expect("the store opens");
    let desired = desired_store(&store, loaded);
    commit_registry(&desired, |held| held.put(komira(1)));
    commit_registry(&desired, |held| {
        held.put(repository("spare", "https://spare.example.com/", &["x"], 2))
    });
    let held = desired.with_repositories(Clone::clone);
    store.stop();

    let (store, loaded) = db.open("second").expect("the store opens again");
    assert_eq!(loaded.desired.repositories, held);
    let desired = desired_store(&store, loaded);
    commit_registry(&desired, |held| {
        held.remove("komira")
            .ok_or(crate::repositories::PutError::Capacity)
    });
    commit_registry(&desired, |held| {
        held.remove("spare")
            .ok_or(crate::repositories::PutError::Capacity)
    });
    assert_eq!(rows(&db), (Vec::new(), vec![pair("1", "4")]));
    store.stop();

    let (store, loaded) = db.open("third").expect("the store opens a third time");
    assert_eq!(loaded.desired.repositories.generation, 4);
    assert!(loaded.desired.repositories.entries.is_empty());
    store.stop();
}

/// Proves: the tables refuse what the registry never holds: a name that is no repository name, a
/// second generation row, a generation row that is not `id` 1 and a generation of 0 or below.
///
/// Catches: a row a restart would refuse to read, written by hand or by a later build.
#[test]
fn the_tables_refuse_what_the_registry_cannot_hold() {
    let db = TestDatabase::new();
    let (store, _) = db.open("coordinator").expect("the store opens");
    for sql in [
        "INSERT INTO repositories (name, record) VALUES ('Komira', '{}')",
        "INSERT INTO repositories (name, record) VALUES ('', '{}')",
        "INSERT INTO repository_generation (id, generation) VALUES (2, 1)",
        "INSERT INTO repository_generation (id, generation) VALUES (1, 0)",
    ] {
        assert!(db.run(sql).is_err(), "{sql}");
    }
    db.run("INSERT INTO repository_generation (id, generation) VALUES (1, 5)")
        .expect("the one row");
    assert!(
        db.run("INSERT INTO repository_generation (id, generation) VALUES (1, 6)")
            .is_err()
    );
    store.stop();
}

/// Proves: a row of `repositories` whose record does not read stops the start, with the text that
/// names the row; a registry whose repository is at a generation the registry has not reached stops
/// it too.
///
/// Catches: a start that serves a registry it cannot read, or an error that does not say which row.
#[test]
fn a_row_that_does_not_read_stops_the_start_naming_the_repository() {
    let db = TestDatabase::new();
    let (store, _) = db.open("first").expect("the store opens");
    store.stop();
    db.run("INSERT INTO repositories (name, record) VALUES ('komira', '{\"generation\": 1}')")
        .expect("a row");
    let refused = db.open("second").err().expect("refused");
    assert_eq!(
        refused.to_string(),
        "database:repositories: the desired state is malformed: the row komira: no repository"
    );
    db.run("DELETE FROM repositories").expect("cleared");
    let record = crate::repositories::Kept {
        repository: komira(1),
        generation: 3,
    }
    .to_json();
    db.run(&format!(
        "INSERT INTO repositories (name, record) VALUES ('komira', '{record}'); \
         INSERT INTO repository_generation (id, generation) VALUES (1, 2)"
    ))
    .expect("rows");
    let refused = db.open("third").err().expect("refused");
    assert_eq!(
        refused.to_string(),
        "database:repositories: the desired state is malformed: komira is at generation 3, past the registry's 2"
    );
}

/// Proves: through a coordinator on Postgres a `Repositories.Put` is kept in the tables, and a
/// coordinator started again on the database lists the repository with its generation and the
/// registry's, and the next `Put` is the generation after.
///
/// Catches: a registry kept in memory but not in the database, or lost by a restart.
#[test]
fn a_repository_put_through_the_coordinator_is_served_again_after_a_restart() {
    let db = TestDatabase::new();
    let start = |test: &str| {
        let (store, loaded) = db.open("coordinator").expect("the store opens");
        let rig = Rig::new(test);
        let marker = rig.dir.path().join("halt.marker");
        let coordinator = start_on_postgres(&rig, &store, loaded, &marker);
        (coordinator, store, rig)
    };
    let (first, store, rig) = start("repositories-pg-first");
    first
        .repositories_put(
            &operator(),
            proto::PutRepositoryRequest {
                repository: Some(komira(1)),
                reason: "first".to_owned(),
            },
        )
        .expect("put");
    assert_eq!(rows(&db).0, vec![pair("komira", "1")]);
    store.stop();
    drop((first, rig));

    let (second, store, _rig) = start("repositories-pg-second");
    let listed = second
        .repositories_list(&operator(), &proto::ListRepositoriesRequest {})
        .expect("listed");
    assert_eq!(listed.generation, 1);
    assert_eq!(
        listed.repositories[0]
            .repository
            .as_ref()
            .map(|r| r.name.as_str()),
        Some("komira")
    );
    let next = second
        .repositories_put(
            &operator(),
            proto::PutRepositoryRequest {
                repository: Some(komira(2)),
                reason: "rotate".to_owned(),
            },
        )
        .expect("put");
    assert_eq!(next.repository.expect("held").generation, 2);
    store.stop();
}

/// Proves: the release before this one (the migrations `0001` to `0006`) starts on a database this
/// release migrated and holding a registry, because `0007` says `may-run`: its runner refuses
/// nothing and runs nothing, and its load of the desired state reads the state as it did; and this
/// release starts on a database the release before migrated, runs `0007` and holds an empty
/// registry.
///
/// Catches: a registry that stops the previous coordinator from starting, or a migration this
/// release does not run on a database of the release before.
#[test]
fn the_release_before_starts_on_a_database_with_0007_and_this_one_on_its() {
    let db = TestDatabase::new();
    let (store, loaded) = db.open("this release").expect("the store opens");
    let desired = desired_store(&store, loaded);
    commit_registry(&desired, |held| held.put(komira(1)));
    store.stop();

    let (ran, held) = with_session(&db, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..6]).expect("the release before's set");
        migrate::refuse_newer(session, &before).expect("0007 does not stop the release before");
        let ran = migrate::apply(session, &before).expect("its runner takes the database");
        let mut tables = Tables::new();
        for table in &state::DESIRED[..3] {
            let rows = session
                .query(&table.select(), &[])
                .and_then(|rows| table.rows_of(&rows))
                .expect("the table reads");
            tables.insert(table.name, rows);
        }
        (
            ran,
            state::desired_of(&tables).expect("its load reads the state"),
        )
    });
    assert_eq!(ran, Vec::<i32>::new());
    assert!(held.repositories.entries.is_empty(), "it reads no registry");
    assert_eq!(rows(&db).0.len(), 1, "its tables are left");

    let older = TestDatabase::new();
    let ran = with_session(&older, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..6]).expect("the release before's set");
        migrate::apply(session, &before).expect("the release before migrates")
    });
    assert_eq!(ran, vec![1, 2, 3, 4, 5, 6]);
    let (store, loaded) = older.open("this release").expect("this release starts");
    assert_eq!(
        loaded.desired.repositories,
        crate::repositories::Registry::default()
    );
    store.stop();
    assert_eq!(
        older.rows("SELECT version FROM schema_version ORDER BY version"),
        (1..=7)
            .map(|n| vec![Some(n.to_string())])
            .collect::<Vec<_>>()
    );
}
