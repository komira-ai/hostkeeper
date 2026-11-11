//! `hostkeeper-coordinator export`: the coordinator's state moved out of the database of
//! `--database-dsn-file` into files of the file store (`docs/design/coordinator-storage.md`,
//! section 11; `docs/operations/coordinator-state.md`).
//!
//! [`export`] reads the whole state of the database under its lock, and writes it with the file
//! store's own encodings into targets that hold no state, through the write path of `restore`
//! ([`crate::backup`]): never over a file, with the marker of an unfinished restore while it
//! writes, and what it wrote removed again when a write fails. The desired-state file is in the
//! single-state form exactly when the file store would write it so (the legacy service is the
//! only service). An audit history the database's import record says was pruned is written as a
//! sealed segment past the first, so the file store says the same time of the oldest line it
//! holds. It prints the rows of every table.

use std::io;
use std::path::{Path, PathBuf};

use clap::Args;
use hostkeeper_enrollment::enroll::StoreError as RecordsError;
use hostkeeper_mdm_gate::encode_record;

use crate::audit_file::{self, UnixClock};
use crate::backup::{self, BackupError, Report, StatePaths};
use crate::config::StartError;
use crate::desired;
use crate::jti_file::{self, jti_path};
use crate::lock::StateLock;
use crate::mdm::DEVICES_DIR;
use crate::ops_file;
use crate::releases::{self, releases_path};
use crate::repositories::{self, Registry, repositories_path};
use crate::store::postgres::transfer::{self, Moved};
use crate::store::postgres::{DatabaseError, Dsn};
use crate::verdict_file::{self, verdicts_path};

/// The flags of `export`.
#[derive(Clone, Debug, Args)]
pub struct Export {
    /// The database to export, as `serve --database-dsn-file` names it. Its lock is taken, so a
    /// coordinator serving on it refuses the export.
    #[arg(long)]
    pub database_dsn_file: PathBuf,
    /// Where the file store's state goes: the flags `serve` will be started with.
    #[command(flatten)]
    pub paths: StatePaths,
    /// The legacy service, as `serve --legacy-service` will name it: the desired-state file is
    /// written in the single-state form when it is the only service.
    #[arg(long)]
    pub legacy_service: Option<String>,
    /// Export into targets that hold no state. Required: the export writes only where none of
    /// the state files is and the enrollment directory is missing or empty, and never writes
    /// over a file.
    #[arg(long, required = true)]
    pub into_empty: bool,
}

