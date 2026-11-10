//! The host group of an enrolled host as the store answers it and writes it: a host is in no
//! group until a write puts it in one, a write changes the group and the move under way and
//! nothing else of the host's record, a host never approved or denied has no group to read or
//! write, and a record that cannot be read or written is an error, not a silent gap.

mod support;

use std::sync::Arc;

use hostkeeper_enrollment::enroll::records::{MemoryRecords, Records as _, pb};
use hostkeeper_enrollment::enroll::{ApprovalRecord, EnrollmentStore, Membership, StoreError};
use prost::Message as _;
use support::{HostKey, decision, host_id, request};

/// A store over `records` with the hosts `names` approved, in that order.
fn store_with(records: &Arc<MemoryRecords>, names: &[&str]) -> EnrollmentStore {
    let store = EnrollmentStore::new(records.clone());
    for (n, name) in names.iter().enumerate() {
        let key = HostKey::new();
        let id = store
            .post(request(&key, &format!("m-{n}")), 1)
            .expect("posted")
            .request_id;
        store
            .approve_recorded(
                &id,
                &key.key_hash(),
                &decision(name, false),
                &ApprovalRecord {
                    approver: "alice".to_owned(),
                    approved_unix: 1_800_000_000,
                    reason: "new rack".to_owned(),
                    mac_nonce: None,
                    mac_enrolled_since_unix: None,
                },
            )
            .expect("approved");
    }
    store
}

fn in_group(group: &str, moving_from: Option<&str>) -> Membership {
    Membership {
        host_group: group.to_owned(),
        moving_from: moving_from.map(str::to_owned),
    }
}

/// The host record of `name`, as the records hold it.
fn record_of(records: &MemoryRecords, name: &str) -> pb::Host {
    records
        .host(&host_id(name))
        .expect("read")
        .expect("a host record")
}

/// Proves: a host just approved is in no group and not moving; a write puts it in the group
/// `web`, moving from none, and answers the membership it held; the next write, to `db` moving
/// from `web`, answers that; a write that ends the move (no `moving_from`) leaves the group.
/// Each is read back by `membership` and by `enrolled_host`.
///
/// Catches: a group that is not kept, a previous membership answered wrong (a caller writing it
/// back would restore the wrong group), or a move that cannot be ended.
#[test]
fn a_write_changes_the_group_and_the_move_and_answers_what_was_there() {
    let records = Arc::new(MemoryRecords::new());
    let store = store_with(&records, &["db-1"]);
    let host = host_id("db-1");
    assert_eq!(
        store.membership(&host).expect("read"),
        Some(in_group("", None))
    );

    let before = store
        .set_membership(&host, in_group("web", Some("")))
        .expect("written");
    assert_eq!(before, in_group("", None));
    assert_eq!(
        store.membership(&host).expect("read"),
        Some(in_group("web", Some("")))
    );
    let enrolled = store.enrolled_host(&host).expect("read").expect("enrolled");
    assert_eq!(
        (
            enrolled.host_group.as_str(),
            enrolled.moving_from.as_deref()
        ),
        ("web", Some(""))
    );

    let before = store
        .set_membership(&host, in_group("db", Some("web")))
        .expect("written");
    assert_eq!(before, in_group("web", Some("")));

    let before = store
        .set_membership(&host, in_group("db", None))
        .expect("written");
    assert_eq!(before, in_group("db", Some("web")));
    assert_eq!(
        store.membership(&host).expect("read"),
        Some(in_group("db", None))
    );
}

/// Proves: a write of the group changes the host record's `host_group` and `moving_from` and no
/// other field: the key, the machine, the role, the services, the first-heartbeat time and the
/// generation of the host's facts are as they were.
///
/// Catches: a write that rebuilds the record and loses a time or the key, or one that raises
/// the facts' generation, which is another change's.
#[test]
fn a_write_of_the_group_changes_no_other_field_of_the_host_record() {
    let records = Arc::new(MemoryRecords::new());
    let store = store_with(&records, &["db-1"]);
    let host = host_id("db-1");
    store.note_heartbeat(&host, 1_800_000_050).expect("noted");
    let before = record_of(&records, "db-1");

    store
        .set_membership(&host, in_group("web", Some("")))
        .expect("written");

    let after = record_of(&records, "db-1");
    assert_eq!(
        pb::Host {
            host_group: String::new(),
            moving_from: None,
            ..after.clone()
        }
        .encode_to_vec(),
        before.encode_to_vec()
    );
    assert_eq!(after.first_heartbeat_unix, Some(1_800_000_050));
    assert_eq!(after.facts_generation, before.facts_generation);
}

/// Proves: a host id never approved and a host denied have no membership, and a write for
/// either is `UnknownHost` and writes nothing.
///
/// Catches: a denied host moved into a group (it would be counted in it), or a write that makes
/// a host record for an id never approved.
#[test]
fn a_host_never_approved_or_denied_has_no_group_to_read_or_write() {
    let records = Arc::new(MemoryRecords::new());
    let store = store_with(&records, &["db-1", "db-2"]);
    store.deny(&host_id("db-2")).expect("denied");

    for name in ["db-2", "db-9"] {
        let host = host_id(name);
        assert_eq!(store.membership(&host).expect("read"), None, "{name}");
        let refused = store
            .set_membership(&host, in_group("web", None))
            .expect_err("refused");
        assert!(
            matches!(&refused, StoreError::UnknownHost(id) if *id == host),
            "{name}: {refused:?}"
        );
    }
    assert_eq!(record_of(&records, "db-2").host_group, "");
    assert_eq!(records.host(&host_id("db-9")).expect("read"), None);
}

/// Proves: `memberships` lists every enrolled host by host id with its group, a denied host
/// left out.
///
/// Catches: a count of a group's hosts that includes a retired host, or a list out of order.
#[test]
fn memberships_list_the_enrolled_hosts_by_id_with_their_groups() {
    let records = Arc::new(MemoryRecords::new());
    let store = store_with(&records, &["db-2", "db-1", "db-3"]);
    store
        .set_membership(&host_id("db-1"), in_group("web", Some("db")))
        .expect("written");
    store
        .set_membership(&host_id("db-2"), in_group("db", None))
        .expect("written");
    store.deny(&host_id("db-3")).expect("denied");

    assert_eq!(
        store.memberships().expect("read"),
        vec![
            (host_id("db-1"), in_group("web", Some("db"))),
            (host_id("db-2"), in_group("db", None)),
        ]
    );
}

/// Proves: a read or write that fails in the records is an error from each of `membership`,
/// `memberships` and `set_membership`, whichever of its steps fails, and a failed write leaves
/// the host's group as it was.
///
/// Catches: a failed read taken for a host in no group, or a half-made write.
#[test]
fn a_failing_record_is_an_error_and_a_failed_write_changes_nothing() {
    let records = Arc::new(MemoryRecords::new());
    let store = store_with(&records, &["db-1"]);
    let host = host_id("db-1");

    for step in 1..=2 {
        records.fail_at(records.count() + step);
        store.membership(&host).expect_err("membership fails");
    }
    for step in 1..=3 {
        records.fail_at(records.count() + step);
        store.memberships().expect_err("memberships fails");
    }
    for step in 1..=4 {
        records.fail_at(records.count() + step);
        store
            .set_membership(&host, in_group("web", None))
            .expect_err("the write fails");
    }
    records.fail_at(0);
    assert_eq!(
        store.membership(&host).expect("read"),
        Some(in_group("", None))
    );
}
