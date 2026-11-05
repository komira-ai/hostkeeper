//! What one read of the convergence says, from the canonical JSON `releases converge` prints.
//!
//! proto3 JSON leaves out a field that has its default: an absent count is 0, an absent state
//! is unspecified, an absent desired version is none.

use serde_json::Value;

/// The hosts of one role.
#[derive(Debug, PartialEq, Eq)]
struct Role {
    name: String,
    at_desired: u64,
    updating: u64,
    failed: u64,
    down: u64,
}

impl Role {
    fn of(json: &Value) -> Role {
        let count = |key: &str| json[key].as_u64().unwrap_or(0);
        Role {
            name: json["role"].as_str().unwrap_or("").to_owned(),
            at_desired: count("atDesired"),
            updating: count("updating"),
            failed: count("failed"),
            down: count("down"),
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
    /// The convergence in the reply of `releases converge`.
    pub(crate) fn of(reply: &Value) -> Convergence {
        let json = &reply["convergence"];
        Convergence {
            desired: json["desired"].as_str().map(str::to_owned),
            state: json["state"]
                .as_str()
                .unwrap_or("CONVERGENCE_STATE_UNSPECIFIED")
                .trim_start_matches("CONVERGENCE_STATE_")
                .to_ascii_lowercase(),
            roles: json["roles"]
                .as_array()
                .map(|roles| roles.iter().map(Role::of).collect())
                .unwrap_or_default(),
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
