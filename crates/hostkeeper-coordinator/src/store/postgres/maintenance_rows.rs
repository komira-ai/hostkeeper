//! The maintenance runs of the operations state as the rows of `0029`'s tables, and back
//! (`crate::maintenance_runs`): one row of `maintenance_runs` for each run, in the order the runs
//! are held, and one row of `maintenance_hosts` for each host's turn of one, in the run's order.
//! A kind, a step and a reason a host is left out are written by their names; a value a run
//! leaves out (no group, no batch, no drain deadline, no cancel) is `NULL`.

use super::at::At;
use super::rows::{KeyTy, KeyVal, Rows, Table, Tables, Ty, Val};
use super::state::int;
use crate::desired::StoreError;
use crate::maintenance_runs::{Kind, Run, RunHost, Skip, Step};
use crate::ops::OpsError;

/// The table of the runs.
pub(crate) const RUNS: Table = Table {
    name: "maintenance_runs",
    keys: &[("run_id", KeyTy::Text)],
    values: &[
        ("kinds", Ty::Texts),
        ("host_ids", Ty::Texts),
        ("host_group", Ty::Text),
        ("window_start_unix", Ty::Int),
        ("window_end_unix", Ty::Int),
        ("batch", Ty::Int),
        ("drain_deadline_secs", Ty::Int),
        ("take_last_host", Ty::Bool),
        ("reason", Ty::Text),
        ("by_name", Ty::Text),
        ("scheduled_unix", Ty::Int),
        ("cancelled_unix", Ty::Int),
        ("position", Ty::Int),
    ],
};

/// The table of each host's turn of a run.
pub(crate) const HOSTS: Table = Table {
    name: "maintenance_hosts",
    keys: &[("run_id", KeyTy::Text), ("host_id", KeyTy::Text)],
    values: &[
        ("host_group", Ty::Text),
        ("step", Ty::Text),
        ("since_unix", Ty::Int),
        ("kinds", Ty::Texts),
        ("applied", Ty::Texts),
        ("skipped_because", Ty::Text),
        ("reason", Ty::Text),
        ("position", Ty::Int),
    ],
};

fn text(s: &str) -> KeyVal {
    KeyVal::Text(s.to_owned())
}

/// `kinds` by their names.
fn names(kinds: &[Kind]) -> Val {
    Val::Texts(kinds.iter().map(|kind| kind.as_str().to_owned()).collect())
}

/// `n` as a `bigint`, or `NULL` for none.
fn int_or_null(n: Option<u64>) -> Result<Val, StoreError> {
    n.map_or(Ok(Val::Null), |n| int(n).map(Val::Int))
}

/// Puts the rows of `runs` into `tables`, under the tables' names.
///
/// # Errors
/// [`StoreError`]: a time or a count is above the largest the database keeps.
pub(crate) fn insert_runs(runs: &[Run], tables: &mut Tables) -> Result<(), StoreError> {
    let (mut run_rows, mut host_rows) = (Rows::new(), Rows::new());
    for (position, run) in runs.iter().enumerate() {
        run_rows.insert(
            vec![text(&run.id)],
            vec![
                names(&run.kinds),
                Val::Texts(run.host_ids.clone()),
                Val::text_or_null(Some(run.host_group.as_str()).filter(|g| !g.is_empty())),
                Val::Int(int(run.window_start_unix)?),
                Val::Int(int(run.window_end_unix)?),
                int_or_null(run.batch.map(u64::from))?,
                int_or_null(run.drain_deadline_secs)?,
                Val::Bool(run.take_last_host),
                Val::Text(run.reason.clone()),
                Val::Text(run.by.clone()),
                Val::Int(int(run.scheduled_unix)?),
                int_or_null(run.cancelled_unix)?,
                Val::Int(int(position as u64)?),
            ],
        );
        for (position, host) in run.hosts.iter().enumerate() {
            host_rows.insert(
                vec![text(&run.id), text(&host.host_id)],
                vec![
                    Val::text_or_null(Some(host.host_group.as_str()).filter(|g| !g.is_empty())),
                    Val::Text(host.step.as_str().to_owned()),
                    Val::Int(int(host.since_unix)?),
                    names(&host.kinds),
                    names(&host.applied),
                    Val::text_or_null(host.skipped_because.map(Skip::as_str)),
                    Val::Text(host.reason.clone()),
                    Val::Int(int(position as u64)?),
                ],
            );
        }
    }
    tables.insert(RUNS.name, run_rows);
    tables.insert(HOSTS.name, host_rows);
    Ok(())
}

