//! The lab's sleds (`docs/design/rack-sleds.md`): a sled is the one thing that occupies U in a
//! rack. It is a range of U (lowest U and height) and an optional label; the servers and switches
//! stand on it, side by side: two Mac Studios on a 3 U sled, three NUCs on a 2 U one.
//!
//! Every placed subject (a host, an inventory entry, a switch) stands on a slot of a sled, and its
//! position is the address cluster / row / rack / sled / slot ([`Layout::address`]). A 4 U server
//! is on a 4 U sled alone, a 1 U switch on a 1 U sled. A subject has no U or height of its own:
//! it names its sled, so a sled that moves takes everything on it along. The U-collision check
//! is between sleds.
//!
//! A sled declares its number of slots, 1 to [`MAX_SLOTS`] (1 when the record has none: every sled
//! of an older coordinator is a one-slot sled). A subject is put on a free slot of it, the lowest
//! one when the placement names none. A sled's number is not
//! stored either: it is its place counting from the bottom of its rack (the lowest sled is 0), so
//! it always matches the rack as one sees it. The file keeps the sleds under `sleds` and the
//! subjects on them under `sled_items`, Postgres in `lab_sleds` and `lab_sled_items`
//! (migration `0017`).
//!
//! A layout from before sleds holds placements (rack, lowest U, height): each is turned, as the
//! layout is built, into an `implicit` sled of that rack, U and height with the subject on slot 0
//! ([`convert`]), and the next write of the file leaves the placements out. A placement made by
//! rack and U is the same: it makes an implicit sled, which goes when its subject leaves it and
//! it is empty; a sled put by hand is never implicit.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{
    Device, Layout, LayoutError, MAX_HEIGHT_U, MAX_NAME_CHARS, MAX_PLACEMENTS, Placement, Rack,
    Subject, checked_subject, is_id, is_name, next_id, unplaced,
};

/// The most sleds the layout holds.
pub const MAX_SLEDS: usize = MAX_PLACEMENTS;
/// The most U a sled is high: a rack's.
pub const MAX_SLED_HEIGHT_U: u32 = MAX_HEIGHT_U;
/// The most subjects a sled carries: slots 0 to this minus 1.
pub const MAX_SLOTS: u32 = 8;

/// A sled, as the layout keeps it. `id` is the key of the record and is not in its JSON.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sled {
    /// `sled-<n>`.
    #[serde(skip)]
    pub id: String,
    /// The id of the rack it stands in.
    pub rack: String,
    /// The lowest U it occupies (U 1 is the bottom).
    pub u: u32,
    /// How many U it occupies, 1 to [`MAX_SLED_HEIGHT_U`].
    pub height_u: u32,
    /// 0 to [`MAX_NAME_CHARS`] characters of the cluster name's set; empty for none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    /// How many slots it carries side by side, 1 to [`MAX_SLOTS`]; 1 when the record has none.
    #[serde(default = "one_slot", skip_serializing_if = "is_one")]
    pub slots: u32,
    /// Made by a placement by rack and U, and gone when its subject leaves; a sled put by hand
    /// is not.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub implicit: bool,
}

/// The slot count of a record that has none.
fn one_slot() -> u32 {
    1
}

/// Whether `n` is 1, the count a record leaves out.
fn is_one(n: &u32) -> bool {
    *n == 1
}

impl Sled {
    /// The highest U it occupies.
    fn top(&self) -> u32 {
        self.u.saturating_add(self.height_u).saturating_sub(1)
    }
}

/// A subject on a sled. `subject` is the key of the record and is not in its JSON.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SledItem {
    /// What stands on the sled.
    #[serde(skip, default = "unplaced")]
    pub subject: Subject,
    /// The id of the sled.
    pub sled: String,
    /// Its slot, 0 to [`MAX_SLOTS`] minus 1, counted from the left as one faces the rack.
    pub slot: u32,
}

/// The parts of the layout besides the rows, racks and name: the devices, the sleds and what
/// stands on them. They travel together through every change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Extras {
    /// The network devices, by id.
    pub(crate) devices: BTreeMap<String, Device>,
    /// The sleds, by id.
    pub(crate) sleds: BTreeMap<String, Sled>,
    /// The subjects on sleds, by subject key.
    pub(crate) items: BTreeMap<String, SledItem>,
}

