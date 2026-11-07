//! `Enrollment.OpenRecord` over a coordinator with enrollment: its answer says whether a host is
//! enrolled against the record and what the store holds for its host id, and a store that cannot
//! be read refuses the call before anything is written.

use std::sync::Arc;

use hostkeeper_enrollment::fs::FaultFs;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use super::support::{HostKey, Rig, add_of, aligned, approve, claiming};
use crate::enrollment::os_random;
use crate::marks_support::operator;

use proto::EnrollmentRefusal as Refusal;

/// The data MAC of the record `db-1`.
const DB1_DATA: &str = "0c:c4:7a:00:00:02";

fn open_for(host: &str, secs: u64) -> proto::OpenRecordRequest {
    proto::OpenRecordRequest {
        host_id: host.to_owned(),
        approve_for_secs: secs,
        ..proto::OpenRecordRequest::default()
    }
}

/// Proves: the answer of `OpenRecord` is the record as the list shows it: `enrolled` when a host
/// enrolled against it (here approved by an operator, which does not use the record), and
/// `host_held` what the enrollment store holds for its host id.
#[test]
fn the_answer_says_whether_a_host_is_enrolled_against_the_record_and_what_the_store_holds() {
    let enrollment = Rig::new("open-record-answer");
    let rig = aligned("open-record-answer-coordinator", &enrollment);
    let key = HostKey::new();
    enrollment
        .enrollment
        .enroll(claiming(&key, "m-1", &[DB1_DATA], ""))
        .expect("posted");
    approve(&enrollment.enrollment, &key, "db-1", false);
    rig.coordinator
        .add_record(&operator(), add_of("db-1", &[DB1_DATA], ""))
        .expect("added");

    let opened = rig
        .coordinator
        .open_record(&operator(), open_for("db-1", 3600))
        .expect("opened");

    let record = opened.record.expect("a record");
    assert_eq!(
        (record.enrolled, record.host_held(), record.state()),
        (true, proto::HostHeld::Approved, proto::RecordState::Open)
    );
}

/// Proves: when the enrolled hosts or what the store holds for the host id cannot be read,
/// `OpenRecord` is refused `store_failed` and the window stays as it was; a failure of the store
/// after the change (the pending requests tried against the record) refuses nothing.
#[test]
fn an_open_whose_store_cannot_be_read_is_store_failed_and_changes_nothing() {
    let fs = Arc::new(FaultFs::new());
    let enrollment = Rig::over("open-record-failing", fs.clone(), os_random());
    let rig = aligned("open-record-failing-coordinator", &enrollment);
    rig.coordinator
        .add_record(&operator(), add_of("db-1", &[DB1_DATA], ""))
        .expect("added");
    let window = |rig: &crate::testutil::Rig| {
        rig.coordinator
            .ops
            .read(|state| state.inventory.first().map(|r| r.approve_until_unix))
    };
    let at = fs.count();
    rig.coordinator
        .open_record(&operator(), open_for("db-1", 3600))
        .expect("opened");
    let operations = fs.count() - at;
    assert!(operations >= 3, "{operations}");

    let mut refused = 0;
    for n in 1..=operations {
        let before = window(&rig);
        fs.fail_at(fs.count() + n);
        let done = rig
            .coordinator
            .open_record(&operator(), open_for("db-1", 60));
        match done {
            Err(Refusal::StoreFailed) => {
                refused += 1;
                assert_eq!(window(&rig), before, "operation {n}");
            }
            Ok(_) => assert_ne!(window(&rig), before, "operation {n}"),
            Err(other) => panic!("operation {n}: {other:?}"),
        }
    }
    assert!(refused >= 2, "{refused}");
}
