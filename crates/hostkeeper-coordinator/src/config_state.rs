//! The start's state files ([`super`]): the checks of what a stop left beside them (the marker
//! of changes that were in memory and not yet written, [`crate::ops::Marker`], and the marker of a
//! restore that did not finish, [`crate::backup::restore_marker`]), the files opened as one
//! backend ([`Files`]) under the three stores and CI's spent ids, and the refusal of a part the
//! backend could not load, as that part's store has always said it.

use std::io;
use std::path::Path;
use std::sync::Arc;

use hostkeeper_oidc::Clock;

use super::{Serve, StartError};
use crate::audit_file::Rotation;
use crate::backup::restore_marker;
use crate::jti_file::jti_path;
use crate::ops::{Marker, OpsState, unlanded};
use crate::store::{Backend, BackendError, Files, Loaded};
use crate::verdict_file::verdicts_path;

/// The state files `flags` name, the ops file at `ops` and the legacy service `legacy`, opened as
/// one backend and loaded, once the markers beside them are settled: a marker of changes not yet
/// written is refused when it lists nothing this build reads, and after the load when the ops
/// file does not hold every change it lists; a restore's marker is refused.
///
/// # Errors
/// [`StartError`]: a marker refuses the start, or a file cannot be opened or read.
pub(super) fn open(
    flags: &Serve,
    legacy: Option<&str>,
    ops: &Path,
    oidc_clock: &Clock,
) -> Result<(Files, Loaded), StartError> {
    let marker = refuse_unfinished_halt(ops)?;
    refuse_unfinished_restore(&flags.state_file)?;
    let rotation = Rotation {
        max_bytes: flags.audit_max_bytes,
        retained: flags.audit_retained_segments,
    };
    let files = Files::default()
        .with_jti(&jti_path(&flags.state_file), Arc::clone(oidc_clock))?
        .with_desired(&flags.state_file, legacy)
        .with_ops(ops, rotation)?
        .with_verdicts(&verdicts_path(&flags.state_file));
    let loaded = files.load()?;
    if let Some(numbers) = marker {
        settle_unfinished(ops, &numbers, &loaded.ops)?;
    }
    Ok((files, loaded))
}

impl From<BackendError> for StartError {
    /// The refusal of the part the backend could not load, as that part's store has always said
    /// it.
    fn from(why: BackendError) -> Self {
        match why {
            BackendError::Desired(why) => why.into(),
            BackendError::Ops(why) => why.into(),
            BackendError::Verdicts(why) => why.into(),
            BackendError::Jti(why) => why.into(),
            BackendError::Spent => Self::Io(io::Error::other(why)),
        }
    }
}

/// Refuses to start over the marker of changes that were in memory and not yet written to the
/// ops file at `ops` when the coordinator stopped ([`crate::ops::Marker`]) when it lists nothing
/// this build reads (the marker of the release before, which marked a halt): what it marked may
/// be lost, and the operator settles the halts file by hand and removes the marker. Answers the
/// audit numbers a marker lists, settled once the state is loaded ([`settle_unfinished`]).
///
/// # Errors
/// [`StartError::HaltWriteUnfinished`]: the marker lists nothing this build reads.
pub(crate) fn refuse_unfinished_halt(ops: &Path) -> Result<Option<Vec<u64>>, StartError> {
    let path = crate::ops_file::unfinished_path(ops);
    match Marker::read(&path) {
        None => Ok(None),
        Some(Marker::Listed(numbers)) => Ok(Some(numbers)),
        Some(Marker::Unlisted) => Err(StartError::HaltWriteUnfinished { path }),
    }
}

/// Settles the marker of the ops file at `ops`, which lists the audit `numbers`, against
/// `committed`, the state it holds: a marker whose every change landed before the coordinator
/// stopped is logged and removed; one with a change that did not refuses the start, naming it.
///
/// # Errors
/// [`StartError::ChangesUnwritten`]: a change it lists did not land. [`StartError::Io`]: the
/// marker could not be removed.
pub(crate) fn settle_unfinished(
    ops: &Path,
    numbers: &[u64],
    committed: &OpsState,
) -> Result<(), StartError> {
    let path = crate::ops_file::unfinished_path(ops);
    let numbers = unlanded(numbers, committed);
    if !numbers.is_empty() {
        return Err(StartError::ChangesUnwritten { path, numbers });
    }
    std::fs::remove_file(&path)?;
    tracing::warn!(
        path = %path.display(),
        "the changes the marker listed landed before the coordinator stopped: the marker is removed"
    );
    Ok(())
}

/// Refuses to start over the marker of a restore into the state file `state` that did not
/// finish ([`crate::backup::restore_marker`]): the state files may be part of a snapshot only.
///
/// # Errors
/// [`StartError::RestoreUnfinished`]: the marker is there.
pub(crate) fn refuse_unfinished_restore(state: &Path) -> Result<(), StartError> {
    let marker = restore_marker(state);
    if marker.exists() {
        return Err(StartError::RestoreUnfinished { path: marker });
    }
    Ok(())
}
