//! The console targets file (`--console-targets-file`, `docs/design/console-broker.md` decision
//! D8 and section 9.2): the address the coordinator dials for each device, by the device's MAC.
//!
//! ```toml
//! [[target]]
//! mac = "0c:c4:7a:00:00:02"
//! address = "192.0.2.12"
//!
//! [[target]]
//! mac = "a8:5e:45:00:00:01"
//! address = "mac-01.lab.example"
//! ```
//!
//! For a Linux server `mac` is its BMC's MAC (the inventory record's `bmc_mac`); for a Mac it is
//! one of the Mac's data MACs. The MAC is written as the inventory takes one (six pairs of hex
//! digits split by `:` or `-`, or twelve hex digits, in either case). `address` is an IPv4 or IPv6
//! address (an IPv4 address written as IPv6, `::ffff:192.0.2.12`, is that IPv4 address), or a DNS
//! name (labels of 1 to 63 bytes of letters, digits and `-`, not starting or ending with `-`, at
//! most 253 bytes in all, whose last label is not a number: no digits alone and no `0x` and hex
//! digits, which a resolver reads as an IPv4 address in an old form, `0300.0.2.1` as 192.0.2.1), with
//! no port: the port is the device's (623 for a
//! BMC, 5900 for a Mac). Addresses are DHCP reservations, so the file is written once beside the
//! inventory. A MAC and an address are each named once. The inventory record is not changed: a
//! field added there would be refused by the release before (rule B8).

use std::collections::BTreeMap;
use std::fmt;
use std::net::IpAddr;
use std::path::Path;

use hostkeeper_enrollment::names::MacAddress;
use serde::Deserialize;

use crate::file::{ReadError, read_text};

/// The flag that names the file, which error texts name instead of its path.
pub const FLAG: &str = "--console-targets-file";

/// The longest file read.
pub const MAX_FILE_BYTES: u64 = 1 << 20;

/// The longest DNS name, and the longest label of one.
const MAX_NAME_BYTES: usize = 253;
const MAX_LABEL_BYTES: usize = 63;

/// Why the file is refused. The text names the flag, never the path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TargetsError {
    /// The file could not be read.
    #[error(transparent)]
    Read(#[from] ReadError),
    /// The text is not TOML of `[[target]]` entries with `mac` and `address`.
    #[error("--console-targets-file: {0}")]
    Parse(String),
    /// An entry's MAC is no MAC address.
    #[error("--console-targets-file: target {index}: {mac:?} is not a MAC address")]
    Mac {
        /// The entry's place in the file, from 1.
        index: usize,
        /// The MAC as written.
        mac: String,
    },
    /// An entry's address is neither an IP address nor a DNS name.
    #[error(
        "--console-targets-file: target {index}: {address:?} is not an IP address or a DNS name"
    )]
    Address {
        /// The entry's place in the file, from 1.
        index: usize,
        /// The address as written.
        address: String,
    },
    /// Two entries name one MAC.
    #[error("--console-targets-file: the MAC {0} is named twice")]
    RepeatedMac(MacAddress),
    /// Two entries name one address.
    #[error("--console-targets-file: the address {0} is named twice")]
    RepeatedAddress(Address),
}

/// Where the coordinator dials a device.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Address {
    /// An IPv4 or IPv6 address.
    Ip(IpAddr),
    /// A DNS name, in lower case.
    Name(String),
}

impl Address {
    /// The address `text` is, if it is one.
    fn parse(text: &str) -> Option<Self> {
        if let Ok(ip) = text.parse::<IpAddr>() {
            return Some(Address::Ip(ip.to_canonical()));
        }
        is_dns_name(text).then(|| Address::Name(text.to_ascii_lowercase()))
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Address::Ip(ip) => ip.fmt(f),
            Address::Name(name) => f.write_str(name),
        }
    }
}

/// The targets of the file: an address for each MAC it names.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Targets {
    by_mac: BTreeMap<MacAddress, Address>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    #[serde(default)]
    target: Vec<RawTarget>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTarget {
    mac: String,
    address: String,
}

impl Targets {
    /// The targets of the file's text `text`.
    ///
    /// # Errors
    /// [`TargetsError`]: the text is not the file's TOML, an entry's MAC or address does not
    /// parse, or a MAC or an address is named twice.
    pub fn parse(text: &str) -> Result<Self, TargetsError> {
        let file: RawFile =
            toml::from_str(text).map_err(|e| TargetsError::Parse(e.message().to_owned()))?;
        let mut by_mac = BTreeMap::new();
        for (index, entry) in file.target.into_iter().enumerate() {
            let index = index + 1;
            let mac = MacAddress::parse(&entry.mac).map_err(|_| TargetsError::Mac {
                index,
                mac: entry.mac.clone(),
            })?;
            let address = Address::parse(&entry.address).ok_or_else(|| TargetsError::Address {
                index,
                address: entry.address.clone(),
            })?;
            if by_mac.contains_key(&mac) {
                return Err(TargetsError::RepeatedMac(mac));
            }
            if by_mac.values().any(|held| *held == address) {
                return Err(TargetsError::RepeatedAddress(address));
            }
            by_mac.insert(mac, address);
        }
        Ok(Self { by_mac })
    }

    /// The targets of the file at `path`.
    ///
    /// # Errors
    /// [`TargetsError`]: the file cannot be read, is over [`MAX_FILE_BYTES`] or not UTF-8, or its
    /// text is refused as [`Self::parse`] refuses it.
    pub fn read(path: &Path) -> Result<Self, TargetsError> {
        Self::parse(&read_text(path, FLAG, MAX_FILE_BYTES)?)
    }

    /// The address of the device whose MAC is `mac`, if the file gives one.
    #[must_use]
    pub fn address_of(&self, mac: &MacAddress) -> Option<&Address> {
        self.by_mac.get(mac)
    }
}

/// Whether `text` is a DNS name: dot-separated labels of 1 to 63 bytes of letters, digits and
/// `-`, none starting or ending with `-`, at most 253 bytes in all, the last not a number.
fn is_dns_name(text: &str) -> bool {
    (1..=MAX_NAME_BYTES).contains(&text.len())
        && text.split('.').all(|label| {
            (1..=MAX_LABEL_BYTES).contains(&label.len())
                && label
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        })
        && !text.rsplit('.').next().is_some_and(is_number)
}

/// Whether `label` is a number as a resolver reads a part of an IPv4 address: decimal or octal
/// digits alone, or `0x` and hex digits.
fn is_number(label: &str) -> bool {
    let hex = label
        .strip_prefix("0x")
        .or_else(|| label.strip_prefix("0X"));
    match hex {
        Some(digits) => digits.bytes().all(|c| c.is_ascii_hexdigit()),
        None => label.bytes().all(|c| c.is_ascii_digit()),
    }
}

#[cfg(test)]
#[path = "targets_tests.rs"]
mod tests;
