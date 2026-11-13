//! `hostkeeper-coordinator backup` and `restore`: a copy of the coordinator's durable state, and
//! putting one back into targets that hold no state (`docs/operations/coordinator-state.md`).
//!
//! The state is the files `serve` keeps, named by the same flags:
//!
//! - beside `--state-file`: the state file itself, the releases set with targets
//!   (`.releases`), the registry of package repositories (`.repositories`), the spent ids of CI's
//!   tokens (`.jti`), the verdicts (`.verdicts`), the users and API tokens (`.accounts`) and the
//!   lab's layout (`.layout`);
//! - beside the ops file (`--ops-file`, by default the state file's name with `.ops` added): the
//!   ops file itself, the audit file (`.audit`) and each of its sealed segments (`.audit.<n>`,
//!   named `ops-audit.<n>` in a snapshot, with the number it has), the halts file (`.halts`), a
//!   halts file staged by a change that had not finished (`.halts.next`, which the next start
//!   settles) and the marker of changes that were not yet written (`.halts.unfinished`, which
//!   refuses a start unless the ops file holds every change it lists);
//! - with `--enrollment-dir`, every file of the enrollment store: each file ending `.pb` in that
//!   directory and in the directories under it. Anything else there (the store's `lock`, a
//!   `.tmp` file a crash left, a CA key or a server certificate kept in the same directory) is
//!   not copied, and the backup names each such file it passed over but `lock`.
//!
//! The CA's key and certificate (`--ca-key-file`) are never in a snapshot: they are backed up
//! once, sealed and offline (`docs/operations/ca-backup.md`), and the backup says so.
//!
//! [`backup`] takes the lock of the state files (`<state-file>.lock`, [`crate::lock`]), the one
//! `serve` holds for as long as it runs, so a backup is taken of a stopped coordinator and no
//! file changes while it is copied; a coordinator holding the lock refuses the backup. Every
//! file is read whole and written into `<out>.partial` with a manifest, `manifest.json`, that
//! names each file of the snapshot with its size and SHA-256; the directory is then renamed to
//! `<out>`. An `<out>` or `<out>.partial` that is already there refuses the backup; a backup
//! that fails removes its partial directory. The backup names each file of the list above that
//! is not there.
//!
//! [`restore`] reads the manifest and every file of the snapshot, and refuses a snapshot whose
//! manifest names a file it does not hold, holds a file the manifest does not name, or holds a
//! file whose size or SHA-256 is not the manifest's, before it writes anything. It then takes the
//! same lock, refuses targets that hold state (any of the files above, a sealed segment of the
//! audit file among them, or an enrollment directory that is not empty), and writes each file
//! through `<file>.restoring`, linked into place with a hard link, which refuses a name that is
//! already there: no file is ever written over. Before
//! its first file it writes `<state-file>.restore-unfinished`, and it removes it after its last,
//! so `serve` refuses to start on a restore that stopped partway (killed, or the machine lost).
//! A write that fails removes what the restore had written, so the targets hold no state again.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use clap::Args;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::accounts::accounts_path;
use crate::audit_file;
use crate::jti_file::jti_path;
use crate::layout::layout_path;
use crate::lock::{LockError, StateLock, lock_path};
use crate::ops_file::{halts_path, staged_path, unfinished_path};
use crate::releases::releases_path;
use crate::repositories::repositories_path;
use crate::verdict_file::verdicts_path;

/// The name of the manifest in a snapshot.
pub const MANIFEST: &str = "manifest.json";

/// The schema of the manifest this build writes and restores.
pub const SCHEMA: u64 = 1;

/// The directory of a snapshot that holds the enrollment store.
const ENROLLMENT: &str = "enrollment";

/// What a backup is written into before it is renamed to its name.
const PARTIAL: &str = ".partial";

/// What a restored file is written into before it is linked into place.
const RESTORING: &str = ".restoring";

/// The store's own lock file, at the top of the enrollment directory: no state, never copied
/// and never named as passed over.
const STORE_LOCK: &str = "lock";

/// What is added to the state file's name for the marker of a restore under way.
const RESTORE_UNFINISHED: &str = ".restore-unfinished";

