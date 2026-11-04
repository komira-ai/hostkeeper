//! The transient systemd unit that runs one step of a hook, and what it is still built from.
//!
//! The per-service declaration, its loader, the widening check, the approvals and the gate are
//! gone: the daemon config and the package manifest replace them (`hostkeeper-config`,
//! docs/design/hostkeeper-design.md sections 4.1 and 4.7). What is left is `launch` and the part
//! of the declaration it reads, which B3 rewrites; the README says what each kept item is for.
//!
//! The crate names no product and depends on no crate of this workspace but the hook contract
//! (`hostkeeper-hook`) and the config crate (`hostkeeper-config`); a layering test of the
//! workspace keeps both true.

pub mod declaration;
pub mod launch;
pub mod load;
pub mod names;

pub use declaration::{Declaration, Hook, Refusal, StepCapabilities, Timeouts};
pub use launch::{
    Finished, Launch, LaunchError, RunError, Spawner, StepRun, Systemd, SystemdRun, command,
    properties, run_step, unit_name,
};
pub use load::Declared;
