//! A Mac's thermal pressure (docs/design/host-health-alerts.md sections 3.2, 4.2 and 4.5): the
//! source [`SOURCE`], one sample every [`THERMAL_EVERY_SECS`] by [`ThermalSource`] on a worker of
//! its own ([`crate::hardware_sources`]), judged by the pure rule [`judge`].
//!
//! **A sample.** [`PMSET`] `-g therm` ([`PMSET_ARGS`], no root needed) says whether macOS limits
//! the machine for heat ([`parse_pmset`]): a thermal or performance warning level above 0, or a
//! CPU speed or scheduler limit under 100. A machine it does not limit is at `nominal`, and nothing
//! else is run. One it limits is measured by [`POWERMETRICS`] `--samplers thermal -n 1`
//! ([`POWERMETRICS_ARGS`]), whose `Current pressure level` is the sample ([`parse_powermetrics`]);
//! `powermetrics` runs at most once every [`POWERMETRICS_EVERY_SECS`], and a sample within that
//! time of its last run takes what that run gave: its level, or, when it failed, its reason (so
//! a `powermetrics` that fails is not run again every minute). Each program runs through the [`Launchctl`] port, with
//! a fixed argument list, an empty environment, its output capped, killed at
//! [`crate::mac::TOOL_DEADLINE`].
//!
//! **The rule** ([`judge`], over the mark [`ThermalMark`], JSON):
//!
//! - [`HEAVY_SAMPLES`] samples in a row at `heavy` or worse give an `asserted` record
//!   [`CODE_PRESSURE`] (component `thermal`, subject [`SUBJECT`]): a warning, or critical when one
//!   of them was at `trapping` or `sleeping`. A `moderate` or `nominal` sample starts the count
//!   again.
//! - While a warning is asserted, a sample at `trapping` or worse gives a second `asserted` record,
//!   critical, about the first, which it stands for from then on.
//! - Samples at `nominal` for [`NOMINAL_SECS`] (from the first of them to the one being judged)
//!   end the assertion with a `deasserted` record about it, at its severity, so a queue that folds
//!   `info` records never loses it. A sample that is not `nominal` starts that time again.
//!
//! A record's `event_id` is `thermal:<level>:<second>` (the level of the sample that asserted)
//! or `thermal:nominal:<second>`; its `raw` what the programs printed of it, as one line.
//!
//! **The source's state**: `ok` when it sampled; `unreadable`, with its reason and the mark kept
//! (no sample: the counts go on from where they were), when `pmset` or `powermetrics` does not run
//! or exit 0, or prints nothing this module reads.

use std::path::Path;

use hostkeeper_launchd::{Launchctl, LaunchctlProcess};
use hostkeeper_proto::health::{MAX_EVENT_ID, MAX_MESSAGE, MAX_RAW};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use serde::{Deserialize, Serialize};

use crate::hardware_sources::{HardwareSource, SourceRead};
use crate::health_state::cut;
use crate::mac::TOOL_DEADLINE;
use crate::mac_health::run_text;

/// The source's token.
pub const SOURCE: &str = "thermal";

/// The program that says whether macOS limits the machine for heat.
pub const PMSET: &str = "/usr/bin/pmset";

/// Its arguments.
pub const PMSET_ARGS: [&str; 2] = ["-g", "therm"];

/// The program that measures the thermal pressure.
pub const POWERMETRICS: &str = "/usr/bin/powermetrics";

/// Its arguments: the thermal sampler, one sample.
pub const POWERMETRICS_ARGS: [&str; 4] = ["--samplers", "thermal", "-n", "1"];

/// The seconds between two samples.
pub const THERMAL_EVERY_SECS: u64 = 60;

/// The fewest seconds between two runs of [`POWERMETRICS`].
pub const POWERMETRICS_EVERY_SECS: u64 = 5 * 60;

/// How many samples in a row at `heavy` or worse assert the condition.
pub const HEAVY_SAMPLES: u32 = 5;