/// The marker of a restore into the state file `state` that has not finished: written before
/// the restore's first file and removed after its last, so `serve` refuses to start on the
/// files of a restore that stopped partway ([`crate::config::build`]).
#[must_use]
pub fn restore_marker(state: &Path) -> PathBuf {
    with_suffix(state, RESTORE_UNFINISHED)
}

/// What is added to the state file's name for the marker of a `host-groups migrate --apply` under
/// way, a verb of release 0.1.253 that this release does not have.
const MIGRATE_UNFINISHED: &str = ".migrate-unfinished";

/// The marker of a `host-groups migrate --apply` of release 0.1.253 on the state file `state` that
/// has not finished: that release wrote it before its first file and removed it after its last.
/// This release writes none, and still refuses the files of one that stopped partway: `serve` and
/// `backup` refuse them, and `restore` and `export` take the marker for state in their targets.
#[must_use]
pub fn migrate_marker(state: &Path) -> PathBuf {
    with_suffix(state, MIGRATE_UNFINISHED)
}

/// The name in a snapshot of the marker of changes that were not yet written.
const UNFINISHED: &str = "ops-halts-unfinished";

/// The name in a snapshot of the audit file; its sealed segment `n` is `ops-audit.<n>`.
const AUDIT: &str = "ops-audit";

/// Where the coordinator's state is: the flags `serve` names it by.
#[derive(Clone, Debug, Args)]
pub struct StatePaths {
    /// The desired-state file, as `serve --state-file` names it. The releases
    /// (`<state-file>.releases`), the registry of package repositories
    /// (`<state-file>.repositories`), the spent ids of CI's tokens (`<state-file>.jti`), the
    /// verdicts (`<state-file>.verdicts`), the users and API tokens (`<state-file>.accounts`) and
    /// the lab layout (`<state-file>.layout`) are beside it, and the lock taken is
    /// `<state-file>.lock`, the one `serve` holds.
    #[arg(long)]
    pub state_file: PathBuf,
    /// The ops file, as `serve --ops-file` names it. Default: the state file's name with `.ops`
    /// added. The audit file with its sealed segments, the halts file and the halts file's
    /// staged copy and marker are beside it.
    #[arg(long)]
    pub ops_file: Option<PathBuf>,
    /// The enrollment store, as `serve --enrollment-dir` names it, when the coordinator is the
    /// hosts' CA.
    #[arg(long)]
    pub enrollment_dir: Option<PathBuf>,
}

impl StatePaths {
    /// The ops file: `--ops-file`, or the state file's name with `.ops` added.
    #[must_use]
    pub fn ops_file(&self) -> PathBuf {
        ops_path(&self.state_file, self.ops_file.as_deref())
    }

    /// Each file beside the state file and the ops file, with its name in a snapshot: the
    /// paths are the ones `serve` opens, from the same functions.
    fn beside(&self) -> Vec<(&'static str, PathBuf)> {
        let state = &self.state_file;
        let ops = self.ops_file();
        vec![
            ("state", state.clone()),
            ("state-releases", releases_path(state)),
            ("state-repositories", repositories_path(state)),
            ("state-jti", jti_path(state)),
            ("state-verdicts", verdicts_path(state)),
            ("state-accounts", accounts_path(state)),
            ("state-layout", layout_path(state)),
            ("ops", ops.clone()),
            (AUDIT, audit_file::path_of(&ops)),
            ("ops-halts", halts_path(&ops)),
            ("ops-halts-next", staged_path(&ops)),
            (UNFINISHED, unfinished_path(&ops)),
        ]
    }

    /// Each sealed segment of the audit file, oldest first, with its name in a snapshot
    /// (`ops-audit.<n>`, `n` its number): the segments `serve` reads, listed as it lists them.
    fn segments(&self) -> Result<Vec<(String, PathBuf)>, BackupError> {
        let audit = audit_file::path_of(&self.ops_file());
        let listed = audit_file::segments(&audit).map_err(io_at(parent_of(&audit)))?;
        Ok(listed
            .into_iter()
            .map(|(number, path)| (format!("{AUDIT}.{number}"), path))
            .collect())
    }

    /// Where the file named `name` in a snapshot goes when it is a sealed segment of the audit
    /// file: the segment of the same number beside the audit file.
    fn segment_target(&self, name: &str) -> Option<PathBuf> {
        let number = audit_file::segment_number(OsStr::new(AUDIT), OsStr::new(name))?;
        let audit = audit_file::path_of(&self.ops_file());
        Some(audit_file::segment_path(&audit, number))
    }
}

