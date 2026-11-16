//! `hkssh`: the operator's side of SSH through the coordinator
//! (`docs/design/ssh-through-coordinator.md`, sections 7 and 8).
//!
//! - `proxy <host-id>`: ssh's `ProxyCommand`. Its stdin and stdout are a byte stream to port 22
//!   of the host, carried by the coordinator's `Ssh/Tunnel`. It gets a certificate for the host
//!   first when it holds none that is valid.
//! - `ssh <host-id> [-- ssh args]`: reads the pinned host keys, gets a one-host certificate for
//!   the operator's key, and runs `ssh` with this program as its proxy and as the judge of the
//!   host key (`KnownHostsCommand` accepts only a key whose fingerprint is pinned, with
//!   `StrictHostKeyChecking yes`).
//! - `revoke` and `status`: `Ssh/Revoke`, and `Ssh/HostKeys` with the certificate held.
//! - `known-host`: the command ssh runs to ask whether a key is the pinned one.
//!
//! Every setting is a flag. The operator's token is read from the file `--token-file` names for
//! each run, and is in no argument, no file of this program and no environment. `hkctl ssh`,
//! `ssh-proxy`, `ssh-revoke` and `ssh-status` run this program, passing the flags they took.

mod access;
mod cli;
mod client;
mod error;
mod programs;
mod run;
mod state;
mod tunnel;

use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::Parser as _;

pub use access::{Identity, Pins, known_host_line};
pub use cli::{Cli, Command, Global};
pub use error::Error;
pub use programs::{Programs, System, normalize_public_key};
pub use run::{Env, run};
pub use state::{CertInfo, State, check_host_id};

/// The program: parses the command line, runs the command on a runtime of this thread and exits
/// with its code (2 for a usage error, 1 for any other failure, ssh's own code for `ssh`).
pub fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let _ = error.print();
            return ExitCode::from(u8::from(error.use_stderr()) * 2);
        }
    };
    match execute(cli) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(255)),
        Err(error) => {
            eprintln!("hkssh: {error}");
            ExitCode::from(error.exit_code())
        }
    }
}

fn execute(cli: Cli) -> Result<i32, Error> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::Usage(format!("cannot start the runtime: {e}")))?;
    let programs = System {
        ssh: cli.global.ssh_program.clone(),
        keygen: cli.global.ssh_keygen_program.clone(),
        add: cli.global.ssh_add_program.clone(),
        agent_socket: cli.global.ssh_agent_socket.clone(),
    };
    let env = Env {
        programs: &programs,
        now: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs()),
        exe: std::env::current_exe().unwrap_or_else(|_| "hkssh".into()),
    };
    let code = runtime.block_on(run(
        cli,
        &env,
        tokio::io::stdin(),
        tokio::io::stdout(),
        &mut std::io::stderr(),
    ));
    // The blocking read of stdin may still be waiting for a byte; do not wait for it.
    runtime.shutdown_background();
    code
}
