//! A host's turn of a maintenance run (docs/design/maintenance-updates.md, sections 5.2 to 5.7):
//! the run record and its state machine, the daemon's memory of the turn and the pure rules of
//! what comes next ([`MaintenanceRecord`], [`transition`]), and the run of one service's `drain`
//! or `resume` through the launcher of the host ([`run_maintenance_step`], in `step.rs`, whose
//! documentation gives how each end of such a step is read).
//!
//! The step runner's sequence does not fit an OS update (it has no release to restart and no
//! rollback), so a `maintain` order is run by a small sequencer of the daemon's own. This module is
//! what such a sequencer keeps and asks; it launches nothing and reads no clock.
//!
//! # The file
//!
//! - **Name:** [`MAINTENANCE_RECORD_FILE`], `maintenance-record.json`.
//! - **Location:** the directory [`MAINTENANCE_DIR`], `maintenance`, beside the daemon's state
//!   directory ([`MaintenanceStore::beside`]:
//!   `/var/lib/updater/maintenance/maintenance-record.json` beside `/var/lib/updater/state`), as
//!   the hooks' records are beside it in `hooks`. It is a
//!   sibling and not a file of the state directory, whose entries are the services' own
//!   directories, so that no service name can meet it.
//! - **Writing:** the whole record, as pretty JSON, to `maintenance-record.json.tmp`, fsynced,
//!   renamed over the record, and the directory fsynced ([`MaintenanceStore::save`]): before each
//!   step is launched ([`Event::Started`]) and again when it ends. It holds no secret: a reason
//!   is the step and the sequencer's words, never a password.
//! - **Schema:** the number [`MAINTENANCE_SCHEMA`] in the key `schema`. A record whose `schema` is
//!   newer than this binary's reads as [`MaintenanceLoaded::Newer`] and is never written over
//!   ([`MaintenanceRecordError::Newer`]); a record that does not read, or reads and breaks a rule
//!   of [`MaintenanceRecord::check`], is [`MaintenanceRecordError::Corrupt`] and is never written
//!   over either: only [`MaintenanceStore::clear_record`] sets it aside, renamed to
//!   [`MAINTENANCE_CORRUPT_FILE`]. A key this binary does not know is ignored, so a later release
//!   may add one under the same schema.
//!
//! Schema 1 (every key is written; `reboot_under_way`, `deferred` and `halted` only when set):
//!
//! ```json
//! {
//!   "schema": 1,
//!   "run_id": "run-7",
//!   "order_id": "order-12",
//!   "order_try": 1,
//!   "kinds": [
//!     { "kind": "linux", "applied": true },
//!     { "kind": "reboot", "applied": false,
//!       "reboot_under_way": "0b6f3f3e-2a43-4b83-9a0a-6e0f1a8d1c55" }
//!   ],
//!   "services": ["js-agent", "tierstore"],
//!   "drained": ["js-agent", "tierstore"],
//!   "step": "apply",
//!   "running": true,
//!   "deferred": "drain tierstore: the drain deadline passed",
//!   "halted": "check: exit 1"
//! }
//! ```
//!
//! - `kinds`: the kinds of the run, each once, in the order they are applied (macOS first,
//!   [`Kind`]'s order), each with whether it is applied and, while its reboot is under way, the
//!   boot id before that reboot.
//! - `services`: the services to drain, in the group's package order; `drained`: the ones whose
//!   `drain` was launched and whose `resume` has not ended, always the first ones of `services`.
//!   A service is counted drained when its `drain` is launched, so a `drain` cut short is
//!   resumed too.
//! - `step`: `stage`, `drain`, `apply`, `check`, `put_back` or `done`; `running`: the step's launch
//!   was written and its end was not.
//! - `deferred`: why the turn is deferred (stage failed, or a drain's deadline passed): the
//!   drained services are put back and the turn ends deferred. `halted`: why the turn halted (a
//!   step from drain on failed): nothing more runs, not even a `resume`, and the step stays the
//!   one that failed.
//!
//! # The machine
//!
//! [`transition`] takes a record and an [`Event`] and gives the next record and the next
//! [`Action`]; [`next_action`] gives the action of a record as it stands, which is how a record
//! read after a crash, a reboot or a restart as a newer release is resumed. A step cut short is
//! launched again: every step of the sequence is one that may run twice (section 5.2).

mod machine;
mod record;
mod step;

pub use machine::{
    Action, Ending, Event, Launch, Outcome, TransitionError, next_action, transition,
};
pub use record::{
    InvalidRecord, Kind, KindProgress, MAINTENANCE_CORRUPT_FILE, MAINTENANCE_DIR,
    MAINTENANCE_RECORD_FILE, MAINTENANCE_SCHEMA, MaintenanceLoaded, MaintenanceRecord,
    MaintenanceRecordError, MaintenanceStore, RunStep,
};
pub use step::{
    MaintenanceCall, MaintenanceExit, MaintenanceOutcome, MaintenanceStep, RESUME_BOUND_SECS,
    run_maintenance_step,
};
