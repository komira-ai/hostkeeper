//! The run record of a host, kept in the maintenance run record's file
//! ([`hostkeeper_steps::maintenance::MaintenanceStore`]): the hook's port [`RunRecord`] over the
//! record the daemon's sequencer keeps, in the directory the `--params` file is in.
//!
//! The hook reads and changes one thing of that record: each kind's progress (`applied` and
//! `reboot_under_way`). Every other field is the daemon's sequencer's, and the hook writes it back
//! as it read it. The sequencer does not write the record while the hook runs (it saves before the
//! launch and after the step ends), so the hook's load, change and save of its kind is not raced.
//!
//! A record of another run, no record, one of a newer schema and one that does not read each fail
//! the call, naming what; no text holds a parameter's value other than the run id.

use std::path::Path;

use hostkeeper_steps::BootId;
use hostkeeper_steps::maintenance::{
    Kind as RecordKind, KindProgress, MaintenanceLoaded, MaintenanceRecord, MaintenanceStore,
};

use super::STDIN_PARAMS;
use super::params::Kind;
use super::ports::{KindState, RecordError, RunRecord};

/// The run record in a maintenance directory.
pub struct StoreRecord {
    store: MaintenanceStore,
}

impl StoreRecord {
    /// The record of `store`.
    pub fn new(store: MaintenanceStore) -> StoreRecord {
        StoreRecord { store }
    }

    /// The record in the directory of the `params` file, where the daemon's sequencer puts the
    /// parameters beside the record; `None` for parameters on standard input ([`STDIN_PARAMS`])
    /// and for a path with no parent (the empty path, or `/`); a bare file name is a record in the
    /// working directory.
    pub fn beside_params(params: &Path) -> Option<StoreRecord> {
        if params == Path::new(STDIN_PARAMS) {
            return None;
        }
        params
            .parent()
            .map(|dir| StoreRecord::new(MaintenanceStore::new(dir)))
    }

    /// The record of the run `run_id`.
    fn load(&self, run_id: &str) -> Result<MaintenanceRecord, RecordError> {
        match self.store.load() {
            Ok(MaintenanceLoaded::Current(record)) if record.run_id == run_id => Ok(*record),
            Ok(MaintenanceLoaded::Current(record)) => Err(failed(format!(
                "the run record holds run {}, not {run_id}",
                record.run_id
            ))),
            Ok(MaintenanceLoaded::Absent) => Err(failed("there is no run record".to_owned())),
            Ok(MaintenanceLoaded::Newer { schema }) => Err(failed(format!(
                "the run record has schema {schema}, of a newer hostkeeper"
            ))),
            Err(error) => Err(failed(error.to_string())),
        }
    }

    /// Loads the record of `run_id`, applies `change` to `kind`'s progress and saves it.
    fn change(
        &self,
        run_id: &str,
        kind: Kind,
        change: impl FnOnce(&mut KindProgress),
    ) -> Result<(), RecordError> {
        let mut record = self.load(run_id)?;
        let wanted = record_kind(kind);
        let progress = record
            .kinds
            .iter_mut()
            .find(|progress| progress.kind == wanted)
            .ok_or_else(|| failed(format!("the run record has no kind {kind}")))?;
        change(progress);
        self.store
            .save(&record)
            .map_err(|error| failed(error.to_string()))
    }
}

impl RunRecord for StoreRecord {
    fn state(&self, run_id: &str, kind: Kind) -> Result<KindState, RecordError> {
        let record = self.load(run_id)?;
        let wanted = record_kind(kind);
        let state = match record.kinds.iter().find(|progress| progress.kind == wanted) {
            Some(KindProgress { applied: true, .. }) => KindState::Applied,
            Some(KindProgress {
                reboot_under_way: Some(boot),
                ..
            }) => KindState::RebootUnderWay { boot: boot.clone() },
            _ => KindState::Pending,
        };
        Ok(state)
    }

    fn reboot_under_way(&self, run_id: &str, kind: Kind, boot: &BootId) -> Result<(), RecordError> {
        self.change(run_id, kind, |progress| {
            progress.reboot_under_way = Some(boot.clone());
        })
    }

    /// Sets `kind` applied and its reboot no longer under way. The record has no field for the
    /// version the host runs, so `_version` is not kept.
    fn applied(&self, run_id: &str, kind: Kind, _version: &str) -> Result<(), RecordError> {
        self.change(run_id, kind, |progress| {
            progress.applied = true;
            progress.reboot_under_way = None;
        })
    }
}

fn failed(text: String) -> RecordError {
    RecordError::Failed(text)
}

/// The record's word for a hook's kind.
pub(crate) fn record_kind(kind: Kind) -> RecordKind {
    match kind {
        Kind::Macos => RecordKind::Macos,
        Kind::Xcode => RecordKind::Xcode,
        Kind::Linux => RecordKind::Linux,
        Kind::Reboot => RecordKind::Reboot,
        Kind::Probe => RecordKind::Probe,
    }
}
