//! The declaration, reduced to what `launch.rs` reads.
//!
//! B3 rewrites launch.rs and deletes this. The per-service declaration is replaced by the daemon
//! config and the package manifest (`hostkeeper-config`); what is left here is the part of the
//! old declaration that the transient unit of one step is still built from, and the rules on the
//! values that reach a unit property. Every table refuses a key it does not know.

use std::collections::{BTreeMap, BTreeSet};

use hostkeeper_config::names::MAX_UNIT_NAME;
use hostkeeper_hook::Step;
use serde::{Deserialize, Serialize};

use crate::launch::unit_name;
use crate::load::Violation;
use crate::names::{
    Capability, Family, PathError, check_path, check_relative, env_allowed, is_account_name,
    is_env_name, is_env_value, is_service_name,
};

/// The longest a step may be given: one day. B3 rewrites launch.rs and deletes this.
pub const MAX_SECS: u64 = 86_400;
/// The most leading arguments a hook may be given before the step. B3 rewrites launch.rs and
/// deletes this.
pub const MAX_HOOK_ARGS: usize = 8;

fn default_hook_args() -> Vec<String> {
    vec!["update-hook".to_owned()]
}

/// The bound of each step, in seconds. B3 rewrites launch.rs and deletes this.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timeouts {
    pub prepare: u64,
    pub cutover: u64,
    pub verify: u64,
    pub rollback: u64,
    pub cleanup: u64,
}

impl Timeouts {
    /// The bound of `step`. B3 rewrites launch.rs and deletes this.
    pub fn get(&self, step: Step) -> u64 {
        match step {
            Step::Prepare => self.prepare,
            Step::Cutover => self.cutover,
            Step::Verify => self.verify,
            Step::Rollback => self.rollback,
            Step::Cleanup => self.cleanup,
        }
    }
}

/// The capabilities of each step. A step with no entry falls back to the hook-wide
/// `capabilities`; an entry with an empty list is a step that holds none. B3 rewrites launch.rs
/// and deletes this.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepCapabilities {
    pub prepare: Option<BTreeSet<Capability>>,
    pub cutover: Option<BTreeSet<Capability>>,
    pub verify: Option<BTreeSet<Capability>>,
    pub rollback: Option<BTreeSet<Capability>>,
    pub cleanup: Option<BTreeSet<Capability>>,
}

impl StepCapabilities {
    /// The entry of `step`, if any. B3 rewrites launch.rs and deletes this.
    pub fn get(&self, step: Step) -> Option<&BTreeSet<Capability>> {
        match step {
            Step::Prepare => self.prepare.as_ref(),
            Step::Cutover => self.cutover.as_ref(),
            Step::Verify => self.verify.as_ref(),
            Step::Rollback => self.rollback.as_ref(),
            Step::Cleanup => self.cleanup.as_ref(),
        }
    }
}

/// The `[hook]` table: whom the hook runs as and with what rights. B3 rewrites launch.rs and
/// deletes this.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hook {
    pub user: String,
    pub group: String,
    /// The hook-wide grant: the fallback of a step with no entry in `step_capabilities`.
    #[serde(default)]
    pub capabilities: BTreeSet<Capability>,
    #[serde(default)]
    pub step_capabilities: StepCapabilities,
    #[serde(default)]
    pub read_paths: BTreeSet<String>,
    #[serde(default)]
    pub write_paths: BTreeSet<String>,
    #[serde(default)]
    pub address_families: BTreeSet<Family>,
    /// Exactly these variables; nothing is inherited.
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    pub timeout_secs: Timeouts,
}

impl Hook {
    /// The capabilities the hook holds in `step`. B3 rewrites launch.rs and deletes this.
    pub fn capabilities_for(&self, step: Step) -> &BTreeSet<Capability> {
        self.step_capabilities
            .get(step)
            .unwrap_or(&self.capabilities)
    }
}

/// What one step's unit is built from. The top-level keys come first in the file: in TOML a key
/// after a table header belongs to that table. B3 rewrites launch.rs and deletes this.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Declaration {
    pub name: String,
    /// The hook executable, a path within the staged release.
    pub binary: String,
    /// Leading arguments before the step; `["update-hook"]` when absent.
    #[serde(default = "default_hook_args")]
    pub hook_args: Vec<String>,
    /// The hook's record directory.
    pub state_dir: String,
    pub hook: Hook,
}

