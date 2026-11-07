//! `push-cert` through the command line: flags, grants and exit codes, on the fake host.

mod common;
mod push_cert_support;

use common::{FakeApi, FakeMachine, FakeNet, example, with_system_api};
use hostkeeper_mdm::api::Method;
use hostkeeper_mdm::run;
use hostkeeper_mdm_step::Host;
use hostkeeper_mdm_step::fake::FakeHost;
use push_cert_support::*;

fn args(words: &[&str]) -> Vec<String> {
    std::iter::once("hostkeeper-mdm")
        .chain(words.iter().copied())
        .map(str::to_owned)
        .collect()
}

/// Runs the command line `words`; the exit code, standard output, standard error.
fn cli(host: &FakeHost, api: &FakeApi, words: &[&str]) -> (u8, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let machine = FakeMachine::new(host);
    let net = FakeNet::new(Default::default());
    let code = with_system_api(&machine, &net, api, |system| {
        run(&args(words), system, &mut out, &mut err)
    });
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

/// Catches flag combinations push-cert cannot act on being accepted: `--cert` without `--key`
/// and the reverse, `--cert` with `--profile-only`, `--topic` without `--profile-only`, a topic
/// that is not one, and neither the pair nor `--profile-only`. Each exits 2 with clap's message
/// naming the flags, and nothing is printed to standard output.
#[test]
fn conflicting_flags_exit_2_with_the_parsers_message() {
    let host = FakeHost::new(0);
    for (words, said) in [
        (
            &["push-cert", "--settings", "/c", "--cert", "/c.pem"][..],
            "the following required arguments were not provided:\n  --key <KEY>",
        ),
        (
            &["push-cert", "--settings", "/c", "--key", "/k.pem"],
            "the following required arguments were not provided:\n  --cert <CERT>",
        ),
        (
            &[
                "push-cert",
                "--settings",
                "/c",
                "--cert",
                "/c.pem",
                "--key",
                "/k.pem",
                "--profile-only",
            ],
            "the argument '--cert <CERT>' cannot be used with '--profile-only'",
        ),
        (
            &["push-cert", "--settings", "/c", "--topic", TOPIC],
            "the following required arguments were not provided:\n  --profile-only",
        ),
        (
            &[
                "push-cert",
                "--settings",
                "/c",
                "--profile-only",
                "--topic",
                "nope",
            ],
            "topic \"nope\" is not com.apple.mgmt.External.<uuid>",
        ),
        (
            &["push-cert", "--settings", "/c"],
            "the following required arguments were not provided:\n  --profile-only",
        ),
    ] {
        let (code, out, err) = cli(&host, &FakeApi::new(), words);
        assert_eq!((code, out.as_str()), (2, ""), "{words:?}: {err}");
        assert!(err.starts_with("error: "), "{words:?}: {err}");
        assert!(err.contains(said), "{words:?}: {err}");
    }
}

/// Catches push-cert not wired to its module: through the command line a dry run prints the
/// plan and makes no call, `--apply` uploads and writes the records and the profile (the key
/// and certificate files are in a directory of their own, outside the state), and `--profile-only
/// --topic` then rewrites nothing; a missing settings file is refused (exit 2) and a NanoHUB
/// that refuses fails the run (exit 1).
#[test]
fn push_cert_runs_through_the_command_line() {
    let p = push_pair();
    let host = host(&p);
    host.put_file("/cfg", 0o644, (0, 0), example().as_bytes());
    let api = FakeApi::new();
    api.reply(
        Method::Put,
        PUSHCERT,
        200,
        format!("{{\"topic\":\"{TOPIC}\"}}").as_bytes(),
    );
    let pair = ["--cert", CERT, "--key", KEY];
    let words = |extra: &[&'static str]| {
        let mut w = vec!["push-cert", "--settings", "/cfg"];
        w.extend_from_slice(&pair);
        w.extend_from_slice(extra);
        w
    };
    let (code, out, err) = cli(&host, &api, &words(&[]));
    assert_eq!((code, err.as_str()), (0, ""), "{out}");
    assert!(out.contains("DRY-RUN: PUT http://127.0.0.1:9004"), "{out}");
    assert!(api.sent().is_empty());
    assert!(host.node(PROFILE).is_none());

    let (code, out, err) = cli(&host, &api, &words(&["--apply"]));
    assert_eq!((code, err.as_str()), (0, ""), "{out}");
    assert_eq!(api.sent().len(), 1);
    assert_eq!(
        host.node(TOPIC_FILE).unwrap().bytes,
        format!("{TOPIC}\n").into_bytes()
    );
    assert_eq!(host.node(PROFILE).unwrap().mode, 0o640);

    let (code, out, err) = cli(
        &host,
        &api,
        &[
            "push-cert",
            "--settings",
            "/cfg",
            "--profile-only",
            "--topic",
            TOPIC,
        ],
    );
    assert_eq!((code, err.as_str()), (0, ""), "{out}");
    assert!(out.ends_with("nothing to do\n"), "{out}");

    let (code, out, err) = cli(
        &host,
        &api,
        &["push-cert", "--settings", "/nope", "--profile-only"],
    );
    assert_eq!((code, out.as_str()), (2, ""));
    assert_eq!(err, "REFUSED: /nope is not there\n");

    host.remove(SUM_FILE).unwrap();
    let refusing = FakeApi::new();
    refusing.reply(Method::Put, PUSHCERT, 500, b"no");
    let (code, _, err) = cli(&host, &refusing, &words(&["--apply"]));
    assert_eq!(code, 1);
    assert!(
        err.starts_with("FAILED: PUT http://127.0.0.1:9004"),
        "{err}"
    );
}
