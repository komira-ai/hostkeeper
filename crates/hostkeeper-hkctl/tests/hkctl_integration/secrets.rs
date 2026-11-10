//! The token and every secret stay in the script's unexported variables and reach curl only
//! through a pipe: never in an argument list, a child's environment (curl's or find's) or a
//! diagnostic; and a token file its group or others may read is refused before it is read.

use crate::gateway;
use crate::rig::{Call, Rig, Script, TOKEN, args};

/// What no recorded call may hold, anywhere in its arguments or its environment.
fn assert_absent(calls: &[Call], secrets: &[&str]) {
    assert!(!calls.is_empty());
    for call in calls {
        for secret in secrets {
            for arg in &call.argv {
                assert!(!arg.contains(secret), "{}: argument {arg:?}", call.program);
            }
            for entry in &call.environ {
                assert!(
                    !entry.contains(secret),
                    "{}: environment {entry:?}",
                    call.program
                );
            }
        }
    }
}

/// The names of a call's environment.
fn names(call: &Call) -> Vec<String> {
    call.environ
        .iter()
        .map(|e| e.split('=').next().unwrap_or_default().to_owned())
        .collect()
}

/// Catches the token or a secret field's value reaching curl or find as an argument or through
/// the environment (a variable of the script's that the caller's environment exported first,
/// planted here under each name a secret is kept in), or printed on stderr; and the token not
/// reaching curl's header pipe as the bearer.
#[test]
fn the_token_and_a_secret_never_reach_an_argument_an_environment_or_stderr() {
    let rig = Rig::new("token-absent");
    let gateway = gateway::start(&rig.path(""));
    let planted = [
        ("_hk_token", "planted"),
        ("_hk_body", "planted"),
        ("_hk_vals", "planted"),
        ("_hk_value", "planted"),
        ("_hk_endpoint", "https://planted.example"),
    ];

    // From the token file, through the real gateway, answered and refused.
    let mut globals = rig.globals(gateway.port);
    globals.extend(args(&["hosts", "list"]));
    let ran = rig.run(Script::Real, &globals, b"", &planted);
    assert_eq!((ran.code, ran.stderr.as_str()), (0, ""));
    let request = rig.put("bad.json", b"{\"noSuchField\":1}", 0o644);
    globals.extend(args(&["--request", &request]));
    let ran = rig.run(Script::Real, &globals, b"", &planted);
    // The refusal is followed by the probe of the coordinator's version, which carries no token.
    let seen = gateway.seen();
    let probe = seen.last().expect("the probe");
    assert_eq!(probe.target(), "/v1/version?at_least=1.2.3");
    assert_eq!((probe.status, probe.header("authorization")), (409, None));
    let said: serde_json::Value = serde_json::from_slice(&probe.answer).expect("a version");
    let coordinator = said["version"].as_str().expect("a version").to_owned();
    assert_eq!(
        (ran.code, ran.stderr),
        (
            2,
            format!(
                "hkctl: the coordinator answered INVALID_ARGUMENT\nhkctl: the coordinator is older than this hkctl (1.2.3): it runs {coordinator}; update the coordinator, then call again\nrefused: coordinator_older\n"
            )
        )
    );
    // From stdin.
    let stdin_globals = args(&[
        "--endpoint",
        &format!("https://127.0.0.1:{}", gateway.port),
        "--ca-file",
        &rig.path("ca.pem").display().to_string(),
        "--token-stdin",
        "hosts",
        "list",
    ]);
    let ran = rig.run(
        Script::Real,
        &stdin_globals,
        format!("{TOKEN}\n").as_bytes(),
        &planted,
    );
    assert_eq!((ran.code, ran.stderr.as_str()), (0, ""));

    let calls = rig.calls();
    // find ran once per call from the token file, curl once per call.
    let programs: Vec<&str> = calls.iter().map(|c| c.program.as_str()).collect();
    assert_eq!(programs, ["find", "curl", "find", "curl", "curl", "curl"]);
    assert_absent(&calls, &[TOKEN, "planted"]);
    for call in &calls {
        for (name, _) in planted {
            assert!(
                !names(call).iter().any(|n| n == name),
                "{name} in {:?}",
                call.environ
            );
        }
    }
    for curl in calls.iter().filter(|c| c.program == "curl") {
        // The probe of the coordinator's version holds no credential at all.
        if curl.argv.iter().any(|a| a.contains("/v1/version")) {
            assert!(
                !curl.argv.iter().any(|a| a == "--header"),
                "{:?}",
                curl.argv
            );
            assert!(curl.reads.is_empty(), "{:?}", curl.reads);
            continue;
        }
        assert_eq!(
            curl.read_after("--header", 0).as_deref(),
            Some(format!("Authorization: Bearer {TOKEN}\n").as_str())
        );
        assert!(
            curl.argv
                .iter()
                .any(|a| a.ends_with(&format!(":{}", gateway.port))
                    || a.contains(&format!(":{}/", gateway.port)))
        );
    }

    // A secret field of the fixture, from its file and from stdin, refused or not.
    rig.reset();
    let password = "the-password-of-the-test";
    let file = rig.put("password", format!("{password}\n").as_bytes(), 0o600);
    let mut globals = rig.globals(1);
    globals.extend(args(&["widgets", "login", "--password-file", &file]));
    rig.reply(200, "{}");
    let ran = rig.run(Script::Fixture, &globals, b"", &planted);
    assert_eq!((ran.code, ran.stderr.as_str()), (0, ""));
    rig.reply(403, "{\"code\":\"PERMISSION_DENIED\",\"reason\":\"no\"}");
    let mut stdin_globals = rig.globals(1);
    stdin_globals.extend(args(&["widgets", "login", "--password-stdin"]));
    let ran = rig.run(
        Script::Fixture,
        &stdin_globals,
        password.as_bytes(),
        &planted,
    );
    assert_eq!(
        (ran.code, ran.stderr.as_str()),
        (
            3,
            "hkctl: the coordinator answered PERMISSION_DENIED\nrefused: no\n"
        )
    );
    let calls = rig.calls();
    assert_absent(&calls, &[TOKEN, password]);
    let curls = rig.curls();
    assert_eq!(curls.len(), 2);
    for curl in curls {
        assert_eq!(
            curl.read_after("--data-binary", 0).as_deref(),
            Some(format!("{{\"password\":\"{password}\"}}").as_str())
        );
    }
}

