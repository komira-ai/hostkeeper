//! The serial console baseline as the host's daemon keeps it (`HeartbeatRequest.serial_console`
//! of `hostkeeper.agent.v1`): the port, the state and its detail. The daemon writes the baseline
//! and says where it stands (`hostkeeper_daemon::serial_baseline`); this crate only carries the
//! report in every gRPC heartbeat it is given for. It is a few tens of bytes, counted toward a
//! heartbeat's bound and never dropped from it.

use serde_json::{Value, json};

/// Where the baseline stands on the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SerialConsoleReport {
    /// The port the daemon config names (`ttyS1`).
    pub port: String,
    /// `applied`, `reboot_pending`, `failed` or `unsupported`.
    pub state: String,
    /// The step that failed, or why the host cannot take the baseline; empty otherwise.
    pub detail: String,
}

impl SerialConsoleReport {
    /// The report as JSON with the wire's names: what a heartbeat's bound counts of it.
    pub fn to_json(&self) -> Value {
        json!({ "port": self.port, "state": self.state, "detail": self.detail })
    }
}
