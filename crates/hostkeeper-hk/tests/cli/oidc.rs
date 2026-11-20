//! `--github-oidc-audience`: `hk` asks the runner's token endpoint (here a fake HTTPS server on
//! loopback, at the URL `ACTIONS_ID_TOKEN_REQUEST_URL` names, whose CA is among the roots the
//! world gives and not the coordinator's CA file) for a fresh ID token with the request token as
//! its bearer, masks the token in the job's log and calls with it.
//!
//! The fake endpoint answers the operator token of the tests as the ID token's `value`, so the
//! real gateway admits the call that follows: what is checked here is how `hk` gets and uses the
//! token, not the coordinator's OIDC rules, which its own tests hold.

use crate::gateway;
use crate::rig::{Opts, Rig, Server, TOKEN, args, der_of, pki};

/// The runner's request token of the tests.
const REQUEST_TOKEN: &str = "runner-request-token-of-the-test";

/// The arguments of a call with the audience `audience`, the answer in `response.json`.
fn call(rig: &Rig, port: u16, audience: &str, words: &[&str]) -> Vec<String> {
    let mut all = args(&[
        "--endpoint",
        &format!("https://127.0.0.1:{port}"),
        "--ca-file",
        &rig.path("ca.pem").display().to_string(),
        "--github-oidc-audience",
        audience,
        "--response-file",
        &rig.path("response.json").display().to_string(),
    ]);
    all.extend(args(words));
    all
}

/// Proves: the audience is percent-encoded byte by byte (an `&`, `#`, `/`, a space and a
/// multi-byte letter), the query is appended with `&` to a URL that has one and `?` to one that
/// has none, the request token is the bearer of the token request only, the ID token is
/// printed as `::add-mask::<token>` on stdout and nothing else is, and the call carries the ID
/// token; the runner's token is trusted by the roots of the world and not by the CA file.
#[test]
fn github_oidc_audience_asks_the_runner_for_a_token_masks_it_and_calls_with_it() {
    let rig = Rig::new("oidc");
    let gateway = gateway::start(&rig.pki, &rig.path(""));
    let runner_pki = pki();
    let tokens = Server::canned(
        &runner_pki,
        200,
        &format!("{{\"count\":1,\"value\":\"{TOKEN}\"}}"),
    );
    let roots = vec![der_of(&runner_pki.ca_pem)];
    let url = format!("https://127.0.0.1:{}/token?api-version=2.0", tokens.port);
    let ran = rig.run(
        Opts::real().runner(&url, REQUEST_TOKEN, roots.clone()),
        &call(
            &rig,
            gateway.port,
            "hostkeeper a&b#c/é",
            &["releases", "converge", "--service", "kbf"],
        ),
    );
    assert_eq!(
        (ran.code, ran.stdout.clone(), ran.stderr.as_str()),
        (0, format!("::add-mask::{TOKEN}\n"), "")
    );
    let asked = tokens.seen();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].method, "GET");
    assert_eq!(
        asked[0].target,
        "/token?api-version=2.0&audience=hostkeeper%20a%26b%23c%2F%C3%A9"
    );
    assert_eq!(
        asked[0].header("authorization"),
        Some(format!("Bearer {REQUEST_TOKEN}").as_str())
    );
    let called = gateway.seen();
    assert_eq!(called.len(), 1);
    assert_eq!(
        called[0].header("authorization"),
        Some(format!("Bearer {TOKEN}").as_str())
    );
    assert_eq!(called[0].status, 200);
    assert_eq!(
        std::fs::read_to_string(rig.path("response.json")).expect("the response file"),
        format!("{}\n", String::from_utf8_lossy(&called[0].answer))
    );
    assert!(!called[0].text().contains(REQUEST_TOKEN));

    // A URL with no query gets one.
    let url = format!("https://127.0.0.1:{}/token", tokens.port);
    let ran = rig.run(
        Opts::real().runner(&url, REQUEST_TOKEN, roots),
        &call(
            &rig,
            gateway.port,
            "aud",
            &["releases", "converge", "--service", "kbf"],
        ),
    );
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(tokens.seen()[1].target, "/token?audience=aud");

    // The coordinator's CA is not a root of the runner's endpoint: the request is not answered.
    let ran = rig.run(
        Opts::real().runner(&url, REQUEST_TOKEN, vec![der_of(&rig.pki.ca_pem)]),
        &call(&rig, gateway.port, "aud", &["hosts", "list"]),
    );
    assert_eq!((ran.code, ran.stdout.as_str()), (1, ""));
    assert!(
        ran.stderr
            .starts_with("hk: the runner's token request failed ("),
        "{}",
        ran.stderr
    );
    assert_eq!(tokens.seen().len(), 2);
    assert_eq!(gateway.seen().len(), 2);
}

