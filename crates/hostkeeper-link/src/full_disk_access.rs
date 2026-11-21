//! Whether a Mac's Full Disk Access grant is in force for its daemon, as every gRPC heartbeat
//! reports it (`HeartbeatRequest.full_disk_access` of `hostkeeper.agent.v1`). The daemon finds
//! it (`hostkeeper_daemon::full_disk_access`); this crate only carries it. The JSON route is
//! frozen and sends nothing of it, but the heartbeat's bound counts it on both routes.

use std::sync::Arc;

/// The state of the grant, as the daemon found it. A daemon that does not report it says none
/// (`None` where a heartbeat holds one), which is not any of these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FullDiskAccess {
    /// The directory only the grant allows could be listed.
    Granted,
    /// The system refused the listing for want of permission.
    NotGranted,
    /// The host is not a Mac, so there is no grant.
    NotApplicable,
    /// The listing failed in another way.
    Undetermined,
}

impl FullDiskAccess {
    /// The state's word, as the JSON of a heartbeat and the daemon's log say it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::NotGranted => "not_granted",
            Self::NotApplicable => "not_applicable",
            Self::Undetermined => "undetermined",
        }
    }
}

/// The state of the grant, read at every heartbeat.
pub trait FullDiskAccessSource: Send + Sync {
    fn full_disk_access(&self) -> FullDiskAccess;
}

/// What `source` says now; none from a host that has no source.
pub(crate) fn read(source: Option<&Arc<dyn FullDiskAccessSource>>) -> Option<FullDiskAccess> {
    source.map(|source| source.full_disk_access())
}
