//! The claims a host makes about itself in its enrollment request: its machine id, its DMI
//! product serial and uuid, the link-layer addresses of its network interfaces, and the MAC
//! address of its BMC.
//!
//! The BMC MAC is parameter 5 of the BMC's LAN configuration parameters (IPMI `Get LAN
//! Configuration Parameters`, network function 0x0C, command 0x02), asked on channels 1 to 11 in
//! order through the [`Ipmi`] port; the first channel that answers with a non-zero MAC gives the
//! claim ([`parse_lan_mac`]). No implementation of [`Ipmi`] is in this crate: reading
//! `/dev/ipmi0` needs root and an `ioctl`. The daemon's reader of `/dev/ipmi0`
//! (`hostkeeper_daemon::ipmi::DevIpmi`) is given to its Linux claims through
//! [`LinuxClaims::with_ipmi`]; [`LinuxClaims::host`] alone has none, so its BMC MAC claim is
//! absent.
//!
//! On a Mac the claims are the serial number and the platform UUID that `ioreg` reports.
//!
//! Claims help an operator recognise a machine. They are not proof and authorise nothing: an
//! operator approves a request by the SHA-256 of its key, never by its claims (design section
//! 5.7, step 2). On Linux a claim that cannot be read (a virtual machine without DMI data, a
//! daemon that is not root, a missing file, a host with no BMC or a BMC that refuses) is absent,
//! never an error: [`LinuxClaims`] always returns `Ok`. On a Mac the serial and the platform
//! UUID are required: a missing or malformed value is a [`ClaimsError`] that stops enrollment
//! before any request is posted, and no placeholder takes its place (the coordinator's
//! allowlist check would refuse an enrollment without a serial anyway).

use std::fs;
use std::io;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::wire::Claims;

/// Where the claims come from.
pub trait ClaimsSource: Send {
    fn claims(&self) -> Result<Claims, ClaimsError>;
}

