//! `hostkeeper-wire`: what an updater agent and the update coordinator say to each other, and
//! the rules both sides apply to what they read.
//!
//! - [`wire`] (with [`wire_report`]): the heartbeat an agent sends and the reply;
//! - [`version`]: the one rule for a version string;
//! - [`names`]: the role, host and platform names;
//! - [`san`] and [`identity`]: who a certificate says its holder is, read from its
//!   subjectAltNames under two configurable URI prefixes;
//! - [`secret`]: a secret file read under the file rule (regular, owned by this user, mode
//!   `0600` or `0400`).

pub mod identity;
pub mod names;
pub mod san;
pub mod secret;
pub mod version;
pub mod wire;
pub mod wire_report;

pub use names::{HostId, HostIdError, Platform, RoleName, RoleNameError};

#[cfg(test)]
mod identity_tests;
#[cfg(test)]
mod san_tests;
#[cfg(test)]
mod secret_tests;
#[cfg(test)]
mod wire_absent_tests;
#[cfg(test)]
mod wire_acks_tests;
#[cfg(test)]
mod wire_agent_tests;
#[cfg(test)]
mod wire_dropped_facts_tests;
#[cfg(test)]
mod wire_features_tests;
#[cfg(test)]
mod wire_halts_tests;
#[cfg(test)]
mod wire_legacy_tests;
#[cfg(test)]
mod wire_service_facts_tests;
#[cfg(test)]
mod wire_size_tests;
#[cfg(test)]
mod wire_tests;
#[cfg(test)]
mod wire_verdict_tests;
