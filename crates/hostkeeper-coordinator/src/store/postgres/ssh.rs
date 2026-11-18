//! The SSH ledger ([`Ledger`]) in the database: the tables of migration `0020`, `ssh_cert`,
//! `ssh_revocation` and `ssh_audit`, and the sequence `ssh_cert_serial`.
//!
//! Each call is one transaction of the writing session. It first takes the transaction-level
//! advisory lock of the audit chain, so the row it appends is the one after the newest, whichever
//! coordinator instance wrote that. The time is the server's (`now()`), so instances need not
//! agree on one. Nothing is written when the transaction does not commit.

use super::Postgres;
use super::writer::{Tx, WriteFail};
use crate::ssh_ledger::{
    CERT_SIGNED, DENIED, GENESIS, Issue, Issued, Ledger, LedgerError, Line, REVOKE, Revocation,
    chain_hash, denied_detail, revoke_detail, revoke_subject, signed_detail,
};

/// The key of the advisory lock of the audit chain; the coordinator's own lock has another
/// ([`super::writer`]).
const CHAIN_KEY: i64 = 0x686b_7373_6861;

/// The ledger over the store `store`.
pub(crate) struct PgLedger {
    store: Postgres,
}

impl PgLedger {
    /// The ledger of `store`.
    pub(crate) fn new(store: Postgres) -> PgLedger {
        PgLedger { store }
    }
}

/// A count or a time of the database as the unsigned number it is.
pub(super) fn unsigned(n: i64) -> u64 {
    u64::try_from(n).unwrap_or_default()
}

