//! `hostkeeper-pxe-packets`: the packets of network install (docs/design/network-install.md
//! section 3, item N7), as pure functions. No socket, no clock, no file and no inventory read:
//! the caller (`pxe serve`, item N9) passes in what it read and sends what comes out.
//!
//! - [`dhcp`]: a DHCP packet read off UDP 67 or 4011. In proxy mode it gets a proxy offer or
//!   ACK (no address, the echoed vendor class, option 43 and the boot file of its
//!   architecture), or nothing and the reason. In router mode nothing is ever sent: a
//!   DISCOVER's architecture is kept for its MAC, and a REQUEST teaches the address its client
//!   takes ([`dhcp::Lease`]).
//! - [`tftp`]: a TFTP read request resolved to a file of the served set, a generated name, or
//!   the shim that `hostkeeper/boot.efi` stands for; every other request refused with a TFTP
//!   error. A [`tftp::Transfer`] numbers the blocks and ends with the short block of RFC 1350,
//!   after the option negotiation of RFC 2347 (`blksize`, `tsize`).
//! - [`Mac`], [`ClientArch`] and [`BootSets`]: a client's hardware address, the architecture
//!   it states in DHCP option 93 and the platform that takes, and the platforms that have a
//!   boot set, with the TFTP names of their boot loaders.

pub mod dhcp;
pub mod tftp;

mod arch;
mod mac;

pub use arch::{BootSets, BootSetsError, ClientArch, LoaderNames};
pub use mac::Mac;
