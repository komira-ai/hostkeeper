//! The number a desired version is set under (`docs/design/coordinator-multi-instance.md`,
//! sections 5.1, 5.2 and 16): the queue of the services that wait for their turn to roll
//! ([`crate::queue`]) is ordered by it, so every coordinator on the database orders alike.
//!
//! The number is `desired.set_seq` or `group_desired.set_seq`, taken from `desired_seq`, with the
//! version it was taken for in `set_for`. The columns are in none of the part's table
//! descriptors, so a row's write ([`super::rows`]) never lists them and a change of any other
//! column keeps them; they are written by exactly two things:
//!
//! - [`stamps`], in the transaction of a commit of the desired state, for each desired version the
//!   commit sets or changes (a rollback is a change). The gate is held there, so the numbers
//!   follow the commits. Memory does not know the numbers just taken, so a coordinator that made
//!   the commit forgets its copy's version ([`super::edit::Open::commit`]) and reads the part
//!   again before it next answers.
//! - [`restamp`], at start, for the rows a release before wrote or changed, which knows neither
//!   column.
//!
//! A load reads the numbers with the rows of the desired part ([`read`]) into
//! [`crate::desired::Held::seqs`]; the files hold none of them.

use std::collections::BTreeMap;

use super::rows::{Stmt, Val};
use super::session::Session;
use crate::desired::Held;

/// The row a desired version was set for: its group (none for an ungrouped service), its service,
/// and the version.
type Moved<'a> = (Option<&'a str>, &'a str, String);

/// The desired versions `after` sets or changes from `before`: new, or not the version `before`
/// held (a rollback to the last good version is a change).
fn moved<'a>(before: &Held, after: &'a Held) -> Vec<Moved<'a>> {
    let mut out = Vec::new();
    for (service, now) in &after.desired {
        if before.desired.get(service).map(|was| &was.version) != Some(&now.version) {
            out.push((None, service.as_str(), now.version.to_string()));
        }
    }
    for (name, group) in &after.host_groups.0 {
        let held = before.host_groups.0.get(name);
        for (package, now) in &group.desired {
            let was = held.and_then(|held| held.desired.get(package));
            if was.map(|was| &was.version) != Some(&now.version) {
                out.push((Some(name.as_str()), package, now.version.to_string()));
            }
        }
    }
    out
}

/// Whether a commit from `before` to `after` takes a number ([`stamps`] is not empty).
pub(super) fn stamped(before: &Held, after: &Held) -> bool {
    !moved(before, after).is_empty()
}

/// The statements that number each desired version the commit from `before` to `after` sets or
/// changes, to run after the rows are written: one number each, taken only when the row's
/// `set_for` is not already the version.
pub(super) fn stamps(before: &Held, after: &Held) -> Vec<Stmt> {
    moved(before, after)
        .into_iter()
        .map(|(group, service, version)| match group {
            None => Stmt {
                sql: "UPDATE desired SET set_seq = nextval('desired_seq'), set_for = $2 \
                      WHERE service = $1 AND set_for IS DISTINCT FROM $2"
                    .to_owned(),
                params: vec![Val::Text(service.to_owned()), Val::Text(version)],
            },
            Some(group) => Stmt {
                sql: "UPDATE group_desired SET set_seq = nextval('desired_seq'), set_for = $3 \
                      WHERE host_group = $1 AND service = $2 AND set_for IS DISTINCT FROM $3"
                    .to_owned(),
                params: vec![
                    Val::Text(group.to_owned()),
                    Val::Text(service.to_owned()),
                    Val::Text(version),
                ],
            },
        })
        .collect()
}

/// Every number the rows hold, by (group, service), in the transaction `session` is in.
///
/// # Errors
/// The client's error.
pub(super) fn read(
    session: &mut Session,
) -> Result<BTreeMap<(Option<String>, String), i64>, postgres::Error> {
    let rows = session.query(
        "SELECT NULL::text, service, set_seq FROM desired WHERE set_seq IS NOT NULL \
         UNION ALL \
         SELECT host_group, service, set_seq FROM group_desired WHERE set_seq IS NOT NULL",
        &[],
    )?;
    Ok(rows
        .iter()
        .map(|row| ((row.get(0), row.get(1)), row.get(2)))
        .collect())
}

