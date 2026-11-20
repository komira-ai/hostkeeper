//! What a call is on the wire, and whom it trusts: one POST over TLS to the endpoint, trusting
//! only the CA file, following no redirect, and saying plainly when nothing answers.

use crate::rig::{Reply, Rig, Server, TOKEN, args, call, closed_port, pki};

/// Proves: a call is one POST of JSON to the endpoint (a trailing `/` of it dropped) and the
/// method's path, with the bearer token and `Content-Type: application/json`, no cookie and no
/// other credential, and the body as built.
#[test]
fn a_call_is_one_post_of_json_with_a_bearer_token() {
    let rig = Rig::new("wire");
    let server = Server::canned(&rig.pki, 200, "{}");
    let mut words = rig.globals(server.port);
    words[1] = format!("https://127.0.0.1:{}/", server.port);
    words.extend(args(&[
        "--timeout-secs",
        "7",
        "gadgets",
        "tune",
        "--speed",
        "fast",
    ]));
    let ran = rig.fx(&words);
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (0, "{}\n", "")
    );
    let seen = server.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].method, "POST");
    assert_eq!(seen[0].target, "/hostkeeper.fixture.v1.Gadgets/Tune");
    assert_eq!(seen[0].text(), "{\"speed\":\"FAST\"}");
    assert_eq!(
        seen[0].header("authorization"),
        Some(format!("Bearer {TOKEN}").as_str())
    );
    assert_eq!(seen[0].header("content-type"), Some("application/json"));
    assert_eq!(seen[0].header("cookie"), None);
    assert_eq!(seen[0].header("proxy-authorization"), None);
}

/// Proves: only the CA file is trusted: a coordinator whose certificate is of another CA is no
/// answer (exit 1, nothing sent to it, the token not said), and so is a CA file with no
/// certificate in it; a CA file with several certificates trusts each.
#[test]
fn only_the_ca_file_is_trusted() {
    let rig = Rig::new("trust");
    let other = pki();
    let server = Server::canned(&other, 200, "{}");
    let ran = rig.fx(&call(&rig, server.port, &["gadgets", "list"]));
    assert_eq!((ran.code, ran.stdout.as_str()), (1, ""));
    assert!(
        ran.stderr.starts_with(&format!(
            "hk: no answer from https://127.0.0.1:{} (",
            server.port
        )),
        "{}",
        ran.stderr
    );
    assert!(!ran.stderr.contains(TOKEN));
    assert!(server.seen().is_empty());

    // A file with no certificate.
    rig.put("ca.pem", b"not a certificate\n", 0o644);
    let own = Server::canned(&rig.pki, 200, "{}");
    let ran = rig.fx(&call(&rig, own.port, &["gadgets", "list"]));
    assert_eq!(
        (ran.code, ran.stderr),
        (
            1,
            format!(
                "hk: no answer from https://127.0.0.1:{} (no certificate to trust)\n",
                own.port
            )
        )
    );

    // A file with the CAs of both servers, and a private key, which is no root.
    rig.put(
        "ca.pem",
        format!("{}\n{}\n{}", rig.pki.key_pem, other.ca_pem, rig.pki.ca_pem).as_bytes(),
        0o644,
    );
    for port in [server.port, own.port] {
        let ran = rig.fx(&call(&rig, port, &["gadgets", "list"]));
        assert_eq!(
            (ran.code, ran.stdout.as_str()),
            (0, "{}\n"),
            "{}",
            ran.stderr
        );
    }
}

/// Proves: no answer (nothing listens, or the server hangs up) is exit 1 with `no answer from
/// <endpoint> (<why>)` on one line, and a redirect is not followed: the answer is the 302's, not
/// the gateway's, and the address it names is not contacted.
#[test]
fn no_answer_and_a_redirect_end_the_call() {
    let rig = Rig::new("no-answer");
    let port = closed_port();
    let ran = rig.fx(&call(&rig, port, &["gadgets", "list"]));
    assert_eq!((ran.code, ran.stdout.as_str()), (1, ""));
    assert!(
        ran.stderr
            .starts_with(&format!("hk: no answer from https://127.0.0.1:{port} (")),
        "{}",
        ran.stderr
    );
    assert_eq!(ran.stderr.lines().count(), 1);

    let hung = Server::serve(&rig.pki, || (), |(), _| Reply::hang_up());
    let ran = rig.fx(&call(&rig, hung.port, &["gadgets", "list"]));
    assert_eq!(ran.code, 1);
    assert!(
        ran.stderr.starts_with("hk: no answer from "),
        "{}",
        ran.stderr
    );
    assert_eq!(hung.seen().len(), 1);

    let target = Server::canned(&rig.pki, 200, "{}");
    let location = format!("https://127.0.0.1:{}/elsewhere", target.port);
    let redirecting = Server::serve(
        &rig.pki,
        || (),
        move |(), _| Reply {
            location: Some(location.clone()),
            ..Reply::new(302, "")
        },
    );
    let ran = rig.fx(&call(&rig, redirecting.port, &["gadgets", "list"]));
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (1, "", "hk: the answer (HTTP 302) is not the gateway's\n")
    );
    assert!(target.seen().is_empty());
}

/// Proves: an answer that ends before the length it announced is no answer, said on one line.
#[test]
fn an_answer_cut_short_is_no_answer() {
    let rig = Rig::new("short-answer");
    let server = Server::serve(
        &rig.pki,
        || (),
        |(), _| Reply {
            short: true,
            ..Reply::new(200, "{\"a\":")
        },
    );
    let ran = rig.fx(&call(&rig, server.port, &["gadgets", "list"]));
    assert_eq!((ran.code, ran.stdout.as_str()), (1, ""));
    assert!(
        ran.stderr.starts_with(&format!(
            "hk: no answer from https://127.0.0.1:{} (",
            server.port
        )),
        "{}",
        ran.stderr
    );
    assert_eq!(ran.stderr.lines().count(), 1);
}

/// Proves: an answer is printed whole, a body with no content as the newline alone, and bytes that
/// are not text as they are; the endpoint may be given with a name the certificate holds.
#[test]
fn an_answer_is_printed_as_it_came() {
    let rig = Rig::new("answers-as-sent");
    for body in ["", "{\"a\":\"\u{e9}\"}", "{\"a\":\"x\0y\"}"] {
        let server = Server::canned(&rig.pki, 200, body);
        let ran = rig.fx(&call(&rig, server.port, &["gadgets", "list"]));
        assert_eq!((ran.code, ran.stdout), (0, format!("{body}\n")));
    }
}
