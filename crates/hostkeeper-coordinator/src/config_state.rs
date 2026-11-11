//! The start's store ([`super`]): the files of `--state-file`, or the database of
//! `--database-dsn-file` ([`open_store`]).
//!
//! For the files: the checks of what a stop left beside them (the marker of changes that were in
//! memory and not yet written, [`crate::ops::Marker`], and the marker of a restore that did not
//! finish, [`crate::backup::restore_marker`]), the files opened as one backend ([`Files`]) under
//! the three stores and CI's spent ids, and the refusal of a part the backend could not load, as
//! that part's store has always said it.
//!
//! For the database (`docs/design/coordinator-storage.md`, sections 4.3, 5 and 8): the flags of
//! the file store refused beside it, the connection string read from its secret file, the store
//! opened (the lock, the migrations, the epoch, the load), and the marker of `--halt-marker-file`
//! settled against the `audit` table: removed when every change it lists has its audit row, a
//! refusal to start naming those that do not.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_enrollment::enroll::records::Records;
use hostkeeper_mdm_gate::DeviceStore;
use hostkeeper_oidc::Clock;

use super::{Serve, StartError, take_lock};
use crate::accounts::accounts_path;
use crate::audit_file::{DEFAULT_MAX_BYTES, DEFAULT_RETAINED, Rotation, wall_clock};
use crate::backup::{migrate_marker, ops_path, restore_marker};
use crate::initial_admin::InitialAdmin;
use crate::jti_file::jti_path;
use crate::lock::{LockGuard, StateLock, lock_path};
use crate::ops::{Marker, OpsState, unlanded};
use crate::service::Coordinator;
use crate::store::postgres::{DatabaseError, Dsn, PgDevices, PgRecords, Postgres};
use crate::store::{Backend, BackendError, Files, Loaded, Shared};
use crate::tokens::OperatorTokens;
use crate::verdict_file::verdicts_path;

/// The store a start opened: the backend under the three stores and CI's spent ids, what it
/// loaded, the marker of changes not yet committed, what holds the store for this process, and
/// the enrollment records and the MDM gate's device records when the store keeps them (the
/// file store keeps both in `--enrollment-dir`, opened there).
pub(super) struct Opened {
    pub(super) backend: Shared,
    pub(super) loaded: Loaded,
    pub(super) marker: PathBuf,
    pub(super) held: LockGuard,
    pub(super) records: Option<Arc<dyn Records>>,
    pub(super) devices: Option<Box<dyn DeviceStore>>,
}

