//! The book of the SSH tunnels over the Postgres store (`docs/design/ssh-through-coordinator.md`,
//! sections 7 and 8; migration `0021`): the rules the memory book passes
//! (`crate::ssh_book::tests`), run on the Postgres book, and what only a database shows: one
//! chain across the ledger's lines and the book's, two instances over one database, and the
//! failures of the database.

use std::sync::Arc;

use super::{TestDatabase, off_runtime};
use crate::ssh_book::tests::{self as rules, Entry, World, book_lines};
use crate::ssh_book::{Book, BookError, Cause, Closing, OpenRefused, Opening, PinRefused, Polled};
use crate::ssh_ledger::{GENESIS, Issue, Ledger, Line, Revocation, chain_hash};
use crate::store::postgres::Postgres;
use crate::store::postgres::ssh::PgLedger;
use crate::store::postgres::ssh_book::PgBook;

/// A database, its store, and the ledger and the book over it.
struct PgWorld {
    book: PgBook,
    ledger: PgLedger,
    store: Postgres,
    db: TestDatabase,
}

impl PgWorld {
    fn new() -> PgWorld {
        let db = TestDatabase::new();
        let (store, _) = db.open("coordinator").expect("the store opens");
        PgWorld {
            book: PgBook::new(store.clone()),
            ledger: PgLedger::new(store.clone()),
            store,
            db,
        }
    }

    fn numbers(&self, sql: &str) -> Vec<u64> {
        self.db
            .rows(sql)
            .into_iter()
            .map(|row| {
                row[0]
                    .as_deref()
                    .and_then(|n| n.parse().ok())
                    .expect("a number")
            })
            .collect()
    }
}

impl World for PgWorld {
    fn book(&self) -> &dyn Book {
        &self.book
    }

    fn issue(&self, operator: &str, host: &str, ttl_secs: u64) -> u64 {
        self.ledger
            .issue(&Issue {
                operator,
                host_id: host,
                fingerprint: "SHA256:abc",
                ttl_secs,
                reason: "",
                api_token: None,
            })
            .expect("issued")
            .serial
    }

    fn revoke(&self, serial: u64) {
        self.ledger
            .revoke("carol", Revocation::Serial(serial))
            .expect("revoked");
    }

    fn lines(&self) -> Vec<Entry> {
        self.db
            .rows("SELECT by_name, action, subject, detail FROM ssh_audit ORDER BY seq")
            .into_iter()
            .map(|row| {
                let text = |i: usize| row[i].clone().expect("a column");
                (text(0), text(1), row[2].clone(), text(3))
            })
            .collect()
    }

    fn tunnel_ids(&self) -> Vec<u64> {
        self.numbers("SELECT id FROM ssh_tunnel ORDER BY id")
    }

    fn seen(&self, host: &str) -> Option<u64> {
        self.numbers(&format!(
            "SELECT seen_unix FROM ssh_host WHERE host_id = '{host}'"
        ))
        .first()
        .copied()
    }

    fn shift_host(&self, host: &str, secs: u64, back: bool) {
        let sign = if back { '-' } else { '+' };
        self.db
            .run(&format!(
                "UPDATE ssh_host SET seen_unix = seen_unix {sign} {secs} WHERE host_id = '{host}'"
            ))
            .expect("shifted");
    }

    fn age_tunnel(&self, id: u64, secs: u64) {
        self.db
            .run(&format!(
                "UPDATE ssh_tunnel SET opened_unix = opened_unix - {secs}, \
                 last_seen_unix = last_seen_unix - {secs} WHERE id = {id}"
            ))
            .expect("aged");
    }

    fn break_audit(&self) {
        self.db
            .run("ALTER TABLE ssh_audit RENAME TO ssh_audit_away")
            .expect("renamed");
    }

    fn fix_audit(&self) {
        self.db
            .run("ALTER TABLE ssh_audit_away RENAME TO ssh_audit")
            .expect("renamed back");
    }
}

macro_rules! on_postgres {
    ($($name:ident),* $(,)?) => {
        $(
            /// The rule of the same name in `crate::ssh_book::tests`, on the Postgres book.
            #[test]
            fn $name() {
                rules::$name(&PgWorld::new());
            }
        )*
    };
}

