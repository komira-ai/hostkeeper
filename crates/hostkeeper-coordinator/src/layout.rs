//! The lab's layout, which the web page's Home draws (`docs/design/ui-shell-lab.md`): the one
//! cluster's name, its rows (numbered 0, 1, 2, ...), the racks that stand in them (numbered within
//! their row) and the placements of servers in the racks. A hostkeeper runs one lab only.
//!
//! The layout is a part of the store of its own, beside the desired state, the operations state,
//! the verdicts and the accounts, and not part of the inventory: where a server stands is not a
//! field of its record. It is written whole, when a commit changes it: `<state-file>.layout` on
//! the file store ([`encode_file`], [`parse_file`]), the tables `lab_rows`, `lab_racks` and
//! `lab_placements` (migration `0015`, `may-run`) on Postgres. An older coordinator never reads
//! either, and a coordinator that finds neither starts with an empty layout, which is the state it
//! starts in.
//!
//! The file is JSON, `{"name": text, "rows": {id: record}, "racks": {id: record}, "placements":
//! {subject: record}}`, each part left out when empty, and is read as the accounts file is: a key it does
//! not know is skipped, so a later build may add one, and a value that breaks a rule of
//! [`Layout::new`] stops the start, naming the file.
//!
//! Every change is made on a copy and judged whole by [`Layout::new`], so a rule is stated once:
//!
//! - the cluster's name is empty or 1 to [`MAX_NAME_CHARS`] characters of letters, digits, space,
//!   `.`, `_` and `-`, none of them leading or trailing space; row numbers differ and so do the
//!   rack numbers of one row, each from 0;
//! - a rack is 1 to [`MAX_HEIGHT_U`] U high and stands in a row the layout holds;
//! - a placement is in a rack the layout holds, occupies U `u` to `u + height_u - 1`
//!   within U 1 to the rack's height, and shares no U with another placement of the rack; a
//!   subject has at most one placement; its lowest U is `u`, its height `height_u`, both from 1;
//! - at most [`MAX_ROWS`] rows, [`MAX_RACKS`] racks and [`MAX_PLACEMENTS`] placements.
//!
//! [`LayoutStore`] is the layout and the backend a change of it is committed to.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};

use crate::desired::StoreError;
use crate::store::{BackendError, Commit, Fake, Files, Landing, Shared};

/// The most rows the layout holds.
pub const MAX_ROWS: usize = 64;
/// The most racks the layout holds.
pub const MAX_RACKS: usize = 512;
/// The most placements the layout holds.
pub const MAX_PLACEMENTS: usize = 4096;
/// The most U a rack is high.
pub const MAX_HEIGHT_U: u32 = 60;
/// The most characters of a row's or a rack's name.
pub const MAX_NAME_CHARS: usize = 32;
/// The most bytes of a subject's id.
pub const MAX_SUBJECT_BYTES: usize = 128;

/// The file beside the state file `state`: `<state>.layout`.
#[must_use]
pub fn layout_path(state: &Path) -> PathBuf {
    let mut path = state.as_os_str().to_owned();
    path.push(".layout");
    PathBuf::from(path)
}

/// Why a layout was refused, or not written.
#[derive(Debug, thiserror::Error)]
pub enum LayoutError {
    /// The file could not be read or written.
    #[error(transparent)]
    Io(#[from] StoreError),
    /// The file or a stored record holds a value this build cannot read.
    #[error(
        "{path}: {why}; the coordinator does not guess where the hosts stand, so fix or remove the file to start"
    )]
    Stored {
        /// The file, or the table.
        path: PathBuf,
        /// Why.
        why: String,
    },
    /// A field breaks its rule: a name, a height, an id.
    #[error("{0}")]
    Invalid(String),
    /// A row or rack name is taken.
    #[error("the name {0:?} is taken")]
    NameTaken(String),
    /// No row has the id.
    #[error("there is no row {0}")]
    UnknownRow(String),
    /// No rack has the id.
    #[error("there is no rack {0}")]
    UnknownRack(String),
    /// The subject is not placed.
    #[error("{0} is not placed")]
    UnknownPlacement(String),
    /// A row holds racks, or a rack holds placements.
    #[error("{0} is not empty")]
    InUse(String),
    /// Two placements of a rack share a U.
    #[error("{first} and {second} share a U of {rack}")]
    Overlap {
        /// The rack.
        rack: String,
        /// The placement that sorts first.
        first: String,
        /// The other.
        second: String,
    },
    /// A placement is not within its rack.
    #[error("{subject} is not within U 1 to {height} of {rack}")]
    OutOfRack {
        /// The rack.
        rack: String,
        /// The subject.
        subject: String,
        /// The rack's height.
        height: u32,
    },
    /// No network device has the id.
    #[error("there is no device {0}")]
    UnknownDevice(String),
    /// More rows, racks or placements than the layout holds.
    #[error("the layout holds at most {0}")]
    Capacity(String),
}

