//! The methods of the real protos the operator's tools call: the path each posts to, the body the
//! flags make (the keys in the order of the table's fields, under their JSON names), the answer
//! printed, and the refusals that end them.

use crate::rig::{Rig, Server, call};

const PREFIX: &str = "/hostkeeper.operator.v1.";

/// Runs the real `hk` with `words` against a server that answers `status` and `body`: what it
/// ended with, and the method and the body of the one request it posted.
fn post(rig: &Rig, words: &[&str], status: u16, body: &str) -> (crate::rig::Ran, String, String) {
    let server = Server::canned(&rig.pki, status, body);
    let ran = rig.hk(&call(rig, server.port, words));
    let seen = server.seen();
    assert_eq!(seen.len(), 1, "{words:?}: {seen:#?}");
    assert_eq!(seen[0].method, "POST");
    assert_eq!(
        seen[0].header("content-type"),
        Some("application/json"),
        "{words:?}"
    );
    (ran, seen[0].target.clone(), seen[0].text())
}

/// Proves: each of these calls posts to the method of its command exactly the body its flags
/// make, and prints the coordinator's answer with one newline: the accounts (a name; a list's
/// empty request; an optional owner that is absent when not given), the console (enum flags as
/// their full names, bool flags as JSON booleans, a probe's window as numbers), the host groups (an empty group name is a
/// given flag, a 64-bit generation is a string and follows the request file's text), the
/// repositories of a service (an empty one clears it), `hosts retry`, and the methods of the
/// service `ssh`.
#[test]
fn each_method_posts_the_body_its_flags_make() {
    let rig = Rig::new("methods");
    let fingerprint_a = format!("SHA256:{}", "A".repeat(43));
    let fingerprint_b = format!("SHA256:{}", "B".repeat(43));
    let group = rig.put(
        "group.json",
        b"{\"hostGroup\":{\"name\":\"web\",\"packages\":[\"hostkeeper\",\"tierstore\"]}}",
        0o644,
    );
    let desired = rig.put(
        "desired.json",
        b"{\"desired\":{\"version\":\"1.2.3\"}}",
        0o644,
    );
    let cases: Vec<(Vec<&str>, &str, String)> = vec![
        (
            vec!["users", "create", "--name", "carol"],
            "Users/Create",
            r#"{"name":"carol"}"#.to_owned(),
        ),
        (
            vec!["users", "disable", "--name", "carol"],
            "Users/Disable",
            r#"{"name":"carol"}"#.to_owned(),
        ),
        (
            vec!["users", "enable", "--name", "carol"],
            "Users/Enable",
            r#"{"name":"carol"}"#.to_owned(),
        ),
        (
            vec!["users", "reset-password", "--name", "carol"],
            "Users/ResetPassword",
            r#"{"name":"carol"}"#.to_owned(),
        ),
        (
            vec!["users", "delete", "--name", "carol"],
            "Users/Delete",
            r#"{"name":"carol"}"#.to_owned(),
        ),
        (
            vec!["api-tokens", "create", "--name", "carol"],
            "ApiTokens/Create",
            r#"{"name":"carol"}"#.to_owned(),
        ),
        (
            vec!["api-tokens", "revoke", "--name", "carol"],
            "ApiTokens/Revoke",
            r#"{"name":"carol"}"#.to_owned(),
        ),
        (vec!["users", "list"], "Users/List", "{}".to_owned()),
        (vec!["api-tokens", "list"], "ApiTokens/List", "{}".to_owned()),
        (
            vec!["api-tokens", "create", "--name", "laptop", "--owner", "erin"],
            "ApiTokens/Create",
            r#"{"name":"laptop","owner":"erin"}"#.to_owned(),
        ),
        (
            vec![
                "console", "open", "--host-id", "node-02", "--reason", "boot order",
            ],
            "Console/Open",
            r#"{"hostId":"node-02","reason":"boot order"}"#.to_owned(),
        ),
        (
            vec![
                "console", "power", "--host-id", "node-02", "--action", "cycle", "--reason",
                "hung",
            ],
            "Console/Power",
            r#"{"hostId":"node-02","action":"POWER_ACTION_CYCLE","reason":"hung"}"#.to_owned(),
        ),
        (
            vec![
                "console",
                "set-boot-device",
                "--host-id",
                "node-02",
                "--device",
                "disk",
                "--persistent",
                "true",
                "--efi",
                "false",
                "--reason",
                "boot order",
            ],
            "Console/SetBootDevice",
            r#"{"hostId":"node-02","device":"BOOT_DEVICE_DISK","persistent":true,"efi":false,"reason":"boot order"}"#.to_owned(),
        ),
        (vec!["console", "list"], "Console/List", "{}".to_owned()),
        (
            vec!["console", "close", "--host-id", "node-02"],
            "Console/Close",
            r#"{"hostId":"node-02"}"#.to_owned(),
        ),
        (
            vec![
                "console", "probe", "--host-id", "node-02", "--reason", "after the BIOS change",
                "--wait-secs", "120", "--quiet-secs", "30",
            ],
            "Console/Probe",
            r#"{"hostId":"node-02","reason":"after the BIOS change","waitSecs":120,"quietSecs":30}"#
                .to_owned(),
        ),
        (
            vec![
                "host-groups", "move", "--node-id", "web-04", "--host-group", "web-canary",
                "--reason", "r",
            ],
            "HostGroups/Move",
            r#"{"nodeId":"web-04","hostGroup":"web-canary","reason":"r"}"#.to_owned(),
        ),
        (
            vec![
                "host-groups", "move", "--node-id", "web-04", "--host-group", "", "--reason", "r",
            ],
            "HostGroups/Move",
            r#"{"nodeId":"web-04","hostGroup":"","reason":"r"}"#.to_owned(),
        ),
        (
            vec![
                "host-groups", "put", "--request", &group, "--if-generation", "4", "--reason", "r",
            ],
            "HostGroups/Put",
            r#"{"hostGroup":{"name":"web","packages":["hostkeeper","tierstore"]},"ifGeneration":"4","reason":"r"}"#.to_owned(),
        ),
        (
            vec![
                "releases", "set-desired", "--service", "s", "--host-group", "web", "--request",
                &desired,
            ],
            "Releases/SetDesired",
            r#"{"desired":{"version":"1.2.3"},"service":"s","hostGroup":"web"}"#.to_owned(),
        ),
        (
            vec![
                "rollouts", "pause", "--service", "s", "--host-group", "web", "--reason", "r",
            ],
            "Rollouts/Pause",
            r#"{"service":"s","reason":"r","hostGroup":"web"}"#.to_owned(),
        ),
        (
            vec!["hosts", "retry", "--node-id", "node-4", "--service", "tierstore"],
            "Hosts/Retry",
            r#"{"nodeId":"node-4","service":"tierstore"}"#.to_owned(),
        ),
        (
            vec![
                "services", "set-repository", "--service", "tierstore-config-web",
                "--repository", "private", "--reason", "config moves",
            ],
            "Services/SetRepository",
            r#"{"service":"tierstore-config-web","repository":"private","reason":"config moves"}"#.to_owned(),
        ),
        (
            vec![
                "services", "set-repository", "--service", "tierstore", "--repository", "",
                "--reason", "back",
            ],
            "Services/SetRepository",
            r#"{"service":"tierstore","repository":"","reason":"back"}"#.to_owned(),
        ),
        (
            vec!["services", "set-repository", "--service", "tierstore", "--reason", "back"],
            "Services/SetRepository",
            r#"{"service":"tierstore","reason":"back"}"#.to_owned(),
        ),
        (
            vec![
                "ssh", "pin-host-keys", "--host-id", "node-4", "--fingerprints", &fingerprint_a,
                "--fingerprints", &fingerprint_b,
            ],
            "Ssh/PinHostKeys",
            format!("{{\"hostId\":\"node-4\",\"fingerprints\":[\"{fingerprint_a}\",\"{fingerprint_b}\"]}}"),
        ),
        (
            vec!["ssh", "pin-host-keys", "--host-id", "node-4", "--fingerprints", &fingerprint_a],
            "Ssh/PinHostKeys",
            format!("{{\"hostId\":\"node-4\",\"fingerprints\":[\"{fingerprint_a}\"]}}"),
        ),
        (
            vec!["ssh", "host-keys", "--host-id", "node-4"],
            "Ssh/HostKeys",
            r#"{"hostId":"node-4"}"#.to_owned(),
        ),
    ];
    for (words, method, body) in cases {
        let answer = format!("{{\"answer\":\"{method}\"}}");
        let (ran, target, sent) = post(&rig, &words, 200, &answer);
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
            (0, format!("{answer}\n").as_str(), ""),
            "{words:?}"
        );
        assert_eq!(target, format!("{PREFIX}{method}"), "{words:?}");
        assert_eq!(sent, body, "{words:?}");
    }
}

