//! What a host's daemon said it runs, in the database (migration `0006`, the table
//! `enrollment_host_running`): the enrollment store's `note_running` over the Postgres records
//! writes the record into its own table, an approval of the host takes it away, an export reads
//! it back and refuses one that does not decode, and the release before and this one run on each
//! other's databases.

use std::sync::Arc;

use hostkeeper_enrollment::enroll::records::pb;
use hostkeeper_enrollment::enroll::{EnrollmentStore, HostRunning, RunningService, StoreError};
use prost::Message as _;

use super::coordinator::approved;
use super::devices::with_session;
use super::{SPENT_NOW, TestDatabase};
use crate::enrollment::tests::support::NOW;
use crate::store::postgres::{PgRecords, load_all, migrate, records};

fn runs(version: &str) -> HostRunning {
    HostRunning {
        agent_name: "hostkeeper".to_owned(),
        agent_version: "0.2.0".to_owned(),
        services: vec![RunningService {
            service: "web".to_owned(),
            version: version.to_owned(),
            installer: "deb".to_owned(),
        }],
    }
}

/// The records the table holds, by host id.
fn held(database: &TestDatabase) -> Vec<pb::Running> {
    let mut client = database.client();
    client
        .batch_execute("SET search_path TO hostkeeper")
        .expect("the search path");
    client
        .query(
            "SELECT running FROM enrollment_host_running ORDER BY host_id",
            &[],
        )
        .expect("the table reads")
        .iter()
        .map(|row| pb::Running::decode(row.get::<_, Vec<u8>>(0).as_slice()).expect("decodes"))
        .collect()
}

/// Proves: over Postgres, what a host runs is written into the table `enrollment_host_running`
/// and not into `enrollment_hosts`, whose columns are the ones the release before reads; a host
/// id never approved writes nothing; the same record again runs no statement; a change replaces
/// the row; a store made again over the tables reads the record back and writes nothing for it;
/// and a new approval of the host id deletes the row.
///
/// Catches: a record kept in a column of `enrollment_hosts` (which would stop the release before
/// from running on the database), one written at every heartbeat, one lost across a restart,
/// and one that outlives the approval it was said under.
#[test]
fn what_a_host_runs_is_a_row_of_its_own_table_written_on_change_and_deleted_by_an_approval() {
    let database = TestDatabase::new();
    let (store, _) = database.open("records").expect("the store opens");
    let (enrollment, id) = approved(&store, "web-1");
    assert_eq!(
        database.rows(
            "SELECT column_name FROM information_schema.columns \
             WHERE table_name = 'enrollment_hosts' ORDER BY ordinal_position"
        ),
        [
            "host_id",
            "key_hash",
            "machine_id",
            "role",
            "services",
            "duplicate",
            "first_heartbeat_unix",
            "first_serving_unix"
        ]
        .map(|column| vec![Some(column.to_owned())])
    );

    let statements = store.statements();
    let never = hostkeeper_enrollment::names::HostId::parse("web-9").expect("a host id");
    enrollment
        .note_running(&never, &runs("1.0"), NOW + 30)
        .expect("noted");
    assert_eq!(store.statements(), statements, "a host never approved");
    assert_eq!(held(&database), Vec::<pb::Running>::new());

    enrollment
        .note_running(&id, &runs("1.0"), NOW + 60)
        .expect("noted");
    let first = held(&database);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].at_unix, NOW + 60);
    assert_eq!(first[0].services[0].installer, "deb");
    let written = store.statements();
    enrollment
        .note_running(&id, &runs("1.0"), NOW + 90)
        .expect("noted");
    assert_eq!(store.statements(), written, "the second ran nothing");

    enrollment
        .note_running(&id, &runs("1.1"), NOW + 120)
        .expect("noted");
    let second = held(&database);
    assert_eq!(second.len(), 1, "replaced");
    assert_eq!(
        (second[0].at_unix, second[0].services[0].version.as_str()),
        (NOW + 120, "1.1")
    );

    let again = EnrollmentStore::new(Arc::new(PgRecords::over(&store).expect("records")));
    let read = again.enrolled_host(&id).expect("read").expect("enrolled");
    assert_eq!(
        (read.running, read.running_unix),
        (Some(runs("1.1")), Some(NOW + 120))
    );
    let loaded = store.statements();
    again
        .note_running(&id, &runs("1.1"), NOW + 200)
        .expect("noted");
    assert_eq!(
        store.statements(),
        loaded,
        "a store made again writes nothing"
    );

    super::serving::approve_again(&again, &id);
    assert_eq!(held(&database), Vec::<pb::Running>::new(), "an approval");
    again
        .note_running(&id, &runs("1.1"), NOW + 300)
        .expect("noted");
    assert_eq!(held(&database)[0].at_unix, NOW + 300);
}

