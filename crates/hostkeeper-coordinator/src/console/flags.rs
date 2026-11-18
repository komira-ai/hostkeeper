//! The coordinator's `--console-*` flags (`docs/design/console-broker.md`, decisions D5, D6, D8,
//! D9 and D12), and the console they make.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, ValueEnum};
use hostkeeper_console::ipmi::{Privilege, Settings};
use hostkeeper_console::session::Sessions;

use super::files::{ConsoleFiles, ConsoleFilesError, Paths};
use super::{Console, Ipmitool};

/// Where the BMC tool is, by default.
pub const DEFAULT_IPMITOOL: &str = "/usr/bin/ipmitool";

/// The privilege level of a BMC session, as the flag takes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum PrivilegeFlag {
    /// `USER`.
    User,
    /// `OPERATOR`: enough for power and Serial over LAN.
    Operator,
    /// `ADMINISTRATOR`.
    Administrator,
}

/// The console's flags.
#[derive(Clone, Debug, Args)]
pub struct ConsoleFlags {
    /// Who may use the console: a TOML file with an `[[operator]]` table for each, with its `name`
    /// (a name of the tokens file) and `power` (true unless false: may also call `Console.Power`
    /// and `Console.SetBootDevice`). No secret: any regular file of at most 64 KiB. Needs
    /// --console-targets-file and --console-bmc-credential-file. Read again every
    /// --tls-reload-secs. Default: nobody has console access, and the other two files are not
    /// read.
    #[arg(
        long,
        value_name = "FILE",
        requires_all = ["console_targets_file", "console_bmc_credential_file"]
    )]
    pub console_operators_file: Option<PathBuf>,
    /// The address of each BMC and Mac: a TOML file with a `[[target]]` table for each, with its
    /// `mac` (a BMC's MAC, as the host's inventory record names it, or a Mac's data MAC) and its
    /// `address` (an IP address or a DNS name, no port). No secret: any regular file of at most
    /// 1 MiB. Read again every --tls-reload-secs.
    #[arg(long, value_name = "FILE")]
    pub console_targets_file: Option<PathBuf>,
    /// The one user and password of every BMC: a TOML file of `user` and `password`; mode 0600 or
    /// 0400, owned by the coordinator's user. The password reaches the BMC tool in its
    /// environment only. Read again every --tls-reload-secs.
    #[arg(long, value_name = "FILE")]
    pub console_bmc_credential_file: Option<PathBuf>,
    /// The BMC tool, `ipmitool`, that the coordinator runs for each power and boot device call.
    #[arg(long, value_name = "PATH", default_value = DEFAULT_IPMITOOL)]
    pub console_ipmitool: PathBuf,
    /// The RMCP+ cipher suite of a BMC session (`ipmitool -C`).
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u8).range(0..=20))]
    pub console_ipmi_cipher: u8,
    /// The privilege level of a BMC session (`ipmitool -L`).
    #[arg(long, value_enum, default_value_t = PrivilegeFlag::Administrator)]
    pub console_ipmi_privilege: PrivilegeFlag,
    /// How long the BMC tool waits for each answer, in seconds (`ipmitool -N`).
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..=60))]
    pub console_ipmi_timeout_s: u32,
    /// How often the BMC tool sends a request again that had no answer (`ipmitool -R`).
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u32).range(0..=10))]
    pub console_ipmi_retries: u32,
    /// How long a console session waits to be attached, in seconds.
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..=600))]
    pub console_attach_s: u64,
    /// The most console sessions at once.
    #[arg(long, default_value_t = 8, value_parser = clap::value_parser!(u64).range(1..=256))]
    pub console_max_sessions: u64,
}

impl ConsoleFlags {
    /// The settings of a BMC session.
    #[must_use]
    pub fn settings(&self) -> Settings {
        Settings {
            cipher: self.console_ipmi_cipher,
            privilege: match self.console_ipmi_privilege {
                PrivilegeFlag::User => Privilege::User,
                PrivilegeFlag::Operator => Privilege::Operator,
                PrivilegeFlag::Administrator => Privilege::Administrator,
            },
            timeout_secs: self.console_ipmi_timeout_s,
            retries: self.console_ipmi_retries,
        }
    }

    /// The console the flags make, its files read now.
    ///
    /// # Errors
    /// [`ConsoleFilesError`]: a file is refused.
    pub fn console(&self) -> Result<Console, ConsoleFilesError> {
        let files = match (
            &self.console_operators_file,
            &self.console_targets_file,
            &self.console_bmc_credential_file,
        ) {
            (Some(operators), Some(targets), Some(credential)) => ConsoleFiles::read(Paths {
                operators: operators.clone(),
                targets: targets.clone(),
                credential: credential.clone(),
            })?,
            _ => ConsoleFiles::default(),
        };
        let settings = self.settings();
        let max = usize::try_from(self.console_max_sessions).unwrap_or(usize::MAX);
        Ok(Console::new(
            files,
            Arc::new(Ipmitool::new(&self.console_ipmitool, &settings)),
            settings,
            Sessions::new(max, Duration::from_secs(self.console_attach_s)),
        ))
    }
}
