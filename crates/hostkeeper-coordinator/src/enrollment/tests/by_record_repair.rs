//! The audit entry of an approval by a record, and the mark that the record is used, are one
//! change of the ops file made right after the store's write. When that change does not land
//! (a crash between the two files, or a write that fails) the approval stands, and the
//! coordinator's start makes the change again.

use std::sync::Arc;

use hostkeeper_enrollment::enroll::RequestState;
use hostkeeper_enrollment::validity::UnixClock;
use hostkeeper_proto::hostkeeper::agent::v1::EnrollState;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use super::support::{Audited, HostKey, NOW, Rig, add_of, audit_of, audited, claiming};
use crate::inventory::{InventoryRecord, RecordUse};
use crate::marks_support::operator;
use crate::ops::{AuditEntry, Write};
use crate::service::Coordinator;
use crate::testutil::{Log, Rig as CoordinatorRig, ops_of};

/// The data MAC of the record `db-1`.
const DB1_DATA: &str = "0c:c4:7a:00:00:02";

/// A coordinator over the files of `rig` and the enrollment `enrollment`, whose ops store (the
/// file beside the state file) reads the enrollment's clock.
fn start(
    rig: &CoordinatorRig,
    clock: &Rig,
    enrollment: Arc<crate::enrollment::Enrollment>,
) -> Coordinator {
    let clock = Arc::clone(&clock.clock);
    CoordinatorRig::start(&rig.dir, &rig.clock)
        .with_ops(ops_of(&rig.dir).with_unix(Arc::new(move || clock.now_unix())))
        .with_enrollment(enrollment)
}

/// The record of `host` in the ops state of `coordinator`.
fn record_of(coordinator: &Coordinator, host: &str) -> Option<InventoryRecord> {
    coordinator
        .ops
        .read(|state| state.inventory.iter().find(|r| r.host_id == host).cloned())
}

/// The text of the audit entry of an approval of `key` by the record `db-1`.
fn auto_entry(key: &HostKey) -> Audited {
    audited(
        "inventory",
        "enrollment.auto_approved",
        Some("db-1"),
        &format!(
            "request {}; record db-1; role storage; services none; claims machine_id=m-1 \
             addresses={DB1_DATA}; approved by its inventory record db-1",
            key.key_hash()
        ),
    )
}

/// Records `db-1` through a coordinator with no enrollment (so no request is tried), posts the
/// request of a new key to `enrollment` itself, and approves it by the record as an enrollment no
/// coordinator audits for does: the store holds the approval, and neither the audit entry nor the
/// mark is written. Gives the key.
fn crash_after_the_store_write(rig: &CoordinatorRig, enrollment: &Rig) -> HostKey {
    let clock = Arc::clone(&enrollment.clock);
    let plain = CoordinatorRig::start(&rig.dir, &rig.clock)
        .with_ops(ops_of(&rig.dir).with_unix(Arc::new(move || clock.now_unix())));
    plain
        .add_record(&operator(), add_of("db-1", &[DB1_DATA], ""))
        .expect("recorded");
    let key = HostKey::new();
    enrollment
        .enrollment
        .enroll(claiming(&key, "m-1", &[DB1_DATA], ""))
        .expect("posted");
    let records = plain.ops.read(|state| state.inventory.clone());
    let done = enrollment
        .enrollment
        .approve_by_record(&key.key_hash(), &records);
    assert!(
        matches!(
            done,
            crate::enrollment::ByRecord::Approved { entry: None, .. }
        ),
        "{done:?}"
    );
    assert_eq!(
        enrollment
            .enrollment
            .store()
            .request(&key.key_hash())
            .expect("read")
            .map(|view| view.state),
        Some(RequestState::Approved)
    );
    assert_eq!(
        record_of(&plain, "db-1").map(|r| r.used),
        Some(None),
        "no mark yet"
    );
    key
}

/// Proves: an approval by a record whose entry and mark did not land before a stop is made
/// again at the next start: the entry `enrollment.auto_approved` in its exact text, and the
/// record used by that request at the approval's time; a start after that writes nothing
/// again (the entry is recognised as the approval's), whether the audit holds it as such.
#[test]
fn a_crash_between_the_store_and_the_ops_write_is_repaired_at_the_next_start() {
    let enrollment = Rig::new("by-record-repair");
    let rig = CoordinatorRig::new("by-record-repair-coordinator");
    let key = crash_after_the_store_write(&rig, &enrollment);
    enrollment.clock.set(NOW + 500);

    let restarted = start(&rig, &enrollment, enrollment.restart());

    assert_eq!(
        audit_of(&restarted)
            .into_iter()
            .filter(|(_, action, _, _)| action == "enrollment.auto_approved")
            .collect::<Vec<_>>(),
        [auto_entry(&key)]
    );
    assert_eq!(
        record_of(&restarted, "db-1").and_then(|r| r.used),
        Some(RecordUse {
            request_id: key.key_hash().to_hex(),
            at_unix: NOW
        })
    );
    let listed = restarted
        .list_records(&operator(), proto::ListRecordsRequest::default())
        .expect("listed");
    assert_eq!(
        listed.records.first().map(|r| (r.state(), r.used_unix)),
        Some((proto::RecordState::Used, NOW))
    );

    let entries = audit_of(&restarted).len();
    let again = start(&rig, &enrollment, enrollment.restart());
    assert_eq!(audit_of(&again).len(), entries);
    assert_eq!(
        record_of(&again, "db-1").and_then(|r| r.used.map(|u| u.at_unix)),
        Some(NOW)
    );
}