/// Catches a token file its group or others may read being used (and so a token left readable
/// on a shared machine), a mode check that refuses a private file or a link to one, and an
/// unreadable, empty or malformed token passing to curl.
#[test]
fn a_token_file_its_group_or_others_may_read_is_refused() {
    let rig = Rig::new("token-mode");
    let token = rig.path("token").display().to_string();
    let mut globals = rig.globals(1);
    globals.extend(args(&["gadgets", "list"]));
    for mode in [0o640, 0o604, 0o644, 0o660, 0o606, 0o440] {
        rig.reset();
        rig.put("token", format!("{TOKEN}\n").as_bytes(), mode);
        let ran = rig.run(Script::Fixture, &globals, b"", &[]);
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr),
            (
                2,
                "",
                format!(
                    "hkctl: --token-file {token} may be read by its group or others; make it 0600\n"
                )
            ),
            "{mode:o}"
        );
        assert!(rig.curls().is_empty(), "{mode:o}");
    }
    for mode in [0o600, 0o400, 0o700] {
        rig.reset();
        rig.reply(200, "{}");
        rig.put("token", format!("{TOKEN}\n").as_bytes(), mode);
        let ran = rig.run(Script::Fixture, &globals, b"", &[]);
        assert_eq!((ran.code, ran.stderr.as_str()), (0, ""), "{mode:o}");
    }
    // A link to a private file is the file's mode, not the link's.
    rig.reset();
    rig.reply(200, "{}");
    rig.put("token", format!("{TOKEN}\n").as_bytes(), 0o600);
    std::os::unix::fs::symlink(rig.path("token"), rig.path("link")).expect("a link");
    let mut linked = globals.clone();
    let at = linked
        .iter()
        .position(|a| a == &token)
        .expect("the token flag");
    linked[at] = "link".to_owned();
    let ran = rig.run(Script::Fixture, &linked, b"", &[]);
    assert_eq!((ran.code, ran.stderr.as_str()), (0, ""));

    // A relative path that starts with a dash is a file, not a flag of find.
    rig.reset();
    rig.reply(200, "{}");
    rig.put("-token", format!("{TOKEN}\n").as_bytes(), 0o600);
    linked[at] = "-token".to_owned();
    let ran = rig.run(Script::Fixture, &linked, b"", &[]);
    assert_eq!((ran.code, ran.stderr.as_str()), (0, ""));

    let missing = rig.path("missing").display().to_string();
    let cases = [
        (
            missing.as_str(),
            &b""[..],
            0o600,
            format!("cannot read the mode of --token-file {missing}"),
        ),
        (
            token.as_str(),
            b"",
            0o600,
            "the token is empty or holds a character that is not visible ASCII".to_owned(),
        ),
        (
            token.as_str(),
            b"two words\n",
            0o600,
            "the token is empty or holds a character that is not visible ASCII".to_owned(),
        ),
        (
            token.as_str(),
            b"line\nbreak\n",
            0o600,
            "the token is empty or holds a character that is not visible ASCII".to_owned(),
        ),
        (
            token.as_str(),
            b"back\\slash",
            0o600,
            "the token is empty or holds a character that is not visible ASCII".to_owned(),
        ),
    ];
    for (path, content, mode, why) in cases {
        rig.reset();
        rig.put("token", content, mode);
        let mut words = globals.clone();
        words[at] = path.to_owned();
        let ran = rig.run(Script::Fixture, &words, b"", &[]);
        assert_eq!(
            (ran.code, ran.stderr),
            (2, format!("hkctl: {why}\n")),
            "{path} {content:?}"
        );
        assert!(rig.curls().is_empty());
    }
}