/// The ops file of the state file `state`: `ops` when given, else the state file's name with
/// `.ops` added.
#[must_use]
pub fn ops_path(state: &Path, ops: Option<&Path>) -> PathBuf {
    ops.map_or_else(|| with_suffix(state, ".ops"), Path::to_path_buf)
}

/// The flags of `backup`.
#[derive(Clone, Debug, Args)]
pub struct Backup {
    /// Where the coordinator's state is.
    #[command(flatten)]
    pub paths: StatePaths,
    /// The directory the snapshot is written to. It must not exist: a backup never replaces
    /// one. The snapshot is written to `<out>.partial` first and renamed to it once whole.
    #[arg(long)]
    pub out: PathBuf,
}

/// The flags of `restore`.
#[derive(Clone, Debug, Args)]
pub struct Restore {
    /// The snapshot: a directory `backup` wrote.
    #[arg(long)]
    pub from: PathBuf,
    /// Where the coordinator's state goes: the flags `serve` will be started with.
    #[command(flatten)]
    pub paths: StatePaths,
    /// Restore into targets that hold no state. Required: the restore writes only where none
    /// of the state files is and the enrollment directory is missing or empty, and refuses
    /// anything else; it never writes over a file.
    #[arg(long, required = true)]
    pub into_empty: bool,
}

/// Why a backup or a restore was refused, or failed.
#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    /// A coordinator holds the lock of the state files.
    #[error(
        "{path}: a coordinator holds the lock of these state files; stop it first, so that no file changes while it is copied"
    )]
    Locked {
        /// The lock file.
        path: PathBuf,
    },
    /// The lock of the state files could not be taken.
    #[error("{path}: the lock of the state files could not be taken: {source}")]
    Lock {
        /// The lock file.
        path: PathBuf,
        /// Why.
        source: io::Error,
    },
    /// A file or directory could not be read or written.
    #[error("{path}: {source}")]
    Io {
        /// The file or directory.
        path: PathBuf,
        /// Why.
        source: io::Error,
    },
    /// The backup's directory, or its partial one, is already there.
    #[error("{path} is already there; a backup never replaces one: name another --out")]
    Exists {
        /// The directory.
        path: PathBuf,
    },
    /// None of the state files is there.
    #[error(
        "{path}: none of the coordinator's state files is there; check --state-file and --ops-file"
    )]
    NothingThere {
        /// The state file.
        path: PathBuf,
    },
    /// `--enrollment-dir` names no directory.
    #[error("--enrollment-dir: {path} is no directory")]
    NoEnrollmentDir {
        /// The flag's path.
        path: PathBuf,
    },
    /// The manifest is not one this build reads.
    #[error("{path}: the manifest cannot be read: {source}")]
    Manifest {
        /// The manifest.
        path: PathBuf,
        /// Why.
        source: serde_json::Error,
    },
    /// The manifest is of another schema.
    #[error("{path}: the manifest is of schema {schema}; this build restores schema {SCHEMA}")]
    Schema {
        /// The manifest.
        path: PathBuf,
        /// Its schema.
        schema: u64,
    },
    /// The manifest names a file no snapshot holds.
    #[error("the manifest names {name}, which is no file of a snapshot")]
    Name {
        /// The name.
        name: String,
    },
    /// The manifest names a file twice.
    #[error("the manifest names {name} twice")]
    Twice {
        /// The name.
        name: String,
    },
    /// The snapshot holds something its manifest does not name.
    #[error(
        "{path}: the snapshot holds this, and its manifest does not name it; nothing was restored"
    )]
    Unlisted {
        /// What it holds.
        path: PathBuf,
    },
    /// A file of the snapshot is not what the manifest says.
    #[error(
        "{path}: {bytes} bytes with SHA-256 {sha256}, where the manifest says {want_bytes} bytes with SHA-256 {want_sha256}; the snapshot changed after the backup, so nothing was restored"
    )]
    Mismatch {
        /// The file.
        path: PathBuf,
        /// Its size.
        bytes: u64,
        /// Its SHA-256.
        sha256: String,
        /// The size the manifest says.
        want_bytes: u64,
        /// The SHA-256 the manifest says.
        want_sha256: String,
    },
    /// The snapshot holds the enrollment store and no `--enrollment-dir` says where it goes.
    #[error(
        "the snapshot holds the enrollment store and --enrollment-dir is not given: restoring without it would lose the enrolled hosts"
    )]
    EnrollmentDirNeeded,
    /// A `host-groups migrate --apply` of release 0.1.253, a verb this release does not have, on
    /// these state files stopped before it finished.
    #[error(
        "{path}: a host-groups migrate --apply of release 0.1.253 stopped partway; restore the backup taken before it"
    )]
    MigrateUnfinished {
        /// The marker file.
        path: PathBuf,
    },
    /// A target holds state.
    #[error(
        "{path} is already there; restore writes only into targets that hold no state, never over a file"
    )]
    NotEmpty {
        /// The target.
        path: PathBuf,
    },
    /// A write of the restore failed, after it had begun writing.
    #[error("{path}: {source}; {}", left_text(left))]
    Unfinished {
        /// What could not be written.
        path: PathBuf,
        /// Why.
        source: io::Error,
        /// What the restore wrote and could not remove again.
        left: Vec<PathBuf>,
    },
}

