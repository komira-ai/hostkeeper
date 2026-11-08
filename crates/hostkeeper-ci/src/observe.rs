//! What one read of the convergence says, from the reply of `Converge`.
//!
//! An absent convergence is an empty one: no desired version, state unspecified, no roles.

use hostkeeper_proto::hostkeeper::operator::v1::{ConvergeResponse, ConvergenceState, RoleCounts};

/// The hosts of one role.
#[derive(Debug, PartialEq, Eq)]
struct Role {
    name: String,
    at_desired: u32,
    updating: u32,
    failed: u32,
    down: u32,
}

impl Role {
    fn of(counts: &RoleCounts) -> Role {
        Role {
            name: counts.role.clone(),
            at_desired: counts.at_desired,
            updating: counts.updating,
            failed: counts.failed,
            down: counts.down,
        }
    }

    fn line(&self) -> String {
        format!(
            "{}: {} at the version, {} updating, {} failed, {} down",
            self.name, self.at_desired, self.updating, self.failed, self.down
        )
    }
}

/// How far a service's hosts are from its desired version.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Convergence {
    /// The desired version; none when none was ever set.
    pub desired: Option<String>,
    /// The state's name in lower case, without its prefix: `converged`, `in_progress`...
    pub state: String,
    roles: Vec<Role>,
}

impl Convergence {
    /// The convergence in the reply of `Converge`.
    pub(crate) fn of(reply: &ConvergeResponse) -> Convergence {
        let convergence = reply.convergence.clone().unwrap_or_default();
        Convergence {
            desired: convergence.desired,
            state: state_name(convergence.state),
            roles: convergence.roles.iter().map(Role::of).collect(),
        }
    }

    /// One line: `in_progress, desired 1.2.3, web: 2 at the version, 1 updating, 0 failed, 0 down`.
    pub(crate) fn line(&self) -> String {
        let mut line = format!(
            "{}, desired {}",
            self.state,
            self.desired.as_deref().unwrap_or("none")
        );
        for (i, role) in self.roles.iter().enumerate() {
            line.push_str(if i == 0 { ", " } else { "; " });
            line.push_str(&role.line());
        }
        line
    }
}

/// The name of the state with number `number` in lower case, without its prefix; `unknown_<n>`
/// for a number no state has (a coordinator newer than this client).
fn state_name(number: i32) -> String {
    ConvergenceState::try_from(number).map_or_else(
        |_| format!("unknown_{number}"),
        |state| {
            state
                .as_str_name()
                .trim_start_matches("CONVERGENCE_STATE_")
                .to_ascii_lowercase()
        },
    )
}