fn invalid(id: &str, why: &str) -> LayoutError {
    LayoutError::Invalid(format!("the sled {id}: {why}"))
}

/// Checks the sleds against the racks and puts each one's id in it.
pub(super) fn judge_sleds(
    sleds: &mut BTreeMap<String, Sled>,
    racks: &BTreeMap<String, Rack>,
) -> Result<(), LayoutError> {
    if sleds.len() > MAX_SLEDS {
        return Err(LayoutError::Capacity(format!("{MAX_SLEDS} sleds")));
    }
    for (id, sled) in sleds.iter_mut() {
        if !is_id(id, "sled") {
            return Err(invalid(id, "an id is sled-<n>"));
        }
        if !(1..=MAX_SLED_HEIGHT_U).contains(&sled.height_u) || sled.u == 0 {
            return Err(invalid(
                id,
                &format!("a U is 1 or more, a height is 1 to {MAX_SLED_HEIGHT_U} U"),
            ));
        }
        if !(1..=MAX_SLOTS).contains(&sled.slots) {
            return Err(invalid(id, &format!("slots are 1 to {MAX_SLOTS}")));
        }
        if !sled.label.is_empty() && !is_name(&sled.label) {
            return Err(invalid(
                id,
                &format!("the label is 1 to {MAX_NAME_CHARS} of letters, digits, space, . _ -"),
            ));
        }
        let rack = racks
            .get(&sled.rack)
            .ok_or_else(|| LayoutError::UnknownRack(sled.rack.clone()))?;
        if sled.top() > rack.height_u {
            return Err(LayoutError::OutOfRack {
                rack: sled.rack.clone(),
                subject: id.clone(),
                height: rack.height_u,
            });
        }
        id.clone_into(&mut sled.id);
    }
    Ok(())
}

/// Turns the placements (rack, lowest U, height) of a layout from before sleds into implicit
/// implicit sleds with the subject on slot 0, in the order of rack, U and key: the same sled ids
/// on every start.
pub(super) fn convert(
    sleds: &mut BTreeMap<String, Sled>,
    items: &mut BTreeMap<String, SledItem>,
    placements: BTreeMap<String, Placement>,
) -> Result<(), LayoutError> {
    let mut held: Vec<(String, Placement)> = placements.into_iter().collect();
    held.sort_by(|a, b| (&a.1.rack, a.1.u, &a.0).cmp(&(&b.1.rack, b.1.u, &b.0)));
    for (key, placement) in held {
        let subject = checked_subject(&key)?;
        if placement.u == 0 || placement.height_u == 0 {
            return Err(LayoutError::Invalid(format!(
                "the placement {key}: a U and a height are 1 or more"
            )));
        }
        if items.contains_key(&key) {
            return Err(LayoutError::Invalid(format!(
                "the placement {key}: in a rack and on a sled"
            )));
        }
        let id = next_id("sled", sleds.keys());
        sleds.insert(
            id.clone(),
            Sled {
                id: id.clone(),
                rack: placement.rack,
                u: placement.u,
                height_u: placement.height_u,
                label: String::new(),
                slots: 1,
                implicit: true,
            },
        );
        items.insert(
            key,
            SledItem {
                subject,
                sled: id,
                slot: 0,
            },
        );
    }
    Ok(())
}

/// Checks what stands on the sleds against the sleds, and puts each subject in its record.
pub(super) fn judge_items(
    items: &mut BTreeMap<String, SledItem>,
    sleds: &BTreeMap<String, Sled>,
) -> Result<(), LayoutError> {
    let mut taken: BTreeMap<(String, u32), String> = BTreeMap::new();
    for (key, item) in items.iter_mut() {
        let subject = checked_subject(key)?;
        let held = sleds
            .get(&item.sled)
            .ok_or_else(|| LayoutError::UnknownSled(item.sled.clone()))?;
        if item.slot >= held.slots {
            return Err(LayoutError::Invalid(format!(
                "the placement {key}: a slot of {} is 0 to {}",
                item.sled,
                held.slots - 1
            )));
        }
        if let Some(first) = taken.insert((item.sled.clone(), item.slot), key.clone()) {
            return Err(LayoutError::SlotTaken {
                sled: item.sled.clone(),
                slot: item.slot,
                first,
                second: key.clone(),
            });
        }
        item.subject = subject;
    }
    Ok(())
}

