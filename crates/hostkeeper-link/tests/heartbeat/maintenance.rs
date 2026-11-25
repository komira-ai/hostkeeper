//! The host's maintenance report: announced, and sent, only to a coordinator that advertises
//! `maintenance-v1`, and counted toward the heartbeat's bound. Its mapping onto the gRPC request
//! is tested in `proto_map`.

use hostkeeper_link::testing::World;
use hostkeeper_link::{
    BeatOutcome, DEFAULT_HEARTBEAT_SECS, Feature, Heartbeater, LoopConfig, MaintainOrder, Platform,
    Reply,
};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use serde_json::json;

use crate::{Rig, order_id, reply, version};

/// A report with every part of it set, as a Mac's daemon in the middle of a run would send it.
fn full_report() -> pb::MaintenanceReport {
    pb::MaintenanceReport {
        host: Some(pb::MaintenanceHost {
            os_name: "ubuntu".to_owned(),
            os_version: "24.04".to_owned(),
            os_build: "24A1".to_owned(),
            hardware_serial: "C02X".to_owned(),
            running_kernel: "6.8.0-45-generic".to_owned(),
            newest_kernel: "6.8.0-48-generic".to_owned(),
            boot_id: "b0".to_owned(),
            boot_unix: 1_700_000_000,
        }),
        xcodes: vec![pb::InstalledXcode {
            version: "16.1".to_owned(),
            build: "16B40".to_owned(),
            path: "/Applications/Xcode-16.1.app".to_owned(),
            selected: true,
            foreign: false,
        }],
        mac: Some(pb::MacUpdateFacts {
            filevault: pb::FileVaultState::On.into(),
            authrestart_supported: true,
            data_free_bytes: Some(1_000),
            update_account: Some(pb::UpdateAccountState {
                present: true,
                admin: false,
                secure_token: true,
                hidden: true,
            }),
            command_line_tools: Some(pb::CommandLineToolsState {
                receipt: true,
                files: false,
                version: "16.0".to_owned(),
            }),
            mdm_conflicts: vec!["com.apple.SoftwareUpdate".to_owned()],
        }),
        due: vec![pb::DueItem {
            kind: pb::MaintenanceKind::Linux.into(),
            from: "1.0".to_owned(),
            to: "1.1".to_owned(),
            needs_reboot: true,
            label: "linux-image-generic".to_owned(),
            security: true,
        }],
        due_left_out: 2,
        looked_unix: 1_700_000_100,
        unparsed_lines: 3,
        run: Some(pb::MaintenanceRunState {
            run_id: "run-1".to_owned(),
            order_id: "o-1".to_owned(),
            step: pb::MaintenanceStep::Draining.into(),
            applied: vec![pb::MaintenanceKind::Linux.into()],
            halted_reason: "a halt".to_owned(),
        }),
        look_failed: "apt: it failed".to_owned(),
        probe: Some(pb::MaintenanceProbe {
            run_id: "run-0".to_owned(),
            probed_unix: 1_700_000_050,
            steps: vec![pb::MaintenanceProbeStep {
                name: "console".to_owned(),
                exit_code: Some(0),
                output: "root".to_owned(),
                cut: false,
            }],
        }),
        on_demand: vec![pb::MaintenanceKind::Probe.into()],
    }
}

/// Proves a host with a maintenance source announces `maintenance-v1` (after `ssh-v1`) and sends
/// its report only while the coordinator's last reply advertised it: the first heartbeat, before
/// any reply, and one after a reply that no longer advertises it carry neither the name nor a
/// report, and the source is not asked for them.
///
/// Catches: the report sent, or the name announced, to a coordinator that does not read it.
#[test]
fn the_report_and_the_name_go_only_to_a_coordinator_that_advertises_it() {
    let world = World::default().setting_ssh_up().keeping_maintenance();
    let rig = Rig::with_world("maintenance-advertised", world);
    rig.world.maintenance.set(full_report());
    let mut heartbeater = rig.heartbeater();
    rig.world
        .link
        .answer(Ok(reply(None, &[Feature::MaintenanceV1, Feature::SshV1])));
    rig.world.link.answer(Ok(reply(None, &[])));
    for _ in 0..3 {
        assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    }
    let beats = rig.world.link.beats();
    assert_eq!(beats.len(), 3);
    assert_eq!(beats[0].maintenance, None);
    assert!(!beats[0].features.contains(&Feature::MaintenanceV1));
    assert_eq!(beats[1].maintenance, Some(full_report()));
    assert_eq!(
        &beats[1].features[beats[1].features.len() - 2..],
        [Feature::SshV1, Feature::MaintenanceV1]
    );
    assert_eq!(beats[2].maintenance, None);
    assert!(!beats[2].features.contains(&Feature::MaintenanceV1));
    assert_eq!(rig.world.maintenance.asked(), 1);
}