/// Opens the store `flags` name: the database of `--database-dsn-file`, or the files of
/// `--state-file` under the lock `lock` takes, the single-state file read as the state of
/// `legacy`; CI's spent ids judged past by `oidc_clock`.
///
/// # Errors
/// [`StartError`]: a flag of the file store beside the database, the lock, a marker, or a file or
/// the database refused.
pub(super) fn open_store(
    flags: &Serve,
    legacy: Option<&str>,
    lock: &dyn StateLock,
    oidc_clock: &Clock,
) -> Result<Opened, StartError> {
    let (Some(dsn), Some(marker)) = (&flags.database_dsn_file, &flags.halt_marker_file) else {
        // clap requires one of --state-file and --database-dsn-file, and --halt-marker-file
        // with the latter.
        let state = flags.state_file.clone().unwrap_or_default();
        // The lock before any state file: none is opened (an open settles a staged halts file,
        // which is a write) before this process is the one coordinator on them.
        let held = take_lock(lock, &lock_path(&state))?;
        let ops = ops_path(&state, flags.ops_file.as_deref());
        let (files, loaded) = open(flags, &state, legacy, &ops, oidc_clock)?;
        return Ok(Opened {
            backend: Arc::new(files),
            loaded,
            marker: crate::ops_file::unfinished_path(&ops),
            held,
            records: None,
            devices: None,
        });
    };
    let file_flags = [
        ("--state-file", flags.state_file.is_some()),
        ("--ops-file", flags.ops_file.is_some()),
        ("--enrollment-dir", flags.enrollment_dir.is_some()),
        ("--audit-max-bytes", flags.audit_max_bytes.is_some()),
        (
            "--audit-retained-segments",
            flags.audit_retained_segments.is_some(),
        ),
    ];
    if let Some((flag, _)) = file_flags.iter().find(|(_, given)| *given) {
        return Err(StartError::DatabaseAndFileFlag(flag));
    }
    // A marker this build cannot read is refused before the database is touched.
    let listed = match Marker::read(marker) {
        None => None,
        Some(Marker::Listed(numbers)) => Some(numbers),
        Some(Marker::Unlisted) => {
            return Err(StartError::HaltWriteUnfinished {
                path: marker.clone(),
            });
        }
    };
    let dsn = Dsn::read(dsn).map_err(DatabaseError::from)?;
    let holder = flags
        .server_names
        .first()
        .map_or("coordinator", String::as_str);
    let (store, loaded) = Postgres::open(&dsn, holder, &wall_clock(), Arc::clone(oidc_clock))?;
    if let Some(numbers) = listed {
        settle_in_database(&store, marker, &numbers)?;
    }
    let records: Option<Arc<dyn Records>> = match flags.ca_key_file {
        Some(_) => Some(Arc::new(
            PgRecords::over(&store).map_err(|e| DatabaseError::Load(e.to_string()))?,
        )),
        None => None,
    };
    // The gate's device records are the table `mdm_devices` when the gate runs.
    let devices: Option<Box<dyn DeviceStore>> = match flags.mdm.mdm_api_url {
        Some(_) => Some(Box::new(PgDevices::over(&store))),
        None => None,
    };
    Ok(Opened {
        backend: Arc::new(store.clone()),
        loaded,
        marker: marker.clone(),
        held: LockGuard::new(store),
        records,
        devices,
    })
}

/// Settles the marker at `marker`, which lists the audit `numbers`, against the `audit` table of
/// `store` (section 4.3): every change a memory-first commit holds has its audit row in the same
/// transaction, so a number in the table is a change that landed. A marker whose every number is
/// there is logged and removed; one with a number that is not refuses the start, naming it. A
/// marker of another epoch than the one the database held before this start refuses it naming
/// every number: another coordinator led since, and a number in the table may be its change.
///
/// # Errors
/// [`StartError::ChangesUnwritten`]: a change it lists did not land; [`StartError::Database`]:
/// the table could not be read; [`StartError::Io`]: the marker could not be removed.
fn settle_in_database(store: &Postgres, marker: &Path, numbers: &[u64]) -> Result<(), StartError> {
    // The epoch the database was at before this start raised it. A marker written at another one
    // is a coordinator's that did not lead last: another led since, and its audit rows can carry
    // the same numbers, so none of the marker's is taken as landed.
    let before = store.epoch().unwrap_or_default().saturating_sub(1);
    if Marker::epoch_at(marker).is_some_and(|epoch| epoch != before) {
        return Err(StartError::ChangesUnwritten {
            path: marker.to_owned(),
            numbers: numbers.to_vec(),
        });
    }
    let wanted: Vec<i64> = numbers
        .iter()
        .map(|n| i64::try_from(*n).unwrap_or(i64::MAX))
        .collect();
    let held: Vec<i64> = store
        .read(move |session| {
            Ok(session
                .query("SELECT seq FROM audit WHERE seq = ANY($1)", &[&wanted])?
                .iter()
                .map(|row| row.get(0))
                .collect())
        })
        .map_err(DatabaseError::Load)?;
    let missing: Vec<u64> = numbers
        .iter()
        .copied()
        .filter(|n| !held.iter().any(|h| u64::try_from(*h).ok() == Some(*n)))
        .collect();
    if !missing.is_empty() {
        return Err(StartError::ChangesUnwritten {
            path: marker.to_owned(),
            numbers: missing,
        });
    }
    std::fs::remove_file(marker)?;
    tracing::warn!(
        path = %marker.display(),
        "the changes the marker listed landed before the coordinator stopped: the marker is removed"
    );
    Ok(())
}

