//! The write of `host-groups migrate --apply` to a file store: the files in the order that leaves
//! the store a coordinator refuses until the last of them is written.
//!
//! The marker `<state-file>.migrate-unfinished` is written first and removed last, so that
//! `serve` and the other verbs refuse files a stopped migration left. The changed host and
//! request records of the enrollment store come next, then the audit lines and the ops file, and
//! the state file last: it introduces `host_groups`, which a coordinator without groups refuses,
//! so it is the commit point. A write that fails leaves the marker, and the operator restores the
//! backup taken before it.

use std::fs::{self, File};
use std::path::Path;
use std::sync::Arc;

use hostkeeper_enrollment::enroll::records::{FileRecords, Record, Records as _};
use hostkeeper_enrollment::fs::RealFs;

use crate::audit_file::DEFAULT_ROTATION;
use crate::backup::{StatePaths, migrate_marker, parent_of};
use crate::migrate::MigrateError;
use crate::migrate_plan::Plan;
use crate::store::{Backend as _, Commit, Files, Loaded};

/// Writes `plan`, made over `before`, into the file store `paths` names, whose enrollment store
/// is the directory `records`, with the desired-state file read as `legacy`'s state when it is in
/// the single-state form.
///
/// # Errors
/// [`MigrateError`]: a file could not be written. The marker stays.
pub(crate) fn apply_files(
    paths: &StatePaths,
    records: &Path,
    legacy: Option<&str>,
    before: &Loaded,
    plan: &Plan,
) -> Result<(), MigrateError> {
    let state = &paths.state_file;
    let marker = migrate_marker(state);
    let at = |path: &Path| {
        let path = path.to_path_buf();
        move |source| MigrateError::Io { path, source }
    };
    File::create(&marker)
        .and_then(|file| file.sync_all())
        .and_then(|()| File::open(parent_of(&marker))?.sync_all())
        .map_err(at(&marker))?;
    let store = FileRecords::open(Arc::new(RealFs), records)?;
    let changed: Vec<Record> = plan
        .changed
        .hosts
        .iter()
        .cloned()
        .map(Record::Host)
        .chain(plan.changed.requests.iter().cloned().map(Record::Request))
        .collect();
    store.commit(&changed)?;
    let files = Files::default()
        .with_desired(state, legacy)
        .with_ops(&paths.ops_file(), DEFAULT_ROTATION)?;
    files.commit(&Commit {
        ops: Some((&before.ops, &plan.after.loaded.ops)),
        audit: &plan.audit,
        ..Commit::default()
    })?;
    files.commit(&Commit {
        desired: Some((&before.desired, &plan.after.loaded.desired)),
        ..Commit::default()
    })?;
    fs::remove_file(&marker)
        .and_then(|()| File::open(parent_of(&marker))?.sync_all())
        .map_err(at(&marker))
}
