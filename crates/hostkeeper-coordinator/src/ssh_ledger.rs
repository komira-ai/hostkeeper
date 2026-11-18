//! The ledger of the SSH user certificates the coordinator signs (docs/design/ssh-through-
//! coordinator.md, section 5): the row of each certificate, the revocations, and the audit of the
//! SSH calls, whose rows are chained by hash.
//!
//! Every write is one transaction that holds the audit's advisory lock, so a certificate and the
//! line that says it was signed are both there or neither is, and the chain has one writer at a
//! time across any number of coordinator instances. The time of every row, and so the validity of
//! a certificate, is the database's clock: instances need not agree on one. The serial of a
//! certificate comes from a sequence of the database, so no two instances give one serial twice.
//!
//! The chain is tamper evidence for the database, not proof against a coordinator that holds the
//! CA key: each row carries the SHA-256 of the row before it and its own content ([`chain_hash`]),
//! so a row that is deleted or edited breaks every row after it.

use serde::Serialize;
use sha2::{Digest as _, Sha256};

/// The `action` of the line a certificate's row is written with.
pub(crate) const CERT_SIGNED: &str = "ssh_cert_signed";
/// The `action` of a revocation's line.
pub(crate) const REVOKE: &str = "ssh_revoke";
/// The `action` of a refusal's line, for a caller the coordinator knew.
pub(crate) const DENIED: &str = "ssh_denied";

/// The `prev_hash` of the first row of the chain.
pub(crate) const GENESIS: [u8; 32] = [0; 32];

/// One row of the audit, as the chain hashes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Line {
    pub(crate) at_unix: u64,
    pub(crate) by: String,
    pub(crate) action: &'static str,
    /// The host the line concerns, or the operator a revocation names.
    pub(crate) subject: Option<String>,
    /// A JSON object of the rest: the serial, the key's fingerprint, the expiry, the reason.
    pub(crate) detail: String,
}

/// The hash of `line` after the row whose hash is `prev`: SHA-256 of `prev` and each field of
/// the line with its length in front (eight bytes, big-endian), so no two lines hash alike by
/// where one field ends and the next begins.
pub(crate) fn chain_hash(prev: &[u8], line: &Line) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(prev);
    let at = line.at_unix.to_string();
    let subject = line.subject.as_deref().unwrap_or_default();
    for field in [at.as_str(), &line.by, line.action, subject, &line.detail] {
        digest.update((field.len() as u64).to_be_bytes());
        digest.update(field.as_bytes());
    }
    digest.finalize().into()
}

/// A certificate to be signed, as the ledger records it before it exists.
pub(crate) struct Issue<'a> {
    pub(crate) operator: &'a str,
    pub(crate) host_id: &'a str,
    pub(crate) fingerprint: &'a str,
    pub(crate) ttl_secs: u64,
    pub(crate) reason: &'a str,
}

/// What the ledger gives for the certificate: its serial and the time it was recorded at, the
/// database's, from which the certificate's validity is counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Issued {
    pub(crate) serial: u64,
    pub(crate) issued_unix: u64,
}

/// What a revocation names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Revocation<'a> {
    /// Every certificate of the operator that has not expired.
    Operator(&'a str),
    /// One certificate.
    Serial(u64),
}

/// Why the ledger did not write.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum LedgerError {
    /// A revocation names an operator or a serial no certificate was made for.
    #[error("no certificate was made for it")]
    UnknownCert,
    /// The database could not take the write, or its outcome is not known: nothing changed as far
    /// as this call can tell.
    #[error("the ledger could not be written: {0}")]
    Unwritten(String),
    /// The database could not be read.
    #[error("the ledger could not be read: {0}")]
    Unread(String),
}

/// The detail of the line of a certificate.
pub(crate) fn signed_detail(serial: u64, issue: &Issue<'_>, expires_unix: u64) -> String {
    #[derive(Serialize)]
    struct Signed<'a> {
        serial: u64,
        key_fingerprint: &'a str,
        expires_unix: u64,
        reason: &'a str,
    }
    json(&Signed {
        serial,
        key_fingerprint: issue.fingerprint,
        expires_unix,
        reason: issue.reason,
    })
}

/// The detail of the line of a revocation.
pub(crate) fn revoke_detail(target: Revocation<'_>, revoked: u64) -> String {
    #[derive(Serialize)]
    struct Revoked<'a> {
        #[serde(skip_serializing_if = "Option::is_none")]
        operator: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        serial: Option<u64>,
        revoked: u64,
    }
    let (operator, serial) = match target {
        Revocation::Operator(name) => (Some(name), None),
        Revocation::Serial(serial) => (None, Some(serial)),
    };
    json(&Revoked {
        operator,
        serial,
        revoked,
    })
}

