//! The script's surface: the exact curl arguments of a call, the hold of a `wait_secs` field, the
//! help and the version, the usage errors of the global flags, and a new RPC of the fixture,
//! which appears as a command with no hand edit.

use crate::rig::{Rig, STAMP, Script, args};

/// Catches a curl option of the design dropped or changed (`-q` first so no `.curlrc` is read,
/// https only, TLS 1.2 or later, no redirect, the CA file the only root, the token and the body
/// each through a pipe, the status after the body) and the method's path misspelled.
#[test]
fn a_call_runs_curl_with_exactly_the_designs_arguments() {
    let rig = Rig::new("curl-args");
    rig.reply(200, "{}");
    let ca = rig.path("ca.pem").display().to_string();
    let mut words = rig.globals(8443);
    words.extend(args(&[
        "--timeout-secs",
        "7",
        "gadgets",
        "tune",
        "--speed",
        "fast",
    ]));
    let ran = rig.run(Script::Fixture, &words, b"", &[]);
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (0, "{}\n", "")
    );
    let curls = rig.curls();
    assert_eq!(curls.len(), 1);
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
            "--cacert",
            &ca,
            "--connect-timeout",
            "7",
            "--max-time",
            "7",
            "--header",
            "@/dev/fd/N",
            "--header",
            "Content-Type: application/json",
            "--data-binary",
            "@/dev/fd/N",
            "--write-out",
            "\\n%{http_code}",
            "--url",
            "https://127.0.0.1:8443/hostkeeper.fixture.v1.Gadgets/Tune",
        ])
    );
    assert_eq!(
        curls[0].read_after("--data-binary", 0).as_deref(),
        Some("{\"speed\":\"FAST\"}")
    );
    // Only curl and find ran.
    let programs: Vec<String> = rig.calls().into_iter().map(|c| c.program).collect();
    assert_eq!(programs, ["find", "curl"]);
}

/// Catches a held call cut by curl before the coordinator answers it: a `wait_secs` field, from
/// its flag or the request file, adds its seconds to curl's `--max-time`; a value that is no
/// whole number adds nothing, and the gateway judges it.
#[test]
fn a_wait_secs_field_lengthens_curls_deadline_by_its_hold() {
    let rig = Rig::new("hold");
    let request = rig.put(
        "wait.json",
        b"{\"service\":\"s\", \"waitSecs\": \"60\" }",
        0o644,
    );
    let odd = rig.put("odd.json", b"{\"wait_secs\": -1}", 0o644);
    let cases: [(Vec<&str>, &str); 5] = [
        (vec!["--wait-secs", "1800"], "1830"),
        (vec!["--timeout-secs", "5", "--wait-secs", "1800"], "1805"),
        (vec!["--request", &request], "90"),
        (vec!["--request", &odd], "30"),
        (vec![], "30"),
    ];
    for (words, max) in cases {
        rig.reset();
        rig.reply(200, "{}");
        let mut all = rig.globals(1);
        all.extend(args(&["widgets", "wait"]));
        all.extend(args(&words));
        let ran = rig.run(Script::Fixture, &all, b"", &[]);
        assert_eq!(ran.code, 0, "{ran:?}");
        let argv = &rig.curls()[0].argv;
        let at = argv
            .iter()
            .position(|a| a == "--max-time")
            .expect("--max-time");
        assert_eq!(argv[at + 1], max, "{words:?}");
        let connect = argv
            .iter()
            .position(|a| a == "--connect-timeout")
            .expect("--connect-timeout");
        assert_ne!(argv[connect + 1], "0");
    }
}

