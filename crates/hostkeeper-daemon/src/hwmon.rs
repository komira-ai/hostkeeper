//! The hwmon temperatures of a Linux host against their driver's own critical threshold
//! (docs/design/host-health-alerts.md sections 3.1 and 4.5): the source `hwmon`, read every
//! [`HWMON_EVERY_SECS`] from [`HWMON_DIR`] under the host's root. It only reads files.
//!
//! **The sensors** are the `temp<n>_input` (`n` from 1 to [`MAX_SENSOR_INDEX`]) of the chips whose
//! `name` is one of [`DRIVERS`] (`coretemp`, `k10temp`, `nvme`), that have a `temp<n>_crit` of 1 to
//! 200 °C; a sensor whose `crit` is missing or outside that range has no threshold to be judged
//! by and is passed over, and one whose `crit` cannot be read or does not parse could not be
//! read. At most [`MAX_CHIPS`] chips are looked at, in the order of their
//! directories' names. Each file is read to at most [`MAX_FILE_BYTES`] bytes; a value is
//! millidegrees Celsius, and an input outside -40 to 200 °C, or one that does not parse, is a
//! sensor that could not be read.
//!
//! A sensor's `subject` is `<chip>/<label>`: the chip its driver's name, or, when two chips of a
//! host share one (two NVMe disks), the name of its `device` (`nvme0`), else its directory's; the
//! label its `temp<n>_label`, else `temp<n>`. It is cut to 32 bytes.
//!
//! **What is judged**: a sensor at or over its `crit` gives an `asserted` record
//! `thermal_critical`, critical (component `thermal`); it ends, with a `deasserted` record about
//! the assertion, critical too (so a queue that folds `info` records never loses it), once the
//! sensor reads at or under its `crit_hyst` when the driver gives one under `crit`, else 5 °C
//! under `crit`, or once it is no longer listed by a read that listed every chip (one that lists
//! no sensor at all, [`HWMON_DIR`] absent included, ends every assertion it holds). The mark
//! ([`HwmonMark`], JSON) keeps the sensors over their threshold with their assertions' ids, so a
//! sensor that stays hot makes one record. A record's `event_id` is `hwmon:<subject>:hot:<second>`
//! or `hwmon:<subject>:cool:<second>`; its `raw` the files' values.
//!
//! **The source's state**: `unsupported` when [`HWMON_DIR`] is absent or lists no sensor to judge
//! (the mark kept as it was unless that read ended an assertion);
//! `unreadable` when it cannot be listed, when more than [`MAX_CHIPS`] chips are listed, or when a
//! sensor to judge could not be read (the reason names the first); else `ok`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use hostkeeper_proto::health::{MAX_RAW, MAX_SUBJECT};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use serde::{Deserialize, Serialize};

use crate::hardware_sources::{HardwareSource, SourceRead};
use crate::health_state::cut;

/// The source's token.
pub const SOURCE: &str = "hwmon";

/// The hwmon chips, under the host's root.
pub const HWMON_DIR: &str = "sys/class/hwmon";

/// The seconds between two reads.
pub const HWMON_EVERY_SECS: u64 = 60;

/// The drivers whose temperatures are judged.
pub const DRIVERS: [&str; 3] = ["coretemp", "k10temp", "nvme"];

/// The most chips one read looks at.
pub const MAX_CHIPS: usize = 64;

/// The highest `n` of a `temp<n>_input` looked at.
pub const MAX_SENSOR_INDEX: u32 = 64;

/// The most bytes read of one file.
pub const MAX_FILE_BYTES: u64 = 64;

/// The lowest input taken as a reading, millidegrees Celsius.
pub const MIN_MILLI: i64 = -40_000;

/// The highest input, and the highest `crit`, taken as a reading, millidegrees Celsius.
pub const MAX_MILLI: i64 = 200_000;

/// How far under `crit` a sensor must read to end its assertion when its driver gives no
/// `crit_hyst`, millidegrees Celsius.
pub const CLEAR_MARGIN_MILLI: i64 = 5_000;

/// The code of a sensor's records.
pub const CODE_THERMAL: &str = "thermal_critical";

/// The reason of a host with no sensor to judge.
pub const NO_SENSOR: &str = "no coretemp, k10temp or nvme sensor with a critical threshold";

/// What the source keeps between reads: the sensors over their threshold, by subject, with the
/// `event_id` of each one's assertion.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HwmonMark {
    pub hot: BTreeMap<String, String>,
}

/// One sensor as read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sensor {
    /// `<chip>/<label>`, cut to its bound.
    pub subject: String,
    /// Its reading, millidegrees Celsius.
    pub input: i64,
    /// Its driver's critical threshold, millidegrees Celsius.
    pub crit: i64,
    /// The reading at or under which its assertion ends, millidegrees Celsius.
    pub clear_at: i64,
    /// The files' values, as one line.
    pub raw: String,
}

