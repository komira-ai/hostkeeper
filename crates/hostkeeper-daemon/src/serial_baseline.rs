//! The serial console baseline of a Linux server, kept by the daemon on the port its config names
//! (`serial_console`; docs/design/console-broker.md, section 5.2), so that a host reinstalled by
//! hand gets it back from its daemon alone, and reported in every heartbeat
//! (`HeartbeatRequest.serial_console`).
//!
//! The baseline is the two files of [`hostkeeper_config::serial_console`], the network install's
//! seed's own: the GRUB file `/etc/default/grub.d/99_hostkeeper.cfg` (the kernel's console on the
//! port, and GRUB's terminal on a `ttyS<n>`) and the getty's drop-in that holds the port at 115200
//! baud; and the port's getty, `serial-getty@<port>.service`, enabled and running. A look, at the
//! first heartbeat and then at the first heartbeat at least [`RECHECK_SECS`] after the last (and
//! after the host's clock went back), goes in this order:
//!
//! 1. A host with no `update-grub` (a distribution whose GRUB reads no `grub.d`), or not under
//!    systemd, is `unsupported`, and nothing is written.
//! 2. Each file is written when it is not already the baseline's bytes, owned by the daemon's user
//!    with mode 0644 (through a temporary file and a rename); a file that is is not touched.
//! 3. `update-grub` runs when the GRUB file was written, or when the generated menu
//!    (`/boot/grub/grub.cfg`) does not hold the port's kernel argument; after it, the menu must
//!    hold it.
//! 4. A written drop-in is read by `systemctl daemon-reload`. The getty is enabled when it is not;
//!    a running one is restarted when its drop-in was written (it runs its old command until
//!    then), and a stopped one started.
//! 5. The running kernel's command line (`/proc/cmdline`) says the rest: `applied` when it holds
//!    the port's kernel argument, `reboot_pending` when it does not (the argument takes effect at
//!    the next boot). The daemon never reboots the host.
//!
//! A step that fails ends the look `failed`, with the step as its detail (`grub_file`,
//! `getty_drop_in`, `update_grub`, `grub_cfg`, `daemon_reload`, `getty`, `getty_enable`,
//! `getty_start`, `cmdline`); the next look starts again from step 1. On a host whose baseline is
//! in place a look writes nothing and runs one `systemctl show`. The daemon edits no file but its
//! two: a GRUB file or getty drop-in of another name that overrides the console stays as it is
//! (console.md, "Moving a server to the serial baseline", step 2).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use hostkeeper_config::serial_console::{GETTY_DROP_IN, GRUB_CONSOLE_FILE, SerialConsole};
use hostkeeper_launchd::PlatformKind;
use hostkeeper_link::{Log, SerialConsoleReport, SerialConsoleSource, UnixClock};
use hostkeeper_observe::CommandRunner;
use hostkeeper_observe::ProcessRunner;
use hostkeeper_observe::command::MAX_STDOUT;

use crate::mac_files::{Rendered, replace};

/// The least time between two looks, in seconds: ten minutes, so a file edited away is put back
/// within ten minutes and a heartbeat.
pub const RECHECK_SECS: u64 = 600;

/// The longest `systemctl` is given.
pub const SYSTEMCTL_DEADLINE: Duration = Duration::from_secs(20);

/// The longest `update-grub` is given: it runs on the heartbeat loop, whose watchdog is 60
/// seconds, and only when the GRUB file was written or the menu lacks the argument.
pub const UPDATE_GRUB_DEADLINE: Duration = Duration::from_secs(40);

/// The name the temporary file of a written file starts with.
const TEMPORARY: &str = "hostkeeper-serial-console";

/// Where the baseline stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BaselineState {
    /// The files are in place, the getty runs, and the running kernel's console is on the port.
    Applied,
    /// The files are in place and the getty runs; the kernel's console is on the port from the
    /// next boot.
    RebootPending,
    /// A step failed; the detail names it.
    Failed,
    /// The host cannot take the baseline; nothing was written.
    Unsupported,
}

impl BaselineState {
    /// The state's name on the wire.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            BaselineState::Applied => "applied",
            BaselineState::RebootPending => "reboot_pending",
            BaselineState::Failed => "failed",
            BaselineState::Unsupported => "unsupported",
        }
    }
}

/// The files the baseline writes and reads.
#[derive(Clone, Debug)]
pub struct BaselinePaths {
    /// The GRUB file.
    pub grub_file: PathBuf,
    /// The getty's drop-in.
    pub getty_drop_in: PathBuf,
    /// The menu `update-grub` generates.
    pub grub_cfg: PathBuf,
    /// The running kernel's command line.
    pub cmdline: PathBuf,
}

