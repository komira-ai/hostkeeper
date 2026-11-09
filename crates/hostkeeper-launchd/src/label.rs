//! The reverse-DNS prefix and the launchd labels of the daemon and of each step job.

use std::fmt;
use std::str::FromStr;

use hostkeeper_hook::Step;

/// The reverse-DNS prefix of every label hostkeeper gives a launchd job.
pub const REVERSE_DNS_PREFIX: &str = "dev.hostkeeper";

/// The longest label, in bytes.
pub const MAX_LABEL: usize = 255;

/// The start of the unit name a step runs in, as `hostkeeper_steps::Record::unit` makes it.
const UNIT_PREFIX: &str = "updater-";

/// The word of the unit a `[hook] boot` step runs in, `updater-<service>-boot-<generation>`
/// (the daemon's boot pass): an additive step whose word is none of the steps.
const BOOT_WORD: &str = "boot";

/// A launchd label: 1 to 255 bytes of ASCII letters, digits, `.`, `_` and `-`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Label(String);

impl Label {
    /// Checks `text` and wraps it.
    pub fn parse(text: &str) -> Result<Label, LabelError> {
        let bad = text.bytes().find(|byte| !is_label_byte(*byte));
        match (text.len(), bad) {
            (0, _) => Err(LabelError::Empty),
            (len, _) if len > MAX_LABEL => Err(LabelError::TooLong { len }),
            (_, Some(byte)) => Err(LabelError::BadCharacter {
                label: text.to_owned(),
                byte,
            }),
            (_, None) => Ok(Label(text.to_owned())),
        }
    }

    /// The label text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The label of the daemon's own job: `<prefix>.hostkeeper`.
    pub fn daemon() -> Label {
        Label(format!("{REVERSE_DNS_PREFIX}.hostkeeper"))
    }

    /// The label of the job `step` of `service` runs in during update `generation`:
    /// `<prefix>.hostkeeper.step.<service>.<step>.<generation>`.
    pub fn step(service: &str, step: Step, generation: u64) -> Result<Label, LabelError> {
        Label::parse(&format!(
            "{REVERSE_DNS_PREFIX}.hostkeeper.step.{service}.{}.{generation}",
            step.name()
        ))
    }

    /// The step label of a unit name `updater-<service>-<step>-<generation>`, read from the
    /// right: the generation digits, then the step word, then the service (which may hold
    /// dashes). The word is a step's name, or [`BOOT_WORD`] for the unit of a `[hook] boot`
    /// step, whose label is `<prefix>.hostkeeper.step.<service>.boot.<generation>`.
    pub fn from_unit(unit: &str) -> Result<Label, LabelError> {
        match split_unit(unit) {
            Some((service, word, generation)) => Label::parse(&format!(
                "{REVERSE_DNS_PREFIX}.hostkeeper.step.{service}.{word}.{generation}"
            )),
            None => Err(LabelError::NotAStepUnit {
                unit: unit.to_owned(),
            }),
        }
    }
}

fn is_label_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() | matches!(byte, b'.' | b'_' | b'-')
}

fn split_unit(unit: &str) -> Option<(&str, &str, u64)> {
    let rest = unit.strip_prefix(UNIT_PREFIX)?;
    let (rest, generation) = rest.rsplit_once('-')?;
    let generation = Some(generation)
        .filter(|digits| digits.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|digits| digits.parse::<u64>().ok())?;
    let (service, word) = rest.rsplit_once('-')?;
    let word = Some(word).filter(|word| (*word == BOOT_WORD) | Step::from_str(word).is_ok())?;
    Some((service, word, generation)).filter(|(service, _, _)| !service.is_empty())
}

impl fmt::Display for Label {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why a label could not be made.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LabelError {
    #[error("a launchd label may not be empty")]
    Empty,
    #[error("a launchd label is at most 255 bytes, this one is {len}")]
    TooLong { len: usize },
    #[error("launchd label {label:?} holds the byte 0x{byte:02x}, which a label may not")]
    BadCharacter { label: String, byte: u8 },
    #[error("{unit:?} is not the name of a step unit (updater-<service>-<step>-<generation>)")]
    NotAStepUnit { unit: String },
}

impl LabelError {
    /// The short token that names the refusal.
    pub fn reason(&self) -> &'static str {
        match self {
            LabelError::Empty => "label_empty",
            LabelError::TooLong { .. } => "label_too_long",
            LabelError::BadCharacter { .. } => "label_bad_character",
            LabelError::NotAStepUnit { .. } => "label_not_a_step_unit",
        }
    }
}