/// What [`BackupError::Unfinished`] says of what a failed restore left.
fn left_text(left: &[PathBuf]) -> String {
    if left.is_empty() {
        return "the restore removed what it had written".to_owned();
    }
    let names: Vec<String> = left.iter().map(|p| p.display().to_string()).collect();
    format!(
        "the restore could not remove what it had written of: {}; remove those before restoring again",
        names.join(", ")
    )
}

/// What a backup or a restore did, a line each, for the operator.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report(Vec<String>);

impl Report {
    /// The report of `lines`.
    pub(crate) fn of(lines: Vec<String>) -> Report {
        Report(lines)
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for line in &self.0 {
            writeln!(f, "{line}")?;
        }
        Ok(())
    }
}

/// The manifest of a snapshot.
#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    /// [`SCHEMA`].
    schema: u64,
    /// The coordinator that wrote it, and its version.
    made_by: String,
    /// Every file of the snapshot but the manifest.
    files: Vec<Entry>,
}

/// One file of a snapshot.
#[derive(Debug, Serialize, Deserialize)]
struct Entry {
    /// Its path in the snapshot, `/` between the names.
    name: String,
    /// Its size.
    bytes: u64,
    /// Its SHA-256, lowercase hex.
    sha256: String,
}

/// The note every backup ends with.
const NO_CA_KEY: &str = "not in this backup: the CA's key and certificate (--ca-key-file); back them up once, sealed and offline, as docs/operations/ca-backup.md says";

/// Takes a snapshot of the state `flags.paths` names into `flags.out`, under the lock of the
/// state files taken by `lock`.
///
/// # Errors
/// [`BackupError`]: `--out` or `<out>.partial` is there, a coordinator holds the lock, none of
/// the state files is there (nor the state file's directory), `--enrollment-dir` is no
/// directory, or a file could not be read or written (the partial directory is then removed).
pub fn backup(flags: &Backup, lock: &dyn StateLock) -> Result<Report, BackupError> {
    let partial = with_suffix(&flags.out, PARTIAL);
    for path in [&flags.out, &partial] {
        if there(path)? {
            return Err(BackupError::Exists { path: path.clone() });
        }
    }
    if !there(parent_of(&flags.paths.state_file))? {
        return Err(BackupError::NothingThere {
            path: flags.paths.state_file.clone(),
        });
    }
    let _held = take(lock, &flags.paths.state_file)?;
    refuse_migrating(&flags.paths.state_file)?;
    let mut sources = Vec::new();
    let mut absent = Vec::new();
    for (name, path) in flags.paths.beside() {
        if there(&path)? {
            sources.push((name.to_owned(), path));
        } else {
            absent.push(path);
        }
    }
    sources.extend(flags.paths.segments()?);
    if sources.is_empty() {
        return Err(BackupError::NothingThere {
            path: flags.paths.state_file.clone(),
        });
    }
    let mut passed = Vec::new();
    if let Some(dir) = &flags.paths.enrollment_dir {
        if !fs::symlink_metadata(dir).is_ok_and(|meta| meta.is_dir()) {
            return Err(BackupError::NoEnrollmentDir { path: dir.clone() });
        }
        store_files(dir, ENROLLMENT, &mut sources, &mut passed)?;
    }
    // A partial directory made since the check above is another's: this backup stops, and
    // removes nothing it did not make.
    DirBuilder::new()
        .mode(0o700)
        .create(&partial)
        .map_err(io_at(&partial))?;
    // From here `<out>.partial` is this backup's own: it made it, and a failure removes it.
    let written = write_snapshot(&partial, &sources).and_then(|entries| {
        fs::rename(&partial, &flags.out).map_err(io_at(&flags.out))?;
        Ok(entries)
    });
    let entries = match written {
        Ok(entries) => entries,
        Err(why) => {
            let _ = fs::remove_dir_all(&partial);
            return Err(why);
        }
    };
    sync_dir(parent_of(&flags.out))?;
    let mut lines = vec![format!(
        "hostkeeper-coordinator backup: {} ({} files, each with its SHA-256 in {MANIFEST})",
        flags.out.display(),
        entries.len()
    )];
    for ((_, source), entry) in sources.iter().zip(&entries) {
        lines.push(format!(
            "  {} {} bytes sha256 {}",
            source.display(),
            entry.bytes,
            entry.sha256
        ));
    }
    for path in absent {
        lines.push(format!(
            "not there, so not in this backup: {}",
            path.display()
        ));
    }
    for path in passed {
        lines.push(format!(
            "not copied, no file of the enrollment store: {}",
            path.display()
        ));
    }
    lines.push(NO_CA_KEY.to_owned());
    Ok(Report(lines))
}

