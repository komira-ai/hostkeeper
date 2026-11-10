//! The releases of a desired state over the Postgres store (`docs/design/coordinator-storage.md`,
//! section 3.2; migration `0004`): the table `releases`, its columns, what a commit writes to it,
//! how a load reads it, and the release before this one on a database this release migrated.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::releases_server::Releases as _;
use hostkeeper_proto::hostkeeper::release::v1::{
    Blob, Hook, InstallerBounds, Location, Manifest, StepBounds, Target, TargetList,
};
use tonic::{Code, Request};

use super::coordinator::start_on_postgres;
use super::{TestDatabase, off_runtime};
use crate::desired::{Desired, DesiredStore};
use crate::grpc::Admitted;
use crate::grpc::operator::ReleasesService;
use crate::registry::Digest;
use crate::releases::{Release, Targets};
use crate::releases_tests::{repository_targets, target_list_before};
use crate::store::postgres::migrate;
use crate::store::postgres::releases;
use crate::store::postgres::rows::Tables;
use crate::store::postgres::rows::Val;
use crate::store::postgres::session::Session;
use crate::store::postgres::state;
use crate::store::postgres::{Postgres, TIMEOUTS};
use crate::store::{Loaded, Shared};
use crate::testutil::{OPERATOR, Rig, ci_token};
use crate::write_record::WriteAsk;

/// A manifest that passes every rule a manifest keeps, for `web` on Linux.
fn manifest(installer: &str) -> Manifest {
    if installer == "hostkeeper-archive" {
        return Manifest {
            min_hostkeeper: "0.4.0".to_owned(),
            ..Manifest::default()
        };
    }
    Manifest {
        package: (installer == "deb" || installer == "rpm").then(|| "web".to_owned()),
        hook: Some(Hook {
            command: vec!["bin/web".to_owned(), "update-hook".to_owned()],
            timeout_secs: Some(StepBounds {
                prepare: 0,
                cutover: 10,
                verify: 20,
                rollback: 30,
                cleanup: 40,
                uninstall: None,
            }),
            verify_deadline_secs: 50,
            ..Hook::default()
        }),
        run: vec!["web.service".to_owned()],
        min_hostkeeper: "0.4.0".to_owned(),
        installer_bounds: Some(InstallerBounds {
            stage_secs: 60,
            activate_secs: 120,
            remove_secs: 300,
        }),
        ..Manifest::default()
    }
}

/// An entry of `installer` whose artifact's digest is all `c`.
fn entry(installer: &str, c: char) -> Target {
    Target {
        installer: installer.to_owned(),
        locations: vec![Location {
            store: "github".to_owned(),
            locator: format!("example-org/web/releases/download/v2.0/web-{installer}"),
        }],
        digest: c.to_string().repeat(64),
        bytes: 4096,
        attestation: Some(Blob {
            digest: "e".repeat(64),
            bytes: 7311,
            locations: vec![Location {
                store: "github".to_owned(),
                locator: "example-org/web/releases/download/v2.0/web.sigstore.json".to_owned(),
            }],
        }),
        manifest: Some(manifest(installer)),
        ..Target::default()
    }
}

/// Targets of two platforms, the first with three entries in an order that is not alphabetical.
fn targets() -> Targets {
    BTreeMap::from([
        (
            "linux-x86_64".to_owned(),
            TargetList {
                entries: vec![entry("tarball", 'b'), entry("deb", 'a'), entry("rpm", 'f')],
            },
        ),
        (
            "macos-arm64".to_owned(),
            TargetList {
                entries: vec![entry("tarball", 'c')],
            },
        ),
    ])
}