/// The U ranges the sleds hold, as the overlap check takes them: (rack, lowest, highest, name).
pub(super) fn spans(sleds: &BTreeMap<String, Sled>) -> Vec<(String, u32, u32, String)> {
    sleds
        .iter()
        .map(|(id, s)| (s.rack.clone(), s.u, s.top(), id.clone()))
        .collect()
}

impl Layout {
    /// The devices, sleds and subjects on sleds, to carry through a change of another part.
    pub(super) fn extras(&self) -> Extras {
        Extras {
            devices: self.devices.clone(),
            sleds: self.sleds.clone(),
            items: self.sled_items.clone(),
        }
    }

    /// Every sled, by rack id, then lowest U, then id.
    #[must_use]
    pub fn sleds(&self) -> Vec<&Sled> {
        let mut sleds: Vec<&Sled> = self.sleds.values().collect();
        sleds.sort_by(|a, b| (&a.rack, a.u, &a.id).cmp(&(&b.rack, b.u, &b.id)));
        sleds
    }

    /// The sled `id`.
    #[must_use]
    pub fn sled(&self, id: &str) -> Option<&Sled> {
        self.sleds.get(id)
    }

    /// Every subject on a sled, by sled id, then slot.
    #[must_use]
    pub fn sled_items(&self) -> Vec<&SledItem> {
        let mut items: Vec<&SledItem> = self.sled_items.values().collect();
        items.sort_by(|a, b| (&a.sled, a.slot).cmp(&(&b.sled, b.slot)));
        items
    }

    /// The place of `subject` on a sled.
    #[must_use]
    pub fn sled_item(&self, subject: &Subject) -> Option<&SledItem> {
        self.sled_items.get(&subject.key())
    }

    /// Every placed subject with its sled, by rack id, lowest U of the sled, then slot.
    #[must_use]
    pub fn placements(&self) -> Vec<(&SledItem, &Sled)> {
        let mut held: Vec<(&SledItem, &Sled)> = self
            .sled_items
            .values()
            .filter_map(|item| self.sleds.get(&item.sled).map(|sled| (item, sled)))
            .collect();
        held.sort_by(|a, b| {
            (&a.1.rack, a.1.u, &a.1.id, a.0.slot).cmp(&(&b.1.rack, b.1.u, &b.1.id, b.0.slot))
        });
        held
    }

    /// Where `subject` stands: its slot and the sled.
    #[must_use]
    pub fn placement(&self, subject: &Subject) -> Option<(&SledItem, &Sled)> {
        let item = self.sled_items.get(&subject.key())?;
        self.sleds.get(&item.sled).map(|sled| (item, sled))
    }

    /// The keys of the subjects on the sled `id`, by slot.
    fn occupants(&self, id: &str) -> Vec<String> {
        let mut held: Vec<&SledItem> = self
            .sled_items
            .values()
            .filter(|item| item.sled == id)
            .collect();
        held.sort_by_key(|item| item.slot);
        held.into_iter().map(|item| item.subject.key()).collect()
    }

    /// The number of the sled `id`: its place among the sleds of its rack counting from the
    /// bottom, 0 for the lowest.
    #[must_use]
    pub fn sled_number(&self, id: &str) -> Option<u32> {
        let sled = self.sleds.get(id)?;
        let below = self
            .sleds
            .values()
            .filter(|other| other.rack == sled.rack && other.u < sled.u)
            .count();
        u32::try_from(below).ok()
    }

