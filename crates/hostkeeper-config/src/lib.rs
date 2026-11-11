//! What hostkeeper reads to decide what a host runs:
//!
//! - [`daemon`]: `/etc/hostkeeper/hostkeeper.toml`, the host's only configuration and only trust
//!   root (docs/design/hostkeeper-design.md, section 4.1), read by a strict loader that refuses
//!   every key it does not know, in every table, naming the key and its table;
//! - [`manifest`]: the manifest of an installed version, the service's lifecycle (section 4.7),
//!   its refusals and the rules a release manifest shares with it.
//!
//! [`platform`] is the closed set of platforms, [`trust`] the owner and mode check of a file the
//! host trusts, and [`names`] the grammars of service names, versions, paths and unit names.
//!
//! The crate depends on no crate of this workspace; a layering test of the workspace keeps it so.

pub mod daemon;
pub mod manifest;
pub mod names;
pub mod platform;
pub mod trust;

mod table;
mod url;
