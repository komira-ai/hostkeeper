//! What is left of the old per-service declaration.
//!
//! The per-service declaration, its loader, the widening check, the approvals and the gate are
//! gone: the daemon config and the package manifest replace them (`hostkeeper-config`,
//! docs/design/hostkeeper-design.md sections 4.1 and 4.7). The transient unit of a step moved to
//! `hostkeeper-steps` (`systemd.rs`), which runs a hook as root with no confinement (section 5.3).
//! What is left is the parser of the declaration, the reserved-path rules and the name grammars,
//! which nothing launches from any more and B26 deletes; the README says what each kept item is.
//!
//! The crate names no product and depends on no crate of this workspace but the hook contract
//! (`hostkeeper-hook`) and the config crate (`hostkeeper-config`); a layering test of the
//! workspace keeps both true.

pub mod declaration;
pub mod load;
pub mod names;

pub use declaration::{Declaration, Hook, Refusal, StepCapabilities, Timeouts};
pub use load::Declared;
