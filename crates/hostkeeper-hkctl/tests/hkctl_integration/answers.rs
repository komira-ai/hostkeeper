//! What a script does with an answer: calls through the real gateway, the answer on stdout or in
//! `--response-file`, the exit code of every code a gateway error body names, the `refused:`
//! line and the reasons it drops, and an answer that is not the gateway's.

use crate::gateway;
use crate::rig::{Rig, Script, TOKEN, args};

/// Every code of the gateway's error body, the HTTP status the gateway answers it with
/// (`docs/gateway.md`), and the exit code hkctl gives it (`docs/design/two-programs.md` 2.4).
const CODES: [(&str, u16, i32); 16] = [
    ("CANCELLED", 499, 1),
    ("UNKNOWN", 500, 1),
    ("INVALID_ARGUMENT", 400, 2),
    ("DEADLINE_EXCEEDED", 504, 1),
    ("NOT_FOUND", 404, 1),
    ("ALREADY_EXISTS", 409, 1),
    ("PERMISSION_DENIED", 403, 3),
    ("RESOURCE_EXHAUSTED", 429, 1),
    ("FAILED_PRECONDITION", 400, 2),
    ("ABORTED", 409, 1),
    ("OUT_OF_RANGE", 400, 1),
    ("UNIMPLEMENTED", 501, 1),
    ("INTERNAL", 500, 1),
    ("UNAVAILABLE", 503, 1),
    ("DATA_LOSS", 500, 1),
    ("UNAUTHENTICATED", 401, 3),
];

/// `words` after the global flags of a call to `port`.
fn call(rig: &Rig, port: u16, words: &[&str]) -> Vec<String> {
    let mut all = rig.globals(port);
    all.extend(args(words));
    all
}

/// Catches an answer printed other than as the gateway sent it, a request the gateway cannot
/// read (the real router reads it), an answer written to stdout when `--response-file` names a
/// file, and an unwritable response file passing as success.
#[test]
fn a_call_through_the_real_gateway_prints_its_answer_or_writes_the_response_file() {
    let rig = Rig::new("real-gateway");
    let gateway = gateway::start(&rig.path(""));
    let request = rig.put("set.json", b"{\"desired\":{\"version\":\"1.2.3\"}}", 0o644);
    let ran = rig.run(
        Script::Real,
        &call(
            &rig,
            gateway.port,
            &[
                "releases",
                "set-desired",
                "--service",
                "kbf",
                "--request",
                &request,
            ],
        ),
        b"",
        &[],
    );
    let seen = gateway.seen();
    assert_eq!(seen.len(), 1, "{seen:#?}");
    assert_eq!(
        seen[0].target(),
        "/hostkeeper.operator.v1.Releases/SetDesired"
    );
    assert_eq!(
        String::from_utf8_lossy(&seen[0].body),
        "{\"desired\":{\"version\":\"1.2.3\"},\"service\":\"kbf\"}"
    );
    assert_eq!(
        seen[0].header("authorization"),
        Some(format!("Bearer {TOKEN}").as_str())
    );
    assert_eq!(
        seen[0].status,
        200,
        "{}",
        String::from_utf8_lossy(&seen[0].answer)
    );
    assert_eq!(
        (ran.code, ran.stdout.clone(), ran.stderr.as_str()),
        (
            0,
            format!("{}\n", String::from_utf8_lossy(&seen[0].answer)),
            ""
        )
    );

    // The same answer into --response-file, and nothing on stdout.
    let response = rig.path("response.json").display().to_string();
    let ran = rig.run(
        Script::Real,
        &call(
            &rig,
            gateway.port,
            &[
                "--response-file",
                &response,
                "releases",
                "converge",
                "--service",
                "kbf",
            ],
        ),
        b"",
        &[],
    );
    let seen = gateway.seen();
    assert_eq!(
        seen[1].target(),
        "/hostkeeper.operator.v1.Releases/Converge"
    );
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (0, "", "")
    );
    assert_eq!(
        std::fs::read_to_string(&response).expect("the response file"),
        format!("{}\n", String::from_utf8_lossy(&seen[1].answer))
    );

    // A response file that cannot be written: exit 1, said once.
    let directory = rig.path("").display().to_string();
    let ran = rig.run(
        Script::Real,
        &call(
            &rig,
            gateway.port,
            &[
                "--response-file",
                &directory,
                "releases",
                "converge",
                "--service",
                "kbf",
            ],
        ),
        b"",
        &[],
    );
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr),
        (
            1,
            "",
            format!("hkctl: cannot write --response-file {directory}\n")
        )
    );
}

