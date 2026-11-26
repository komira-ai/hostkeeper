//! `maintenance-v1` through the conversions of `proto_map`: the report out as `maintenance`, the
//! feature read from a reply's list, and a `maintain` order read into the reply's `maintain`.

use hostkeeper_proto::hostkeeper::agent::v1 as pb;

use hostkeeper_config::names::Version;

use super::{NOT_READ, heartbeat, reply};
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
        full_disk_access: None,
        operator_keys: None,
        prerequisites: None,
        ssh: None,
        bmc: None,
        serial_console: None,
        maintenance,
        health: None,
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

/// Proves a `maintain` order maps to the reply's `maintain` with its order id, its try and the
/// message unchanged, and to no service order, from a coordinator that advertises
/// `maintenance-v1` as from one that does not (the loop decides what to do with it), and that
/// nothing is dropped.
#[test]
fn a_maintain_order_maps_to_the_reply_unchanged() {
    for features in [&[][..], &["maintenance-v1"][..]] {
        let response = maintain(features);
        let Some(pb::order::Verb::Maintain(terms)) =
            response.order.clone().and_then(|order| order.verb)
        else {
            panic!("a maintain order");
        };
        let mapped = reply(response);
        assert_eq!(mapped.reply.order, None, "{features:?}");
        assert_eq!(mapped.dropped, None, "{features:?}");
        let order = mapped.reply.maintain.expect("a maintain order");
        assert_eq!(order.id.as_str(), "o-1");
        assert_eq!(order.order_try, 1);
        assert_eq!(order.maintain, terms);
    }
}

/// Proves a `maintain` order whose order id the daemon's grammar refuses is not given to the
/// loop and comes back dropped, naming the verb and the id.
#[test]
fn a_maintain_order_with_an_unreadable_id_is_dropped() {
    let mut response = maintain(&["maintenance-v1"]);
    response.order.as_mut().expect("an order").order_id = "a/b".to_owned();
    let mapped = reply(response);
    assert_eq!(mapped.reply.order, None);
    assert_eq!(mapped.reply.maintain, None);
    let dropped = mapped.dropped.expect("dropped");
    assert_eq!(dropped.what, "maintain order \"a/b\"");
    assert_eq!(dropped.why, NOT_READ);
}
