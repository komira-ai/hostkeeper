//! An operations store takes the state the rows hold in place of memory only when memory has
//! nothing the rows lack ([`OpsStore::take_rows`]).

use std::sync::Arc;

use crate::host_move::ACTION_MOVE;
use crate::ops::{AuditEntry, OpsState, OpsStore, ServiceOps, Write};
use crate::store::Fake;

/// A store over the fake, with one committed change (audit number 1).
fn seeded() -> (Fake, OpsStore) {
    let fake = Fake::new();
    let store = OpsStore::over(Arc::new(fake.clone()), OpsState::default());
    store
        .change(
            Write::BeforeMemory,
            AuditEntry::new("operator", "hold"),
            |state| {
                state
                    .services
                    .insert("web".to_owned(), ServiceOps::default());
            },
        )
        .expect("seeded");
    assert_eq!(store.newest_seq(), 1);
    (fake, store)
}

/// What the rows hold after another coordinator moved the host `h1` and marked `db`: the store's
/// state with one more audit entry, number 2.
fn rows_after_another(store: &OpsStore) -> OpsState {
    let mut fresh = store.state();
    fresh
        .services
        .insert("db".to_owned(), ServiceOps::default());
    let mut moved = AuditEntry::new("operator", ACTION_MOVE);
    moved.seq = 2;
    moved.node_id = Some("h1".to_owned());
    fresh.audit.push(moved);
    fresh
}

/// Proves: the state of the rows replaces memory when memory still ends at the audit number the
/// reader knew: the other coordinator's service is held, the newest number is the rows', and the
/// facts of the host the other coordinator moved are at that number.
///
/// Catches: a coordinator that keeps the state it loaded at start after another coordinator
/// committed a change, or one that leaves a host's facts generation behind its move.
#[test]
fn the_rows_state_replaces_memory_that_ends_where_the_reader_knew() {
    let (_fake, store) = seeded();
    let fresh = rows_after_another(&store);
    assert!(store.take_rows(fresh, 1));
    assert!(store.state().services.contains_key("db"));
    assert_eq!(store.newest_seq(), 2);
    assert_eq!(store.facts_generation("h1"), 2);
}

/// Proves: memory that moved since the reader knew its newest number (a change of this
/// coordinator committed meanwhile) is left as it is: the rows were read before that change.
///
/// Catches: a read of the rows taken in over a change made after it, which loses the change.
#[test]
fn memory_that_moved_since_the_read_is_left_as_it_is() {
    let (_fake, store) = seeded();
    let fresh = rows_after_another(&store);
    assert!(!store.take_rows(fresh, 0));
    assert!(!store.state().services.contains_key("db"));
    assert_eq!(store.newest_seq(), 1);
}

/// Proves: memory that holds a change not yet committed (a halt whose commit failed) is left as
/// it is, so the halt is not lost to the rows' older state.
///
/// Catches: a refresh from the rows that drops a halt standing in memory and waiting to be
/// committed.
#[test]
fn memory_with_a_change_not_yet_committed_is_left_as_it_is() {
    let (fake, store) = seeded();
    fake.fail();
    let _ = store.change(
        Write::Halt,
        AuditEntry::new("coordinator", "halt"),
        |state| {
            state
                .services
                .insert("halted".to_owned(), ServiceOps::default());
        },
    );
    assert!(store.halt_unwritten());
    let mut fresh = rows_after_another(&store);
    fresh.audit.clear();
    assert!(!store.take_rows(fresh, store.newest_seq()));
    assert!(store.state().services.contains_key("halted"));
    assert!(!store.state().services.contains_key("db"));
}

/// Proves: the desired state of the rows replaces memory when the step that takes the operations
/// state in agrees, and not when it does not: the state of the rows is taken in with the
/// operations state or not at all.
///
/// Catches: a desired state taken in over an operations state that was left as it was, which
/// pairs a version of one commit with the plan position of another.
#[test]
fn the_desired_state_of_the_rows_is_taken_in_with_the_operations_state_or_not_at_all() {
    use crate::desired::{DesiredStore, Held};
    use crate::testutil::{LEGACY, desired};

    let store = DesiredStore::over(Arc::new(Fake::new()), Held::default());
    let mut fresh = Held::default();
    fresh
        .desired
        .insert(LEGACY.to_owned(), desired("2.0", 0, 0));
    assert!(!store.take_rows(fresh.clone(), Box::new(|| false)));
    assert!(store.get(LEGACY).is_none());
    assert!(store.take_rows(fresh, Box::new(|| true)));
    assert_eq!(
        store.get(LEGACY).expect("a state").version.to_string(),
        "2.0"
    );
}

/// Proves: memory whose change was committed but whose audit line was not (the line waits to be
/// written) is left as it is: the rows hold the change without the line, and taking them in
/// would drop the line that waits.
///
/// Catches: a refresh from the rows that loses an audit line still waiting to be written,
/// because the state it belongs to is committed.
#[test]
fn memory_with_an_audit_line_not_yet_written_is_left_as_it_is() {
    let (fake, store) = seeded();
    fake.refuse(|commit| (!commit.audit.is_empty()).then(crate::store::audit_failure));
    let _ = store.change(
        Write::Halt,
        AuditEntry::new("coordinator", "halt"),
        |state| {
            state
                .services
                .insert("halted".to_owned(), ServiceOps::default());
        },
    );
    assert!(
        fake.ops().services.contains_key("halted"),
        "the state is committed without its line"
    );
    assert!(store.halt_unwritten(), "the line waits");
    let fresh = fake.ops();
    assert!(!store.take_rows(fresh, store.newest_seq()));
    assert!(store.halt_unwritten(), "the line still waits");
}
