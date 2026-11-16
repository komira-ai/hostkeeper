//! The Remote Login probe a Mac's daemon runs once at its start, as every gRPC heartbeat after it
//! reports it (`HeartbeatRequest.remote_login_probe` of `hostkeeper.agent.v1`): the steps it ran,
//! in order, each with its exit code and its output. The daemon makes it
//! (`hostkeeper_daemon::remote_login_probe`); this crate only carries it, and drops it first when
//! a heartbeat would not fit its bound ([`crate::beat::fit`]).

use serde_json::{Value, json};

/// One run of the probe.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RemoteLoginProbe {
    /// The host's unix clock when the probe started.
    pub started_unix: u64,
    /// Each step, in the order it ran.
    pub steps: Vec<ProbeStep>,
}

/// One step of the probe: a command it ran, or a file or socket it read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProbeStep {
    /// What the step is (`remote_login_set_on`).
    pub name: String,
    /// The program and its arguments as run, or what the step read.
    pub argv: Vec<String>,
    /// The exit status; none when nothing ran, or it could not be started, timed out or was ended
    /// by a signal.
    pub exit_code: Option<i32>,
    /// What it wrote to its standard output, or what the step read.
    pub stdout: String,
    /// What it wrote to its standard error.
    pub stderr: String,
    /// Why the step could not be done; empty when it was done.
    pub error: String,
    /// Whether `stdout` or `stderr` was cut.
    pub truncated: bool,
}

impl RemoteLoginProbe {
    /// The probe as JSON with the wire's names: what a heartbeat's bound counts of it.
    pub fn to_json(&self) -> Value {
        let steps: Vec<Value> = self
            .steps
            .iter()
            .map(|step| {
                json!({
                    "name": step.name,
                    "argv": step.argv,
                    "exit_code": step.exit_code,
                    "stdout": step.stdout,
                    "stderr": step.stderr,
                    "error": step.error,
                    "truncated": step.truncated,
                })
            })
            .collect();
        json!({ "started_unix": self.started_unix, "steps": steps })
    }
}