/// Catches `--version` printing anything but `hkctl <stamp>`, a field named `version` read as
/// hkctl's own flag after the method, help that calls or exits other than 0 (or 2 with nothing
/// to run), and a service or method name that is no command passing.
#[test]
fn version_help_and_names_are_answered_without_a_call() {
    let rig = Rig::new("surface");
    let ran = rig.run(Script::Real, &args(&["--version"]), b"", &[]);
    assert_eq!(
        (ran.code, ran.stdout, ran.stderr),
        (0, format!("hkctl {STAMP}\n"), String::new())
    );
    for words in [
        vec!["--help"],
        vec!["-h"],
        vec!["releases", "--help"],
        vec!["releases", "set-desired", "--help"],
        vec!["--endpoint", "https://x", "hosts", "get", "-h"],
    ] {
        let ran = rig.run(Script::Real, &args(&words), b"", &[]);
        assert_eq!((ran.code, ran.stderr.as_str()), (0, ""), "{words:?}");
        assert!(ran.stdout.starts_with("hkctl"), "{words:?}: {}", ran.stdout);
    }
    let ran = rig.run(
        Script::Real,
        &args(&["releases", "set-desired", "--help"]),
        b"",
        &[],
    );
    assert!(
        ran.stdout.starts_with(
            "hkctl releases set-desired: /hostkeeper.operator.v1.Releases/SetDesired\n"
        )
    );
    assert!(
        ran.stdout.contains("\n  --service STRING\n"),
        "{}",
        ran.stdout
    );
    assert!(
        ran.stdout
            .contains("Through --request only: desired (hostkeeper.operator.v1.DesiredState)"),
        "{}",
        ran.stdout
    );
    let ran = rig.run(Script::Real, &args(&["--help"]), b"", &[]);
    assert!(ran.stdout.contains("\n  --github-oidc-audience AUDIENCE\n"));
    assert!(ran.stdout.contains(
        "\n  3  the coordinator refused the caller: UNAUTHENTICATED or PERMISSION_DENIED\n"
    ));
    assert!(ran.stdout.contains("\nServices:\n  alerts\n"));
    assert!(
        !ran.stdout.contains("\n  coordinator\n"),
        "the host API has no command"
    );

    // Nothing to run: the help on stderr, exit 2.
    for words in [vec![], vec!["hosts"], vec!["--endpoint", "https://x"]] {
        let ran = rig.run(Script::Real, &args(&words), b"", &[]);
        assert_eq!((ran.code, ran.stdout.as_str()), (2, ""), "{words:?}");
        assert!(ran.stderr.starts_with("hkctl"), "{words:?}");
    }
    for (words, why) in [
        (
            vec!["nosuch"],
            "no service nosuch; 'hkctl --help' lists them",
        ),
        (
            vec!["coordinator", "heartbeat"],
            "no service coordinator; 'hkctl --help' lists them",
        ),
        (
            vec!["hosts", "nosuch"],
            "the service hosts has no method nosuch; 'hkctl hosts --help' lists them",
        ),
        (vec!["--nosuch=secret", "hosts"], "unknown flag --nosuch"),
        (vec!["hosts", "--nosuch", "get"], "unknown flag --nosuch"),
        (
            vec!["hosts", "get", "--version"],
            "hosts get has no flag --version",
        ),
    ] {
        let ran = rig.run(Script::Real, &args(&words), b"", &[]);
        assert_eq!(
            (ran.code, ran.stderr),
            (2, format!("hkctl: {why}\n")),
            "{words:?}"
        );
    }
    assert!(rig.calls().is_empty());

    // After the method, --version is the field of that name.
    rig.reply(200, "{}");
    let mut words = rig.globals(1);
    words.extend(args(&["gadgets", "tune", "--version", "1.0"]));
    let ran = rig.run(Script::Fixture, &words, b"", &[]);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(
        rig.curls()[0].read_after("--data-binary", 0).as_deref(),
        Some("{\"version\":\"1.0\"}")
    );
}

