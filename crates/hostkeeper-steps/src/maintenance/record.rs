//! The maintenance run record and its file (the file's name, place and schema are in the
//! module's documentation, [`crate::maintenance`]).

use std::fmt;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use hostkeeper_config::names::ServiceName;
use serde::{Deserialize, Serialize};

use crate::clock::BootId;
use crate::record::{CrashGate, CrashPoint, write_atomic};

/// The schema this binary writes.
pub const MAINTENANCE_SCHEMA: u32 = 1;
/// The directory of the record, beside the daemon's state directory.
pub const MAINTENANCE_DIR: &str = "maintenance";
/// The record's file name in [`MAINTENANCE_DIR`].
pub const MAINTENANCE_RECORD_FILE: &str = "maintenance-record.json";
/// The name a record that does not read is kept under by [`MaintenanceStore::clear_record`],
/// beside where it was.
pub const MAINTENANCE_CORRUPT_FILE: &str = "maintenance-record.json.corrupt";

/// A kind of maintenance, in the order a run applies them: macOS before Xcode (a new Xcode can
/// need a newer macOS), and a Linux update before a reboot with nothing to install.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A macOS update or upgrade.
    Macos,
    /// An Xcode install, removal or switch of the default.
    Xcode,
    /// A Linux package update.
    Linux,
    /// A reboot the host needs, with nothing to install.
    Reboot,
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Kind::Macos => "macos",
            Kind::Xcode => "xcode",
            Kind::Linux => "linux",
            Kind::Reboot => "reboot",
        })
    }
}

/// Where a host's turn is in the sequence of section 5.2.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStep {
    /// The maintenance hook's `prepare`: check the space and download.
    Stage,
    /// Each service's `drain`, in the group's package order.
    Drain,
    /// The maintenance hook's `cutover <kind>`, once per kind, with its reboot.
    Apply,
    /// The maintenance hook's `verify`.
    Check,
    /// Each drained service's `resume`, in reverse order.
    PutBack,
    /// The turn is over.
    Done,
}

impl RunStep {
    /// The step after this one; the last is its own.
    pub(super) fn following(self) -> RunStep {
        match self {
            RunStep::Stage => RunStep::Drain,
            RunStep::Drain => RunStep::Apply,
            RunStep::Apply => RunStep::Check,
            RunStep::Check => RunStep::PutBack,
            RunStep::PutBack | RunStep::Done => RunStep::Done,
        }
    }
}

/// One kind of a run and how far it got.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KindProgress {
    pub kind: Kind,
    /// The kind is applied: its `cutover` ended well and, when it rebooted, the host came back
    /// on a new boot.
    pub applied: bool,
    /// The kind's reboot is under way: the boot id the host had before it. Written only when set.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "boot_id_text"
    )]
    pub reboot_under_way: Option<BootId>,
}

/// The maintenance run record: one host's turn of one run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaintenanceRecord {
    pub schema: u32,
    /// The run's id (`Maintain.run_id`).
    pub run_id: String,
    /// The id of the `maintain` order the turn was taken by (`Order.order_id`).
    pub order_id: String,
    /// The coordinator's try of that order (`Order.try`).
    pub order_try: u64,
    /// The kinds of the run, in the order they are applied.
    pub kinds: Vec<KindProgress>,
    /// The services to drain, in the group's package order.
    #[serde(with = "service_names")]
    pub services: Vec<ServiceName>,
    /// The services whose `drain` was launched and whose `resume` has not ended: always the
    /// first ones of `services`, in the same order.
    #[serde(with = "service_names")]
    pub drained: Vec<ServiceName>,
    pub step: RunStep,
    /// The step's launch was written and its end was not.
    pub running: bool,
    /// Why the turn is deferred: once the drained services are put back it ends deferred.
    /// Written only when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred: Option<String>,
    /// Why the turn halted: nothing more runs. Written only when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub halted: Option<String>,
}

