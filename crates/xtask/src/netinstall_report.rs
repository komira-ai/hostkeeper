//! `cargo xtask netinstall-report`: the measurement of network install, item N11 of
//! `docs/design/network-install.md` (section 11, "The measurement for the white paper"): how long
//! one machine took from power-on to serving, with the person's wait (pending to approved) apart
//! from the machine's time.
//!
//! It reads the times the components record, from three files the operator (or the end-to-end
//! test) keeps of one machine's install, and reads no clock:
//!
//! - `--pxe-log`: the log of `hostkeeper-pxe serve` (its standard error), whose lines are
//!   `hostkeeper-pxe serve: <unix seconds> <text>`. Power-on is its first `DHCPDISCOVER` from
//!   the MAC in proxy mode, or its first `DHCPREQUEST` in router mode; the install started at
//!   the seed fetch of the MAC as the record's host.
//! - `--pending`: what `hostkeeper ctl --output json enrollment list` printed while the
//!   machine's request was pending: the request matched to the record's host gives pending,
//!   its `firstSeenUnix`.
//! - `--host`: what `hostkeeper ctl --output json hosts get --node-id <host-id>` printed once
//!   the host served: approved is its `enrolledUnix`, the first heartbeat its
//!   `firstHeartbeatUnix`, and serving its `firstServingUnix` (the first heartbeat that reported
//!   a service the coordinator manages for the host installed and healthy).
//!
//! The parsing is [`inputs`], the report [`render`]; both are pure functions of their text. An
//! event the files do not show is printed `not reached`, never as a number.

mod inputs;
mod render;

#[cfg(test)]
mod inputs_tests;
#[cfg(test)]
mod render_tests;
#[cfg(test)]
mod tests;

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use inputs::Events;

/// Why the report could not be made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum Error {
    /// A file could not be read.
    #[error("{what}: {why}")]
    Io { what: String, why: String },
    /// A file does not say what its flag names.
    #[error("{0}")]
    Refused(String),
}

/// A MAC address as `hostkeeper-pxe` logs it: six pairs of lower-case hex digits, separated by
/// colons. Upper-case digits are read as lower-case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Mac(String);

impl FromStr for Mac {
    type Err = String;

    fn from_str(text: &str) -> Result<Mac, String> {
        let pairs: Vec<&str> = text.split(':').collect();
        let pair = |p: &&str| p.len() == 2 && p.bytes().all(|b| b.is_ascii_hexdigit());
        if pairs.len() == 6 && pairs.iter().all(pair) {
            Ok(Mac(text.to_ascii_lowercase()))
        } else {
            Err(format!(
                "`{text}` is not a MAC address: six pairs of hex digits, separated by colons"
            ))
        }
    }
}

impl fmt::Display for Mac {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The mode `serve` ran in, which says which DHCP message is the power-on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum Mode {
    /// Power-on is the first `DHCPDISCOVER` from the MAC.
    Proxy,
    /// Power-on is the first `DHCPREQUEST` from the MAC.
    Router,
}

impl Mode {
    /// The mode as `--mode` spells it.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Mode::Proxy => "proxy",
            Mode::Router => "router",
        }
    }

    /// The DHCP message whose first from the MAC is its power-on.
    pub(crate) fn power_on(self) -> &'static str {
        match self {
            Mode::Proxy => "DHCPDISCOVER",
            Mode::Router => "DHCPREQUEST",
        }
    }
}

/// The flags of `netinstall-report`.
#[derive(Debug, clap::Args)]
pub(crate) struct Flags {
    /// The MAC the machine booted from, as its record names it.
    #[arg(long)]
    pub(crate) mac: Mac,
    /// The record's host id.
    #[arg(long)]
    pub(crate) host_id: String,
    /// The mode `serve` installed it in.
    #[arg(long, value_enum)]
    pub(crate) mode: Mode,
    /// The log of `hostkeeper-pxe serve` (its standard error).
    #[arg(long)]
    pub(crate) pxe_log: PathBuf,
    /// What `hostkeeper ctl --output json enrollment list` printed while the request was
    /// pending.
    #[arg(long)]
    pub(crate) pending: PathBuf,
    /// What `hostkeeper ctl --output json hosts get --node-id <host-id>` printed once it served.
    #[arg(long)]
    pub(crate) host: PathBuf,
}

/// The text of `path`, the `what` of `flag`.
fn read(flag: &str, path: &Path) -> Result<String, Error> {
    std::fs::read_to_string(path).map_err(|e| Error::Io {
        what: format!("{flag} {}", path.display()),
        why: e.to_string(),
    })
}

/// The report of the machine `flags` name, as lines.
pub(crate) fn run(flags: &Flags) -> Result<Vec<String>, Error> {
    let log = read("--pxe-log", &flags.pxe_log)?;
    let pending = read("--pending", &flags.pending)?;
    let host = read("--host", &flags.host)?;
    let (power_on, install_started) = inputs::pxe_log(&log, &flags.mac, flags.mode, &flags.host_id);
    let host = inputs::host(&host, &flags.host_id)?;
    let events = Events {
        power_on,
        install_started,
        pending: inputs::pending(&pending, &flags.host_id)?,
        approved: host.approved,
        first_heartbeat: host.first_heartbeat,
        serving: host.serving,
    };
    Ok(render::report(
        &flags.mac,
        &flags.host_id,
        flags.mode,
        &events,
    ))
}
