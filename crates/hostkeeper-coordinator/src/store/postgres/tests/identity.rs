//! An operator's approval of a host's key for service identity, in the database (migration
//! `0022`, the table `enrollment_host_identity`): the enrollment store's `approve_identity` over
//! the Postgres records writes the approval into its own table, an approval of the host takes it
//! away, a row for another key counts as absent, an export reads it back and refuses one that
//! does not decode, the deny list is read from the deny rows, and the release before and this
//! one run on each other's databases.

use std::sync::Arc;

use hostkeeper_enrollment::enroll::records::pb;
use hostkeeper_enrollment::enroll::{
    ApprovalRecord, Assurance, Claims, Decision, EnrollmentStore, INVENTORY_APPROVER,
    IdentityApproved, NewRequest, StoreError,
};
use hostkeeper_enrollment::names::{HostId, KeyHash, Role};
use prost::Message as _;

use super::with_session;
use super::{SPENT_NOW, TestDatabase};
use crate::enrollment::tests::support::{HostKey, NOW};
use crate::store::postgres::{PgRecords, Postgres, load_all, migrate, records};

/// An enrollment store over the records of `store`, with `host` approved by `approver` as an
/// inventory record would approve it; and the key it holds.
fn approved_by(store: &Postgres, host: &str, approver: &str) -> (EnrollmentStore, HostId, KeyHash) {
    let enrollment = EnrollmentStore::new(Arc::new(PgRecords::over(store).expect("the records")));
    let id = HostId::parse(host).expect("a host id");
    let key = HostKey::new();
    let posted = enrollment
        .post(
            NewRequest {
                csr_der: key.csr(),
                claims: Claims {
                    machine_id: format!("m-{host}"),
                    ..Claims::default()
                },
                image: "hostkeeper-image-1".to_owned(),
                renewal_of: None,
                mac_nonce_presented: false,
            },
            NOW,
        )
        .expect("posted");
    let decision = Decision {
        host_id: id.clone(),
        role: Role::parse("web").expect("a role"),
        services: Vec::new(),
        replace: false,
        host_group: None,
    };
    enrollment
        .approve_recorded(
            &posted.request_id,
            &key.key_hash(),
            &decision,
            &ApprovalRecord {
                approver: approver.to_owned(),
                approved_unix: NOW,
                reason: String::new(),
                mac_nonce: None,
                mac_enrolled_since_unix: None,
            },
        )
        .expect("approved");
    (enrollment, id, key.key_hash())
}

/// The approvals the table holds, by host id, with the host each is for.
fn held(database: &TestDatabase) -> Vec<(String, pb::IdentityApproval)> {
    let mut client = database.client();
    client
        .batch_execute("SET search_path TO hostkeeper")
        .expect("the search path");
    client
        .query(
            "SELECT host_id, identity_approval FROM enrollment_host_identity ORDER BY host_id",
            &[],
        )
        .expect("the table reads")
        .iter()
        .map(|row| {
            (
                row.get::<_, String>(0),
                pb::IdentityApproval::decode(row.get::<_, Vec<u8>>(1).as_slice()).expect("decodes"),
            )
        })
        .collect()
}

