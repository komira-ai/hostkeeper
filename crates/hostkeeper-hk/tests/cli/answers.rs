//! What `hk` does with an answer: calls through the real gateway, the answer on stdout or in
//! `--response-file`, the exit code of every code a gateway error body names, the `refused:`
//! line and the reasons it drops, and an answer that is not the gateway's.

use crate::gateway;
use crate::rig::{Rig, Server, TOKEN, call};

/// Every code of the gateway's error body, the HTTP status the gateway answers it with
/// (`docs/gateway.md`), and the exit code `hk` gives it.
const CODES: [(&str, u16, u8); 16] = [
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

/// Proves: an answer is printed as the gateway sent it plus one newline, the real router reads
/// the request `hk` makes (the body is the request file's object with the flag's field added), an
/// answer goes to the file `--response-file` names and not to stdout, and a response file that
/// cannot be written is exit 1.
#[test]
fn a_call_through_the_real_gateway_prints_its_answer_or_writes_the_response_file() {
    let rig = Rig::new("real-gateway");
    let gateway = gateway::start(&rig.pki, &rig.path(""));
    let request = rig.put("set.json", b"{\"desired\":{\"version\":\"1.2.3\"}}", 0o644);
    let ran = rig.hk(&call(
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
    ));
    let seen = gateway.seen();
    assert_eq!(seen.len(), 1, "{seen:#?}");
    assert_eq!(seen[0].method, "POST");
    assert_eq!(
        seen[0].target,
        "/hostkeeper.operator.v1.Releases/SetDesired"
    );
    assert_eq!(
        seen[0].text(),
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
    let ran = rig.hk(&call(
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
    ));
    let seen = gateway.seen();
    assert_eq!(seen[1].target, "/hostkeeper.operator.v1.Releases/Converge");
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
    let ran = rig.hk(&call(
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
    ));
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr),
        (
            1,
            "",
            format!("hk: cannot write --response-file {directory}\n")
        )
    );
}

/// Proves: a refused caller and a refused request of the real gateway each exit with their code
/// and print the gateway's reason on a `refused:` line; a refused request makes the probe of the
/// coordinator's version, which says the coordinator is older than the stamp 1.2.3.
#[test]
fn a_refusal_of_the_real_gateway_exits_with_its_code_and_says_its_reason() {
    let rig = Rig::new("real-refusals");
    let gateway = gateway::start(&rig.pki, &rig.path(""));
    // A token the coordinator does not hold.
    rig.put("token", b"a-token-no-file-names\n", 0o600);
    let ran = rig.hk(&call(&rig, gateway.port, &["hosts", "list"]));
    let seen = gateway.seen();
    let answer: serde_json::Value = serde_json::from_slice(&seen[0].answer).expect("a refusal");
    assert_eq!(answer["code"], "UNAUTHENTICATED");
    let reason = answer["reason"].as_str().expect("a reason");
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr),
        (
            3,
            "",
            format!("hk: the coordinator answered UNAUTHENTICATED\nrefused: {reason}\n")
        )
    );

    // A request the coordinator refuses: a body that is not the method's request.
    rig.put("token", format!("{TOKEN}\n").as_bytes(), 0o600);
    let request = rig.put("bad.json", b"{\"noSuchField\":1}", 0o644);
    let ran = rig.hk(&call(
        &rig,
        gateway.port,
        &["hosts", "list", "--request", &request],
    ));
    let seen = gateway.seen();
    let probe = seen.last().expect("the probe");
    assert_eq!(probe.method, "GET");
    assert_eq!(probe.target, "/v1/version?at_least=1.2.3");
    let said: serde_json::Value = serde_json::from_slice(&probe.answer).expect("a version");
    assert_eq!(said["error"], "coordinator_older");
    let version = said["version"].as_str().expect("a version");
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr),
        (
            2,
            "",
            format!(
                "hk: the coordinator answered INVALID_ARGUMENT\nhk: the coordinator is older than this hk (1.2.3): it runs {version}; update the coordinator, then call again\nrefused: coordinator_older\n"
            )
        )
    );
}

