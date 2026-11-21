//! The `Maintenance` calls, as `hk` sends them: the path each posts to and the body its flags
//! make, a kind or a state the proto does not name refused before anything is sent, and every
//! call through the real gateway of a coordinator that holds no run and no enrolled host: each
//! answers what an empty state answers (`schedule`, `get`, `cancel`, `retry-host` and `put-back` a
//! refusal `NOT_FOUND`, exit 1; `list` no runs).

use crate::gateway;
use crate::rig::{Rig, Server, call};

const PREFIX: &str = "/hostkeeper.operator.v1.";

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
                "4102444800",
                "--window-end-unix",
                "4102448400",
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
            r#"{"kinds":["MAINTENANCE_KIND_LINUX","MAINTENANCE_KIND_REBOOT"],"hostIds":["node-01","node-02"],"windowStartUnix":"4102444800","windowEndUnix":"4102448400","batch":2,"drainDeadlineSecs":"0","reason":"monthly update","takeLastHost":true,"confirm":true}"#,
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
    let rig = Rig::new("maintenance-post");
    for (words, method, body) in commands() {
        let server = Server::canned(&rig.pki, 200, "{\"runs\":[]}");
        let ran = rig.hk(&call(&rig, server.port, &words));
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
            (0, "{\"runs\":[]}\n", ""),
            "{words:?}"
        );
        let seen = server.seen();
        assert_eq!(seen.len(), 1, "{words:?}: {seen:#?}");
        assert_eq!(seen[0].method, "POST", "{words:?}");
        assert_eq!(seen[0].target, format!("{PREFIX}{method}"), "{words:?}");
        assert_eq!(seen[0].text(), body, "{words:?}");
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
    let rig = Rig::new("maintenance-enums");
    let server = Server::canned(&rig.pki, 200, "{}");
    for (words, why) in [
        (
            vec![
                "maintenance",
                "schedule",
                "--kinds",
                "windows",
                "--host-group",
                "macs",
            ],
            "--kinds takes one of: macos, xcode, linux, reboot",
        ),
        (
            vec![
                "maintenance",
                "schedule",
                "--kinds",
                "unspecified",
                "--host-group",
                "macs",
            ],
            "--kinds takes one of: macos, xcode, linux, reboot",
        ),
        (
            vec!["maintenance", "list", "--states", "paused"],
            "--states takes one of: scheduled, running, halted, done, cancelled, window_ended",
        ),
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

/// Proves: through the real gateway of a coordinator with no run and no enrolled host, each
/// maintenance command reaches its method with its body and gets what an empty state answers.
/// `schedule` of host ids that are not enrolled is `NOT_FOUND` (HTTP 404) `unknown_host`, of a host
/// group the coordinator does not hold `NOT_FOUND` `unknown_group`; `get` and `cancel` of a run id
/// no run has, and `retry-host` and `put-back` of a host in such a run, are `NOT_FOUND`
/// `unknown_run`; each of those exits 1 with the code and the `refused:` reason on stderr and
/// nothing on stdout. `list` is 200 with no run, printed as the gateway sent it, exit 0.
///
/// Catches: a served method answered `UNIMPLEMENTED`, a missing run or host answered as a
/// refusal of the caller (exit 3) or of the request (exit 2), and a refusal that loses its reason.
#[test]
fn maintenance_commands_through_the_real_gateway_answer_what_an_empty_state_answers() {
    let rig = Rig::new("maintenance-gateway");
    let gateway = gateway::start(&rig.pki, &rig.path(""));
    let not_found = |reason: &str| {
        (
            404,
            1,
            format!("hk: the coordinator answered NOT_FOUND\nrefused: {reason}\n"),
        )
    };
    let wants = [
        not_found("unknown_host"),
        not_found("unknown_group"),
        (200, 0, String::new()),
        (200, 0, String::new()),
        not_found("unknown_run"),
        not_found("unknown_run"),
        not_found("unknown_run"),
        not_found("unknown_run"),
    ];
    for (i, ((words, method, body), (status, code, stderr))) in
        commands().into_iter().zip(wants).enumerate()
    {
        let ran = rig.hk(&call(&rig, gateway.port, &words));
        let seen = gateway.seen();
        assert_eq!(seen.len(), i + 1, "{words:?}: {seen:#?}");
        let last = &seen[i];
        assert_eq!(last.method, "POST", "{words:?}");
        assert_eq!(last.target, format!("{PREFIX}{method}"), "{words:?}");
        assert_eq!(last.text(), body, "{words:?}");
        assert_eq!(last.status, status, "{words:?}");
        let answer = String::from_utf8_lossy(&last.answer).into_owned();
        let stdout = if code == 0 {
            assert!(
                matches!(answer.as_str(), "{}" | "{\"runs\":[]}"),
                "{answer}"
            );
            format!("{answer}\n")
        } else {
            String::new()
        };
        assert_eq!(
            (ran.code, ran.stdout, ran.stderr),
            (code, stdout, stderr),
            "{words:?}"
        );
    }
}