/// The rows to number at start, in the order the release before queued them: the ungrouped
/// services by name, which that release never queued, so they take the lowest numbers; then each
/// package of each group by the group's last change, the group's name, the package's place in the
/// group's list and its name. A package the group no longer lists, which no queue holds, comes
/// last in its group.
const UNNUMBERED: [&str; 2] = [
    "SELECT NULL::text, service FROM desired \
     WHERE set_seq IS NULL OR set_for IS DISTINCT FROM version ORDER BY service",
    "SELECT g.host_group, g.service FROM group_desired g \
     JOIN host_groups h ON h.name = g.host_group \
     LEFT JOIN host_group_packages p ON p.host_group = g.host_group AND p.service = g.service \
     WHERE g.set_seq IS NULL OR g.set_for IS DISTINCT FROM g.version \
     ORDER BY h.at_unix, g.host_group, coalesce(p.position, 9223372036854775807), g.service",
];

/// Numbers, at start, every desired version a release before set without a number: raises
/// `desired_seq` to at least the highest number held, then gives each row with no number, or whose
/// `set_for` is not its `version` (a release before changed it), a new one in the order of the
/// release before's queue ([`UNNUMBERED`]), and moves the desired part's version on when it
/// numbered any. In one transaction that holds the gate, so no commit of a desired version
/// numbers meanwhile; run after the shared lock is held and before the part is loaded. A second
/// start finds nothing to number and changes nothing. True when it numbered a row.
///
/// # Errors
/// The client's error: nothing is changed.
pub(super) fn restamp(session: &mut Session) -> Result<bool, postgres::Error> {
    session.batch("BEGIN")?;
    match restamp_held(session) {
        Ok(numbered) => {
            session.batch("COMMIT")?;
            Ok(numbered)
        }
        Err(e) => {
            // A session that cannot roll back is broken, and its caller drops it.
            let _ = session.batch("ROLLBACK");
            Err(e)
        }
    }
}

/// [`restamp`] in the transaction `session` is in.
fn restamp_held(session: &mut Session) -> Result<bool, postgres::Error> {
    session.query_one("SELECT 1 FROM rollout_gate WHERE id = 1 FOR UPDATE", &[])?;
    let highest: Option<i64> = session
        .query_one(
            "SELECT greatest((SELECT max(set_seq) FROM desired), \
                             (SELECT max(set_seq) FROM group_desired))",
            &[],
        )?
        .get(0);
    let taken = session.query_one("SELECT last_value, is_called FROM desired_seq", &[])?;
    let last = if taken.get::<_, bool>(1) {
        taken.get::<_, i64>(0)
    } else {
        0
    };
    if let Some(highest) = highest.filter(|highest| *highest > last) {
        session.query_one("SELECT setval('desired_seq', $1)", &[&highest])?;
    }
    let mut numbered = false;
    for (sql, table) in UNNUMBERED.iter().zip(["desired", "group_desired"]) {
        for row in session.query(sql, &[])? {
            let (group, service): (Option<String>, String) = (row.get(0), row.get(1));
            let set = "SET set_seq = nextval('desired_seq'), set_for = version";
            match group {
                None => session.execute(
                    &format!("UPDATE {table} {set} WHERE service = $1"),
                    &[&service],
                )?,
                Some(group) => session.execute(
                    &format!("UPDATE {table} {set} WHERE host_group = $1 AND service = $2"),
                    &[&group, &service],
                )?,
            };
            numbered = true;
        }
    }
    if numbered {
        session.execute(
            "UPDATE state_versions SET version = version + 1 WHERE part = 'desired'",
            &[],
        )?;
    }
    Ok(numbered)
}
