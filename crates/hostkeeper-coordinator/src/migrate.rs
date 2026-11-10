//! `hostkeeper-coordinator host-groups migrate`: the conversion of a store that places hosts by
//! roles, a rollout file and assignments into one that places them by host group, in one offline
//! verb (`docs/design/hostgroups.md` section 10).
//!
//! It reads the whole store, files (`--state-file`, `--ops-file`, `--enrollment-dir`, read as
//! `import` reads them, writing nothing) or database (`--database-dsn-file`), and the rollout file
//! once ([`crate::services::Services::from_file`]), and plans ([`crate::migrate_plan`]): the
//! groups (one per role and set of packages), each group's packages and policy, its desired
//! versions, marks and minimum versions, the repository of each service that names one, the holds
//! of a role, the host group of every host, approval and inventory record, and the edits the Mac
//! allowlist needs. Without `--apply` it prints the plan and writes nothing. With `--apply` it
//! writes it ([`crate::migrate_apply`]) in one transaction (database) or in the order the files
//! need, under the lock of the state files and the marker `<state-file>.migrate-unfinished`
//! (files), and refuses while the plan holds an error, writing nothing.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use hostkeeper_enrollment::enroll::StoreError as RecordsError;

use crate::audit_file::UnixClock;
use crate::backup::{self, BackupError, Report, StatePaths};
use crate::config::StartError;
use crate::enrollment::{AllowlistError, MacAllowlist};
use crate::lock::StateLock;
use crate::migrate_apply::apply_files;
use crate::migrate_plan::{Inputs, Plan, plan};
use crate::ops::OpsError;
use crate::services::{Services, ServicesError};
use crate::store::BackendError;
use crate::store::postgres::transfer::{self, MoveError, Moved, Rewrite};
use crate::store::postgres::{DatabaseError, Dsn};
use crate::transfer::{TransferError, read_files};

/// The verbs under `host-groups`.
#[derive(Clone, Debug, Subcommand)]
pub enum HostGroupsCommand {
    /// Convert the store of a stopped coordinator from placing hosts by roles, the rollout file
    /// and assignments to placing them by host group: print the plan, or with --apply write it
    /// in one transaction (database) or one commit of the files, with one audit entry per group,
    /// by `migration`. Writes nothing and exits 1 while the plan holds an error.
    Migrate(Migrate),
}

/// The flags of `host-groups migrate`.
#[derive(Clone, Debug, Args)]
// The store of the enrollment records: the directory or the database (which refuses the files
// beside it, naming the flag).
#[group(id = "records_store", args = ["enrollment_dir", "database_dsn_file"], multiple = true)]
pub struct Migrate {
    /// The rollout file `serve` was given, read once: the services, the roles and selectors that
    /// place them, their policies, priorities, minimum versions and repositories.
    #[arg(long)]
    pub rollout_file: PathBuf,
    /// The desired-state file of the file store, as `serve --state-file` names it. Exactly one
    /// of this and `--database-dsn-file`.
    #[arg(long, required_unless_present = "database_dsn_file")]
    pub state_file: Option<PathBuf>,
    /// The ops file, as `serve --ops-file` names it. Default: the state file's name with `.ops`
    /// added.
    #[arg(long)]
    pub ops_file: Option<PathBuf>,
    /// The enrollment store of the file store, as `serve --enrollment-dir` names it: required
    /// with --state-file, since without the enrolled hosts there is no host to put in a group.
    #[arg(long)]
    pub enrollment_dir: Option<PathBuf>,
    /// The Postgres store, as `serve --database-dsn-file` names it. Its lock is taken by
    /// `--apply`, so a coordinator serving on it refuses it.
    #[arg(long)]
    pub database_dsn_file: Option<PathBuf>,
    /// The legacy service, as `serve --legacy-service` names it: a desired-state file in the
    /// single-state form is the state of this service.
    #[arg(long)]
    pub legacy_service: Option<String>,
    /// `serve --live-rollouts` as it will be given: the plan says how many packages each group
    /// may roll at once.
    #[arg(
        long,
        default_value_t = 1,
        value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(1..=16)
    )]
    pub live_rollouts: usize,
    /// The Mac serial allowlist `serve` was given: the plan says the `host_group` each entry
    /// needs and the `services` that go.
    #[arg(long)]
    pub mac_serial_allowlist: Option<PathBuf>,
    /// Write the plan. Without it nothing is written.
    #[arg(long)]
    pub apply: bool,
}

