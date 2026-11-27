//! The parameters of the maintenance hook: the fields of a `maintain` order
//! (`hostkeeper.agent.v1.Maintain`) as proto3 canonical JSON, read from a file or from standard
//! input, never from an argument. [`MaintainParams::read`] refuses anything but one valid
//! `Maintain`, and no error it gives holds a byte of what it read. A field this release does not
//! know is passed over, as a protobuf reader passes it over, so that parameters a newer daemon
//! wrote are read by an older hook.

use std::collections::BTreeSet;
use std::fmt;
use std::io::Read;

use hostkeeper_proto::FILE_DESCRIPTOR_SET;
use hostkeeper_proto::hostkeeper::agent::v1::{Maintain, MaintenanceKind};
use hostkeeper_proto::maintenance::{MAX_APPLIED, MAX_RUN_ID};
use prost_reflect::{DescriptorPool, DeserializeOptions, DynamicMessage};

/// The message the parameters are the JSON of.
const MAINTAIN_MESSAGE: &str = "hostkeeper.agent.v1.Maintain";

/// The most bytes of the parameters: far more than a `Maintain` at every bound.
pub const MAX_PARAMS_BYTES: u64 = 64 * 1024;

/// A kind of maintenance the hook applies, in the order a cutover applies them: macOS before
/// Xcode (a new Xcode can need a newer macOS), then Linux, then a reboot alone; a probe, which the
/// coordinator orders only alone, last.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Macos,
    Xcode,
    Linux,
    Reboot,
    Probe,
}

impl Kind {
    /// Every kind, in the order a cutover applies them.
    pub const ALL: [Kind; 5] = [
        Kind::Macos,
        Kind::Xcode,
        Kind::Linux,
        Kind::Reboot,
        Kind::Probe,
    ];

    /// The kind's word, as the design and the report name it.
    pub const fn name(self) -> &'static str {
        match self {
            Kind::Macos => "macos",
            Kind::Xcode => "xcode",
            Kind::Linux => "linux",
            Kind::Reboot => "reboot",
            Kind::Probe => "probe",
        }
    }

    /// The kind of a wire value; `None` for `UNSPECIFIED` and for a value this release does not
    /// know.
    pub fn from_wire(value: i32) -> Option<Kind> {
        match MaintenanceKind::try_from(value).ok()? {
            MaintenanceKind::Unspecified => None,
            MaintenanceKind::Macos => Some(Kind::Macos),
            MaintenanceKind::Xcode => Some(Kind::Xcode),
            MaintenanceKind::Linux => Some(Kind::Linux),
            MaintenanceKind::Reboot => Some(Kind::Reboot),
            MaintenanceKind::Probe => Some(Kind::Probe),
            // The coordinator's repair cordon, which no daemon of this release takes.
            MaintenanceKind::Repair => None,
        }
    }

    /// The kind's wire value.
    pub const fn wire(self) -> MaintenanceKind {
        match self {
            Kind::Macos => MaintenanceKind::Macos,
            Kind::Xcode => MaintenanceKind::Xcode,
            Kind::Linux => MaintenanceKind::Linux,
            Kind::Reboot => MaintenanceKind::Reboot,
            Kind::Probe => MaintenanceKind::Probe,
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Why the parameters were refused. No variant holds a byte of what was read: a field is named,
/// never its value.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ParamsError {
    /// The file or standard input could not be read.
    #[error("the parameters could not be read: {0}")]
    Read(String),
    /// More than [`MAX_PARAMS_BYTES`].
    #[error("the parameters are more than {MAX_PARAMS_BYTES} bytes")]
    TooLarge,
    /// Not the proto3 JSON of a `Maintain`.
    #[error("the parameters are not the JSON of a {MAINTAIN_MESSAGE}")]
    NotMaintain,
    /// `run_id` is empty or longer than [`MAX_RUN_ID`] bytes.
    #[error("run_id is not 1 to {MAX_RUN_ID} bytes")]
    RunId,
    /// `kinds` is empty or holds more than [`MAX_APPLIED`] entries.
    #[error("kinds holds {0} entries, not 1 to {MAX_APPLIED}")]
    KindCount(usize),
    /// An entry of `kinds` is `UNSPECIFIED` or a value this release does not know.
    #[error("kinds holds a kind this hook does not know")]
    UnknownKind,
    /// An entry of `kinds` is given twice.
    #[error("kinds holds {0} twice")]
    DuplicateKind(Kind),
}

/// The parameters of one run of the hook: a `Maintain` whose run id and kinds are valid.
#[derive(Clone, Debug, PartialEq)]
pub struct MaintainParams {
    maintain: Maintain,
    kinds: BTreeSet<Kind>,
}

impl MaintainParams {
    /// Reads and checks the parameters `source` holds, at most [`MAX_PARAMS_BYTES`].
    pub fn read(source: &mut dyn Read) -> Result<MaintainParams, ParamsError> {
        let mut bytes = Vec::new();
        source
            .take(MAX_PARAMS_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| ParamsError::Read(error.to_string()))?;
        if bytes.len() as u64 > MAX_PARAMS_BYTES {
            return Err(ParamsError::TooLarge);
        }
        MaintainParams::new(decode(&bytes)?)
    }

    /// Checks `maintain`: a run id of 1 to [`MAX_RUN_ID`] bytes, and 1 to [`MAX_APPLIED`] kinds,
    /// each known and given once.
    pub fn new(maintain: Maintain) -> Result<MaintainParams, ParamsError> {
        if !(1..=MAX_RUN_ID).contains(&maintain.run_id.len()) {
            return Err(ParamsError::RunId);
        }
        if !(1..=MAX_APPLIED).contains(&maintain.kinds.len()) {
            return Err(ParamsError::KindCount(maintain.kinds.len()));
        }
        let mut kinds = BTreeSet::new();
        for value in &maintain.kinds {
            let kind = Kind::from_wire(*value).ok_or(ParamsError::UnknownKind)?;
            if !kinds.insert(kind) {
                return Err(ParamsError::DuplicateKind(kind));
            }
        }
        Ok(MaintainParams { maintain, kinds })
    }

    /// The run's id.
    pub fn run_id(&self) -> &str {
        &self.maintain.run_id
    }

    /// The kinds of the run, in the order a cutover applies them ([`Kind::ALL`]), whatever order
    /// the order gave them in.
    pub fn kinds(&self) -> impl Iterator<Item = Kind> + '_ {
        self.kinds.iter().copied()
    }

    /// The whole `Maintain`, for the kinds' own fields (the Linux scope, the macOS path and whether
    /// a major upgrade is allowed, the Xcodes).
    pub fn maintain(&self) -> &Maintain {
        &self.maintain
    }
}

/// The `Maintain` `bytes` hold as proto3 canonical JSON. The decoder's own message is dropped: it
/// can quote what it read.
fn decode(bytes: &[u8]) -> Result<Maintain, ParamsError> {
    let decoded = || {
        let descriptor = DescriptorPool::decode(FILE_DESCRIPTOR_SET)
            .ok()?
            .get_message_by_name(MAINTAIN_MESSAGE)?;
        let mut reader = serde_json::Deserializer::from_slice(bytes);
        let options = DeserializeOptions::new().deny_unknown_fields(false);
        let message =
            DynamicMessage::deserialize_with_options(descriptor, &mut reader, &options).ok()?;
        reader.end().ok()?;
        message.transcode_to::<Maintain>().ok()
    };
    decoded().ok_or(ParamsError::NotMaintain)
}
