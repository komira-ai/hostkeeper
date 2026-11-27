//! `hk hardware log`, as `hk` sends it: the path it posts to and the body its flags make, a
//! component or a severity the proto does not name refused before anything is sent, and the call
//! through the real gateway of a coordinator whose hardware event log holds no record.

use crate::gateway;
use crate::rig::{Rig, Server, call};

/// The path of `Hardware.Log`.
const LOG: &str = "/hostkeeper.operator.v1.Hardware/Log";

/// Proves: `hk hardware log` posts to `Hardware.Log` with its flags under their JSON names: the
/// host id and the serial as texts, `--since-unix` as a 64-bit string, the component and the
/// severity by their enum names, the limit as a number and the page token as text; with no flag
/// the body is empty; each exits 0 with the coordinator's answer on stdout.
///
/// Catches: the command dispatched to another method, a filter that does not reach its field,
/// and an enum value sent by its flag spelling.
#[test]
fn hardware_log_posts_its_filters_to_its_method() {
    let rig = Rig::new("hardware-log-post");
    let cases = [
        (
            vec![
                "hardware",
                "log",
                "--host-id",
                "db-1",
                "--serial",
                "SN-1",
                "--since-unix",
                "1795662000",
                "--component",
                "memory",
                "--severity",
                "warning",
                "--limit",
                "10",
                "--page-token",
                "1795662012.db-1.sel:01a4",
            ],
            r#"{"hostId":"db-1","serial":"SN-1","sinceUnix":"1795662000","component":"HARDWARE_COMPONENT_MEMORY","severity":"HARDWARE_SEVERITY_WARNING","limit":10,"pageToken":"1795662012.db-1.sel:01a4"}"#,
        ),
        (vec!["hardware", "log"], "{}"),
    ];
    for (words, body) in cases {
        let server = Server::canned(&rig.pki, 200, "{\"records\":[]}");
        let ran = rig.hk(&call(&rig, server.port, &words));
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
            (0, "{\"records\":[]}\n", ""),
            "{words:?}"
        );
        let seen = server.seen();
        assert_eq!(seen.len(), 1, "{words:?}: {seen:#?}");
        assert_eq!(
            (seen[0].method.as_str(), seen[0].target.as_str()),
            ("POST", LOG),
            "{words:?}"
        );
        assert_eq!(seen[0].text(), body, "{words:?}");
    }
}

/// Proves: a component or a severity the proto does not name (`gpu`, `fatal`), and the
/// unspecified zero, are usage errors that exit 2 with exactly the flag and its values, and
/// nothing is sent to the coordinator.
///
/// Catches: a filter the coordinator would refuse sent anyway, or one that silently lists every
/// record.
#[test]
fn a_component_or_severity_the_proto_does_not_name_is_refused_before_anything_is_sent() {
    let rig = Rig::new("hardware-log-enums");
    let server = Server::canned(&rig.pki, 200, "{}");
    let components =
        "--component takes one of: memory, cpu, pcie, disk, thermal, power, board, host";
    let severities = "--severity takes one of: info, warning, critical";
    for (words, why) in [
        (vec!["hardware", "log", "--component", "gpu"], components),
        (
            vec!["hardware", "log", "--component", "unspecified"],
            components,
        ),
        (vec!["hardware", "log", "--severity", "fatal"], severities),
    ] {
        let ran = rig.hk(&call(&rig, server.port, &words));
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr),
            (2, "", format!("hk: {why}\n")),
            "{words:?}"
        );
    }
    assert!(server.seen().is_empty());
}

/// Proves: through the real gateway of a coordinator whose log holds no record, `hk hardware
/// log` reaches `Hardware.Log` and is answered 200 with no record, printed as the gateway sent
/// it, exit 0; a page token the coordinator did not give is `INVALID_ARGUMENT` (HTTP 400)
/// `invalid_request`, exit 2, with the code and the reason on stderr.
///
/// Catches: the method answered `UNIMPLEMENTED` by a coordinator that does not serve it, and a
/// refusal of the request answered as one of the caller (exit 3).
#[test]
fn hardware_log_through_the_real_gateway_answers_an_empty_log() {
    let rig = Rig::new("hardware-log-gateway");
    let gateway = gateway::start(&rig.pki, &rig.path(""));
    let ran = rig.hk(&call(&rig, gateway.port, &["hardware", "log"]));
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (0, "{}\n", "")
    );
    let ran = rig.hk(&call(
        &rig,
        gateway.port,
        &["hardware", "log", "--page-token", "not a token"],
    ));
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (
            2,
            "",
            "hk: the coordinator answered INVALID_ARGUMENT\nrefused: invalid_request\n"
        )
    );
    let seen = gateway.seen();
    assert_eq!(
        seen.iter()
            .map(|s| (s.target.as_str(), s.status))
            .collect::<Vec<_>>(),
        [(LOG, 200), (LOG, 400)]
    );
}
