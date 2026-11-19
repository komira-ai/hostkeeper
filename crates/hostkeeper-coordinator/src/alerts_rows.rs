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
