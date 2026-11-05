//! The step record: the updater's memory of one update (docs/design/hostkeeper-design.md, 6.2).
//!
//! Before each step the record is written and fsynced (temporary file, fsync, rename, directory
//! fsync) with the step in state `running`; when the step ends it is written again as `done`
//! with the exit code and the hook's last line. It holds no secret. It carries a `schema`
//! number; a record newer than this binary is never changed, only reported.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use hostkeeper_hook::{Class, Phase, Step, Verdict};
use serde::{Deserialize, Serialize};

use crate::clock::Stamp;
use crate::force::ForceRecord;

/// The schema this binary writes.
pub const SCHEMA: u32 = 1;
/// The record's file name in the state directory.
pub const RECORD_FILE: &str = "step-record.json";
/// The params file's name in the state directory.
pub const PARAMS_FILE: &str = "params.toml";
/// The reason of a hooked update a `force` stopped (design section 5.6, steps 4 to 6).
pub const REASON_FORCED_OVER: &str = "forced_over";
/// The reason of the `Failed` an update of the updater itself ends with when the binary that
/// runs after it is not the new one: the fallback, or a hand switch, put the previous one back
/// (design section 6.6).
pub const REASON_SELF_UPDATE_FALLBACK: &str = "self_update_fallback";
/// The reasons of the `Deferred` verdicts that a confirmed update of the updater itself clears
/// ([`RecordStore::clear_updater_old`]): a hook that needs a newer updater
/// ([`crate::runner::REASON_UPDATER_OLD`]) and a package that needs a newer daemon
/// (`daemon_too_old`).
pub const REASONS_CLEARED_BY_SELF_UPDATE: [&str; 2] =
    [crate::runner::REASON_UPDATER_OLD, "daemon_too_old"];

/// Which release's hook a step runs from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookSel {
    /// The staged (new) release: the rule for every step.
    New,
    /// The previous release: `rollback` only, when the new hook cannot start at all.
    Previous,
}

/// Where a step is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum State {
    /// The step is chosen and has not started (or its unit was stopped on resume).
    Pending,
    /// The step started at `started` and must end by `deadline_secs` on the same boot.
    Running {
        started: Stamp,
        /// The second it must end by; for an `unbounded` step the second it started, and read by
        /// nothing.
        deadline_secs: u64,
        /// The step has no bound (a `prepare` whose manifest timeout is 0): the dead-man was not
        /// armed.
        /// Written only when true, so a bounded step writes the bytes it always did, and a
        /// record without it (every record from before it existed) reads as bounded.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        unbounded: bool,
    },
    /// The step ended.
    Done {
        code: Option<i32>,
        last_line: Option<String>,
    },
    /// The update is over.
    Finished {
        verdict: Verdict,
        reason: Option<String>,
    },
}

/// What the runner remembers between steps, besides the step.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    /// A step reported `reversible: false`.
    pub committed: bool,
    /// The last `verify` reported `promotion_pending`.
    pub promotion_pending: bool,
    /// What to report when the running `rollback` succeeds.
    pub then: Option<Verdict>,
    /// How many times `rollback` or `cleanup` was run again.
    pub retry: u32,
    /// When `cutover` succeeded: the verify window began.
    pub verify_started: Option<Stamp>,
    /// When the post-success watcher began; `Some` while it runs.
    pub watching: Option<Stamp>,
    /// The `reason` of the step that sent the update to `rollback`.
    #[serde(default)]
    pub reason: Option<String>,
    /// The update of the updater itself is over but for the restart: its new binary is in place
    /// and has not run yet (design section 6.6). The record stays unfinished (`done` at
    /// `cleanup`) until the next start settles it ([`crate::selfupdate::settle`]), so that no
    /// binary that does not know this field reads it as succeeded. Additive: written only when
    /// set, and a record without it loads as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awaiting_restart: Option<AwaitingRestart>,
}