    /// The address of the sled `sled`: `C0-R<row>-R<rack>-S<sled>`, every number counted from
    /// 0 (the cluster is always 0: a hostkeeper runs one lab), the row and the rack as numbered
    /// by the layout less one, the sled from the bottom of its rack. Computed, never stored.
    #[must_use]
    pub fn sled_address(&self, sled: &str) -> Option<String> {
        let held = self.sleds.get(sled)?;
        let rack = self.racks.get(&held.rack)?;
        let row = self.rows.get(&rack.row)?;
        Some(format!(
            "C0-R{}-R{}-S{}",
            row.number.saturating_sub(1),
            rack.number.saturating_sub(1),
            self.sled_number(sled)?,
        ))
    }

    /// The address of slot `slot` of the sled `sled`: the sled's with `-S<slot>`.
    #[must_use]
    pub fn address(&self, sled: &str, slot: u32) -> Option<String> {
        Some(format!("{}-S{slot}", self.sled_address(sled)?))
    }

    /// The lowest slot of the sled `id` that no subject holds.
    fn free_slot(&self, id: &str) -> Result<u32, LayoutError> {
        let slots = self.sleds.get(id).map_or(0, |sled| sled.slots);
        (0..slots)
            .find(|slot| {
                !self
                    .sled_items
                    .values()
                    .any(|item| item.sled == id && item.slot == *slot)
            })
            .ok_or_else(|| LayoutError::Capacity(format!("{slots} slots on {id}")))
    }

    /// The layout with a sled added to the rack `rack` (`id` empty: the next id, one more than
    /// the highest), or the sled `id` replaced: its rack, lowest U, height and label. What stands
    /// on it stays on it, so a sled that moves takes its subjects along. Also the sled as kept.
    ///
    /// # Errors
    /// [`LayoutError::UnknownSled`]: `id` names none. Any other: the result breaks a rule.
    pub fn with_sled(
        &self,
        id: &str,
        rack: &str,
        u: u32,
        height_u: u32,
        slots: u32,
        label: &str,
    ) -> Result<(Self, Sled), LayoutError> {
        let mut sleds = self.sleds.clone();
        let id = if id.is_empty() {
            next_id("sled", sleds.keys())
        } else if sleds.contains_key(id) {
            id.to_owned()
        } else {
            return Err(LayoutError::UnknownSled(id.to_owned()));
        };
        // A sled added by hand is not implicit; one replaced keeps what it was.
        let implicit = sleds.get(&id).is_some_and(|held| held.implicit);
        sleds.insert(
            id.clone(),
            Sled {
                id: id.clone(),
                rack: rack.to_owned(),
                u,
                height_u,
                label: label.to_owned(),
                slots,
                implicit,
            },
        );
        let crowded: Vec<&SledItem> = self
            .sled_items
            .values()
            .filter(|item| item.sled == id && item.slot >= slots)
            .collect();
        if !crowded.is_empty() {
            return Err(LayoutError::SledInUse {
                sled: id,
                items: crowded.iter().map(|item| item.subject.key()).collect(),
            });
        }
        let next = Self::build(
            Extras {
                sleds,
                ..self.extras()
            },
            self.name.clone(),
            self.rows.clone(),
            self.racks.clone(),
            BTreeMap::new(),
        )?;
        let kept = next.sleds[&id].clone();
        Ok((next, kept))
    }

    /// The layout without the sled `id`.
    ///
    /// # Errors
    /// [`LayoutError::UnknownSled`]; [`LayoutError::SledInUse`]: subjects stand on it.
    pub fn without_sled(&self, id: &str) -> Result<Self, LayoutError> {
        let mut sleds = self.sleds.clone();
        sleds
            .remove(id)
            .ok_or_else(|| LayoutError::UnknownSled(id.to_owned()))?;
        let held = self.occupants(id);
        if !held.is_empty() {
            return Err(LayoutError::SledInUse {
                sled: id.to_owned(),
                items: held,
            });
        }
        Self::build(
            Extras {
                sleds,
                ..self.extras()
            },
            self.name.clone(),
            self.rows.clone(),
            self.racks.clone(),
            BTreeMap::new(),
        )
    }

