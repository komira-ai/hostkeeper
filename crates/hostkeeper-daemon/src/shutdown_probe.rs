//! Whether the system is going down, asked when the daemon is sent SIGTERM
//! (docs/design/host-health-alerts.md section 4.4): only then does it write its clean-shutdown
//! marker, so a SIGTERM of a daemon restart, or of a service manager stopping only the daemon,
//! leaves the marker `running`, and a later crash never reads as a clean reboot.
//!
//! - **systemd** ([`Systemd`]): `systemctl is-system-running` printing `stopping`, or a start
//!   job of the reboot, poweroff, halt or kexec target in `systemctl list-jobs --no-legend`.
//!   Both are one static argument list each ([`IS_SYSTEM_RUNNING`], [`LIST_JOBS`]), run through
//!   the [`Launchctl`] port with an empty environment, a deadline ([`PROBE_DEADLINE`]) and capped
//!   output. `is-system-running` exits non-zero for every state but `running`, so its exit status
//!   is not read; only what it printed.
//! - **OpenRC** ([`Openrc`]): the runlevel OpenRC is changing to, the file `softlevel` of its run
//!   directory, reading `shutdown` or `reboot`.
//! - **macOS** ([`Advisory`]): launchd sends SIGTERM at a shutdown and at a `bootout` alike, and
//!   no probe tells them apart, so every SIGTERM counts and the marker is advisory there.
//!
//! A probe that cannot run, or cannot be read, says so ([`GoingDown::Unknown`]), and the marker
//! stays `running`: an unclean shutdown wrongly reported is a warning to look at, a crash read as
//! clean is one nobody looks at.

use std::path::{Path, PathBuf};
use std::time::Duration;

use hostkeeper_launchd::PlatformKind;
use hostkeeper_launchd::{Launchctl, LaunchctlProcess};

use crate::maintenance_linux::rebooter::SYSTEMCTL;

/// How long each probe program may run.
pub const PROBE_DEADLINE: Duration = Duration::from_secs(5);

/// The arguments that print the system's state.
pub const IS_SYSTEM_RUNNING: [&str; 1] = ["is-system-running"];

/// The arguments that list the queued jobs, one a line: id, unit, type, state.
pub const LIST_JOBS: [&str; 2] = ["list-jobs", "--no-legend"];

/// The targets whose start job means the system is going down, without their `.target` suffix.
pub const SHUTDOWN_TARGETS: [&str; 4] = ["reboot", "poweroff", "halt", "kexec"];

/// The suffix of a target unit's name.
const TARGET_SUFFIX: &str = ".target";

/// OpenRC's file of the runlevel it is in or changing to, in its run directory.
pub const OPENRC_SOFTLEVEL: &str = "softlevel";

/// The runlevels OpenRC goes down through.
pub const OPENRC_DOWN: [&str; 2] = ["shutdown", "reboot"];

/// What a probe found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GoingDown {
    /// The system is going down; why the probe says so.
    Yes(String),
    /// It is not; what the probe read instead.
    No(String),
    /// The probe could not tell; why.
    Unknown(String),
}

/// Asks whether the system is going down.
pub trait ShutdownProbe: Send + Sync {
    fn going_down(&self) -> GoingDown;
}

/// The probe of a systemd host.
pub struct Systemd {
    systemctl: Box<dyn Launchctl>,
}

impl Systemd {
    /// The probe over `systemctl`, the program [`IS_SYSTEM_RUNNING`] and [`LIST_JOBS`] are given to.
    #[must_use]
    pub fn new(systemctl: Box<dyn Launchctl>) -> Systemd {
        Systemd { systemctl }
    }

    fn printed(&self, args: &[&str]) -> Result<String, String> {
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        self.systemctl
            .run(&args)
            .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
            .map_err(|e| format!("systemctl {} did not run: {e}", args.join(" ")))
    }
}

impl ShutdownProbe for Systemd {
    fn going_down(&self) -> GoingDown {
        let state = match self.printed(&IS_SYSTEM_RUNNING) {
            Ok(text) => text.trim().to_owned(),
            Err(why) => return GoingDown::Unknown(why),
        };
        if state == "stopping" {
            return GoingDown::Yes("the system is stopping".to_owned());
        }
        match self.printed(&LIST_JOBS) {
            Ok(jobs) => match shutdown_job(&jobs) {
                Some(target) => {
                    GoingDown::Yes(format!("a start job of the {target} target is queued"))
                }
                None => GoingDown::No(format!(
                    "the system is {state}, with no shutdown job queued"
                )),
            },
            Err(why) => GoingDown::Unknown(why),
        }
    }
}

/// The shutdown target (by its name less [`TARGET_SUFFIX`]) whose start job `jobs` lists, if any:
/// a line whose second word is one of [`SHUTDOWN_TARGETS`] with the suffix, and whose third is
/// `start`.
#[must_use]
pub fn shutdown_job(jobs: &str) -> Option<&'static str> {
    jobs.lines().find_map(|line| {
        let mut words = line.split_whitespace().skip(1);
        let unit = words.next()?.strip_suffix(TARGET_SUFFIX)?;
        let target = SHUTDOWN_TARGETS.iter().find(|t| **t == unit)?;
        (words.next() == Some("start")).then_some(*target)
    })
}

/// The probe of an OpenRC host.
pub struct Openrc {
    softlevel: PathBuf,
}

impl Openrc {
    /// The probe over OpenRC's run directory `run_dir`.
    #[must_use]
    pub fn new(run_dir: &Path) -> Openrc {
        Openrc {
            softlevel: run_dir.join(OPENRC_SOFTLEVEL),
        }
    }
}

impl ShutdownProbe for Openrc {
    fn going_down(&self) -> GoingDown {
        match std::fs::read_to_string(&self.softlevel) {
            Ok(text) => {
                let level = text.trim();
                if OPENRC_DOWN.contains(&level) {
                    GoingDown::Yes(format!("OpenRC is changing to the runlevel {level}"))
                } else {
                    GoingDown::No(format!("OpenRC's runlevel is {level}"))
                }
            }
            Err(e) => {
                GoingDown::Unknown(format!("{} cannot be read: {e}", self.softlevel.display()))
            }
        }
    }
}

/// The probe of a Mac: every SIGTERM counts.
pub struct Advisory;

impl ShutdownProbe for Advisory {
    fn going_down(&self) -> GoingDown {
        GoingDown::Yes("launchd sent SIGTERM (advisory on a Mac)".to_owned())
    }
}

/// The probe of a host of `kind`, OpenRC's run directory being `openrc_run_dir`.
#[must_use]
pub fn for_platform(kind: PlatformKind, openrc_run_dir: &Path) -> Box<dyn ShutdownProbe> {
    match kind {
        PlatformKind::Linux => Box::new(Systemd::new(Box::new(LaunchctlProcess::new(
            Path::new(SYSTEMCTL),
            PROBE_DEADLINE,
        )))),
        PlatformKind::Openrc => Box::new(Openrc::new(openrc_run_dir)),
        PlatformKind::Macos => Box::new(Advisory),
    }
}
