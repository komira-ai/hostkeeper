//! SSH through the coordinator, the operator's side
//! (`docs/design/ssh-through-coordinator.md`, sections 7 and 8).
//!
//! - [`Verb::Proxy`] (`hk ssh-proxy <host-id>`): ssh's `ProxyCommand`. Its stdin and stdout are a
//!   byte stream to port 22 of the host, carried by the coordinator's `Ssh/Tunnel`. It gets a
//!   certificate for the host first when it holds none that is valid.
//! - [`Verb::Ssh`] (`hk ssh <host-id> [-- ssh args]`): reads the pinned host keys, gets a
//!   one-host certificate for the operator's key, and runs `ssh` with `hk` as its proxy and as
//!   the judge of the host key (`KnownHostsCommand` accepts only a key whose fingerprint is
//!   pinned, with `StrictHostKeyChecking yes`).
//! - [`Verb::Revoke`] and [`Verb::Status`]: `Ssh/Revoke`, and `Ssh/HostKeys` with the
//!   certificate held.
//! - [`known_host`] (`hk ssh-known-host`): the command ssh runs to ask whether a key is the
//!   pinned one.
//!
//! `hk` takes the coordinator's flags, and hands the rest of the command line to [`Launch`]. The
//! operator's token is read from the file [`Coordinator::token_file`] names for each run, and is
//! in no argument, no file of this program and no environment.

mod access;
mod cli;
mod client;
mod error;
mod launch;
pub mod programs;
mod run;
mod state;
mod tunnel;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub use access::{CERT_KEY_TYPE_SUFFIX, Identity, Pins, known_host_line};
pub use error::Error;
pub use launch::System;
pub use programs::{Programs, normalize_public_key};
pub use run::{Env, known_host, run};
pub use state::{CertInfo, State, check_host_id};

/// An ssh verb of `hk`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    /// `hk ssh`: a certificate, then ssh.
    Ssh,
    /// `hk ssh-proxy`: the byte stream of ssh's `ProxyCommand`.
    Proxy,
    /// `hk ssh-revoke`.
    Revoke,
    /// `hk ssh-status`.
    Status,
}

/// How to reach the coordinator's operator listener, as `hk`'s global flags gave it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coordinator {
    /// `https://<host>[:<port>]`, with no trailing `/`.
    pub endpoint: String,
    /// The PEM file of the CA the coordinator's certificate must chain to.
    pub ca_file: PathBuf,
    /// The file holding the operator's token; it must not be readable by group or others.
    pub token_file: PathBuf,
    /// The longest a unary call, and the connection to the coordinator, may take.
    pub timeout_secs: u64,
}

/// What runs an ssh verb: the production [`System`], or a double.
pub trait Launch {
    /// Runs `verb` with the tail of the command line `args` (what follows the verb's name);
    /// the exit code. A diagnostic starts with `name` and goes to `stderr`; `home` is the
    /// operator's home directory.
    fn launch(
        &self,
        verb: Verb,
        coordinator: Coordinator,
        args: Vec<OsString>,
        name: &str,
        home: &Path,
        stderr: &mut dyn std::io::Write,
    ) -> u8;
}
