//! A heartbeat as the fleet's rows keep it (`docs/design/coordinator-multi-instance.md`, sections
//! 5.1 and 5.2): what a backend that other coordinators share writes for each heartbeat this one
//! answers ([`super::Backend::keep_beat`]), so whichever instance hears a host's next beat finds
//! the host's row, its bakes and its outstanding orders where the last one left them.
//!
//! Nothing here decides. The rows are what the heartbeat's deciding transaction reads under the
//! gate ([`super::decision`]).

use hostkeeper_wire::identity::Identity;
use hostkeeper_wire::wire::{Health, Heartbeat};

use crate::bake;

/// What one heartbeat says of one service of the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ServiceSaid {
    /// The service, by name.
    pub(crate) service: String,
    /// The version the host reports of it.
    pub(crate) version: String,
    /// Whether the beat counts toward the bake of that version ([`bake::counts`]).
    pub(crate) counts: bool,
    /// Whether the host runs that version, rather than holding an absent copy of it: an update
    /// or install ordered to that version is then done.
    pub(crate) runs: bool,
}

/// One heartbeat of one host, as the fleet's rows keep it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HostBeat {
    /// The host its certificate names.
    pub(crate) host_id: String,
    /// The role its certificate names.
    pub(crate) role: String,
    /// The heartbeat, as the coordinator read it (the JSON of `hostkeeper_wire::wire`, which the
    /// gRPC route maps its message to).
    pub(crate) beat: Vec<u8>,
    /// The host's own clock when it made the beat, in unix seconds; 0 for an agent that sends
    /// none, whose beats are then kept in the order they arrive.
    pub(crate) sent_unix: i64,
    /// Each service the beat names, once: the legacy service's top level first, then the beat's
    /// list in its order. A service named twice is kept as it was named first.
    pub(crate) services: Vec<ServiceSaid>,
}

impl HostBeat {
    /// `beat` of the host `who`, whose top level is the service `legacy` when one is named.
    pub(crate) fn of(who: &Identity, beat: &Heartbeat, legacy: Option<&str>) -> HostBeat {
        let top = legacy.map(|name| said(name, beat));
        let listed = beat
            .services
            .iter()
            .flatten()
            .map(|one| said(&one.name, &one.as_heartbeat(beat.platform)));
        let mut services: Vec<ServiceSaid> = Vec::new();
        for one in top.into_iter().chain(listed) {
            if !services.iter().any(|kept| kept.service == one.service) {
                services.push(one);
            }
        }
        HostBeat {
            host_id: who.host_id.as_str().to_owned(),
            role: who.role.name().to_owned(),
            // A heartbeat the coordinator read is one it can write back: its fields are plain
            // data, and a map's keys are strings.
            beat: serde_json::to_vec(beat).unwrap_or_default(),
            sent_unix: beat
                .sent_unix
                .map_or(0, |sent| i64::try_from(sent).unwrap_or(i64::MAX)),
            services,
        }
    }
}

/// What `beat`, the host's word of `service`, says of it.
fn said(service: &str, beat: &Heartbeat) -> ServiceSaid {
    ServiceSaid {
        service: service.to_owned(),
        version: beat.version.to_string(),
        counts: bake::counts(beat),
        runs: beat.health != Health::Absent,
    }
}

#[cfg(test)]
#[path = "beat_tests.rs"]
mod tests;