/// Restores the snapshot `flags.from` into the targets `flags.paths` names, under the lock of
/// the state files taken by `lock`.
///
/// # Errors
/// [`BackupError`]: the manifest is unreadable or names what no snapshot holds, the snapshot
/// holds a file the manifest does not name or one that is not what it says, it holds the
/// enrollment store and `--enrollment-dir` is not given (all before anything is written), a
/// coordinator holds the lock, a target holds state, or a write failed (what the restore wrote
/// is then removed).
pub fn restore(flags: &Restore, lock: &dyn StateLock) -> Result<Report, BackupError> {
    let manifest_path = flags.from.join(MANIFEST);
    let text = fs::read(&manifest_path).map_err(io_at(&manifest_path))?;
    let value: serde_json::Value =
        serde_json::from_slice(&text).map_err(json_at(&manifest_path))?;
    // The schema is read before the shape, so a later schema is named as one, not as garbage.
    let schema = value.get("schema").and_then(serde_json::Value::as_u64);
    if let Some(schema) = schema.filter(|schema| *schema != SCHEMA) {
        return Err(BackupError::Schema {
            path: manifest_path,
            schema,
        });
    }
    let manifest: Manifest = serde_json::from_value(value).map_err(json_at(&manifest_path))?;
    let targets = targets_of(&flags.paths, &manifest)?;
    refuse_unlisted(&flags.from, &manifest)?;
    let mut files = Vec::new();
    for (entry, target) in manifest.files.iter().zip(targets) {
        files.push((target, verified(&flags.from, entry)?));
    }
    write_targets(&flags.paths, lock, &files)?;
    let mut lines = vec![format!(
        "hostkeeper-coordinator restore: {} files from {}, each checked against its SHA-256 in {MANIFEST}",
        files.len(),
        flags.from.display()
    )];
    for (target, bytes) in &files {
        lines.push(format!("  {} {} bytes", target.display(), bytes.len()));
    }
    if manifest.files.iter().any(|entry| entry.name == UNFINISHED) {
        lines.push(
            "the snapshot holds the marker of changes that were not yet written: serve refuses to start until they are settled by hand and the marker removed, unless the ops file holds every change it lists, as it did before the backup".to_owned(),
        );
    }
    Ok(Report(lines))
}

/// Makes the state file's directory (it holds the lock's file, and stays), takes the lock,
/// refuses targets that hold state, and writes `files`, each to its target. A write that fails
/// removes, still under the lock, every file and directory the writes had made.
pub(crate) fn write_targets(
    paths: &StatePaths,
    lock: &dyn StateLock,
    files: &[(PathBuf, Vec<u8>)],
) -> Result<(), BackupError> {
    let state_dir = parent_of(&paths.state_file);
    Made::default().dirs(state_dir).map_err(io_at(state_dir))?;
    let _held = take(lock, &paths.state_file)?;
    refuse_state(paths)?;
    let mut made = Made::default();
    if let Err((path, source)) = put_all(&mut made, &restore_marker(&paths.state_file), files) {
        return Err(BackupError::Unfinished {
            path,
            source,
            left: made.undo(),
        });
    }
    Ok(())
}

