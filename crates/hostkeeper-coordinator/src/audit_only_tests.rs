//! The audit-only append ([`OpsStore::append_audit_only`], `docs/design/console-broker.md`
//! decision D4): a numbered line that changes no operations state, on the files and on the fake;
//! the Postgres store's start is in `store/postgres/tests/audit_only.rs`.

use std::fs;
use std::sync::Arc;

use serde::Deserialize;

use crate::audit_actions::{self, AuditOnly};
use crate::audit_file::{self, AuditLine};
use crate::ops::{AUDIT_RING, AuditEntry, OpsError, OpsState, OpsStore, Write};
use crate::store::{Fake, audit_failure, store_failure};
use crate::testutil::scratch;

const AT: u64 = 1_800_000_000;

/// The store over the ops file `file`, its audit stamped `AT`.
fn open(file: &std::path::Path) -> OpsStore {
    OpsStore::open(file)
        .expect("opened")
        .with_unix(Arc::new(|| AT))
}

/// One change by `by` of `action`, written before memory, as numbered.
fn make(store: &OpsStore, by: &str, action: &str) -> AuditEntry {
    store
        .change(Write::BeforeMemory, AuditEntry::new(by, action), |_| {})
        .expect("a change")
}

/// A console open by `alice` on `node-02`.
fn open_line(store: &OpsStore) -> Result<AuditEntry, OpsError> {
    store.append_audit_only(
        AuditOnly::ConsoleOpen,
        "alice",
        Some("node-02"),
        Some("boot order; session=ab12; kind=serial"),
    )
}

/// The numbers of `entries`, in order.
fn seqs(entries: &[AuditEntry]) -> Vec<u64> {
    entries.iter().map(|entry| entry.seq).collect()
}

/// Proves: an audit-only line takes the number after the newest change's, lands in the audit
/// file as a line of the shape every change's has (by exact equality), and leaves the ring, the
/// state and the ops file as they were; the next change takes the number after it, and a store
/// opened again on the files numbers on from it with a ring that still holds the changes only.
///
/// Catches: a console line that enters the `/v1/status` ring or rewrites the ops file, one that
/// reuses a change's number or is numbered from the ring alone, and a restart that brings it into
/// the ring or numbers the next line again.
#[test]
fn an_audit_only_line_takes_the_next_seq_and_stays_out_of_the_status_ring() {
    let dir = scratch("audit-only-next-seq");
    let file = dir.path().join("ops.json");
    let store = open(&file);
    let include = make(&store, "alice", "include");
    let held = fs::read(&file).expect("the ops file");

    let line = open_line(&store).expect("appended");
    assert_eq!(line.seq, 2);
    assert_eq!(line.action, "console_open");
    assert_eq!(store.state().audit, std::slice::from_ref(&include));
    assert_eq!(
        fs::read(&file).expect("the ops file"),
        held,
        "not rewritten"
    );
    let text = fs::read_to_string(audit_file::path_of(&file)).expect("the audit file");
    let console = concat!(
        r#"{"seq":2,"at_unix":1800000000,"by":"alice","action":"console_open","detail":"#,
        r#"{"node_id":"node-02","reason":"boot order; session=ab12; kind=serial"}}"#,
        "\n"
    );
    assert_eq!(text.lines().count(), 2);
    assert!(text.ends_with(console), "{text}");

    let exclude = make(&store, "alice", "exclude");
    assert_eq!(exclude.seq, 3);
    assert_eq!(seqs(&store.state().audit), [1, 3]);
    assert_eq!(seqs(&store.audit_log().expect("the log")), [1, 2, 3]);
    drop(store);

    let again = open(&file);
    assert_eq!(seqs(&again.state().audit), [1, 3]);
    let power = again
        .append_audit_only(AuditOnly::PowerAction, "alice", Some("node-02"), None)
        .expect("appended");
    assert_eq!(power.seq, 4);
    assert_eq!(seqs(&again.state().audit), [1, 3]);
}

/// Proves: an audit-only line that the audit file cannot take is refused `AuditUnavailable`
/// with nothing written (no ops file, no ring entry), and once the file can be written the next
/// line takes the number the refused one would have had.
///
/// Catches: a console session opened with no `console_open` line (the caller would act on an
/// answer that is not an error), and a number spent by a line that never landed.
#[test]
fn console_open_is_not_made_when_the_audit_line_cannot_be_appended() {
    let dir = scratch("audit-only-refused");
    let file = dir.path().join("ops.json");
    let audit = audit_file::path_of(&file);
    let store = open(&file);
    fs::create_dir(&audit).expect("a directory in its place");

    let refused = open_line(&store).expect_err("refused");
    assert!(
        matches!(refused, OpsError::AuditUnavailable(_)),
        "{refused:?}"
    );
    assert_eq!(store.state().audit, []);
    assert!(!file.exists(), "no ops file is written");

    fs::remove_dir(&audit).expect("the directory removed");
    assert_eq!(open_line(&store).expect("appended").seq, 1);
}

