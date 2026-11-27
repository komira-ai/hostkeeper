//! The BMC's sensor data records (IPMI 2.0 section 43), decoded only for what the SEL decoder
//! needs: each sensor's name, by the sensor's owner, logical unit and number, which a SEL entry
//! carries ([`SensorNames`]). Pure: [`crate::sel_source`] asks the BMC.
//!
//! A full sensor record (type 0x01), a compact one (0x02) and an event-only one (0x03) name a
//! sensor; every other type is passed over. A name is read when its type/length byte says 8-bit
//! ASCII and Latin-1 (bits 7:6 are 11b); a name in another encoding (Unicode, BCD plus, 6-bit
//! packed ASCII) is passed over, so its sensor is named by its type and number instead.

use std::collections::BTreeMap;

/// The record types that name a sensor, with the offset of the id string's type/length byte.
const NAMED: [(u8, usize); 3] = [(0x01, 47), (0x02, 31), (0x03, 16)];

/// The offsets of the sensor owner id, the owner's logical unit and the sensor number.
const OWNER: usize = 5;
const OWNER_LUN: usize = 6;
const NUMBER: usize = 7;

/// What `Get SDR Repository Info` answered that says the records changed: their most recent
/// addition and erase.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SdrInfo {
    pub added: u32,
    pub erased: u32,
}

/// The response data of `Get SDR Repository Info` (version, record count, free space, the most
/// recent addition and erase, the operations supported) read; none when shorter than its 14
/// bytes.
#[must_use]
pub fn parse_info(data: &[u8]) -> Option<SdrInfo> {
    let data = data.get(..14)?;
    Some(SdrInfo {
        added: u32::from_le_bytes([data[5], data[6], data[7], data[8]]),
        erased: u32::from_le_bytes([data[9], data[10], data[11], data[12]]),
    })
}

/// The sensors' names: by the owner id (the SEL generator id's first byte), the owner's logical
/// unit and the sensor number.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SensorNames(BTreeMap<(u8, u8, u8), String>);

impl SensorNames {
    /// Keeps the name the record `bytes` gives its sensor; false when it gives none (a type
    /// that names no sensor, a name in another encoding, a record cut short).
    pub fn add(&mut self, bytes: &[u8]) -> bool {
        let Some(&(_, at)) = bytes
            .get(3)
            .and_then(|kind| NAMED.iter().find(|(named, _)| named == kind))
        else {
            return false;
        };
        let (Some(&owner), Some(&lun), Some(&number), Some(&code)) = (
            bytes.get(OWNER),
            bytes.get(OWNER_LUN),
            bytes.get(NUMBER),
            bytes.get(at),
        ) else {
            return false;
        };
        if code >> 6 != 0b11 {
            return false;
        }
        let len = usize::from(code & 0x1F);
        let Some(name) = bytes.get(at + 1..at + 1 + len) else {
            return false;
        };
        let name: String = name
            .iter()
            .take_while(|b| **b != 0)
            .map(|b| char::from(*b))
            .collect();
        let name = name.trim();
        if name.is_empty() {
            return false;
        }
        self.0.insert((owner, lun & 0x03, number), name.to_owned());
        true
    }

    /// The name of the sensor `number` of the owner `owner` on its logical unit `lun`; that of
    /// the same owner and number on another unit when none is named on `lun`.
    #[must_use]
    pub fn name(&self, owner: u8, lun: u8, number: u8) -> Option<&str> {
        self.0
            .get(&(owner, lun, number))
            .or_else(|| {
                self.0
                    .iter()
                    .find(|((o, _, n), _)| *o == owner && *n == number)
                    .map(|(_, name)| name)
            })
            .map(String::as_str)
    }
}

#[cfg(test)]
#[path = "sdr_tests.rs"]
mod tests;