/// The subject of the line of a revocation.
pub(crate) fn revoke_subject(target: Revocation<'_>) -> String {
    match target {
        Revocation::Operator(name) => name.to_owned(),
        Revocation::Serial(serial) => serial.to_string(),
    }
}

/// The detail of a refusal's line.
pub(crate) fn denied_detail(why: &str) -> String {
    #[derive(Serialize)]
    struct Denied<'a> {
        why: &'a str,
    }
    json(&Denied { why })
}

/// `value` as JSON text, its keys in the order its struct declares them: the text is hashed into
/// the chain, so it must not depend on a map's order.
fn json(value: &impl Serialize) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// Where the certificates, revocations and the audit chain are kept: the database's tables
/// (`ssh_cert`, `ssh_revocation`, `ssh_audit`), or, in a test, memory.
pub(crate) trait Ledger: Send + Sync {
    /// Records the certificate and its audit line, in one write, and gives its serial and the
    /// time. Nothing is recorded when this fails, and then no certificate may be made.
    ///
    /// # Errors
    /// [`LedgerError::Unwritten`].
    fn issue(&self, issue: &Issue<'_>) -> Result<Issued, LedgerError>;

    /// Revokes by `by`, and writes its audit line in the same write; how many certificates were
    /// newly revoked, none when every one the target names was revoked already or is past its
    /// `valid_before` (one second after its `expires_unix`).
    ///
    /// # Errors
    /// [`LedgerError::UnknownCert`] when no certificate was ever made for the target;
    /// [`LedgerError::Unwritten`].
    fn revoke(&self, by: &str, target: Revocation<'_>) -> Result<u64, LedgerError>;

    /// Writes the audit line of a refusal of `by`: the host asked for and the reason token.
    ///
    /// # Errors
    /// [`LedgerError::Unwritten`].
    fn denied(&self, by: &str, host_id: &str, why: &str) -> Result<(), LedgerError>;

    /// The serials, ascending, of the revoked certificates of the host `host_id` that sshd could
    /// still accept by the ledger's clock, through the second `expires_unix` itself (the
    /// certificate's `valid_before` is `expires_unix + 1`): the ones the host's sshd is told to
    /// refuse. A certificate is valid only for its own host's principal, so another host's serials
    /// add nothing, and one past its `valid_before` is refused by sshd anyway (on a host whose
    /// clock agrees with the ledger's).
    ///
    /// # Errors
    /// [`LedgerError::Unread`] when the ledger cannot be read.
    fn revoked_serials(&self, host_id: &str) -> Result<Vec<u64>, LedgerError>;
}

#[cfg(test)]
pub(crate) use memory::Memory;

/// The ledger in memory, for tests that do not need a database; the Postgres ledger passes the
/// same tests.
#[cfg(test)]
mod memory {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex, PoisonError};

    use super::{
        CERT_SIGNED, DENIED, GENESIS, Issue, Issued, Ledger, LedgerError, Line, REVOKE, Revocation,
        chain_hash, denied_detail, revoke_detail, revoke_subject, signed_detail,
    };

    /// A certificate's row.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(crate) struct Cert {
        pub(crate) operator: String,
        pub(crate) host_id: String,
        pub(crate) fingerprint: String,
        pub(crate) issued_unix: u64,
        pub(crate) expires_unix: u64,
        pub(crate) reason: String,
    }

