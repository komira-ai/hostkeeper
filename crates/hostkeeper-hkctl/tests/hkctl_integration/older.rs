//! A call refused `invalid_request` makes one more call, with no token, to the coordinator's
//! `GET /v1/version?at_least=<this hkctl's release>`, and says `coordinator_older` when the
//! coordinator is older than the hkctl (a coordinator before a field the request carries refuses
//! it with a bare `invalid_request`).

use crate::gateway;
use crate::rig::{Rig, STAMP, Script, TOKEN, args};

/// What a refusal `invalid_request` of the gateway looks like.
const INVALID: &str = "{\"code\":\"INVALID_ARGUMENT\",\"reason\":\"invalid_request\"}";

/// The first line of every run's stderr: the code of the refusal.
const ANSWERED: &str = "hkctl: the coordinator answered INVALID_ARGUMENT\n";

/// The arguments of a call of the fixture's `gadgets list` to port 1.
fn gadgets(rig: &Rig) -> Vec<String> {
    let mut all = rig.globals(1);
    all.extend(args(&["gadgets", "list"]));
    all
}

/// The arguments of the probe the script must make for the fixture's stamp, port 1 and a CA file.
fn probe_argv(rig: &Rig) -> Vec<String> {
    let mut want = args(&[
        "-q",
        "--silent",
        "--show-error",
        "--proto",
        "=https",
        "--tlsv1.2",
        "--max-redirs",
        "0",
        "--globoff",
        "--cacert",
    ]);
    want.push(rig.path("ca.pem").display().to_string());
    want.extend(args(&[
        "--connect-timeout",
        "30",
        "--max-time",
        "30",
        "--write-out",
        "\\n%{http_code}",
        "--url",
        "https://127.0.0.1:1/v1/version?at_least=1.2.3",
    ]));
    want
}

/// A rig whose first call is refused `invalid_request` and whose second, the probe, is answered
/// `status` and `body`.
fn refused_then(name: &str, status: u16, body: &str) -> Rig {
    let rig = Rig::new(name);
    rig.reply_nth(1, 400, INVALID);
    rig.reply_nth(2, status, body);
    rig
}

/// Catches an older coordinator not named: the probe answered `409` with its version gives the
/// two lines on stderr, `coordinator_older` as the reason and exit 2; the probe is one GET to the
/// version route with this hkctl's release (the stamp's number, not its commit) and no header,
/// no body, no pipe and no token; the program sequence is find, the call, the probe.
#[test]
fn a_refusal_invalid_request_probed_409_says_the_coordinator_is_older() {
    let rig = refused_then(
        "older-409",
        409,
        "{\"error\":\"coordinator_older\",\"version\":\"1.0.0\",\"commit\":\"0123456789ab\"}",
    );
    assert_eq!(STAMP, "1.2.3+0123456789ab");
    let ran = rig.run(Script::Fixture, &gadgets(&rig), b"", &[]);
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr),
        (
            2,
            "",
            format!(
                "{ANSWERED}hkctl: the coordinator is older than this hkctl (1.2.3): it runs 1.0.0; update the coordinator, then call again\nrefused: coordinator_older\n"
            )
        )
    );
    let calls = rig.calls();
    let programs: Vec<&str> = calls.iter().map(|c| c.program.as_str()).collect();
    assert_eq!(programs, ["find", "curl", "curl"]);
    let probe = &calls[2];
    assert_eq!(probe.argv, probe_argv(&rig));
    assert!(probe.reads.is_empty(), "{:?}", probe.reads);
    for entry in probe.argv.iter().chain(&probe.environ) {
        assert!(!entry.contains(TOKEN), "{entry:?}");
        assert!(!entry.contains("Authorization"), "{entry:?}");
    }
    // The call itself did carry the token, through its pipe.
    assert_eq!(
        calls[1].read_after("--header", 0).as_deref(),
        Some(format!("Authorization: Bearer {TOKEN}\n").as_str())
    );
}

