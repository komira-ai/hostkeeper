//! The maintenance reports the tests of `additions_maintenance*.rs` build: one at every bound of
//! `hostkeeper_proto::maintenance`, and a Linux host's as a daemon words it.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::maintenance::{
    MAX_APPLIED, MAX_DUE, MAX_DUE_LABEL, MAX_DUE_VERSION, MAX_HALTED_REASON, MAX_KERNEL_TEXT,
    MAX_LOOK_FAILED, MAX_MAC_TEXT, MAX_MDM_CONFLICTS, MAX_ON_DEMAND, MAX_OS_TEXT, MAX_PROBE_NAME,
    MAX_PROBE_OUTPUT, MAX_PROBE_STEPS, MAX_RUN_ID, MAX_XCODE_PATH, MAX_XCODE_VERSION, MAX_XCODES,
};

/// `n` bytes of text.
pub fn text(n: usize) -> String {
    "a".repeat(n)
}

/// A Mac's report with every list and every text at its bound, and every number at its largest.
pub fn at_every_bound() -> agent::MaintenanceReport {
    agent::MaintenanceReport {
        host: Some(agent::MaintenanceHost {
            os_name: text(MAX_OS_TEXT),
            os_version: text(MAX_OS_TEXT),
            os_build: text(MAX_OS_TEXT),
            hardware_serial: text(MAX_OS_TEXT),
            running_kernel: text(MAX_KERNEL_TEXT),
            newest_kernel: text(MAX_KERNEL_TEXT),
            boot_id: text(MAX_KERNEL_TEXT),
            boot_unix: u64::MAX,
        }),
        xcodes: vec![
            agent::InstalledXcode {
                version: text(MAX_XCODE_VERSION),
                build: text(MAX_XCODE_VERSION),
                path: text(MAX_XCODE_PATH),
                selected: true,
                foreign: true,
            };
            MAX_XCODES
        ],
        mac: Some(agent::MacUpdateFacts {
            filevault: agent::FileVaultState::Decrypting.into(),
            authrestart_supported: true,
            data_free_bytes: Some(u64::MAX),
            update_account: Some(agent::UpdateAccountState {
                present: true,
                admin: true,
                secure_token: true,
                hidden: true,
            }),
            command_line_tools: Some(agent::CommandLineToolsState {
                receipt: true,
                files: true,
                version: text(MAX_MAC_TEXT),
            }),
            mdm_conflicts: vec![text(MAX_MAC_TEXT); MAX_MDM_CONFLICTS],
        }),
        due: vec![
            agent::DueItem {
                kind: agent::MaintenanceKind::Reboot.into(),
                from: text(MAX_DUE_VERSION),
                to: text(MAX_DUE_VERSION),
                needs_reboot: true,
                label: text(MAX_DUE_LABEL),
                security: true,
            };
            MAX_DUE
        ],
        due_left_out: u32::MAX,
        looked_unix: u64::MAX,
        unparsed_lines: u32::MAX,
        run: Some(agent::MaintenanceRunState {
            run_id: text(MAX_RUN_ID),
            order_id: text(MAX_RUN_ID),
            step: agent::MaintenanceStep::Halted.into(),
            applied: vec![agent::MaintenanceKind::Reboot.into(); MAX_APPLIED],
            halted_reason: text(MAX_HALTED_REASON),
        }),
        look_failed: text(MAX_LOOK_FAILED),
        probe: Some(agent::MaintenanceProbe {
            run_id: text(MAX_RUN_ID),
            probed_unix: u64::MAX,
            steps: vec![
                agent::MaintenanceProbeStep {
                    name: text(MAX_PROBE_NAME),
                    exit_code: Some(i32::MIN),
                    output: text(MAX_PROBE_OUTPUT),
                    cut: true,
                };
                MAX_PROBE_STEPS
            ],
        }),
        on_demand: vec![agent::MaintenanceKind::Probe.into(); MAX_ON_DEMAND],
    }
}

/// A Linux host's report as a daemon words it: one security update and a reboot due, no Mac facts
/// and no run.
pub fn linux_report() -> agent::MaintenanceReport {
    agent::MaintenanceReport {
        host: Some(agent::MaintenanceHost {
            os_name: "ubuntu".to_owned(),
            os_version: "24.04".to_owned(),
            running_kernel: "6.8.0-49-generic".to_owned(),
            newest_kernel: "6.8.0-51-generic".to_owned(),
            boot_id: "6f1c2d4e-0b7a-4c55-9a1e-3d2b8c7f6e5a".to_owned(),
            boot_unix: 1_800_000_000,
            ..agent::MaintenanceHost::default()
        }),
        due: vec![
            agent::DueItem {
                kind: agent::MaintenanceKind::Linux.into(),
                from: "3.0.13-0ubuntu3.4".to_owned(),
                to: "3.0.13-0ubuntu3.5".to_owned(),
                label: "openssl".to_owned(),
                security: true,
                ..agent::DueItem::default()
            },
            agent::DueItem {
                kind: agent::MaintenanceKind::Reboot.into(),
                from: "6.8.0-49-generic".to_owned(),
                needs_reboot: true,
                label: "kernel".to_owned(),
                ..agent::DueItem::default()
            },
        ],
        looked_unix: 1_800_003_600,
        ..agent::MaintenanceReport::default()
    }
}