/// A row, as the layout keeps it. `id` is the key of the record and is not in its JSON.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    /// `row-<n>`.
    #[serde(skip)]
    pub id: String,
    /// 0, 1, 2, ...: the number shown.
    pub number: u32,
}

/// A rack, as the layout keeps it. `id` is the key of the record and is not in its JSON.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rack {
    /// `rack-<n>`.
    #[serde(skip)]
    pub id: String,
    /// The id of the row it stands in.
    pub row: String,
    /// 0, 1, 2, ... within its row: the number shown.
    pub number: u32,
    /// How many U high it is.
    pub height_u: u32,
}

/// What a placement places.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// An enrolled host, by node id.
    Host,
    /// An inventory entry.
    Inventory,
}

impl Kind {
    /// The kind's spelling in a key.
    fn word(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Inventory => "inventory",
        }
    }
}

/// What is placed: a host or an inventory entry, by id.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Subject {
    /// Which it is.
    pub kind: Kind,
    /// Its id: 1 to [`MAX_SUBJECT_BYTES`] visible ASCII bytes.
    pub id: String,
}

impl Subject {
    /// The key the placement is kept under: `host:<id>` or `inventory:<id>`.
    #[must_use]
    pub fn key(&self) -> String {
        format!("{}:{}", self.kind.word(), self.id)
    }

    /// The subject a key names.
    fn of_key(key: &str) -> Result<Self, LayoutError> {
        let (word, id) = key
            .split_once(':')
            .ok_or_else(|| LayoutError::Invalid(format!("the placement {key}: not kind:id")))?;
        let kind = match word {
            "host" => Kind::Host,
            "inventory" => Kind::Inventory,
            _ => {
                return Err(LayoutError::Invalid(format!(
                    "the placement {key}: the kind is host or inventory"
                )));
            }
        };
        Ok(Self {
            kind,
            id: id.to_owned(),
        })
    }
}

/// A subject in a rack. `subject` is the key of the record and is not in its JSON.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    /// What is placed.
    #[serde(skip, default = "unplaced")]
    pub subject: Subject,
    /// The id of the rack.
    pub rack: String,
    /// The lowest U it occupies (U 1 is the bottom).
    pub u: u32,
    /// How many U it occupies.
    pub height_u: u32,
}

/// The subject of a placement read before its key is put on it.
fn unplaced() -> Subject {
    Subject {
        kind: Kind::Host,
        id: String::new(),
    }
}

impl Placement {
    /// The highest U it occupies.
    fn top(&self) -> u32 {
        self.u.saturating_add(self.height_u).saturating_sub(1)
    }
}

/// Everything the layout holds, by id (rows and racks) and by subject key (placements).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layout {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    name: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    rows: BTreeMap<String, Row>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    racks: BTreeMap<String, Rack>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    placements: BTreeMap<String, Placement>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    devices: BTreeMap<String, Device>,
}

/// Whether `name` is a cluster's name.
fn is_name(name: &str) -> bool {
    let chars = name.chars().count();
    (1..=MAX_NAME_CHARS).contains(&chars)
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-'))
        && name.trim() == name
}

/// Whether `id` is an id the coordinator makes: `prefix-` and one to nine digits.
fn is_id(id: &str, prefix: &str) -> bool {
    id.strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('-'))
        .is_some_and(|n| (1..=9).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The id after the highest of `ids`, which are all `prefix-<n>`.
fn next_id<'a>(prefix: &str, ids: impl Iterator<Item = &'a String>) -> String {
    let highest = ids
        .filter_map(|id| id.rsplit('-').next().and_then(|n| n.parse::<u64>().ok()))
        .max()
        .unwrap_or(0);
    format!("{prefix}-{}", highest.saturating_add(1))
}