/// Catches a coordinator before the version route taken for current: the probe answered `404`
/// says it does not give its version, and the reason is `coordinator_older`.
#[test]
fn a_refusal_invalid_request_probed_404_says_the_coordinator_does_not_say_its_version() {
    let rig = refused_then("older-404", 404, "{\"error\":\"not_found\"}");
    let ran = rig.run(Script::Fixture, &gadgets(&rig), b"", &[]);
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr),
        (
            2,
            "",
            format!(
                "{ANSWERED}hkctl: the coordinator is older than this hkctl (1.2.3): it does not say its version; update the coordinator, then call again\nrefused: coordinator_older\n"
            )
        )
    );
    assert_eq!(rig.curls().len(), 2);
}

/// Catches a coordinator that is not older blamed: the probe answered `200` leaves the reason
/// `invalid_request`, with one line saying which version it runs.
#[test]
fn a_refusal_invalid_request_probed_200_stays_invalid_request() {
    let rig = refused_then(
        "older-200",
        200,
        "{\"version\":\"1.2.4\",\"commit\":\"0123456789ab\"}",
    );
    let ran = rig.run(Script::Fixture, &gadgets(&rig), b"", &[]);
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr),
        (
            2,
            "",
            format!(
                "{ANSWERED}hkctl: the coordinator runs 1.2.4, not older than this hkctl (1.2.3)\nrefused: invalid_request\n"
            )
        )
    );
    assert_eq!(rig.curls().len(), 2);
}

/// Catches a probe that changes the refusal when it fails or is not understood: curl failing,
/// a `409` with no version (or one that is not visible text), a `200` with no version, another
/// status, and a body that is not JSON each leave exactly the `invalid_request` refusal.
#[test]
fn a_probe_that_fails_or_is_not_understood_leaves_invalid_request() {
    let plain = format!("{ANSWERED}refused: invalid_request\n");
    let rig = Rig::new("older-fails");
    rig.reply_nth(1, 400, INVALID);
    rig.fail_nth(2);
    let ran = rig.run(Script::Fixture, &gadgets(&rig), b"", &[]);
    assert_eq!((ran.code, ran.stderr.as_str()), (2, plain.as_str()));
    assert_eq!(rig.curls().len(), 2);

    for (status, body) in [
        (409, "{\"error\":\"coordinator_older\"}"),
        (409, "{\"version\":\"\"}"),
        (409, "{\"version\":\"a b\"}"),
        (409, "{\"version\":\"a\\\\b\"}"),
        (409, "<html>conflict</html>"),
        (200, "<html>fine</html>"),
        (400, "{\"error\":\"bad_request\"}"),
        (500, "{\"version\":\"1.0.0\"}"),
        (502, ""),
    ] {
        rig.reset();
        rig.reply_nth(1, 400, INVALID);
        rig.reply_nth(2, status, body);
        let ran = rig.run(Script::Fixture, &gadgets(&rig), b"", &[]);
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
            (2, "", plain.as_str()),
            "{status} {body}"
        );
        assert_eq!(rig.curls().len(), 2, "{status} {body}");
    }
}

/// Catches a probe after a refusal of another reason: only the reason `invalid_request` makes
/// one (whatever the code or method, as hkctl names no method), so the program sequence of any
/// other refusal is find and the one call.
#[test]
fn a_refusal_other_than_invalid_request_makes_no_probe() {
    let rig = Rig::new("older-none");
    for (code, http, exit, reason) in [
        ("INVALID_ARGUMENT", 400, 2, "invalid_window"),
        ("INVALID_ARGUMENT", 400, 2, "invalid_request_too"),
        ("FAILED_PRECONDITION", 400, 2, "invalid_request_"),
        ("PERMISSION_DENIED", 403, 3, "denied"),
    ] {
        rig.reset();
        rig.reply(
            http,
            &format!("{{\"code\":\"{code}\",\"reason\":\"{reason}\"}}"),
        );
        let ran = rig.run(Script::Fixture, &gadgets(&rig), b"", &[]);
        assert_eq!(
            (ran.code, ran.stderr),
            (
                exit,
                format!("hkctl: the coordinator answered {code}\nrefused: {reason}\n")
            ),
            "{reason}"
        );
        let programs: Vec<String> = rig.calls().into_iter().map(|c| c.program).collect();
        assert_eq!(programs, ["find", "curl"], "{reason}");
    }
    // The reason alone decides: `invalid_request` under another code is probed too, and the exit
    // status stays that of the code.
    rig.reset();
    rig.reply_nth(
        1,
        500,
        "{\"code\":\"INTERNAL\",\"reason\":\"invalid_request\"}",
    );
    rig.reply_nth(2, 404, "{}");
    let ran = rig.run(Script::Fixture, &gadgets(&rig), b"", &[]);
    assert_eq!(
        (ran.code, ran.stderr),
        (
            1,
            "hkctl: the coordinator answered INTERNAL\nhkctl: the coordinator is older than this hkctl (1.2.3): it does not say its version; update the coordinator, then call again\nrefused: coordinator_older\n".to_owned()
        )
    );
}

