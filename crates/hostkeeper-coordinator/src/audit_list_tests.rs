//! `Audit.List`'s time and name of each entry, its filter by token name, its newest page, and a
//! file in the format a released coordinator wrote. Every time is an injected clock's; nothing
//! sleeps.

use std::sync::Arc;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use crate::marks_support::{WEB, operator, rig, start};
use crate::operator::Caller;
use crate::ops::{AuditEntry, OpsStore, Write};
use crate::testutil::{Rig, ops_of};

const AT: u64 = 1_900_000_000;

/// The operator token named `name`.
fn named(name: &str) -> Caller {
    Caller::Operator(name.parse().expect("a token name"))
}

/// A rig over `web` and `db` whose audit file is stamped `AT`.
fn timed(test: &str) -> Rig {
    let mut rig = rig(test, "");
    rig.coordinator =
        start(&rig.dir, &rig.clock, "").with_ops(ops_of(&rig.dir).with_unix(Arc::new(|| AT)));
    rig
}

/// One change by `by` of `action` on `web`.
fn record(rig: &Rig, by: &str, action: &str) {
    let mut entry = AuditEntry::new(by, action);
    entry.service = Some(WEB.to_owned());
    rig.coordinator
        .ops()
        .change(Write::BeforeMemory, entry, |_| {})
        .expect("a recorded change");
}

fn page(rig: &Rig, request: proto::ListAuditRequest) -> proto::ListAuditResponse {
    rig.coordinator
        .audit_list(&operator(), request)
        .expect("a page")
}

fn seqs(page: &proto::ListAuditResponse) -> Vec<u64> {
    page.entries.iter().map(|e| e.seq).collect()
}

fn by_name(name: &str) -> proto::ListAuditRequest {
    proto::ListAuditRequest {
        by_name: Some(name.to_owned()),
        ..proto::ListAuditRequest::default()
    }
}

/// Proves: a change made with the token `alice` is listed with the time of the injected clock
/// and the name `alice` (its actor an operator); the call's own answer has the name and no
/// time.
///
/// Catches: a listed entry with no time or name, a time from another clock, or the name dropped
/// for the actor.
#[test]
fn audit_list_returns_time_and_token_name() {
    let rig = timed("audit-time-name");
    let done = rig
        .coordinator
        .mark_bad(
            &named("alice"),
            proto::MarkBadRequest {
                service: WEB.to_owned(),
                version: "1.0".to_owned(),
                reason: "crashes".to_owned(),
            },
        )
        .expect("marked")
        .entry
        .expect("an entry");
    assert_eq!(done.by_name.as_deref(), Some("alice"));
    assert_eq!(done.at_unix, None);

    let listed = page(&rig, proto::ListAuditRequest::default());
    assert_eq!(
        listed.entries,
        [proto::AuditEntry {
            seq: done.seq,
            by: i32::from(proto::Actor::Operator),
            action: "mark_bad".to_owned(),
            service: Some(WEB.to_owned()),
            node_id: None,
            reason: Some("1.0: crashes".to_owned()),
            at_unix: Some(AT),
            by_name: Some("alice".to_owned()),
            host_group: None,
        }]
    );
}

/// Proves: `by_name` keeps only the entries made under that name, alone or with another filter
/// and with `newest`; it names the coordinator's and CI's own entries too; a name no entry has
/// lists none; and a name that is no token name is refused.
///
/// Catches: a filter by name ignored, matched by prefix or by actor, or a bad name taken as no
/// filter.
#[test]
fn audit_list_filters_by_name() {
    let rig = timed("audit-by-name");
    record(&rig, "alice", "exclude");
    record(&rig, "bob", "exclude");
    record(&rig, "alice", "hold");
    record(&rig, "bob", "hold");
    record(&rig, "bobby", "hold");
    record(&rig, "coordinator", "last_good");

    let bob = page(&rig, by_name("bob"));
    assert_eq!(seqs(&bob), vec![2, 4]);
    assert!(
        bob.entries
            .iter()
            .all(|e| e.by_name.as_deref() == Some("bob"))
    );
    assert_eq!(bob.next_seq, 4);
    let bob_holds = page(
        &rig,
        proto::ListAuditRequest {
            action: Some("hold".to_owned()),
            ..by_name("bob")
        },
    );
    assert_eq!(seqs(&bob_holds), vec![4]);
    let newest_alice = page(
        &rig,
        proto::ListAuditRequest {
            limit: 1,
            newest: true,
            ..by_name("alice")
        },
    );
    assert_eq!(seqs(&newest_alice), vec![3]);
    assert_eq!(seqs(&page(&rig, by_name("coordinator"))), vec![6]);
    let nobody = page(&rig, by_name("carol"));
    assert!(nobody.entries.is_empty());
    assert_eq!(nobody.next_seq, 0);

    for bad in ["", "Bob", "bob smith", &"a".repeat(33)] {
        assert_eq!(
            rig.coordinator.audit_list(&operator(), by_name(bad)),
            Err(proto::AuditRefusal::InvalidRequest),
            "{bad:?}"
        );
    }
}