impl Layout {
    /// The layout of `name`, `rows`, `racks` and `placements`, each by its key and judged whole:
    /// every rule of this module.
    ///
    /// # Errors
    /// [`LayoutError`]: the first rule broken.
    pub fn new(
        name: String,
        rows: BTreeMap<String, Row>,
        racks: BTreeMap<String, Rack>,
        placements: BTreeMap<String, Placement>,
    ) -> Result<Self, LayoutError> {
        Self::build(BTreeMap::new(), name, rows, racks, placements)
    }

    /// The layout of `devices` (by id) and the parts [`Layout::new`] takes, judged whole.
    ///
    /// # Errors
    /// [`LayoutError`]: the first rule broken.
    pub(crate) fn build(
        devices: BTreeMap<String, Device>,
        name: String,
        rows: BTreeMap<String, Row>,
        racks: BTreeMap<String, Rack>,
        placements: BTreeMap<String, Placement>,
    ) -> Result<Self, LayoutError> {
        let devices = device::judge(devices)?;
        if !name.is_empty() && !is_name(&name) {
            return Err(LayoutError::Invalid(format!(
                "the name: 1 to {MAX_NAME_CHARS} of letters, digits, space, . _ -"
            )));
        }
        if rows.len() > MAX_ROWS {
            return Err(LayoutError::Capacity(format!("{MAX_ROWS} rows")));
        }
        if racks.len() > MAX_RACKS {
            return Err(LayoutError::Capacity(format!("{MAX_RACKS} racks")));
        }
        if placements.len() > MAX_PLACEMENTS {
            return Err(LayoutError::Capacity(format!(
                "{MAX_PLACEMENTS} placements"
            )));
        }
        let mut seen = BTreeSet::new();
        let mut rows = rows;
        for (id, row) in &mut rows {
            if !is_id(id, "row") {
                return Err(LayoutError::Invalid(format!(
                    "the row {id}: an id is row-<n>"
                )));
            }
            if !seen.insert(row.number) {
                return Err(LayoutError::NameTaken(format!("row {}", row.number)));
            }
            id.clone_into(&mut row.id);
        }
        let mut seen = BTreeSet::new();
        let mut racks = racks;
        for (id, rack) in &mut racks {
            if !is_id(id, "rack") {
                return Err(LayoutError::Invalid(format!(
                    "the rack {id}: an id is rack-<n>"
                )));
            }
            if !(1..=MAX_HEIGHT_U).contains(&rack.height_u) {
                return Err(LayoutError::Invalid(format!(
                    "the rack {id}: a height is 1 to {MAX_HEIGHT_U} U"
                )));
            }
            if !rows.contains_key(&rack.row) {
                return Err(LayoutError::UnknownRow(rack.row.clone()));
            }
            if !seen.insert((rack.row.clone(), rack.number)) {
                return Err(LayoutError::NameTaken(format!("rack {}", rack.number)));
            }
            id.clone_into(&mut rack.id);
        }
        let mut placements = placements;
        let mut by_rack: BTreeMap<String, Vec<(u32, u32, String)>> = BTreeMap::new();
        for (key, placement) in &mut placements {
            let subject = Subject::of_key(key)?;
            let visible = (1..=MAX_SUBJECT_BYTES).contains(&subject.id.len())
                && subject.id.bytes().all(|b| (0x21..=0x7e).contains(&b));
            if !visible {
                return Err(LayoutError::Invalid(format!(
                    "the placement {key}: an id is 1 to {MAX_SUBJECT_BYTES} visible ASCII bytes"
                )));
            }
            if placement.u == 0 || placement.height_u == 0 {
                return Err(LayoutError::Invalid(format!(
                    "the placement {key}: a U and a height are 1 or more"
                )));
            }
            let rack = racks
                .get(&placement.rack)
                .ok_or_else(|| LayoutError::UnknownRack(placement.rack.clone()))?;
            if placement.top() > rack.height_u {
                return Err(LayoutError::OutOfRack {
                    rack: placement.rack.clone(),
                    subject: key.clone(),
                    height: rack.height_u,
                });
            }
            by_rack.entry(placement.rack.clone()).or_default().push((
                placement.u,
                placement.top(),
                key.clone(),
            ));
            placement.subject = subject;
        }
        for (rack, mut spans) in by_rack {
            spans.sort_unstable();
            for pair in spans.windows(2) {
                if pair[1].0 <= pair[0].1 {
                    return Err(LayoutError::Overlap {
                        rack: rack.clone(),
                        first: pair[0].2.clone(),
                        second: pair[1].2.clone(),
                    });
                }
            }
        }
        Ok(Self {
            name,
            rows,
            racks,
            placements,
            devices,
        })
    }