    /// The layout with `subject` put on the slot `slot` of the sled `sled` (`None`: the lowest
    /// free slot), moved there from where it was (another slot, another sled: an implicit sled
    /// it leaves empty goes), and the place as kept, with its sled.
    ///
    /// # Errors
    /// [`LayoutError`]: the result breaks a rule (a sled the layout lacks, a slot past the last
    /// or taken, no free slot, a subject's id).
    pub fn with_sled_placement(
        &self,
        subject: &Subject,
        sled: &str,
        slot: Option<u32>,
    ) -> Result<(Self, (SledItem, Sled)), LayoutError> {
        let key = subject.key();
        let mut extras = self.extras();
        release(&mut extras, &key, sled);
        let moved = Self::build(
            extras.clone(),
            self.name.clone(),
            self.rows.clone(),
            self.racks.clone(),
            BTreeMap::new(),
        )?;
        if !moved.sleds.contains_key(sled) {
            return Err(LayoutError::UnknownSled(sled.to_owned()));
        }
        let slot = match slot {
            Some(slot) => slot,
            None => moved.free_slot(sled)?,
        };
        extras.items.insert(
            key.clone(),
            SledItem {
                subject: subject.clone(),
                sled: sled.to_owned(),
                slot,
            },
        );
        self.finish_placement(extras, &key)
    }

    /// The layout with `placement` made: a new implicit sled in the rack at its U and height,
    /// with the subject on slot 0 of it, moved there from where it was. Also the place as kept,
    /// with its sled.
    ///
    /// # Errors
    /// [`LayoutError`]: the result breaks a rule (a rack the layout lacks, a subject's id, a U
    /// outside the rack, a U another sled holds).
    pub fn with_placement(
        &self,
        placement: Placement,
    ) -> Result<(Self, (SledItem, Sled)), LayoutError> {
        let key = placement.subject.key();
        let mut extras = self.extras();
        release(&mut extras, &key, "");
        let id = next_id("sled", extras.sleds.keys());
        extras.sleds.insert(
            id.clone(),
            Sled {
                id: id.clone(),
                rack: placement.rack,
                u: placement.u,
                height_u: placement.height_u,
                label: String::new(),
                slots: 1,
                implicit: true,
            },
        );
        extras.items.insert(
            key.clone(),
            SledItem {
                subject: placement.subject,
                sled: id,
                slot: 0,
            },
        );
        self.finish_placement(extras, &key)
    }

    /// The layout `extras` make and the place of the subject `key` in it.
    fn finish_placement(
        &self,
        extras: Extras,
        key: &str,
    ) -> Result<(Self, (SledItem, Sled)), LayoutError> {
        let next = Self::build(
            extras,
            self.name.clone(),
            self.rows.clone(),
            self.racks.clone(),
            BTreeMap::new(),
        )?;
        let item = next.sled_items[key].clone();
        let sled = next.sleds[&item.sled].clone();
        Ok((next, (item, sled)))
    }

    /// The layout without the placement of `subject`; an implicit sled it leaves empty goes too.
    ///
    /// # Errors
    /// [`LayoutError::UnknownPlacement`]: it is not placed.
    pub fn without_placement(&self, subject: &Subject) -> Result<Self, LayoutError> {
        let key = subject.key();
        if !self.sled_items.contains_key(&key) {
            return Err(LayoutError::UnknownPlacement(key));
        }
        let mut extras = self.extras();
        release(&mut extras, &key, "");
        Self::build(
            extras,
            self.name.clone(),
            self.rows.clone(),
            self.racks.clone(),
            BTreeMap::new(),
        )
    }
}

/// Takes the subject `key` off its sled, if it stands on one; an implicit sled that is then
/// empty goes, unless it is `keep`.
pub(super) fn release(extras: &mut Extras, key: &str, keep: &str) {
    let Some(item) = extras.items.remove(key) else {
        return;
    };
    let gone = item.sled != keep
        && extras.sleds.get(&item.sled).is_some_and(|s| s.implicit)
        && !extras.items.values().any(|other| other.sled == item.sled);
    if gone {
        extras.sleds.remove(&item.sled);
    }
}

#[cfg(test)]
#[path = "sled_tests.rs"]
mod tests;