/// A number of ours as the database's `bigint`.
pub(super) fn signed(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// Takes the chain's lock for the rest of the transaction.
pub(super) fn lock(tx: &mut Tx<'_>) -> Result<(), postgres::Error> {
    tx.query_one("SELECT pg_advisory_xact_lock($1)", &[&CHAIN_KEY])?;
    Ok(())
}

/// Reads the server's time, in seconds: the transaction's start.
pub(super) fn clock(tx: &mut Tx<'_>) -> Result<u64, postgres::Error> {
    let now = tx.query_one("SELECT floor(extract(epoch FROM now()))::bigint", &[])?;
    Ok(unsigned(now.get(0)))
}

/// Takes the chain's lock for the rest of the transaction, and reads the server's time.
pub(super) fn begin(tx: &mut Tx<'_>) -> Result<u64, postgres::Error> {
    lock(tx)?;
    clock(tx)
}

/// Appends `line` to the chain, after the newest row, which the caller's lock keeps from changing.
pub(super) fn append(tx: &mut Tx<'_>, line: &Line) -> Result<(), postgres::Error> {
    let prev: Vec<u8> = tx
        .query_one(
            "SELECT coalesce((SELECT hash FROM ssh_audit ORDER BY seq DESC LIMIT 1), $1::bytea)",
            &[&GENESIS.as_slice()],
        )?
        .get(0);
    let hash = chain_hash(&prev, line);
    tx.execute(
        "INSERT INTO ssh_audit (at_unix, by_name, action, subject, detail, prev_hash, hash) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
        &[
            &signed(line.at_unix),
            &line.by,
            &line.action,
            &line.subject,
            &line.detail,
            &prev,
            &hash.as_slice(),
        ],
    )?;
    Ok(())
}

fn unwritten(fail: &WriteFail) -> LedgerError {
    LedgerError::Unwritten(fail.why().to_owned())
}

impl Ledger for PgLedger {
    fn issue(&self, issue: &Issue<'_>) -> Result<Issued, LedgerError> {
        let operator = issue.operator.to_owned();
        let host_id = issue.host_id.to_owned();
        let fingerprint = issue.fingerprint.to_owned();
        let reason = issue.reason.to_owned();
        let ttl = issue.ttl_secs;
        self.store
            .write(move |tx| {
                let now = begin(tx)?;
                let serial = unsigned(tx.query_one("SELECT nextval('ssh_cert_serial')", &[])?.get(0));
                let expires = now.saturating_add(ttl);
                tx.execute(
                    "INSERT INTO ssh_cert \
                     (serial, operator, host_id, key_fingerprint, issued_unix, expires_unix, reason) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7)",
                    &[
                        &signed(serial),
                        &operator,
                        &host_id,
                        &fingerprint,
                        &signed(now),
                        &signed(expires),
                        &reason,
                    ],
                )?;
                let again = Issue {
                    operator: &operator,
                    host_id: &host_id,
                    fingerprint: &fingerprint,
                    ttl_secs: ttl,
                    reason: &reason,
                };
                append(
                    tx,
                    &Line {
                        at_unix: now,
                        by: operator.clone(),
                        action: CERT_SIGNED,
                        subject: Some(host_id.clone()),
                        detail: signed_detail(serial, &again, expires),
                    },
                )?;
                Ok(Issued {
                    serial,
                    issued_unix: now,
                })
            })
            .map_err(|fail| unwritten(&fail))
    }

    fn revoke(&self, by: &str, target: Revocation<'_>) -> Result<u64, LedgerError> {
        let by = by.to_owned();
        let (operator, serial) = match target {
            Revocation::Operator(name) => (Some(name.to_owned()), None),
            Revocation::Serial(n) => (None, Some(signed(n))),
        };
        let written = self.store.write(move |tx| {
            let now = begin(tx)?;
            let target = match (&operator, serial) {
                (Some(name), _) => Revocation::Operator(name),
                (None, n) => Revocation::Serial(unsigned(n.unwrap_or_default())),
            };
            let made: i64 = match (&operator, serial) {
                (Some(name), _) => tx
                    .query_one("SELECT count(*) FROM ssh_cert WHERE operator = $1", &[name])?
                    .get(0),
                (None, n) => tx
                    .query_one("SELECT count(*) FROM ssh_cert WHERE serial = $1", &[&n])?
                    .get(0),
            };
            if made == 0 {
                return Ok(None);
            }
            let revoked = match (&operator, serial) {
                (Some(name), _) => tx.execute(
                    "INSERT INTO ssh_revocation (serial, by_name, at_unix) \
                     SELECT serial, $2, $3 FROM ssh_cert \
                     WHERE operator = $1 AND expires_unix > $3 ON CONFLICT DO NOTHING",
                    &[name, &by, &signed(now)],
                )?,
                (None, n) => tx.execute(
                    "INSERT INTO ssh_revocation (serial, by_name, at_unix) \
                     SELECT serial, $2, $3 FROM ssh_cert \
                     WHERE serial = $1 AND expires_unix > $3 ON CONFLICT DO NOTHING",
                    &[&n, &by, &signed(now)],
                )?,
            };
            append(
                tx,
                &Line {
                    at_unix: now,
                    by: by.clone(),
                    action: REVOKE,
                    subject: Some(revoke_subject(target)),
                    detail: revoke_detail(target, revoked),
                },
            )?;
            Ok(Some(revoked))
        });
        match written {
            Ok(Some(revoked)) => Ok(revoked),
            Ok(None) => Err(LedgerError::UnknownCert),
            Err(fail) => Err(unwritten(&fail)),
        }
    }

    fn denied(&self, by: &str, host_id: &str, why: &str) -> Result<(), LedgerError> {
        let (by, host_id, why) = (by.to_owned(), host_id.to_owned(), why.to_owned());
        self.store
            .write(move |tx| {
                let now = begin(tx)?;
                append(
                    tx,
                    &Line {
                        at_unix: now,
                        by,
                        action: DENIED,
                        subject: Some(host_id),
                        detail: denied_detail(&why),
                    },
                )
            })
            .map_err(|fail| unwritten(&fail))
    }

    fn revoked_serials(&self, host_id: &str) -> Result<Vec<u64>, LedgerError> {
        let host_id = host_id.to_owned();
        self.store
            .read(move |session| {
                let rows = session.query(
                    "SELECT c.serial FROM ssh_cert c JOIN ssh_revocation r ON r.serial = c.serial \
                     WHERE c.host_id = $1 \
                     AND c.expires_unix > floor(extract(epoch FROM now()))::bigint \
                     ORDER BY c.serial",
                    &[&host_id],
                )?;
                Ok(rows
                    .iter()
                    .map(|row| unsigned(row.get::<_, i64>(0)))
                    .collect())
            })
            .map_err(LedgerError::Unread)
    }
}