    /// The cluster's name; empty until it is named.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether the layout holds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Every row, by number.
    #[must_use]
    pub fn rows(&self) -> Vec<&Row> {
        let mut rows: Vec<&Row> = self.rows.values().collect();
        rows.sort_by_key(|r| (r.number, r.id.clone()));
        rows
    }

    /// Every rack, by row number, then rack number.
    #[must_use]
    pub fn racks(&self) -> Vec<&Rack> {
        let mut racks: Vec<&Rack> = self.racks.values().collect();
        racks.sort_by_key(|r| {
            (
                self.rows.get(&r.row).map_or(0, |row| row.number),
                r.number,
                r.id.clone(),
            )
        });
        racks
    }

    /// Every placement, by rack id, then lowest U, then key.
    #[must_use]
    pub fn placements(&self) -> Vec<&Placement> {
        let mut placements: Vec<&Placement> = self.placements.values().collect();
        placements
            .sort_by(|a, b| (&a.rack, a.u, a.subject.key()).cmp(&(&b.rack, b.u, b.subject.key())));
        placements
    }

    /// The row `id`.
    #[must_use]
    pub fn row(&self, id: &str) -> Option<&Row> {
        self.rows.get(id)
    }

    /// The rack `id`.
    #[must_use]
    pub fn rack(&self, id: &str) -> Option<&Rack> {
        self.racks.get(id)
    }

    /// The placement of `subject`.
    #[must_use]
    pub fn placement(&self, subject: &Subject) -> Option<&Placement> {
        self.placements.get(&subject.key())
    }

    /// The layout named `name`.
    ///
    /// # Errors
    /// [`LayoutError::Invalid`]: the name breaks its rule, or is empty.
    pub fn with_name(&self, name: &str) -> Result<Self, LayoutError> {
        if name.is_empty() {
            return Err(LayoutError::Invalid(
                "the name is 1 or more characters".to_owned(),
            ));
        }
        Self::build(
            self.devices.clone(),
            name.to_owned(),
            self.rows.clone(),
            self.racks.clone(),
            self.placements.clone(),
        )
    }

    /// The layout with the next row added, and the row: its number is one more than the highest, 0 for the first.
    ///
    /// # Errors
    /// [`LayoutError::Capacity`]: the layout holds the most rows.
    pub fn with_next_row(&self) -> Result<(Self, Row), LayoutError> {
        let mut rows = self.rows.clone();
        let id = next_id("row", rows.keys());
        let number = rows
            .values()
            .map(|r| r.number)
            .max()
            .map_or(0, |highest| highest.saturating_add(1));
        let row = Row {
            id: id.clone(),
            number,
        };
        rows.insert(id, row.clone());
        let next = Self::build(
            self.devices.clone(),
            self.name.clone(),
            rows,
            self.racks.clone(),
            self.placements.clone(),
        )?;
        Ok((next, row))
    }

    /// The layout without the row `id`.
    ///
    /// # Errors
    /// [`LayoutError::UnknownRow`]; [`LayoutError::InUse`]: a rack stands in it.
    pub fn without_row(&self, id: &str) -> Result<Self, LayoutError> {
        if !self.rows.contains_key(id) {
            return Err(LayoutError::UnknownRow(id.to_owned()));
        }
        if self.racks.values().any(|rack| rack.row == id) {
            return Err(LayoutError::InUse(id.to_owned()));
        }
        let mut rows = self.rows.clone();
        rows.remove(id);
        Self::build(
            self.devices.clone(),
            self.name.clone(),
            rows,
            self.racks.clone(),
            self.placements.clone(),
        )
    }

