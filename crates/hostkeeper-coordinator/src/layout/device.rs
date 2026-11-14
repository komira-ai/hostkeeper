//! The lab's network devices (`docs/design/network-gear.md`): the switches of the racks. A device
//! is part of the layout, kept with it (`devices` in the file, `lab_devices` on Postgres), and
//! is placed like an inventory entry: its placement subject is `inventory:<id>`. Its id is
//! `net.<n>`; a host id is a DNS label and has no `.`, so the two never share an id.

use std::collections::BTreeMap;

use hostkeeper_enrollment::names::MacAddress;
use serde::{Deserialize, Serialize};

use super::{Kind, Layout, LayoutError, MAX_NAME_CHARS, Subject, is_name};
use crate::inventory::valid_notes;

/// The most network devices the layout holds.
pub const MAX_DEVICES: usize = 256;
/// The most bytes of a console device's path.
const MAX_DEVICE_PATH: usize = 48;

/// What a network device carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Segment {
    /// The BMC network.
    Bmc,
    /// The data network.
    Data,
}

/// A network device, as the layout keeps it. `id` is the key of the record and is not in its JSON.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    /// `net.<n>`.
    #[serde(skip)]
    pub id: String,
    /// 1 to [`MAX_NAME_CHARS`] characters of the cluster name's set; unique.
    pub name: String,
    /// What it carries.
    pub role: Segment,
    /// The host the console cable ends at; empty for none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub console_host: String,
    /// The console port on that host (`/dev/ttyUSB0`); empty for none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub console_device: String,
    /// The management interface's MAC, lower-case colon-separated; empty for none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub management_mac: String,
    /// 0 to 256 bytes; empty for none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
}

/// Whether `id` is `net.` and one to nine digits.
fn is_device_id(id: &str) -> bool {
    id.strip_prefix("net.")
        .is_some_and(|n| (1..=9).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether `host` is a host id: a DNS label of `a-z`, `0-9` and `-`, not edged by `-`.
fn is_host(host: &str) -> bool {
    (1..=63).contains(&host.len())
        && host
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !host.starts_with('-')
        && !host.ends_with('-')
}

/// Whether `path` is a console device: `/dev/` and a name of letters, digits and `. _ - /`.
fn is_console_device(path: &str) -> bool {
    path.len() <= MAX_DEVICE_PATH
        && path.strip_prefix("/dev/").is_some_and(|rest| {
            !rest.is_empty()
                && !rest.contains("..")
                && rest
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'))
        })
}

fn invalid(id: &str, why: &str) -> LayoutError {
    LayoutError::Invalid(format!("the device {id}: {why}"))
}

/// Checks one device and puts it in its one spelling (its id, its MAC, its notes).
fn check(id: &str, device: &mut Device) -> Result<(), LayoutError> {
    if !is_device_id(id) {
        return Err(invalid(id, "an id is net.<n>"));
    }
    id.clone_into(&mut device.id);
    if !is_name(&device.name) {
        return Err(invalid(
            id,
            &format!("the name is 1 to {MAX_NAME_CHARS} of letters, digits, space, . _ -"),
        ));
    }
    if device.console_host.is_empty() != device.console_device.is_empty() {
        return Err(invalid(
            id,
            "give the console's host and device together, or neither",
        ));
    }
    let console_ok = device.console_host.is_empty()
        || (is_host(&device.console_host) && is_console_device(&device.console_device));
    if !console_ok {
        return Err(invalid(
            id,
            "the console is a host id and a device under /dev/ (letters, digits, . _ - /)",
        ));
    }
    if !device.management_mac.is_empty() {
        let mac = MacAddress::parse(&device.management_mac)
            .map_err(|_| invalid(id, "the management MAC is not a MAC"))?;
        device.management_mac = mac.to_string();
    }
    if !device.notes.trim().is_empty() {
        device.notes = valid_notes(&device.notes).ok_or_else(|| {
            invalid(
                id,
                "notes are at most 256 bytes, no control character but the line break",
            )
        })?;
    } else {
        device.notes.clear();
    }
    Ok(())
}

/// The devices, each checked, with no two sharing a name.
pub(super) fn judge(
    mut devices: BTreeMap<String, Device>,
) -> Result<BTreeMap<String, Device>, LayoutError> {
    if devices.len() > MAX_DEVICES {
        return Err(LayoutError::Capacity(format!("{MAX_DEVICES} devices")));
    }
    let mut names = std::collections::BTreeSet::new();
    for (id, device) in &mut devices {
        check(id, device)?;
        if !names.insert(device.name.clone()) {
            return Err(LayoutError::NameTaken(device.name.clone()));
        }
    }
    Ok(devices)
}

impl Layout {
    /// Every device, by name.
    #[must_use]
    pub fn devices(&self) -> Vec<&Device> {
        let mut devices: Vec<&Device> = self.devices.values().collect();
        devices.sort_by(|a, b| (&a.name, &a.id).cmp(&(&b.name, &b.id)));
        devices
    }

    /// The device `id`.
    #[must_use]
    pub fn device(&self, id: &str) -> Option<&Device> {
        self.devices.get(id)
    }

    /// The layout with a device added (`id` empty: the next id, one more than the highest) or the
    /// device `id` replaced by `device`, and the device as kept.
    ///
    /// # Errors
    /// [`LayoutError::UnknownDevice`]: `id` names none. Any other: the result breaks a rule.
    pub fn with_device(&self, id: &str, device: Device) -> Result<(Self, Device), LayoutError> {
        let mut devices = self.devices.clone();
        let id = if id.is_empty() {
            let highest = devices
                .keys()
                .filter_map(|k| k.strip_prefix("net.").and_then(|n| n.parse::<u64>().ok()))
                .max()
                .unwrap_or(0);
            format!("net.{}", highest.saturating_add(1))
        } else if devices.contains_key(id) {
            id.to_owned()
        } else {
            return Err(LayoutError::UnknownDevice(id.to_owned()));
        };
        devices.insert(id.clone(), device);
        let next = Self::build(
            devices,
            self.name.clone(),
            self.rows.clone(),
            self.racks.clone(),
            self.placements.clone(),
        )?;
        let kept = next.devices[&id].clone();
        Ok((next, kept))
    }

    /// The layout without the device `id` and without its placement.
    ///
    /// # Errors
    /// [`LayoutError::UnknownDevice`]: it is not held.
    pub fn without_device(&self, id: &str) -> Result<Self, LayoutError> {
        let mut devices = self.devices.clone();
        devices
            .remove(id)
            .ok_or_else(|| LayoutError::UnknownDevice(id.to_owned()))?;
        let mut placements = self.placements.clone();
        placements.remove(
            &Subject {
                kind: Kind::Inventory,
                id: id.to_owned(),
            }
            .key(),
        );
        Self::build(
            devices,
            self.name.clone(),
            self.rows.clone(),
            self.racks.clone(),
            placements,
        )
    }
}

#[cfg(test)]
#[path = "device_tests.rs"]
mod tests;
