//! Maintenance updates (docs/design/maintenance-updates.md): so far only the name of their
//! feature, [`FEATURE`]. This coordinator does not advertise it ([`crate::features::FEATURES`]
//! leaves it out), reads no `HeartbeatRequest.maintenance` and sends no `maintain` order; a name is
//! advertised only by the change that implements what it names.

/// The feature a daemon announces to report its host's maintenance report
/// (`HeartbeatRequest.maintenance`) and to take the `maintain` order; not advertised yet.
pub const FEATURE: &str = "maintenance-v1";