/// Proves: over Postgres, an approval of a host's key for identity is written into the table
/// `enrollment_host_identity` and not into `enrollment_hosts`; a host that needs none writes
/// nothing, and a second approval of the same key writes nothing; a store made again over the
/// tables reads the approval back; an export's read of the tables has it in the host; and a new
/// approval of the host id (a new key) deletes the row.
///
/// Catches: an approval kept in a column of `enrollment_hosts` (which would stop the release
/// before from running on the database), one written at every call, one lost across a restart,
/// and one that outlives the key it was given for.
#[test]
fn an_identity_approval_is_a_row_of_its_own_table_deleted_by_an_approval_of_the_host() {
    let database = TestDatabase::new();
    let (store, _) = database.open("records").expect("the store opens");
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
            "first_serving_unix",
            "host_group",
            "moving_from",
            "facts_generation"
        ]
        .map(|column| vec![Some(column.to_owned())])
    );
    let (enrollment, id, key) = approved_by(&store, "web-1", INVENTORY_APPROVER);
    let (operators, by_hand, by_hand_key) = approved_by(&store, "web-2", "ops");
    assert_eq!(held(&database), vec![]);

    let operator = operators
        .approve_identity(&by_hand, &by_hand_key, "alice", NOW + 1)
        .expect("answered");
    assert_eq!(operator, IdentityApproved::OperatorAssured);
    assert_eq!(held(&database), vec![], "a host that needs none");

    let written = enrollment
        .approve_identity(&id, &key, "alice", NOW + 5)
        .expect("approved");
    assert!(matches!(written, IdentityApproved::Approved(_)));
    let wanted = pb::IdentityApproval {
        key_hash: key.as_bytes().to_vec(),
        approver: "alice".to_owned(),
        approved_unix: NOW + 5,
    };
    assert_eq!(held(&database), vec![("web-1".to_owned(), wanted.clone())]);
    let again = enrollment
        .approve_identity(&id, &key, "bob", NOW + 9)
        .expect("approved again");
    assert!(matches!(again, IdentityApproved::AlreadyApproved(_)));
    assert_eq!(
        held(&database),
        vec![("web-1".to_owned(), wanted.clone())],
        "the second wrote nothing"
    );

    let snapshot = with_session(&database, |session| {
        records::read_all(session)
            .expect("reads")
            .expect("the records decode")
    });
    let hosts: Vec<(String, Option<pb::IdentityApproval>)> = snapshot
        .hosts
        .iter()
        .map(|host| (host.host_id.clone(), host.identity_approval.clone()))
        .collect();
    assert_eq!(
        hosts,
        vec![
            ("web-1".to_owned(), Some(wanted)),
            ("web-2".to_owned(), None)
        ]
    );

    let reopened = EnrollmentStore::new(Arc::new(PgRecords::over(&store).expect("records")));
    assert!(reopened.identity_allowed(&id).expect("read"));
    let state = reopened.identity_state(&id).expect("read").expect("a host");
    assert_eq!(state.assurance, Assurance::Auto);
    assert_eq!(state.approval.map(|a| a.approver), Some("alice".to_owned()));

    super::serving::approve_again(&reopened, &id);
    assert_eq!(held(&database), vec![], "an approval of the host id");
    let state = reopened.identity_state(&id).expect("read").expect("a host");
    assert_eq!(state.approval, None);
    assert_ne!(state.key_hash, key, "the host holds the new key");
}