/// The state files `flags` name (the state file `state`), the ops file at `ops` and the legacy
/// service `legacy`, opened as one backend and loaded, once the markers beside them are settled: a marker of changes not yet
/// written is refused when it lists nothing this build reads, and after the load when the ops
/// file does not hold every change it lists; a restore's marker is refused.
///
/// # Errors
/// [`StartError`]: a marker refuses the start, or a file cannot be opened or read.
fn open(
    flags: &Serve,
    state: &Path,
    legacy: Option<&str>,
    ops: &Path,
    oidc_clock: &Clock,
) -> Result<(Files, Loaded), StartError> {
    let marker = refuse_unfinished_halt(ops)?;
    refuse_unfinished_restore(state)?;
    refuse_unfinished_migrate(state)?;
    let rotation = Rotation {
        max_bytes: flags.audit_max_bytes.unwrap_or(DEFAULT_MAX_BYTES),
        retained: flags.audit_retained_segments.unwrap_or(DEFAULT_RETAINED),
    };
    let files = Files::default()
        .with_jti(&jti_path(state), Arc::clone(oidc_clock))?
        .with_desired(state, legacy)
        .with_ops(ops, rotation)?
        .with_verdicts(&verdicts_path(state))
        .with_accounts(&accounts_path(state));
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
            BackendError::Accounts(why) => why.into(),
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

/// Refuses to start over the marker of a `host-groups migrate --apply` that did not finish
/// ([`crate::backup::migrate_marker`]): the state files may hold part of the migration only.
///
/// # Errors
/// [`StartError::MigrateUnfinished`]: the marker is there.
pub(crate) fn refuse_unfinished_migrate(state: &Path) -> Result<(), StartError> {
    let marker = migrate_marker(state);
    if marker.exists() {
        return Err(StartError::MigrateUnfinished { path: marker });
    }
    Ok(())
}

/// The tokens an operator calls with: the file of `--tokens-file`, the one token of
/// `--operator-token-file`, or none when only `--initial-admin-file` is given (the users sign in
/// with a password).
///
/// # Errors
/// [`StartError::TokenFlags`]: both token flags are given. [`StartError::NoTokenFlag`]: none of
/// the three flags is. [`StartError::Tokens`] and [`StartError::OperatorToken`]: the file is
/// refused.
pub(super) fn operator_tokens(flags: &Serve) -> Result<OperatorTokens, StartError> {
    let initial = flags.login.initial_admin_file.is_some();
    Ok(match (&flags.tokens_file, &flags.operator_token_file) {
        (Some(path), None) => OperatorTokens::from_file(path)?,
        (None, Some(path)) => OperatorTokens::single(
            &hostkeeper_wire::secret::read_token(path).map_err(StartError::OperatorToken)?,
        ),
        (Some(_), Some(_)) => return Err(StartError::TokenFlags),
        (None, None) if initial => OperatorTokens::none(),
        (None, None) => return Err(StartError::NoTokenFlag),
    })
}

/// The initial-admin file `--initial-admin-file` names, judged by the wall clock, or none.
///
/// # Errors
/// [`StartError::InitialAdmin`]: the password settings are refused.
pub(super) fn initial_admin(flags: &Serve) -> Result<Option<InitialAdmin>, StartError> {
    let login = &flags.login;
    login
        .initial_admin_file
        .as_ref()
        .map(|path| {
            InitialAdmin::new(
                path.clone(),
                login.initial_password_secs,
                login.password_settings(),
                wall_clock(),
            )
        })
        .transpose()
        .map_err(StartError::from)
}

/// `coordinator` looking at the initial-admin file of `--initial-admin-file`, if it is given, and
/// having looked once: the user a fresh file asks for exists before the first call is served.
///
/// # Errors
/// [`StartError::InitialAdmin`]: the settings or the file are refused.
pub(super) fn with_initial_admin(
    coordinator: Coordinator,
    flags: &Serve,
) -> Result<Coordinator, StartError> {
    let coordinator = match initial_admin(flags)? {
        Some(file) => coordinator.with_initial_admin(file),
        None => coordinator,
    };
    coordinator.look_at_initial_admin()?;
    Ok(coordinator)
}