/// Proves a host without a maintenance source announces nothing of it and sends no report, even to
/// a coordinator that advertises `maintenance-v1`.
#[test]
fn a_host_without_a_source_announces_and_sends_nothing() {
    let rig = Rig::new("maintenance-none");
    let mut heartbeater = rig.heartbeater();
    rig.world
        .link
        .answer(Ok(reply(None, &[Feature::MaintenanceV1])));
    heartbeater.beat_once();
    heartbeater.beat_once();
    for beat in rig.world.link.beats() {
        assert_eq!(beat.maintenance, None);
        assert!(!beat.features.contains(&Feature::MaintenanceV1));
    }
}

/// Proves the report counts toward the heartbeat's bound: the heartbeat's JSON holds it as
/// `maintenance`, with the gRPC message's field names and the enums by their wire names, and one
/// without it holds none.
#[test]
fn a_report_is_counted_as_json_with_the_names_of_the_message() {
    let world = World::default().keeping_maintenance();
    let rig = Rig::with_world("maintenance-json", world);
    rig.world.maintenance.set(full_report());
    let mut heartbeater = rig.heartbeater();
    rig.world
        .link
        .answer(Ok(reply(None, &[Feature::MaintenanceV1])));
    heartbeater.beat_once();
    heartbeater.beat_once();
    let mut beat = rig.world.link.beats().remove(1);
    assert_eq!(
        beat.to_json()["maintenance"],
        json!({
            "host": {
                "os_name": "ubuntu",
                "os_version": "24.04",
                "os_build": "24A1",
                "hardware_serial": "C02X",
                "running_kernel": "6.8.0-45-generic",
                "newest_kernel": "6.8.0-48-generic",
                "boot_id": "b0",
                "boot_unix": 1_700_000_000_u64,
            },
            "xcodes": [{
                "version": "16.1",
                "build": "16B40",
                "path": "/Applications/Xcode-16.1.app",
                "selected": true,
                "foreign": false,
            }],
            "mac": {
                "filevault": "FILE_VAULT_STATE_ON",
                "authrestart_supported": true,
                "data_free_bytes": 1000,
                "update_account": {
                    "present": true,
                    "admin": false,
                    "secure_token": true,
                    "hidden": true,
                },
                "command_line_tools": { "receipt": true, "files": false, "version": "16.0" },
                "mdm_conflicts": ["com.apple.SoftwareUpdate"],
            },
            "due": [{
                "kind": "MAINTENANCE_KIND_LINUX",
                "from": "1.0",
                "to": "1.1",
                "needs_reboot": true,
                "label": "linux-image-generic",
                "security": true,
            }],
            "due_left_out": 2,
            "looked_unix": 1_700_000_100_u64,
            "unparsed_lines": 3,
            "run": {
                "run_id": "run-1",
                "order_id": "o-1",
                "step": "MAINTENANCE_STEP_DRAINING",
                "applied": ["MAINTENANCE_KIND_LINUX"],
                "halted_reason": "a halt",
            },
            "look_failed": "apt: it failed",
        })
    );
    let with = beat.encoded_len();
    beat.maintenance = Some(pb::MaintenanceReport::default());
    let bare = beat.to_json()["maintenance"].clone();
    assert_eq!(bare["mac"], json!(null));
    assert_eq!(bare["run"], json!(null));
    assert_eq!(bare["due"], json!([]));
    beat.maintenance = None;
    assert_eq!(beat.to_json().get("maintenance"), None);
    assert!(with > beat.encoded_len());
}

/// A reply that carries the `maintain` order `o-1` try 1 and advertises `features`.
fn maintaining(features: &[Feature]) -> (Reply, MaintainOrder) {
    let order = MaintainOrder {
        id: order_id("o-1"),
        order_try: 1,
        maintain: pb::Maintain {
            run_id: "run-1".to_owned(),
            action: pb::MaintainAction::Run.into(),
            kinds: vec![pb::MaintenanceKind::Linux.into()],
            ..pb::Maintain::default()
        },
    };
    let carrying = Reply {
        maintain: Some(order.clone()),
        features: features.to_vec(),
        ..Reply::default()
    };
    (carrying, order)
}

