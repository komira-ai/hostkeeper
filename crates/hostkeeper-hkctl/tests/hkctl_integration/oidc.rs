//! `--github-oidc-audience`: the script asks the runner's token endpoint (here a fake on a
//! loopback listener, at the URL `ACTIONS_ID_TOKEN_REQUEST_URL` names) for a fresh ID token, with
//! the request token in a header pipe, masks the token in the job's log and calls with it.
//!
//! The fake endpoint answers the operator token of the tests as the ID token's `value`, so the
//! real gateway admits the call that follows: what is checked here is how the script gets and
//! uses the token, not the coordinator's OIDC rules, which its own tests hold.

use crate::gateway;
use crate::rig::{Call, Loopback, Rig, Script, TOKEN, args};

/// The runner's request token of the tests.
const REQUEST_TOKEN: &str = "runner-request-token-of-the-test";

/// A fake token endpoint answering `status` and `body` to every request.
fn endpoint(status: u16, body: &str) -> Loopback {
    let body = body.as_bytes().to_vec();
    Loopback::serve(|| (), move |(), _, _| (status, body.clone()))
}

/// The arguments of a call with the audience `audience`, the answer in `response`.
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

/// The two variables the runner sets, the URL at `port` with `query`.
fn runner(port: u16, query: &str) -> [(&'static str, String); 2] {
    [
        (
            "ACTIONS_ID_TOKEN_REQUEST_URL",
            format!("https://127.0.0.1:{port}/token{query}"),
        ),
        ("ACTIONS_ID_TOKEN_REQUEST_TOKEN", REQUEST_TOKEN.to_owned()),
    ]
}

/// What no call may hold: the two tokens, and the runner's two variable names.
fn assert_clean(calls: &[Call]) {
    for call in calls {
        for text in call.argv.iter().chain(&call.environ) {
            assert!(!text.contains(TOKEN), "{}: {text:?}", call.program);
            assert!(!text.contains(REQUEST_TOKEN), "{}: {text:?}", call.program);
        }
        for entry in &call.environ {
            assert!(!entry.starts_with("ACTIONS_ID_TOKEN_REQUEST"), "{entry:?}");
        }
    }
}

/// Catches the audience appended without its percent-encoding (an `&`, `#` or space would cut
/// or bend the query), a second `?` on a URL that has a query, the request token sent other than
/// in the header pipe or left in a child's environment, the ID token used before it is masked
/// (`::add-mask::` must be on stdout before the call), and stdout holding anything but the mask
/// when `--response-file` names the answer's file.
#[test]
fn github_oidc_audience_asks_the_runner_for_a_token_masks_it_and_calls_with_it() {
    let rig = Rig::new("oidc");
    let gateway = gateway::start(&rig.path(""));
    let tokens = endpoint(200, &format!("{{\"count\":1,\"value\":\"{TOKEN}\"}}"));
    let env = runner(tokens.port, "?api-version=2.0");
    let env: Vec<(&str, &str)> = env.iter().map(|(n, v)| (*n, v.as_str())).collect();
    let ran = rig.run(
        Script::Real,
        &call(
            &rig,
            gateway.port,
            "hostkeeper a&b#c/é",
            &["releases", "converge", "--service", "kbf"],
        ),
        b"",
        &env,
    );
    assert_eq!(
        (ran.code, ran.stdout.clone(), ran.stderr.as_str()),
        (0, format!("::add-mask::{TOKEN}\n"), "")
    );
    let asked = tokens.seen();
    assert_eq!(asked.len(), 1);
    assert_eq!(
        asked[0].target(),
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

    let curls = rig.curls();
    assert_eq!(curls.len(), 2);
    // The token request: the system's roots (no --cacert), no proxy, the request token in a pipe.
    assert_eq!(
        curls[0].argv_shape(),
        args(&[
            "-q",
            "--silent",
            "--show-error",
            "--proto",
            "=https",
            "--tlsv1.2",
            "--max-redirs",
            "0",
            "--globoff",
            "--noproxy",
            "*",
            "--connect-timeout",
            "30",
            "--max-time",
            "30",
            "--header",
            "@/dev/fd/N",
            "--write-out",
            "\\n%{http_code}",
            "--url",
            &format!(
                "https://127.0.0.1:{}/token?api-version=2.0&audience=hostkeeper%20a%26b%23c%2F%C3%A9",
                tokens.port
            ),
        ])
    );
    assert_eq!(curls[0].stdout, "");
    // The call, after the mask was printed.
    assert_eq!(curls[1].stdout, format!("::add-mask::{TOKEN}\n"));
    assert_clean(&rig.calls());

    // A URL with no query gets one.
    rig.reset();
    let env = runner(tokens.port, "");
    let env: Vec<(&str, &str)> = env.iter().map(|(n, v)| (*n, v.as_str())).collect();
    let ran = rig.run(
        Script::Real,
        &call(
            &rig,
            gateway.port,
            "aud",
            &["releases", "converge", "--service", "kbf"],
        ),
        b"",
        &env,
    );
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(tokens.seen()[1].target(), "/token?audience=aud");
}

/// Catches a call made with no token or a token that is not one: a reply without `value`, with
/// a value that is empty, holds a space or an escape, or is not a string, a refused or failed
/// token request, and a job without the runner's variables (no `id-token: write`) each end the
/// script, exit 1, before any call of the coordinator.
#[test]
fn a_token_reply_without_a_token_and_a_job_without_the_variables_are_refused() {
    let rig = Rig::new("oidc-refused");
    let cases = [
        (
            200,
            "{\"count\":1}",
            "hkctl: the runner's token reply has no value\n",
        ),
        (
            200,
            "{\"value\":\"\"}",
            "hkctl: the runner's token reply has no value that is a token\n",
        ),
        (
            200,
            "{\"value\":\"two words\"}",
            "hkctl: the runner's token reply has no value that is a token\n",
        ),
        (
            200,
            "{\"value\":\"a\\/b\"}",
            "hkctl: the runner's token reply has no value that is a token\n",
        ),
        (
            200,
            "{\"value\":1}",
            "hkctl: the runner's token reply has no value that is a token\n",
        ),
        (
            200,
            "{\"value\":\"unterminated",
            "hkctl: the runner's token reply has no value that is a token\n",
        ),
        (
            401,
            "{\"message\":\"no\"}",
            "hkctl: the runner's token request was answered HTTP 401\n",
        ),
    ];
    for (status, body, why) in cases {
        rig.reset();
        let tokens = endpoint(status, body);
        let env = runner(tokens.port, "?api-version=2.0");
        let env: Vec<(&str, &str)> = env.iter().map(|(n, v)| (*n, v.as_str())).collect();
        let ran = rig.run(
            Script::Fixture,
            &call(&rig, 1, "aud", &["gadgets", "list"]),
            b"",
            &env,
        );
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
            (1, "", why),
            "{body}"
        );
        assert_eq!(rig.curls().len(), 1, "{body}");
        assert_clean(&rig.calls());
    }
    // A token endpoint that does not answer.
    rig.reset();
    rig.fail();
    let env = runner(1, "?api-version=2.0");
    let env: Vec<(&str, &str)> = env.iter().map(|(n, v)| (*n, v.as_str())).collect();
    let ran = rig.run(
        Script::Fixture,
        &call(&rig, 1, "aud", &["gadgets", "list"]),
        b"",
        &env,
    );
    assert_eq!(
        (ran.code, ran.stderr.as_str()),
        (
            1,
            "curl: (7) Failed to connect\nhkctl: the runner's token request failed (curl exited 7)\n"
        )
    );

    let no_vars = "hkctl: --github-oidc-audience needs the job to have id-token: write: the runner set no ACTIONS_ID_TOKEN_REQUEST_URL and ACTIONS_ID_TOKEN_REQUEST_TOKEN\n";
    let bad_char = "hkctl: the runner set ACTIONS_ID_TOKEN_REQUEST_URL or ACTIONS_ID_TOKEN_REQUEST_TOKEN with a character that is not visible ASCII\n";
    let envs: [(&[(&str, &str)], &str); 5] = [
        (&[], no_vars),
        (&[("ACTIONS_ID_TOKEN_REQUEST_URL", "https://x/t")], no_vars),
        (
            &[
                ("ACTIONS_ID_TOKEN_REQUEST_URL", "http://x/t"),
                ("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "t"),
            ],
            "hkctl: ACTIONS_ID_TOKEN_REQUEST_URL is not an https URL\n",
        ),
        (
            &[
                ("ACTIONS_ID_TOKEN_REQUEST_URL", "https://x/t"),
                ("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "a b"),
            ],
            bad_char,
        ),
        (
            &[
                ("ACTIONS_ID_TOKEN_REQUEST_URL", "https://x/ t"),
                ("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "t"),
            ],
            bad_char,
        ),
    ];
    for (env, why) in envs {
        rig.reset();
        let ran = rig.run(
            Script::Fixture,
            &call(&rig, 1, "aud", &["gadgets", "list"]),
            b"",
            env,
        );
        assert_eq!((ran.code, ran.stderr.as_str()), (1, why), "{env:?}");
        assert!(rig.calls().is_empty());
    }
}
