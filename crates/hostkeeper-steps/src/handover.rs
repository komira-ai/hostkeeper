//! The hand-over of the updater's own update, read at a binary's start
//! (docs/design/two-programs.md, section 4.3).
//!
//! A hand-over is the way a later release updates the updater: it stages the new binary as
//! `versions/<N>`, points the link `next` of the binary directory at it, and only then writes the
//! record as waiting for the restart into `N` ([`AwaitingRestart::next`] set, a start count of
//! 0), and exits; `current` is not moved. The unit then starts the binary `current` names, the
//! old one. This module is what every binary's start reads of it, before the daemon reads its
//! configuration ([`at_start`]):
//!
//! 1. it is not `N`, the record waits for `N`, `next` names an executable `versions/<N>`, and
//!    the count is below [`HAND_OVER_TRIES`]: the count is raised, written and synced, and the
//!    caller execs `versions/<N>` with its own arguments ([`AtStart::Exec`]);
//! 2. it is `N`: nothing is written here ([`AtStart::Settles`]); its settlement under the state
//!    directory's lock finishes the switch, confirms `Succeeded`, and only then removes `next`
//!    ([`crate::selfupdate::settle_with`]);
//! 3. it is not `N` and step 1 does not apply (the count has reached [`HAND_OVER_TRIES`], `next`
//!    is missing, names another version, or `versions/<N>` is not an executable file): `N` goes
//!    on the local list of bad versions, the update is settled `Failed` after the commit, of
//!    class release, [`crate::record::REASON_SELF_UPDATE_FALLBACK`], and `next` is removed
//!    ([`AtStart::FellBack`]). An exec that returned is settled the same way ([`exec_failed`]).
//!    Nothing was switched, so nothing switches back, and no wait is left standing.
//!
//! A wait without [`AwaitingRestart::next`], written the old way by a release that switched
//! `current` first, is none of these: the start leaves it to the settlement, which settles it as
//! it always did. A record a newer binary wrote is never changed.

use std::path::PathBuf;

use crate::bad::BadVersions;
use crate::record::{AwaitingRestart, Loaded, RecordStore, Settlement};
use crate::selfupdate::{Layout, SelfUpdateError, Settled};

/// The link of the binary directory that names the binary a hand-over waits to start.
pub const NEXT: &str = "next";

/// The starts of the new binary a hand-over makes before it settles the update `Failed`: the
/// binary `current` names execs `next` while the count is below this.
pub const HAND_OVER_TRIES: u32 = 3;

/// What the start of a binary does about a hand-over ([`at_start`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AtStart {
    /// No hand-over waits: no record, a record of a newer binary, no wait, or a wait written the
    /// old way. Nothing was written.
    NotPending,
    /// This binary is the version the hand-over waits for (step 2): its settlement confirms it.
    /// Nothing was written.
    Settles,
    /// Step 1: the count is raised to `starts` and written; the caller execs `binary`
    /// (`versions/<N>`) with its own arguments, and calls [`exec_failed`] if the exec returns.
    Exec { binary: PathBuf, starts: u32 },
    /// Step 3: the update is settled `Failed`, its version is bad here, and `next` is removed.
    FellBack,
}

/// The hand-over the record of `store` waits for, when it waits for one: a wait that carries
/// [`AwaitingRestart::next`]. Reads only; a record that is absent, newer, or waits the old way or
/// not at all gives `None`.
pub fn pending(store: &RecordStore) -> Result<Option<AwaitingRestart>, SelfUpdateError> {
    Ok(match store.load()? {
        Loaded::Current(rec) => rec.progress.awaiting_restart.filter(|a| a.next.is_some()),
        Loaded::Absent | Loaded::Newer { .. } => None,
    })
}

/// At the start of a binary of version `running`, before the daemon reads its configuration:
/// steps 1 to 3 of the module for the binary directory `layout` and the record of `store`. The
/// caller holds the state directory's lock, so that no daemon writes the record meanwhile.
pub fn at_start(
    layout: &Layout,
    store: &RecordStore,
    running: &str,
) -> Result<AtStart, SelfUpdateError> {
    match raise(layout, store, running)? {
        Some(at) => Ok(at),
        None => {
            hand_over_failed(layout, store)?;
            Ok(AtStart::FellBack)
        }
    }
}

/// Under the record lock: [`AtStart::NotPending`] or [`AtStart::Settles`] with nothing written,
/// or step 1 with the count raised; `None` when step 3 applies.
fn raise(
    layout: &Layout,
    store: &RecordStore,
    running: &str,
) -> Result<Option<AtStart>, SelfUpdateError> {
    let _held = store.hold();
    let mut rec = match store.load()? {
        Loaded::Current(rec) => rec,
        Loaded::Absent | Loaded::Newer { .. } => return Ok(Some(AtStart::NotPending)),
    };
    let handed = rec.progress.awaiting_restart.as_mut();
    let Some(awaiting) = handed.filter(|a| a.next.is_some()) else {
        return Ok(Some(AtStart::NotPending));
    };
    if awaiting.version == running {
        return Ok(Some(AtStart::Settles));
    }
    let starts = awaiting.starts.unwrap_or(0);
    if (starts >= HAND_OVER_TRIES) | !startable(layout, awaiting) {
        return Ok(None);
    }
    let starts = starts + 1;
    awaiting.starts = Some(starts);
    let binary = layout.version_path(&awaiting.version);
    store.save(&rec)?;
    Ok(Some(AtStart::Exec { binary, starts }))
}

/// Whether the hand-over of `awaiting` can start its binary: the wait and the link `next` both
/// name its version, and `versions/<version>` is an executable file. A link that cannot be read
/// starts nothing.
fn startable(layout: &Layout, awaiting: &AwaitingRestart) -> bool {
    let version = Some(awaiting.version.as_str());
    let linked = layout.next().ok().flatten();
    (awaiting.next.as_deref() == version)
        & (linked.as_deref() == version)
        & layout.check_binary(&awaiting.version).is_ok()
}

/// The exec of step 1 returned: the binary that runs is not the new one, and step 3 settles the
/// update `Failed` at once ([`AtStart::FellBack`]); [`Settled::NotPending`] when no hand-over
/// waits any more.
pub fn exec_failed(layout: &Layout, store: &RecordStore) -> Result<Settled, SelfUpdateError> {
    hand_over_failed(layout, store)
}

/// Step 3 for the hand-over the record of `store` waits for: its version on the list of bad
/// versions first, so that a crash never loses the mark, then the record `Failed` (its order id
/// kept for the verdict), then `next` removed. [`Settled::NotPending`], and nothing written, when
/// no hand-over waits.
pub(crate) fn hand_over_failed(
    layout: &Layout,
    store: &RecordStore,
) -> Result<Settled, SelfUpdateError> {
    let Some(awaiting) = pending(store)? else {
        return Ok(Settled::NotPending);
    };
    BadVersions::load(store.dir())?.add(&awaiting.version)?;
    store.settle_awaiting_restart(Settlement::FellBack)?;
    layout.remove_next()?;
    Ok(Settled::FellBack)
}
