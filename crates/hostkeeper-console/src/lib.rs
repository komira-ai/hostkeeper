//! The console's library (`docs/design/console-broker.md`, decision D1): what the coordinator's
//! `Console` calls read, write and decide that is neither a socket nor a process.
//!
//! - [`operators`]: the console operators file (`--console-operators-file`, decision D9), who may
//!   open a console and who may also change power and the boot device. No file means nobody.
//! - [`targets`]: the console targets file (`--console-targets-file`, decision D8), the address
//!   of each BMC and Mac by its MAC.
//! - [`credential`]: the one BMC user and password (`--console-bmc-credential-file`, decision
//!   D5), a secret file, held in a type that prints nothing of it.
//! - [`reason`]: the reason of a console audit line, the operator's and the session's details,
//!   cut at the audit's 256 bytes (decision D4).
//! - [`ipmi`]: the `ipmitool` port (decision D6): how a session of the BMC tool is invoked, with the
//!   password for its environment only, and the trait the coordinator's process implements.
//! - [`bmc`]: a Linux server's power and boot device through its BMC over that port, each after
//!   the check that the BMC at the address has the host's BMC MAC (decision D8), and the closed
//!   errors of a call.
//! - [`sol`]: a Linux server's serial console over its BMC's Serial over LAN, on the same port:
//!   the checks before `sol activate`, the activation, the deactivation, and why a stream ended.
//! - [`session`]: the console sessions, one per host, on a time the caller passes: the window to
//!   attach, the attach of a WebSocket by the operator who opened the session, and the idle and
//!   total limits of an attached one.
//! - [`bridge`]: the bounded copy between a WebSocket and a device, both ways, over channels.
//! - [`close`]: the close codes and reason tokens of a stream that ended after its upgrade.
//! - `scripted` (feature `testing`): the port's scripted double.
//!
//! The three files are new and read by this release alone: no file of an earlier release
//! changes (rule B8). Every error names the flag of its file, never its path, and an error of the
//! credential file holds none of its text.

pub mod bmc;
pub mod bridge;
pub mod close;
pub mod credential;
mod file;
pub mod ipmi;
pub mod operators;
pub mod reason;
#[cfg(any(test, feature = "testing"))]
pub mod scripted;
pub mod session;
pub mod sol;
pub mod targets;

pub use file::ReadError;

#[cfg(test)]
mod testutil;
