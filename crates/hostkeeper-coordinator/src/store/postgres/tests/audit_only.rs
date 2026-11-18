//! The audit-only append on the Postgres store ([`crate::ops::OpsStore::append_audit_only`]):
//! its lines are rows of the `audit` table, numbered with the changes', and a start that rebuilds
//! the ring from that table leaves them out of it.

use std::sync::Arc;

use super::{AT, TestDatabase};
use crate::audit_actions::AuditOnly;
use crate::ops::{AUDIT_RING, AuditEntry, OpsStore, Write};

/// The numbers of `entries`, in order.
fn seqs(entries: &[AuditEntry]) -> Vec<u64> {
    entries.iter().map(|entry| entry.seq).collect()
}

/// Proves: on Postgres, audit-only lines are rows of the audit numbered between the changes',
/// the ring holds the changes only, and a start on the database rebuilds that ring without them,
/// even when they are the newest `AUDIT_RING` rows, and numbers the next line after the newest
/// row.
///
/// Catches: a start that rebuilds the ring from the newest rows whatever their action, which
/// would put console lines into `/v1/status` after every restart and push the operators' changes
/// out of it, and a start that numbers the next line from the ring alone and reuses a number.
#[test]
fn a_start_on_postgres_leaves_audit_only_lines_out_of_the_ring() {
    let database = TestDatabase::new();
    let (store, loaded) = database.open("first").expect("the store opens");
    let ops = OpsStore::over(Arc::new(store.clone()), loaded.ops).with_unix(Arc::new(|| AT));
    let change = |action: &str| {
        ops.change(
            Write::BeforeMemory,
            AuditEntry::new("alice", action),
            |_| {},
        )
        .expect("a change")
    };
    change("include");
    ops.append_audit_only(
        AuditOnly::ConsoleOpen,
        "alice",
        Some("node-02"),
        Some("why"),
    )
    .expect("appended");
    change("exclude");
    for _ in 0..AUDIT_RING {
        ops.append_audit_only(
            AuditOnly::PowerAction,
            "alice",
            Some("node-02"),
            Some("why"),
        )
        .expect("appended");
    }
    let newest = AUDIT_RING as u64 + 3;
    assert_eq!(seqs(&ops.state().audit), [1, 3]);
    assert_eq!(ops.audit_log().expect("the log").len() as u64, newest);
    drop(ops);
    store.stop();

    let (again, loaded) = database.open("second").expect("the store opens again");
    assert_eq!(seqs(&loaded.ops.audit), [1, 3]);
    let ops = OpsStore::over(Arc::new(again), loaded.ops).with_unix(Arc::new(|| AT));
    let close = ops
        .append_audit_only(AuditOnly::ConsoleClose, "alice", Some("node-02"), None)
        .expect("appended");
    assert_eq!(close.seq, newest + 1);
    assert_eq!(seqs(&ops.state().audit), [1, 3]);
}