/// Proves: `newest` answers the newest `limit` entries after `since_seq`, oldest first, with
/// `next_seq` the newest of them, from the audit file and from a store with no file (in memory,
/// each entry with the time it was made); with `since_seq` past every entry it answers none.
///
/// Catches: a newest page answered newest first, from the start of the log, past `since_seq`,
/// or only from the file.
#[test]
fn audit_list_newest_pages_from_the_end() {
    let file = timed("audit-newest");
    let mut ring = rig("audit-newest-ring", "");
    ring.coordinator = start(&ring.dir, &ring.clock, "")
        .with_ops(OpsStore::volatile().with_unix(Arc::new(|| AT + 1)));
    for rig in [&file, &ring] {
        for _ in 0..20 {
            record(rig, "alice", "exclude");
        }
        let newest = |since_seq, limit| {
            page(
                rig,
                proto::ListAuditRequest {
                    since_seq,
                    limit,
                    newest: true,
                    ..proto::ListAuditRequest::default()
                },
            )
        };
        let last = newest(0, 5);
        assert_eq!(seqs(&last), (16..=20).collect::<Vec<u64>>());
        assert_eq!(last.next_seq, 20);
        assert_eq!(seqs(&newest(17, 5)), vec![18, 19, 20]);
        assert!(newest(20, 5).entries.is_empty());
        let oldest = page(
            rig,
            proto::ListAuditRequest {
                since_seq: 17,
                limit: 2,
                ..proto::ListAuditRequest::default()
            },
        );
        assert_eq!(seqs(&oldest), vec![18, 19]);
    }
    let times = |rig: &Rig| -> Vec<Option<u64>> {
        page(rig, proto::ListAuditRequest::default())
            .entries
            .iter()
            .map(|e| e.at_unix)
            .collect()
    };
    assert_eq!(times(&file), vec![Some(AT); 20]);
    assert_eq!(times(&ring), vec![Some(AT + 1); 20]);
}

/// Proves: an audit file in the format a released coordinator wrote (the same five keys, no
/// field added on disk) lists every entry with its time, name and actor, and the store numbers
/// on from its last line.
///
/// Catches: a reader that needs a key the released file does not have, or a change of the line
/// format that leaves older files unread.
#[test]
fn audit_reads_existing_file_format() {
    let mut rig = rig("audit-released-format", "");
    std::fs::write(
        rig.dir.path().join("desired.json.ops.audit"),
        include_str!("../tests/fixtures/audit/released.ops.audit"),
    )
    .expect("the fixture copied");
    rig.coordinator = start(&rig.dir, &rig.clock, "").with_ops(ops_of(&rig.dir));

    let listed = page(&rig, proto::ListAuditRequest::default());
    let got: Vec<_> = listed
        .entries
        .iter()
        .map(|e| {
            (
                e.seq,
                e.at_unix,
                e.by_name.as_deref(),
                e.by,
                e.action.as_str(),
                e.node_id.as_deref(),
            )
        })
        .collect();
    let (operator_actor, coordinator, ci) = (
        i32::from(proto::Actor::Operator),
        i32::from(proto::Actor::Coordinator),
        i32::from(proto::Actor::Ci),
    );
    assert_eq!(
        got,
        [
            (
                1,
                Some(1_800_000_000),
                Some("alice"),
                operator_actor,
                "exclude",
                Some("s2")
            ),
            (
                2,
                Some(1_800_000_060),
                Some("coordinator"),
                coordinator,
                "last_good",
                None
            ),
            (3, Some(1_800_000_120), Some("ci"), ci, "lift_halt", None),
            (
                4,
                Some(1_800_000_180),
                Some("mdm-nonce"),
                operator_actor,
                "enroll_approve",
                Some("mac-1")
            ),
        ]
    );
    assert_eq!(listed.entries[0].reason.as_deref(), Some("bad disk"));
    assert_eq!(listed.entries[2].service, None);

    record(&rig, "bob", "hold");
    assert_eq!(seqs(&page(&rig, by_name("bob"))), vec![5]);
}
