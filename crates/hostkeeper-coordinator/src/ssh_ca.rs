//! `hostkeeper-coordinator ssh-ca init` and `ssh-ca sign` (docs/design/ssh-through-
//! coordinator.md, section 5): the verbs the owner runs on a machine of their own, not on the
//! coordinator.
//!
//! `init --out <dir>` makes one Ed25519 CA key, once, offline: it writes the private key file
//! into the directory (mode 0600, and never over a file that is there), and prints the public key
//! and its fingerprint, nothing of the private key. The owner loads the file as the Swarm secret
//! `--ssh-ca-secret-file` reads, and keeps the offline copy as `docs/operations/ca-backup.md`
//! keeps the other CA's. Why the coordinator does not make it at its first start: N instances
//! would each make one.
//!
//! `sign` makes a certificate by the same rules as the coordinator's, for a host the coordinator
//! may not sign for (the control plane's, section 5), on the owner's machine. It prints the
//! certificate line and nothing of the key. With no database to give a serial, the serial is
//! random and in the high half of the numbers, where the coordinator's sequence does not come.

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use hostkeeper_enrollment::names::HostId;
use hostkeeper_wire::secret::{SecretError, read_secret};

use crate::backup::Report;
use crate::enrollment::Random;
use crate::ssh_cert::{CaKey, KeyError, Spec, UserKey};
use crate::ssh_sign::principal;
use crate::tokens::TokenName;

/// The most bytes of a CA secret file: two keys, with room.
const MAX_SECRET_BYTES: u64 = 16 * 1024;

/// The most bytes of a public key file.
const MAX_PUBLIC_BYTES: u64 = 8 * 1024;

/// The longest validity `sign` and the coordinator give, in seconds.
pub const MAX_TTL_SECS: u64 = 28_800;

/// The name `init` gives the private key file in the directory.
pub const KEY_FILE: &str = "ssh_user_ca";

/// The comment the private key file carries.
const COMMENT: &str = "hostkeeper-ssh-user-ca";

/// The serials `sign` draws are at least this: the coordinator's sequence starts at 1.
const OFFLINE_SERIALS: u64 = 1 << 62;

/// The verbs of `ssh-ca`.
#[derive(Clone, Debug, Args)]
pub struct SshCa {
    /// What to do.
    #[command(subcommand)]
    pub command: SshCaCommand,
}

/// The verbs of `ssh-ca`.
#[derive(Clone, Debug, Subcommand)]
pub enum SshCaCommand {
    /// Make an SSH user CA key, once: the private key file goes into `--out`, mode 0600, and
    /// the public key is printed.
    Init(Init),
    /// Sign a user certificate for one host with a CA key file, as the coordinator does, and
    /// print it.
    Sign(Sign),
}

/// The flags of `ssh-ca init`.
#[derive(Clone, Debug, Args)]
pub struct Init {
    /// The directory the private key file `ssh_user_ca` is written into; made, mode 0700, when
    /// it is not there. A key file that is there is never replaced.
    #[arg(long)]
    pub out: PathBuf,
}

/// The flags of `ssh-ca sign`.
#[derive(Clone, Debug, Args)]
pub struct Sign {
    /// The CA key file `init` wrote, mode 0600 or 0400, owned by the caller; its first key signs.
    #[arg(long)]
    pub ca_key_file: PathBuf,
    /// The operator's public key, a file in `authorized_keys` syntax (`id_ed25519.pub`).
    #[arg(long)]
    pub public_key_file: PathBuf,
    /// The host id the certificate is for: its one principal is `hk-host-<host id>`.
    #[arg(long)]
    pub host_id: String,
    /// The operator's name, 1 to 32 bytes of a-z, 0-9 and -, in the certificate's key id.
    #[arg(long)]
    pub operator: String,
    /// How long the certificate is valid, in seconds, 60 to 28800.
    #[arg(long, default_value_t = 3600, value_parser = clap::value_parser!(u64).range(60..=MAX_TTL_SECS))]
    pub ttl_s: u64,
}