/// Writes the restore's marker, then each of `files` to its target, then removes the marker,
/// recording in `made` what it made. What could not be written, and why.
fn put_all(
    made: &mut Made,
    marker: &Path,
    files: &[(PathBuf, Vec<u8>)],
) -> Result<(), (PathBuf, io::Error)> {
    let at = |path: &Path| {
        let path = path.to_path_buf();
        move |source| (path, source)
    };
    made.put(marker, b"").map_err(at(marker))?;
    for (target, bytes) in files {
        made.put(target, bytes).map_err(at(target))?;
    }
    fs::remove_file(marker)
        .and_then(|()| sync_parent(marker))
        .map_err(at(marker))
}

/// The lock of the state file `state`'s state files, taken by `lock`.
pub(crate) fn take(
    lock: &dyn StateLock,
    state: &Path,
) -> Result<crate::lock::LockGuard, BackupError> {
    let path = lock_path(state);
    lock.acquire(&path).map_err(|why| match why {
        LockError::Held => BackupError::Locked { path: path.clone() },
        LockError::Io(source) => BackupError::Lock {
            path: path.clone(),
            source,
        },
    })
}

/// Adds, to `sources`, every file of the enrollment store in `dir` with its name in a snapshot
/// (`prefix`, `/`, its path under the store), and to `passed` everything else there but the
/// store's lock. A symbolic link is never followed.
fn store_files(
    dir: &Path,
    prefix: &str,
    sources: &mut Vec<(String, PathBuf)>,
    passed: &mut Vec<PathBuf>,
) -> Result<(), BackupError> {
    for (name, path) in listed(dir)? {
        let meta = fs::symlink_metadata(&path).map_err(io_at(&path))?;
        let inner = format!("{prefix}/{name}");
        if meta.is_dir() {
            store_files(&path, &inner, sources, passed)?;
        } else if meta.is_file() && name.ends_with(".pb") {
            sources.push((inner, path));
        } else if !(prefix == ENROLLMENT && name == STORE_LOCK) {
            passed.push(path);
        }
    }
    Ok(())
}

/// The entries of `dir`, by name, sorted. A name that is not UTF-8 is kept as the lossy text
/// of it, so it never ends `.pb` by accident and is passed over by name.
fn listed(dir: &Path) -> Result<Vec<(String, PathBuf)>, BackupError> {
    let mut names = Vec::new();
    for entry in fs::read_dir(dir).map_err(io_at(dir))? {
        let entry = entry.map_err(io_at(dir))?;
        let name = entry.file_name();
        let text = name.to_str().map_or_else(
            || format!("{}\u{fffd}", name.to_string_lossy()),
            str::to_owned,
        );
        names.push((text, entry.path()));
    }
    names.sort();
    Ok(names)
}

/// Writes each of `sources` into the directory `partial`, just made, under its name, and the
/// manifest beside them; every file and directory synced. The entries of the manifest, in the
/// order of `sources`.
fn write_snapshot(
    partial: &Path,
    sources: &[(String, PathBuf)],
) -> Result<Vec<Entry>, BackupError> {
    let mut dirs = BTreeSet::from([partial.to_path_buf()]);
    let mut entries = Vec::new();
    for (name, source) in sources {
        let bytes = fs::read(source).map_err(io_at(source))?;
        let dest = partial.join(name);
        let parent = parent_of(&dest).to_path_buf();
        DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(&parent)
            .map_err(io_at(&parent))?;
        dirs.insert(parent);
        write_new(&dest, &bytes).map_err(io_at(&dest))?;
        entries.push(Entry {
            name: name.clone(),
            bytes: bytes.len() as u64,
            sha256: sha256_hex(&bytes),
        });
    }
    let manifest = Manifest {
        schema: SCHEMA,
        made_by: format!("hostkeeper-coordinator {}", env!("CARGO_PKG_VERSION")),
        files: entries,
    };
    let path = partial.join(MANIFEST);
    let mut text = serde_json::to_vec_pretty(&manifest).map_err(json_at(&path))?;
    text.push(b'\n');
    write_new(&path, &text).map_err(io_at(&path))?;
    for dir in dirs.iter().rev() {
        sync_dir(dir)?;
    }
    Ok(manifest.files)
}