/// What one listing of the chips found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listing {
    /// The sensors read.
    pub sensors: Vec<Sensor>,
    /// The subjects of every sensor to judge, read or not.
    pub listed: BTreeSet<String>,
    /// Why the first sensor that could not be read was not, `<subject>: <why>`.
    pub problem: Option<String>,
    /// More chips than [`MAX_CHIPS`] were listed: how many.
    pub too_many: Option<usize>,
}

/// One chip of a driver of [`DRIVERS`].
struct Chip {
    dir: PathBuf,
    driver: String,
    /// Its `device`'s name, else its directory's.
    device: String,
}

/// `path` read to [`MAX_FILE_BYTES`] bytes, trimmed.
fn read_small(path: &Path) -> io::Result<String> {
    let mut text = String::new();
    fs::File::open(path)?
        .take(MAX_FILE_BYTES)
        .read_to_string(&mut text)?;
    Ok(text.trim().to_owned())
}

/// The number in `path`.
fn read_value(path: &Path) -> Result<i64, String> {
    let text = read_small(path).map_err(|e| e.to_string())?;
    text.parse()
        .map_err(|_| format!("{text:?} is not a number"))
}

/// A millidegree value of `path`, within `min..=max`.
fn read_milli(path: &Path, min: i64, max: i64) -> Result<i64, String> {
    let value = read_value(path)?;
    if (min..=max).contains(&value) {
        Ok(value)
    } else {
        Err(format!("{value} is outside {min} to {max}"))
    }
}

/// Lists the chips and reads their sensors, under `root` (the module's doc).
///
/// # Errors
/// [`HWMON_DIR`] cannot be listed; `NotFound` when it is absent.
pub fn list(root: &Path) -> io::Result<Listing> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(root.join(HWMON_DIR))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    dirs.sort();
    let mut listing = Listing::default();
    if dirs.len() > MAX_CHIPS {
        listing.too_many = Some(dirs.len());
    }
    let chips: Vec<Chip> = dirs
        .into_iter()
        .take(MAX_CHIPS)
        .filter_map(|dir| {
            let driver = read_small(&dir.join("name")).ok()?;
            if !DRIVERS.contains(&driver.as_str()) {
                return None;
            }
            let device = fs::read_link(dir.join("device"))
                .ok()
                .and_then(|link| link.file_name().map(|n| n.to_string_lossy().into_owned()))
                .or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_default();
            Some(Chip {
                dir,
                driver,
                device,
            })
        })
        .collect();
    for chip in &chips {
        let shared = chips.iter().filter(|c| c.driver == chip.driver).count() > 1;
        let name = if shared { &chip.device } else { &chip.driver };
        for n in 1..=MAX_SENSOR_INDEX {
            read_sensor(chip, name, n, &mut listing);
        }
    }
    Ok(listing)
}

/// Reads `temp<n>` of `chip`, named `name`, into `listing` when it is a sensor to judge.
fn read_sensor(chip: &Chip, name: &str, n: u32, listing: &mut Listing) {
    let file = |what: &str| chip.dir.join(format!("temp{n}_{what}"));
    if !file("input").exists() || !file("crit").exists() {
        return;
    }
    let crit = read_value(&file("crit"));
    if crit
        .as_ref()
        .is_ok_and(|crit| !(1..=MAX_MILLI).contains(crit))
    {
        return;
    }
    let label = read_small(&file("label"))
        .ok()
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| format!("temp{n}"));
    let subject = cut(&format!("{name}/{label}"), MAX_SUBJECT);
    listing.listed.insert(subject.clone());
    let read = crit
        .map_err(|why| format!("temp{n}_crit: {why}"))
        .and_then(|crit| Ok((crit, read_milli(&file("input"), MIN_MILLI, MAX_MILLI)?)));
    let (crit, input) = match read {
        Ok(both) => both,
        Err(why) => {
            listing
                .problem
                .get_or_insert_with(|| format!("{subject}: {why}"));
            return;
        }
    };
    let clear_at =
        read_milli(&file("crit_hyst"), MIN_MILLI, crit - 1).unwrap_or(crit - CLEAR_MARGIN_MILLI);
    let raw = cut(
        &format!(
            "{} {}: temp{n}_input={input} temp{n}_crit={crit} clear_at={clear_at}",
            chip.dir
                .file_name()
                .map_or_else(String::new, |d| d.to_string_lossy().into_owned()),
            chip.driver,
        ),
        MAX_RAW,
    );
    listing.sensors.push(Sensor {
        subject,
        input,
        crit,
        clear_at,
        raw,
    });
}

/// The whole degrees of `milli` millidegrees.
fn degrees(milli: i64) -> i64 {
    milli / 1000
}