/// A record that breaks a rule of [`MaintenanceRecord::check`].
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum InvalidRecord {
    /// `run_id` or `order_id` is empty.
    #[error("the maintenance record's {0} is empty")]
    EmptyId(&'static str),
    /// No kind.
    #[error("the maintenance record names no kind")]
    NoKinds,
    /// A kind named twice, or the kinds out of the order they are applied in.
    #[error("the maintenance record's kinds are not each named once, macos first: {0}")]
    Kinds(String),
    /// A service named twice in `services`.
    #[error("the maintenance record names the service {0} twice")]
    RepeatedService(String),
    /// `drained` is not the first services of `services`.
    #[error("the maintenance record's drained services are not the first of its services")]
    DrainedNotPlanned,
}

impl MaintenanceRecord {
    /// The record of a new turn: schema [`MAINTENANCE_SCHEMA`], at `stage`, nothing launched,
    /// nothing applied or drained. `kinds` may come in any order; the record keeps them in the
    /// order they are applied ([`Kind`]'s).
    pub fn new(
        run_id: &str,
        order_id: &str,
        order_try: u64,
        kinds: &[Kind],
        services: Vec<ServiceName>,
    ) -> Result<MaintenanceRecord, InvalidRecord> {
        let mut sorted = kinds.to_vec();
        sorted.sort();
        let record = MaintenanceRecord {
            schema: MAINTENANCE_SCHEMA,
            run_id: run_id.to_owned(),
            order_id: order_id.to_owned(),
            order_try,
            kinds: sorted
                .into_iter()
                .map(|kind| KindProgress {
                    kind,
                    applied: false,
                    reboot_under_way: None,
                })
                .collect(),
            services,
            drained: Vec::new(),
            step: RunStep::Stage,
            running: false,
            deferred: None,
            halted: None,
        };
        record.check()?;
        Ok(record)
    }

    /// The rules every record keeps: both ids given; one kind at least, each once, in the order
    /// they are applied; each service once; the drained services the first of the services.
    pub fn check(&self) -> Result<(), InvalidRecord> {
        let empty = [("run_id", &self.run_id), ("order_id", &self.order_id)]
            .into_iter()
            .find(|(_, id)| id.is_empty());
        if let Some((name, _)) = empty {
            return Err(InvalidRecord::EmptyId(name));
        }
        if self.kinds.is_empty() {
            return Err(InvalidRecord::NoKinds);
        }
        if !self
            .kinds
            .windows(2)
            .all(|pair| pair[0].kind < pair[1].kind)
        {
            let named: Vec<String> = self.kinds.iter().map(|k| k.kind.to_string()).collect();
            return Err(InvalidRecord::Kinds(named.join(", ")));
        }
        let repeated = self
            .services
            .iter()
            .enumerate()
            .find(|&(at, service)| self.services[..at].contains(service));
        if let Some((_, service)) = repeated {
            return Err(InvalidRecord::RepeatedService(service.to_string()));
        }
        if !self.services.starts_with(&self.drained) {
            return Err(InvalidRecord::DrainedNotPlanned);
        }
        Ok(())
    }
}

/// What the maintenance directory holds.
#[derive(Debug, PartialEq, Eq)]
pub enum MaintenanceLoaded {
    Absent,
    Current(Box<MaintenanceRecord>),
    /// A record written by a newer binary: read, never changed.
    Newer {
        schema: u64,
    },
}

/// Why the record could not be read, written or set aside.
#[derive(Debug, thiserror::Error)]
pub enum MaintenanceRecordError {
    #[error("cannot use the maintenance directory: {0}")]
    Io(#[from] io::Error),
    /// The record does not read, or breaks a rule of [`MaintenanceRecord::check`]: it is not
    /// written over, and only [`MaintenanceStore::clear_record`] sets it aside.
    #[error("the maintenance record is not readable: {0}")]
    Corrupt(String),
    /// The record is of a newer schema: it is not written over and not set aside.
    #[error(
        "the maintenance record has schema {schema}, of a newer hostkeeper; it is left as it is"
    )]
    Newer { schema: u64 },
    /// [`MaintenanceStore::save`] of a record that breaks a rule of [`MaintenanceRecord::check`]:
    /// nothing is written.
    #[error("the maintenance record is not written: {0}")]
    Invalid(#[from] InvalidRecord),
    /// [`MaintenanceStore::clear_record`] of a record that reads.
    #[error("the maintenance record reads; only one that does not is set aside")]
    Reads,
    /// [`MaintenanceStore::clear_record`] with no record.
    #[error("there is no maintenance record")]
    Absent,
    /// A test gate stopped the writer here, as a crash would.
    #[error("stopped at {0:?}")]
    Crashed(CrashPoint),
}

/// The maintenance record in its directory.
pub struct MaintenanceStore {
    dir: PathBuf,
    gate: Option<CrashGate>,
    writes: AtomicU64,
}

impl MaintenanceStore {
    /// The store in `dir`, which exists.
    pub fn new(dir: &Path) -> MaintenanceStore {
        MaintenanceStore {
            dir: dir.to_path_buf(),
            gate: None,
            writes: AtomicU64::new(0),
        }
    }

    /// The store of the daemon whose state directory is `state_dir`: [`MAINTENANCE_DIR`] beside
    /// it (in its parent; in the root for a state directory that has none).
    pub fn beside(state_dir: &Path) -> MaintenanceStore {
        let parent = state_dir.parent().unwrap_or(state_dir);
        MaintenanceStore::new(&parent.join(MAINTENANCE_DIR))
    }

    /// The same store, stopped wherever `gate` says: the seam of the crash-and-resume tests.
    pub fn with_crash_gate(mut self, gate: CrashGate) -> MaintenanceStore {
        self.gate = Some(gate);
        self
    }

    /// The directory the store keeps its record in.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Reads the record.
    pub fn load(&self) -> Result<MaintenanceLoaded, MaintenanceRecordError> {
        let bytes = match fs::read(self.dir.join(MAINTENANCE_RECORD_FILE)) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(MaintenanceLoaded::Absent),
            Err(e) => return Err(e.into()),
        };
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(corrupt)?;
        let schema = value.get("schema").and_then(serde_json::Value::as_u64);
        if let Some(schema) = schema.filter(|schema| *schema > u64::from(MAINTENANCE_SCHEMA)) {
            return Ok(MaintenanceLoaded::Newer { schema });
        }
        let record: MaintenanceRecord = serde_json::from_value(value).map_err(corrupt)?;
        record.check().map_err(corrupt)?;
        Ok(MaintenanceLoaded::Current(Box::new(record)))
    }

    /// Writes the record, durably, over an absent record or one that reads. A record that breaks
    /// a rule of [`MaintenanceRecord::check`] is not written ([`MaintenanceRecordError::Invalid`]);
    /// a record on disk of a newer schema ([`MaintenanceRecordError::Newer`]) or one that does not
    /// read ([`MaintenanceRecordError::Corrupt`]) is refused and left as it is.
    pub fn save(&self, record: &MaintenanceRecord) -> Result<(), MaintenanceRecordError> {
        record.check()?;
        if let MaintenanceLoaded::Newer { schema } = self.load()? {
            return Err(MaintenanceRecordError::Newer { schema });
        }
        let n = self.writes.fetch_add(1, Ordering::SeqCst) + 1;
        self.cross(CrashPoint::BeforeWrite(n))?;
        let bytes = serde_json::to_vec_pretty(record).map_err(io::Error::other)?;
        write_atomic(&self.dir, MAINTENANCE_RECORD_FILE, &bytes)?;
        self.cross(CrashPoint::AfterWrite(n))
    }

    /// Sets aside a record that does not read: renames it to [`MAINTENANCE_CORRUPT_FILE`] (an
    /// earlier one is replaced) and fsyncs the directory, so that a new turn can be written and
    /// what broke can still be read. Returns why it did not read. An absent record
    /// ([`MaintenanceRecordError::Absent`]), one that reads ([`MaintenanceRecordError::Reads`])
    /// and one of a newer schema ([`MaintenanceRecordError::Newer`]) are refused and not touched.
    pub fn clear_record(&self) -> Result<String, MaintenanceRecordError> {
        let why = match self.load() {
            Err(MaintenanceRecordError::Corrupt(why)) => why,
            Err(other) => return Err(other),
            Ok(MaintenanceLoaded::Absent) => return Err(MaintenanceRecordError::Absent),
            Ok(MaintenanceLoaded::Current(_)) => return Err(MaintenanceRecordError::Reads),
            Ok(MaintenanceLoaded::Newer { schema }) => {
                return Err(MaintenanceRecordError::Newer { schema });
            }
        };
        fs::rename(
            self.dir.join(MAINTENANCE_RECORD_FILE),
            self.dir.join(MAINTENANCE_CORRUPT_FILE),
        )?;
        File::open(&self.dir)?.sync_all()?;
        Ok(why)
    }

    fn cross(&self, point: CrashPoint) -> Result<(), MaintenanceRecordError> {
        match &self.gate {
            Some(gate) if gate(point) => Err(MaintenanceRecordError::Crashed(point)),
            _ => Ok(()),
        }
    }
}

fn corrupt(error: impl fmt::Display) -> MaintenanceRecordError {
    MaintenanceRecordError::Corrupt(error.to_string())
}

/// A boot id as its text; a record whose boot id is empty does not read.
mod boot_id_text {
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use crate::clock::BootId;

    pub fn serialize<S: Serializer>(id: &Option<BootId>, out: S) -> Result<S::Ok, S::Error> {
        id.as_ref().map(BootId::as_str).serialize(out)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(input: D) -> Result<Option<BootId>, D::Error> {
        Option::<String>::deserialize(input)?
            .map(|text| BootId::new("the maintenance record", &text).map_err(D::Error::custom))
            .transpose()
    }
}

/// Service names as their text; a record naming a service by a name that is not one does not
/// read.
mod service_names {
    use hostkeeper_config::names::ServiceName;
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(names: &[ServiceName], out: S) -> Result<S::Ok, S::Error> {
        out.collect_seq(names.iter().map(ServiceName::as_str))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(input: D) -> Result<Vec<ServiceName>, D::Error> {
        Vec::<String>::deserialize(input)?
            .iter()
            .map(|text| ServiceName::new(text).map_err(D::Error::custom))
            .collect()
    }
}
