//! The book of the SSH tunnels ([`Book`]) in the database: the tables of migration `0020`,
//! `ssh_host`, `ssh_host_key_pin` and `ssh_tunnel`, and the audit chain of migration `0019` that
//! every line the book writes joins.
//!
//! Each write is one transaction of the writing session. A write that writes an audit line takes
//! the chain's advisory lock first ([`super::ssh::begin`]), so the line is the one after the
//! newest whichever coordinator instance wrote that, and the row and its line are both there or
//! neither is. Every time is the server's clock. The unique index of `ssh_tunnel` on the operator
//! and the host holds one open tunnel per operator per host across instances, because a tunnel's
//! row is deleted when it closes.

use super::Postgres;
use super::ssh::{append, begin, clock, lock, signed, unsigned};
use super::writer::WriteFail;
use crate::ssh_book::{
    ADDRESS_CHANGED, Book, BookError, Cause, Closing, HOSTKEY_CHANGED, HOSTKEY_PINNED, HostKeys,
    OpenRefused, Opened, Opening, PinRefused, Polled, TUNNEL_CLOSE, TUNNEL_OPEN, address_detail,
    close_detail, keys_detail, open_detail, same_set,
};
use crate::ssh_ledger::Line;

/// The `by` of the lines the coordinator itself writes: a host heard from elsewhere.
const COORDINATOR: &str = "coordinator";

/// The book over the store `store`.
pub(crate) struct PgBook {
    store: Postgres,
}

impl PgBook {
    /// The book of `store`.
    pub(crate) fn new(store: Postgres) -> PgBook {
        PgBook { store }
    }
}

fn book_error(fail: &WriteFail) -> BookError {
    BookError(fail.why().to_owned())
}

/// Ids as the database's `bigint`s.
fn signed_all(ids: &[u64]) -> Vec<i64> {
    ids.iter().copied().map(signed).collect()
}

/// The line of a tunnel that closed.
fn close_line(now: u64, by: String, host_id: String, detail: String) -> Line {
    Line {
        at_unix: now,
        by,
        action: TUNNEL_CLOSE,
        subject: Some(host_id),
        detail,
    }
}

impl Book for PgBook {
    fn heard(&self, host_id: &str, address: &str, reported: &[String]) -> Result<(), BookError> {
        let (host, address, reported) = (host_id.to_owned(), address.to_owned(), reported.to_vec());
        self.store
            .write(move |tx| {
                let now = clock(tx)?;
                let before = tx
                    .query_opt(
                        "SELECT address, reported FROM ssh_host WHERE host_id = $1 FOR UPDATE",
                        &[&host],
                    )?
                    .map(|row| (row.get::<_, String>(0), row.get::<_, Vec<String>>(1)));
                tx.execute(
                    "INSERT INTO ssh_host (host_id, address, seen_unix, reported) \
                     VALUES ($1, $2, $3, $4) \
                     ON CONFLICT (host_id) DO UPDATE SET address = EXCLUDED.address, \
                       seen_unix = GREATEST(ssh_host.seen_unix, EXCLUDED.seen_unix), \
                       reported = CASE WHEN cardinality(EXCLUDED.reported) = 0 \
                                  THEN ssh_host.reported ELSE EXCLUDED.reported END",
                    &[&host, &address, &signed(now), &reported],
                )?;
                let mut lines = Vec::new();
                if let Some((was, _)) = &before
                    && *was != address
                {
                    lines.push(Line {
                        at_unix: now,
                        by: COORDINATOR.to_owned(),
                        action: ADDRESS_CHANGED,
                        subject: Some(host.clone()),
                        detail: address_detail(was, &address),
                    });
                }
                if !reported.is_empty() {
                    let pinned: Option<Vec<String>> = tx
                        .query_opt(
                            "SELECT fingerprints FROM ssh_host_key_pin WHERE host_id = $1",
                            &[&host],
                        )?
                        .map(|row| row.get(0));
                    let was = before.as_ref().map(|(_, keys)| keys.as_slice());
                    if let Some(pinned) = pinned
                        && !same_set(&pinned, &reported)
                        && !same_set(was.unwrap_or_default(), &reported)
                    {
                        lines.push(Line {
                            at_unix: now,
                            by: COORDINATOR.to_owned(),
                            action: HOSTKEY_CHANGED,
                            subject: Some(host.clone()),
                            detail: keys_detail(&pinned, &reported),
                        });
                    }
                }
                if !lines.is_empty() {
                    lock(tx)?;
                    for line in &lines {
                        append(tx, line)?;
                    }
                }
                Ok(())
            })
            .map_err(|fail| book_error(&fail))
    }