/// The state of `web` at `version`.
fn desired(version: &str) -> Desired {
    Desired::parse(format!(r#"{{"version":"{version}","min_up":{{"daemon":0}}}}"#).as_bytes())
        .expect("a desired state")
}

/// A desired store over the Postgres store `store`, holding what it loaded.
fn desired_store(store: &Postgres, loaded: Loaded) -> DesiredStore {
    let backend: Shared = Arc::new(store.clone());
    DesiredStore::over(backend, loaded.desired)
}

/// Sets `version` of `web` with `release` in one commit.
fn set_with(store: &DesiredStore, version: &str, release: Release) {
    store
        .record_as(
            "web",
            desired(version),
            |_| false,
            &WriteAsk::default(),
            Some(Arc::new(release)),
        )
        .expect("the set commits");
}

/// The rows of the table `releases`: service, version, targets as text, digest.
fn release_rows(db: &TestDatabase) -> Vec<Vec<Option<String>>> {
    db.rows(
        "SELECT service, version, targets::text, release_index_digest FROM releases \
         ORDER BY service, version",
    )
}

/// The digest column of each row of `releases`.
fn digests(db: &TestDatabase) -> Vec<Option<String>> {
    release_rows(db).iter().map(|row| row[3].clone()).collect()
}

/// Proves: a release committed with its desired state is one row of `releases`, keyed by service
/// and version, whose `targets` is the JSON the releases file holds for it and whose
/// `release_index_digest` is `NULL` for a set without an index; the desired state beside it has
/// neither a `release_digest` nor a `release_index`, and the registry of digests has no row.
///
/// Catches: a release written into the columns an older coordinator reads, or a digest stored
/// for a release that has no index.
#[test]
fn a_release_is_one_row_with_its_targets_and_no_digest_without_an_index() {
    let db = TestDatabase::new();
    let (store, loaded) = db.open("coordinator").expect("the store opens");
    let desired = desired_store(&store, loaded);
    let release = Release::new(targets(), None);
    let want = release.to_json()["targets"].clone();
    set_with(&desired, "2.0", release);

    let rows = release_rows(&db);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0].as_deref(), Some("web"));
    assert_eq!(rows[0][1].as_deref(), Some("2.0"));
    let stored: serde_json::Value =
        serde_json::from_str(rows[0][2].as_deref().expect("targets")).expect("JSON");
    assert_eq!(stored, want);
    assert_eq!(rows[0][3], None);
    assert_eq!(
        db.rows("SELECT release_digest, release_index FROM desired WHERE service = 'web'"),
        vec![vec![None, None]]
    );
    assert_eq!(
        db.rows("SELECT count(*) FROM desired_digests"),
        vec![vec![Some("0".to_owned())]]
    );
    store.stop();
}

/// Proves: a release set with an index keeps that index's digest in `release_index_digest`, and
/// the table refuses a digest that is not 64 lower-case hex characters.
///
/// Catches: the digest lost on the way to the table, or a malformed one accepted.
#[test]
fn a_release_with_an_index_keeps_its_digest_and_the_table_checks_it() {
    let db = TestDatabase::new();
    let (store, loaded) = db.open("coordinator").expect("the store opens");
    let desired = desired_store(&store, loaded);
    let digest = Digest::parse(&"9".repeat(64)).expect("a digest");
    set_with(&desired, "2.0", Release::new(targets(), Some(digest)));
    assert_eq!(digests(&db), vec![Some("9".repeat(64))]);
    db.run("INSERT INTO releases (service, version, targets) VALUES ('web', '3.0', '{}')")
        .expect("a row without a digest");
    let refused = db.run(
        "INSERT INTO releases (service, version, targets, release_index_digest) \
         VALUES ('web', '3.1', '{}', 'ABC')",
    );
    assert!(refused.is_err());
    store.stop();
}

/// Proves: a release committed is loaded back equal (the same entries in the same order, the same
/// index digest) by a store opened again on the database, and the state beside it loads as it was.
///
/// Catches: a release that comes back with its entries reordered, its digest lost, or on another
/// version.
#[test]
fn a_committed_release_loads_back_equal() {
    let db = TestDatabase::new();
    let (store, loaded) = db.open("first").expect("the store opens");
    let desired = desired_store(&store, loaded);
    let digest = Digest::parse(&"9".repeat(64)).expect("a digest");
    let with_index = Release::new(targets(), Some(digest));
    let without = Release::new(
        BTreeMap::from([(
            "linux-arm64".to_owned(),
            TargetList {
                entries: vec![entry("tarball", 'e'), entry("deb", 'd')],
            },
        )]),
        None,
    );
    set_with(&desired, "2.0", with_index.clone());
    set_with(&desired, "2.1", without.clone());
    store.stop();

    let (store, loaded) = db.open("second").expect("the store opens again");
    let book = &loaded.desired.releases["web"];
    assert_eq!(book.keys().collect::<Vec<_>>(), ["2.0", "2.1"]);
    assert_eq!(*book["2.0"], with_index);
    assert_eq!(*book["2.1"], without);
    let installers: Vec<_> = book["2.1"].targets()["linux-arm64"]
        .entries
        .iter()
        .map(|e| e.installer.as_str())
        .collect();
    assert_eq!(installers, ["tarball", "deb"]);
    assert_eq!(loaded.desired.desired["web"].version.as_str(), "2.1");
    assert!(loaded.desired.desired["web"].release_digest.is_none());
    assert!(loaded.desired.desired["web"].release_index.is_none());
    store.stop();
}