/// Proves: an audit-only line waits for a change pending in memory: while the store cannot
/// commit, the line is refused `AuditUnavailable` and nothing lands; once it can, the pending
/// change lands first and the line after it, in their numbers' order, and the committed ring
/// holds the change only.
///
/// Catches: a console line committed ahead of an earlier change (its number above the pending
/// one's, which would then be passed over as already in the audit), or one appended while the
/// change before it is still unwritten.
#[test]
fn an_audit_only_line_lands_after_what_is_pending() {
    let fake = Fake::new();
    let store = OpsStore::on_fake(&fake, OpsState::default());
    fake.refuse(|_| Some(audit_failure()));
    let raise = store
        .change(
            Write::AfterMemory,
            AuditEntry::new("coordinator", "mark_bad"),
            |_| {},
        )
        .expect("a raise stands in memory");
    assert!(store.halt_unwritten());

    let refused = open_line(&store).expect_err("refused while the raise is pending");
    assert!(
        matches!(refused, OpsError::AuditUnavailable(_)),
        "{refused:?}"
    );
    assert_eq!(fake.audit(), []);

    fake.heal();
    let line = open_line(&store).expect("appended");
    assert_eq!((raise.seq, line.seq), (1, 2));
    let landed: Vec<(u64, String)> = fake
        .audit()
        .into_iter()
        .map(|line| (line.seq, line.into_logged().entry.action))
        .collect();
    assert_eq!(
        landed,
        [(1, "mark_bad".to_owned()), (2, "console_open".to_owned())]
    );
    assert_eq!(seqs(&fake.ops().audit), [1]);
    assert!(!store.halt_unwritten());
}

/// Proves: whatever keeps an audit-only line out of the store, a commit that fails for its audit
/// or for the store as a whole, the append answers `AuditUnavailable`, the one error its caller
/// acts on, and keeps nothing.
///
/// Catches: a store failure answered as another error, which a console call would map to a
/// refusal other than `audit_unavailable`, or treat as written.
#[test]
fn every_failure_of_an_audit_only_line_is_audit_unavailable() {
    for why in [store_failure, audit_failure] {
        let fake = Fake::new();
        let store = OpsStore::on_fake(&fake, OpsState::default());
        fake.refuse(move |_| Some(why()));
        let refused = open_line(&store).expect_err("refused");
        assert!(
            matches!(refused, OpsError::AuditUnavailable(_)),
            "{refused:?}"
        );
        assert_eq!(fake.audit(), []);
        assert_eq!(fake.commits(), 0);
    }
}

/// Proves: more audit-only lines than the ring holds push no change out of it: after a change
/// and `AUDIT_RING + 1` console lines, the ring is that one change, and every line is in the
/// audit, numbered in turn.
///
/// Catches: console lines that enter the ring, where a burst of them would push the operators'
/// changes out of `/v1/status`.
#[test]
fn a_burst_of_audit_only_lines_pushes_no_change_out_of_the_ring() {
    let fake = Fake::new();
    let store = OpsStore::on_fake(&fake, OpsState::default());
    let include = make(&store, "alice", "include");
    for _ in 0..=AUDIT_RING {
        open_line(&store).expect("appended");
    }
    assert_eq!(store.state().audit, [include]);
    let numbers: Vec<u64> = fake.audit().iter().map(|line| line.seq).collect();
    let wanted: Vec<u64> = (1..=AUDIT_RING as u64 + 2).collect();
    assert_eq!(numbers, wanted);
}

/// The audit line reader of the release before: not strict about keys it does not know, as
/// `audit_actions_tests` has it.
#[derive(Deserialize)]
struct PreviousLine {
    seq: u64,
    by: String,
    action: String,
    detail: PreviousDetail,
}

#[derive(Deserialize)]
struct PreviousDetail {
    node_id: Option<String>,
    reason: Option<String>,
}

/// Proves: the release before reads every file this release writes with the console's lines in
/// it: the ops file (which those lines leave as it was) starts the release before's strict
/// reader, every audit line reads with the release before's reader as the number, name, action,
/// node and reason it was written with, and a line holds no key the release before does not
/// write. The console's own files (operators, targets and the BMC credential) are the
/// operator's, written by no release and opened by no release before this one.
///
/// Catches: an audit-only line in a shape the release before cannot page or number on from, a
/// new key in a line or in the ops file, and an ops file the console's lines change.
#[test]
fn the_previous_release_still_reads_every_file_this_release_writes() {
    let dir = scratch("audit-only-previous");
    let file = dir.path().join("ops.json");
    let store = open(&file);
    make(&store, "alice", "include");
    let held = fs::read_to_string(&file).expect("the ops file");
    for action in AuditOnly::ALL {
        store
            .append_audit_only(action, "alice", Some("node-02"), Some("why"))
            .expect("appended");
    }
    let text = fs::read_to_string(&file).expect("the ops file");
    assert_eq!(text, held);
    crate::ops_downgrade_tests::previous::reads(&text).expect("the release before starts on it");

    let lines = fs::read_to_string(audit_file::path_of(&file)).expect("the audit file");
    let mut read = Vec::new();
    for line in lines.lines().skip(1) {
        let old: PreviousLine = serde_json::from_str(line).expect("the release before reads it");
        assert_eq!(
            (old.by.as_str(), old.detail.node_id.as_deref()),
            ("alice", Some("node-02"))
        );
        assert_eq!(old.detail.reason.as_deref(), Some("why"));
        read.push((old.seq, old.action));
        let keys: serde_json::Value = serde_json::from_str(line).expect("JSON");
        let mut top: Vec<&str> = keys
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        top.sort_unstable();
        assert_eq!(top, ["action", "at_unix", "by", "detail", "seq"]);
        let _: AuditLine = serde_json::from_str(line).expect("this release reads it");
    }
    let wanted: Vec<(u64, String)> = (2..)
        .zip(AuditOnly::ALL.iter().map(|only| only.as_str().to_owned()))
        .collect();
    assert_eq!(read, wanted);
    assert_eq!(
        wanted
            .iter()
            .map(|(_, action)| action.as_str())
            .collect::<Vec<_>>(),
        [
            audit_actions::CONSOLE_OPEN,
            audit_actions::CONSOLE_CLOSE,
            audit_actions::CONSOLE_DENIED,
            audit_actions::POWER_ACTION,
            audit_actions::BOOT_DEVICE_SET,
            audit_actions::CONSOLE_COORDINATOR_START,
        ]
    );
}