on_postgres!(
    an_open_records_the_tunnel_and_its_line,
    an_open_is_refused_for_the_first_defect_in_the_documented_order,
    the_limits_are_one_tunnel_per_operator_per_host_and_a_count_per_operator,
    a_close_removes_the_tunnel_and_writes_its_line_once,
    a_poll_reports_the_revoked_and_the_gone_and_refreshes_its_own,
    a_poll_reaps_the_stale_tunnels_of_other_instances,
    a_poll_reaps_its_own_stale_row_that_it_does_not_hold_and_keeps_the_ones_it_does,
    a_heartbeat_from_another_address_writes_one_line,
    a_changed_host_key_writes_one_line_per_change_and_only_with_a_pin,
    an_empty_report_keeps_the_reported_set_and_two_changes_write_two_lines,
    a_pin_is_taken_only_for_the_reported_set_and_replaces_an_earlier_one,
    a_refused_audit_write_changes_nothing,
    an_unknown_host_has_no_keys,
    the_last_heartbeat_time_never_goes_back,
);

/// One audit row as the table holds it.
struct Row {
    line: Line,
    prev_hash: Vec<u8>,
    hash: Vec<u8>,
}

fn audit(world: &PgWorld) -> Vec<Row> {
    let name = world.db.name().to_owned();
    off_runtime(move || {
        let mut client = super::admin(Some(&name));
        client
            .query(
                "SELECT at_unix, by_name, action, subject, detail, prev_hash, hash \
                 FROM hostkeeper.ssh_audit ORDER BY seq",
                &[],
            )
            .expect("the audit reads")
            .iter()
            .map(|row| {
                let action: String = row.get(2);
                Row {
                    line: Line {
                        at_unix: u64::try_from(row.get::<_, i64>(0)).expect("a time"),
                        by: row.get(1),
                        action: Box::leak(action.into_boxed_str()),
                        subject: row.get(3),
                        detail: row.get(4),
                    },
                    prev_hash: row.get(5),
                    hash: row.get(6),
                }
            })
            .collect()
    })
}

fn opening(operator: &str, host: &str, serial: u64, instance: &str, per_operator: u64) -> Opening {
    Opening {
        operator: operator.to_owned(),
        host_id: host.to_owned(),
        serial,
        instance: instance.to_owned(),
        live_secs: 120,
        per_operator,
    }
}

/// Proves: the lines of the ledger (a certificate, a revocation) and of the book (an address, a
/// pin, a changed key, an open, a close, a reap) are one hash chain: each row holds the hash of
/// the row before it (the first, 32 zero bytes) and its own by `chain_hash` over its content;
/// the lines are in the order the calls were made.
///
/// Catches: a book that keeps a chain of its own beside the ledger's, a line appended without
/// the row before it, and a line hashed from other content than it holds.
#[test]
fn the_book_and_the_ledger_write_one_hash_chain() {
    let world = PgWorld::new();
    let book = &world.book;
    book.heard("node-07", "192.0.2.7", &["a".to_owned()])
        .expect("heard");
    book.heard("node-07", "192.0.2.9", &["a".to_owned()])
        .expect("heard");
    book.pin("carol", "node-07", &["a".to_owned()])
        .expect("pinned");
    book.heard("node-07", "192.0.2.9", &["b".to_owned()])
        .expect("heard");
    let serial = world.issue("alice", "node-07", 3600);
    let first = book
        .open(&opening("alice", "node-07", serial, "inst-a", 4))
        .expect("opened");
    let serial = world.issue("bob", "node-07", 3600);
    let second = book
        .open(&opening("bob", "node-07", serial, "inst-a", 4))
        .expect("opened");
    book.close(&Closing {
        id: first.id,
        cause: Cause::Eof,
        bytes_up: 1,
        bytes_down: 2,
    })
    .expect("closed");
    world.revoke(serial);
    world.age_tunnel(second.id, 400);
    assert_eq!(
        book.poll("inst-b", &[], 120),
        Ok(Polled {
            revoked: vec![],
            gone: vec![],
            reaped: 1
        })
    );
    let rows = audit(&world);
    assert_eq!(
        rows.iter().map(|r| r.line.action).collect::<Vec<_>>(),
        [
            "ssh_address_changed",
            "ssh_hostkey_pinned",
            "ssh_hostkey_changed",
            "ssh_cert_signed",
            "ssh_tunnel_open",
            "ssh_cert_signed",
            "ssh_tunnel_open",
            "ssh_tunnel_close",
            "ssh_revoke",
            "ssh_tunnel_close",
        ]
    );
    let mut prev = GENESIS.to_vec();
    for (n, row) in rows.iter().enumerate() {
        assert_eq!(row.prev_hash, prev, "row {n}");
        assert_eq!(row.hash, chain_hash(&prev, &row.line).to_vec(), "row {n}");
        prev = row.hash.clone();
    }
    let reaped = book_lines(&world).pop().expect("a line");
    assert_eq!(
        (reaped.0.as_str(), reaped.1.as_str()),
        ("bob", "ssh_tunnel_close")
    );
    assert!(
        reaped.3.contains("\"cause\":\"instance_lost\""),
        "{}",
        reaped.3
    );
}

