//! The two files hostkeeper reads to decide what a host runs, each with a strict loader:
//!
//! - [`daemon`]: `/etc/hostkeeper/hostkeeper.toml`, the host's only configuration and only trust
//!   root (docs/design/hostkeeper-design.md, section 4.1);
//! - [`manifest`]: the `manifest.toml` inside each package, the service's lifecycle (section 4.7).
//!
//! Both refuse every key they do not know, in every table, naming the key and its table.
//! [`trust`] is the owner and mode check of a file the host trusts, and [`names`] the grammars of
//! service names, versions, paths and unit names.
//!
//! The crate depends on no crate of this workspace; a layering test of the workspace keeps it so.

pub mod daemon;
pub mod manifest;
pub mod names;
pub mod trust;

mod table;
mod url;