/// Catches a call made without what it needs (an endpoint that is https, a readable CA file,
/// exactly one credential), a global flag given twice, a timeout that is no whole number of
/// seconds, and two inputs both reading stdin: each a usage error before any program runs.
#[test]
fn the_global_flags_usage_errors_end_the_script_before_any_call() {
    let rig = Rig::new("global-errors");
    let ca = rig.path("ca.pem").display().to_string();
    let token = rig.path("token").display().to_string();
    let missing = rig.path("missing").display().to_string();
    let directory = rig.path("").display().to_string();
    let call = ["gadgets", "list"];
    let cases: [(Vec<&str>, String); 16] = [
        (
            vec!["--ca-file", &ca, "--token-file", &token],
            "a call needs --endpoint".to_owned(),
        ),
        (
            vec![
                "--endpoint",
                "http://x",
                "--ca-file",
                &ca,
                "--token-file",
                &token,
            ],
            "--endpoint takes https://<host>[:<port>]".to_owned(),
        ),
        (
            vec![
                "--endpoint",
                "https://x/path",
                "--ca-file",
                &ca,
                "--token-file",
                &token,
            ],
            "--endpoint takes https://<host>[:<port>]".to_owned(),
        ),
        (
            vec!["--endpoint", "https://x", "--token-file", &token],
            "a call needs --ca-file".to_owned(),
        ),
        (
            vec![
                "--endpoint",
                "https://x",
                "--ca-file",
                &missing,
                "--token-file",
                &token,
            ],
            format!("cannot read --ca-file {missing}"),
        ),
        (
            vec![
                "--endpoint",
                "https://x",
                "--ca-file",
                &directory,
                "--token-file",
                &token,
            ],
            format!("cannot read --ca-file {directory}"),
        ),
        (
            vec!["--endpoint", "https://x", "--ca-file", &ca],
            "a call needs exactly one of --token-file, --token-stdin and --github-oidc-audience"
                .to_owned(),
        ),
        (
            vec![
                "--endpoint",
                "https://x",
                "--ca-file",
                &ca,
                "--token-file",
                &token,
                "--token-stdin",
            ],
            "a call needs exactly one of --token-file, --token-stdin and --github-oidc-audience"
                .to_owned(),
        ),
        (
            vec!["--endpoint", "https://x", "--endpoint", "https://y"],
            "--endpoint is given twice".to_owned(),
        ),
        (
            vec!["--ca-file", &ca, "--ca-file", &ca],
            "--ca-file is given twice".to_owned(),
        ),
        (
            vec!["--token-file", &token, "--token-file", &token],
            "--token-file is given twice".to_owned(),
        ),
        (
            vec!["--token-stdin", "--token-stdin"],
            "--token-stdin is given twice".to_owned(),
        ),
        (
            vec!["--github-oidc-audience", "a", "--github-oidc-audience=b"],
            "--github-oidc-audience is given twice".to_owned(),
        ),
        (
            vec!["--response-file", "a", "--response-file", "b"],
            "--response-file is given twice".to_owned(),
        ),
        (
            vec!["--timeout-secs", "0"],
            "--timeout-secs takes a whole number of seconds, 1 to 999999".to_owned(),
        ),
        (
            vec!["--timeout-secs", "5", "--timeout-secs", "6"],
            "--timeout-secs is given twice".to_owned(),
        ),
    ];
    for (globals, why) in cases {
        let mut words = args(&globals);
        words.extend(args(&call));
        let ran = rig.run(Script::Fixture, &words, b"", &[]);
        assert_eq!(
            (ran.code, ran.stderr),
            (2, format!("hkctl: {why}\n")),
            "{globals:?}"
        );
        assert!(rig.calls().is_empty(), "{globals:?}");
    }
    // An empty response file path, and two inputs reading stdin, after the method too.
    let mut words = rig.globals(1);
    words.extend(args(&["--response-file=", "gadgets", "list"]));
    let ran = rig.run(Script::Fixture, &words, b"", &[]);
    assert_eq!(
        (ran.code, ran.stderr.as_str()),
        (2, "hkctl: --response-file needs a path\n")
    );
    let words = args(&["--token-stdin", "widgets", "make", "--request", "-"]);
    let ran = rig.run(Script::Fixture, &words, b"", &[]);
    assert_eq!(
        (ran.code, ran.stderr.as_str()),
        (
            2,
            "hkctl: --token-stdin and --request both read stdin; at most one input may\n"
        )
    );
    let words = args(&["--token-stdin=x", "gadgets", "list"]);
    let ran = rig.run(Script::Fixture, &words, b"", &[]);
    assert_eq!(
        (ran.code, ran.stderr.as_str()),
        (2, "hkctl: --token-stdin takes no value\n")
    );
    assert!(rig.calls().is_empty());
}

/// Catches a command that needs a hand edit to appear: the fixture's `BrandNewMethod`, which no
/// real proto has, is a command of the fixture's script that posts to its path, has its help,
/// and is not in the real script; and the fixture's host API package has no command.
#[test]
fn a_new_rpc_in_the_fixture_protos_is_a_command_with_no_edit() {
    let rig = Rig::new("new-rpc");
    rig.reply(200, "{}");
    let mut words = rig.globals(1);
    words.extend(args(&["widgets", "brand-new-method"]));
    let ran = rig.run(Script::Fixture, &words, b"", &[]);
    assert_eq!((ran.code, ran.stdout.as_str()), (0, "{}\n"));
    assert_eq!(
        rig.curls()[0].argv.last().map(String::as_str),
        Some("https://127.0.0.1:1/hostkeeper.fixture.v1.Widgets/BrandNewMethod")
    );
    let ran = rig.run(
        Script::Fixture,
        &args(&["widgets", "brand-new-method", "--help"]),
        b"",
        &[],
    );
    assert!(ran.stdout.starts_with(
        "hkctl widgets brand-new-method: /hostkeeper.fixture.v1.Widgets/BrandNewMethod\n\nA method no real proto has"
    ));
    let ran = rig.run(
        Script::Real,
        &args(&["widgets", "brand-new-method"]),
        b"",
        &[],
    );
    assert_eq!(ran.code, 2);
    let ran = rig.run(Script::Fixture, &args(&["beats", "beat"]), b"", &[]);
    assert_eq!(
        (ran.code, ran.stderr.as_str()),
        (2, "hkctl: no service beats; 'hkctl --help' lists them\n")
    );
}
