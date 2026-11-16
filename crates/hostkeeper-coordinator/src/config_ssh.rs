//! The flags of the SSH user CA and its signing (`docs/design/ssh-through-coordinator.md`,
//! section 10), and the start's use of them: the CA secret read, the operators file checked, and
//! the ledger of the database handed to the signing.
//!
//! Every flag has a default, and the feature is off without `--ssh-ca-secret-file`: the
//! coordinator then signs nothing and answers `Ssh` `not_configured`. The CA key is a secret every
//! instance reads (a Swarm secret), never a file of one task's disk and never in the database; the
//! database holds the certificate table, the serial sequence and the audit.

use std::collections::BTreeSet;
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, ValueEnum};
use futures::future::{Either, select};
use hostkeeper_enrollment::names::HostId;
use hostkeeper_wire::secret::{SecretError, read_secret};

use super::{Serve, StartError};
use crate::enrollment::{Random, os_random};
use crate::service::Coordinator;
use crate::ssh_book::Book;
use crate::ssh_ca::MAX_TTL_SECS;
use crate::ssh_cert::{CaKey, KeyError};
use crate::ssh_ledger::Ledger;
use crate::ssh_operators::{Operators, OperatorsError};
use crate::ssh_sign::Signing;
use crate::ssh_tunnel::{Limits, Tunnels, instance_id};

/// The most bytes of a CA secret: two keys, with room.
const MAX_SECRET_BYTES: u64 = 16 * 1024;

/// The port of a host's sshd, which a tunnel dials: not a flag, every host runs it on 22.
pub(crate) const SSHD_PORT: u16 = 22;

/// Which key of the CA secret signs, while a rotation runs (section 5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum SigningKey {
    /// The first key of the secret.
    #[default]
    Current,
    /// The second key, once every host trusts both.
    Next,
}

/// The flags of the SSH user CA.
#[derive(Clone, Debug, Args)]
pub struct SshFlags {
    /// The SSH user CA of the fleet: a file of one OpenSSH private key made by
    /// `ssh-ca init` (or two, `current` first and `next` second, while the CA is rotated), mode
    /// 0600 or 0400, owned by the coordinator's user, a Swarm secret in a deployment. Without it
    /// the coordinator signs no certificate and tunnels nothing. Requires
    /// --database-dsn-file, which holds the certificates, their serial sequence and the audit,
    /// and --ssh-operators-file.
    #[arg(long, value_name = "FILE", requires_all = ["database_dsn_file", "ssh_operators_file"])]
    pub ssh_ca_secret_file: Option<PathBuf>,
    /// Who may have a certificate, and for which hosts: a TOML file of `[[operator]]` tables
    /// with `name`, `classes` (`fleet`, `control-plane` or a host group) and `hosts` (host ids).
    /// Read at start, which refuses a file that does not parse, and again at every call. Default
    /// deny: an operator it does not list, or a host outside the entry's lists, is refused.
    #[arg(long, value_name = "FILE", requires = "ssh_ca_secret_file")]
    pub ssh_operators_file: Option<PathBuf>,
    /// The private key of the control-plane CA, as --ssh-ca-secret-file takes it (one key). Only
    /// when the owner has the coordinator sign for the control-plane hosts; without it a
    /// certificate for those hosts is made offline with `ssh-ca sign`.
    #[arg(long, value_name = "FILE", requires = "ssh_ca_secret_file")]
    pub ssh_cp_ca_secret_file: Option<PathBuf>,
    /// A host id of the control-plane class, which trusts the control-plane CA and not the
    /// fleet's. Repeatable.
    #[arg(long, value_name = "HOST_ID", requires = "ssh_ca_secret_file")]
    pub ssh_control_plane_host: Vec<String>,
    /// Which key of --ssh-ca-secret-file signs: `current`, the first, or `next`, the second
    /// (only once every host trusts both).
    #[arg(long, value_enum, default_value_t = SigningKey::Current, requires = "ssh_ca_secret_file")]
    pub ssh_signing_key: SigningKey,
    /// How long a certificate is valid, in seconds, 60 to 28800.
    #[arg(
        long,
        default_value_t = 3600,
        value_parser = clap::value_parser!(u64).range(60..=MAX_TTL_SECS),
        requires = "ssh_ca_secret_file"
    )]
    pub ssh_cert_ttl_s: u64,
    /// The account a Linux host takes the certificate for.
    #[arg(long, default_value = "root", requires = "ssh_ca_secret_file")]
    pub ssh_login_user_linux: String,
    /// The account a Mac takes the certificate for; empty, the Mac's daemon picks its one local
    /// administrator.
    #[arg(long, default_value = "", requires = "ssh_ca_secret_file")]
    pub ssh_login_user_macos: String,
    /// How recent a host's last heartbeat must be for a tunnel to be opened to it, in seconds,
    /// 10 to 3600. A host that has not been heard in that time is refused `host_not_live`.
    #[arg(
        long,
        default_value_t = 120,
        value_parser = clap::value_parser!(u64).range(10..=3600),
        requires = "ssh_ca_secret_file"
    )]
    pub ssh_live_s: u64,
    /// How long a tunnel may carry no byte either way before it is closed `idle`, in seconds,
    /// 10 to 86400.
    #[arg(
        long,
        default_value_t = 900,
        value_parser = clap::value_parser!(u64).range(10..=86_400),
        requires = "ssh_ca_secret_file"
    )]
    pub ssh_idle_s: u64,
    /// How long a tunnel may stay open before it is closed `max_session`, in seconds, 60 to
    /// 86400.
    #[arg(
        long,
        default_value_t = 28_800,
        value_parser = clap::value_parser!(u64).range(60..=86_400),
        requires = "ssh_ca_secret_file"
    )]
    pub ssh_max_session_s: u64,
    /// How long the connection to a host's sshd may take, in seconds, 1 to 60.
    #[arg(
        long,
        default_value_t = 5,
        value_parser = clap::value_parser!(u64).range(1..=60),
        requires = "ssh_ca_secret_file"
    )]
    pub ssh_dial_timeout_s: u64,
    /// The most tunnels this coordinator instance holds at once, 1 to 1024.
    #[arg(
        long,
        default_value_t = 16,
        value_parser = clap::value_parser!(u64).range(1..=1024),
        requires = "ssh_ca_secret_file"
    )]
    pub ssh_max_tunnels: u64,
    /// The most tunnels one operator holds at once over every instance, 1 to 64.
    #[arg(
        long,
        default_value_t = 4,
        value_parser = clap::value_parser!(u64).range(1..=64),
        requires = "ssh_ca_secret_file"
    )]
    pub ssh_max_tunnels_per_operator: u64,
    /// How often an instance looks for the tunnels that must close (a token or a certificate
    /// revoked, an operators file that no longer permits them), in seconds, 1 to 300.
    #[arg(
        long,
        default_value_t = 10,
        value_parser = clap::value_parser!(u64).range(1..=300),
        requires = "ssh_ca_secret_file"
    )]
    pub ssh_revoke_poll_s: u64,
    /// How long a stopping instance waits for its tunnels to end on their own, in seconds, 0 to
    /// 3600, before it closes them `coordinator_stopping`. The container's stop grace period must
    /// be at least this long.
    #[arg(
        long,
        default_value_t = 600,
        value_parser = clap::value_parser!(u64).range(0..=3600),
        requires = "ssh_ca_secret_file"
    )]
    pub ssh_drain_s: u64,
}