/// Proves: a running record that does not decode is refused loudly, never read as a host that
/// ran nothing: the records of a store made over the tables are refused with an error that names
/// the host's row of the table, and an export's read of the tables is refused the same way.
///
/// Catches: a record dropped when it does not decode, which the next heartbeat would write over
/// without a trace, and an export that writes the hosts back without it.
#[test]
fn a_running_record_that_does_not_decode_is_refused_at_start_and_by_an_export() {
    let database = TestDatabase::new();
    let (store, _) = database.open("records").expect("the store opens");
    let (enrollment, id) = approved(&store, "web-1");
    enrollment
        .note_running(&id, &runs("1.0"), NOW + 60)
        .expect("noted");
    database
        .run("UPDATE enrollment_host_running SET running = '\\xff'::bytea")
        .expect("the row is damaged");
    let refused = PgRecords::over(&store).expect_err("refused");
    assert!(matches!(refused, StoreError::Corrupt { .. }), "{refused}");
    assert!(
        refused
            .to_string()
            .contains("enrollment_host_running/web-1 is not a store file this release reads: the running record does not decode"),
        "{refused}"
    );
    let read = with_session(&database, |session| {
        records::read_all(session).expect("reads")
    });
    let refused = read.expect_err("refused");
    assert!(matches!(refused, StoreError::Corrupt { .. }), "{refused}");
}

/// Proves: the release before this one (migrations `0001` to `0005`) starts on a database this
/// release migrated and holds a running record of: it runs nothing, `0006` says `may-run`, its
/// load reads the state this release loads, the columns of `enrollment_hosts` it reads and
/// writes are all still there, and its write of a host row leaves the running record as it was.
/// And the other way: this release starts on a database the release before migrated, runs `0006`
/// alone, and reads its hosts with nothing run.
///
/// Catches: a record that makes the previous coordinator refuse to start (the back-out of a
/// release would need the database restored), a column the release before reads that is gone,
/// and a migration that this release does not run on a database of the release before.
#[test]
fn the_release_before_starts_on_a_database_with_0006_and_this_one_on_its() {
    let database = TestDatabase::new();
    let (store, loaded) = database.open("this release").expect("the store opens");
    let (enrollment, id) = approved(&store, "web-1");
    enrollment
        .note_running(&id, &runs("1.0"), NOW + 60)
        .expect("noted");
    store.stop();
    let (ran, before_loaded) = with_session(&database, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..5]).expect("the release before's set");
        (
            migrate::apply(session, &before).expect("the release before migrates"),
            load_all(session, SPENT_NOW).expect("its load reads the state"),
        )
    });
    assert_eq!(ran, Vec::<i32>::new());
    assert_eq!(before_loaded, loaded);
    // The release before writes a host row as it always did.
    database
        .run(
            "INSERT INTO enrollment_hosts (host_id, key_hash, machine_id, role, services, \
             duplicate, first_heartbeat_unix, first_serving_unix) \
             VALUES ('web-1', '\\x07', 'm-1', 'web', '{}', true, 5, NULL) \
             ON CONFLICT (host_id) DO UPDATE SET (host_id, key_hash, machine_id, role, services, \
             duplicate, first_heartbeat_unix, first_serving_unix) = ROW(EXCLUDED.*)",
        )
        .expect("the release before's write");
    assert_eq!(held(&database).len(), 1, "its running record is left");

    let older = TestDatabase::new();
    let ran = with_session(&older, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..5]).expect("the release before's set");
        migrate::apply(session, &before).expect("the release before migrates")
    });
    assert_eq!(ran, vec![1, 2, 3, 4, 5]);
    let (store, _) = older.open("this release").expect("this release starts");
    let (enrollment, id) = approved(&store, "web-1");
    let read = enrollment
        .enrolled_host(&id)
        .expect("read")
        .expect("enrolled");
    assert_eq!((read.running, read.running_unix), (None, None));
    store.stop();
    assert_eq!(
        older.rows("SELECT version FROM schema_version ORDER BY version"),
        (1..=6)
            .map(|n| vec![Some(n.to_string())])
            .collect::<Vec<_>>()
    );
}
