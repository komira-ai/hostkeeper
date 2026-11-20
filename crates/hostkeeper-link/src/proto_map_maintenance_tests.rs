//! `maintenance-v1` through the conversions of `proto_map`: the report out as `maintenance`, the
//! feature read from a reply's list, and a `maintain` order, which this daemon does not run, dropped unrun whatever the reply
//! advertises.

use hostkeeper_proto::hostkeeper::agent::v1 as pb;

use hostkeeper_config::names::Version;

use super::{NOT_RUN, heartbeat, reply};
use crate::beat::{ANNOUNCED, Feature, Heartbeat, Platform};

fn beat(maintenance: Option<pb::MaintenanceReport>) -> Heartbeat {
    Heartbeat {
        platform: Platform::LinuxX86_64,
        agent_version: Version::new("0.9.0").expect("a version"),
        features: ANNOUNCED.to_vec(),
        services: Vec::new(),
        sent_unix: 0,
        renewal: crate::Renewal::default(),
        trusted_root: None,
        installers: Vec::new(),
        repositories: None,
        facts_generation: None,
        remote_login_probe: None,
        operator_keys: None,
        prerequisites: None,
        ssh: None,
        bmc: None,
        maintenance,
    }
}

/// Proves the report a heartbeat holds goes out as `maintenance` field for field, and a
/// heartbeat with none sends none.
#[test]
fn the_report_goes_out_as_its_field() {
    let report = pb::MaintenanceReport {
        host: Some(pb::MaintenanceHost {
            os_name: "debian".to_owned(),
            os_version: "12".to_owned(),
            running_kernel: "6.1.0-1".to_owned(),
            newest_kernel: "6.1.0-2".to_owned(),
            ..pb::MaintenanceHost::default()
        }),
        due: vec![pb::DueItem {
            kind: pb::MaintenanceKind::Reboot.into(),
            needs_reboot: true,
            label: "kernel".to_owned(),
            from: "6.1.0-1".to_owned(),
            ..pb::DueItem::default()
        }],
        due_left_out: 1,
        looked_unix: 1_700_000_000,
        unparsed_lines: 2,
        look_failed: "apt: it failed".to_owned(),
        ..pb::MaintenanceReport::default()
    };
    assert_eq!(
        heartbeat(&beat(Some(report.clone())))
            .expect("a request")
            .maintenance,
        Some(report)
    );
    assert_eq!(heartbeat(&beat(None)).expect("a request").maintenance, None);
}

/// A `maintain` order with every argument set, in a reply that advertises `features`.
fn maintain(features: &[&str]) -> pb::HeartbeatResponse {
    pb::HeartbeatResponse {
        order: Some(pb::Order {
            order_id: "o-1".to_owned(),
            r#try: 1,
            verb: Some(pb::order::Verb::Maintain(pb::Maintain {
                run_id: "run-1".to_owned(),
                action: pb::MaintainAction::Run.into(),
                kinds: vec![pb::MaintenanceKind::Linux.into()],
                linux_scope: pb::LinuxScope::Security.into(),
                drain_deadline_secs: Some(3_600),
                return_secs: 900,
                ..pb::Maintain::default()
            })),
        }),
        features: features.iter().map(|name| (*name).to_owned()).collect(),
        ..pb::HeartbeatResponse::default()
    }
}

/// Proves `maintenance-v1` reads back as the feature of that name, in a response's list as by
/// `Feature::parse`, and that it is not one of the features every daemon announces.
#[test]
fn the_feature_reads_back_from_its_name() {
    assert_eq!(
        Feature::parse("maintenance-v1"),
        Some(Feature::MaintenanceV1)
    );
    assert_eq!(Feature::MaintenanceV1.as_str(), "maintenance-v1");
    assert!(!ANNOUNCED.contains(&Feature::MaintenanceV1));
    let mapped = reply(pb::HeartbeatResponse {
        features: vec!["maintenance-v1".to_owned()],
        ..pb::HeartbeatResponse::default()
    });
    assert_eq!(mapped.reply.features, vec![Feature::MaintenanceV1]);
}

/// Proves a `maintain` order is not given to the loop and comes back as a verb the daemon does
/// not run, from a coordinator that advertises `maintenance-v1` as from one that does not: no
/// host runs maintenance on this release.
#[test]
fn a_maintain_order_is_dropped_unrun() {
    for features in [&[][..], &["maintenance-v1"][..]] {
        let mapped = reply(maintain(features));
        assert_eq!(mapped.reply.order, None, "{features:?}");
        let dropped = mapped.dropped.expect("dropped");
        assert_eq!(dropped.what, "order \"o-1\"");
        assert_eq!(dropped.why, NOT_RUN);
    }
}
