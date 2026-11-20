//! The `Maintenance` calls, as the real script sends them: the path each posts to and the body
//! curl reads, a kind or a state the proto does not name refused before anything is sent, and
//! every call through the real gateway answered `UNIMPLEMENTED`, as this release's coordinator,
//! which has no handler of the service yet, answers it.

use crate::gateway;
use crate::rig::{Rig, Script, args};

/// `words` after the global flags of a call to the coordinator at `port`.
fn call(rig: &Rig, port: u16, words: &[&str]) -> Vec<String> {
    let mut all = rig.globals(port);
    all.extend(args(words));
    all
}

/// The target and the body of the only `curl` call the rig recorded.
fn posted(rig: &Rig) -> (String, String) {
    let curls = rig.curls();
    assert_eq!(curls.len(), 1, "{curls:#?}");
    let url = curls[0]
        .argv
        .iter()
        .skip_while(|a| *a != "--url")
        .nth(1)
        .expect("a url")
        .clone();
    let body = curls[0]
        .read_after("--data-binary", 0)
        .expect("the body is read from a pipe");
    (url, body)
}

/// Each command of the service, its method, and the body its words make.
fn commands() -> Vec<(Vec<&'static str>, &'static str, &'static str)> {
    vec![
        (
            vec![
                "maintenance",
                "schedule",
                "--kinds",
                "linux",
                "--kinds",
                "reboot",
                "--host-ids",
                "node-01",
                "--host-ids",
                "node-02",
                "--window-start-unix",
                "1800000000",
                "--window-end-unix",
                "1800003600",
                "--batch",
                "2",
                "--drain-deadline-secs",
                "0",
                "--reason",
                "monthly update",
                "--take-last-host",
                "true",
                "--confirm",
                "true",
            ],
            "Maintenance/Schedule",
            r#"{"kinds":["MAINTENANCE_KIND_LINUX","MAINTENANCE_KIND_REBOOT"],"hostIds":["node-01","node-02"],"windowStartUnix":"1800000000","windowEndUnix":"1800003600","batch":2,"drainDeadlineSecs":"0","reason":"monthly update","takeLastHost":true,"confirm":true}"#,
        ),
        (
            vec![
                "maintenance",
                "schedule",
                "--host-group",
                "macs",
                "--reason",
                "update",
                "--confirm",
                "true",
            ],
            "Maintenance/Schedule",
            r#"{"hostGroup":"macs","reason":"update","confirm":true}"#,
        ),
        (
            vec![
                "maintenance",
                "list",
                "--states",
                "running",
                "--states",
                "window_ended",
                "--limit",
                "10",
            ],
            "Maintenance/List",
            r#"{"states":["MAINTENANCE_STATE_RUNNING","MAINTENANCE_STATE_WINDOW_ENDED"],"limit":10}"#,
        ),
        (vec!["maintenance", "list"], "Maintenance/List", "{}"),
        (
            vec!["maintenance", "get", "--run-id", "run-1"],
            "Maintenance/Get",
            r#"{"runId":"run-1"}"#,
        ),
        (
            vec![
                "maintenance",
                "cancel",
                "--run-id",
                "run-1",
                "--reason",
                "stop",
            ],
            "Maintenance/Cancel",
            r#"{"runId":"run-1","reason":"stop"}"#,
        ),
        (
            vec![
                "maintenance",
                "retry-host",
                "--run-id",
                "run-1",
                "--host-id",
                "node-01",
                "--reason",
                "again",
            ],
            "Maintenance/RetryHost",
            r#"{"runId":"run-1","hostId":"node-01","reason":"again"}"#,
        ),
        (
            vec![
                "maintenance",
                "put-back",
                "--run-id",
                "run-1",
                "--host-id",
                "node-01",
                "--reason",
                "give up",
            ],
            "Maintenance/PutBack",
            r#"{"runId":"run-1","hostId":"node-01","reason":"give up"}"#,
        ),
    ]
}

