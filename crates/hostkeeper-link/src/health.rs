//! The host's hardware health as the heartbeat's JSON counts it (`Heartbeat::health`, as
//! `health`), and a service's restarts (`ServiceFacts::restarts`, as `restarts`): the field names
//! of the gRPC messages, an enum by its wire name. Both are the daemon's own messages
//! (`hostkeeper.agent.v1.HostHealth`, `ServiceRestarts`), sent as they are; this is only what the
//! bound measures.

use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use serde_json::{Value, json};

/// `health` as JSON with the gRPC message's field names, every field present.
pub(crate) fn health_json(health: &pb::HostHealth) -> Value {
    let events: Vec<Value> = health
        .events
        .iter()
        .map(|event| {
            json!({
                "event_id": event.event_id,
                "time_unix": event.time_unix,
                "of_previous_boot": event.of_previous_boot,
                "component": event.component().as_str_name(),
                "subject": event.subject,
                "severity": event.severity().as_str_name(),
                "state": event.state().as_str_name(),
                "source": event.source,
                "code": event.code,
                "message": event.message,
                "raw": event.raw,
                "about": event.about,
            })
        })
        .collect();
    let sources: Vec<Value> = health
        .sources
        .iter()
        .map(|source| {
            json!({
                "source": source.source,
                "status": source.status().as_str_name(),
                "reason": source.reason,
            })
        })
        .collect();
    json!({
        "events": events,
        "baseline": health.baseline.as_ref().map(baseline_json),
        "boot_id": health.boot_id,
        "boot_unix": health.boot_unix,
        "clean_previous_shutdown": health.clean_previous_shutdown,
        "sources": sources,
    })
}

/// The host's capacity, as `baseline`.
fn baseline_json(baseline: &pb::Baseline) -> Value {
    let devices: Vec<Value> = baseline
        .memory_devices
        .iter()
        .map(|device| json!({ "locator": device.locator, "size_bytes": device.size_bytes }))
        .collect();
    let disks: Vec<Value> = baseline
        .disks
        .iter()
        .map(|disk| {
            json!({
                "name": disk.name,
                "size_bytes": disk.size_bytes,
                "serial_digest": disk.serial_digest,
            })
        })
        .collect();
    let nics: Vec<Value> = baseline
        .nics
        .iter()
        .map(|nic| json!({ "name": nic.name, "speed_mbps": nic.speed_mbps }))
        .collect();
    json!({
        "mem_total_kib": baseline.mem_total_kib,
        "memory_devices": devices,
        "cpus_present": baseline.cpus_present,
        "cpus_online": baseline.cpus_online,
        "smt": baseline.smt,
        "disks": disks,
        "nics": nics,
    })
}

/// A service's restarts as `restarts`.
pub(crate) fn restarts_json(restarts: &pb::ServiceRestarts) -> Value {
    json!({
        "count": restarts.count,
        "recent": restarts.recent,
        "last_result": restarts.last_result,
        "exit_status": restarts.exit_status,
    })
}
