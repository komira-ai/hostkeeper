//! `hk ssh`, `ssh-proxy`, `ssh-revoke`, `ssh-status` and `ssh-known-host`: the global flags `hk`
//! takes and checks, and what it hands to the ssh module: the verb, the coordinator's flags (the
//! path of the token file, never the token), the home, and the rest of the words in order.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt as _;

use hostkeeper_hk::ssh::Verb;

use crate::rig::{Opts, Recorder, Rig, Server, TOKEN, args};

/// Proves: each of the four commands hands the ssh module its verb, the coordinator's flags it
/// took (the endpoint without its trailing `/`, the CA file, the token file's path, the timeout,
/// 30 by default), the home and the rest of its words in order (a `--` and the ssh arguments after
/// it included, one that is not text too, and a flag after the `--` untouched); the globals given
/// after the command word are taken, and the token is in no argument; no gateway call is made.
#[test]
fn each_command_hands_the_ssh_module_the_flags_it_took() {
    let rig = Rig::new("ssh-hand");
    let server = Server::canned(&rig.pki, 200, "{}");
    let globals = rig.globals(server.port);
    let cases: [(&[&str], Verb, &[&str]); 6] = [
        (
            &["ssh", "host-7", "--", "uname", "-a"],
            Verb::Ssh,
            &["host-7", "--", "uname", "-a"],
        ),
        (
            &["ssh", "host-7", "--identity", "/k/id", "--reason", "fix"],
            Verb::Ssh,
            &["host-7", "--identity", "/k/id", "--reason", "fix"],
        ),
        (&["ssh-proxy", "host-7"], Verb::Proxy, &["host-7"]),
        (
            &["ssh-revoke", "--serial", "5"],
            Verb::Revoke,
            &["--serial", "5"],
        ),
        (&["ssh-status", "host-7"], Verb::Status, &["host-7"]),
        (
            &["ssh", "host-7", "--", "--endpoint", "x", "--help"],
            Verb::Ssh,
            &["host-7", "--", "--endpoint", "x", "--help"],
        ),
    ];
    for (words, verb, expected) in cases {
        let recorder = Recorder::default();
        let mut line = globals.clone();
        line.extend(args(words));
        let ran = rig.run(Opts::real().ssh(&recorder), &line);
        assert_eq!((ran.code, ran.stderr.as_str()), (0, ""), "{words:?}");
        let calls = recorder.calls.lock().expect("the calls");
        assert_eq!(calls.len(), 1, "{words:?}");
        let call = &calls[0];
        assert_eq!(call.verb, verb, "{words:?}");
        assert_eq!(
            call.coordinator.endpoint,
            format!("https://127.0.0.1:{}", server.port)
        );
        assert_eq!(call.coordinator.ca_file, rig.path("ca.pem"));
        assert_eq!(call.coordinator.token_file, rig.path("token"));
        assert_eq!(call.coordinator.timeout_secs, 30);
        assert_eq!(
            call.args,
            expected.iter().map(OsString::from).collect::<Vec<_>>()
        );
        assert_eq!((call.name.as_str(), &call.home), ("hk", &rig.path("home")));
        let shown = format!("{call:?}");
        assert!(!shown.contains(TOKEN), "{words:?}");
    }
    assert!(server.seen().is_empty());

    // Globals after the command word, and a trailing slash on the endpoint.
    let recorder = Recorder::default();
    let mut line = args(&["ssh-status", "--timeout-secs", "9", "host-7", "--endpoint"]);
    line.push(format!("https://127.0.0.1:{}/", server.port));
    line.extend(args(&["--ca-file"]));
    line.push(rig.path("ca.pem").display().to_string());
    line.extend(args(&["--token-file"]));
    line.push(rig.path("token").display().to_string());
    let ran = rig.run(Opts::real().ssh(&recorder), &line);
    assert_eq!(ran.code, 0, "{ran:?}");
    let calls = recorder.calls.lock().expect("the calls");
    assert_eq!(calls[0].coordinator.timeout_secs, 9);
    assert_eq!(
        calls[0].coordinator.endpoint,
        format!("https://127.0.0.1:{}", server.port)
    );
    assert_eq!(calls[0].args, [OsString::from("host-7")]);

    // The same words after the service `ssh`, which takes the host id for a method it has not.
    let recorder = Recorder::default();
    let mut line = globals.clone();
    line.extend(args(&[
        "ssh",
        "--timeout-secs",
        "7",
        "host-8",
        "--reason",
        "r",
    ]));
    let ran = rig.run(Opts::real().ssh(&recorder), &line);
    assert_eq!(ran.code, 0, "{ran:?}");
    let calls = recorder.calls.lock().expect("the calls");
    assert_eq!(
        (calls[0].verb, calls[0].coordinator.timeout_secs),
        (Verb::Ssh, 7)
    );
    assert_eq!(
        calls[0].args,
        args(&["host-8", "--reason", "r"])
            .iter()
            .map(OsString::from)
            .collect::<Vec<_>>()
    );
}

