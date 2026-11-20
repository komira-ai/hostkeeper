//! A call refused `invalid_request` makes one more call, with no token, to the coordinator's
//! `GET /v1/version?at_least=<this hk's release>`, and says `coordinator_older` when the
//! coordinator is older than `hk` (a coordinator before a field the request carries refuses it
//! with a bare `invalid_request`).

use crate::gateway;
use crate::rig::{Opts, Reply, Rig, STAMP, Server, TOKEN, args, call};

/// What a refusal `invalid_request` of the gateway looks like.
const INVALID: &str = "{\"code\":\"INVALID_ARGUMENT\",\"reason\":\"invalid_request\"}";

/// The first line of every run's stderr: the code of the refusal.
const ANSWERED: &str = "hk: the coordinator answered INVALID_ARGUMENT\n";

/// A server whose first answer is `invalid_request` and whose second, the probe's, is `probe`.
fn refused_then(rig: &Rig, probe: Reply) -> Server {
    Server::sequence(&rig.pki, vec![Reply::new(400, INVALID), probe])
}

/// Proves: an older coordinator is named: the probe answered `409` with its version gives the two
/// lines on stderr, `coordinator_older` as the reason and exit 2; the probe is one GET to the
/// version route with this release (the stamp's number, not its commit) and no token, no body.
#[test]
fn a_refusal_invalid_request_probed_409_says_the_coordinator_is_older() {
    let rig = Rig::new("older-409");
    let server = refused_then(
        &rig,
        Reply::new(
            409,
            "{\"error\":\"coordinator_older\",\"version\":\"1.0.0\",\"commit\":\"0123456789ab\"}",
        ),
    );
    assert_eq!(STAMP, "1.2.3+0123456789ab");
    let ran = rig.fx(&call(&rig, server.port, &["gadgets", "list"]));
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr),
        (
            2,
            "",
            format!(
                "{ANSWERED}hk: the coordinator is older than this hk (1.2.3): it runs 1.0.0; update the coordinator, then call again\nrefused: coordinator_older\n"
            )
        )
    );
    let seen = server.seen();
    assert_eq!(seen.len(), 2);
    let probe = &seen[1];
    assert_eq!(
        (probe.method.as_str(), probe.target.as_str()),
        ("GET", "/v1/version?at_least=1.2.3")
    );
    assert_eq!(probe.header("authorization"), None);
    assert_eq!(probe.header("content-type"), None);
    assert!(probe.body.is_empty());
    // The call itself did carry the token.
    assert_eq!(
        seen[0].header("authorization"),
        Some(format!("Bearer {TOKEN}").as_str())
    );
}

/// Proves: a coordinator before the version route is taken as older: the probe answered `404`
/// says it does not give its version, and the reason is `coordinator_older`.
#[test]
fn a_refusal_invalid_request_probed_404_says_the_coordinator_does_not_say_its_version() {
    let rig = Rig::new("older-404");
    let server = refused_then(&rig, Reply::new(404, "{\"error\":\"not_found\"}"));
    let ran = rig.fx(&call(&rig, server.port, &["gadgets", "list"]));
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr),
        (
            2,
            "",
            format!(
                "{ANSWERED}hk: the coordinator is older than this hk (1.2.3): it does not say its version; update the coordinator, then call again\nrefused: coordinator_older\n"
            )
        )
    );
    assert_eq!(server.seen().len(), 2);
}

/// Proves: a coordinator that is not older is not blamed: the probe answered `200` leaves the
/// reason `invalid_request`, with one line saying which version it runs.
#[test]
fn a_refusal_invalid_request_probed_200_stays_invalid_request() {
    let rig = Rig::new("older-200");
    let server = refused_then(
        &rig,
        Reply::new(200, "{\"version\":\"1.2.4\",\"commit\":\"0123456789ab\"}"),
    );
    let ran = rig.fx(&call(&rig, server.port, &["gadgets", "list"]));
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr),
        (
            2,
            "",
            format!(
                "{ANSWERED}hk: the coordinator runs 1.2.4, not older than this hk (1.2.3)\nrefused: invalid_request\n"
            )
        )
    );
    assert_eq!(server.seen().len(), 2);
}

/// Proves: a probe that fails or is not understood changes nothing: a hang-up, a `409` with no
/// version (or one that is not visible text), a `200` with no version, another status, and a body
/// that is not JSON each leave exactly the `invalid_request` refusal.
#[test]
fn a_probe_that_fails_or_is_not_understood_leaves_invalid_request() {
    let plain = format!("{ANSWERED}refused: invalid_request\n");
    let rig = Rig::new("older-fails");
    let server = refused_then(&rig, Reply::hang_up());
    let ran = rig.fx(&call(&rig, server.port, &["gadgets", "list"]));
    assert_eq!((ran.code, ran.stderr.as_str()), (2, plain.as_str()));
    assert_eq!(server.seen().len(), 2);

    for (status, body) in [
        (409, "{\"error\":\"coordinator_older\"}"),
        (409, "{\"version\":\"\"}"),
        (409, "{\"version\":\"a b\"}"),
        (409, "{\"version\":\"a\\\\b\"}"),
        (409, "{\"version\":\"unterminated"),
        (409, "<html>conflict</html>"),
        (200, "<html>fine</html>"),
        (400, "{\"error\":\"bad_request\"}"),
        (500, "{\"version\":\"1.0.0\"}"),
        (502, ""),
    ] {
        let server = refused_then(&rig, Reply::new(status, body));
        let ran = rig.fx(&call(&rig, server.port, &["gadgets", "list"]));
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
            (2, "", plain.as_str()),
            "{status} {body}"
        );
        assert_eq!(server.seen().len(), 2, "{status} {body}");
    }
}

