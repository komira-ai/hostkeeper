//! The tail of each ssh verb: what follows `hk ssh`, `hk ssh-proxy`, `hk ssh-revoke` and
//! `hk ssh-status` once `hk` has taken its global flags. The coordinator's flags, the state
//! directory and the keepalive are not here: the caller gives them ([`crate::ssh::Coordinator`],
//! [`crate::ssh::Env`]).

use std::ffi::OsString;
use std::path::PathBuf;

use clap::Parser;

/// `hk ssh <host-id> [--identity <file>] [--reason <text>] [-- ssh args]`.
#[derive(Debug, Parser)]
#[command(
    name = "hk ssh",
    about = "Get a certificate for a host and run ssh to it through the coordinator"
)]
pub struct SshArgs {
    /// The host's id, as enrollment made it.
    pub host_id: String,
    /// The private key to use (its `.pub` beside it), instead of the agent's or the
    /// state directory's.
    #[arg(long)]
    pub identity: Option<PathBuf>,
    /// Why, for the audit: at most 200 printable ASCII characters.
    #[arg(long, default_value = "hk ssh")]
    pub reason: String,
    /// Arguments for ssh, after the destination: a command to run, for one.
    #[arg(last = true)]
    pub ssh_args: Vec<OsString>,
}

/// `hk ssh-proxy <host-id> [--identity <file>] [--reason <text>]`: the byte stream ssh's
/// `ProxyCommand` uses.
#[derive(Debug, Parser)]
#[command(
    name = "hk ssh-proxy",
    about = "The byte stream ssh's ProxyCommand uses: stdin and stdout are the stream to the host's port 22"
)]
pub struct ProxyArgs {
    /// The host's id.
    pub host_id: String,
    /// As for `hk ssh`.
    #[arg(long)]
    pub identity: Option<PathBuf>,
    /// As for `hk ssh`.
    #[arg(long, default_value = "hk ssh-proxy")]
    pub reason: String,
}

/// `hk ssh-revoke (--operator <name> | --serial <n> | --host <id>)`.
#[derive(Debug, Parser)]
#[command(
    name = "hk ssh-revoke",
    about = "Revoke certificates: an operator's, one by serial, or the one held for a host"
)]
pub struct RevokeArgs {
    /// Every certificate of this operator.
    #[arg(long, conflicts_with_all = ["serial", "host"], required_unless_present_any = ["serial", "host"])]
    pub operator: Option<String>,
    /// One certificate by serial.
    #[arg(long, conflicts_with_all = ["operator", "host"])]
    pub serial: Option<u64>,
    /// The certificate this machine holds for this host.
    #[arg(long, conflicts_with_all = ["operator", "serial"])]
    pub host: Option<String>,
}

/// `hk ssh-status <host-id>`.
#[derive(Debug, Parser)]
#[command(
    name = "hk ssh-status",
    about = "The pinned host keys of a host and the certificate held for it"
)]
pub struct StatusArgs {
    /// The host's id.
    pub host_id: String,
}

/// Parses `args` (what follows the verb) as `T`; a usage text names the verb (`hk ssh`).
pub fn parse<T: Parser>(args: &[OsString]) -> Result<T, clap::Error> {
    let name = OsString::from(T::command().get_name());
    T::try_parse_from(std::iter::once(name).chain(args.iter().cloned()))
}
