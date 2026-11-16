//! The command line of `hkssh`. Every setting is a flag (no environment variable); `hkctl` fills
//! in the ones it knows (`--state-dir` under the operator's home) and passes the rest through.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// `hkssh [flags] <command>`.
#[derive(Debug, Parser)]
#[command(
    name = "hkssh",
    version,
    about = "SSH to a host through the coordinator"
)]
pub struct Cli {
    #[command(flatten)]
    pub global: Global,
    #[command(subcommand)]
    pub command: Command,
}

/// The flags every command takes, before or after the command's name. The coordinator flags are
/// optional here only because `known-host` takes none; each command that calls the coordinator
/// refuses their absence.
#[derive(Debug, Clone, Args)]
pub struct Global {
    /// The coordinator's operator listener: `https://<host>[:<port>]`.
    #[arg(long, global = true)]
    pub endpoint: Option<String>,
    /// The PEM file of the CA the coordinator's certificate must chain to.
    #[arg(long, global = true)]
    pub ca_file: Option<PathBuf>,
    /// The file holding the operator's token; it must not be readable by group or others.
    #[arg(long, global = true)]
    pub token_file: Option<PathBuf>,
    /// Where the key, the certificates and the pins are kept (mode 0700).
    #[arg(long, global = true)]
    pub state_dir: Option<PathBuf>,
    /// The longest a unary call, and the connection to the coordinator, may take.
    #[arg(long, global = true, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..=999_999))]
    pub timeout_secs: u64,
    /// The HTTP/2 PING interval of a tunnel, which keeps a quiet session open through Envoy and
    /// NAT (the coordinator's `--ssh-keepalive-s`).
    #[arg(long, global = true, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..=3600))]
    pub keepalive_secs: u64,
    /// The `ssh` program `hkssh ssh` runs.
    #[arg(long, global = true, default_value = "ssh")]
    pub ssh_program: PathBuf,
    /// The `ssh-keygen` program that makes the operator's key.
    #[arg(long, global = true, default_value = "ssh-keygen")]
    pub ssh_keygen_program: PathBuf,
    /// The `ssh-add` program that reads the agent's first key.
    #[arg(long, global = true, default_value = "ssh-add")]
    pub ssh_add_program: PathBuf,
    /// The agent's socket, for `ssh-add` and `ssh`; without it they use the agent their own
    /// environment names.
    #[arg(long, global = true)]
    pub ssh_agent_socket: Option<PathBuf>,
}

/// One command.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Get a certificate for a host and run ssh to it through the coordinator.
    Ssh {
        /// The host's id, as enrollment made it.
        host_id: String,
        /// The private key to use (its `.pub` beside it), instead of the agent's or the
        /// state directory's.
        #[arg(long)]
        identity: Option<PathBuf>,
        /// Why, for the audit: at most 200 printable ASCII characters.
        #[arg(long, default_value = "hkctl ssh")]
        reason: String,
        /// Arguments for ssh, after the destination: a command to run, for one.
        #[arg(last = true)]
        ssh_args: Vec<String>,
    },
    /// The byte stream ssh's `ProxyCommand` uses: stdin and stdout are the stream to the host's
    /// port 22. Gets a certificate for the host first when there is no valid one.
    Proxy {
        /// The host's id.
        host_id: String,
        /// As for `ssh`.
        #[arg(long)]
        identity: Option<PathBuf>,
        /// As for `ssh`.
        #[arg(long, default_value = "hkctl ssh-proxy")]
        reason: String,
    },
    /// Revoke certificates: an operator's, one by serial, or the one held for a host.
    Revoke {
        /// Every certificate of this operator.
        #[arg(long, conflicts_with_all = ["serial", "host"], required_unless_present_any = ["serial", "host"])]
        operator: Option<String>,
        /// One certificate by serial.
        #[arg(long, conflicts_with_all = ["operator", "host"])]
        serial: Option<u64>,
        /// The certificate this machine holds for this host.
        #[arg(long, conflicts_with_all = ["operator", "serial"])]
        host: Option<String>,
    },
    /// The pinned host keys of a host and the certificate held for it.
    Status {
        /// The host's id.
        host_id: String,
    },
    /// ssh's `KnownHostsCommand`: prints the known-hosts line for the key ssh presents when its
    /// fingerprint is one of the pins in the file, and nothing otherwise.
    KnownHost {
        /// The file of pinned fingerprints, one per line.
        pins: PathBuf,
        /// `%H`, `%t`, `%K` and `%f`, as ssh expands them (the last three are absent when ssh
        /// only asks which key types the host may offer).
        #[arg(num_args = 0..=4)]
        rest: Vec<String>,
    },
}
