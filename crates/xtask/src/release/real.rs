//! The real adapters: the one module that builds them, and the one place a release verb starts a
//! process. `SystemExec` runs a program and waits for it; every adapter (`git`, `gh`, the binary
//! a `check-binary` runs) runs its program through it. [`connect`] is called by the `xtask`
//! binary only, for a verb that reads a port; no test names `SystemExec` or `connect` (the test
//! `release_commands_never_call_the_network_in_tests` holds that), so no test reaches git, GitHub
//! or the network.

use std::process::Command;

use super::check_binary::VersionOf;
use super::git::GitCli;
use super::github::Gh;
use super::ports::{Adapters, Exec, Ports, Ran};

/// Starts processes.
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

/// The adapters a release verb runs against outside a test.
pub(crate) struct Real {
    git: GitCli<SystemExec>,
    gh: Gh<SystemExec>,
    version_of: VersionOf<SystemExec>,
}

/// The real adapters: `git` and `gh` from the `PATH`, in the current directory.
pub(crate) fn connect() -> Real {
    Real {
        git: GitCli::new(SystemExec),
        gh: Gh::new(SystemExec),
        version_of: VersionOf::new(SystemExec),
    }
}

impl Adapters for Real {
    fn ports(&self) -> Ports<'_> {
        Ports {
            git: &self.git,
            runs: &self.gh,
            store: &self.gh,
            verifier: &self.gh,
            run_version: &self.version_of,
        }
    }
}
