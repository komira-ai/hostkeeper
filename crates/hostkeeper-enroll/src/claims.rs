//! The claims a host makes about itself in its enrollment request: its machine id, its DMI
//! product serial and uuid, and the link-layer addresses of its network interfaces.
//!
//! Claims help an operator recognise a machine. They are not proof and authorise nothing: an
//! operator approves a request by the SHA-256 of its key, never by its claims (design section
//! 5.7, step 2). A claim that cannot be read (a virtual machine without DMI data, a daemon that is
//! not root, a missing file) is absent, never an error.

use std::fs;
use std::path::{Path, PathBuf};

use crate::wire::Claims;

/// Where the claims come from.
pub trait ClaimsSource: Send {
    fn claims(&self) -> Claims;
}

/// The address a loopback interface reports; not a claim.
const NO_ADDRESS: &str = "00:00:00:00:00:00";

/// The claims of a Linux host, read from the files it is given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinuxClaims {
    machine_id: PathBuf,
    product_serial: PathBuf,
    product_uuid: PathBuf,
    net: PathBuf,
}

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
        }
    }

    /// The host's own files: `/etc/machine-id`, `/sys/class/dmi/id/product_serial`,
    /// `/sys/class/dmi/id/product_uuid` and `/sys/class/net`.
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
    fn claims(&self) -> Claims {
        Claims {
            machine_id: read_claim(&self.machine_id),
            product_serial: read_claim(&self.product_serial),
            product_uuid: read_claim(&self.product_uuid),
            network_addresses: addresses(&self.net),
        }
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