/// Why a declaration was refused, or a launch from one. B3 rewrites launch.rs and deletes this.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error("not a declaration: {0}")]
    Syntax(String),
    #[error("{field}: {why}")]
    Value { field: String, why: String },
    #[error("{field}: path {path:?} {why}")]
    Path {
        field: &'static str,
        path: String,
        why: PathError,
    },
    #[error("{field}: path {path:?} {why}")]
    Reserved {
        field: &'static str,
        path: String,
        why: Violation,
    },
    #[error(
        "{field}: path {path:?} is below {ancestor:?}, which an account other than root owns or can write, so it can be swapped for a link"
    )]
    Ancestor {
        field: &'static str,
        path: String,
        ancestor: String,
    },
    #[error("conflicts with {with} over {what}")]
    Conflict { what: &'static str, with: String },
}

fn value(field: &str, why: impl Into<String>) -> Refusal {
    Refusal::Value {
        field: field.to_owned(),
        why: why.into(),
    }
}

fn plain_word(field: &str, word: &str) -> Result<(), Refusal> {
    let ok = (1..=64).contains(&word.len())
        & word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._=:/@+-".contains(&b));
    if ok {
        Ok(())
    } else {
        Err(value(
            field,
            format!("{word:?} is not 1 to 64 letters, digits and `._=:/@+-`"),
        ))
    }
}

fn account(field: &str, name: &str) -> Result<(), Refusal> {
    if is_account_name(name) {
        Ok(())
    } else {
        Err(value(field, format!("{name:?} is not an account name")))
    }
}

fn path(field: &'static str, entry: &str) -> Result<(), Refusal> {
    check_path(entry).map_err(|why| Refusal::Path {
        field,
        path: entry.to_owned(),
        why,
    })
}

impl Declaration {
    /// Reads the text of a declaration and applies [`Declaration::check`]. B3 rewrites launch.rs
    /// and deletes this.
    pub fn parse(text: &str) -> Result<Declaration, Refusal> {
        let declaration: Declaration =
            toml::from_str(text).map_err(|e| Refusal::Syntax(e.message().to_owned()))?;
        declaration.check()?;
        Ok(declaration)
    }

    /// The rules on the values that reach a unit property: the grammars of the name, the
    /// binary, the arguments, the accounts, the paths and the environment, and a bound from 1 s
    /// to a day for each step. B3 rewrites launch.rs and deletes this.
    pub fn check(&self) -> Result<(), Refusal> {
        if !is_service_name(&self.name) {
            return Err(value(
                "name",
                format!("{:?} is not a service name", self.name),
            ));
        }
        let longest_unit = Step::ALL
            .iter()
            .map(|step| unit_name(&self.name, *step, u64::MAX).len())
            .max()
            .unwrap_or_default()
            + ".service".len();
        if longest_unit > MAX_UNIT_NAME {
            return Err(value(
                "name",
                format!(
                    "{:?} is too long for the units its steps run in: \
                     `updater-<name>-<step>-<generation>.service` would pass {MAX_UNIT_NAME} bytes",
                    self.name
                ),
            ));
        }
        check_relative(&self.binary).map_err(|why| value("binary", format!("{why}")))?;
        if self.hook_args.len() > MAX_HOOK_ARGS {
            return Err(value(
                "hook_args",
                format!("more than {MAX_HOOK_ARGS} arguments"),
            ));
        }
        for arg in &self.hook_args {
            plain_word("hook_args", arg)?;
        }
        account("hook.user", &self.hook.user)?;
        account("hook.group", &self.hook.group)?;
        for step in Step::ALL {
            let seconds = self.hook.timeout_secs.get(step);
            if !(1..=MAX_SECS).contains(&seconds) {
                return Err(value(
                    &format!("hook.timeout_secs.{step}"),
                    format!("{seconds} is not within 1 to {MAX_SECS}"),
                ));
            }
        }
        path("state_dir", &self.state_dir)?;
        for entry in &self.hook.read_paths {
            path("hook.read_paths", entry)?;
        }
        for entry in &self.hook.write_paths {
            path("hook.write_paths", entry)?;
        }
        for (key, text) in &self.hook.environment {
            if !(is_env_name(key) & is_env_value(text) & env_allowed(key, text)) {
                return Err(value(
                    "hook.environment",
                    format!("{key:?} is not a variable name with a plain value"),
                ));
            }
        }
        Ok(())
    }
}