/// Proves: `maintenance schedule`, `list`, `get`, `cancel`, `retry-host` and `put-back` each post
/// to their own method of `Maintenance` a body that holds exactly their flags, under the fields'
/// JSON names: the kinds and states by their enum names, in the order given, the host ids as a
/// list, the window and the drain deadline as 64-bit strings, a drain deadline of 0 kept (no
/// deadline, not the group's), the batch and the limit as numbers and the two ticks as booleans;
/// and each exits 0 with the coordinator's answer on stdout.
///
/// Catches: a maintenance command dispatched to another method, a flag that does not reach its
/// field (the run id, the host id, the host group, the reason), a repeated kind or host id
/// dropped, an enum value sent by its flag spelling, and an explicit drain deadline of 0 lost.
#[test]
fn each_maintenance_command_posts_its_flags_to_its_method() {
    for (words, method, body) in commands() {
        let rig = Rig::new("maintenance-post");
        rig.reply(200, "{\"runs\":[]}");
        let ran = rig.run(Script::Real, &call(&rig, 1, &words), b"", &[]);
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
            (0, "{\"runs\":[]}\n", ""),
            "{words:?}"
        );
        let (url, sent) = posted(&rig);
        assert_eq!(
            url,
            format!("https://127.0.0.1:1/hostkeeper.operator.v1.{method}")
        );
        assert_eq!(sent, body, "{words:?}");
    }
}

/// Proves: a kind or a state the proto does not name (`windows`, `paused`), and the unspecified
/// zero, are usage errors that exit 2 with exactly the flag and its values, and nothing is sent
/// to the coordinator.
///
/// Catches: a run scheduled with a kind the coordinator would have to guess at, and a list
/// filter that silently matches nothing.
#[test]
fn a_kind_or_state_the_proto_does_not_name_is_refused_before_anything_is_sent() {
    for (words, stderr) in [
        (
            &[
                "maintenance",
                "schedule",
                "--kinds",
                "windows",
                "--host-group",
                "macs",
            ][..],
            "hkctl: --kinds takes one of: macos, xcode, linux, reboot\n",
        ),
        (
            &[
                "maintenance",
                "schedule",
                "--kinds",
                "unspecified",
                "--host-group",
                "macs",
            ][..],
            "hkctl: --kinds takes one of: macos, xcode, linux, reboot\n",
        ),
        (
            &["maintenance", "list", "--states", "paused"][..],
            "hkctl: --states takes one of: scheduled, running, halted, done, cancelled, \
             window_ended\n",
        ),
    ] {
        let rig = Rig::new("maintenance-usage");
        rig.reply(200, "{}");
        let ran = rig.run(Script::Real, &call(&rig, 1, words), b"", &[]);
        assert_eq!(ran.code, 2, "{words:?}: {}", ran.stderr);
        assert_eq!(ran.stderr, stderr, "{words:?}");
        assert!(rig.curls().is_empty(), "{words:?}");
    }
}

/// Proves: every maintenance command, through the real gateway, reaches the coordinator's method
/// of `Maintenance` with its body, and the coordinator, which serves no handler of the service
/// yet, answers it `UNIMPLEMENTED` (HTTP 501): `hkctl` exits 1 with exactly the code on stderr, no
/// `refused:` line and nothing on stdout.
///
/// Catches: a coordinator that serves a maintenance method before its handler lands, an
/// `UNIMPLEMENTED` answer that exits 0 or as a refusal of the caller (exit 3), and a command whose
/// path the gateway does not know.
#[test]
fn each_maintenance_command_through_the_real_gateway_is_unimplemented() {
    for (words, method, body) in commands() {
        let rig = Rig::new("maintenance-gateway");
        let gateway = gateway::start(&rig.path(""));
        let ran = rig.run(Script::Real, &call(&rig, gateway.port, &words), b"", &[]);
        let seen = gateway.seen();
        assert_eq!(seen.len(), 1, "{seen:#?}");
        assert_eq!(
            seen[0].target(),
            format!("/hostkeeper.operator.v1.{method}")
        );
        assert_eq!(String::from_utf8_lossy(&seen[0].body), body, "{words:?}");
        assert_eq!(seen[0].status, 501, "{words:?}");
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
            (1, "", "hkctl: the coordinator answered UNIMPLEMENTED\n"),
            "{words:?}"
        );
    }
}