/// Why a Mac's claims could not be made.
#[derive(Debug, thiserror::Error)]
pub enum ClaimsError {
    #[error("ioreg could not be read: {0}")]
    Unreadable(#[source] std::io::Error),
    #[error("ioreg output has no {0}")]
    Missing(&'static str),
    #[error("ioreg output has an empty {0}")]
    Empty(&'static str),
    #[error("ioreg output has a malformed {0}")]
    Malformed(&'static str),
    #[error("ioreg output has more than one {0}")]
    Repeated(&'static str),
}

/// The key of a Mac's serial number in `ioreg` output.
const SERIAL_KEY: &str = "IOPlatformSerialNumber";
/// The key of a Mac's platform UUID in `ioreg` output.
const UUID_KEY: &str = "IOPlatformUUID";
/// The longest serial number accepted.
const MAX_SERIAL: usize = 64;

/// The identifiers of a Mac, as `ioreg` reports them.
#[derive(Debug, PartialEq, Eq)]
pub struct MacIds {
    /// `IOPlatformSerialNumber`: one to 64 ASCII letters and digits.
    pub serial: String,
    /// `IOPlatformUUID`: 36 characters in the 8-4-4-4-12 form of hex digits.
    pub platform_uuid: String,
}

/// The identifiers in `text`, the output of `ioreg -rd1 -c IOPlatformExpertDevice`, whose
/// relevant lines look like `    "IOPlatformSerialNumber" = "C02ABCDEFGH1"`. Each key must be on
/// exactly one line with a double-quoted value and nothing after it.
pub fn parse_ioreg(text: &str) -> Result<MacIds, ClaimsError> {
    let serial = value_of(text, SERIAL_KEY)?;
    let serial_ok = (1..=MAX_SERIAL).contains(&serial.len())
        & serial.bytes().all(|b| b.is_ascii_alphanumeric());
    if !serial_ok {
        return Err(ClaimsError::Malformed(SERIAL_KEY));
    }
    let uuid = value_of(text, UUID_KEY)?;
    let uuid_ok = (uuid.len() == 36)
        & uuid.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        });
    if !uuid_ok {
        return Err(ClaimsError::Malformed(UUID_KEY));
    }
    Ok(MacIds {
        serial: serial.to_owned(),
        platform_uuid: uuid.to_owned(),
    })
}

/// The text between the double quotes of the one line `"<key>" = "<value>"` of `text`.
fn value_of<'a>(text: &'a str, key: &'static str) -> Result<&'a str, ClaimsError> {
    let prefix = format!("\"{key}\" = ");
    let mut found = text
        .lines()
        .filter_map(|line| line.trim().strip_prefix(prefix.as_str()));
    let rest = found.next().ok_or(ClaimsError::Missing(key))?;
    if found.next().is_some() {
        return Err(ClaimsError::Repeated(key));
    }
    let quoted = rest.strip_prefix('"').ok_or(ClaimsError::Malformed(key))?;
    let (value, after) = quoted.split_once('"').ok_or(ClaimsError::Malformed(key))?;
    if !after.is_empty() {
        return Err(ClaimsError::Malformed(key));
    }
    if value.trim().is_empty() {
        return Err(ClaimsError::Empty(key));
    }
    Ok(value)
}

/// What `ioreg` prints for the platform expert device.
pub trait Ioreg: Send {
    /// The text `ioreg -rd1 -c IOPlatformExpertDevice` prints. No implementation that runs
    /// `ioreg` is in this crate; the daemon's macOS construction is to provide one.
    fn platform_expert(&self) -> std::io::Result<String>;
}

/// The claims of a Mac: its serial number and platform UUID, read through [`Ioreg`].
pub struct MacClaims {
    ioreg: Box<dyn Ioreg>,
}

impl MacClaims {
    pub fn new(ioreg: Box<dyn Ioreg>) -> MacClaims {
        MacClaims { ioreg }
    }
}

impl ClaimsSource for MacClaims {
    fn claims(&self) -> Result<Claims, ClaimsError> {
        let text = self
            .ioreg
            .platform_expert()
            .map_err(ClaimsError::Unreadable)?;
        let ids = parse_ioreg(&text)?;
        Ok(Claims {
            mac_serial: Some(ids.serial),
            platform_uuid: Some(ids.platform_uuid),
            ..Claims::default()
        })
    }
}

/// The address a loopback interface reports; not a claim.
const NO_ADDRESS: &str = "00:00:00:00:00:00";

/// The LAN configuration parameter that holds the BMC's MAC address.
pub const LAN_MAC_PARAMETER: u8 = 5;

/// The channels asked for the BMC's MAC, in order: IPMI's channel numbers 1 to 11 (0 is the
/// primary IPMB, 12 and up are reserved or special).
pub const LAN_CHANNELS: RangeInclusive<u8> = 1..=11;

/// The host's BMC, asked through IPMI.
pub trait Ipmi: Send + Sync {
    /// The response to `Get LAN Configuration Parameters` (network function 0x0C, command 0x02)
    /// for `parameter` on `channel`, set and block selectors 0: the completion code first, then
    /// the response data. An error when the BMC cannot be asked (no device, no BMC).
    fn lan_parameter(&self, channel: u8, parameter: u8) -> io::Result<Vec<u8>>;
}

/// The MAC address in `response`, a response to `Get LAN Configuration Parameters` for
/// parameter 5, as lower-case `aa:bb:cc:dd:ee:ff`: exactly a completion code of 0, the parameter
/// revision and six bytes. Any other response, and the all-zero MAC of an unconfigured channel,
/// is `None`.
pub fn parse_lan_mac(response: &[u8]) -> Option<String> {
    let [0, _revision, a, b, c, d, e, f] = *response else {
        return None;
    };
    let mac = [a, b, c, d, e, f];
    if mac == [0; 6] {
        return None;
    }
    let octets: Vec<String> = mac.iter().map(|byte| format!("{byte:02x}")).collect();
    Some(octets.join(":"))
}

/// The BMC's MAC: the first channel of [`LAN_CHANNELS`] that answers with one. A channel that
/// cannot be asked or answers with no MAC is passed over.
fn bmc_mac(ipmi: &dyn Ipmi) -> Option<String> {
    LAN_CHANNELS
        .filter_map(|channel| ipmi.lan_parameter(channel, LAN_MAC_PARAMETER).ok())
        .find_map(|response| parse_lan_mac(&response))
}

/// The claims of a Linux host, read from the files it is given and, when it has a reader, from
/// its BMC.
#[derive(Clone)]
pub struct LinuxClaims {
    machine_id: PathBuf,
    product_serial: PathBuf,
    product_uuid: PathBuf,
    net: PathBuf,
    ipmi: Option<Arc<dyn Ipmi>>,
}

impl std::fmt::Debug for LinuxClaims {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinuxClaims")
            .field("machine_id", &self.machine_id)
            .field("product_serial", &self.product_serial)
            .field("product_uuid", &self.product_uuid)
            .field("net", &self.net)
            .field("ipmi", &self.ipmi.is_some())
            .finish()
    }
}

/// Two sources are the same when they read the same files and the same BMC reader (or none).
impl PartialEq for LinuxClaims {
    fn eq(&self, other: &LinuxClaims) -> bool {
        let same_reader = match (&self.ipmi, &other.ipmi) {
            (Some(mine), Some(theirs)) => Arc::ptr_eq(mine, theirs),
            (mine, theirs) => mine.is_none() && theirs.is_none(),
        };
        same_reader
            && (
                &self.machine_id,
                &self.product_serial,
                &self.product_uuid,
                &self.net,
            ) == (
                &other.machine_id,
                &other.product_serial,
                &other.product_uuid,
                &other.net,
            )
    }
}

impl Eq for LinuxClaims {}

impl LinuxClaims {
    /// The claims read from `machine_id`, `product_serial` and `product_uuid`, and from the
    /// `address` file of each directory in `net`.
    pub fn new(
        machine_id: &Path,
        product_serial: &Path,
        product_uuid: &Path,
        net: &Path,
    ) -> LinuxClaims {
        LinuxClaims {
            machine_id: machine_id.to_path_buf(),
            product_serial: product_serial.to_path_buf(),
            product_uuid: product_uuid.to_path_buf(),
            net: net.to_path_buf(),
            ipmi: None,
        }
    }

    /// These claims, with the BMC's MAC asked of `ipmi`.
    #[must_use]
    pub fn with_ipmi(self, ipmi: Arc<dyn Ipmi>) -> LinuxClaims {
        LinuxClaims {
            ipmi: Some(ipmi),
            ..self
        }
    }

    /// The host's own files: `/etc/machine-id`, `/sys/class/dmi/id/product_serial`,
    /// `/sys/class/dmi/id/product_uuid` and `/sys/class/net`, with no BMC reader (see the module
    /// documentation).
    pub fn host() -> LinuxClaims {
        LinuxClaims::new(
            Path::new("/etc/machine-id"),
            Path::new("/sys/class/dmi/id/product_serial"),
            Path::new("/sys/class/dmi/id/product_uuid"),
            Path::new("/sys/class/net"),
        )
    }
}

impl ClaimsSource for LinuxClaims {
    fn claims(&self) -> Result<Claims, ClaimsError> {
        Ok(Claims {
            machine_id: read_claim(&self.machine_id),
            product_serial: read_claim(&self.product_serial),
            product_uuid: read_claim(&self.product_uuid),
            network_addresses: addresses(&self.net),
            mac_serial: None,
            platform_uuid: None,
            bmc_mac: self.ipmi.as_deref().and_then(bmc_mac),
        })
    }
}

/// The trimmed text of `path`; `None` when it cannot be read, is not UTF-8 or is blank.
fn read_claim(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let value = text.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// The addresses in `<net>/<interface>/address`, sorted, without duplicates and without the
/// all-zero address of a loopback interface.
fn addresses(net: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(net) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| read_claim(&entry.path().join("address")))
        .filter(|address| address != NO_ADDRESS)
        .collect();
    out.sort();
    out.dedup();
    out
}