/// An update of the updater itself waiting for its new binary to start.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AwaitingRestart {
    /// The version the update switched to.
    pub version: String,
    /// What the `Succeeded` the new binary confirms says of the promotion.
    pub verdict_promotion_pending: bool,
}

/// How an update waiting for the restart ends ([`RecordStore::settle_awaiting_restart`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settlement {
    /// The new binary runs: `Succeeded`.
    Confirmed,
    /// Another binary runs: `Failed` after the commit, of class release,
    /// [`REASON_SELF_UPDATE_FALLBACK`].
    FellBack,
}

/// The step record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub schema: u32,
    pub service: String,
    pub target_version: String,
    pub artifact_digest: String,
    /// The sha256 of the artifact (tarball) the order names.
    pub tarball_digest: String,
    pub previous_digest: Option<String>,
    /// The number of this update on this host; the unit names carry it.
    pub generation: u64,
    /// The params file of this update: written once, the same path for every step, removed when
    /// the update ends or is abandoned.
    pub params: PathBuf,
    pub step: Step,
    pub attempt: u32,
    pub state: State,
    pub hook: HookSel,
    pub progress: Progress,
    /// The coordinator's try of the order this update runs for (`Order.try`), when the daemon was
    /// given one. Additive: a record written before the field existed loads with `None`, and a
    /// record without a try is written without the key, byte for byte as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_try: Option<u64>,
    /// A `force` of the service stopped this update (design section 5.6, step 4): the runner
    /// launches no further step of it and closes it `Failed`, `forced_over`. Additive: written
    /// only when set, and a record without it loads as `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub forced: bool,
    /// The record is the journal of a `force` ([`ForceRecord`]), not of a hooked update.
    /// Additive: written only when set, and a record without it loads as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force: Option<ForceRecord>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl Record {
    /// The deterministic name of the unit a step of this update runs in.
    pub fn unit(&self, step: Step) -> String {
        crate::systemd::unit_name(&self.service, step, self.generation)
    }

    /// Closes the update as a `force` closes one it stopped: `forced` set, and `Failed` with the
    /// reason `forced_over`, in the phase it had reached (after the commit once a step committed),
    /// of class environment. Nothing of the update is undone.
    pub fn close_forced_over(&mut self) {
        self.forced = true;
        let phase = match self.progress.committed {
            true => Phase::AfterCommit,
            false => Phase::BeforeCommit,
        };
        self.state = State::Finished {
            verdict: Verdict::Failed {
                phase,
                class: Class::Environment,
            },
            reason: Some(REASON_FORCED_OVER.to_owned()),
        };
    }
}

/// What a state directory holds.
#[derive(Debug, PartialEq, Eq)]
pub enum Loaded {
    Absent,
    Current(Box<Record>),
    /// A record written by a newer binary: read, never changed.
    Newer {
        schema: u64,
    },
}

/// A place in the writing of a record, for a test that stops the updater there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrashPoint {
    /// Before the n-th write (counted from `1`) touched the disk.
    BeforeWrite(u64),
    /// After the n-th write was durable.
    AfterWrite(u64),
}

/// Why a record could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    #[error("cannot use the state directory: {0}")]
    Io(#[from] io::Error),
    #[error("the step record is not readable: {0}")]
    Corrupt(String),
    /// A test gate stopped the updater here, as a crash would.
    #[error("stopped at {0:?}")]
    Crashed(CrashPoint),
}

/// A gate that answers `true` at the points where the updater is to die.
pub type CrashGate = Arc<dyn Fn(CrashPoint) -> bool + Send + Sync>;

/// A test's way to make the n-th write of the record (counted from `1`) fail with an I/O error
/// before it touches the disk, as a full disk would: `Some` for a write that fails.
pub type WriteFault = Arc<dyn Fn(u64) -> Option<io::Error> + Send + Sync>;