/// Proves: a row of the table for another key than the host's counts as absent: the host is not
/// allowed, its state shows no approval, and approving its key writes over the row.
///
/// Catches: a row an older release left behind approving a new key of the host id.
#[test]
fn a_row_for_another_key_is_no_approval_of_the_hosts_key() {
    let database = TestDatabase::new();
    let (store, _) = database.open("records").expect("the store opens");
    let (enrollment, id, key) = approved_by(&store, "web-1", INVENTORY_APPROVER);
    let other = pb::IdentityApproval {
        key_hash: KeyHash::of_spki(b"another key").as_bytes().to_vec(),
        approver: "old".to_owned(),
        approved_unix: NOW - 1,
    };
    database
        .run(&format!(
            "INSERT INTO enrollment_host_identity (host_id, identity_approval) \
             VALUES ('web-1', '\\x{}'::bytea)",
            hex(&other.encode_to_vec())
        ))
        .expect("a row of another key");
    let reopened = EnrollmentStore::new(Arc::new(PgRecords::over(&store).expect("records")));
    assert!(!reopened.identity_allowed(&id).expect("read"));
    assert_eq!(
        reopened
            .identity_state(&id)
            .expect("read")
            .expect("a host")
            .approval,
        None
    );
    assert!(matches!(
        reopened
            .approve_identity(&id, &key, "alice", NOW + 5)
            .expect("approved"),
        IdentityApproved::Approved(_)
    ));
    assert_eq!(held(&database)[0].1.approver, "alice");
    drop(enrollment);
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Proves: an identity approval that does not decode is refused loudly, never read as an
/// approval that is not there: the records of a store made over the tables are refused with an
/// error that names the host's row of the table, and an export's read of the tables is refused
/// the same way.
///
/// Catches: a row dropped when it does not decode, which would show an approved host as not
/// approved, and an export that writes the hosts back without it.
#[test]
fn an_identity_approval_that_does_not_decode_is_refused_at_start_and_by_an_export() {
    let database = TestDatabase::new();
    let (store, _) = database.open("records").expect("the store opens");
    let (enrollment, id, key) = approved_by(&store, "web-1", INVENTORY_APPROVER);
    enrollment
        .approve_identity(&id, &key, "alice", NOW + 5)
        .expect("approved");
    database
        .run("UPDATE enrollment_host_identity SET identity_approval = '\\xff'::bytea")
        .expect("the row is damaged");
    let refused = PgRecords::over(&store).expect_err("refused");
    assert!(matches!(refused, StoreError::Corrupt { .. }), "{refused}");
    assert!(
        refused.to_string().contains(
            "enrollment_host_identity/web-1 is not a store file this release reads: the identity approval does not decode"
        ),
        "{refused}"
    );
    let read = with_session(&database, |session| {
        records::read_all(session).expect("reads")
    });
    let refused = read.expect_err("refused");
    assert!(matches!(refused, StoreError::Corrupt { .. }), "{refused}");
}

/// Proves: the denied hosts are read from the deny rows the database holds: a retiring host and
/// a retired one, sorted, by a store made again over the tables too.
///
/// Catches: a deny list read from the heartbeat's copy of the hosts, which would leave out a
/// deny row of a host id with no host row.
#[test]
fn the_denied_hosts_are_read_from_the_deny_rows_of_the_database() {
    let database = TestDatabase::new();
    let (store, _) = database.open("records").expect("the store opens");
    let (enrollment, web_2, _) = approved_by(&store, "web-2", "ops");
    let (_, web_1, _) = approved_by(&store, "web-1", "ops");
    assert_eq!(enrollment.denied_hosts().expect("listed"), vec![]);
    enrollment.deny(&web_2).expect("retiring");
    enrollment.deny(&web_1).expect("retiring");
    enrollment.retire(&web_1).expect("retired");
    let zed = HostId::parse("zed-1").expect("a host id");
    enrollment.retire(&zed).expect("a deny row with no host");
    let wanted = vec![web_1, web_2, zed];
    assert_eq!(enrollment.denied_hosts().expect("listed"), wanted);
    let reopened = EnrollmentStore::new(Arc::new(PgRecords::over(&store).expect("records")));
    assert_eq!(reopened.denied_hosts().expect("listed"), wanted);
}

/// Proves: `0022` does not stop the release before it (migrations `0001` to `0021`) on a
/// database this release migrated and holds an identity approval in: its runner runs nothing and
/// is not refused, the state reads as this release loads it, and its write of a host row, naming
/// the columns `enrollment_hosts` has, leaves the approval's row; and the other way: this release
/// starts on a database the release before migrated and runs `0022` alone.
///
/// Catches: an approval that stops the previous coordinator by itself, a host write of the
/// release before that takes the approval away, and a migration this release does not run on a
/// database of the release before.
#[test]
fn the_release_before_starts_on_a_database_with_0022_and_this_one_on_its() {
    let database = TestDatabase::new();
    let (store, loaded) = database.open("this release").expect("the store opens");
    let (enrollment, id, key) = approved_by(&store, "web-1", INVENTORY_APPROVER);
    enrollment
        .approve_identity(&id, &key, "alice", NOW + 5)
        .expect("approved");
    store.stop();
    let (ran, before_loaded) = with_session(&database, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..21]).expect("the release before's set");
        (
            migrate::apply(session, &before).expect("the release before is not refused"),
            load_all(session, SPENT_NOW).expect("the state reads"),
        )
    });
    assert_eq!(ran, Vec::<i32>::new());
    assert_eq!(before_loaded, loaded);
    // The release before's write of a host row names the columns the table has: it works, and
    // leaves the approval where it was.
    database
        .run(&format!(
            "INSERT INTO enrollment_hosts (host_id, key_hash, machine_id, role, services, \
             duplicate, first_heartbeat_unix, first_serving_unix, host_group, moving_from, \
             facts_generation) \
             VALUES ('web-1', '\\x{}', 'm-web-1', 'web', '{{}}', true, 5, NULL, NULL, NULL, NULL) \
             ON CONFLICT (host_id) DO UPDATE SET (host_id, key_hash, machine_id, role, services, \
             duplicate, first_heartbeat_unix, first_serving_unix, host_group, moving_from, \
             facts_generation) = ROW(EXCLUDED.*)",
            hex(key.as_bytes())
        ))
        .expect("the release before's write is taken");
    assert_eq!(held(&database).len(), 1, "its approval is left");
    let reopened = EnrollmentStore::new(Arc::new(
        PgRecords::over(&database.open("again").expect("starts").0).expect("records"),
    ));
    assert!(
        reopened.identity_allowed(&id).expect("read"),
        "the approval of the host's key stands after the release before wrote the host"
    );

    let older = TestDatabase::new();
    let ran = with_session(&older, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..21]).expect("the release before's set");
        migrate::apply(session, &before).expect("the release before migrates")
    });
    assert_eq!(ran, (1..=21).collect::<Vec<i32>>());
    let (store, _) = older.open("this release").expect("this release starts");
    store.stop();
    assert_eq!(
        older.rows("SELECT version FROM schema_version WHERE version > 21"),
        vec![vec![Some("22".to_owned())]]
    );
    assert_eq!(held(&older), vec![]);
}