/// Proves: a row of `releases` whose targets do not read stops the start, with the text that names
/// the service and the version of the row, and a row whose targets hold a field this build does
/// not know loads and keeps the field when the release is written back.
///
/// Catches: a start that serves a state with a release it cannot read, an error that does not say
/// which row, or a field of a newer release dropped on the way through.
#[test]
fn a_row_whose_targets_do_not_read_stops_the_start_naming_service_and_version() {
    let db = TestDatabase::new();
    let (store, _) = db.open("first").expect("the store opens");
    store.stop();
    db.run(
        "INSERT INTO releases (service, version, targets) \
         VALUES ('web', '2.0', '{\"linux-x86_64\": 5}')",
    )
    .expect("a row");
    let error = db.open("second").err().expect("the start is refused");
    assert_eq!(
        error.to_string(),
        "database:releases: the desired state is malformed: the row web 2.0: the list of linux-x86_64 does not read"
    );

    db.run("DELETE FROM releases").expect("the row is removed");
    db.run(
        "INSERT INTO releases (service, version, targets) VALUES ('web', '2.0', \
         '{\"linux-x86_64\": {\"entries\": [{\"installer\": \"tarball\", \"future\": 7}]}}')",
    )
    .expect("a row from a newer release");
    let (store, loaded) = db
        .open("third")
        .expect("a field this build does not know loads");
    let kept = loaded.desired.releases["web"]["2.0"].to_json();
    assert_eq!(
        kept["targets"]["linux-x86_64"]["entries"][0]["future"],
        serde_json::json!(7)
    );
    store.stop();
}

/// Proves: a commit that changes only a service's limits, or sets another version, writes no row
/// of `releases` for a release already stored: a row changed by hand survives both.
///
/// Catches: every commit rewriting the whole table, which would replace a release that is meant to
/// be written once.
#[test]
fn a_commit_that_does_not_change_a_release_leaves_its_row_alone() {
    let db = TestDatabase::new();
    let (store, loaded) = db.open("coordinator").expect("the store opens");
    let desired = desired_store(&store, loaded);
    set_with(&desired, "2.0", Release::new(targets(), None));
    let marked = "7".repeat(64);
    db.run(&format!(
        "UPDATE releases SET release_index_digest = '{marked}' WHERE version = '2.0'"
    ))
    .expect("the row is changed by hand");

    // The same version again with other limits, and no release: the release stays as it is.
    let limits = Desired::parse(br#"{"version":"2.0","min_up":{"daemon":1}}"#).expect("limits");
    desired
        .record_as("web", limits, |_| false, &WriteAsk::default(), None)
        .expect("the limits commit");
    assert_eq!(digests(&db), vec![Some(marked.clone())]);

    // Another version with its own release: the row of the first is not rewritten.
    set_with(&desired, "2.1", Release::new(targets(), None));
    assert_eq!(digests(&db), vec![Some(marked), None]);
    store.stop();
}

/// The statements the store ran while `change` ran.
fn statements_of(store: &Postgres, change: impl FnOnce()) -> u64 {
    let before = store.statements();
    change();
    store.statements() - before
}

/// Proves: a commit that sets a version with a release runs one more statement than the same set
/// without one (the row), and a repeat of the same set with the same release runs no more than a
/// set without one (the row is not written again).
///
/// Catches: a release written twice, or a set that writes the table when it has no release.
#[test]
fn a_release_costs_one_row_and_an_unchanged_commit_costs_nothing() {
    let db = TestDatabase::new();
    let (store, loaded) = db.open("coordinator").expect("the store opens");
    let desired = desired_store(&store, loaded);
    let plain = statements_of(&store, || {
        desired
            .record_as(
                "web",
                self::desired("1.0"),
                |_| false,
                &WriteAsk::default(),
                None,
            )
            .expect("a plain set");
    });
    let with = statements_of(&store, || {
        set_with(&desired, "2.0", Release::new(targets(), None));
    });
    let again = statements_of(&store, || {
        set_with(&desired, "2.0", Release::new(targets(), None));
    });
    let plain_again = statements_of(&store, || {
        desired
            .record_as(
                "web",
                self::desired("2.0"),
                |_| false,
                &WriteAsk::default(),
                None,
            )
            .expect("a plain set of the same version");
    });
    assert!(plain > 0);
    assert_eq!(with, plain + 1);
    assert_eq!(again, plain_again);
    store.stop();
}

/// A session on `db` and `f` run with it on a thread of its own.
fn with_session<R: Send + 'static>(
    db: &TestDatabase,
    f: impl FnOnce(&mut Session) -> R + Send + 'static,
) -> R {
    let config = db.dsn().config(TIMEOUTS);
    off_runtime(move || {
        let mut session =
            Session::connect(&config, Arc::new(AtomicU64::new(0)), false).expect("a session");
        f(&mut session)
    })
}

