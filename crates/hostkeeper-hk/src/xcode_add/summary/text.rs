//! The lines of the summary, in the shape of docs/design/xcode-one-action.md section 1: the Macs
//! that are due the release by group, what each one's run or the run to store says, and the Macs
//! that have it or are left out.

use std::collections::BTreeMap;

use super::read::{Policy, Preview, Turn, schedule_line};
use super::rule::{State, When};
use crate::xcode_add::store::Held;

/// What the summary found of one Mac.
pub(super) struct Row {
    pub(super) id: String,
    pub(super) group: String,
    pub(super) state: State,
    pub(super) when: When,
}

/// `2026-12-01 02:00 UTC` for `unix` seconds.
pub(super) fn utc(unix: u64) -> String {
    let (days, secs) = (unix / 86_400, unix % 86_400);
    // The civil date of a day count from the unix epoch (Howard Hinnant's algorithm), in days
    // counted from the first of March of year zero.
    let z = days + 719_468;
    let era = z / 146_097;
    let day_of_era = z % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = if month_from_march < 10 {
        month_from_march + 3
    } else {
        month_from_march - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        secs / 3_600,
        secs % 3_600 / 60
    )
}

/// `1 Mac`, `3 Macs`.
fn macs(n: usize) -> String {
    format!("{n} Mac{}", if n == 1 { "" } else { "s" })
}

/// The ids of `rows`, joined.
fn ids<'a>(rows: impl IntoIterator<Item = &'a str>) -> String {
    rows.into_iter().collect::<Vec<_>>().join(", ")
}

/// What the Mac's run says.
fn when_note(when: &When) -> Option<String> {
    Some(match when {
        When::Free => return None,
        When::Takes {
            run,
            window: Some((start, end)),
        } => {
            let end = if *end == 0 {
                "no end".to_owned()
            } else {
                utc(*end)
            };
            format!("in run {run}, window {} to {end}", utc(*start))
        }
        When::Takes { run, window: None } => format!("in run {run}, now"),
        When::NoXcode(run) => {
            format!("in run {run}, which does not take Xcode: schedule after it ends")
        }
        When::InTurn(run) => {
            format!("in run {run}, whose turn has already started: schedule after it ends")
        }
        When::Halted { run, group } => format!(
            "in run {run}, which is halted in group {group}: put the halted host back or cancel \
             the run, then schedule"
        ),
    })
}

/// The ids of the turns of `turns` at `step`.
fn at_step(turns: &[Turn], step: &str) -> Vec<String> {
    turns
        .iter()
        .filter(|turn| turn.step == step)
        .map(|turn| turn.host_id.clone())
        .collect()
}

/// The lines that say what a run of `group` for the release would be.
fn run_lines(group: &str, version: &str, batch: u32, preview: &Preview) -> Vec<String> {
    match preview {
        Preview::Run { batch: own, turns } => {
            let taken = at_step(turns, "MAINTENANCE_HOST_STEP_WAITING");
            let mut lines = vec![format!(
                "    no run takes it yet; a run would take {} (batch {})",
                taken.join(", "),
                own.unwrap_or(batch)
            )];
            let skipped: Vec<String> = turns
                .iter()
                .filter(|turn| turn.step == "MAINTENANCE_HOST_STEP_SKIPPED")
                .map(|turn| {
                    let reason = turn.skipped_because.to_lowercase();
                    let reason = reason.strip_prefix("maintenance_skip_").unwrap_or(&reason);
                    format!("{} ({reason})", turn.host_id)
                })
                .collect();
            if !skipped.is_empty() {
                lines.push(format!("    skipped: {}", skipped.join(", ")));
            }
            lines.push(
                "    no run takes it yet. Press Update on the Maintenance page, or:".to_owned(),
            );
            lines.push(format!("    {}", schedule_line(group, version, "")));
            lines
        }
        Preview::Refused { code, reason } => {
            let mut lines = vec![format!(
                "    no run takes it yet; the coordinator would refuse a run: {reason} ({code})"
            )];
            if reason == "last_host" {
                lines.push(format!(
                    "    a run that takes every host of {group} needs --take-last-host true:"
                ));
                lines.push(format!(
                    "    {}",
                    schedule_line(group, version, " --take-last-host true")
                ));
            }
            lines
        }
    }
}

/// The summary of `rows` for `release`, with the settings of each group and the preview of the run
/// of each group that has a Mac no run takes.
pub(super) fn lines(
    release: &Held,
    rows: &[Row],
    policies: &BTreeMap<String, Policy>,
    previews: &BTreeMap<String, Preview>,
) -> Vec<String> {
    let version = release.version.as_str();
    let due: Vec<&Row> = rows.iter().filter(|row| row.state.is_due()).collect();
    let mut lines = vec![if due.is_empty() {
        format!("Xcode {version} is due on no Mac:")
    } else {
        format!("Xcode {version} is now due on {}:", macs(due.len()))
    }];
    let mut groups: BTreeMap<&str, Vec<&Row>> = BTreeMap::new();
    for row in &due {
        groups.entry(row.group.as_str()).or_default().push(row);
    }
    for (group, members) in groups {
        let policy = policies.get(group).cloned().unwrap_or_default();
        lines.push(format!(
            "  {group} (xcode_keep {}, batch {}): {}",
            policy.keep,
            policy.batch,
            ids(members.iter().map(|row| row.id.as_str()))
        ));
        for row in &members {
            let state = match &row.state {
                State::Behind(newest) => {
                    Some(format!("due: {newest}, then {version} in the same turn"))
                }
                State::Unselected => Some("installed, not selected: due to be selected".to_owned()),
                _ => None,
            };
            for note in state.into_iter().chain(when_note(&row.when)) {
                lines.push(format!("    {}: {note}", row.id));
            }
        }
        if let Some(preview) = previews.get(group) {
            lines.extend(run_lines(group, version, policy.batch, preview));
        }
    }
    let installed: Vec<&str> = rows
        .iter()
        .filter(|row| row.state == State::Installed)
        .map(|row| row.id.as_str())
        .collect();
    if !installed.is_empty() {
        lines.push(format!("  installed: {}", ids(installed)));
    }
    let mut out: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for row in rows {
        if let State::LeftOut(why) = &row.state {
            out.entry(why.as_str()).or_default().push(row.id.as_str());
        }
    }
    for (why, left) in out {
        lines.push(format!("  left out: {} ({why})", ids(left)));
    }
    lines
}