/// Catches a refusal of the real gateway read wrongly: a refused caller and a refused request
/// each exit with their code, and print the gateway's reason on a `refused:` line.
#[test]
fn a_refusal_of_the_real_gateway_exits_with_its_code_and_says_its_reason() {
    let rig = Rig::new("real-refusals");
    let gateway = gateway::start(&rig.path(""));
    // A token the coordinator does not hold.
    rig.put("token", b"a-token-no-file-names\n", 0o600);
    let ran = rig.run(
        Script::Real,
        &call(&rig, gateway.port, &["hosts", "list"]),
        b"",
        &[],
    );
    let seen = gateway.seen();
    let answer: serde_json::Value = serde_json::from_slice(&seen[0].answer).expect("a refusal");
    assert_eq!(answer["code"], "UNAUTHENTICATED");
    let reason = answer["reason"].as_str().expect("a reason");
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr),
        (
            3,
            "",
            format!("hkctl: the coordinator answered UNAUTHENTICATED\nrefused: {reason}\n")
        )
    );

    // A request the coordinator refuses: a body that is not the method's request.
    rig.put("token", format!("{TOKEN}\n").as_bytes(), 0o600);
    let request = rig.put("bad.json", b"{\"noSuchField\":1}", 0o644);
    let ran = rig.run(
        Script::Real,
        &call(
            &rig,
            gateway.port,
            &["hosts", "list", "--request", &request],
        ),
        b"",
        &[],
    );
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (
            2,
            "",
            "hkctl: the coordinator answered INVALID_ARGUMENT\nrefused: invalid_request\n"
        )
    );
}

/// Catches a gateway code mapped to the wrong exit (a refused request read as a failure, a
/// refused caller as a refused request, a failure as success), and a reason printed although it
/// is outside `[a-z0-9_]{1,64}` (a word with a capital or a space, an escape, 65 characters,
/// nothing).
#[test]
fn every_gateway_code_exits_with_its_code_and_a_reason_outside_the_grammar_is_dropped() {
    let rig = Rig::new("every-code");
    for (code, http, exit) in CODES {
        rig.reset();
        rig.reply(
            http,
            &format!("{{\"code\":\"{code}\",\"reason\":\"why_{exit}\"}}"),
        );
        let ran = rig.run(
            Script::Fixture,
            &call(&rig, 1, &["gadgets", "list"]),
            b"",
            &[],
        );
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr),
            (
                exit,
                "",
                format!("hkctl: the coordinator answered {code}\nrefused: why_{exit}\n")
            ),
            "{code}"
        );
    }
    let long = "a".repeat(65);
    let edge = "b".repeat(64);
    for (reason, printed) in [
        ("Bad Thing", false),
        ("bad thing", false),
        ("x\\\"y", false),
        ("x\\u001b[2J", false),
        ("", false),
        (long.as_str(), false),
        (edge.as_str(), true),
        ("below_min_version_2", true),
    ] {
        rig.reset();
        rig.reply(
            400,
            &format!("{{\"code\":\"FAILED_PRECONDITION\",\"reason\":\"{reason}\"}}"),
        );
        let ran = rig.run(
            Script::Fixture,
            &call(&rig, 1, &["gadgets", "list"]),
            b"",
            &[],
        );
        let mut want = "hkctl: the coordinator answered FAILED_PRECONDITION\n".to_owned();
        if printed {
            want.push_str(&format!("refused: {reason}\n"));
        }
        assert_eq!((ran.code, ran.stderr), (2, want), "{reason:?}");
    }
}

/// Catches an answer that is not the gateway's read as a refusal or a success (an HTML page, an
/// error body that says `OK` or a name that is no code, one without a reason), and no answer at
/// all (curl failed) read as anything but exit 1.
#[test]
fn an_answer_that_is_not_the_gateways_and_no_answer_exit_1() {
    let rig = Rig::new("not-the-gateway");
    for (http, body) in [
        (502, "<html>bad gateway</html>"),
        (500, "{\"code\":\"OK\",\"reason\":\"x\"}"),
        (500, "{\"code\":\"NOPE\",\"reason\":\"x\"}"),
        (500, "{\"code\":\"INTERNAL\"}"),
        (500, "{\"reason\":\"x\",\"code\":\"INTERNAL\"}"),
    ] {
        rig.reset();
        rig.reply(http, body);
        let ran = rig.run(
            Script::Fixture,
            &call(&rig, 1, &["gadgets", "list"]),
            b"",
            &[],
        );
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr),
            (
                1,
                "",
                format!("hkctl: the answer (HTTP {http}) is not the gateway's\n")
            ),
            "{body}"
        );
    }
    rig.reset();
    rig.fail();
    let ran = rig.run(
        Script::Fixture,
        &call(&rig, 1, &["gadgets", "list"]),
        b"",
        &[],
    );
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (
            1,
            "",
            "curl: (7) Failed to connect\nhkctl: no answer from https://127.0.0.1:1 (curl exited 7)\n"
        )
    );
}
