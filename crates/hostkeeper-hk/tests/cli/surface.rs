//! The surface of `hk`: the hold of a `wait_secs` field, the help and the version, the names of
//! services and methods, the usage errors of the global flags, and a new RPC of the fixture,
//! which appears as a command with no hand edit.

use crate::rig::{Opts, Rig, STAMP, Server, args, call};

/// Proves: `--version` prints `<name> <stamp>`, help is printed to stdout with exit 0 at every
/// level, with the words of the command table in it and the host API not in it; no words, a
/// service alone, and global flags alone print the help to stderr with exit 2; a service or
/// method that is no command is said, and `--version` after the method is the method's field.
#[test]
fn version_help_and_names_are_answered_without_a_call() {
    let rig = Rig::new("surface");
    let ran = rig.hk(&args(&["--version"]));
    assert_eq!(
        (ran.code, ran.stdout, ran.stderr),
        (0, format!("hk {STAMP}\n"), String::new())
    );
    for words in [
        vec!["--help"],
        vec!["-h"],
        vec!["releases", "--help"],
        vec!["releases", "-h"],
        vec!["releases", "set-desired", "--help"],
        vec!["releases", "set-desired", "-h"],
        vec!["releases", "set-desired", "--help=x"],
        vec![
            "--endpoint",
            "https://coordinator.example.com",
            "hosts",
            "get",
            "-h",
        ],
    ] {
        let ran = rig.hk(&args(&words));
        assert_eq!((ran.code, ran.stderr.as_str()), (0, ""), "{words:?}");
        assert!(ran.stdout.starts_with("hk "), "{words:?}: {}", ran.stdout);
    }
    let ran = rig.hk(&args(&["releases", "set-desired", "--help"]));
    assert!(
        ran.stdout
            .starts_with("hk releases set-desired: /hostkeeper.operator.v1.Releases/SetDesired\n")
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
    let ran = rig.hk(&args(&["releases", "--help"]));
    assert!(
        ran.stdout
            .starts_with("hk releases: the service `Releases` of the package")
    );
    assert!(ran.stdout.contains("\nMethods:\n  set-desired\n"));
    let ran = rig.hk(&args(&["--help"]));
    assert!(
        ran.stdout
            .starts_with(&format!("hk {STAMP}: call one method"))
    );
    assert!(ran.stdout.contains("\n  --github-oidc-audience AUDIENCE\n"));
    assert!(ran.stdout.contains(
        "\n  3  the coordinator refused the caller: UNAUTHENTICATED or PERMISSION_DENIED\n"
    ));
    assert!(ran.stdout.contains("\nServices:\n  alerts\n"));
    assert!(
        !ran.stdout.contains("\n  coordinator\n"),
        "the host API has no command"
    );
    for gone in ["bash", "curl 7", "hkssh"] {
        assert!(
            !ran.stdout.replace("no bash, no curl", "").contains(gone),
            "{gone}"
        );
    }

    // Nothing to run: the help on stderr, exit 2.
    for words in [
        vec![],
        vec!["hosts"],
        vec!["--endpoint", "https://coordinator.example.com"],
    ] {
        let ran = rig.hk(&args(&words));
        assert_eq!((ran.code, ran.stdout.as_str()), (2, ""), "{words:?}");
        assert!(ran.stderr.starts_with("hk "), "{words:?}");
    }
    for (words, why) in [
        (vec!["nosuch"], "no service nosuch; 'hk --help' lists them"),
        (
            vec!["coordinator", "heartbeat"],
            "no service coordinator; 'hk --help' lists them",
        ),
        (vec![""], "no service ; 'hk --help' lists them"),
        (
            vec!["nosuch", "--help"],
            "no service nosuch; 'hk --help' lists them",
        ),
        (
            vec!["hosts", "nosuch"],
            "the service hosts has no method nosuch; 'hk hosts --help' lists them",
        ),
        (vec!["--nosuch=secret", "hosts"], "unknown flag --nosuch"),
        (vec!["hosts", "--nosuch", "get"], "unknown flag --nosuch"),
        (vec!["--version=1"], "unknown flag --version"),
        (vec!["-x"], "unknown flag -x"),
        (vec!["--"], "unknown flag --"),
        (vec!["hosts", "--"], "unknown flag --"),
        (vec!["--help=x"], "unknown flag --help"),
        (
            vec!["hosts", "get", "--version"],
            "hosts get has no flag --version",
        ),
        (
            vec!["hosts", "get", "stray"],
            "hosts get takes no argument; it has flags only",
        ),
        (
            vec!["hosts", "get", "-x"],
            "hosts get takes no argument; it has flags only",
        ),
    ] {
        let ran = rig.hk(&args(&words));
        assert_eq!(
            (ran.code, ran.stderr),
            (2, format!("hk: {why}\n")),
            "{words:?}"
        );
    }

    // After the method, --version is the field of that name.
    let server = Server::canned(&rig.pki, 200, "{}");
    let ran = rig.fx(&call(
        &rig,
        server.port,
        &["gadgets", "tune", "--version", "1.0"],
    ));
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(server.seen()[0].text(), "{\"version\":\"1.0\"}");
}

/// Proves: a program called `hkctl` prints `hkctl` before its diagnostics, in the first line of
/// `--version`, and in the words of its messages that name the program; and its help says `hk`.
#[test]
fn the_old_name_is_the_name_in_messages_and_the_version() {
    let rig = Rig::new("old-name");
    let ran = rig.run(Opts::real().named("hkctl"), &args(&["--version"]));
    assert_eq!(
        (ran.code, ran.stdout, ran.stderr),
        (0, format!("hkctl {STAMP}\n"), String::new())
    );
    for (words, why) in [
        (
            vec!["nosuch"],
            "no service nosuch; 'hkctl --help' lists them",
        ),
        (
            vec!["hosts", "nosuch"],
            "the service hosts has no method nosuch; 'hkctl hosts --help' lists them",
        ),
        (vec!["--nosuch"], "unknown flag --nosuch"),
    ] {
        let ran = rig.run(Opts::real().named("hkctl"), &args(&words));
        assert_eq!(
            (ran.code, ran.stderr),
            (2, format!("hkctl: {why}\n")),
            "{words:?}"
        );
    }
    let ran = rig.run(Opts::real().named("hkctl"), &args(&["--help"]));
    assert!(ran.stdout.starts_with(&format!("hk {STAMP}: ")));
    assert!(ran.stdout.contains("Usage: hk [global flags]"));
}

/// Proves: a call made without what it needs (an endpoint that is https, a readable CA file,
/// exactly one credential), a global flag given twice, a timeout that is no whole number of
/// seconds, and two inputs both reading stdin are each a usage error, said exactly, before any
/// request.
#[test]
fn the_global_flags_usage_errors_end_the_run_before_any_call() {
    let rig = Rig::new("global-errors");
    let server = Server::canned(&rig.pki, 200, "{}");
    let ca = rig.path("ca.pem").display().to_string();
    let token = rig.path("token").display().to_string();
    let missing = rig.path("missing").display().to_string();
    let directory = rig.path("").display().to_string();
    let call = ["gadgets", "list"];
    let cases: [(Vec<&str>, String); 18] = [
        (
            vec!["--ca-file", &ca, "--token-file", &token],
            "a call needs --endpoint".to_owned(),
        ),
        (
            vec![
                "--endpoint",
                "http://coordinator.example.com",
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
                "https://runner.example.com/path",
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
                "https://coordinator.example.com",
                "--token-file",
                &token,
            ],
            "a call needs --ca-file".to_owned(),
        ),
        (
            vec![
                "--endpoint",
                "https://coordinator.example.com",
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
                "https://coordinator.example.com",
                "--ca-file",
                &directory,
                "--token-file",
                &token,
            ],
            format!("cannot read --ca-file {directory}"),
        ),
        (
            vec![
                "--endpoint",
                "https://coordinator.example.com",
                "--ca-file",
                &ca,
            ],
            "a call needs exactly one of --token-file, --token-stdin and --github-oidc-audience"
                .to_owned(),
        ),
        (
            vec![
                "--endpoint",
                "https://coordinator.example.com",
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
            vec![
                "--endpoint",
                "https://coordinator.example.com",
                "--endpoint",
                "https://other.example.com",
            ],
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
            vec![
                "--endpoint",
                "https://user@coordinator.example.com",
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
                "https://[::1",
                "--ca-file",
                &ca,
                "--token-file",
                &token,
            ],
            "--endpoint takes https://<host>[:<port>]".to_owned(),
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
        let ran = rig.fx(&words);
        assert_eq!(
            (ran.code, ran.stderr),
            (2, format!("hk: {why}\n")),
            "{globals:?}"
        );
    }
    // An empty response file path, two inputs reading stdin, and a switch given a value.
    let mut words = rig.globals(server.port);
    words.extend(args(&["--response-file=", "gadgets", "list"]));
    let ran = rig.fx(&words);
    assert_eq!(
        (ran.code, ran.stderr.as_str()),
        (2, "hk: --response-file needs a path\n")
    );
    let words = args(&["--token-stdin", "widgets", "make", "--request", "-"]);
    let ran = rig.fx(&words);
    assert_eq!(
        (ran.code, ran.stderr.as_str()),
        (
            2,
            "hk: --token-stdin and --request both read stdin; at most one input may\n"
        )
    );
    let words = args(&["--token-stdin=x", "gadgets", "list"]);
    let ran = rig.fx(&words);
    assert_eq!(
        (ran.code, ran.stderr.as_str()),
        (2, "hk: --token-stdin takes no value\n")
    );
    // A global flag needs its value, whichever level it is at.
    for words in [
        vec!["--endpoint"],
        vec!["gadgets", "--ca-file", "--help"],
        vec!["gadgets", "list", "--token-file"],
    ] {
        let ran = rig.fx(&args(&words));
        assert_eq!(ran.code, 2, "{words:?}");
        assert!(
            ran.stderr.ends_with(" needs a value\n"),
            "{words:?}: {}",
            ran.stderr
        );
    }
    // At every level a global flag is taken: before the service, between it and the method, and
    // among the method's flags.
    for words in [
        vec!["--timeout-secs", "5", "gadgets", "list"],
        vec!["gadgets", "--timeout-secs", "5", "list"],
        vec!["gadgets", "list", "--timeout-secs", "5"],
    ] {
        let server = Server::canned(&rig.pki, 200, "{}");
        let mut all = rig.globals(server.port);
        all.extend(args(&words));
        let ran = rig.fx(&all);
        assert_eq!(
            (ran.code, ran.stdout.as_str()),
            (0, "{}\n"),
            "{words:?}: {}",
            ran.stderr
        );
    }
    assert!(server.seen().is_empty());
}

/// Proves: a command appears with no hand edit: the fixture's `BrandNewMethod`, which no real
/// proto has, is a command of the fixture's table that posts to its path and has its help, and
/// is not a command of the real table; and the fixture's host API package has no command.
#[test]
fn a_new_rpc_in_the_fixture_protos_is_a_command_with_no_edit() {
    let rig = Rig::new("new-rpc");
    let server = Server::canned(&rig.pki, 200, "{}");
    let ran = rig.fx(&call(&rig, server.port, &["widgets", "brand-new-method"]));
    assert_eq!((ran.code, ran.stdout.as_str()), (0, "{}\n"));
    assert_eq!(
        server.seen()[0].target,
        "/hostkeeper.fixture.v1.Widgets/BrandNewMethod"
    );
    let ran = rig.fx(&args(&["widgets", "brand-new-method", "--help"]));
    assert!(ran.stdout.starts_with(
        "hk widgets brand-new-method: /hostkeeper.fixture.v1.Widgets/BrandNewMethod\n\nA method no real proto has"
    ));
    let ran = rig.hk(&args(&["widgets", "brand-new-method"]));
    assert_eq!(ran.code, 2);
    let ran = rig.fx(&args(&["beats", "beat"]));
    assert_eq!(
        (ran.code, ran.stderr.as_str()),
        (2, "hk: no service beats; 'hk --help' lists them\n")
    );
}

/// Proves: a field named `wait_secs` does not change what is sent (the deadline is the
/// transport's, and its value is the unit test of `total_secs`): the call is made with the hold
/// as the field's value, from a flag or from the request file, and a value that is no whole
/// number is sent as it is for the gateway to judge.
#[test]
fn a_wait_secs_field_is_sent_as_given() {
    let rig = Rig::new("hold");
    let request = rig.put(
        "wait.json",
        b"{\"service\":\"s\", \"waitSecs\": \"60\" }",
        0o644,
    );
    let odd = rig.put("odd.json", b"{\"wait_secs\": -1}", 0o644);
    let cases: [(Vec<&str>, &str); 5] = [
        (vec!["--wait-secs", "1800"], "{\"waitSecs\":1800}"),
        (
            vec!["--timeout-secs", "5", "--wait-secs", "1800"],
            "{\"waitSecs\":1800}",
        ),
        (
            vec!["--request", &request],
            "{\"service\":\"s\", \"waitSecs\": \"60\"}",
        ),
        (vec!["--request", &odd], "{\"wait_secs\": -1}"),
        (vec![], "{}"),
    ];
    for (words, body) in cases {
        let server = Server::canned(&rig.pki, 200, "{}");
        let mut all = call(&rig, server.port, &["widgets", "wait"]);
        all.extend(args(&words));
        let ran = rig.fx(&all);
        assert_eq!(ran.code, 0, "{ran:?}");
        assert_eq!(server.seen()[0].text(), body, "{words:?}");
    }
}

/// Proves: no request field of either table takes a flag that `hk` keeps for itself (the global
/// flags `--github-oidc-audience` and `--response-file`, `--request`, and the ones the generator
/// reserves), which would make the field or the global flag unreachable at the method.
#[test]
fn no_field_takes_one_of_hks_own_flags() {
    use hostkeeper_proto::ctl::Input;
    let own = [
        "github-oidc-audience",
        "response-file",
        "request",
        "endpoint",
        "ca-file",
        "token-file",
        "token-stdin",
        "timeout-secs",
        "help",
    ];
    let mut checked = 0;
    for table in [
        hostkeeper_proto::ctl::COMMANDS,
        crate::rig::fixture::COMMANDS,
    ] {
        for command in table.iter().filter(|c| hostkeeper_hk::is_served(c)) {
            for field in command.fields {
                let taken = match field.input {
                    Input::Flag => vec![field.flag.to_owned()],
                    Input::SecretFile => vec![
                        format!("{}-file", field.flag),
                        format!("{}-stdin", field.flag),
                    ],
                    Input::RequestFile => Vec::new(),
                };
                for flag in taken {
                    checked += 1;
                    assert!(
                        !own.contains(&flag.as_str()),
                        "{} {}: --{flag}",
                        command.service_command,
                        command.method_command
                    );
                }
            }
        }
    }
    assert!(checked > 100, "{checked}");
}
