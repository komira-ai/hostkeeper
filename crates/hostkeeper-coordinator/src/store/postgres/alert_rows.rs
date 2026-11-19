//! The alerts observed now as rows of `alerts` (migration `0023`;
//! `docs/design/coordinator-multi-instance.md`, section 9, job `alerts`), so that of any number of
//! coordinators exactly one logs an alert when it is first observed: the one whose insert made
//! its row. An alert is its subject, six texts (the kind's log token, the service, the host, the
//! version, the request, the other host), empty where it names none.

use super::Postgres;
use super::writer::Tx;

/// The subject of an alert as its row's key: kind, service, host, version, request, other host.
pub(crate) type Key = [String; 6];

/// Of an observation of `found` at `at_unix`: each one's first observed second, and whether this
/// observation made its row (it is new, and this coordinator logs it). The rows of the alerts not
/// in `found` are deleted (they cleared), in the same transaction.
///
/// # Errors
/// The client's error.
pub(crate) fn note(
    tx: &mut Tx<'_>,
    found: &[Key],
    at_unix: i64,
) -> Result<Vec<(i64, bool)>, postgres::Error> {
    let column = |i: usize| -> Vec<&str> { found.iter().map(|key| key[i].as_str()).collect() };
    let (kind, service, host, version, request, other) = (
        column(0),
        column(1),
        column(2),
        column(3),
        column(4),
        column(5),
    );
    const FOUND: &str = "unnest($1::text[], $2::text[], $3::text[], $4::text[], $5::text[], \
                         $6::text[]) AS u(kind, service, host, version, request, other_host)";
    tx.execute(
        &format!(
            "DELETE FROM alerts a WHERE NOT EXISTS (SELECT 1 FROM {FOUND} \
             WHERE (u.kind, u.service, u.host, u.version, u.request, u.other_host) = \
                   (a.kind, a.service, a.host, a.version, a.request, a.other_host))"
        ),
        &[&kind, &service, &host, &version, &request, &other],
    )?;
    let made = tx.query(
        &format!(
            "INSERT INTO alerts (kind, service, host, version, request, other_host, first_unix) \
             SELECT DISTINCT u.kind, u.service, u.host, u.version, u.request, u.other_host, \
               $7::bigint FROM {FOUND} \
             ON CONFLICT DO NOTHING \
             RETURNING kind, service, host, version, request, other_host"
        ),
        &[&kind, &service, &host, &version, &request, &other, &at_unix],
    )?;
    let mut made: std::collections::BTreeSet<Key> = made.iter().map(key_of).collect();
    let held = tx.query(
        "SELECT kind, service, host, version, request, other_host, first_unix FROM alerts",
        &[],
    )?;
    let first: std::collections::BTreeMap<Key, i64> =
        held.iter().map(|row| (key_of(row), row.get(6))).collect();
    Ok(found
        .iter()
        .map(|key| {
            // A subject found twice is new once.
            let made = made.remove(key);
            (first.get(key).copied().unwrap_or(at_unix), made)
        })
        .collect())
}

fn key_of(row: &postgres::Row) -> Key {
    [
        row.get(0),
        row.get(1),
        row.get(2),
        row.get(3),
        row.get(4),
        row.get(5),
    ]
}

/// The alert rows of a coordinator over a database.
#[derive(Clone)]
pub(crate) struct PgAlerts {
    store: Postgres,
}

impl std::fmt::Debug for PgAlerts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PgAlerts").finish_non_exhaustive()
    }
}

impl PgAlerts {
    /// The alert rows of `store`.
    pub(crate) fn new(store: Postgres) -> PgAlerts {
        PgAlerts { store }
    }

    /// [`note`] in one transaction.
    ///
    /// # Errors
    /// Why the rows could not be written: nothing was noted.
    pub(crate) fn note(&self, found: Vec<Key>, at_unix: u64) -> Result<Vec<(u64, bool)>, String> {
        let at = i64::try_from(at_unix).unwrap_or(i64::MAX);
        self.store
            .write(move |tx| note(tx, &found, at))
            .map(|noted| {
                noted
                    .into_iter()
                    .map(|(first, made)| (u64::try_from(first).unwrap_or_default(), made))
                    .collect()
            })
            .map_err(|fail| fail.why().to_owned())
    }
}