/// Proves: the exit code of the ssh module is `hk`'s, what it writes to stderr is `hk`'s, a name
/// of `hkctl` is handed to it as the name of its diagnostics, and an argument that is not text
/// reaches it byte for byte.
#[test]
fn the_exit_code_the_name_and_the_bytes_of_the_ssh_module_are_hks() {
    let rig = Rig::new("ssh-code");
    let recorder = Recorder {
        code: 3,
        says: "hkctl: nothing doing\n",
        ..Recorder::default()
    };
    let mut line: Vec<OsString> = rig.globals(1).into_iter().map(OsString::from).collect();
    line.push("ssh".into());
    line.push("host-7".into());
    line.push("--".into());
    line.push(OsString::from_vec(b"a\xffb".to_vec()));
    let ran = rig.run_os(Opts::real().named("hkctl").ssh(&recorder), &line);
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (3, "", "hkctl: nothing doing\n")
    );
    let calls = recorder.calls.lock().expect("the calls");
    assert_eq!(calls[0].name, "hkctl");
    assert_eq!(calls[0].args[2], OsString::from_vec(b"a\xffb".to_vec()));
}

/// Proves: a word after `ssh` that is a method of the service (`revoke`, `host-keys`,
/// `sign-user-cert`) is still the gateway call and does not reach the ssh module; only another
/// word is a host id; and the fixture's table, which has no service `ssh`, has no command `ssh`.
#[test]
fn the_methods_of_the_service_ssh_stay_gateway_calls() {
    let rig = Rig::new("ssh-methods");
    let server = Server::canned(&rig.pki, 200, "{\"revoked\":\"1\"}");
    let recorder = Recorder::default();
    let mut line = rig.globals(server.port);
    line.extend(args(&["ssh", "revoke", "--serial", "5"]));
    let ran = rig.run(Opts::real().ssh(&recorder), &line);
    assert_eq!(
        (ran.code, ran.stdout.as_str()),
        (0, "{\"revoked\":\"1\"}\n")
    );
    assert!(recorder.calls.lock().expect("the calls").is_empty());
    assert_eq!(server.seen().len(), 1);
    let ran = rig.fx(&args(&["ssh", "host-7"]));
    assert_eq!(
        (ran.code, ran.stderr.as_str()),
        (2, "hk: no service ssh; 'hk --help' lists them\n")
    );
    // `ssh` alone, and `ssh --help`, are the service's help.
    let ran = rig.hk(&args(&["ssh"]));
    assert_eq!((ran.code, ran.stdout.as_str()), (2, ""));
    assert!(
        ran.stderr.starts_with("hk ssh: the service `Ssh`"),
        "{}",
        ran.stderr
    );
    let ran = rig.hk(&args(&["ssh", "--help"]));
    assert_eq!(ran.code, 0);
    assert!(
        ran.stdout.starts_with("hk ssh: the service `Ssh`"),
        "{}",
        ran.stdout
    );
}