/// How long the samples must stay `nominal` to end it, seconds.
pub const NOMINAL_SECS: u64 = 10 * 60;

/// The code of the source's records.
pub const CODE_PRESSURE: &str = "thermal_pressure";

/// The subject of the source's records: the whole machine, which macOS limits as one.
pub const SUBJECT: &str = "thermal pressure";

/// The start of the line of [`POWERMETRICS`] that gives the level.
pub const PRESSURE_LINE: &str = "Current pressure level:";

/// A thermal pressure level, as macOS names them, the least first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pressure {
    Nominal,
    Moderate,
    Heavy,
    Trapping,
    Sleeping,
}

impl Pressure {
    /// The level `word` names, in any case; none for a word this release does not know.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Pressure> {
        match word.to_ascii_lowercase().as_str() {
            "nominal" => Some(Pressure::Nominal),
            "moderate" => Some(Pressure::Moderate),
            "heavy" => Some(Pressure::Heavy),
            "trapping" => Some(Pressure::Trapping),
            "sleeping" => Some(Pressure::Sleeping),
            _ => None,
        }
    }

    /// Its word, lower case.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Pressure::Nominal => "nominal",
            Pressure::Moderate => "moderate",
            Pressure::Heavy => "heavy",
            Pressure::Trapping => "trapping",
            Pressure::Sleeping => "sleeping",
        }
    }

    /// The severity of a condition at this level: critical at `trapping` and `sleeping`.
    fn severity(self) -> pb::HardwareSeverity {
        if self >= Pressure::Trapping {
            pb::HardwareSeverity::Critical
        } else {
            pb::HardwareSeverity::Warning
        }
    }
}

/// What `pmset -g therm` says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Therm {
    /// Whether macOS limits the machine for heat.
    pub limited: bool,
    /// The lines read, trimmed and joined by `; `.
    pub lines: String,
}

/// A number `pmset -g therm` prints.
#[derive(Clone, Copy)]
enum Gauge {
    /// A thermal or performance warning level: 0 when macOS warns of nothing.
    WarningLevel,
    /// A CPU speed or scheduler limit, percent: 100 when macOS limits nothing.
    Limit,
}

impl Gauge {
    /// Whether `value` of this gauge says macOS limits the machine.
    fn limits(self, value: u64) -> bool {
        match self {
            Gauge::WarningLevel => value >= 1,
            Gauge::Limit => value < 100,
        }
    }
}

/// Reads what `pmset -g therm` printed: the lines `<Thermal|Performance> warning level set to
/// <n>.` (a level above 0 limits), `CPU_Speed_Limit = <n>` and `CPU_Scheduler_Limit = <n>` (under
/// 100 limits), and the notes that no warning level has been recorded.
///
/// # Errors
/// Why it gives no answer: none of those lines, or one whose number does not read.
pub fn parse_pmset(text: &str) -> Result<Therm, String> {
    let mut limited = false;
    let mut lines = Vec::new();
    for line in text.lines().map(str::trim) {
        let level = line
            .strip_suffix('.')
            .and_then(|rest| rest.split_once(" warning level set to "))
            .map(|(_, number)| (number, Gauge::WarningLevel));
        let limit = line
            .split_once('=')
            .filter(|(key, _)| matches!(key.trim(), "CPU_Speed_Limit" | "CPU_Scheduler_Limit"))
            .map(|(_, number)| (number, Gauge::Limit));
        let noted = line.starts_with("Note: No ") & line.contains(" warning level ");
        if let Some((number, gauge)) = level.or(limit) {
            let value: u64 = number
                .trim()
                .parse()
                .map_err(|_| format!("pmset printed {line:?}, which has no number"))?;
            limited |= gauge.limits(value);
        } else if !noted {
            continue;
        }
        lines.push(line.to_owned());
    }
    if lines.is_empty() {
        return Err("pmset printed no thermal state".to_owned());
    }
    Ok(Therm {
        limited,
        lines: lines.join("; "),
    })
}

