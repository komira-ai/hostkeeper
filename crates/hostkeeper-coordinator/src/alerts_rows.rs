//! The book of alerts over a database's rows (`docs/design/coordinator-multi-instance.md`,
//! section 9, job `alerts`): an observation notes what it found in `alerts`, and an alert is
//! logged by the coordinator whose insert made its row, once however many coordinators observe
//! it. When the rows cannot be written the book falls back to its memory, so an alert is still
//! logged while the database is away (the alert that says so first of all).

use super::{Alert, Book, Observed, log};
use crate::store::postgres::alert_rows::{Key, PgAlerts};

impl Book {
    /// The same book noting its alerts in `rows`.
    #[must_use]
    pub(crate) fn over(mut self, rows: PgAlerts) -> Self {
        self.rows = Some(rows);
        self
    }

    /// The alerts `find` finds, for a list: over rows, each with the first observed second its
    /// row holds (now for one with no row yet), nothing written or logged; without rows, or when
    /// they cannot be read, an observation ([`Book::observe_with`]).
    pub(super) fn list_with<E>(
        &self,
        find: impl FnOnce() -> Result<(Vec<Alert>, u64), E>,
    ) -> Result<Vec<Observed>, E> {
        let Some(rows) = &self.rows else {
            return self.observe_with(find);
        };
        let (alerts, unix) = find()?;
        match rows.firsts(alerts.iter().map(key).collect()) {
            Ok(firsts) => Ok(alerts
                .into_iter()
                .zip(firsts)
                .map(|(alert, first)| Observed {
                    alert,
                    first_unix: first.unwrap_or(unix),
                })
                .collect()),
            Err(why) => {
                tracing::warn!(%why, "the alert rows could not be read: the alerts are observed by this coordinator");
                self.observe_with(|| Ok((alerts, unix)))
            }
        }
    }
}

/// The key of `alert`'s subject in the rows.
fn key(alert: &Alert) -> Key {
    let subject = &alert.subject;
    [
        subject.kind.log_token().to_owned(),
        subject.service.clone(),
        subject.host.clone(),
        subject.version.clone(),
        subject.request.clone(),
        subject.other_host.clone(),
    ]
}

/// The observation of `alerts` at `unix` as `rows` note it, each alert this coordinator made the
/// row of logged; none without rows or when they could not be written.
pub(super) fn observed(
    rows: Option<&PgAlerts>,
    alerts: &[Alert],
    unix: u64,
) -> Option<Vec<Observed>> {
    let noted = rows?.note(alerts.iter().map(key).collect(), unix);
    let noted = noted
        .inspect_err(|why| {
            tracing::warn!(%why, "the alert rows could not be written: the alerts are judged by this coordinator's memory");
        })
        .ok()?;
    Some(
        alerts
            .iter()
            .zip(noted)
            .map(|(alert, (first_unix, made))| {
                if made {
                    log(alert);
                }
                Observed {
                    alert: alert.clone(),
                    first_unix,
                }
            })
            .collect(),
    )
}