/// Proves: the usage errors of the ssh commands, each said exactly before the ssh module runs: a
/// credential other than a token file, a missing global flag, a bad endpoint and a missing or
/// empty home, with the name `hkctl` in the messages that name the program; and `--help` prints
/// what the five commands are, whichever word asks.
#[test]
fn the_commands_refuse_what_they_cannot_take_and_say_what_they_are() {
    let rig = Rig::new("ssh-usage");
    let recorder = Recorder::default();
    let with = |extra: &[&str]| {
        let mut line = rig.globals(4443);
        line.extend(args(extra));
        line
    };
    for flag in [
        "--token-stdin",
        "--response-file=/x",
        "--github-oidc-audience=a",
    ] {
        let mut line = rig.globals(4443);
        line.truncate(4);
        line.extend(args(&[flag, "ssh-status", "h"]));
        let ran = rig.run(Opts::real().ssh(&recorder), &line);
        assert_eq!(
            (ran.code, ran.stderr.as_str()),
            (
                2,
                "hk: hk ssh-status takes --token-file only: --token-stdin, --github-oidc-audience and --response-file are not for it\n"
            ),
            "{flag}"
        );
    }
    let ran = rig.run(
        Opts::real().named("hkctl").ssh(&recorder),
        &args(&["ssh-status", "h"]),
    );
    assert_eq!(
        (ran.code, ran.stderr.as_str()),
        (
            2,
            "hkctl: hkctl ssh-status needs --endpoint, --ca-file and --token-file\n"
        )
    );
    let ran = rig.run(Opts::real().ssh(&recorder), &args(&["ssh-proxy", "h"]));
    assert_eq!(
        ran.stderr,
        "hk: hk ssh-proxy needs --endpoint, --ca-file and --token-file\n"
    );
    let mut line = with(&["ssh-status", "h"]);
    line[1] = "http://127.0.0.1:4443".to_owned();
    let ran = rig.run(Opts::real().ssh(&recorder), &line);
    assert_eq!(
        (ran.code, ran.stderr.as_str()),
        (2, "hk: --endpoint takes https://<host>[:<port>]\n")
    );
    for opts in [
        Opts::real().without_home(),
        Opts::real().home(std::path::Path::new("")),
    ] {
        let ran = rig.run(opts.ssh(&recorder), &with(&["ssh-status", "h"]));
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
            (
                1,
                "",
                "hk: hk ssh-status keeps its key and certificates under $HOME, which is not set\n"
            )
        );
    }
    let ran = rig.run(
        Opts::real().named("hkctl").without_home().ssh(&recorder),
        &with(&["ssh", "h"]),
    );
    assert_eq!(
        ran.stderr,
        "hkctl: hkctl ssh keeps its key and certificates under $HOME, which is not set\n"
    );
    // A global flag of the command needs its value too.
    let ran = rig.run(
        Opts::real().ssh(&recorder),
        &args(&["ssh-status", "h", "--endpoint"]),
    );
    assert_eq!(
        (ran.code, ran.stderr.as_str()),
        (2, "hk: --endpoint needs a value\n")
    );
    assert!(recorder.calls.lock().expect("the calls").is_empty());

    for words in [
        vec!["ssh-proxy", "--help"],
        vec!["ssh-status", "h", "-h"],
        vec!["ssh-revoke", "--serial", "5", "--help"],
        vec!["ssh", "h", "--help"],
    ] {
        let ran = rig.run(Opts::real().ssh(&recorder), &args(&words));
        assert_eq!((ran.code, ran.stderr.as_str()), (0, ""), "{words:?}");
        assert!(
            ran.stdout
                .starts_with("hk ssh <host-id> [flags] [-- ssh args]\n"),
            "{}",
            ran.stdout
        );
        assert!(ran.stdout.contains("\nhk ssh-proxy <host-id>\n"));
        assert!(
            ran.stdout
                .contains("\nhk ssh-known-host <pins-file> %H %t %K %f\n")
        );
        assert!(ran.stdout.ends_with("are not taken.\n"));
        assert!(!ran.stdout.contains("hkssh"));
    }
    assert!(recorder.calls.lock().expect("the calls").is_empty());
}

/// Proves: `hk ssh-known-host <pins> %H %t %K %f`, the command ssh runs, takes no global flag and
/// prints the host's key line for a pinned fingerprint and nothing for another.
#[test]
fn ssh_known_host_vouches_for_a_pinned_fingerprint_only() {
    let rig = Rig::new("known-host");
    let pins = rig.put("pins", b"SHA256:abc\n", 0o600);
    let ran = rig.hk(&args(&[
        "ssh-known-host",
        &pins,
        "h",
        "ssh-ed25519",
        "AAAA",
        "SHA256:abc",
    ]));
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (0, "h ssh-ed25519 AAAA\n", "")
    );
    let ran = rig.hk(&args(&[
        "ssh-known-host",
        &pins,
        "h",
        "ssh-ed25519",
        "AAAA",
        "SHA256:other",
    ]));
    assert_eq!((ran.code, ran.stdout.as_str()), (0, ""));
}

/// Proves: `hk ssh-known-host` with no argument at all is a usage error (exit 2, nothing on
/// stdout), while one with a pins file and fewer than the four words of ssh stays silent, exit 0.
#[test]
fn ssh_known_host_with_no_argument_is_a_usage_error() {
    let rig = Rig::new("known-host-none");
    let ran = rig.hk(&args(&["ssh-known-host"]));
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (2, "", "hk: ssh-known-host needs <pins-file> %H %t %K %f\n")
    );
    let ran = rig.hk(&args(&["ssh-known-host", "pins"]));
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (0, "", "")
    );
}