/// Proves a `maintain` order of a reply that advertises `maintenance-v1` is given to the
/// daemon's runner on a worker of the dispatcher, once per reply: the heartbeat that takes it
/// gives nothing yet, running the worker gives the order unchanged, and the same order in a
/// second reply is given again, for the daemon to recognise by its id and try.
///
/// Catches: a `maintain` order dropped, run on the loop's own thread, or kept back as a resend.
#[test]
fn a_maintain_order_goes_to_the_runner_on_a_worker_once_per_reply() {
    let rig = Rig::with_world("maintain-given", World::default().keeping_maintenance());
    let mut heartbeater = rig.heartbeater();
    let (carrying, order) = maintaining(&[Feature::MaintenanceV1]);
    rig.world.link.answer(Ok(carrying.clone()));
    rig.world.link.answer(Ok(carrying));
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 1);
    assert_eq!(rig.world.maintain.given(), Vec::new());
    assert!(rig.world.dispatch.run_next());
    assert_eq!(rig.world.maintain.given(), vec![order.clone()]);
    heartbeater.beat_once();
    assert!(rig.world.dispatch.run_next());
    assert_eq!(rig.world.maintain.given(), vec![order.clone(), order]);
    assert_eq!(rig.world.orders.orders(), Vec::new());
    assert_eq!(rig.world.log.lines(), Vec::<String>::new());
}

/// Proves a `maintain` order from a coordinator that does not advertise `maintenance-v1` is not
/// given to the runner, and the loop says so.
///
/// Catches: an order run for a coordinator that did not say it speaks maintenance.
#[test]
fn a_maintain_order_without_the_feature_is_not_given_and_is_logged() {
    let rig = Rig::with_world(
        "maintain-unadvertised",
        World::default().keeping_maintenance(),
    );
    let mut heartbeater = rig.heartbeater();
    rig.world.link.answer(Ok(maintaining(&[]).0));
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 0);
    assert_eq!(rig.world.maintain.given(), Vec::new());
    assert_eq!(
        rig.world.log.lines(),
        vec![
            "maintain order o-1 try 1 is not run: the coordinator does not advertise maintenance-v1"
                .to_owned()
        ]
    );
}

/// Proves a `maintain` order on a host with no runner is not run, whatever the coordinator
/// advertises, and the loop says so.
///
/// Catches: a host with no runner that starts a job for the order all the same.
#[test]
fn a_maintain_order_on_a_host_with_no_runner_is_not_run_and_is_logged() {
    let rig = Rig::new("maintain-no-runner");
    let mut heartbeater = rig.heartbeater();
    rig.world
        .link
        .answer(Ok(maintaining(&[Feature::MaintenanceV1]).0));
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 0);
    assert_eq!(
        rig.world.log.lines(),
        vec!["maintain order o-1 try 1 is not run: this host has no runner of it".to_owned()]
    );
}

/// Proves a daemon that reads the host's maintenance but has no runner of `maintain` orders
/// announces no `maintenance-v1`, and sends no report, to a coordinator that advertises it.
///
/// Catches: the feature announced on the source alone, so the coordinator orders a run no
/// daemon takes.
#[test]
fn a_source_without_a_runner_announces_no_maintenance() {
    let rig = Rig::with_world(
        "maintain-source-only",
        World::default().keeping_maintenance(),
    );
    let mut ports = rig.world.ports();
    ports.maintain = None;
    let mut heartbeater = Heartbeater::new(
        LoopConfig {
            platform: Platform::LinuxX86_64,
            agent_version: version("0.9.0"),
            state_dir: rig.state().to_path_buf(),
            period_secs: DEFAULT_HEARTBEAT_SECS,
            installers: Vec::new(),
            private_repository: false,
        },
        ports,
    );
    rig.world
        .link
        .answer(Ok(reply(None, &[Feature::MaintenanceV1])));
    heartbeater.beat_once();
    heartbeater.beat_once();
    for beat in rig.world.link.beats() {
        assert_eq!(beat.maintenance, None);
        assert!(!beat.features.contains(&Feature::MaintenanceV1));
    }
    assert_eq!(rig.world.maintenance.asked(), 0);
}