/// Where each file the manifest names goes, in its order.
fn targets_of(paths: &StatePaths, manifest: &Manifest) -> Result<Vec<PathBuf>, BackupError> {
    let beside = paths.beside();
    let mut seen = BTreeSet::new();
    let mut targets = Vec::new();
    for entry in &manifest.files {
        if !seen.insert(entry.name.as_str()) {
            return Err(BackupError::Twice {
                name: entry.name.clone(),
            });
        }
        let named = || BackupError::Name {
            name: entry.name.clone(),
        };
        let found = beside.iter().find(|(name, _)| *name == entry.name);
        let target = match found
            .map(|(_, path)| path.clone())
            .or_else(|| paths.segment_target(&entry.name))
        {
            Some(path) => path,
            None => {
                let inner = entry
                    .name
                    .strip_prefix(ENROLLMENT)
                    .and_then(|rest| rest.strip_prefix('/'))
                    .filter(|rest| store_name(rest))
                    .ok_or_else(named)?;
                let dir = paths
                    .enrollment_dir
                    .as_ref()
                    .ok_or(BackupError::EnrollmentDirNeeded)?;
                dir.join(inner)
            }
        };
        targets.push(target);
    }
    Ok(targets)
}

/// Whether `inner` is the path of a file of the enrollment store under its directory: plain
/// names between `/`, the last ending `.pb`.
fn store_name(inner: &str) -> bool {
    inner.ends_with(".pb")
        && inner
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// Refuses a snapshot that holds anything but its manifest and the files the manifest names.
fn refuse_unlisted(from: &Path, manifest: &Manifest) -> Result<(), BackupError> {
    let named: BTreeSet<&str> = manifest.files.iter().map(|e| e.name.as_str()).collect();
    let mut held = Vec::new();
    snapshot_files(from, "", &mut held)?;
    match held
        .into_iter()
        .find(|(name, _)| name != MANIFEST && !named.contains(name.as_str()))
    {
        Some((_, path)) => Err(BackupError::Unlisted { path }),
        None => Ok(()),
    }
}

/// Adds, to `held`, every file under `dir` with its name in the snapshot (`prefix` and its
/// path, `/` between); a directory is gone into, a symbolic link is not followed but held.
fn snapshot_files(
    dir: &Path,
    prefix: &str,
    held: &mut Vec<(String, PathBuf)>,
) -> Result<(), BackupError> {
    for (name, path) in listed(dir)? {
        let inner = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        if fs::symlink_metadata(&path).map_err(io_at(&path))?.is_dir() {
            snapshot_files(&path, &inner, held)?;
        } else {
            held.push((inner, path));
        }
    }
    Ok(())
}

/// The bytes of the file `entry` names in the snapshot `from`, when they are the size and the
/// SHA-256 the manifest says. A symbolic link is refused, not followed.
fn verified(from: &Path, entry: &Entry) -> Result<Vec<u8>, BackupError> {
    let path = from.join(&entry.name);
    let bytes = read_no_follow(&path).map_err(io_at(&path))?;
    let sha256 = sha256_hex(&bytes);
    let size = bytes.len() as u64;
    if size != entry.bytes || sha256 != entry.sha256 {
        return Err(BackupError::Mismatch {
            path,
            bytes: size,
            sha256,
            want_bytes: entry.bytes,
            want_sha256: entry.sha256.clone(),
        });
    }
    Ok(bytes)
}

/// The bytes of `path`, opened without following a symbolic link.
fn read_no_follow(path: &Path) -> io::Result<Vec<u8>> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let mut bytes = Vec::new();
    io::Read::read_to_end(&mut file, &mut bytes)?;
    Ok(bytes)
}