    /// The layout with a rack added to the row `row`, or the rack `id` resized and, when `row`
    /// names another row, moved there, and the rack. A new rack, or one moved, takes the next
    /// number of its row (0 for the first).
    ///
    /// # Errors
    /// [`LayoutError::UnknownRack`]: `id` names no rack. Any other: the result breaks a rule (a
    /// row the layout lacks, a height, a placement left out of the rack).
    pub fn with_rack(
        &self,
        id: &str,
        row: &str,
        height_u: u32,
    ) -> Result<(Self, Rack), LayoutError> {
        let mut racks = self.racks.clone();
        let held = if id.is_empty() {
            None
        } else {
            Some(
                racks
                    .get(id)
                    .cloned()
                    .ok_or_else(|| LayoutError::UnknownRack(id.to_owned()))?,
            )
        };
        let id = held
            .as_ref()
            .map_or_else(|| next_id("rack", racks.keys()), |rack| rack.id.clone());
        let row = if row.is_empty() {
            held.as_ref()
                .map(|rack| rack.row.clone())
                .unwrap_or_default()
        } else {
            row.to_owned()
        };
        let number = match &held {
            Some(rack) if rack.row == row => rack.number,
            _ => racks
                .values()
                .filter(|rack| rack.row == row && rack.id != id)
                .map(|rack| rack.number)
                .max()
                .map_or(0, |highest| highest.saturating_add(1)),
        };
        let rack = Rack {
            id: id.clone(),
            row,
            number,
            height_u,
        };
        racks.insert(id, rack.clone());
        let next = Self::build(
            self.devices.clone(),
            self.name.clone(),
            self.rows.clone(),
            racks,
            self.placements.clone(),
        )?;
        Ok((next, rack))
    }

    /// The layout without the rack `id`.
    ///
    /// # Errors
    /// [`LayoutError::UnknownRack`]; [`LayoutError::InUse`]: a placement is in it.
    pub fn without_rack(&self, id: &str) -> Result<Self, LayoutError> {
        if !self.racks.contains_key(id) {
            return Err(LayoutError::UnknownRack(id.to_owned()));
        }
        if self
            .placements
            .values()
            .any(|placement| placement.rack == id)
        {
            return Err(LayoutError::InUse(id.to_owned()));
        }
        let mut racks = self.racks.clone();
        racks.remove(id);
        Self::build(
            self.devices.clone(),
            self.name.clone(),
            self.rows.clone(),
            racks,
            self.placements.clone(),
        )
    }

    /// The layout with `placement` made: the subject moved there from where it was.
    ///
    /// # Errors
    /// [`LayoutError`]: the result breaks a rule (a rack the layout lacks, a subject's id, a U
    /// outside the rack, a U another placement holds).
    pub fn with_placement(&self, placement: Placement) -> Result<(Self, Placement), LayoutError> {
        let key = placement.subject.key();
        let mut placements = self.placements.clone();
        placements.insert(key, placement.clone());
        let next = Self::build(
            self.devices.clone(),
            self.name.clone(),
            self.rows.clone(),
            self.racks.clone(),
            placements,
        )?;
        Ok((next, placement))
    }

    /// The layout without the placement of `subject`.
    ///
    /// # Errors
    /// [`LayoutError::UnknownPlacement`]: it is not placed.
    pub fn without_placement(&self, subject: &Subject) -> Result<Self, LayoutError> {
        let mut placements = self.placements.clone();
        placements
            .remove(&subject.key())
            .ok_or_else(|| LayoutError::UnknownPlacement(subject.key()))?;
        Self::build(
            self.devices.clone(),
            self.name.clone(),
            self.rows.clone(),
            self.racks.clone(),
            placements,
        )
    }

    /// The layout the stored records `rows`, `racks` and `placements` hold, each by its key, as
    /// the file's reader reads them.
    ///
    /// # Errors
    /// [`LayoutError::Stored`] naming `place` and the record: one does not read, or the layout
    /// breaks a rule of [`Layout::new`].
    pub(crate) fn of_records(
        place: &Path,
        name: String,
        rows: Vec<(String, serde_json::Value)>,
        racks: Vec<(String, serde_json::Value)>,
        placements: Vec<(String, serde_json::Value)>,
        devices: Vec<(String, serde_json::Value)>,
    ) -> Result<Self, LayoutError> {
        let refused = |why: String| LayoutError::Stored {
            path: place.to_owned(),
            why,
        };
        fn read<T: serde::de::DeserializeOwned>(
            records: Vec<(String, serde_json::Value)>,
            refused: &dyn Fn(String) -> LayoutError,
        ) -> Result<BTreeMap<String, T>, LayoutError> {
            records
                .into_iter()
                .map(|(key, value)| {
                    serde_json::from_value(value)
                        .map(|record| (key.clone(), record))
                        .map_err(|e| refused(format!("the record {key}: {e}")))
                })
                .collect()
        }
        Self::build(
            read(devices, &refused)?,
            name,
            read(rows, &refused)?,
            read(racks, &refused)?,
            read(placements, &refused)?,
        )
        .map_err(|why| refused(why.to_string()))
    }
}