/// A record of `subject`, at `now`, critical, of the code [`CODE_THERMAL`].
fn record(
    event_id: String,
    subject: &str,
    now: u64,
    state: pb::HardwareEventState,
    message: String,
    raw: &str,
) -> pb::HardwareEvent {
    pb::HardwareEvent {
        event_id,
        time_unix: now,
        of_previous_boot: false,
        component: pb::HardwareComponent::Thermal.into(),
        subject: subject.to_owned(),
        severity: pb::HardwareSeverity::Critical.into(),
        state: state.into(),
        source: SOURCE.to_owned(),
        code: CODE_THERMAL.to_owned(),
        message,
        raw: raw.to_owned(),
        about: String::new(),
    }
}

/// Judges `listing` against `mark` at `now`: the records, with `mark` changed to match.
#[must_use]
pub fn judge(listing: &Listing, mark: &mut HwmonMark, now: u64) -> Vec<pb::HardwareEvent> {
    let mut events = Vec::new();
    for sensor in &listing.sensors {
        let subject = sensor.subject.as_str();
        let (t, c) = (degrees(sensor.input), degrees(sensor.crit));
        match mark.hot.get(subject).cloned() {
            None if sensor.input >= sensor.crit => {
                let id = format!("{SOURCE}:{subject}:hot:{now}");
                mark.hot.insert(subject.to_owned(), id.clone());
                events.push(record(
                    id,
                    subject,
                    now,
                    pb::HardwareEventState::Asserted,
                    format!("Sensor {subject} at {t} C, at or over its critical {c} C"),
                    &sensor.raw,
                ));
            }
            Some(assertion) if sensor.input <= sensor.clear_at => {
                mark.hot.remove(subject);
                events.push(pb::HardwareEvent {
                    about: assertion,
                    ..record(
                        format!("{SOURCE}:{subject}:cool:{now}"),
                        subject,
                        now,
                        pb::HardwareEventState::Deasserted,
                        format!("Sensor {subject} back to {t} C, under its critical {c} C"),
                        &sensor.raw,
                    )
                });
            }
            _ => {}
        }
    }
    if listing.too_many.is_none() {
        let gone: Vec<(String, String)> = mark
            .hot
            .iter()
            .filter(|(subject, _)| !listing.listed.contains(*subject))
            .map(|(s, id)| (s.clone(), id.clone()))
            .collect();
        for (subject, assertion) in gone {
            mark.hot.remove(&subject);
            events.push(pb::HardwareEvent {
                about: assertion,
                ..record(
                    format!("{SOURCE}:{subject}:cool:{now}"),
                    &subject,
                    now,
                    pb::HardwareEventState::Deasserted,
                    format!("Sensor {subject} is no longer listed"),
                    "",
                )
            });
        }
    }
    events
}

/// The hwmon temperatures under a host's root.
pub struct HwmonSource {
    root: PathBuf,
}

impl HwmonSource {
    /// The source of the chips under `root`.
    #[must_use]
    pub fn new(root: &Path) -> HwmonSource {
        HwmonSource {
            root: root.to_path_buf(),
        }
    }
}

impl HardwareSource for HwmonSource {
    fn name(&self) -> &'static str {
        SOURCE
    }

    fn every_secs(&self) -> u64 {
        HWMON_EVERY_SECS
    }

    fn read(&mut self, mark: Option<&str>, now_unix: u64) -> SourceRead {
        let (listing, absent) = match list(&self.root) {
            Ok(listing) => (listing, None),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                (Listing::default(), Some(format!("{HWMON_DIR} is absent")))
            }
            Err(e) => return SourceRead::unreadable(format!("{HWMON_DIR} is not listed: {e}")),
        };
        let mut kept: HwmonMark = mark
            .and_then(|text| serde_json::from_str(text).ok())
            .unwrap_or_default();
        let events = judge(&listing, &mut kept, now_unix);
        let none = listing.listed.is_empty() && listing.too_many.is_none();
        if none && events.is_empty() {
            return SourceRead::unsupported(absent.unwrap_or_else(|| NO_SENSOR.to_owned()));
        }
        let text = serde_json::to_string(&kept).unwrap_or_default();
        let (status, reason) = match (listing.too_many, listing.problem) {
            (Some(count), _) => (
                pb::SourceStatus::Unreadable,
                format!("{count} chips listed, the first {MAX_CHIPS} read"),
            ),
            (None, Some(why)) => (pb::SourceStatus::Unreadable, why),
            (None, None) if none => (
                pb::SourceStatus::Unsupported,
                absent.unwrap_or_else(|| NO_SENSOR.to_owned()),
            ),
            (None, None) => (pb::SourceStatus::Ok, String::new()),
        };
        SourceRead {
            status,
            reason,
            mark: Some(text),
            events,
        }
    }
}