/// Proves: only the reason `invalid_request` makes a probe (whatever the code or method): any
/// other refusal is the one call; and the reason alone decides, the exit status staying the
/// code's.
#[test]
fn a_refusal_other_than_invalid_request_makes_no_probe() {
    let rig = Rig::new("older-none");
    for (code, http, exit, reason) in [
        ("INVALID_ARGUMENT", 400, 2, "invalid_window"),
        ("INVALID_ARGUMENT", 400, 2, "invalid_request_too"),
        ("FAILED_PRECONDITION", 400, 2, "invalid_request_"),
        ("PERMISSION_DENIED", 403, 3, "denied"),
    ] {
        let server = Server::canned(
            &rig.pki,
            http,
            &format!("{{\"code\":\"{code}\",\"reason\":\"{reason}\"}}"),
        );
        let ran = rig.fx(&call(&rig, server.port, &["gadgets", "list"]));
        assert_eq!(
            (ran.code, ran.stderr),
            (
                exit,
                format!("hk: the coordinator answered {code}\nrefused: {reason}\n")
            ),
            "{reason}"
        );
        assert_eq!(server.seen().len(), 1, "{reason}");
    }
    let server = Server::sequence(
        &rig.pki,
        vec![
            Reply::new(
                500,
                "{\"code\":\"INTERNAL\",\"reason\":\"invalid_request\"}",
            ),
            Reply::new(404, "{}"),
        ],
    );
    let ran = rig.fx(&call(&rig, server.port, &["gadgets", "list"]));
    assert_eq!(
        (ran.code, ran.stderr),
        (
            1,
            "hk: the coordinator answered INTERNAL\nhk: the coordinator is older than this hk (1.2.3): it does not say its version; update the coordinator, then call again\nrefused: coordinator_older\n".to_owned()
        )
    );
}

/// Proves: the probe reaches the real coordinator's version route and its answer is read: a
/// `set-desired` with a field the coordinator does not know gets the real `invalid_request`; with
/// the stamp newer than the coordinator the real route answers `409` and the reason is
/// `coordinator_older`, with the stamp older it answers `200` and the reason stays
/// `invalid_request`.
#[test]
fn a_set_desired_with_an_unknown_field_is_probed_against_the_real_version_route() {
    let rig = Rig::new("older-real");
    let gateway = gateway::start(&rig.pki, &rig.path(""));
    let request = rig.put(
        "set.json",
        b"{\"desired\":{\"version\":\"1.2.3\"},\"fieldFromTheFuture\":1}",
        0o644,
    );
    let mut words = rig.globals(gateway.port);
    words.extend(args(&[
        "releases",
        "set-desired",
        "--service",
        "kbf",
        "--request",
        &request,
    ]));

    // Stamped 1.2.3, newer than this workspace's coordinator: older.
    let ran = rig.hk(&words);
    let seen = gateway.seen();
    assert_eq!(seen.len(), 2, "{seen:#?}");
    assert_eq!(
        seen[0].target,
        "/hostkeeper.operator.v1.Releases/SetDesired"
    );
    assert_eq!(seen[0].status, 400);
    let refusal: serde_json::Value = serde_json::from_slice(&seen[0].answer).expect("a refusal");
    assert_eq!(
        (refusal["code"].as_str(), refusal["reason"].as_str()),
        (Some("INVALID_ARGUMENT"), Some("invalid_request"))
    );
    assert_eq!(seen[1].target, "/v1/version?at_least=1.2.3");
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
                "{ANSWERED}hk: the coordinator is older than this hk (1.2.3): it runs {version}; update the coordinator, then call again\nrefused: coordinator_older\n"
            )
        )
    );

    // The same stamped 0.0.1, older than the coordinator: not older, so invalid_request.
    let ran = rig.run(Opts::real().stamped("0.0.1+aaaaaaaaaaaa"), &words);
    let seen = gateway.seen();
    assert_eq!(seen.len(), 4, "{seen:#?}");
    assert_eq!(seen[3].target, "/v1/version?at_least=0.0.1");
    assert_eq!(seen[3].status, 200);
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr),
        (
            2,
            "",
            format!(
                "{ANSWERED}hk: the coordinator runs {version}, not older than this hk (0.0.1)\nrefused: invalid_request\n"
            )
        )
    );

    // A stamp with no version leaves the refusal as it was, with no probe.
    let ran = rig.run(Opts::real().stamped("+aaaaaaaaaaaa"), &words);
    assert_eq!(gateway.seen().len(), 5);
    assert_eq!(
        (ran.code, ran.stderr),
        (2, format!("{ANSWERED}refused: invalid_request\n"))
    );
}