/// Catches the probe not reaching the real coordinator's version route, or its answer misread: a
/// `set-desired` with a field the coordinator does not know gets the real `invalid_request`; with
/// a script stamped newer than the coordinator the real route answers `409` and the reason is
/// `coordinator_older`, with the script stamped older it answers `200` and the reason stays
/// `invalid_request`.
#[test]
fn a_set_desired_with_an_unknown_field_is_probed_against_the_real_version_route() {
    let rig = Rig::new("older-real");
    let gateway = gateway::start(&rig.path(""));
    let request = rig.put(
        "set.json",
        b"{\"desired\":{\"version\":\"1.2.3\"},\"fieldFromTheFuture\":1}",
        0o644,
    );
    let mut call = rig.globals(gateway.port);
    call.extend(args(&[
        "releases",
        "set-desired",
        "--service",
        "kbf",
        "--request",
        &request,
    ]));

    // Stamped 1.2.3, newer than this workspace's coordinator: older.
    let ran = rig.run(Script::Real, &call, b"", &[]);
    let seen = gateway.seen();
    assert_eq!(seen.len(), 2, "{seen:#?}");
    assert_eq!(
        seen[0].target(),
        "/hostkeeper.operator.v1.Releases/SetDesired"
    );
    assert_eq!(seen[0].status, 400);
    let refusal: serde_json::Value = serde_json::from_slice(&seen[0].answer).expect("a refusal");
    assert_eq!(
        (refusal["code"].as_str(), refusal["reason"].as_str()),
        (Some("INVALID_ARGUMENT"), Some("invalid_request"))
    );
    assert_eq!(seen[1].target(), "/v1/version?at_least=1.2.3");
    assert_eq!(seen[1].header("authorization"), None);
    assert_eq!(seen[1].status, 409);
    let said: serde_json::Value = serde_json::from_slice(&seen[1].answer).expect("a version");
    let version = said["version"].as_str().expect("a version").to_owned();
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr),
        (
            2,
            "",
            format!(
                "{ANSWERED}hkctl: the coordinator is older than this hkctl (1.2.3): it runs {version}; update the coordinator, then call again\nrefused: coordinator_older\n"
            )
        )
    );

    // The same script stamped 0.0.1, older than the coordinator: not older, so invalid_request.
    let older = hostkeeper_hkctl::script(hostkeeper_proto::ctl::COMMANDS, "0.0.1+aaaaaaaaaaaa")
        .expect("the real protos make a script");
    rig.put("real.hkctl", older.as_bytes(), 0o755);
    let ran = rig.run(Script::Real, &call, b"", &[]);
    let seen = gateway.seen();
    assert_eq!(seen.len(), 4, "{seen:#?}");
    assert_eq!(seen[3].target(), "/v1/version?at_least=0.0.1");
    assert_eq!(seen[3].status, 200);
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr),
        (
            2,
            "",
            format!(
                "{ANSWERED}hkctl: the coordinator runs {version}, not older than this hkctl (0.0.1)\nrefused: invalid_request\n"
            )
        )
    );
}
