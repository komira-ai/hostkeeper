//! The host's BMC through the conversions of `proto_map`: the report out as `bmc`.

use hostkeeper_config::names::Version;
use hostkeeper_proto::hostkeeper::agent::v1 as pb;

use super::heartbeat;
use crate::beat::{ANNOUNCED, Heartbeat, Platform};
use crate::bmc::BmcReport;

fn beat(bmc: Option<BmcReport>) -> Heartbeat {
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
        ssh: None,
        prerequisites: None,
        bmc,
    }
}

/// Proves the BMC a heartbeat holds goes out as `bmc`, its address dotted and its MAC as read,
/// and a heartbeat with none leaves the field out (absent, not an empty report).
#[test]
fn the_bmc_goes_out_as_its_field() {
    let report = BmcReport {
        ip: "198.51.100.3".to_owned(),
        mac: "0c:c4:7a:ab:cd:ef".to_owned(),
    };
    assert_eq!(
        heartbeat(&beat(Some(report))).expect("a request").bmc,
        Some(pb::BmcReport {
            ip: "198.51.100.3".to_owned(),
            mac: "0c:c4:7a:ab:cd:ef".to_owned(),
        })
    );
    assert_eq!(heartbeat(&beat(None)).expect("a request").bmc, None);
}