    /// A row of the audit chain.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(crate) struct Row {
        pub(crate) line: Line,
        pub(crate) prev_hash: Vec<u8>,
        pub(crate) hash: Vec<u8>,
    }

    #[derive(Default)]
    struct State {
        serial: u64,
        certs: BTreeMap<u64, Cert>,
        revoked: BTreeMap<u64, String>,
        audit: Vec<Row>,
        /// Writes still to refuse.
        refuse: usize,
    }

    /// The memory ledger: a clock a test sets and the rows.
    pub(crate) struct Memory {
        clock: Arc<Mutex<u64>>,
        state: Mutex<State>,
    }

    impl Memory {
        /// An empty ledger whose clock reads `now`.
        pub(crate) fn at(now: u64) -> Memory {
            Memory {
                clock: Arc::new(Mutex::new(now)),
                state: Mutex::new(State::default()),
            }
        }

        /// Sets the clock.
        pub(crate) fn set_clock(&self, now: u64) {
            *self.clock.lock().unwrap_or_else(PoisonError::into_inner) = now;
        }

        /// Refuses the next `n` writes, as a database that is down does.
        pub(crate) fn refuse_next(&self, n: usize) {
            self.state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .refuse = n;
        }

        /// The certificates by serial.
        pub(crate) fn certs(&self) -> BTreeMap<u64, Cert> {
            self.state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .certs
                .clone()
        }

        /// The serials revoked, with who revoked each.
        pub(crate) fn revoked(&self) -> BTreeMap<u64, String> {
            self.state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .revoked
                .clone()
        }

        /// Appends `line` to the chain as one write: refused as any write is, and then nothing
        /// is appended.
        ///
        /// # Errors
        /// [`LedgerError::Unwritten`] while writes are refused.
        pub(crate) fn append_line(&self, line: Line) -> Result<(), LedgerError> {
            self.append_lines(vec![line])
        }

        /// Appends `lines` to the chain as one write.
        ///
        /// # Errors
        /// [`LedgerError::Unwritten`] while writes are refused.
        pub(crate) fn append_lines(&self, lines: Vec<Line>) -> Result<(), LedgerError> {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.write()?;
            for line in lines {
                state.append(line);
            }
            Ok(())
        }

        /// The audit rows in order.
        pub(crate) fn audit(&self) -> Vec<Row> {
            self.state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .audit
                .clone()
        }

        /// What the clock reads.
        pub(crate) fn now(&self) -> u64 {
            *self.clock.lock().unwrap_or_else(PoisonError::into_inner)
        }
    }

    impl State {
        fn write(&mut self) -> Result<(), LedgerError> {
            if self.refuse > 0 {
                self.refuse -= 1;
                return Err(LedgerError::Unwritten("the ledger is down".to_owned()));
            }
            Ok(())
        }

        fn append(&mut self, line: Line) {
            let prev = self
                .audit
                .last()
                .map_or(GENESIS.to_vec(), |row| row.hash.clone());
            let hash = chain_hash(&prev, &line).to_vec();
            self.audit.push(Row {
                line,
                prev_hash: prev,
                hash,
            });
        }
    }

    impl Ledger for Memory {
        fn issue(&self, issue: &Issue<'_>) -> Result<Issued, LedgerError> {
            let now = self.now();
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.write()?;
            state.serial += 1;
            let serial = state.serial;
            let expires = now + issue.ttl_secs;
            state.certs.insert(
                serial,
                Cert {
                    operator: issue.operator.to_owned(),
                    host_id: issue.host_id.to_owned(),
                    fingerprint: issue.fingerprint.to_owned(),
                    issued_unix: now,
                    expires_unix: expires,
                    reason: issue.reason.to_owned(),
                },
            );
            state.append(Line {
                at_unix: now,
                by: issue.operator.to_owned(),
                action: CERT_SIGNED,
                subject: Some(issue.host_id.to_owned()),
                detail: signed_detail(serial, issue, expires),
            });
            Ok(Issued {
                serial,
                issued_unix: now,
            })
        }

        fn revoke(&self, by: &str, target: Revocation<'_>) -> Result<u64, LedgerError> {
            let now = self.now();
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.write()?;
            let named: Vec<(u64, u64)> = state
                .certs
                .iter()
                .filter(|(serial, cert)| match target {
                    Revocation::Operator(name) => cert.operator == name,
                    Revocation::Serial(n) => **serial == n,
                })
                .map(|(serial, cert)| (*serial, cert.expires_unix))
                .collect();
            if named.is_empty() {
                return Err(LedgerError::UnknownCert);
            }
            let mut revoked = 0;
            for (serial, expires) in named {
                if expires.saturating_add(1) > now && !state.revoked.contains_key(&serial) {
                    state.revoked.insert(serial, by.to_owned());
                    revoked += 1;
                }
            }
            state.append(Line {
                at_unix: now,
                by: by.to_owned(),
                action: REVOKE,
                subject: Some(revoke_subject(target)),
                detail: revoke_detail(target, revoked),
            });
            Ok(revoked)
        }

        fn revoked_serials(&self, host_id: &str) -> Result<Vec<u64>, LedgerError> {
            let now = self.now();
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.refuse > 0 {
                state.refuse -= 1;
                return Err(LedgerError::Unread("the ledger is down".to_owned()));
            }
            Ok(state
                .certs
                .iter()
                .filter(|(serial, cert)| {
                    cert.host_id == host_id
                        && cert.expires_unix.saturating_add(1) > now
                        && state.revoked.contains_key(serial)
                })
                .map(|(serial, _)| *serial)
                .collect())
        }

        fn denied(&self, by: &str, host_id: &str, why: &str) -> Result<(), LedgerError> {
            let now = self.now();
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.write()?;
            state.append(Line {
                at_unix: now,
                by: by.to_owned(),
                action: DENIED,
                subject: Some(host_id.to_owned()),
                detail: denied_detail(why),
            });
            Ok(())
        }
    }
}

#[cfg(test)]
#[path = "ssh_ledger_tests.rs"]
mod tests;
