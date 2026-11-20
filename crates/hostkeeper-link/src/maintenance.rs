//! The maintenance report as the heartbeat's JSON counts it (`Heartbeat::maintenance`, as
//! `maintenance`): the field names of the gRPC message, an enum by its wire name. The report is
//! the daemon's own message (`hostkeeper.agent.v1.MaintenanceReport`), sent as it is; this is
//! only what the bound measures.

use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use serde_json::{Value, json};

/// `report` as JSON with the gRPC message's field names, every field present.
pub(crate) fn maintenance_json(report: &pb::MaintenanceReport) -> Value {
    let host = report.host.clone().unwrap_or_default();
    let xcodes: Vec<Value> = report
        .xcodes
        .iter()
        .map(|xcode| {
            json!({
                "version": xcode.version,
                "build": xcode.build,
                "path": xcode.path,
                "selected": xcode.selected,
                "foreign": xcode.foreign,
            })
        })
        .collect();
    let due: Vec<Value> = report
        .due
        .iter()
        .map(|item| {
            json!({
                "kind": item.kind().as_str_name(),
                "from": item.from,
                "to": item.to,
                "needs_reboot": item.needs_reboot,
                "label": item.label,
                "security": item.security,
            })
        })
        .collect();
    json!({
        "host": {
            "os_name": host.os_name,
            "os_version": host.os_version,
            "os_build": host.os_build,
            "hardware_serial": host.hardware_serial,
            "running_kernel": host.running_kernel,
            "newest_kernel": host.newest_kernel,
            "boot_id": host.boot_id,
            "boot_unix": host.boot_unix,
        },
        "xcodes": xcodes,
        "mac": report.mac.as_ref().map(mac_json),
        "due": due,
        "due_left_out": report.due_left_out,
        "looked_unix": report.looked_unix,
        "unparsed_lines": report.unparsed_lines,
        "run": report.run.as_ref().map(run_json),
        "look_failed": report.look_failed,
    })
}

/// What a macOS update needs, as `mac`.
fn mac_json(mac: &pb::MacUpdateFacts) -> Value {
    let account = mac.update_account.unwrap_or_default();
    let tools = mac.command_line_tools.clone().unwrap_or_default();
    json!({
        "filevault": mac.filevault().as_str_name(),
        "authrestart_supported": mac.authrestart_supported,
        "data_free_bytes": mac.data_free_bytes,
        "update_account": {
            "present": account.present,
            "admin": account.admin,
            "secure_token": account.secure_token,
            "hidden": account.hidden,
        },
        "command_line_tools": {
            "receipt": tools.receipt,
            "files": tools.files,
            "version": tools.version,
        },
        "mdm_conflicts": mac.mdm_conflicts,
    })
}

/// The run's state, as `run`.
fn run_json(run: &pb::MaintenanceRunState) -> Value {
    let applied: Vec<&str> = run.applied().map(|kind| kind.as_str_name()).collect();
    json!({
        "run_id": run.run_id,
        "order_id": run.order_id,
        "step": run.step().as_str_name(),
        "applied": applied,
        "halted_reason": run.halted_reason,
    })
}