/// Proves: a password the coordinator answers once is printed on stdout exactly once, as it came.
#[test]
fn an_answered_password_is_printed_once() {
    let rig = Rig::new("password");
    let answer = "{\"user\":{\"name\":\"carol\"},\"password\":\"Abcdefghjkmnpqrstuvwxyz2\"}";
    let (ran, _, _) = post(&rig, &["users", "create", "--name", "carol"], 200, answer);
    assert_eq!(ran.code, 0);
    assert_eq!(ran.stdout.matches("Abcdefghjkmnpqrstuvwxyz2").count(), 1);
    assert_eq!(ran.stderr, "");
}

/// Proves: the refusals of these methods end as the coordinator says: a pin that is not what the
/// host reported and a retry with nothing to retry exit 2, an unknown repository 1, an
/// unimplemented call 1 with no `refused:` line (its reason is empty), and an operator with no
/// console access 3.
#[test]
fn the_refusals_of_these_methods_exit_with_their_codes() {
    let rig = Rig::new("method-refusals");
    for (words, status, code, reason, exit) in [
        (
            vec![
                "ssh",
                "pin-host-keys",
                "--host-id",
                "node-4",
                "--fingerprints",
                "SHA256:x",
            ],
            400,
            "FAILED_PRECONDITION",
            "not_reported",
            2,
        ),
        (
            vec![
                "hosts",
                "retry",
                "--node-id",
                "node-4",
                "--service",
                "tierstore",
            ],
            400,
            "FAILED_PRECONDITION",
            "nothing_to_retry",
            2,
        ),
        (
            vec![
                "services",
                "set-repository",
                "--service",
                "s",
                "--repository",
                "gone",
            ],
            404,
            "NOT_FOUND",
            "repository_unknown",
            1,
        ),
        (vec!["host-groups", "list"], 501, "UNIMPLEMENTED", "", 1),
        (
            vec![
                "console",
                "open",
                "--host-id",
                "node-02",
                "--reason",
                "boot order",
            ],
            403,
            "PERMISSION_DENIED",
            "no_console_access",
            3,
        ),
    ] {
        let body = format!("{{\"code\":\"{code}\",\"reason\":\"{reason}\"}}");
        let (ran, _, _) = post(&rig, &words, status, &body);
        let mut want = format!("hk: the coordinator answered {code}\n");
        if !reason.is_empty() {
            want.push_str(&format!("refused: {reason}\n"));
        }
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr),
            (exit, "", want),
            "{words:?}"
        );
    }
}

/// Proves: an action or a device the proto does not name is refused before anything is sent,
/// naming the spellings it takes.
#[test]
fn an_action_or_a_device_the_proto_does_not_name_is_refused_before_anything_is_sent() {
    let rig = Rig::new("console-enums");
    let server = Server::canned(&rig.pki, 200, "{}");
    for (words, why) in [
        (
            vec!["console", "power", "--host-id", "n", "--action", "reboot"],
            "--action takes one of: status, on, off, soft, cycle, reset",
        ),
        (
            vec![
                "console",
                "power",
                "--host-id",
                "n",
                "--action",
                "unspecified",
            ],
            "--action takes one of: status, on, off, soft, cycle, reset",
        ),
        (
            vec![
                "console",
                "set-boot-device",
                "--host-id",
                "n",
                "--device",
                "cdrom",
            ],
            "--device takes one of: pxe, disk, bios",
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
