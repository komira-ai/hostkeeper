//! The facts of `facts-v1` through the conversions of `proto_map`: the generation out, the host's
//! facts in.

use hostkeeper_config::names::Version;
use hostkeeper_proto::hostkeeper::agent::v1 as pb;

use super::{heartbeat, reply};
use crate::beat::{ANNOUNCED, Heartbeat, Platform};

fn beat(platform: Platform) -> Heartbeat {
    Heartbeat {
        platform,
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
    }
}

fn sent(beat: &Heartbeat) -> pb::HeartbeatRequest {
    heartbeat(beat).expect("a request")
}

/// Proves the generation the facts file holds goes out as `facts_generation`, 0 included, and a
/// heartbeat with none leaves the field out.
#[test]
fn the_facts_generation_goes_out_as_its_field() {
    let mut with = beat(Platform::LinuxX86_64);
    with.facts_generation = Some(12);
    assert_eq!(sent(&with).facts_generation, Some(12));
    with.facts_generation = Some(0);
    assert_eq!(sent(&with).facts_generation, Some(0));
    with.facts_generation = None;
    assert_eq!(sent(&with).facts_generation, None);
}

/// Proves the facts of a response come in whole, as decoded, their labels by key, and a response
/// with none has none.
#[test]
fn a_response_keeps_its_facts_as_decoded() {
    use crate::host_facts::PushedFacts;
    let mapped = reply(pb::HeartbeatResponse {
        facts: Some(pb::HostFacts {
            generation: 6,
            host_id: "web-04".to_owned(),
            host_group: "web".to_owned(),
            labels: [("rack", "r12"), ("az", "b")]
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
        }),
        ..pb::HeartbeatResponse::default()
    });
    assert_eq!(
        mapped.reply.facts,
        Some(PushedFacts {
            generation: 6,
            host_id: "web-04".to_owned(),
            host_group: "web".to_owned(),
            labels: [("az", "b"), ("rack", "r12")]
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
        })
    );
    assert_eq!(mapped.dropped, None);
    assert_eq!(reply(pb::HeartbeatResponse::default()).reply.facts, None);
}

/// Proves the Remote Login probe a heartbeat holds goes out as `remote_login_probe`, field for
/// field and step for step in order (an unstarted step with its error and cut text, and a step
/// that ran with its exit code, argv and both outputs), and a heartbeat with none leaves the field
/// out.
#[test]
fn the_remote_login_probe_goes_out_as_its_field() {
    use crate::probe::{ProbeStep, RemoteLoginProbe};
    let mut with = beat(Platform::DarwinArm64);
    with.remote_login_probe = Some(RemoteLoginProbe {
        started_unix: 1_700_000_123,
        steps: vec![
            ProbeStep {
                name: "systemsetup_on".to_owned(),
                argv: Vec::new(),
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                error: "no such file".to_owned(),
                truncated: true,
            },
            ProbeStep {
                name: "launchctl_print".to_owned(),
                argv: vec![
                    "/bin/launchctl".to_owned(),
                    "print".to_owned(),
                    "system/com.openssh.sshd".to_owned(),
                ],
                exit_code: Some(0),
                stdout: "state = running".to_owned(),
                stderr: "warning".to_owned(),
                error: String::new(),
                truncated: false,
            },
        ],
    });
    assert_eq!(
        sent(&with).remote_login_probe,
        Some(pb::RemoteLoginProbe {
            started_unix: 1_700_000_123,
            steps: vec![
                pb::ProbeStep {
                    name: "systemsetup_on".to_owned(),
                    argv: Vec::new(),
                    exit_code: None,
                    stdout: String::new(),
                    stderr: String::new(),
                    error: "no such file".to_owned(),
                    truncated: true,
                },
                pb::ProbeStep {
                    name: "launchctl_print".to_owned(),
                    argv: vec![
                        "/bin/launchctl".to_owned(),
                        "print".to_owned(),
                        "system/com.openssh.sshd".to_owned(),
                    ],
                    exit_code: Some(0),
                    stdout: "state = running".to_owned(),
                    stderr: "warning".to_owned(),
                    error: String::new(),
                    truncated: false,
                },
            ],
        })
    );
    with.remote_login_probe = None;
    assert_eq!(sent(&with).remote_login_probe, None);
}