/// Proves: the repair of an approval whose record was removed writes the entry and marks
/// nothing, and the repair of one whose record another approval used leaves that mark as it is.
#[test]
fn the_repair_leaves_a_removed_record_gone_and_a_used_record_as_it_was() {
    let enrollment = Rig::new("by-record-repair-gone");
    let rig = CoordinatorRig::new("by-record-repair-gone-coordinator");
    let key = crash_after_the_store_write(&rig, &enrollment);
    let used = RecordUse {
        request_id: "f".repeat(64),
        at_unix: NOW - 7,
    };
    let other = used.clone();
    CoordinatorRig::start(&rig.dir, &rig.clock)
        .ops
        .change(
            Write::BeforeMemory,
            AuditEntry::new("operator", "record_add"),
            move |state| {
                for record in &mut state.inventory {
                    record.used = Some(other.clone());
                }
            },
        )
        .expect("marked used by another");

    let restarted = start(&rig, &enrollment, enrollment.restart());
    assert_eq!(
        audit_of(&restarted)
            .into_iter()
            .filter(|(_, action, _, _)| action == "enrollment.auto_approved")
            .collect::<Vec<_>>(),
        [auto_entry(&key)]
    );
    assert_eq!(
        record_of(&restarted, "db-1").and_then(|r| r.used),
        Some(used),
        "the mark of the other approval stands"
    );

    restarted
        .remove_record(
            &operator(),
            proto::RemoveRecordRequest {
                host_id: "db-1".to_owned(),
                reason: "rebuilt".to_owned(),
            },
        )
        .expect("removed");
    assert_eq!(record_of(&restarted, "db-1"), None);
}

/// Proves: when the audit entry of the approval cannot be written, so that the change is not
/// made, the approval stands (the request is approved, the host id held, the answer is the one of
/// an approved request), nothing is audited or marked, the failure is logged at error with its
/// cause, and the next start, over a file that can be written, makes the change.
#[test]
fn a_failed_ops_write_leaves_the_approval_standing_and_the_next_start_repairs_it() {
    let enrollment = Rig::new("by-record-unwritable");
    let rig = CoordinatorRig::new("by-record-unwritable-coordinator");
    let clock = Arc::clone(&enrollment.clock);
    let plain = CoordinatorRig::start(&rig.dir, &rig.clock)
        .with_ops(ops_of(&rig.dir).with_unix(Arc::new(move || clock.now_unix())));
    plain
        .add_record(&operator(), add_of("db-1", &[DB1_DATA], ""))
        .expect("recorded");
    drop(plain);
    let clock = Arc::clone(&enrollment.clock);
    let failing = ops_of(&rig.dir)
        .with_unix(Arc::new(move || clock.now_unix()))
        .with_append(Box::new(|_| Err(std::io::Error::other("disk full"))));
    let coordinator = CoordinatorRig::start(&rig.dir, &rig.clock).with_ops(failing);
    let coordinator = coordinator.with_enrollment(Arc::clone(&enrollment.enrollment));
    let key = HostKey::new();
    let log = Log::default();
    let guard = log.install();
    let before = audit_of(&coordinator).len();

    let answer = coordinator
        .enroll(
            &enrollment.enrollment,
            claiming(&key, "m-1", &[DB1_DATA], ""),
        )
        .expect("posted");
    drop(guard);

    assert_eq!(answer.state, EnrollState::Unspecified as i32);
    assert_eq!(
        enrollment
            .enrollment
            .store()
            .request(&key.key_hash())
            .expect("read")
            .map(|view| view.state),
        Some(RequestState::Approved)
    );
    assert_eq!(
        audit_of(&coordinator).split_off(before),
        Vec::<Audited>::new(),
        "nothing is audited: the audit file takes no line"
    );
    assert_eq!(record_of(&coordinator, "db-1").map(|r| r.used), Some(None));
    let text = log.text();
    assert!(
        text.contains("ERROR")
            && text.contains("the approval stands")
            && text.contains("disk full"),
        "{text}"
    );

    let restarted = start(&rig, &enrollment, enrollment.restart());
    assert_eq!(
        audit_of(&restarted)
            .into_iter()
            .filter(|(_, action, _, _)| action == "enrollment.auto_approved")
            .collect::<Vec<_>>(),
        [auto_entry(&key)]
    );
    assert_eq!(
        record_of(&restarted, "db-1").and_then(|r| r.used.map(|u| u.request_id)),
        Some(key.key_hash().to_hex())
    );
}