/// Proves: a call is not made with a token that is none: a reply without `value`, with a value
/// that is empty, holds a space or an escape, is not a string or never ends, a refused or failed
/// token request, and a job without the runner's variables (no `id-token: write`) each end the
/// run with exit 1 and the message exactly, before any call of the coordinator.
#[test]
fn a_token_reply_without_a_token_and_a_job_without_the_variables_are_refused() {
    let rig = Rig::new("oidc-refused");
    let coordinator = Server::canned(&rig.pki, 200, "{}");
    let runner_pki = pki();
    let roots = vec![der_of(&runner_pki.ca_pem)];
    let no_token = "hk: the runner's token reply has no value that is a token\n";
    let cases = [
        (
            200,
            "{\"count\":1}",
            "hk: the runner's token reply has no value\n",
        ),
        (200, "{\"value\":\"\"}", no_token),
        (200, "{\"value\":\"two words\"}", no_token),
        (200, "{\"value\":\"a\\/b\"}", no_token),
        (200, "{\"value\":1}", no_token),
        (200, "{\"value\":\"unterminated", no_token),
        (
            401,
            "{\"message\":\"no\"}",
            "hk: the runner's token request was answered HTTP 401\n",
        ),
    ];
    for (status, body, why) in cases {
        let tokens = Server::canned(&runner_pki, status, body);
        let url = format!("https://127.0.0.1:{}/token?api-version=2.0", tokens.port);
        let ran = rig.run(
            Opts::fixture().runner(&url, REQUEST_TOKEN, roots.clone()),
            &call(&rig, coordinator.port, "aud", &["gadgets", "list"]),
        );
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
            (1, "", why),
            "{body}"
        );
        assert_eq!(tokens.seen().len(), 1, "{body}");
    }
    // A token with spaces and a value that is a token with a colon and spaces around it.
    let tokens = Server::canned(&runner_pki, 200, "{\"value\" : \"abc.def\"}");
    let url = format!("https://127.0.0.1:{}/token", tokens.port);
    let ran = rig.run(
        Opts::fixture().runner(&url, REQUEST_TOKEN, roots.clone()),
        &call(&rig, coordinator.port, "aud", &["gadgets", "list"]),
    );
    assert_eq!(
        (ran.code, ran.stdout.as_str()),
        (0, "::add-mask::abc.def\n")
    );
    assert_eq!(
        coordinator.seen()[0].header("authorization"),
        Some("Bearer abc.def")
    );

    // A token endpoint that does not answer.
    let url = format!(
        "https://127.0.0.1:{}/token?api-version=2.0",
        crate::rig::closed_port()
    );
    let ran = rig.run(
        Opts::fixture().runner(&url, REQUEST_TOKEN, roots.clone()),
        &call(&rig, coordinator.port, "aud", &["gadgets", "list"]),
    );
    assert_eq!((ran.code, ran.stdout.as_str()), (1, ""));
    assert!(
        ran.stderr
            .starts_with("hk: the runner's token request failed ("),
        "{}",
        ran.stderr
    );
    assert!(!ran.stderr.contains(REQUEST_TOKEN));
    // No root to trust.
    let ran = rig.run(
        Opts::fixture().runner(&url, REQUEST_TOKEN, Vec::new()),
        &call(&rig, coordinator.port, "aud", &["gadgets", "list"]),
    );
    assert_eq!(
        (ran.code, ran.stderr.as_str()),
        (
            1,
            "hk: the runner's token request failed (no certificate to trust)\n"
        )
    );

    let no_vars = "hk: --github-oidc-audience needs the job to have id-token: write: the runner set no ACTIONS_ID_TOKEN_REQUEST_URL and ACTIONS_ID_TOKEN_REQUEST_TOKEN\n";
    let bad_char = "hk: the runner set ACTIONS_ID_TOKEN_REQUEST_URL or ACTIONS_ID_TOKEN_REQUEST_TOKEN with a character that is not visible ASCII\n";
    let envs: [(Option<(&str, &str)>, &str); 5] = [
        (None, no_vars),
        (Some(("https://runner.example.com/t", "")), no_vars),
        (
            Some(("http://runner.example.com/t", "t")),
            "hk: ACTIONS_ID_TOKEN_REQUEST_URL is not an https URL\n",
        ),
        (Some(("https://runner.example.com/t", "a b")), bad_char),
        (Some(("https://runner.example.com/ t", "t")), bad_char),
    ];
    for (env, why) in envs {
        let opts = match env {
            Some((url, token)) => Opts::fixture().runner(url, token, Vec::new()),
            None => Opts::fixture(),
        };
        let ran = rig.run(
            opts,
            &call(&rig, coordinator.port, "aud", &["gadgets", "list"]),
        );
        assert_eq!((ran.code, ran.stderr.as_str()), (1, why), "{env:?}");
    }
    assert_eq!(coordinator.seen().len(), 1);
}
