//! The SSH ledger over the Postgres store (`docs/design/ssh-through-coordinator.md`, section 5;
//! migration `0019`): the certificate rows, the revocations, and the audit chain a write appends
//! to, read back with a client beside the store.

use postgres::Client;

use super::{TestDatabase, off_runtime};
use crate::ssh_ledger::{GENESIS, Issue, Ledger, LedgerError, Line, Revocation, chain_hash};
use crate::store::postgres::ssh::PgLedger;

/// What a test asks the ledger to sign.
fn issue<'a>(operator: &'a str, host: &'a str, ttl_secs: u64) -> Issue<'a> {
    Issue {
        operator,
        host_id: host,
        fingerprint: "SHA256:abc",
        ttl_secs,
        reason: "to look at \"a log\"",
    }
}

/// One audit row as the table holds it.
struct Row {
    seq: i64,
    line: Line,
    prev_hash: Vec<u8>,
    hash: Vec<u8>,
}

/// Every row of `ssh_audit`, in order.
fn audit(db: &TestDatabase) -> Vec<Row> {
    let name = db.name().to_owned();
    off_runtime(move || {
        let mut client: Client = super::admin(Some(&name));
        client
            .query(
                "SELECT seq, at_unix, by_name, action, subject, detail, prev_hash, hash \
                 FROM hostkeeper.ssh_audit ORDER BY seq",
                &[],
            )
            .expect("the audit reads")
            .iter()
            .map(|row| {
                let action: String = row.get(3);
                Row {
                    seq: row.get(0),
                    line: Line {
                        at_unix: u64::try_from(row.get::<_, i64>(1)).expect("a time"),
                        by: row.get(2),
                        action: Box::leak(action.into_boxed_str()),
                        subject: row.get(4),
                        detail: row.get(5),
                    },
                    prev_hash: row.get(6),
                    hash: row.get(7),
                }
            })
            .collect()
    })
}

fn count(db: &TestDatabase, table: &str) -> i64 {
    db.rows(&format!("SELECT count(*) FROM {table}"))[0][0]
        .as_deref()
        .and_then(|n| n.parse().ok())
        .expect("a count")
}

/// Proves: signing records one `ssh_cert` row, whose serial is the sequence's next number, whose
/// times are the server's (issued at its clock, expiring `ttl` seconds after), and one
/// `ssh_audit` row `ssh_cert_signed` for the host with the serial, the key's fingerprint and the
/// reason (its quotes escaped) in the detail; two signings give serials 1 and 2.
///
/// Catches: a certificate without its audit row, a serial that repeats, and a time taken from
/// the coordinator's clock instead of the database's.
#[test]
fn a_signing_is_a_certificate_row_and_an_audit_row_at_the_servers_time() {
    let db = TestDatabase::new();
    let (store, _) = db.open("coordinator").expect("the store opens");
    let ledger = PgLedger::new(store.clone());
    let first = ledger
        .issue(&issue("alice", "node-07", 3600))
        .expect("signed");
    let second = ledger
        .issue(&issue("alice", "node-08", 60))
        .expect("signed");
    assert_eq!((first.serial, second.serial), (1, 2));
    assert!(second.issued_unix >= first.issued_unix);
    let certs = db.rows(
        "SELECT serial, operator, host_id, key_fingerprint, expires_unix - issued_unix, reason \
         FROM ssh_cert ORDER BY serial",
    );
    let text = |s: &str| Some(s.to_owned());
    assert_eq!(
        certs,
        vec![
            vec![
                text("1"),
                text("alice"),
                text("node-07"),
                text("SHA256:abc"),
                text("3600"),
                text("to look at \"a log\"")
            ],
            vec![
                text("2"),
                text("alice"),
                text("node-08"),
                text("SHA256:abc"),
                text("60"),
                text("to look at \"a log\"")
            ],
        ]
    );
    let lines = audit(&db);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].line.action, "ssh_cert_signed");
    assert_eq!(lines[0].line.by, "alice");
    assert_eq!(lines[0].line.subject.as_deref(), Some("node-07"));
    assert_eq!(lines[0].line.at_unix, first.issued_unix);
    assert_eq!(
        lines[0].line.detail,
        format!(
            "{{\"serial\":1,\"key_fingerprint\":\"SHA256:abc\",\"expires_unix\":{},\"reason\":\"to look at \\\"a log\\\"\"}}",
            first.issued_unix + 3600
        )
    );
}