/// Proves: the release before this one (the first three migrations, the desired tables
/// `desired` and `desired_digests` only) starts on a database this release migrated and holding a
/// release, because `0004` says `may-run`: its runner refuses nothing and runs nothing, and its
/// load of the desired state reads the version set with targets with neither a package digest nor
/// an index and no digest in the registry, so a host without the service is shown `no_digest`.
///
/// Catches: a release that makes the previous coordinator refuse to start, or a release stored
/// where the previous coordinator would read it as a package digest or an index.
#[test]
fn the_release_before_runs_on_a_database_with_a_release_and_reads_the_third_form() {
    let db = TestDatabase::new();
    let (store, loaded) = db.open("this release").expect("the store opens");
    let desired = desired_store(&store, loaded);
    set_with(&desired, "2.0", Release::new(targets(), None));
    store.stop();
    assert_eq!(release_rows(&db).len(), 1);

    let (ran, held) = with_session(&db, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..3]).expect("the release before's set");
        migrate::refuse_newer(session, &before).expect("0004 does not stop the release before");
        let ran = migrate::apply(session, &before).expect("its runner takes the database");
        let mut tables = Tables::new();
        for table in &state::DESIRED[..2] {
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
    assert_eq!(
        db.rows("SELECT version FROM schema_version ORDER BY version")
            .len(),
        migrate::SHIPPED.len()
    );
    let state = &held.desired["web"];
    assert_eq!(state.version.as_str(), "2.0");
    assert!(state.release_digest.is_none());
    assert!(state.release_index.is_none());
    assert!(state.release().is_none());
    assert!(
        held.digests
            .get("web")
            .is_none_or(|registry| registry.is_empty())
    );
    assert!(held.releases.is_empty());
}

