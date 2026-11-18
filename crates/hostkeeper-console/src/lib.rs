//! The console's library (`docs/design/console-broker.md`, decision D1): what the coordinator's
//! `Console` calls read and write that is neither a socket nor a process. The coordinator reads
//! none of it yet: its `Console` calls refuse every operator until the step after this one (C2)
//! gives it the flags of these files.
//!
//! - [`operators`]: the console operators file (`--console-operators-file`, decision D9), who may
//!   open a console and who may also change power and the boot device. No file means nobody.
//! - [`targets`]: the console targets file (`--console-targets-file`, decision D8), the address
//!   of each BMC and Mac by its MAC.
//! - [`credential`]: the one BMC user and password (`--console-bmc-credential-file`, decision
//!   D5), a secret file, held in a type that prints nothing of it.
//! - [`reason`]: the reason of a console audit line, the operator's and the session's details,
//!   cut at the audit's 256 bytes (decision D4).
//!
//! The three files are new and read by this release alone: no file of an earlier release
//! changes (rule B8). Every error names the flag of its file, never its path, and an error of the
//! credential file holds none of its text.

pub mod credential;
mod file;
pub mod operators;
pub mod reason;
pub mod targets;

pub use file::ReadError;

#[cfg(test)]
mod testutil;