/// Reads the level of what `powermetrics --samplers thermal` printed: its line
/// [`PRESSURE_LINE`].
///
/// # Errors
/// Why it gives no level: no such line, or a word this release does not know.
pub fn parse_powermetrics(text: &str) -> Result<Pressure, String> {
    let word = text
        .lines()
        .find_map(|line| line.trim().strip_prefix(PRESSURE_LINE))
        .map(str::trim)
        .ok_or_else(|| "powermetrics printed no pressure level".to_owned())?;
    Pressure::from_word(word).ok_or_else(|| format!("powermetrics printed the level {word:?}"))
}

/// The open condition.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Assertion {
    /// Its last `asserted` record's `event_id`.
    pub event_id: String,
    /// Whether it is critical.
    pub critical: bool,
}

/// What the rule keeps between samples.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThermalMark {
    /// The samples in a row at `heavy` or worse.
    pub heavy: u32,
    /// The worst of them; none when there are none.
    pub worst: Option<Pressure>,
    /// When the samples in a row at `nominal` began; none when the last sample was not.
    pub nominal_since: Option<u64>,
    /// The open condition; none when none is.
    pub asserted: Option<Assertion>,
}

/// Judges the sample `level`, taken at `now` and printed as `raw`, against `mark` (the module's
/// doc): the records, with `mark` moved on.
#[must_use]
pub fn judge(
    mark: &mut ThermalMark,
    level: Pressure,
    raw: &str,
    now: u64,
) -> Vec<pb::HardwareEvent> {
    let mut events = Vec::new();
    if level == Pressure::Nominal {
        mark.heavy = 0;
        mark.worst = None;
        let since = *mark.nominal_since.get_or_insert(now);
        if now.saturating_sub(since) >= NOMINAL_SECS
            && let Some(open) = mark.asserted.take()
        {
            let severity = if open.critical {
                pb::HardwareSeverity::Critical
            } else {
                pb::HardwareSeverity::Warning
            };
            events.push(record(
                format!("{SOURCE}:nominal:{now}"),
                now,
                severity,
                pb::HardwareEventState::Deasserted,
                format!(
                    "Thermal pressure back to nominal for {} minutes",
                    NOMINAL_SECS / 60
                ),
                raw,
                open.event_id,
            ));
        }
        return events;
    }
    mark.nominal_since = None;
    if level < Pressure::Heavy {
        mark.heavy = 0;
        mark.worst = None;
        return events;
    }
    mark.heavy = mark.heavy.saturating_add(1);
    let worst = mark.worst.map_or(level, |worst| worst.max(level));
    mark.worst = Some(worst);
    match mark.asserted.clone() {
        None if mark.heavy >= HEAVY_SAMPLES => {
            let id = cut(&format!("{SOURCE}:{}:{now}", level.word()), MAX_EVENT_ID);
            events.push(record(
                id.clone(),
                now,
                worst.severity(),
                pb::HardwareEventState::Asserted,
                format!(
                    "Thermal pressure {} or worse for {} samples in a row, at worst {}",
                    Pressure::Heavy.word(),
                    mark.heavy,
                    worst.word()
                ),
                raw,
                String::new(),
            ));
            mark.asserted = Some(Assertion {
                event_id: id,
                critical: worst >= Pressure::Trapping,
            });
        }
        Some(open) if !open.critical && level >= Pressure::Trapping => {
            let id = cut(&format!("{SOURCE}:{}:{now}", level.word()), MAX_EVENT_ID);
            events.push(record(
                id.clone(),
                now,
                pb::HardwareSeverity::Critical,
                pb::HardwareEventState::Asserted,
                format!("Thermal pressure now {}", level.word()),
                raw,
                open.event_id,
            ));
            mark.asserted = Some(Assertion {
                event_id: id,
                critical: true,
            });
        }
        _ => {}
    }
    events
}

