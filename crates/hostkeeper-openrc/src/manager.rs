//! [`OpenrcServiceManager`]: the observer's [`ServiceManager`] on an OpenRC host.
//!
//! A unit is an OpenRC service, the name of an init script. Its state is read in two steps:
//!
//! 1. a service with no init script in the init directory (`/etc/init.d`) is
//!    [`ActiveState::NotFound`], and nothing is run;
//! 2. otherwise `rc-service <service> status` is run, and its exit status is the state, as
//!    OpenRC's `openrc-run` (and its supervise-daemon module) sets it ([`state_of`]):
//!
//! | exit | `rc-service` prints | state |
//! |---|---|---|
//! | 0 | `started` | [`ActiveState::Active`] |
//! | 3 | `stopped` | [`ActiveState::Inactive`] |
//! | 4 | `stopping` | [`ActiveState::Deactivating`] |
//! | 8 | `starting` | [`ActiveState::Activating`] |
//! | 16 | `inactive` | [`ActiveState::Inactive`] |
//! | 32 | `crashed` | [`ActiveState::Failed`] |
//! | 64 | `unsupervised` (its supervise-daemon is gone) | [`ActiveState::Failed`] |
//!
//! Any other exit, an end by a signal, or a command that could not run is an
//! [`ObserveError::Manager`], never a state.
//!
//! The main process of a started service is the child supervise-daemon runs, whose pid OpenRC
//! keeps in `<run dir>/options/<service>/child_pid` (`/run/openrc`); it is replaced at each
//! respawn, so two reads around one observation agree while the process lives. A service that
//! supervise-daemon does not run has no such file and no main pid; a file that is there and does
//! not hold a pid is an error.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hostkeeper_observe::{ActiveState, CommandRunner, ObserveError, ServiceManager, UnitState};

use crate::rc_service::{RC_SERVICE, service_name};

/// Where OpenRC keeps its state of the services.
pub const OPENRC_RUN_DIR: &str = "/run/openrc";
/// Where OpenRC reads its init scripts.
pub const INIT_DIR: &str = "/etc/init.d";
/// The file, in a service's options directory, that holds the pid of the child its
/// supervise-daemon runs.
pub const CHILD_PID: &str = "child_pid";

/// The state and the words `rc-service <service> status` exits with `code` for, `None` for a
/// code OpenRC does not give (the module's table).
pub fn state_of(code: i32) -> Option<(ActiveState, &'static str)> {
    Some(match code {
        0 => (ActiveState::Active, "started"),
        3 => (ActiveState::Inactive, "stopped"),
        4 => (ActiveState::Deactivating, "stopping"),
        8 => (ActiveState::Activating, "starting"),
        16 => (ActiveState::Inactive, "inactive"),
        32 => (ActiveState::Failed, "crashed"),
        64 => (ActiveState::Failed, "unsupervised"),
        _ => return None,
    })
}

/// The pid in the text of a `child_pid` file: digits, a line feed after them allowed, not 0.
pub fn parse_child_pid(text: &str) -> Option<u32> {
    let digits = text.strip_suffix('\n').unwrap_or(text);
    let all_digits = !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit());
    all_digits
        .then(|| digits.parse::<u32>().ok())
        .flatten()
        .filter(|pid| *pid != 0)
}

/// The observer's [`ServiceManager`] over `rc-service`, through a [`CommandRunner`].
#[derive(Debug)]
pub struct OpenrcServiceManager<R> {
    runner: R,
    deadline: Duration,
    init_dir: PathBuf,
    run_dir: PathBuf,
}

impl<R: CommandRunner> OpenrcServiceManager<R> {
    /// Runs `rc-service` through `runner`, each command for at most `deadline`; init scripts are
    /// looked up in `init_dir` ([`INIT_DIR`] on a host), the child pids in `run_dir`
    /// ([`OPENRC_RUN_DIR`]).
    pub fn new(
        runner: R,
        deadline: Duration,
        init_dir: &Path,
        run_dir: &Path,
    ) -> OpenrcServiceManager<R> {
        OpenrcServiceManager {
            runner,
            deadline,
            init_dir: init_dir.to_path_buf(),
            run_dir: run_dir.to_path_buf(),
        }
    }

    /// The pid of the child supervise-daemon runs for `unit`, if it runs one.
    fn child_pid(&self, unit: &str) -> Result<Option<u32>, String> {
        let path = self.run_dir.join("options").join(unit).join(CHILD_PID);
        match fs::read_to_string(&path) {
            Ok(text) => parse_child_pid(&text)
                .map(Some)
                .ok_or_else(|| format!("{} holds no pid: {text:?}", path.display())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    fn state(&self, unit: &str) -> Result<UnitState, String> {
        service_name(unit).map_err(|e| e.to_string())?;
        if fs::symlink_metadata(self.init_dir.join(unit)).is_err() {
            return Ok(UnitState {
                active: ActiveState::NotFound,
                sub: "no init script".to_owned(),
                main_pid: None,
            });
        }
        let argv = [RC_SERVICE, unit, "status"].map(str::to_owned);
        let output = self
            .runner
            .run(&argv, self.deadline)
            .map_err(|e| e.to_string())?;
        let code = output
            .exit_code
            .ok_or_else(|| format!("{} ended by a signal", argv.join(" ")))?;
        let (active, sub) = state_of(code)
            .ok_or_else(|| format!("{} exited {code}, which is no state", argv.join(" ")))?;
        let main_pid = match active {
            ActiveState::Active => self.child_pid(unit)?,
            _ => None,
        };
        Ok(UnitState {
            active,
            sub: sub.to_owned(),
            main_pid,
        })
    }
}

impl<R: CommandRunner> ServiceManager for OpenrcServiceManager<R> {
    fn unit_state(&self, unit: &str) -> Result<UnitState, ObserveError> {
        self.state(unit).map_err(|why| ObserveError::Manager {
            unit: unit.to_owned(),
            why,
        })
    }
}
