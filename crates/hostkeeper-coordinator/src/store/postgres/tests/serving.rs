//! When a host first served, in the database: the enrollment store's `note_serving` over the
//! Postgres records writes `first_serving_unix` into the host's row once since its approval, and
//! an approval writes the row without it (network-install.md section 11, N11).

use std::sync::Arc;

use hostkeeper_enrollment::enroll::{Claims, Decision, EnrollmentStore, NewRequest};
use hostkeeper_enrollment::names::{HostId, Role};

use super::TestDatabase;
use super::coordinator::approved;
use crate::enrollment::tests::support::{HostKey, NOW};
use crate::store::postgres::PgRecords;

/// A new key of `id` approved in `enrollment`, taking the host id over.
pub(super) fn approve_again(enrollment: &EnrollmentStore, id: &HostId) {
    let key = HostKey::new();
    let posted = enrollment
        .post(
            NewRequest {
                csr_der: key.csr(),
                claims: Claims {
                    machine_id: "m-2".to_owned(),
                    ..Claims::default()
                },
                image: "hostkeeper-image-1".to_owned(),
                renewal_of: None,
                mac_nonce_presented: false,
            },
            NOW,
        )
        .expect("posted");
    enrollment
        .approve(
            &posted.request_id,
            &key.key_hash(),
            &Decision {
                host_id: id.clone(),
                role: Role::parse("web").expect("a role"),
                services: Vec::new(),
                replace: true,
                host_group: None,
            },
        )
        .expect("approved again");
}

/// Proves: over Postgres, a host's first serving heartbeat is written into its row once: a host
/// id never approved writes nothing, the first note stamps the column and leaves the first
/// heartbeat as it was, a later note runs no statement and keeps the first time, a store made
/// again over the tables reads the time back and writes nothing, and a new approval of the host
/// id clears the column until the host serves again.
///
/// Catches: a Postgres store that drops `first_serving_unix` on a write or a read, a later
/// heartbeat that moves the time, or an approval that keeps the time of the key it replaced.
#[test]
fn the_first_serving_heartbeat_is_written_once_into_the_hosts_row() {
    let database = TestDatabase::new();
    let (store, _) = database.open("records").expect("the store opens");
    let (enrollment, id) = approved(&store, "web-1");
    let row = || {
        database
            .rows("SELECT host_id, first_heartbeat_unix, first_serving_unix FROM enrollment_hosts")
    };
    let web_1 = |heartbeat: Option<u64>, serving: Option<u64>| {
        vec![vec![
            Some("web-1".to_owned()),
            heartbeat.map(|at| at.to_string()),
            serving.map(|at| at.to_string()),
        ]]
    };

    let statements = store.statements();
    let never = HostId::parse("web-9").expect("a host id");
    enrollment.note_serving(&never, NOW + 30).expect("noted");
    assert_eq!(store.statements(), statements, "a host never approved");
    assert_eq!(row(), web_1(None, None));

    enrollment.note_heartbeat(&id, NOW + 50).expect("noted");
    enrollment.note_serving(&id, NOW + 60).expect("noted");
    assert_eq!(row(), web_1(Some(NOW + 50), Some(NOW + 60)));

    let written = store.statements();
    enrollment.note_serving(&id, NOW + 90).expect("noted");
    assert_eq!(store.statements(), written, "the second ran nothing");
    assert_eq!(row(), web_1(Some(NOW + 50), Some(NOW + 60)));

    let again = EnrollmentStore::new(Arc::new(PgRecords::over(&store).expect("records")));
    assert_eq!(
        again
            .enrolled_host(&id)
            .expect("read")
            .expect("enrolled")
            .first_serving_unix,
        Some(NOW + 60),
        "read back from the column"
    );
    let loaded = store.statements();
    again.note_serving(&id, NOW + 120).expect("noted");
    assert_eq!(
        store.statements(),
        loaded,
        "a store made again writes nothing"
    );
    assert_eq!(row(), web_1(Some(NOW + 50), Some(NOW + 60)));

    approve_again(&again, &id);
    assert_eq!(row(), web_1(None, None), "an approval clears both times");
    again.note_serving(&id, NOW + 200).expect("noted");
    assert_eq!(row(), web_1(None, Some(NOW + 200)));
}