    fn host_keys(&self, host_id: &str) -> Result<HostKeys, BookError> {
        let host = host_id.to_owned();
        self.store
            .read(move |session| {
                let reported = session
                    .query_opt("SELECT reported FROM ssh_host WHERE host_id = $1", &[&host])?
                    .map(|row| row.get::<_, Vec<String>>(0))
                    .unwrap_or_default();
                let pinned = session
                    .query_opt(
                        "SELECT fingerprints FROM ssh_host_key_pin WHERE host_id = $1",
                        &[&host],
                    )?
                    .map(|row| row.get::<_, Vec<String>>(0));
                Ok(HostKeys { pinned, reported })
            })
            .map_err(BookError)
    }

    fn pin(&self, by: &str, host_id: &str, fingerprints: &[String]) -> Result<(), PinRefused> {
        let (by, host, pinned) = (by.to_owned(), host_id.to_owned(), fingerprints.to_vec());
        let written = self.store.write(move |tx| {
            let now = begin(tx)?;
            let reported: Vec<String> = tx
                .query_opt("SELECT reported FROM ssh_host WHERE host_id = $1", &[&host])?
                .map(|row| row.get(0))
                .unwrap_or_default();
            if reported.is_empty() || !same_set(&reported, &pinned) {
                return Ok(false);
            }
            tx.execute(
                "INSERT INTO ssh_host_key_pin (host_id, fingerprints, by_name, at_unix) \
                 VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (host_id) DO UPDATE SET fingerprints = EXCLUDED.fingerprints, \
                   by_name = EXCLUDED.by_name, at_unix = EXCLUDED.at_unix",
                &[&host, &pinned, &by, &signed(now)],
            )?;
            append(
                tx,
                &Line {
                    at_unix: now,
                    by: by.clone(),
                    action: HOSTKEY_PINNED,
                    subject: Some(host.clone()),
                    detail: keys_detail(&pinned, &reported),
                },
            )?;
            Ok(true)
        });
        match written {
            Ok(true) => Ok(()),
            Ok(false) => Err(PinRefused::NotReported),
            Err(fail) => Err(PinRefused::Unwritten(fail.why().to_owned())),
        }
    }

    fn open(&self, opening: &Opening) -> Result<Opened, OpenRefused> {
        let opening = opening.clone();
        let written = self.store.write(move |tx| {
            let now = begin(tx)?;
            let cert = tx.query_opt(
                "SELECT 1 FROM ssh_cert c WHERE c.serial = $1 AND c.operator = $2 \
                 AND c.host_id = $3 AND c.expires_unix > $4 \
                 AND NOT EXISTS (SELECT 1 FROM ssh_revocation r WHERE r.serial = c.serial)",
                &[
                    &signed(opening.serial),
                    &opening.operator,
                    &opening.host_id,
                    &signed(now),
                ],
            )?;
            if cert.is_none() {
                return Ok(Err(OpenRefused::BadSerial));
            }
            let host = tx.query_opt(
                "SELECT address, seen_unix FROM ssh_host WHERE host_id = $1",
                &[&opening.host_id],
            )?;
            let Some(host) = host else {
                return Ok(Err(OpenRefused::NoAddress));
            };
            let address: String = host.get(0);
            let seen = unsigned(host.get(1));
            if seen < now.saturating_sub(opening.live_secs) {
                return Ok(Err(OpenRefused::HostNotLive));
            }
            let held: i64 = tx
                .query_one(
                    "SELECT count(*) FROM ssh_tunnel WHERE operator = $1",
                    &[&opening.operator],
                )?
                .get(0);
            if unsigned(held) >= opening.per_operator {
                return Ok(Err(OpenRefused::TooMany));
            }
            let inserted = tx.query_opt(
                "INSERT INTO ssh_tunnel \
                 (operator, host_id, cert_serial, instance, address, opened_unix, last_seen_unix) \
                 VALUES ($1, $2, $3, $4, $5, $6, $6) ON CONFLICT DO NOTHING RETURNING id",
                &[
                    &opening.operator,
                    &opening.host_id,
                    &signed(opening.serial),
                    &opening.instance,
                    &address,
                    &signed(now),
                ],
            )?;
            let Some(inserted) = inserted else {
                return Ok(Err(OpenRefused::TooMany));
            };
            let id = unsigned(inserted.get(0));
            append(
                tx,
                &Line {
                    at_unix: now,
                    by: opening.operator.clone(),
                    action: TUNNEL_OPEN,
                    subject: Some(opening.host_id.clone()),
                    detail: open_detail(id, opening.serial, &address, &opening.instance),
                },
            )?;
            Ok(Ok(Opened {
                id,
                address,
                opened_unix: now,
            }))
        });
        match written {
            Ok(answer) => answer,
            Err(fail) => Err(OpenRefused::Unwritten(fail.why().to_owned())),
        }
    }

