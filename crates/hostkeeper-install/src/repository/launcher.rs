//! The [`Tools`] port over the step launcher (packages.md section 5.1): each run is a transient
//! unit of the service, named `hostkeeper-install-<service>-<tool>-<n>`, bounded, offline but
//! for a fetch, and confined to the paths its run may write.

use std::ffi::OsString;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use hostkeeper_steps::{StepLauncher, ToolCall, ToolNetwork};

use super::tools::{Network, Run, ToolError, ToolOutput, Tools};

/// The tools of one service, run by `launcher`.
pub struct LauncherTools<'a> {
    launcher: &'a dyn StepLauncher,
    service: String,
    runs: Runs<'a>,
}

/// The counter of the runs that numbers their units.
enum Runs<'a> {
    /// This port's own, from 0.
    Own(AtomicU64),
    /// One the caller shares between ports, so that no two of them name the same unit.
    Shared(&'a AtomicU64),
}

impl<'a> LauncherTools<'a> {
    /// The tools of `service`, run by `launcher`, their units numbered from 0.
    pub fn new(launcher: &'a dyn StepLauncher, service: &str) -> LauncherTools<'a> {
        LauncherTools {
            launcher,
            service: service.to_owned(),
            runs: Runs::Own(AtomicU64::new(0)),
        }
    }

    /// The tools of `service`, run by `launcher`, their units numbered by `runs`: a daemon makes
    /// a port for each step and each heartbeat's read, and two of them may run a tool at once,
    /// so they share one counter and a unit is never the name of another run's.
    pub fn sharing(
        launcher: &'a dyn StepLauncher,
        service: &str,
        runs: &'a AtomicU64,
    ) -> LauncherTools<'a> {
        LauncherTools {
            launcher,
            service: service.to_owned(),
            runs: Runs::Shared(runs),
        }
    }

    /// The unit the next run of `tool` has.
    fn unit(&self, tool: &str) -> String {
        let runs = match &self.runs {
            Runs::Own(runs) => runs,
            Runs::Shared(runs) => *runs,
        };
        let n = runs.fetch_add(1, Ordering::Relaxed);
        format!("hostkeeper-install-{}-{tool}-{n}", self.service)
    }
}

impl Tools for LauncherTools<'_> {
    fn run(&self, run: &Run) -> Result<ToolOutput, ToolError> {
        let args: Vec<OsString> = run.args.iter().map(OsString::from).collect();
        let writable: Option<Vec<&Path>> = run
            .writable
            .as_ref()
            .map(|paths| paths.iter().map(|p| p.as_path()).collect());
        let env: Vec<(&str, &str)> = run
            .env
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let unit = self.unit(run.tool.name());
        let call = ToolCall {
            service: &self.service,
            unit: &unit,
            program: Path::new(run.tool.path()),
            args: &args,
            timeout_secs: run.bound_secs,
            network: match run.network {
                Network::Offline => ToolNetwork::Offline,
                Network::Fetch => ToolNetwork::Fetch,
            },
            writable: writable.as_deref(),
            working_dir: run.working_dir.as_deref(),
            env: &env,
        };
        let exit = self
            .launcher
            .run_tool(&call)
            .map_err(|e| ToolError(e.to_string()))?;
        Ok(ToolOutput {
            code: exit.code,
            stdout: exit.stdout,
            stderr: exit.stderr,
            cut: exit.cut,
        })
    }
}
