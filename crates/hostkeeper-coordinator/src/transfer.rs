//! `hostkeeper-coordinator import` and `export`: the coordinator's state moved from the file
//! store into the database of `--database-dsn-file`, and back (`docs/design/coordinator-storage.md`,
//! section 11; `docs/operations/coordinator-state.md`).
//!
//! [`import`] reads every file `serve` keeps (the files named by `--state-file`, `--ops-file` and
//! `--enrollment-dir`, the single-state desired file read as the state of `--legacy-service`)
//! with the strict readers `serve` uses, and writes nothing to them: no staged halts file is
//! settled, no file of spent ids rewritten, no directory of the enrollment store made. Without
//! `--apply` it says what it would import, table by table, and whether the database holds state,
//! and writes nothing anywhere. With `--apply` it first takes the lock of `<state-file>.lock`, so
//! a running file coordinator refuses it, and refuses the marker of changes that were not yet
//! written and the marker of a restore that stopped partway; then, in one transaction of the
//! database ([`crate::store::postgres::transfer`]), writes every table, the whole audit history
//! with its numbers, the import's record (the manifest of the files it read and the time of the
//! oldest audit line they still held) and the audit row `state_imported`, reads it all back and
//! compares it with the files. It changes no file: the file coordinator can start again on them.
//!
//! [`export`] reads the whole state of the database under its lock, and writes it with the file
//! store's own encodings into targets that hold no state, through the write path of `restore`
//! ([`crate::backup`]): never over a file, with the marker of an unfinished restore while it
//! writes, and what it wrote removed again when a write fails. The desired-state file is in the
//! single-state form exactly when the file store would write it so (the legacy service is the
//! only service). An audit history the import found pruned is written as a sealed segment past
//! the first, so the file store says the same time of the oldest line it holds. It prints the
//! rows of every table.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::Args;
use hostkeeper_enrollment::enroll::StoreError as RecordsError;
use hostkeeper_enrollment::enroll::records::{FileRecords, Snapshot};
use hostkeeper_enrollment::fs::RealFs;

use crate::audit_file::{self, AuditLine, AuditLog, DEFAULT_ROTATION, Detail, UnixClock};
use crate::backup::{self, BackupError, Report, StatePaths, restore_marker};
use crate::config::StartError;
use crate::desired;
use crate::jti_file::{self, jti_path};
use crate::lock::StateLock;
use crate::mdm::DEVICES_DIR;
use crate::ops::{self, Marker, OpsState};
use crate::ops_file;
use crate::reconcile::COORDINATOR;
use crate::store::postgres::transfer::{self, ACTION_IMPORTED, MoveError, Moved};
use crate::store::postgres::{DatabaseError, Dsn};
use crate::store::{Files, Loaded};
use crate::verdict_file::{self, verdicts_path};

/// The flags of `import`.
#[derive(Clone, Debug, Args)]
pub struct Import {
    /// Where the file store's state is: the flags `serve` names it by.
    #[command(flatten)]
    pub paths: StatePaths,
    /// The legacy service, as `serve --legacy-service` names it: a desired-state file in the
    /// single-state form is the state of this service.
    #[arg(long)]
    pub legacy_service: Option<String>,
    /// The database to import into, as `serve --database-dsn-file` names it: it must hold no
    /// state.
    #[arg(long)]
    pub database_dsn_file: PathBuf,
    /// Import. Without it, the files and the database are read, what would be imported is said
    /// table by table, and nothing is written.
    #[arg(long)]
    pub apply: bool,
}

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

/// Why an import or an export was refused, or failed.
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
    /// The database refused the import or the export.
    #[error(transparent)]
    Database(#[from] MoveError),
    /// Changes stood in memory and were not yet written when the file coordinator stopped.
    #[error(
        "{path}: changes stood in memory and were not yet written when the file coordinator stopped; start it once on these files, so that it settles them, stop it, and import again"
    )]
    Unsettled {
        /// The marker.
        path: PathBuf,
    },
    /// The enrollment store holds the MDM gate's device records, which no table keeps yet.
    #[error(
        "{path} holds the MDM gate's device records, which the database has no table for yet: a coordinator that runs the MDM gate stays on the file store"
    )]
    MdmDevices {
        /// The directory of the records.
        path: PathBuf,
    },
    /// The database holds the enrollment store and no `--enrollment-dir` says where it goes.
    #[error(
        "the database holds the enrollment store and --enrollment-dir is not given: exporting without it would lose the enrolled hosts"
    )]
    EnrollmentDirNeeded,
}

impl From<DatabaseError> for TransferError {
    fn from(why: DatabaseError) -> TransferError {
        TransferError::Database(MoveError::Database(why))
    }
}

/// The error of a file at `path` that could not be read or written.
fn io_at(path: &Path) -> impl FnOnce(io::Error) -> BackupError + '_ {
    move |source| BackupError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// The records of the enrollment store in `dir`, read without writing: a directory that is not