/// One record of the thermal pressure.
fn record(
    event_id: String,
    now: u64,
    severity: pb::HardwareSeverity,
    state: pb::HardwareEventState,
    message: String,
    raw: &str,
    about: String,
) -> pb::HardwareEvent {
    pb::HardwareEvent {
        event_id: cut(&event_id, MAX_EVENT_ID),
        time_unix: now,
        of_previous_boot: false,
        component: pb::HardwareComponent::Thermal.into(),
        subject: SUBJECT.to_owned(),
        severity: severity.into(),
        state: state.into(),
        source: SOURCE.to_owned(),
        code: CODE_PRESSURE.to_owned(),
        message: cut(&message, MAX_MESSAGE),
        raw: cut(raw, MAX_RAW),
        about,
    }
}

/// The thermal pressure, through `pmset` and `powermetrics`.
pub struct ThermalSource {
    pmset: Box<dyn Launchctl>,
    powermetrics: Box<dyn Launchctl>,
    /// When `powermetrics` last ran, and the level it gave or why it gave none; none before it
    /// ran.
    measured: Option<(u64, Result<Pressure, String>)>,
}

impl ThermalSource {
    /// The pressure `pmset` and `powermetrics` (the programs [`PMSET_ARGS`] and
    /// [`POWERMETRICS_ARGS`] are given to) print.
    #[must_use]
    pub fn new(pmset: Box<dyn Launchctl>, powermetrics: Box<dyn Launchctl>) -> ThermalSource {
        ThermalSource {
            pmset,
            powermetrics,
            measured: None,
        }
    }

    /// The host's: [`PMSET`] and [`POWERMETRICS`], each killed at [`TOOL_DEADLINE`].
    #[must_use]
    pub fn at_host() -> ThermalSource {
        ThermalSource::new(
            Box::new(LaunchctlProcess::new(Path::new(PMSET), TOOL_DEADLINE)),
            Box::new(LaunchctlProcess::new(
                Path::new(POWERMETRICS),
                TOOL_DEADLINE,
            )),
        )
    }

    /// The sample at `now` and its `raw` line, or why there is none.
    fn sample(&mut self, now: u64) -> Result<(Pressure, String), String> {
        let therm = parse_pmset(&run_text(self.pmset.as_ref(), &PMSET_ARGS, "pmset")?)?;
        if !therm.limited {
            return Ok((Pressure::Nominal, therm.lines));
        }
        let recent = self
            .measured
            .as_ref()
            .filter(|(at, _)| {
                now.checked_sub(*at)
                    .is_some_and(|age| age < POWERMETRICS_EVERY_SECS)
            })
            .map(|(_, measured)| measured.clone());
        let level = match recent {
            Some(measured) => measured?,
            None => {
                let measured = run_text(
                    self.powermetrics.as_ref(),
                    &POWERMETRICS_ARGS,
                    "powermetrics",
                )
                .and_then(|text| parse_powermetrics(&text));
                self.measured = Some((now, measured.clone()));
                measured?
            }
        };
        Ok((
            level,
            format!("{}; {PRESSURE_LINE} {}", therm.lines, level.word()),
        ))
    }
}

impl HardwareSource for ThermalSource {
    fn name(&self) -> &'static str {
        SOURCE
    }

    fn every_secs(&self) -> u64 {
        THERMAL_EVERY_SECS
    }

    fn read(&mut self, mark: Option<&str>, now_unix: u64) -> SourceRead {
        let (level, raw) = match self.sample(now_unix) {
            Ok(sample) => sample,
            Err(why) => return SourceRead::unreadable(why),
        };
        let before: ThermalMark = mark
            .and_then(|text| serde_json::from_str(text).ok())
            .unwrap_or_default();
        let mut kept = before.clone();
        let events = judge(&mut kept, level, &raw, now_unix);
        SourceRead {
            status: pb::SourceStatus::Ok,
            reason: String::new(),
            mark: (kept != before).then(|| serde_json::to_string(&kept).unwrap_or_default()),
            events,
        }
    }
}
