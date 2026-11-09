//! The parts of the coordinator's answers the runner reads, from the gateway's JSON
//! (docs/gateway.md): proto3's canonical JSON, with lowerCamelCase names, 64-bit integers as
//! strings and a field at its default value possibly left out, so an absent field reads as its
//! default here.

use serde_json::Value;

/// A host as `hkctl hosts list` and `hkctl hosts get` give it (`Host`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostView {
    pub node: String,
    pub live: bool,
    pub last_heartbeat: Option<u64>,
    pub first_heartbeat: Option<u64>,
    pub first_serving: Option<u64>,
    pub services: Vec<ServiceView>,
}

/// A service of a host: the coordinator's view (`HostService`) and, for `updating`, the host's
/// own last report of it (a `ServiceBeat` of the `services` of its `heartbeat`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServiceView {
    pub service: String,
    pub installed: Option<String>,
    pub holds_slot: bool,
    pub health: String,
    pub last_target: Option<String>,
    pub last_outcome: Option<String>,
    pub updating: Option<String>,
}

/// An audit entry (`AuditEntry`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Entry {
    pub seq: u64,
    pub action: String,
    pub node: Option<String>,
    pub at_unix: Option<u64>,
    pub by_name: Option<String>,
}

/// A Mac of the MDM gate (`MdmDevice`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Device {
    pub id: String,
    pub enrolled_since: Option<u64>,
}

/// A uint64 of the gateway: a string of digits, or a number.
pub fn number(value: Option<&Value>) -> Option<u64> {
    match value? {
        Value::String(s) => s.parse().ok(),
        other => other.as_u64(),
    }
}

fn text(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(str::to_owned)
}

fn flag(value: Option<&Value>) -> bool {
    value.and_then(Value::as_bool).unwrap_or(false)
}

fn items<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

impl HostView {
    pub fn read(host: &Value) -> Self {
        let beats = host
            .get("heartbeat")
            .map_or(&[][..], |h| items(h, "services"));
        let services = items(host, "services")
            .iter()
            .map(|s| {
                let service = text(s.get("service")).unwrap_or_default();
                let last = s.get("lastUpdate");
                let updating = beats
                    .iter()
                    .find(|b| b.get("name").and_then(Value::as_str) == Some(service.as_str()))
                    .and_then(|b| text(b.get("updating")));
                ServiceView {
                    installed: text(s.get("installedVersion")),
                    holds_slot: flag(s.get("holdsSlot")),
                    health: text(s.get("health")).unwrap_or_default(),
                    last_target: last.and_then(|l| text(l.get("target"))),
                    last_outcome: last.and_then(|l| text(l.get("outcome"))),
                    updating,
                    service,
                }
            })
            .collect();
        Self {
            node: text(host.get("nodeId")).unwrap_or_default(),
            live: flag(host.get("live")),
            last_heartbeat: number(host.get("lastHeartbeatUnix")),
            first_heartbeat: number(host.get("firstHeartbeatUnix")),
            first_serving: number(host.get("firstServingUnix")),
            services,
        }
    }

    /// The service of this name, if the host has it.
    pub fn service(&self, name: &str) -> Option<&ServiceView> {
        self.services.iter().find(|s| s.service == name)
    }
}

/// The hosts of one page of `hosts list`, and the token of the next page (empty at the end).
pub fn hosts_page(answer: &Value) -> (Vec<HostView>, String) {
    (
        items(answer, "hosts").iter().map(HostView::read).collect(),
        text(answer.get("nextPageToken")).unwrap_or_default(),
    )
}

/// The entries of an `audit list` answer, oldest first, and its `nextSeq`.
pub fn audit_page(answer: &Value) -> (Vec<Entry>, u64) {
    let entries = items(answer, "entries")
        .iter()
        .map(|e| Entry {
            seq: number(e.get("seq")).unwrap_or(0),
            action: text(e.get("action")).unwrap_or_default(),
            node: text(e.get("nodeId")),
            at_unix: number(e.get("atUnix")),
            by_name: text(e.get("byName")),
        })
        .collect();
    (entries, number(answer.get("nextSeq")).unwrap_or(0))
}

/// The Macs of one page of `mdm list-devices`, and the token of the next page.
pub fn devices_page(answer: &Value) -> (Vec<Device>, String) {
    (
        items(answer, "devices")
            .iter()
            .map(|d| Device {
                id: text(d.get("enrollmentId")).unwrap_or_default(),
                enrolled_since: number(d.get("enrolledSinceUnix")),
            })
            .collect(),
        text(answer.get("nextPageToken")).unwrap_or_default(),
    )
}