/// there is refused (a store `serve` would make empty, so every host would enroll again), and so
/// is one that holds the MDM gate's device records.
fn read_records(dir: &Path) -> Result<Snapshot, TransferError> {
    if !fs::symlink_metadata(dir).is_ok_and(|meta| meta.is_dir()) {
        return Err(BackupError::NoEnrollmentDir {
            path: dir.to_path_buf(),
        }
        .into());
    }
    let devices = dir.join(DEVICES_DIR);
    let held = match fs::read_dir(&devices) {
        Ok(mut entries) => entries.next().is_some(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(e) => return Err(io_at(&devices)(e).into()),
    };
    if held {
        return Err(TransferError::MdmDevices { path: devices });
    }
    Ok(FileRecords::read_snapshot(Arc::new(RealFs), dir)?)
}

/// Everything the file store `paths` names holds, read by the strict readers `serve` uses and
/// without writing anything, the single-state desired file read as the state of `legacy` and
/// CI's spent ids past at `now` left out. The markers a stop left refuse it.
///
/// # Errors
/// [`TransferError`]: a marker is there, or a file is refused or cannot be read.
fn read_files(paths: &StatePaths, legacy: Option<&str>, now: u64) -> Result<Moved, TransferError> {
    let state = &paths.state_file;
    let ops = paths.ops_file();
    let unfinished = ops_file::unfinished_path(&ops);
    if Marker::read(&unfinished).is_some() {
        return Err(TransferError::Unsettled { path: unfinished });
    }
    let restoring = restore_marker(state);
    if restoring.exists() {
        return Err(StartError::RestoreUnfinished { path: restoring }.into());
    }
    let files = Files::default()
        .with_desired(state, legacy)
        .with_verdicts(&verdicts_path(state));
    let desired = files.load_desired().map_err(StartError::from)?;
    let parsed = match fs::read(&ops) {
        Ok(bytes) => ops::parse(&ops, &bytes).map_err(StartError::from)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => OpsState::default(),
        Err(e) => return Err(io_at(&ops)(e).into()),
    };
    let ops_state = ops_file::read_joined(&ops, parsed).map_err(StartError::from)?;
    let verdicts = files.load_verdicts().map_err(StartError::from)?;
    let spent = jti_file::read_live(&jti_path(state), now).map_err(StartError::from)?;
    let active = audit_file::path_of(&ops);
    let mut audit = Vec::new();
    AuditLog::open(active.clone(), DEFAULT_ROTATION)
        .and_then(|log| log.each(&mut |line| audit.push(line)))
        .map_err(StartError::from)?;
    let pruned_before = audit_file::pruned_before(&active).map_err(StartError::from)?;
    let records = match &paths.enrollment_dir {
        Some(dir) => read_records(dir)?,
        None => Snapshot::default(),
    };
    Ok(Moved {
        loaded: Loaded {
            desired,
            ops: ops_state,
            verdicts,
            spent,
        },
        audit,
        records,
        pruned_before,
    })
}

/// The lines that say how many rows of each table `counts` holds.
fn table_lines(counts: &[(&str, i64)]) -> Vec<String> {
    counts
        .iter()
        .map(|(table, rows)| format!("  {table}: {rows} rows"))
        .collect()
}

/// Imports the file store `flags.paths` names into the database of `flags.database_dsn_file`,
/// or with no `--apply` says what it would import; with `--apply`, under the lock of the state
/// files taken by `lock`. The import is made at the time `clock` says.
///
/// # Errors
/// [`TransferError`]: a file coordinator holds the lock, a marker is there, a file is refused,
/// or the database refused the import, holds state, or read back what the files do not hold.
pub(crate) fn import(
    flags: &Import,
    lock: &dyn StateLock,
    clock: &UnixClock,
) -> Result<Report, TransferError> {
    let paths = &flags.paths;
    let _held = flags
        .apply
        .then(|| backup::take(lock, &paths.state_file))
        .transpose()?;
    let now = clock();
    let moved = read_files(paths, flags.legacy_service.as_deref(), now)?;
    let dsn = Dsn::read(&flags.database_dsn_file).map_err(DatabaseError::from)?;
    let counts = transfer::counts_of(&moved)?;
    let mut lines = vec![format!(
        "hostkeeper-coordinator import: the state of {} {}",
        paths.state_file.display(),
        if flags.apply {
            "is imported into the database"
        } else {
            "would be imported into the database, nothing written"
        }
    )];
    lines.extend(table_lines(&counts));
    let unlogged = moved
        .loaded
        .ops
        .audit
        .iter()
        .filter(|entry| {
            moved
                .audit
                .binary_search_by_key(&entry.seq, |line| line.seq)
                .is_err()
        })
        .count();
    if unlogged > 0 {
        lines.push(format!(
            "{unlogged} entries of the ops file's audit ring are older than its audit file, which holds no line of them: they are not imported"
        ));
    }
    if !flags.apply {
        lines.push(match transfer::plan(&dsn)? {
            Some((table, rows)) => format!(
                "the database holds state ({rows} rows in {table}): import --apply refuses it; drop it and create it again first"
            ),
            None => "the database holds no state: import --apply writes the rows above".to_owned(),
        });
        return Ok(Report::of(lines));
    }
    let manifest = backup::manifest_of(paths)?;
    // The ring of the ops file may name changes older than its audit file, which has no line, and
    // so no time, for them: they are not imported, and no number they took is taken again.
    let ring = &moved.loaded.ops.audit;
    let newest = ring
        .iter()
        .map(|entry| entry.seq)
        .chain(moved.audit.last().map(|line| line.seq))
        .max()
        .unwrap_or(0);
    let imported = AuditLine {
        seq: newest + 1,
        at_unix: now,
        by: COORDINATOR.to_owned(),
        action: ACTION_IMPORTED.to_owned(),
        detail: Detail {
            reason: Some(format!(
                "from the file store of {}",
                paths.state_file.display()
            )),
            ..Detail::default()
        },
    };
    transfer::import(&dsn, &moved, &manifest, &imported, now)?;
    lines.push(format!(
        "imported in one transaction, read back and compared; the audit row {} is {ACTION_IMPORTED}; no file was changed",
        imported.seq
    ));
    Ok(Report::of(lines))
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
    if moved.records.is_empty() {
        return Ok(files);
    }
    let Some(dir) = &paths.enrollment_dir else {
        return Err(TransferError::EnrollmentDirNeeded);
    };
    for (path, bytes) in moved.records.files()? {
        files.push((dir.join(path), bytes));
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