/// Why a verb of `ssh-ca` did not finish.
#[derive(Debug, thiserror::Error)]
pub enum SshCaError {
    /// The directory or the key file could not be made.
    #[error("{}: {source}", .path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// A file could not be read, or breaks the rule of a secret file.
    #[error(transparent)]
    Secret(#[from] SecretError),
    /// The public key file could not be read.
    #[error("{}: {source}", .path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The CA key file is not an unencrypted Ed25519 OpenSSH key.
    #[error("--ca-key-file: {0}")]
    CaKey(KeyError),
    /// The public key is not one the CA signs.
    #[error("--public-key-file: {0}")]
    PublicKey(KeyError),
    /// The host id is not one.
    #[error("--host-id: {}", HostId::GRAMMAR)]
    HostId,
    /// The operator is not a token's name.
    #[error("--operator: not a token name")]
    Operator,
    /// The system gave no random bytes.
    #[error("the system gave no random bytes")]
    Random,
}

/// Runs the verb `command`; `now` is the wall clock's second and `random` the system's bytes.
///
/// # Errors
/// [`SshCaError`]: nothing was written or signed.
pub fn run(command: &SshCaCommand, now: u64, random: &Random) -> Result<Report, SshCaError> {
    match command {
        SshCaCommand::Init(init) => init_in(&init.out, random),
        SshCaCommand::Sign(sign) => sign_with(sign, now, random),
    }
}

/// `init`: the key file in `out`, and the report of its public key.
fn init_in(out: &Path, random: &Random) -> Result<Report, SshCaError> {
    let write = |path: &Path, source| SshCaError::Write {
        path: path.to_owned(),
        source,
    };
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(out)
        .map_err(|e| write(out, e))?;
    let (key, pem) = CaKey::generate(COMMENT, &**random).map_err(|_| SshCaError::Random)?;
    let path = out.join(KEY_FILE);
    // `create_new`: a key that is there is never replaced.
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| write(&path, e))?;
    file.write_all(pem.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|e| write(&path, e))?;
    Ok(Report::of(vec![
        format!("wrote the private key {}", path.display()),
        format!("public key: {} {COMMENT}", key.public_line()),
        format!("fingerprint: {}", key.fingerprint()),
    ]))
}

/// `sign`: the certificate line.
fn sign_with(sign: &Sign, now: u64, random: &Random) -> Result<Report, SshCaError> {
    let secret = read_secret(&sign.ca_key_file, "--ca-key-file", MAX_SECRET_BYTES)?;
    let keys =
        CaKey::parse_secret(&String::from_utf8_lossy(&secret)).map_err(SshCaError::CaKey)?;
    let host = HostId::parse(&sign.host_id).map_err(|_| SshCaError::HostId)?;
    if sign.operator.parse::<TokenName>().is_err() {
        return Err(SshCaError::Operator);
    }
    let public = read_public(&sign.public_key_file)?;
    let parsed = UserKey::parse(public.trim()).map_err(SshCaError::PublicKey)?;
    let mut draw = [0u8; 8];
    if !random(&mut draw) {
        return Err(SshCaError::Random);
    }
    let serial = OFFLINE_SERIALS | (u64::from_be_bytes(draw) >> 2);
    let expires = now.saturating_add(sign.ttl_s);
    let key_id = format!("{}/{serial}", sign.operator);
    let principal = principal(host.as_str());
    let certificate = keys[0]
        .sign_user(
            &Spec {
                key: &parsed.key,
                serial,
                key_id: &key_id,
                principal: &principal,
                valid_after: now.saturating_sub(60),
                valid_before: expires.saturating_add(1),
            },
            &**random,
        )
        .map_err(|_| SshCaError::Random)?;
    Ok(Report::of(vec![certificate]))
}

/// The text of the public key file at `path`.
fn read_public(path: &Path) -> Result<String, SshCaError> {
    let read = |source| SshCaError::Read {
        path: path.to_owned(),
        source,
    };
    let meta = fs::metadata(path).map_err(read)?;
    if meta.len() > MAX_PUBLIC_BYTES {
        return Err(read(io::Error::other("too long for a public key")));
    }
    fs::read_to_string(path).map_err(read)
}

#[cfg(test)]
#[path = "ssh_ca_tests.rs"]
mod tests;