/// Why a migration was refused, or failed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum MigrateError {
    /// The files could not be locked, read or written, or hold what `serve` refuses.
    #[error(transparent)]
    Transfer(#[from] TransferError),
    /// A flag is refused as `serve` refuses it.
    #[error(transparent)]
    Flag(#[from] StartError),
    /// The rollout file is refused.
    #[error("--rollout-file: {0}")]
    Rollout(#[from] ServicesError),
    /// The Mac serial allowlist is refused.
    #[error(transparent)]
    Allowlist(#[from] AllowlistError),
    /// With `--state-file` and no enrollment store.
    #[error(
        "--enrollment-dir is not given: without the enrolled hosts there is no host to put in a group"
    )]
    NoEnrollment,
    /// The store holds a host group already.
    #[error("the store already holds a host group: it is migrated already")]
    AlreadyMigrated,
    /// The plan holds an error: the plan as printed.
    #[error("{0}")]
    Refused(String),
    /// The database refused.
    #[error(transparent)]
    Database(#[from] MoveError),
    /// A file of the store could not be written.
    #[error(transparent)]
    Files(#[from] BackupError),
    /// The enrollment store could not be written.
    #[error("--enrollment-dir: {0}")]
    Records(#[from] RecordsError),
    /// The ops file or the audit file could not be opened.
    #[error(transparent)]
    Ops(#[from] OpsError),
    /// A file of the state could not be written.
    #[error(transparent)]
    Backend(#[from] BackendError),
    /// A file could not be made or removed.
    #[error("{path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// Why.
        source: std::io::Error,
    },
}

impl From<DatabaseError> for MigrateError {
    fn from(why: DatabaseError) -> Self {
        Self::Database(MoveError::Database(why))
    }
}

/// Where the store is.
enum Store {
    /// The files, and the enrollment store's directory.
    Files(StatePaths, PathBuf),
    /// The database whose connection string is in the file.
    Database(PathBuf),
}

impl Migrate {
    /// The store the flags name: exactly one, the file flags refused beside the database.
    fn store(&self) -> Result<Store, MigrateError> {
        let Some(dsn) = &self.database_dsn_file else {
            let state = self.state_file.clone().unwrap_or_default();
            let Some(dir) = self.enrollment_dir.clone() else {
                return Err(MigrateError::NoEnrollment);
            };
            let paths = StatePaths {
                state_file: state,
                ops_file: self.ops_file.clone(),
                enrollment_dir: Some(dir.clone()),
            };
            return Ok(Store::Files(paths, dir));
        };
        let beside = [
            ("--state-file", self.state_file.is_some()),
            ("--ops-file", self.ops_file.is_some()),
            ("--enrollment-dir", self.enrollment_dir.is_some()),
        ];
        match beside.into_iter().find(|(_, given)| *given) {
            Some((flag, _)) => Err(StartError::DatabaseAndFileFlag(flag).into()),
            None => Ok(Store::Database(dsn.clone())),
        }
    }
}

impl Store {
    /// What the store is called in the report.
    fn named(&self) -> String {
        match self {
            Store::Files(paths, _) => format!("the file store of {}", paths.state_file.display()),
            Store::Database(dsn) => format!("the database of {}", dsn.display()),
        }
    }
}

/// The plan of `moved`, refused when the store holds a group already.
fn planned(
    flags: &Migrate,
    moved: &Moved,
    rollout: &Services,
    allowlist: Option<&MacAllowlist>,
    now: u64,
) -> Result<Plan, MigrateError> {
    if !moved.loaded.desired.host_groups.is_empty() {
        return Err(MigrateError::AlreadyMigrated);
    }
    Ok(plan(&Inputs {
        moved,
        rollout,
        allowlist,
        live_rollouts: u32::try_from(flags.live_rollouts).unwrap_or(u32::MAX),
        now,
    }))
}

/// The lines of the report: the header, the plan, and how it ends in `ending` when it holds no
/// error.
fn report(header: &str, plan: &Plan, ending: &str) -> Vec<String> {
    let mut lines = vec![header.to_owned()];
    lines.extend(plan.lines.iter().cloned());
    if plan.errors.is_empty() {
        lines.push("no errors".to_owned());
        lines.push(ending.to_owned());
    } else {
        lines.push("errors:".to_owned());
        lines.extend(plan.errors.iter().map(|why| format!("  {why}")));
        lines.push("nothing written; --apply refuses while there is an error".to_owned());
    }
    lines
}

/// The refusal that prints `plan` whole, when it holds an error.
fn refusal(header: &str, plan: &Plan) -> MigrateError {
    MigrateError::Refused(report(header, plan, "").join("\n"))
}

/// The audit numbers the plan writes, `first..last`.
fn numbers(plan: &Plan) -> String {
    let at = |line: Option<&crate::audit_file::AuditLine>| line.map_or(0, |line| line.seq);
    format!("{}..{}", at(plan.audit.first()), at(plan.audit.last()))
}

/// Runs `flags`: plans the migration of the store they name and, with `--apply`, writes it, under
/// the lock of the state files taken by `lock` (files). The plan is made at the time `clock`
/// says.
///
/// # Errors
/// [`MigrateError`]: a flag, the rollout file or the allowlist is refused, a marker or a lock
/// refuses the files, the store holds a group already, the plan holds an error (it is the error's
/// text), or a write failed.
pub(crate) fn migrate(
    flags: &Migrate,
    lock: &dyn StateLock,
    clock: &UnixClock,
) -> Result<Report, MigrateError> {
    let store = flags.store()?;
    let rollout = Services::from_file(&flags.rollout_file)?;
    let allowlist = flags
        .mac_serial_allowlist
        .as_deref()
        .map(MacAllowlist::load)
        .transpose()?;
    let mode = if flags.apply { "apply" } else { "dry run" };
    let header = format!(
        "hostkeeper-coordinator host-groups migrate: {} ({mode})",
        store.named()
    );
    let legacy = flags.legacy_service.as_deref();
    match store {
        Store::Files(paths, dir) => {
            let _held = flags
                .apply
                .then(|| backup::take(lock, &paths.state_file))
                .transpose()?;
            let now = clock();
            let moved = read_files(&paths, legacy, now)?;
            let plan = planned(flags, &moved, &rollout, allowlist.as_ref(), now)?;
            if !plan.errors.is_empty() {
                return Err(refusal(&header, &plan));
            }
            if !flags.apply {
                let ending = "nothing written; --apply writes it in one commit";
                return Ok(Report::of(report(&header, &plan, ending)));
            }
            apply_files(&paths, &dir, legacy, &moved.loaded, &plan)?;
            let ending = format!(
                "written in one commit (files); audit entries {}",
                numbers(&plan)
            );
            Ok(Report::of(report(&header, &plan, &ending)))
        }
        Store::Database(file) => {
            let dsn = Dsn::read(&file).map_err(DatabaseError::from)?;
            let now = clock();
            if !flags.apply {
                let moved = transfer::read_only(&dsn, now)?;
                let plan = planned(flags, &moved, &rollout, allowlist.as_ref(), now)?;
                if !plan.errors.is_empty() {
                    return Err(refusal(&header, &plan));
                }
                let ending = "nothing written; --apply writes it in one commit";
                return Ok(Report::of(report(&header, &plan, ending)));
            }
            let plan = transfer::rewrite(&dsn, now, |moved| {
                let plan = planned(flags, moved, &rollout, allowlist.as_ref(), now)?;
                if !plan.errors.is_empty() {
                    return Err(refusal(&header, &plan));
                }
                let rewrite = Rewrite {
                    after: plan.after.clone(),
                    changed: plan.changed.clone(),
                    lines: plan.audit.clone(),
                };
                Ok((rewrite, plan))
            })?;
            let ending = format!(
                "written in one transaction (database); audit entries {}",
                numbers(&plan)
            );
            Ok(Report::of(report(&header, &plan, &ending)))
        }
    }
}