/// The list of texts of column `i` of `at`.
fn texts(at: &At<'_>, i: usize) -> Result<Vec<String>, OpsError> {
    match at.values.get(i) {
        Some(Val::Texts(texts)) => Ok(texts.clone()),
        _ => Err(at.no("a column is not a list of texts")),
    }
}

/// The kinds column `i` of `at` names.
fn kinds(at: &At<'_>, i: usize) -> Result<Vec<Kind>, OpsError> {
    texts(at, i)?
        .iter()
        .map(|name| {
            Kind::parse(name).ok_or_else(|| at.no(format!("{name:?} is not a maintenance kind")))
        })
        .collect()
}

/// The counter of column `i` of `at`, or none for `NULL`.
fn count_or_null(at: &At<'_>, i: usize) -> Result<Option<u64>, OpsError> {
    match at.values.get(i) {
        Some(Val::Null) => Ok(None),
        _ => at.count(i).map(Some),
    }
}

/// The boolean of column `i` of `at`.
fn boolean(at: &At<'_>, i: usize) -> Result<bool, OpsError> {
    match at.values.get(i) {
        Some(Val::Bool(b)) => Ok(*b),
        _ => Err(at.no("a column is not a boolean")),
    }
}

/// The runs `tables` hold, in their order: every value parsed again, a row that does not parse
/// refused, naming its table and its key, and a host's row of a run no row holds refused; a table
/// they lack reads as empty.
///
/// # Errors
/// [`OpsError::Invalid`]: a row holds what this build cannot read.
pub(crate) fn runs_of(tables: &Tables) -> Result<Vec<Run>, OpsError> {
    let empty = Rows::new();
    let rows = |table: &'static Table| {
        tables
            .get(table.name)
            .unwrap_or(&empty)
            .iter()
            .map(move |(key, values)| At {
                table: table.name,
                key,
                values,
            })
    };
    let mut runs = Vec::new();
    for at in rows(&RUNS) {
        let batch = count_or_null(&at, 5)?
            .map(|batch| u32::try_from(batch).map_err(|_| at.no("the batch is too large")))
            .transpose()?;
        let run = Run {
            id: at.key(0)?,
            kinds: kinds(&at, 0)?,
            host_ids: texts(&at, 1)?,
            host_group: at.text(2)?.unwrap_or_default(),
            window_start_unix: at.count(3)?,
            window_end_unix: at.count(4)?,
            batch,
            drain_deadline_secs: count_or_null(&at, 6)?,
            take_last_host: boolean(&at, 7)?,
            reason: at.required(8)?,
            by: at.required(9)?,
            scheduled_unix: at.count(10)?,
            cancelled_unix: count_or_null(&at, 11)?,
            hosts: Vec::new(),
        };
        runs.push((at.position(12)?, run, Vec::new()));
    }
    for at in rows(&HOSTS) {
        let run_id = at.key(0)?;
        let step = at.required(1)?;
        let skipped_because = at
            .text(5)?
            .map(|name| {
                Skip::parse(&name)
                    .ok_or_else(|| at.no(format!("{name:?} is not why a host is left out")))
            })
            .transpose()?;
        let host = RunHost {
            host_id: at.key(1)?,
            host_group: at.text(0)?.unwrap_or_default(),
            step: Step::parse(&step)
                .ok_or_else(|| at.no(format!("{step:?} is not a step of a host's turn")))?,
            since_unix: at.count(2)?,
            kinds: kinds(&at, 3)?,
            applied: kinds(&at, 4)?,
            skipped_because,
            reason: at.text(6)?.unwrap_or_default(),
        };
        let position = at.position(7)?;
        let (_, _, hosts) = runs
            .iter_mut()
            .find(|(_, run, _)| run.id == run_id)
            .ok_or_else(|| at.no("no run holds the host's turn"))?;
        hosts.push((position, host));
    }
    runs.sort_by_key(|(position, _, _)| *position);
    Ok(runs
        .into_iter()
        .map(|(_, mut run, mut hosts)| {
            hosts.sort_by_key(|(position, _)| *position);
            run.hosts = hosts.into_iter().map(|(_, host)| host).collect();
            run
        })
        .collect())
}

#[cfg(test)]
#[path = "maintenance_rows_tests.rs"]
mod tests;
