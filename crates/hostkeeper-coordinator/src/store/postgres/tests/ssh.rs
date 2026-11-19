//! The SSH ledger over the Postgres store (`docs/design/ssh-through-coordinator.md`, section 5;
//! migration `0020`): the certificate rows, the revocations, and the audit chain a write appends
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
        api_token: None,
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

/// Proves: a signing asked by an operator calling with an API token is recorded by the operator
/// (the token's owner) with the token named last in the audit row's detail, the row chained like
/// any other; a signing with none has no `api_token` key.
///
/// Catches: a database ledger that drops the token the in-memory one keeps, so the live store's
/// SSH audit cannot say which of a person's tokens asked for a certificate.
#[test]
fn a_signing_made_with_an_api_token_names_the_token_in_its_audit_row() {
    let db = TestDatabase::new();
    let (store, _) = db.open("coordinator").expect("the store opens");
    let ledger = PgLedger::new(store.clone());
    let with_token = Issue {
        api_token: Some("alice-bot"),
        ..issue("alice", "node-07", 3600)
    };
    let first = ledger.issue(&with_token).expect("signed");
    ledger
        .issue(&issue("alice", "node-08", 60))
        .expect("signed");
    let lines = audit(&db);
    assert_eq!(lines.len(), 2);
    assert_eq!(
        (lines[0].line.by.as_str(), lines[0].line.action),
        ("alice", "ssh_cert_signed")
    );
    assert_eq!(
        lines[0].line.detail,
        format!(
            "{{\"serial\":1,\"key_fingerprint\":\"SHA256:abc\",\"expires_unix\":{},\"reason\":\"to look at \\\"a log\\\"\",\"api_token\":\"alice-bot\"}}",
            first.issued_unix + 3600
        )
    );
    assert!(
        !lines[1].line.detail.contains("api_token"),
        "{}",
        lines[1].line.detail
    );
    assert_eq!(lines[0].hash, chain_hash(&GENESIS, &lines[0].line));
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
    db.run(&format!(
        "UPDATE ssh_cert SET expires_unix = expires_unix - 1 WHERE serial = {}",
        expired.serial
    ))
    .expect("a certificate past its valid_before");
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

/// Proves: the revoked serials of a host are those of its own revoked certificates that have not
/// expired by the server's clock, ascending: another host's revoked serial, an unrevoked
/// certificate of the host and a revoked certificate that has expired are left out.
///
/// Catches: a list that tells a host another host's serials, one that keeps expired serials for
/// ever, and one that lists a certificate nobody revoked.
#[test]
fn the_revoked_serials_of_a_host_are_its_own_unexpired_revoked_certificates() {
    let db = TestDatabase::new();
    let (store, _) = db.open("coordinator").expect("the store opens");
    let ledger = PgLedger::new(store.clone());
    assert_eq!(ledger.revoked_serials("node-07"), Ok(Vec::new()));
    let own = ledger
        .issue(&issue("alice", "node-07", 3600))
        .expect("signed");
    let kept = ledger
        .issue(&issue("alice", "node-07", 3600))
        .expect("signed");
    let other = ledger
        .issue(&issue("alice", "node-08", 3600))
        .expect("signed");
    let again = ledger
        .issue(&issue("bob", "node-07", 3600))
        .expect("signed");
    let expired = ledger.issue(&issue("alice", "node-07", 0)).expect("signed");
    db.run(&format!(
        "UPDATE ssh_cert SET expires_unix = expires_unix - 1 WHERE serial = {}",
        expired.serial
    ))
    .expect("a certificate past its valid_before");
    assert_eq!(
        ledger.revoke("carol", Revocation::Serial(own.serial)),
        Ok(1)
    );
    assert_eq!(
        ledger.revoke("carol", Revocation::Serial(other.serial)),
        Ok(1)
    );
    assert_eq!(ledger.revoke("carol", Revocation::Operator("bob")), Ok(1));
    db.run(&format!(
        "INSERT INTO ssh_revocation (serial, by_name, at_unix) VALUES ({}, 'carol', 1)",
        expired.serial
    ))
    .expect("an expired certificate's revocation");
    assert_eq!(
        ledger.revoked_serials("node-07"),
        Ok(vec![own.serial, again.serial])
    );
    assert_eq!(ledger.revoked_serials("node-08"), Ok(vec![other.serial]));
    assert!(
        !ledger
            .revoked_serials("node-07")
            .expect("read")
            .contains(&kept.serial)
    );
}

/// Proves: a revoked certificate is listed through the second `expires_unix` itself, which is
/// still before its `valid_before` (`expires_unix + 1`), and not from the next second on.
///
/// Catches: a serial dropped from the revoked list for the one second sshd still accepts the
/// certificate.
#[test]
fn a_revoked_serial_is_listed_through_the_second_its_certificate_expires() {
    let db = TestDatabase::new();
    let (store, _) = db.open("coordinator").expect("the store opens");
    let ledger = PgLedger::new(store);
    let cert = ledger
        .issue(&issue("alice", "node-07", 3600))
        .expect("signed");
    assert_eq!(
        ledger.revoke("carol", Revocation::Serial(cert.serial)),
        Ok(1)
    );
    let expires = cert.issued_unix + 3600;
    assert_eq!(
        ledger.revoked_serials_at("node-07", Some(expires - 1)),
        Ok(vec![cert.serial])
    );
    assert_eq!(
        ledger.revoked_serials_at("node-07", Some(expires)),
        Ok(vec![cert.serial])
    );
    assert_eq!(
        ledger.revoked_serials_at("node-07", Some(expires + 1)),
        Ok(Vec::new())
    );
}

/// Proves: a ledger the database cannot read answers `Unread` with the database's reason, and
/// not an empty list.
///
/// Catches: a read failure taken for "nothing is revoked", which would tell a host to trust a
/// revoked certificate.
#[test]
fn a_read_the_database_refuses_is_unread() {
    let db = TestDatabase::new();
    let (store, _) = db.open("coordinator").expect("the store opens");
    let ledger = PgLedger::new(store.clone());
    db.run("DROP TABLE ssh_revocation").expect("dropped");
    assert!(matches!(
        ledger.revoked_serials("node-07"),
        Err(LedgerError::Unread(_))
    ));
}