/// Refuses targets that hold state: a file beside the state file or the ops file, a sealed
/// segment of the audit file, or an enrollment directory with anything in it.
fn refuse_state(paths: &StatePaths) -> Result<(), BackupError> {
    let markers = [
        ("restore marker", restore_marker(&paths.state_file)),
        ("migrate marker", migrate_marker(&paths.state_file)),
    ];
    for (_, path) in paths.beside().into_iter().chain(markers) {
        if there(&path)? {
            return Err(BackupError::NotEmpty { path });
        }
    }
    if let Some((_, path)) = paths.segments()?.into_iter().next() {
        return Err(BackupError::NotEmpty { path });
    }
    if let Some(dir) = paths.enrollment_dir.as_ref().filter(|dir| dir.exists())
        && let Some((_, inside)) = listed(dir)?.into_iter().next()
    {
        return Err(BackupError::NotEmpty { path: inside });
    }
    Ok(())
}

/// What a restore has made, so that a failed one removes it again.
#[derive(Default)]
struct Made {
    /// The directories it made, in the order it made them.
    dirs: Vec<PathBuf>,
    /// The files it wrote, in order.
    files: Vec<PathBuf>,
}

impl Made {
    /// Makes `dir` and its missing parents, mode 0700, remembering each one made.
    fn dirs(&mut self, dir: &Path) -> io::Result<()> {
        if dir.exists() {
            return Ok(());
        }
        self.dirs(parent_of(dir))?;
        DirBuilder::new().mode(0o700).create(dir)?;
        self.dirs.push(dir.to_path_buf());
        Ok(())
    }

    /// Writes `bytes` to `target` through `<target>.restoring`, linked into place: a name
    /// already there is refused, never written over.
    fn put(&mut self, target: &Path, bytes: &[u8]) -> io::Result<()> {
        let parent = parent_of(target);
        self.dirs(parent)?;
        let temp = with_suffix(target, RESTORING);
        let mut file = open_new(&temp)?;
        // Made by this restore from here, so a failure from here on removes it.
        self.files.push(temp.clone());
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::hard_link(&temp, target)?;
        self.files.push(target.to_path_buf());
        fs::remove_file(&temp)?;
        self.files.retain(|path| *path != temp);
        sync_parent(target)
    }

    /// Removes what was made, newest first: what could not be removed.
    fn undo(&mut self) -> Vec<PathBuf> {
        let files = self.files.drain(..).rev();
        let dirs = self.dirs.drain(..).rev();
        let mut left: Vec<PathBuf> = files
            .filter(|path| {
                fs::remove_file(path).is_err_and(|e| e.kind() != io::ErrorKind::NotFound)
            })
            .collect();
        left.extend(dirs.filter(|dir| fs::remove_dir(dir).is_err()));
        left
    }
}

/// Creates `path`, which must not be there, mode 0600, with `bytes`, synced.
fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = open_new(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Creates `path`, which must not be there, mode 0600, open for writing.
fn open_new(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

/// Flushes the directory `path` is in.
fn sync_parent(path: &Path) -> io::Result<()> {
    File::open(parent_of(path))?.sync_all()
}

/// Flushes the directory `dir`, so the names made in it are durable.
fn sync_dir(dir: &Path) -> Result<(), BackupError> {
    File::open(dir)
        .and_then(|file| file.sync_all())
        .map_err(io_at(dir))
}

/// Refuses the state file `state` while the marker of a `host-groups migrate --apply` of release
/// 0.1.253 that did not finish stands beside it.
///
/// # Errors
/// [`BackupError::MigrateUnfinished`]: the marker is there.
pub(crate) fn refuse_migrating(state: &Path) -> Result<(), BackupError> {
    let path = migrate_marker(state);
    if there(&path)? {
        return Err(BackupError::MigrateUnfinished { path });
    }
    Ok(())
}

/// Whether something (a file, a directory, a link) is at `path`.
fn there(path: &Path) -> Result<bool, BackupError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(BackupError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// The directory of `path`: its parent, or `.` for a bare name.
pub(crate) fn parent_of(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

/// `path` with `suffix` added to its name.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// The SHA-256 of `bytes`, lowercase hex.
fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut text, b| {
            let _ = write!(text, "{b:02x}");
            text
        })
}

/// The error of a manifest at `path` that could not be read or written.
fn json_at(path: &Path) -> impl FnOnce(serde_json::Error) -> BackupError + '_ {
    move |source| BackupError::Manifest {
        path: path.to_path_buf(),
        source,
    }
}

/// The error of an I/O failure at `path`.
fn io_at(path: &Path) -> impl FnOnce(io::Error) -> BackupError + '_ {
    move |source| BackupError::Io {
        path: path.to_path_buf(),
        source,
    }
}