/// Proves: each gateway code exits with its code (a refused request 2, a refused caller 3, the
/// rest 1) whatever the HTTP status, and a reason outside `[a-z0-9_]{1,64}` (a capital, a space,
/// an escape, 65 characters, nothing) is not printed.
#[test]
fn every_gateway_code_exits_with_its_code_and_a_reason_outside_the_grammar_is_dropped() {
    let rig = Rig::new("every-code");
    for (code, http, exit) in CODES {
        let server = Server::canned(
            &rig.pki,
            http,
            &format!("{{\"code\":\"{code}\",\"reason\":\"why_{exit}\"}}"),
        );
        let ran = rig.fx(&call(&rig, server.port, &["gadgets", "list"]));
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr),
            (
                exit,
                "",
                format!("hk: the coordinator answered {code}\nrefused: why_{exit}\n")
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
        let server = Server::canned(
            &rig.pki,
            400,
            &format!("{{\"code\":\"FAILED_PRECONDITION\",\"reason\":\"{reason}\"}}"),
        );
        let ran = rig.fx(&call(&rig, server.port, &["gadgets", "list"]));
        let mut want = "hk: the coordinator answered FAILED_PRECONDITION\n".to_owned();
        if printed {
            want.push_str(&format!("refused: {reason}\n"));
        }
        assert_eq!((ran.code, ran.stderr), (2, want), "{reason:?}");
    }
}

/// Proves: an answer that is not the gateway's (an HTML page, an error body that says `OK` or a
/// name that is no code, one without a reason, one with its keys the other way round, a status
/// other than 200 with a success body) is exit 1 with that said and nothing on stdout.
#[test]
fn an_answer_that_is_not_the_gateways_is_exit_1() {
    let rig = Rig::new("not-the-gateway");
    for (http, body) in [
        (502, "<html>bad gateway</html>"),
        (500, "{\"code\":\"OK\",\"reason\":\"x\"}"),
        (500, "{\"code\":\"NOPE\",\"reason\":\"x\"}"),
        (500, "{\"code\":\"INTERNAL\"}"),
        (500, "{\"code\":\"\",\"reason\":\"x\"}"),
        (500, "{\"reason\":\"x\",\"code\":\"INTERNAL\"}"),
        (500, "{\"code\":\"INTERNAL\",\"reason\":\"x\"}\n"),
        (201, "{}"),
        (204, ""),
    ] {
        let server = Server::canned(&rig.pki, http, body);
        let ran = rig.fx(&call(&rig, server.port, &["gadgets", "list"]));
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr),
            (
                1,
                "",
                format!("hk: the answer (HTTP {http}) is not the gateway's\n")
            ),
            "{body}"
        );
    }
}

/// Proves: a gateway answer that ends in a newline is printed with one more, and a reason may
/// hold a `"}` of its own, which does not end the body early.
#[test]
fn an_answer_is_printed_whole_with_one_newline_added() {
    let rig = Rig::new("whole-answer");
    let server = Server::canned(&rig.pki, 200, "{\"a\":1}\n");
    let ran = rig.fx(&call(&rig, server.port, &["gadgets", "list"]));
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (0, "{\"a\":1}\n\n", "")
    );
    let server = Server::canned(
        &rig.pki,
        500,
        "{\"code\":\"INTERNAL\",\"reason\":\"a\\\"}x\"}",
    );
    let ran = rig.fx(&call(&rig, server.port, &["gadgets", "list"]));
    assert_eq!(
        (ran.code, ran.stderr.as_str()),
        (1, "hk: the coordinator answered INTERNAL\n")
    );
}

/// A standard output that refuses every write, as a closed pipe does.
struct Closed;

impl std::io::Write for Closed {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::BrokenPipe.into())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::ErrorKind::BrokenPipe.into())
    }
}

/// Proves: an answer that cannot be written to standard output (a closed pipe) is a failure
/// said on stderr, exit code 1, and not a success with nothing delivered.
#[test]
fn an_answer_that_cannot_be_written_is_a_failure() {
    let rig = Rig::new("answer-closed");
    let server = Server::canned(&rig.pki, 200, "{\"hosts\":[]}");
    let all = call(&rig, server.port, &["hosts", "list"]);
    let os: Vec<std::ffi::OsString> = all.iter().map(std::ffi::OsString::from).collect();
    let mut stdin = std::io::empty();
    let mut stderr = Vec::new();
    let mut stdout = Closed;
    let web_roots = Vec::new;
    let recorder = crate::rig::Recorder::default();
    let mut world = hostkeeper_hk::World {
        table: hostkeeper_proto::ctl::COMMANDS,
        stamp: crate::rig::STAMP,
        stdin: &mut stdin,
        stdout: &mut stdout,
        stderr: &mut stderr,
        home: None,
        oidc_url: None,
        oidc_token: None,
        web_roots: &web_roots,
        ssh: &recorder,
        mac: &hostkeeper_hk::xcode_push::Mac::system(),
    };
    assert_eq!(hostkeeper_hk::run(&mut world, &os), 1);
    assert_eq!(stderr, b"hk: cannot write the answer\n");
}