/// Proves: each audit row holds the hash of the row before it (the first, 32 zero bytes) and its
/// own hash by `chain_hash` over its content, across signings, a revocation and a refusal's
/// line, so a deleted or edited row would break the chain after it.
///
/// Catches: a chain that restarts, a row hashed without the one before, and a hash taken of
/// different content than the row holds.
#[test]
fn the_audit_rows_are_a_hash_chain_across_every_kind_of_line() {
    let db = TestDatabase::new();
    let (store, _) = db.open("coordinator").expect("the store opens");
    let ledger = PgLedger::new(store.clone());
    ledger
        .issue(&issue("alice", "node-07", 3600))
        .expect("signed");
    ledger
        .denied("bob", "node-07", "not_permitted")
        .expect("written");
    ledger
        .revoke("alice", Revocation::Operator("alice"))
        .expect("revoked");
    let rows = audit(&db);
    assert_eq!(
        rows.iter().map(|r| r.line.action).collect::<Vec<_>>(),
        ["ssh_cert_signed", "ssh_denied", "ssh_revoke"]
    );
    let mut prev = GENESIS.to_vec();
    for row in &rows {
        assert_eq!(row.prev_hash, prev, "row {}", row.seq);
        assert_eq!(
            row.hash,
            chain_hash(&prev, &row.line).to_vec(),
            "row {}",
            row.seq
        );
        prev = row.hash.clone();
    }
    assert_eq!(rows[1].line.detail, "{\"why\":\"not_permitted\"}");
    assert_eq!(rows[2].line.subject.as_deref(), Some("alice"));
    assert_eq!(
        rows[2].line.detail,
        "{\"operator\":\"alice\",\"revoked\":1}"
    );
}

/// Proves: when the audit row cannot be written the certificate row is not there either: the
/// write is one transaction, and the call says the ledger could not be written.
///
/// Catches: a certificate that exists without the line that says it was signed.
#[test]
fn a_certificate_whose_audit_row_cannot_be_written_is_not_recorded() {
    let db = TestDatabase::new();
    let (store, _) = db.open("coordinator").expect("the store opens");
    let ledger = PgLedger::new(store.clone());
    db.run("ALTER TABLE ssh_audit RENAME TO ssh_audit_away")
        .expect("renamed");
    let refused = ledger.issue(&issue("alice", "node-07", 3600));
    assert!(
        matches!(refused, Err(LedgerError::Unwritten(_))),
        "{refused:?}"
    );
    assert_eq!(count(&db, "ssh_cert"), 0);
    db.run("ALTER TABLE ssh_audit_away RENAME TO ssh_audit")
        .expect("renamed back");
    let again = ledger
        .issue(&issue("alice", "node-07", 3600))
        .expect("signed");
    assert_eq!(count(&db, "ssh_cert"), 1);
    assert!(again.serial >= 1);
}

/// Proves: revoking an operator revokes each certificate of theirs that has not expired and was
/// not revoked, and says how many; again it revokes none; one serial revokes that certificate; an
/// expired certificate is not revoked (it is no longer good); a serial or an operator no
/// certificate was made for is `UnknownCert` and writes no line.
///
/// Catches: a revocation that counts a certificate twice, revokes another operator's, or writes
/// a line for a call that changed nothing.
#[test]
fn a_revocation_counts_the_certificates_it_newly_revokes() {
    let db = TestDatabase::new();
    let (store, _) = db.open("coordinator").expect("the store opens");
    let ledger = PgLedger::new(store.clone());
    let a = ledger
        .issue(&issue("alice", "node-07", 3600))
        .expect("signed");
    let b = ledger
        .issue(&issue("alice", "node-08", 3600))
        .expect("signed");
    let expired = ledger.issue(&issue("alice", "node-09", 0)).expect("signed");
    let other = ledger
        .issue(&issue("bob", "node-07", 3600))
        .expect("signed");
    assert_eq!(ledger.revoke("carol", Revocation::Serial(a.serial)), Ok(1));
    assert_eq!(ledger.revoke("carol", Revocation::Operator("alice")), Ok(1));
    assert_eq!(ledger.revoke("carol", Revocation::Operator("alice")), Ok(0));
    assert_eq!(
        ledger.revoke("carol", Revocation::Serial(expired.serial)),
        Ok(0)
    );
    let revoked = db.rows("SELECT serial, by_name FROM ssh_revocation ORDER BY serial");
    let row = |serial: u64| vec![Some(serial.to_string()), Some("carol".to_owned())];
    assert_eq!(revoked, vec![row(a.serial), row(b.serial)]);
    assert!(
        revoked
            .iter()
            .all(|r| r[0] != Some(other.serial.to_string()))
    );
    let before = audit(&db).len();
    assert_eq!(
        ledger.revoke("carol", Revocation::Serial(999)),
        Err(LedgerError::UnknownCert)
    );
    assert_eq!(
        ledger.revoke("carol", Revocation::Operator("nobody")),
        Err(LedgerError::UnknownCert)
    );
    assert_eq!(audit(&db).len(), before);
}

/// Proves: a write the database refuses is `Unwritten` for a refusal's line and for a revocation
/// alike, and the revocation leaves no row behind: its write is one transaction with its line.
///
/// Catches: a call that reports success for a write the database did not take, and a revocation
/// that stays when its audit line failed.
#[test]
fn a_write_the_database_refuses_is_unwritten_and_changes_nothing() {
    let db = TestDatabase::new();
    let (store, _) = db.open("coordinator").expect("the store opens");
    let ledger = PgLedger::new(store.clone());
    let signed = ledger
        .issue(&issue("alice", "node-07", 3600))
        .expect("signed");
    db.run("DROP TABLE ssh_audit").expect("dropped");
    assert!(matches!(
        ledger.denied("bob", "node-07", "not_permitted"),
        Err(LedgerError::Unwritten(_))
    ));
    assert!(matches!(
        ledger.revoke("bob", Revocation::Serial(signed.serial)),
        Err(LedgerError::Unwritten(_))
    ));
    assert_eq!(count(&db, "ssh_revocation"), 0);
}