/// The record and the params file of one service's updates, in its state directory.
pub struct RecordStore {
    dir: PathBuf,
    gate: Option<CrashGate>,
    fault: Option<WriteFault>,
    writes: AtomicU64,
    /// Held from a read of the record to the write that follows it, wherever the record may be
    /// written by two parties: the runner and a `force` that flags it.
    lock: Arc<Mutex<()>>,
}

impl RecordStore {
    /// The store in `dir`, which exists.
    pub fn new(dir: &Path) -> RecordStore {
        RecordStore {
            dir: dir.to_path_buf(),
            gate: None,
            fault: None,
            writes: AtomicU64::new(0),
            lock: Arc::default(),
        }
    }

    /// The same store, its record lock `lock`: every store of one state directory is given the
    /// same lock, so that a `force` flagging the record ([`RecordStore::mark_forced`]) and the
    /// runner writing it take turns. A store made without one has a lock of its own.
    pub fn with_lock(mut self, lock: Arc<Mutex<()>>) -> RecordStore {
        self.lock = lock;
        self
    }

    /// The record lock, held until the guard is dropped.
    pub(crate) fn hold(&self) -> MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Step 4 of a `force` (design section 5.6): under the record lock, flags the unfinished
    /// hooked update the record holds `forced` and writes it, unless it already is. Returns that
    /// record; `None`, and nothing written, when the record is absent, finished, the journal of
    /// a force, or written by a newer daemon.
    pub fn mark_forced(&self) -> Result<Option<Record>, RecordError> {
        let _held = self.hold();
        let mut rec = match self.load()? {
            Loaded::Current(rec) => rec,
            Loaded::Absent | Loaded::Newer { .. } => return Ok(None),
        };
        let finished = matches!(rec.state, State::Finished { .. });
        if finished | rec.force.is_some() {
            return Ok(None);
        }
        if !rec.forced {
            rec.forced = true;
            self.save(&rec)?;
        }
        Ok(Some(*rec))
    }

    /// The same store, stopped wherever `gate` says: the seam of the crash-and-resume tests.
    pub fn with_crash_gate(mut self, gate: CrashGate) -> RecordStore {
        self.gate = Some(gate);
        self
    }

    /// The same store, its writes of the record failed wherever `fault` says: the seam of the
    /// tests of a record that cannot be written. It can only make a write fail.
    pub fn with_write_fault(mut self, fault: WriteFault) -> RecordStore {
        self.fault = Some(fault);
        self
    }

    /// The state directory the store keeps its files in.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Under the record lock, closes the update the record holds as waiting for the restart
    /// ([`Progress::awaiting_restart`]) in one write, the field cleared: `Succeeded` for
    /// [`Settlement::Confirmed`], with the promotion the field kept, and `Failed` after the
    /// commit, of class release, [`REASON_SELF_UPDATE_FALLBACK`], for [`Settlement::FellBack`].
    /// `false`, and nothing written, when the record is absent, newer, or waits for no restart.
    pub fn settle_awaiting_restart(&self, outcome: Settlement) -> Result<bool, RecordError> {
        let _held = self.hold();
        let mut rec = match self.load()? {
            Loaded::Current(rec) => rec,
            Loaded::Absent | Loaded::Newer { .. } => return Ok(false),
        };
        let Some(awaiting) = rec.progress.awaiting_restart.take() else {
            return Ok(false);
        };
        rec.state = match outcome {
            Settlement::Confirmed => State::Finished {
                verdict: Verdict::Succeeded {
                    promotion_pending: awaiting.verdict_promotion_pending,
                },
                reason: None,
            },
            Settlement::FellBack => State::Finished {
                verdict: Verdict::Failed {
                    phase: Phase::AfterCommit,
                    class: Class::Release,
                },
                reason: Some(REASON_SELF_UPDATE_FALLBACK.to_owned()),
            },
        };
        self.save(&rec)?;
        Ok(true)
    }

