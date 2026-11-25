//! The ledger's chain and the memory ledger the signing's tests run on. The Postgres ledger
//! passes the same rules (`store/postgres/tests/ssh.rs`).

use super::{
    DENIED, GENESIS, Issue, Ledger, LedgerError, Line, Memory, REVOKE, Revocation, chain_hash,
    denied_detail, revoke_detail, revoke_subject, signed_detail,
};

const AT: u64 = 1_800_000_000;

fn line(subject: Option<&str>, by: &'static str, action: &'static str, detail: &str) -> Line {
    Line {
        at_unix: AT,
        by: by.to_owned(),
        action,
        subject: subject.map(str::to_owned),
        detail: detail.to_owned(),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn issue<'a>(operator: &'a str, host: &'a str, ttl_secs: u64) -> Issue<'a> {
    Issue {
        operator,
        host_id: host,
        fingerprint: "SHA256:abc",
        ttl_secs,
        reason: "a \"quoted\" reason",
        api_token: None,
    }
}

/// Proves: the hash of a row is SHA-256 over the hash before it and each field of the row, each
/// with its length in eight big-endian bytes in front, by exact values made outside this code:
/// the first row after 32 zero bytes, a row with no subject (the same as an empty one), and a
/// row chained to the first.
///
/// Catches: a chain whose hash changes with the code (a break in every row ever written), and
/// fields that run together (`ab`+`c` and `a`+`bc` hashing alike).
#[test]
fn the_chain_hash_is_pinned_by_values_made_outside_the_code() {
    let first = line(
        Some("node-07"),
        "alice",
        "ssh_cert_signed",
        "{\"serial\":1}",
    );
    let one = chain_hash(&GENESIS, &first);
    assert_eq!(
        hex(&one),
        "4b9ff11dbc0eb5c40c681f6f68d214bf8cceaddb19701aef6d5d81c9638c425a"
    );
    let denied = line(None, "alice", "ssh_denied", "{\"why\":\"x\"}");
    assert_eq!(
        hex(&chain_hash(&GENESIS, &denied)),
        "5436f17bb4968df2302685fd928e9f248893817c948ce43db108a4150dbc2f3c"
    );
    assert_eq!(
        chain_hash(&GENESIS, &denied),
        chain_hash(
            &GENESIS,
            &line(Some(""), "alice", "ssh_denied", "{\"why\":\"x\"}")
        )
    );
    let mut second = line(
        Some("alice"),
        "bob",
        "ssh_revoke",
        "{\"revoked\":1,\"operator\":\"alice\"}",
    );
    second.at_unix = AT + 1;
    assert_eq!(
        hex(&chain_hash(&one, &second)),
        "eb58077a5f2f80054232ca5c730fca594820674b032503e6da2d72ab5174dd4d"
    );
    assert_ne!(
        chain_hash(&GENESIS, &line(Some("c"), "a", "b", "")),
        chain_hash(&GENESIS, &line(Some(""), "ab", "c", ""))
    );
}

/// Proves: the details of the lines are JSON objects with these exact keys and values, the
/// reason's quotes escaped, a certificate's issue made with an API token naming the token last
/// (and one made without none), a revocation by operator and by serial each its own, and a refusal's
/// line holds its token only.
///
/// Catches: a reason that breaks out of its string, and a line that names a secret.
#[test]
fn the_details_are_json_objects_with_the_reason_escaped() {
    assert_eq!(
        signed_detail(7, &issue("alice", "node-07", 60), AT + 60),
        "{\"serial\":7,\"key_fingerprint\":\"SHA256:abc\",\"expires_unix\":1800000060,\"reason\":\"a \\\"quoted\\\" reason\"}"
    );
    let with_token = Issue {
        api_token: Some("alice-bot"),
        ..issue("alice", "node-07", 60)
    };
    assert_eq!(
        signed_detail(7, &with_token, AT + 60),
        "{\"serial\":7,\"key_fingerprint\":\"SHA256:abc\",\"expires_unix\":1800000060,\"reason\":\"a \\\"quoted\\\" reason\",\"api_token\":\"alice-bot\"}"
    );
    assert_eq!(
        revoke_detail(Revocation::Operator("alice"), 2),
        "{\"operator\":\"alice\",\"revoked\":2}"
    );
    assert_eq!(
        revoke_detail(Revocation::Serial(9), 0),
        "{\"serial\":9,\"revoked\":0}"
    );
    assert_eq!(revoke_subject(Revocation::Operator("alice")), "alice");
    assert_eq!(revoke_subject(Revocation::Serial(9)), "9");
    assert_eq!(
        denied_detail("not_permitted"),
        "{\"why\":\"not_permitted\"}"
    );
}

/// Proves: the memory ledger numbers certificates 1, 2, 3, stamps each with its clock, records
/// the certificate and its `ssh_cert_signed` line (the serial and expiry in it) together, and
/// chains each line to the one before it, the first to 32 zero bytes.
///
/// Catches: a serial that repeats and a line that is not chained.
#[test]
fn the_memory_ledger_records_a_certificate_with_its_line_and_chains_the_lines() {
    let ledger = Memory::at(AT);
    let first = ledger
        .issue(&issue("alice", "node-07", 3600))
        .expect("issued");
    ledger.set_clock(AT + 5);
    let second = ledger.issue(&issue("bob", "node-08", 60)).expect("issued");
    assert_eq!((first.serial, first.issued_unix), (1, AT));
    assert_eq!((second.serial, second.issued_unix), (2, AT + 5));
    let certs = ledger.certs();
    assert_eq!(certs[&2].operator, "bob");
    assert_eq!(certs[&2].host_id, "node-08");
    assert_eq!(
        (certs[&2].issued_unix, certs[&2].expires_unix),
        (AT + 5, AT + 65)
    );
    assert_eq!(certs[&1].fingerprint, "SHA256:abc");
    assert_eq!(certs[&1].reason, "a \"quoted\" reason");
    let audit = ledger.audit();
    assert_eq!(audit.len(), 2);
    assert_eq!(audit[0].prev_hash, GENESIS.to_vec());
    assert_eq!(audit[1].prev_hash, audit[0].hash);
    assert_eq!(audit[0].line.action, "ssh_cert_signed");
    assert_eq!(audit[0].line.subject.as_deref(), Some("node-07"));
    assert_eq!(audit[0].hash, chain_hash(&GENESIS, &audit[0].line).to_vec());
}

/// Proves: revoking by operator revokes the certificates of that operator that have not expired
/// and are not revoked, and says how many, and by serial one; the clock decides what has expired;
/// a serial or an operator with no certificate is `UnknownCert` and writes no line; every
/// revocation that changes or finds something writes its line.
///
/// Catches: a certificate counted twice and another operator's revoked with them.
#[test]
fn the_memory_ledger_revokes_what_is_not_expired_or_revoked_and_counts_it() {
    let ledger = Memory::at(AT);
    ledger
        .issue(&issue("alice", "node-07", 3600))
        .expect("issued");
    ledger
        .issue(&issue("alice", "node-08", 10))
        .expect("issued");
    ledger
        .issue(&issue("bob", "node-07", 3600))
        .expect("issued");
    ledger.set_clock(AT + 20);
    assert_eq!(ledger.revoke("carol", Revocation::Operator("alice")), Ok(1));
    assert_eq!(ledger.revoke("carol", Revocation::Operator("alice")), Ok(0));
    assert_eq!(ledger.revoke("carol", Revocation::Serial(2)), Ok(0));
    assert_eq!(ledger.revoke("carol", Revocation::Serial(3)), Ok(1));
    assert_eq!(ledger.revoked().keys().copied().collect::<Vec<_>>(), [1, 3]);
    assert_eq!(ledger.revoked()[&1], "carol");
    let lines = ledger.audit().len();
    assert_eq!(
        ledger.revoke("carol", Revocation::Serial(99)),
        Err(LedgerError::UnknownCert)
    );
    assert_eq!(
        ledger.revoke("carol", Revocation::Operator("dave")),
        Err(LedgerError::UnknownCert)
    );
    assert_eq!(ledger.audit().len(), lines);
    let last = ledger.audit().pop().expect("a line");
    assert_eq!((last.line.action, last.line.by.as_str()), (REVOKE, "carol"));
}

/// Proves: a refusal is a line `ssh_denied` for the host with the token; and a ledger that
/// refuses writes (`refuse_next`) gives `Unwritten` for each call kind, records nothing, and
/// writes again once it takes writes.
///
/// Catches: a certificate recorded by a call that reported failure.
#[test]
fn a_refused_call_is_a_line_and_a_ledger_that_cannot_write_records_nothing() {
    let ledger = Memory::at(AT);
    ledger
        .denied("bob", "node-07", "not_permitted")
        .expect("written");
    let line = &ledger.audit()[0].line;
    assert_eq!(
        (line.action, line.by.as_str(), line.subject.as_deref()),
        (DENIED, "bob", Some("node-07"))
    );
    assert_eq!(line.detail, "{\"why\":\"not_permitted\"}");
    ledger.refuse_next(3);
    assert!(matches!(
        ledger.issue(&issue("a", "h", 1)),
        Err(LedgerError::Unwritten(_))
    ));
    assert!(matches!(
        ledger.revoke("a", Revocation::Serial(1)),
        Err(LedgerError::Unwritten(_))
    ));
    assert!(matches!(
        ledger.denied("a", "h", "x"),
        Err(LedgerError::Unwritten(_))
    ));
    assert!(ledger.certs().is_empty());
    assert_eq!(ledger.audit().len(), 1);
    assert_eq!(ledger.issue(&issue("a", "h", 1)).map(|i| i.serial), Ok(1));
}

/// Proves: the memory ledger lists the revoked serials of one host that have not expired by its
/// clock, ascending, and answers `Unread` while it is down, as the database's ledger does.
///
/// Catches: another host's serial, an unrevoked or an expired certificate in the list, and a
/// read failure taken for an empty list.
#[test]
fn the_memory_ledger_lists_the_revoked_unexpired_serials_of_one_host() {
    let ledger = Memory::at(AT);
    let a = ledger.issue(&issue("alice", "node-07", 100)).expect("a");
    let b = ledger.issue(&issue("alice", "node-07", 1000)).expect("b");
    let c = ledger.issue(&issue("alice", "node-08", 1000)).expect("c");
    let _plain = ledger.issue(&issue("bob", "node-07", 1000)).expect("d");
    for serial in [a.serial, b.serial, c.serial] {
        ledger
            .revoke("carol", Revocation::Serial(serial))
            .expect("revoked");
    }
    assert_eq!(
        ledger.revoked_serials("node-07"),
        Ok(vec![a.serial, b.serial])
    );
    ledger.set_clock(AT + 101);
    assert_eq!(ledger.revoked_serials("node-07"), Ok(vec![b.serial]));
    assert_eq!(ledger.revoked_serials("node-08"), Ok(vec![c.serial]));
    ledger.refuse_next(1);
    assert!(matches!(
        ledger.revoked_serials("node-07"),
        Err(LedgerError::Unread(_))
    ));
    assert_eq!(ledger.revoked_serials("node-07"), Ok(vec![b.serial]));
}

/// Proves: the memory ledger lists a revoked serial through the second `expires_unix` itself
/// (the certificate's `valid_before` is one later) and not from the next second, and revokes a
/// certificate at that second too.
///
/// Catches: a serial dropped from the revoked list for the one second sshd still accepts the
/// certificate.
#[test]
fn the_memory_ledger_keeps_a_revoked_serial_through_the_second_its_certificate_expires() {
    let ledger = Memory::at(AT);
    let a = ledger.issue(&issue("alice", "node-07", 100)).expect("a");
    let b = ledger.issue(&issue("bob", "node-07", 100)).expect("b");
    ledger
        .revoke("carol", Revocation::Serial(a.serial))
        .expect("revoked");
    ledger.set_clock(a.issued_unix + 100);
    assert_eq!(ledger.revoked_serials("node-07"), Ok(vec![a.serial]));
    assert_eq!(ledger.revoke("carol", Revocation::Serial(b.serial)), Ok(1));
    assert_eq!(
        ledger.revoked_serials("node-07"),
        Ok(vec![a.serial, b.serial])
    );
    ledger.set_clock(a.issued_unix + 101);
    assert_eq!(ledger.revoked_serials("node-07"), Ok(Vec::new()));
}
