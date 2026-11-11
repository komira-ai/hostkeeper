//! The port through which the Mac path asks what only the MDM server's ABM side knows (design
//! 4.6): whether a serial is in our Apple Business Manager table, and the two settings that name
//! a Mac the table brings: the start of its host id and the group it joins when its inventory
//! record names none.
//!
//! [`crate::abm_approval`] answers this port from the sync's table and the setup's settings; a
//! coordinator without an ABM client has none, and then only the allowlist approves a Mac.

/// The settings that name a Mac of our ABM which has no inventory record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AbmNaming {
    /// What its host id starts with, before the serial in lower case (`host_id_prefix`).
    pub host_id_prefix: String,
    /// The group it joins (`default_group`); `None` when the setting is empty.
    pub default_group: Option<String>,
}

/// The ABM side could not answer: its store could not be read.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("no ABM answer: {0}")]
pub struct AbmUnavailable(pub String);

/// What the Mac path asks of the ABM side.
pub trait AbmDevices: Send + Sync {
    /// Whether `serial` is in our ABM now: in the table and not marked removed.
    ///
    /// # Errors
    /// [`AbmUnavailable`] when the table cannot be read.
    fn serial_in_abm(&self, serial: &str) -> Result<bool, AbmUnavailable>;

    /// The settings that name a Mac with no inventory record.
    ///
    /// # Errors
    /// [`AbmUnavailable`] when the settings cannot be read.
    fn naming(&self) -> Result<AbmNaming, AbmUnavailable>;
}