impl BaselinePaths {
    /// The places of a Linux host, for `port`.
    #[must_use]
    pub fn linux(port: &SerialConsole) -> BaselinePaths {
        BaselinePaths {
            grub_file: PathBuf::from(GRUB_CONSOLE_FILE),
            getty_drop_in: PathBuf::from(port.getty_drop_in_path()),
            grub_cfg: PathBuf::from("/boot/grub/grub.cfg"),
            cmdline: PathBuf::from("/proc/cmdline"),
        }
    }
}

/// The programs the baseline runs.
#[derive(Clone, Debug)]
pub struct BaselineTools {
    pub systemctl: PathBuf,
    /// Debian's and Ubuntu's generator of GRUB's menu; a host without it is `unsupported`.
    pub update_grub: PathBuf,
}

impl BaselineTools {
    /// The places of a Linux host.
    #[must_use]
    pub fn linux() -> BaselineTools {
        BaselineTools {
            systemctl: PathBuf::from("/usr/bin/systemctl"),
            update_grub: PathBuf::from("/usr/sbin/update-grub"),
        }
    }
}

/// A look: when, and what it came to.
#[derive(Clone, Debug)]
struct Look {
    at_unix: u64,
    state: BaselineState,
    detail: &'static str,
}

/// A step that failed, by its detail.
type Failed = &'static str;

/// The daemon's serial console baseline on one port.
pub struct SerialBaseline {
    port: SerialConsole,
    paths: BaselinePaths,
    tools: BaselineTools,
    runner: Arc<dyn CommandRunner>,
    /// The user the files are written as and must be owned by.
    owner: u32,
    /// Why the host cannot take the baseline at all (`service_manager`), or `None`.
    unsupported: Option<&'static str>,
    clock: Arc<dyn UnixClock>,
    log: Arc<dyn Log>,
    last: Mutex<Option<Look>>,
}

impl SerialBaseline {
    /// The baseline of `port` at `paths`, run with `tools` through `runner`, the files written as
    /// `owner`, looked at on `clock`, saying what it changed in `log`.
    #[must_use]
    pub fn new(
        port: SerialConsole,
        paths: BaselinePaths,
        tools: BaselineTools,
        runner: Arc<dyn CommandRunner>,
        owner: u32,
        clock: Arc<dyn UnixClock>,
        log: Arc<dyn Log>,
    ) -> SerialBaseline {
        SerialBaseline {
            port,
            paths,
            tools,
            runner,
            owner,
            unsupported: None,
            clock,
            log,
            last: Mutex::new(None),
        }
    }

    /// The same baseline on a host that cannot take it, for `why`: every look is `unsupported`
    /// with `why` as its detail, and nothing is run or written.
    #[must_use]
    pub fn unsupported(self, why: &'static str) -> SerialBaseline {
        SerialBaseline {
            unsupported: Some(why),
            ..self
        }
    }

    /// Runs `program` with `args` for at most `deadline`; its standard output when it exits 0.
    fn run(&self, program: &Path, args: &[&str], deadline: Duration) -> Option<String> {
        let mut argv = vec![program.display().to_string()];
        argv.extend(args.iter().map(|arg| (*arg).to_owned()));
        match self.runner.run(&argv, deadline) {
            Ok(output) if output.exit_code == Some(0) => {
                Some(String::from_utf8_lossy(&output.stdout).into_owned())
            }
            _ => None,
        }
    }

    /// `systemctl` with `args`, or `failed` when it does not exit 0.
    fn systemctl(&self, args: &[&str], failed: Failed) -> Result<String, Failed> {
        self.run(&self.tools.systemctl, args, SYSTEMCTL_DEADLINE)
            .ok_or(failed)
    }

    /// Writes `bytes` to `path` unless it holds them already; true when it was written.
    fn put(&self, path: &Path, bytes: &[u8], failed: Failed) -> Result<bool, Failed> {
        let written = replace(path, bytes, self.owner, TEMPORARY).map_err(|_| failed)?;
        if written == Rendered::Written {
            self.log.line(&format!(
                "serial_console {}: wrote {}",
                self.port.as_str(),
                path.display()
            ));
        }
        Ok(written == Rendered::Written)
    }

    /// Whether the text at `path` holds the port's kernel argument as one of its words; false
    /// when it cannot be read.
    fn holds_argument(&self, path: &Path) -> bool {
        let argument = self.port.kernel_argument();
        fs::read_to_string(path)
            .is_ok_and(|text| text.split_whitespace().any(|word| word == argument))
    }

