//! The stage of the `linux` kind on a Linux host, under systemd or OpenRC: its `prepare`
//! (docs/design/maintenance-updates.md, section 5.2 step 1): the free space checked, then a
//! download alone with `apt-get`, `dnf` or `apk`, nothing installed ([`stage::Stage`]). The kind
//! itself, whose `prepare` this is, is [`crate::maintenance_linux::kinds::LinuxKind`].

mod apk;
mod apt;
mod dnf;
pub(crate) mod stage;
#[cfg(test)]
mod testing;
#[cfg(test)]
mod tests;