/// Proves: the reader of a coordinator before packages-repositories.md's A16 starts on a
/// `releases` table this release wrote with a repository entry: its load reads every row, its own
/// copy of the entry has no repository and no package version, and the rows it would write back
/// are exactly the rows stored, the entry's `repository` and `packageVersion` included; this
/// release opened again on the database reads the entry with both.
///
/// Catches: a row with a repository entry that stops an older coordinator's load, or that it
/// writes back without the two keys.
#[test]
fn the_previous_coordinators_reader_starts_on_a_table_holding_a_repository_entry() {
    let db = TestDatabase::new();
    let (store, loaded) = db.open("this release").expect("the store opens");
    let desired = desired_store(&store, loaded);
    let release = Release::new(repository_targets("web"), None);
    set_with(&desired, "2.0", release.clone());
    store.stop();

    let stored = with_session(&db, |session| {
        let table = &releases::TABLE;
        session
            .query(&table.select(), &[])
            .and_then(|rows| table.rows_of(&rows))
            .expect("the table reads")
    });
    let before = target_list_before();
    let books = releases::of_by(&stored, Some(&before)).expect("the release before loads it");
    let first = &books["web"]["2.0"].targets()["linux-x86_64"].entries[0];
    assert_eq!(
        (first.repository.clone(), first.package_version.clone()),
        (None, None)
    );
    assert_eq!(
        (first.installer.as_str(), first.locations.len()),
        ("apt", 0)
    );
    assert_eq!(releases::rows(&books), stored);
    let row = stored.values().next().expect("one row");
    let Val::Json(targets) = &row[0] else {
        panic!("targets is JSON: {row:?}");
    };
    assert_eq!(
        targets["linux-x86_64"]["entries"][0]["packageVersion"],
        serde_json::json!("1:1.6.0-1")
    );

    let (store, loaded) = db
        .open("this release again")
        .expect("the store opens again");
    assert_eq!(*loaded.desired.releases["web"]["2.0"], release);
    assert_eq!(
        loaded.desired.releases["web"]["2.0"].targets(),
        &repository_targets("web")
    );
    store.stop();
}

/// A coordinator over `database`, and its `Releases`.
struct Releasing {
    coordinator: Arc<crate::service::Coordinator>,
    store: Postgres,
    _rig: Rig,
}

impl Releasing {
    fn start(test: &str, database: &TestDatabase) -> Releasing {
        let (store, loaded) = database.open("coordinator").expect("the store opens");
        let rig = Rig::new(test);
        let marker = rig.dir.path().join("halt.marker");
        let coordinator = Arc::new(start_on_postgres(&rig, &store, loaded, &marker));
        Releasing {
            coordinator,
            store,
            _rig: rig,
        }
    }

    fn releases(&self) -> ReleasesService {
        ReleasesService::new(Arc::clone(&self.coordinator))
    }

    async fn as_ci<T>(&self, message: T) -> Request<T> {
        let verified = self
            .coordinator
            .ci(Some(&ci_token()))
            .await
            .expect("CI's token is accepted");
        let mut request = Request::new(message);
        request.extensions_mut().insert(Admitted::Ci(verified));
        request
    }

    async fn as_operator<T>(&self, message: T) -> Request<T> {
        let mut request = Request::new(message);
        request.extensions_mut().insert(Admitted::Operator {
            caller: self.coordinator.caller(Some(OPERATOR)).await,
            bearer: OPERATOR.to_owned(),
        });
        request
    }
}

/// Proves: through a coordinator on Postgres, a `SetDesired` with targets is kept as a row of
/// `releases`, and a coordinator started again on the database answers `GetRelease` with the
/// targets as set; a version never set with targets is `release_unknown`.
///
/// Catches: a release that is kept in memory but not in the database, or lost by a restart.
#[tokio::test]
async fn a_release_set_through_the_coordinator_is_served_again_after_a_restart() {
    let db = TestDatabase::new();
    let first = Releasing::start("releases-pg-first", &db);
    let set = proto::SetDesiredRequest {
        service: "web".to_owned(),
        desired: Some(proto::DesiredState {
            version: "2.0".to_owned(),
            ..proto::DesiredState::default()
        }),
        targets: targets(),
        ..proto::SetDesiredRequest::default()
    };
    first
        .releases()
        .set_desired(first.as_ci(set).await)
        .await
        .expect("the set is accepted");
    assert_eq!(release_rows(&db).len(), 1);
    first.store.stop();
    drop(first);

    let second = Releasing::start("releases-pg-second", &db);
    let ask = |version: &str| proto::GetReleaseRequest {
        service: "web".to_owned(),
        version: version.to_owned(),
        host_group: String::new(),
    };
    let answer = second
        .releases()
        .get_release(second.as_operator(ask("2.0")).await)
        .await
        .expect("the release is served")
        .into_inner();
    assert_eq!(answer.targets, targets());
    assert_eq!(answer.release_index_digest, None);
    let status = second
        .releases()
        .get_release(second.as_operator(ask("9.9")).await)
        .await
        .expect_err("a version never set with targets");
    assert_eq!(status.code(), Code::NotFound);
    assert_eq!(status.message(), "release_unknown");
    second.store.stop();
}
