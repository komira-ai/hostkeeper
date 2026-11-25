//! The serial console baseline through the conversions of `proto_map`: the report out as
//! `serial_console`.

use hostkeeper_config::names::Version;
use hostkeeper_proto::hostkeeper::agent::v1 as pb;

use super::heartbeat;
use crate::beat::{ANNOUNCED, Heartbeat, Platform};
use crate::serial_console::SerialConsoleReport;

fn beat(serial_console: Option<SerialConsoleReport>) -> Heartbeat {
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
        ssh: None,
        prerequisites: None,
        bmc: None,
        serial_console,
        maintenance: None,
    }
}

/// Proves the baseline a heartbeat holds goes out as `serial_console`, each of its three fields
/// as the daemon said it, and a heartbeat with none leaves the field out (absent, not an empty
/// report).
#[test]
fn the_serial_console_goes_out_as_its_field() {
    let report = SerialConsoleReport {
        port: "ttyS1".to_owned(),
        state: "failed".to_owned(),
        detail: "update_grub".to_owned(),
    };
    assert_eq!(
        heartbeat(&beat(Some(report)))
            .expect("a request")
            .serial_console,
        Some(pb::SerialConsoleReport {
            port: "ttyS1".to_owned(),
            state: "failed".to_owned(),
            detail: "update_grub".to_owned(),
        })
    );
    assert_eq!(
        heartbeat(&beat(None)).expect("a request").serial_console,
        None
    );
}