/// Proves: two books over one database, as two coordinator instances are, hold one tunnel per
/// operator per host between them (the second instance's open of the host is `TooMany`) and the
/// per-operator count over both; two opens of the same operator and host made at once leave
/// exactly one tunnel; and one instance's reap of the other's stale tunnel writes
/// `instance_lost` and leaves the live ones.
///
/// Catches: a limit held in each instance's memory (so two instances give an operator twice what
/// it may hold), and a race that opens two tunnels of one operator to one host.
#[test]
fn two_instances_over_one_database_hold_one_tunnel_per_operator_per_host() {
    let world = PgWorld::new();
    let a = PgBook::new(world.store.clone());
    let b = Arc::new(PgBook::new(world.store.clone()));
    for host in ["node-07", "node-08", "node-09"] {
        a.heard(host, "192.0.2.7", &[]).expect("heard");
    }
    let one = world.issue("alice", "node-07", 3600);
    let opened = a
        .open(&opening("alice", "node-07", one, "inst-a", 2))
        .expect("opened");
    assert_eq!(
        b.open(&opening("alice", "node-07", one, "inst-b", 2)),
        Err(OpenRefused::TooMany)
    );
    let two = world.issue("alice", "node-08", 3600);
    b.open(&opening("alice", "node-08", two, "inst-b", 2))
        .expect("opened");
    let three = world.issue("alice", "node-09", 3600);
    assert_eq!(
        a.open(&opening("alice", "node-09", three, "inst-a", 2)),
        Err(OpenRefused::TooMany)
    );

    let serial = world.issue("bob", "node-07", 3600);
    let racers: Vec<_> = [("inst-a", Arc::clone(&b)), ("inst-b", Arc::clone(&b))]
        .into_iter()
        .map(|(instance, book)| {
            let attempt = opening("bob", "node-07", serial, instance, 4);
            std::thread::spawn(move || book.open(&attempt))
        })
        .collect();
    let won = racers
        .into_iter()
        .map(|racer| racer.join().expect("the open ran"))
        .filter(Result::is_ok)
        .count();
    assert_eq!(won, 1);
    assert_eq!(
        world.numbers("SELECT count(*) FROM ssh_tunnel WHERE operator = 'bob'"),
        [1]
    );

    world.age_tunnel(opened.id, 400);
    assert_eq!(
        b.poll("inst-b", &[], 120).map(|polled| polled.reaped),
        Ok(1),
        "inst-a's stale tunnel only"
    );
    assert_eq!(world.tunnel_ids().len(), 2);
    let lines = world.lines();
    let last = lines.last().expect("a line");
    assert_eq!(
        (last.0.as_str(), last.1.as_str(), last.2.as_deref()),
        ("alice", "ssh_tunnel_close", Some("node-07"))
    );
}

/// Proves: when the database fails each call says so with the database's text and changes
/// nothing: with `ssh_host`, `ssh_host_key_pin` or `ssh_tunnel` gone, a heartbeat, a read of the
/// keys, a pin, an open, a close and a poll are `BookError`, `Unwritten` or the like.
///
/// Catches: a call that reports success for a write the database did not take, and a read that
/// answers empty for a table it could not read.
#[test]
fn a_database_that_fails_is_reported_by_every_call() {
    let world = PgWorld::new();
    let book = &world.book;
    book.heard("node-07", "192.0.2.7", &["a".to_owned()])
        .expect("heard");
    let serial = world.issue("alice", "node-07", 3600);
    let opened = book
        .open(&opening("alice", "node-07", serial, "inst-a", 4))
        .expect("opened");

    world
        .db
        .run("DROP TABLE ssh_host_key_pin")
        .expect("dropped");
    assert!(matches!(book.host_keys("node-07"), Err(BookError(_))));
    assert!(matches!(
        book.pin("carol", "node-07", &["a".to_owned()]),
        Err(PinRefused::Unwritten(_))
    ));
    world.db.run("DROP TABLE ssh_tunnel").expect("dropped");
    assert!(matches!(
        book.close(&Closing {
            id: opened.id,
            cause: Cause::Eof,
            bytes_up: 0,
            bytes_down: 0
        }),
        Err(BookError(_))
    ));
    assert!(matches!(book.poll("inst-a", &[], 120), Err(BookError(_))));
    let again = world.issue("bob", "node-07", 3600);
    assert!(matches!(
        book.open(&opening("bob", "node-07", again, "inst-a", 4)),
        Err(OpenRefused::Unwritten(_))
    ));
    world.db.run("DROP TABLE ssh_host").expect("dropped");
    assert!(matches!(
        book.heard("node-07", "192.0.2.7", &[]),
        Err(BookError(_))
    ));
}
