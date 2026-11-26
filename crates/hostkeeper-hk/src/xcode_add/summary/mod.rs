//! The summary that ends `hk xcode add` (docs/design/xcode-one-action.md section 2.5): after
//! `stored:` and after `already in the store:`, which Macs will install the release and when.
//!
//! It reads every page of `Hosts.List` and `HostGroups.List` and the open runs ([`read`]), asks
//! of each Mac what the coordinator's own rule makes of the release ([`rule`]), asks
//! `Maintenance.Schedule` without `confirm` (which stores nothing) for each group with a due Mac
//! that no run takes, and prints the lines ([`text`]) on stdout. A Mac is a host of the platform
//! `darwin-arm64`; one the coordinator keeps no maintenance report of is left out. The release
//! is held by the store whether or not the listing the run last read shows it, so it is added to
//! that listing. With no Mac at all, stdout gets nothing and stderr says so.
//!
//! A call that fails ends the summary before it prints anything: stderr then says `summary
//! unavailable: <why>; the archive is stored`, with the two commands to read the Macs by hand,
//! and the run still exits 0.

mod read;
mod rule;
#[cfg(test)]
mod tests;
mod text;

use std::collections::BTreeMap;

use self::read::Preview;
use self::rule::{State, When};
use self::text::Row;
use super::store::{Held, Listing};
use crate::args::Globals;
use crate::xcode_push::say;
use crate::{PROGRAM, World};

/// Prints which Macs will install `release`, and when, after the store's `listing`.
pub(super) fn macs(w: &mut World<'_>, globals: &Globals, listing: &Listing, release: &Held) {
    match summary(w, globals, listing, release) {
        Ok(Some(lines)) => {
            for line in lines {
                let _ = writeln!(w.stdout, "{line}");
            }
        }
        Ok(None) => say(
            w,
            &format!(
                "the coordinator knows no Mac, so no Mac is due Xcode {}",
                release.version
            ),
        ),
        Err(why) => {
            say(
                w,
                &format!("summary unavailable: {why}; the archive is stored"),
            );
            say(
                w,
                &format!(
                    "to read it by hand: {PROGRAM} hosts list, and {PROGRAM} maintenance list"
                ),
            );
        }
    }
}

/// The lines of the summary; `None` when the coordinator knows no Mac.
fn summary(
    w: &mut World<'_>,
    globals: &Globals,
    listing: &Listing,
    release: &Held,
) -> Result<Option<Vec<String>>, String> {
    let macs = read::macs(w, globals)?;
    if macs.is_empty() {
        return Ok(None);
    }
    let policies = read::policies(w, globals)?;
    let open = read::open_runs(w, globals)?;
    let mut archives = listing.archives.clone();
    if !archives.iter().any(|held| held.sha256 == release.sha256) {
        archives.push(release.clone());
    }
    let rows: Vec<Row> = macs
        .iter()
        .map(|mac| {
            let policy = policies.get(&mac.group).cloned().unwrap_or_default();
            let state = match &mac.installed {
                None => {
                    State::LeftOut("the coordinator keeps no maintenance report of it".to_owned())
                }
                Some(installed) => rule::state(&mac.group, &policy, &archives, release, installed),
            };
            Row {
                id: mac.id.clone(),
                group: mac.group.clone(),
                when: rule::when(mac, &open),
                state,
            }
        })
        .collect();
    let mut previews = BTreeMap::new();
    for row in &rows {
        if row.state.is_due() && row.when == When::Free && !previews.contains_key(&row.group) {
            let preview: Preview = read::preview(w, globals, &row.group, &release.version)?;
            previews.insert(row.group.clone(), preview);
        }
    }
    Ok(Some(text::lines(release, &rows, &policies, &previews)))
}
