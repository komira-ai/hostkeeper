//! The real adapters: the one module that builds them, and the one place a release verb starts a
//! process. `SystemExec` runs a program and waits for it; every adapter (`git`, `gh`, the binary
//! a `check-binary` runs, `pkgbuild`, `cargo deny`) runs its program through it, and the
//! coordinator updater's offline `gh` runs through it with an environment of its own
//! ([`ExecSealed`]). [`connect`] is called by the `xtask` binary only, for a verb that reads a
//! port; no test names `SystemExec` or `connect` (the test
//! `release_commands_never_call_the_network_in_tests` holds that), so no test reaches git, GitHub
//! or the network.

use std::process::Command;

use super::Wired;
use super::ports::{Exec, ExecSealed, Ran, Sealed};

/// Starts processes.
#[derive(Clone, Copy)]
struct SystemExec;

impl Exec for SystemExec {
    fn spawn(&self, program: &str, args: &[String]) -> std::io::Result<Ran> {
        let out = Command::new(program).args(args).output()?;
        Ok(Ran {
            success: out.status.success(),
            stdout: out.stdout,
            stderr: out.stderr,
        })
    }
}

impl ExecSealed for SystemExec {
    /// Runs `run.program` in `run.cwd` with exactly `run.env`: nothing of this process's own
    /// environment reaches it.
    fn spawn_sealed(&self, run: &Sealed) -> std::io::Result<Ran> {
        let out = Command::new(&run.program)
            .args(&run.args)
            .env_clear()
            .envs(run.env.iter().map(|(k, v)| (k, v)))
            .current_dir(&run.cwd)
            .output()?;
        Ok(Ran {
            success: out.status.success(),
            stdout: out.stdout,
            stderr: out.stderr,
        })
    }
}

/// The process runner of `mdm-signing-test build` (`openssl`, `pkgbuild`, `productbuild`,
/// `security`, `productsign` and `pkgutil` from the `PATH`, or the path named), of the channel
/// verbs, and of `hostkeeper-coordinator-update` (`curl`, and its offline `gh` sealed).
pub(crate) fn exec() -> impl Exec + ExecSealed {
    SystemExec
}

/// The real adapters: `git`, `gh`, `pkgbuild` and `cargo` from the `PATH`, in the current
/// directory, and the binary a `check-binary` names, each run as a process.
pub(crate) fn connect() -> impl super::ports::Adapters {
    Wired::new(SystemExec)
}
