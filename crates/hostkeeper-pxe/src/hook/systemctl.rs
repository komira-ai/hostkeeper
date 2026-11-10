//! The service manager, as the steps use it: `systemctl <args>`, run as `/usr/bin/systemctl`
//! with an empty environment and no standard input or output, its exit code returned.

use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Where `systemctl` is on a host.
const SYSTEMCTL: &str = "/usr/bin/systemctl";

/// Runs `systemctl`.
pub(crate) trait Systemctl {
    /// Runs it with `args` and returns its exit code (`-1` when a signal ended it).
    ///
    /// # Errors
    /// The program could not be started.
    fn run(&self, args: &[&str]) -> io::Result<i32>;
}

/// The host's `systemctl`.
pub(crate) struct HostSystemctl {
    program: PathBuf,
}

impl HostSystemctl {
    /// `/usr/bin/systemctl`.
    pub(crate) fn host() -> HostSystemctl {
        HostSystemctl::at(SYSTEMCTL.into())
    }

    /// The program at `program`.
    pub(crate) fn at(program: PathBuf) -> HostSystemctl {
        HostSystemctl { program }
    }
}

impl Systemctl for HostSystemctl {
    fn run(&self, args: &[&str]) -> io::Result<i32> {
        let status = Command::new(&self.program)
            .args(args)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .status()?;
        Ok(status.code().unwrap_or(-1))
    }
}