    /// Steps 2 and 3: the GRUB file and the menu made from it.
    fn grub(&self) -> Result<(), Failed> {
        let written = self.put(
            &self.paths.grub_file,
            self.port.grub_defaults().as_bytes(),
            "grub_file",
        )?;
        if !written && self.holds_argument(&self.paths.grub_cfg) {
            return Ok(());
        }
        self.run(&self.tools.update_grub, &[], UPDATE_GRUB_DEADLINE)
            .ok_or("update_grub")?;
        self.log.line(&format!(
            "serial_console {}: ran update-grub",
            self.port.as_str()
        ));
        if self.holds_argument(&self.paths.grub_cfg) {
            Ok(())
        } else {
            Err("grub_cfg")
        }
    }

    /// Step 4: the getty's drop-in and the getty.
    fn getty(&self) -> Result<(), Failed> {
        let written = self.put(
            &self.paths.getty_drop_in,
            GETTY_DROP_IN.as_bytes(),
            "getty_drop_in",
        )?;
        if written {
            self.systemctl(&["daemon-reload"], "daemon_reload")?;
        }
        let unit = self.port.getty_unit();
        let shown = self.systemctl(&["show", "-p", "ActiveState,UnitFileState", &unit], "getty")?;
        let value = |key: &str| {
            shown
                .lines()
                .filter_map(|line| line.split_once('='))
                .find(|(name, _)| *name == key)
                .map_or("", |(_, value)| value)
        };
        let (enabled, active) = (
            value("UnitFileState") == "enabled",
            value("ActiveState") == "active",
        );
        if !enabled {
            self.systemctl(&["enable", &unit], "getty_enable")?;
            self.log.line(&format!(
                "serial_console {}: enabled {unit}",
                self.port.as_str()
            ));
        }
        let action = match (active, written) {
            (true, false) => return Ok(()),
            (true, true) => "restart",
            (false, _) => "start",
        };
        self.systemctl(&[action, &unit], "getty_start")?;
        self.log.line(&format!(
            "serial_console {}: {action}ed {unit}",
            self.port.as_str()
        ));
        Ok(())
    }

    /// One look, as the module says.
    fn look(&self) -> (BaselineState, &'static str) {
        if let Some(why) = self.unsupported {
            return (BaselineState::Unsupported, why);
        }
        if !self.tools.update_grub.is_file() {
            return (BaselineState::Unsupported, "update_grub");
        }
        let made = self.grub().and_then(|()| self.getty()).and_then(|()| {
            fs::read_to_string(&self.paths.cmdline)
                .map(|_| self.holds_argument(&self.paths.cmdline))
                .map_err(|_| "cmdline")
        });
        match made {
            Ok(true) => (BaselineState::Applied, ""),
            Ok(false) => (BaselineState::RebootPending, ""),
            Err(step) => (BaselineState::Failed, step),
        }
    }
}

impl SerialConsoleSource for SerialBaseline {
    fn report(&self) -> Option<SerialConsoleReport> {
        let now = self.clock.unix_secs();
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        let due = last
            .as_ref()
            .is_none_or(|look| now < look.at_unix || now - look.at_unix >= RECHECK_SECS);
        if due {
            let (state, detail) = self.look();
            let changed = last
                .as_ref()
                .is_none_or(|before| (before.state, before.detail) != (state, detail));
            if changed {
                let level = if state == BaselineState::Failed {
                    "error: "
                } else {
                    ""
                };
                let note = if detail.is_empty() {
                    String::new()
                } else {
                    format!(" ({detail})")
                };
                self.log.line(&format!(
                    "{level}serial_console {}: {}{note}",
                    self.port.as_str(),
                    state.as_str()
                ));
            }
            *last = Some(Look {
                at_unix: now,
                state,
                detail,
            });
        }
        last.as_ref().map(|look| SerialConsoleReport {
            port: self.port.as_str().to_owned(),
            state: look.state.as_str().to_owned(),
            detail: look.detail.to_owned(),
        })
    }
}

/// The baseline every heartbeat of a host of `kind` reports, for the `port` its config names:
/// none without one; on Linux under systemd the host's, written as the daemon's user, looked at on
/// `clock`; `unsupported` (`service_manager`) under OpenRC and on a Mac, which have no
/// `serial-getty@` unit.
pub fn at_host(
    kind: PlatformKind,
    port: Option<&SerialConsole>,
    clock: Arc<dyn UnixClock>,
    log: Arc<dyn Log>,
) -> Option<Arc<dyn SerialConsoleSource>> {
    let port = port?;
    let baseline = SerialBaseline::new(
        port.clone(),
        BaselinePaths::linux(port),
        BaselineTools::linux(),
        Arc::new(ProcessRunner::new(MAX_STDOUT)),
        rustix::process::geteuid().as_raw(),
        clock,
        log,
    );
    Some(Arc::new(match kind {
        PlatformKind::Linux => baseline,
        PlatformKind::Openrc | PlatformKind::Macos => baseline.unsupported("service_manager"),
    }))
}

