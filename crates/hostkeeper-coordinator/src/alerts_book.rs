//! The book of alerts: what the last observation found, with when each alert was first observed,
//! and the error log line an alert is logged with when an observation first finds it
//! ([`crate::alerts`]).

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use super::{Alert, Subject, Thresholds, log, rows};

/// An alert as an observation found it: with when it was first observed, unix seconds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observed {
    /// The alert.
    pub alert: Alert,
    /// When it was first observed.
    pub first_unix: u64,
}

/// The alerts the last observation found, each with when it was first observed: in memory, and,
/// over a database, in its rows too ([`Book::over`]), which decide which coordinator logs an alert.
#[derive(Debug, Default)]
pub struct Book {
    pub(super) thresholds: Thresholds,
    pub(super) seen: Mutex<BTreeMap<Subject, u64>>,
    pub(super) rows: Option<crate::store::postgres::alert_rows::PgAlerts>,
}

impl Book {
    /// A book that derives with `thresholds`.
    #[must_use]
    pub fn new(thresholds: Thresholds) -> Self {
        Self {
            thresholds,
            seen: Mutex::new(BTreeMap::new()),
            rows: None,
        }
    }

    /// Notes the alerts `find` finds, with the unix time it found them at, under the book's lock,
    /// so that two observations (the tick and an `Alerts.List`) are made one after the other and
    /// an older finding never follows a newer one: an alert the last observation did not find is
    /// first observed now and logged; one it found keeps its time; one it found and this did not
    /// is forgotten. When `find` fails nothing is noted.
    pub(super) fn observe_with<E>(
        &self,
        find: impl FnOnce() -> Result<(Vec<Alert>, u64), E>,
    ) -> Result<Vec<Observed>, E> {
        let mut seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        let (alerts, unix) = find()?;
        if let Some(observed) = rows::observed(self.rows.as_ref(), &alerts, unix) {
            *seen = observed
                .iter()
                .map(|one| (one.alert.subject.clone(), one.first_unix))
                .collect();
            return Ok(observed);
        }
        let observed: Vec<Observed> = alerts
            .into_iter()
            .map(|alert| {
                let first_unix = seen.get(&alert.subject).copied().unwrap_or_else(|| {
                    log(&alert);
                    unix
                });
                Observed { alert, first_unix }
            })
            .collect();
        *seen = observed
            .iter()
            .map(|one| (one.alert.subject.clone(), one.first_unix))
            .collect();
        Ok(observed)
    }
}