    /// Under the record lock, removes the record when it holds a `Deferred` verdict whose reason
    /// is one of [`REASONS_CLEARED_BY_SELF_UPDATE`]: the updater that deferred the order is gone,
    /// and the next order runs. Any other record, of any state, verdict or reason, is left as it
    /// is, and so is a newer one; no record is fine. `true` when it removed one.
    pub fn clear_updater_old(&self) -> Result<bool, RecordError> {
        let _held = self.hold();
        let rec = match self.load()? {
            Loaded::Current(rec) => rec,
            Loaded::Absent | Loaded::Newer { .. } => return Ok(false),
        };
        let cleared = match &rec.state {
            State::Finished {
                verdict: Verdict::Deferred { .. },
                reason: Some(reason),
            } => REASONS_CLEARED_BY_SELF_UPDATE.contains(&reason.as_str()),
            _ => false,
        };
        if cleared {
            fs::remove_file(self.dir.join(RECORD_FILE))?;
            File::open(&self.dir)?.sync_all()?;
        }
        Ok(cleared)
    }

    /// The params file the updater writes once per update.
    pub fn params_path(&self) -> PathBuf {
        self.dir.join(PARAMS_FILE)
    }

    /// Reads the record.
    pub fn load(&self) -> Result<Loaded, RecordError> {
        let bytes = match fs::read(self.dir.join(RECORD_FILE)) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Loaded::Absent),
            Err(e) => return Err(e.into()),
        };
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|e| RecordError::Corrupt(e.to_string()))?;
        let schema = value.get("schema").and_then(serde_json::Value::as_u64);
        if let Some(schema) = schema.filter(|schema| *schema > u64::from(SCHEMA)) {
            return Ok(Loaded::Newer { schema });
        }
        let record =
            serde_json::from_value(value).map_err(|e| RecordError::Corrupt(e.to_string()))?;
        Ok(Loaded::Current(Box::new(record)))
    }

    /// Writes the record, durably.
    pub fn save(&self, record: &Record) -> Result<(), RecordError> {
        let n = self.writes.fetch_add(1, Ordering::SeqCst) + 1;
        self.cross(CrashPoint::BeforeWrite(n))?;
        if let Some(error) = self.fault.as_ref().and_then(|fault| fault(n)) {
            return Err(RecordError::Io(error));
        }
        let bytes =
            serde_json::to_vec_pretty(record).map_err(|e| RecordError::Corrupt(e.to_string()))?;
        write_atomic(&self.dir, RECORD_FILE, &bytes)?;
        self.cross(CrashPoint::AfterWrite(n))
    }

    /// Refuses a state directory that is not the updater's alone: the params file and the record
    /// live there, so it must be owned by the running user and closed to group and others.
    pub fn check_private(&self) -> Result<(), RecordError> {
        use std::os::unix::fs::MetadataExt;
        let meta = fs::symlink_metadata(&self.dir)?;
        // One comparison: a directory, mine, and closed to group and others.
        let mine = meta.uid() == rustix::process::geteuid().as_raw();
        let private = (meta.is_dir(), mine, meta.mode() & 0o077 == 0);
        if private == (true, true, true) {
            return Ok(());
        }
        Err(RecordError::Io(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the state directory is not a private directory of the updater",
        )))
    }

    /// Removes the params file; one that is not there is fine.
    pub fn remove_params(&self) -> Result<(), RecordError> {
        match fs::remove_file(self.params_path()) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    /// Writes the params file, durably.
    pub fn save_params(&self, text: &str) -> Result<(), RecordError> {
        Ok(write_atomic(&self.dir, PARAMS_FILE, text.as_bytes())?)
    }

    fn cross(&self, point: CrashPoint) -> Result<(), RecordError> {
        match &self.gate {
            Some(gate) if gate(point) => Err(RecordError::Crashed(point)),
            _ => Ok(()),
        }
    }
}

/// Temporary file, fsync, rename, directory fsync.
pub(crate) fn write_atomic(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let tmp = dir.join(format!("{name}.tmp"));
    let mut file = File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&tmp, dir.join(name))?;
    File::open(dir)?.sync_all()
}
