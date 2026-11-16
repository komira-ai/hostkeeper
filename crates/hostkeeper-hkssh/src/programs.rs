//! The programs `hkssh` runs: `ssh-keygen` to make a key, `ssh-add` to read the agent's, and
//! `ssh`. A port, so the commands are measured against a double that records what it is asked.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::error::Error;

/// What `hkssh` asks of the machine's OpenSSH.
pub trait Programs {
    /// Makes an Ed25519 key with no passphrase at `private` (and `private.pub`); never prints it.
    fn generate_key(&self, private: &Path) -> Result<(), Error>;

    /// The agent's first public key, `<type> <base64>`, or `None` when there is no agent or it
    /// holds none.
    fn agent_key(&self) -> Option<String>;

    /// Runs `ssh` with `args`, its standard streams being this process's; its exit code.
    fn run_ssh(&self, args: &[String]) -> Result<i32, Error>;
}

/// The OpenSSH programs by the paths the flags name.
#[derive(Debug, Clone)]
pub struct System {
    pub ssh: PathBuf,
    pub keygen: PathBuf,
    pub add: PathBuf,
    /// The agent's socket given to `ssh-add` and `ssh`; `None` leaves the agent they inherit.
    pub agent_socket: Option<PathBuf>,
}

impl System {
    fn command(&self, program: &Path) -> Command {
        let mut command = Command::new(program);
        if let Some(socket) = &self.agent_socket {
            command.env("SSH_AUTH_SOCK", socket);
        }
        command
    }
}

fn program_error(program: &Path, reason: impl ToString) -> Error {
    Error::Program {
        program: program.display().to_string(),
        reason: reason.to_string(),
    }
}

impl Programs for System {
    fn generate_key(&self, private: &Path) -> Result<(), Error> {
        let status = Command::new(&self.keygen)
            .args(["-q", "-t", "ed25519", "-N", "", "-C", "hkssh", "-f"])
            .arg(private)
            .stdin(Stdio::null())
            .status()
            .map_err(|e| program_error(&self.keygen, e))?;
        if status.success() {
            Ok(())
        } else {
            Err(program_error(&self.keygen, format!("exited {status}")))
        }
    }

    fn agent_key(&self) -> Option<String> {
        let output = self
            .command(&self.add)
            .arg("-L")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8(output.stdout).ok()?;
        normalize_public_key(text.lines().next()?)
    }

    fn run_ssh(&self, args: &[String]) -> Result<i32, Error> {
        let status = self
            .command(&self.ssh)
            .args(args)
            .status()
            .map_err(|e| program_error(&self.ssh, e))?;
        // A program ended by a signal has no code; 255 is what ssh itself exits with on an error.
        Ok(status.code().unwrap_or(255))
    }
}

/// `<type> <base64>` of a public key line, `None` when the line has no such two fields.
pub fn normalize_public_key(line: &str) -> Option<String> {
    let mut fields = line.split_whitespace();
    let kind = fields.next()?;
    let blob = fields.next()?;
    let known = kind.starts_with("ssh-") || kind.starts_with("ecdsa-");
    (known && !kind.contains("-cert-")).then(|| format!("{kind} {blob}"))
}