/// The limits of the tunnels `ssh` names, dialling `port`.
fn limits(ssh: &SshFlags, port: u16) -> Limits {
    Limits {
        live_secs: ssh.ssh_live_s,
        idle: Duration::from_secs(ssh.ssh_idle_s),
        max_session: Duration::from_secs(ssh.ssh_max_session_s),
        dial_timeout: Duration::from_secs(ssh.ssh_dial_timeout_s),
        max_tunnels: usize::try_from(ssh.ssh_max_tunnels).unwrap_or(usize::MAX),
        per_operator: ssh.ssh_max_tunnels_per_operator,
        poll: Duration::from_secs(ssh.ssh_revoke_poll_s),
        drain: Duration::from_secs(ssh.ssh_drain_s),
        port,
    }
}

/// Why the SSH flags are refused at start.
#[derive(Debug, thiserror::Error)]
pub enum SshStartError {
    /// A secret file cannot be read.
    #[error(transparent)]
    Secret(#[from] SecretError),
    /// A secret file is not made of OpenSSH Ed25519 keys.
    #[error("{flag}: {why}")]
    Key { flag: &'static str, why: KeyError },
    /// `--ssh-cp-ca-secret-file` holds more than one key.
    #[error("--ssh-cp-ca-secret-file holds {0} keys: the control-plane CA has one")]
    ControlPlaneKeys(usize),
    /// `--ssh-signing-key next` and the secret holds one key.
    #[error("--ssh-signing-key next: --ssh-ca-secret-file holds one key")]
    NoNextKey,
    /// A `--ssh-control-plane-host` that is not a host id.
    #[error("--ssh-control-plane-host {0:?}: not a host id ({grammar})", grammar = HostId::GRAMMAR)]
    HostId(String),
    /// The operators file is refused.
    #[error("--ssh-operators-file: {0}")]
    Operators(#[from] OperatorsError),
    /// A login user that is empty (Linux) or not a plain account name.
    #[error("{0}: not an account name")]
    LoginUser(&'static str),
    /// The random source gave no bytes for the id of this instance.
    #[error("the random source gave no bytes for the id of this instance")]
    InstanceId,
}

/// Whether `name` is the name of an account: 1 to 32 bytes of letters, digits, `_`, `-` and `.`,
/// not starting with `-`.
fn account(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

/// The keys of the secret file `path`, named by `flag` in its errors.
fn keys_of(path: &Path, flag: &'static str) -> Result<Vec<CaKey>, SshStartError> {
    let text = read_secret(path, flag, MAX_SECRET_BYTES)?;
    CaKey::parse_secret(&String::from_utf8_lossy(&text))
        .map_err(|why| SshStartError::Key { flag, why })
}

/// The signing `flags` name over `ledger`: `None` without `--ssh-ca-secret-file`.
///
/// # Errors
/// [`SshStartError`]: a flag is refused, or a file is.
pub(super) fn signing(
    flags: &Serve,
    ledger: Option<Arc<dyn Ledger>>,
) -> Result<Option<Signing>, SshStartError> {
    let ssh = &flags.ssh;
    // clap requires the three together: the CA, the operators file and the database that holds
    // the ledger.
    let (Some(secret), Some(operators), Some(ledger)) =
        (&ssh.ssh_ca_secret_file, &ssh.ssh_operators_file, ledger)
    else {
        return Ok(None);
    };
    let fleet = keys_of(secret, "--ssh-ca-secret-file")?;
    let index = match ssh.ssh_signing_key {
        SigningKey::Current => 0,
        SigningKey::Next if fleet.len() > 1 => 1,
        SigningKey::Next => return Err(SshStartError::NoNextKey),
    };
    let control_plane = match &ssh.ssh_cp_ca_secret_file {
        Some(path) => {
            let mut keys = keys_of(path, "--ssh-cp-ca-secret-file")?;
            if keys.len() != 1 {
                return Err(SshStartError::ControlPlaneKeys(keys.len()));
            }
            keys.pop()
        }
        None => None,
    };
    let mut control_plane_hosts = BTreeSet::new();
    for host in &ssh.ssh_control_plane_host {
        control_plane_hosts.insert(
            HostId::parse(host)
                .map_err(|_| SshStartError::HostId(host.clone()))?
                .as_str()
                .to_owned(),
        );
    }
    if !account(&ssh.ssh_login_user_linux) {
        return Err(SshStartError::LoginUser("--ssh-login-user-linux"));
    }
    if !ssh.ssh_login_user_macos.is_empty() && !account(&ssh.ssh_login_user_macos) {
        return Err(SshStartError::LoginUser("--ssh-login-user-macos"));
    }
    // A file that does not parse is refused now; the same file is read again at every call.
    Operators::read(operators)?;
    for key in &fleet {
        tracing::info!(fingerprint = %key.fingerprint(), "an SSH user CA key of the fleet is loaded");
    }
    Ok(Some(Signing {
        fleet,
        signing: index,
        control_plane,
        control_plane_hosts,
        operators: operators.clone(),
        ttl_secs: ssh.ssh_cert_ttl_s,
        login_linux: ssh.ssh_login_user_linux.clone(),
        login_macos: ssh.ssh_login_user_macos.clone(),
        ledger,
        random: os_random(),
    }))
}

/// `coordinator` signing and holding tunnels as `flags` say, over `ledger` and `book`, which the
/// database gives: the coordinator itself without `--ssh-ca-secret-file`. The instance's id is
/// made from `random`.
///
/// # Errors
/// [`SshStartError`]: a flag is refused, or a file is, or `random` gave no bytes.
pub(super) fn attach(
    coordinator: Coordinator,
    flags: &Serve,
    ledger: Option<Arc<dyn Ledger>>,
    book: Option<Arc<dyn Book>>,
    random: &Random,
) -> Result<Coordinator, SshStartError> {
    // clap requires --ssh-ca-secret-file to come with the database, which gives both.
    let (Some(signing), Some(book)) = (signing(flags, ledger)?, book) else {
        return Ok(coordinator);
    };
    let instance = instance_id(random).ok_or(SshStartError::InstanceId)?;
    let tunnels = Tunnels::new(book, instance, limits(&flags.ssh, SSHD_PORT));
    Ok(coordinator.with_ssh(signing).with_tunnels(tunnels))
}

/// Drains the tunnels `coordinator` holds, if it holds any ([`Tunnels::drain`]), while `serving`
/// goes on serving: the listeners stay up for the tunnels, and the health listener answers
/// `draining`. Ends when they have ended, or with the error `serving` ends with.
///
/// # Errors
/// [`StartError`]: `serving` ended.
pub(super) async fn drained<S>(coordinator: &Coordinator, serving: S) -> Result<(), StartError>
where
    S: Future<Output = Result<Infallible, StartError>> + Unpin,
{
    let Some(tunnels) = coordinator.tunnels.as_ref() else {
        return Ok(());
    };
    match select(Box::pin(tunnels.drain()), serving).await {
        Either::Left(((), _)) => Ok(()),
        Either::Right((served, _)) => {
            let Err(why) = served;
            Err(why)
        }
    }
}

#[cfg(test)]
#[path = "config_ssh_tests.rs"]
mod tests;