/// Why an export was refused, or failed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TransferError {
    /// A lock, a target, or a file of the state could not be taken, read or written.
    #[error(transparent)]
    Files(#[from] BackupError),
    /// A file of the state is one `serve` refuses.
    #[error(transparent)]
    Refused(#[from] StartError),
    /// The enrollment store holds what `serve` refuses.
    #[error("--enrollment-dir: {0}")]
    Records(#[from] RecordsError),
    /// The database refused the export.
    #[error(transparent)]
    Database(#[from] DatabaseError),
    /// The database holds the enrollment store or the MDM gate's device records, and no
    /// `--enrollment-dir` says where they go.
    #[error(
        "the database holds the enrollment store or the MDM gate's device records, and --enrollment-dir is not given: exporting without it would lose them"
    )]
    EnrollmentDirNeeded,
}

/// The error of a file at `path` that could not be read or written.
fn io_at(path: &Path) -> impl FnOnce(io::Error) -> BackupError + '_ {
    move |source| BackupError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// The lines that say how many rows of each table `counts` holds.
fn table_lines(counts: &[(&str, i64)]) -> Vec<String> {
    counts
        .iter()
        .map(|(table, rows)| format!("  {table}: {rows} rows"))
        .collect()
}

/// The files of the file store `paths` names that hold `moved`, each with its bytes, the
/// desired-state file in the single-state form when `legacy` is its only service.
fn render(
    paths: &StatePaths,
    legacy: Option<&str>,
    moved: &Moved,
) -> Result<Vec<(PathBuf, Vec<u8>)>, TransferError> {
    let state = &paths.state_file;
    let ops = paths.ops_file();
    let loaded = &moved.loaded;
    let text = desired::encode(&loaded.desired, legacy)
        .map_err(io::Error::from)
        .map_err(io_at(state))?;
    let mut files = vec![(state.clone(), text)];
    if !loaded.desired.releases.is_empty() {
        let path = releases_path(state);
        let text = releases::encode_file(&loaded.desired.releases)
            .map_err(io::Error::from)
            .map_err(io_at(&path))?;
        files.push((path, text));
    }
    // A registry emptied by its last removal is written too: its generation is not 0.
    if loaded.desired.repositories != Registry::default() {
        let path = repositories_path(state);
        let text = repositories::encode_file(&loaded.desired.repositories)
            .map_err(io::Error::from)
            .map_err(io_at(&path))?;
        files.push((path, text));
    }
    if !loaded.spent.is_empty() {
        let text = jti_file::text_of_spent(&loaded.spent);
        files.push((jti_path(state), text.into_bytes()));
    }
    if loaded.verdicts != verdict_file::Held::default() {
        let path = verdicts_path(state);
        let text = verdict_file::text_of(&path, &loaded.verdicts).map_err(StartError::from)?;
        files.push((path, text));
    }
    let (ops_text, halts) = ops_file::texts(&ops, &loaded.ops).map_err(StartError::from)?;
    files.push((ops.clone(), ops_text));
    files.extend(halts.map(|text| (ops_file::halts_path(&ops), text)));
    if !moved.audit.is_empty() {
        let active = audit_file::path_of(&ops);
        // A history whose oldest lines were deleted is one sealed segment past the first: the
        // file store then says the time of its first line is where its history begins.
        let path = match moved.pruned_before {
            Some(_) => audit_file::segment_path(&active, 2),
            None => active,
        };
        files.push((path, audit_file::history_text(&moved.audit)));
    }
    if moved.records.is_empty() && moved.devices.is_empty() {
        return Ok(files);
    }
    let Some(dir) = &paths.enrollment_dir else {
        return Err(TransferError::EnrollmentDirNeeded);
    };
    for (path, bytes) in moved.records.files()? {
        files.push((dir.join(path), bytes));
    }
    // Each device record as `FileDeviceStore` writes it: `mdm-devices/<enrollment id>.pb`.
    for record in &moved.devices {
        let name = format!("{}.pb", record.enrollment_id);
        files.push((dir.join(DEVICES_DIR).join(name), encode_record(record)));
    }
    Ok(files)
}

/// Exports the database of `flags.database_dsn_file` into the targets `flags.paths` names, which
/// hold no state, under the lock of their state files taken by `lock`; CI's spent ids past at the
/// time `clock` says are left out.
///
/// # Errors
/// [`TransferError`]: the database refused, a coordinator holds its lock, it holds the enrollment
/// store and no `--enrollment-dir` is given, a target holds state, or a write failed (what the
/// export wrote is then removed).
pub(crate) fn export(
    flags: &Export,
    lock: &dyn StateLock,
    clock: &UnixClock,
) -> Result<Report, TransferError> {
    let dsn = Dsn::read(&flags.database_dsn_file).map_err(DatabaseError::from)?;
    let (moved, counts) = transfer::export(&dsn, clock())?;
    let files = render(&flags.paths, flags.legacy_service.as_deref(), &moved)?;
    backup::write_targets(&flags.paths, lock, &files)?;
    let mut lines = vec![format!(
        "hostkeeper-coordinator export: {} files written from the database",
        files.len()
    )];
    for (path, bytes) in &files {
        lines.push(format!("  {} {} bytes", path.display(), bytes.len()));
    }
    lines.push("the rows of each table:".to_owned());
    lines.extend(table_lines(&counts));
    lines.push(format!("the audit file holds {} lines", moved.audit.len()));
    Ok(Report::of(lines))
}

#[cfg(test)]
#[path = "transfer_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "transfer_export_tests.rs"]
mod export_tests;

#[cfg(test)]
#[path = "transfer_releases_tests.rs"]
mod releases_tests;

#[cfg(test)]
#[path = "transfer_repositories_tests.rs"]
mod repositories_tests;

#[cfg(test)]
#[path = "transfer_host_groups_tests.rs"]
mod host_groups_tests;

#[cfg(test)]
#[path = "transfer_mdm_tests.rs"]
mod mdm_tests;

#[cfg(test)]
#[path = "transfer_readers_tests.rs"]
mod readers_tests;
