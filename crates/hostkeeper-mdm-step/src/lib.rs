//! The host-step layer of the MDM host kit: what a program that configures a Linux host does to
//! it, as data (docs/design/hostkeeper-mdm.md, task M3).
//!
//! A [`Planner`] looks at the host (read only) and records only the [`Step`]s that would change
//! it: a directory, a file, a user, a unit action or a command. [`execute`] then prints the
//! plan (`Mode::DryRun`, the default of every verb built on this crate) or carries it out
//! (`Mode::Apply`). A second run finds nothing to change and prints `nothing to do`.
//!
//! The host is the [`Host`] trait: [`RealHost`] is the machine, [`fake::FakeHost`] keeps a
//! file tree in memory and runs no program it was not told about, which is what the tests of
//! every verb use.
//!
//! # Rules the layer keeps
//!
//! * A plan line names a path, a size and a mode, never a file's contents: a step that
//!   writes a secret describes only its size. No step puts a value on a command line; a
//!   program that needs a secret reads it from a file.
//! * Every file is written to a temporary name of its own writer in its directory
//!   (`NAME.PID-N.hostkeeper-mdm.tmp`), with its mode and owner set, then renamed over the
//!   target, so a reader sees the old file or the new one, the mode is exact whatever the umask,
//!   and two writers of one file never touch each other's temporary file.
//! * Every directory on the way to a path is owned by a user trusted for that path: root and
//!   the effective user ([`RealHost::new`]), the owner a step asks for, and the owner granted
//!   for the paths below a directory ([`RealHost::trust_below`]: the operator's for the
//!   directory of fetched binaries; [`RealHost::trust_below_user`], by name: the service's
//!   for its state, also when the plan makes the service user).
//! * A plan is checked whole before its first step, the same way for a dry run and an apply:
//!   a step that the walk would refuse, given the owners the earlier steps leave on its way,
//!   refuses the plan ([`ExecError::Refused`]), so a dry run refuses what an apply would, and
//!   an apply never stops part way on an owner.
//! * An existing account a service is to run as is refused when it resolves to uid 0
//!   ([`Planner::user`]).
//! * Nothing here reads an environment variable.

pub mod fake;
mod host;
mod plan;
mod real;
mod trust;

pub use host::{Cmd, Host, Kind, Meta, Output, Trustee, Way};
pub use plan::{
    Account, ExecError, Mode, Plan, PlanError, Planner, Step, StepError, UnitVerb, execute, ids,
};
pub use real::{RealHost, TEMP_SUFFIX};

/// Printed after a dry run.
pub const DRY_RUN_NOTE: &str = "dry run: nothing changed; re-run with --apply to carry it out";
/// Printed when the host already is as asked.
pub const NOTHING_TO_DO: &str = "nothing to do";
/// The unit manager's client.
pub const SYSTEMCTL: &str = "/usr/bin/systemctl";
/// The account database lookup.
pub const GETENT: &str = "/usr/bin/getent";
/// Runs a program as another user.
pub const RUNUSER: &str = "/usr/sbin/runuser";
/// Creates a user.
pub const USERADD: &str = "/usr/sbin/useradd";
