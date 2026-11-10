//! How the desired-state file writes and reads the role limits of a state: the order of the keys
//! and the keys the format fills in ([`FORMAT_ROLES`], [`FORMAT_MIN_UP`]).

use serde::Deserialize;

use super::{FORMAT_MIN_UP, FORMAT_ROLES, RoleLimits};

/// The limits in the order the format writes them.
pub(super) fn write_max_updating<S: serde::Serializer>(
    limits: &RoleLimits,
    s: S,
) -> Result<S::Ok, S::Error> {
    s.collect_map(limits.ordered(&FORMAT_ROLES))
}

/// The minimums in the order the format writes them. The format requires `storage` and
/// `frontend`, so a state that names neither writes them as 0, which is what none means.
pub(super) fn write_min_up<S: serde::Serializer>(
    limits: &RoleLimits,
    s: S,
) -> Result<S::Ok, S::Error> {
    s.collect_map(limits.min_up_written().ordered(&FORMAT_MIN_UP))
}

impl RoleLimits {
    /// The minimums as the format writes them: `storage` and `frontend` named, 0 when not set.
    pub(crate) fn min_up_written(&self) -> RoleLimits {
        let mut written = self.clone();
        for role in FORMAT_MIN_UP {
            written.0.entry(role.to_owned()).or_insert(0);
        }
        written
    }
}

/// The limits as written, with the format's four roles filled in with their defaults.
pub(super) fn read_max_updating<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<RoleLimits, D::Error> {
    let mut limits = RoleLimits::deserialize(d)?;
    for (role, n) in RoleLimits::max_updating_default().0 {
        limits.0.entry(role).or_insert(n);
    }
    Ok(limits)
}