    fn close(&self, closing: &Closing) -> Result<(), BookError> {
        let closing = closing.clone();
        self.store
            .write(move |tx| {
                let now = begin(tx)?;
                let gone = tx.query_opt(
                    "DELETE FROM ssh_tunnel WHERE id = $1 RETURNING operator, host_id, opened_unix",
                    &[&signed(closing.id)],
                )?;
                if let Some(row) = gone {
                    let opened = unsigned(row.get(2));
                    append(
                        tx,
                        &close_line(
                            now,
                            row.get(0),
                            row.get(1),
                            close_detail(
                                closing.id,
                                closing.cause,
                                Some((closing.bytes_up, closing.bytes_down)),
                                now.saturating_sub(opened),
                            ),
                        ),
                    )?;
                }
                Ok(())
            })
            .map_err(|fail| book_error(&fail))
    }

    fn poll(&self, instance: &str, ids: &[u64], stale_secs: u64) -> Result<Polled, BookError> {
        let (instance, own) = (instance.to_owned(), signed_all(ids));
        self.store
            .write(move |tx| {
                let now = begin(tx)?;
                tx.execute(
                    "UPDATE ssh_tunnel SET last_seen_unix = $1 \
                     WHERE instance = $2 AND id = ANY($3::bigint[])",
                    &[&signed(now), &instance, &own],
                )?;
                let revoked = tx
                    .query(
                        "SELECT t.id FROM ssh_tunnel t \
                         JOIN ssh_revocation r ON r.serial = t.cert_serial \
                         WHERE t.instance = $1 AND t.id = ANY($2::bigint[]) ORDER BY t.id",
                        &[&instance, &own],
                    )?
                    .iter()
                    .map(|row| unsigned(row.get(0)))
                    .collect();
                let mut lost = tx.query(
                    "DELETE FROM ssh_tunnel WHERE instance <> $1 AND last_seen_unix < $2 \
                     RETURNING id, operator, host_id, opened_unix",
                    &[&instance, &signed(now.saturating_sub(stale_secs))],
                )?;
                lost.sort_by_key(|row| row.get::<_, i64>(0));
                for row in &lost {
                    let opened = unsigned(row.get(3));
                    append(
                        tx,
                        &close_line(
                            now,
                            row.get(1),
                            row.get(2),
                            close_detail(
                                unsigned(row.get(0)),
                                Cause::InstanceLost,
                                None,
                                now.saturating_sub(opened),
                            ),
                        ),
                    )?;
                }
                let held: Vec<u64> = tx
                    .query(
                        "SELECT id FROM ssh_tunnel WHERE id = ANY($1::bigint[])",
                        &[&own],
                    )?
                    .iter()
                    .map(|row| unsigned(row.get(0)))
                    .collect();
                let gone = own
                    .iter()
                    .map(|id| unsigned(*id))
                    .filter(|id| !held.contains(id))
                    .collect();
                Ok(Polled {
                    revoked,
                    gone,
                    reaped: lost.len() as u64,
                })
            })
            .map_err(|fail| book_error(&fail))
    }
}