/// The layout as the text of the file `<state-file>.layout`.
///
/// # Errors
/// The serialiser's, which a map of strings does not give.
pub(crate) fn encode_file(layout: &Layout) -> serde_json::Result<Vec<u8>> {
    let mut text = serde_json::to_vec_pretty(layout)?;
    text.push(b'\n');
    Ok(text)
}

/// The layout the file at `path` holds, in `bytes`.
///
/// # Errors
/// [`LayoutError::Stored`]: it is not JSON of the shape, or breaks a rule of [`Layout::new`].
pub(crate) fn parse_file(path: &Path, bytes: &[u8]) -> Result<Layout, LayoutError> {
    let refused = |why: String| LayoutError::Stored {
        path: path.to_owned(),
        why,
    };
    let held: Layout = serde_json::from_slice(bytes).map_err(|e| refused(e.to_string()))?;
    Layout::build(
        held.devices,
        held.name,
        held.rows,
        held.racks,
        held.placements,
    )
    .map_err(|why| refused(why.to_string()))
}

/// The layout stored, and the backend a change of it is committed to ([`crate::store`]).
pub struct LayoutStore {
    held: Mutex<Layout>,
    backend: Shared,
}

impl std::fmt::Debug for LayoutStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LayoutStore").finish_non_exhaustive()
    }
}

impl LayoutStore {
    /// A store that keeps the layout in memory only, for a coordinator with no state file: each
    /// write is taken as done.
    #[must_use]
    pub fn volatile() -> Self {
        Self::over(Arc::new(Fake::new()), Layout::default())
    }

    /// The layout stored in the file at `path`, none if it is not there; changes are written to
    /// it.
    ///
    /// # Errors
    /// [`LayoutError`]: the file cannot be read, or holds a value this build cannot read.
    pub fn open(path: &Path) -> Result<Self, LayoutError> {
        let files = Files::default().with_layout(path);
        let held = files.load_layout()?;
        Ok(Self::over(Arc::new(files), held))
    }

    /// The layout `held`, as loaded from `backend`, to which its changes are committed.
    pub(crate) fn over(backend: Shared, held: Layout) -> Self {
        Self {
            held: Mutex::new(held),
            backend,
        }
    }

    /// Takes the layout of a commit of unknown outcome that `landing` found landed into memory,
    /// with what memory changed since laid over it.
    pub(crate) fn take_landed(&self, landing: &Landing) {
        let mut held = self.held();
        if let Some(taken) = landing.layout_over(&held) {
            *held = taken;
        }
    }

    fn held(&self) -> MutexGuard<'_, Layout> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The layout as it is now.
    #[must_use]
    pub fn snapshot(&self) -> Layout {
        self.held().clone()
    }

    /// Makes the layout `edit` makes of the one held, durable, and then the one held, and
    /// answers what `edit` found. A change that leaves the layout as it was writes nothing.
    ///
    /// # Errors
    /// [`LayoutError`]: `edit` refused, or the backend did not take the change; nothing changed
    /// in memory.
    pub fn change<T>(
        &self,
        edit: impl FnOnce(&Layout) -> Result<(Layout, T), LayoutError>,
    ) -> Result<T, LayoutError> {
        let mut held = self.held();
        let (next, found) = edit(&held)?;
        self.commit_change(&mut held, next)?;
        Ok(found)
    }

    /// Commits `next`, the result of an edit of `held`, and puts it in `held`.
    fn commit_change(
        &self,
        held: &mut MutexGuard<'_, Layout>,
        next: Layout,
    ) -> Result<(), LayoutError> {
        if next == **held {
            return Ok(());
        }
        let commit = Commit {
            layout: Some((&**held, &next)),
            ..Commit::default()
        };
        self.backend.commit(&commit).map_err(|why| match why {
            BackendError::Layout(why) => why,
            other => LayoutError::Io(other.into_store()),
        })?;
        **held = next;
        Ok(())
    }
}

mod device;
pub use device::{Device, MAX_DEVICES, Segment};

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
