//! What the command line and the coordinator flags refuse, and the program as a process.

use std::os::unix::fs::PermissionsExt as _;
use std::process::Command;

use clap::Parser as _;
use hostkeeper_hkssh::Cli;

use crate::fake::{Double, HOST, NOW, Rig, invoke, pki, write_private};

fn status(args: &[String]) -> Result<i32, hostkeeper_hkssh::Error> {
    let mut all = args.to_vec();
    all.extend(["status".to_owned(), HOST.to_owned()]);
    invoke(&all, &Double::new(), NOW, Vec::new()).result
}

fn flags(rig: &Rig, without: &str) -> Vec<String> {
    let all = rig.args(&[]);
    let mut kept = vec![all[0].clone()];
    let mut pairs = all[1..].chunks(2);
    for pair in pairs.by_ref() {
        if pair[0] != without {
            kept.extend(pair.iter().cloned());
        }
    }
    kept
}

/// Proves: each flag a call needs is asked for by name when it is missing, as a usage error
/// (exit 2), and none is read from the environment.
#[test]
fn a_missing_flag_is_a_usage_error_naming_it() {
    let rig = Rig::start();
    for flag in ["--endpoint", "--ca-file", "--token-file", "--state-dir"] {
        let error = status(&flags(&rig, flag)[..]).expect_err(flag);
        assert_eq!(
            (error.to_string(), error.exit_code()),
            (format!("this command needs {flag}"), 2)
        );
    }
}

/// Proves: only `https://<host>[:<port>]` is an endpoint: a scheme other than https, a user, a
/// path, a query or a trailing slash is refused, an IPv6 address in brackets is taken.
#[test]
fn an_endpoint_is_https_host_and_port_only() {
    let rig = Rig::start();
    for bad in [
        "http://localhost:1",
        "https://u@localhost",
        "https://localhost/p",
        "https://localhost/",
        "https://localhost?q=1",
        "localhost",
        "https://",
    ] {
        let mut args = flags(&rig, "--endpoint");
        args.extend(["--endpoint".to_owned(), bad.to_owned()]);
        let error = status(&args).expect_err(bad);
        assert_eq!(
            (error.to_string(), error.exit_code()),
            ("--endpoint takes https://<host>[:<port>]".to_owned(), 2),
            "{bad}"
        );
    }
    let mut args = flags(&rig, "--endpoint");
    args.extend(["--endpoint".to_owned(), "https://[::1]:1".to_owned()]);
    let error = status(&args).expect_err("nothing listens");
    assert!(
        error
            .to_string()
            .starts_with("no answer from the coordinator"),
        "{error}"
    );
}

/// Proves: the token file is refused when group or others can read it, when it is missing, empty,
/// or holds a character that is not visible ASCII; the refusal never prints the token.
#[test]
fn a_token_file_must_be_private_and_a_visible_word() {
    let rig = Rig::start();
    let token = rig.dir().join("token");
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o640)).expect("mode");
    let error = status(&rig.args(&[])).expect_err("readable");
    assert!(
        error
            .to_string()
            .ends_with("may be read by its group or others; make it 0600"),
        "{error}"
    );
    for (content, label) in [
        ("\n", "empty"),
        ("two words\n", "a space"),
        ("t\u{f6}ken\n", "non-ASCII"),
    ] {
        write_private(&token, content);
        let error = status(&rig.args(&[])).expect_err(label);
        assert_eq!(
            error.to_string(),
            "the token is empty or holds a character that is not visible ASCII",
            "{label}"
        );
    }
    write_private(&token, "no-newline");
    // A token that is fine goes on to the call, which the fake answers.
    assert_eq!(status(&rig.args(&[])).expect("answered"), 0);
    std::fs::remove_file(&token).expect("remove");
    let error = status(&rig.args(&[])).expect_err("missing");
    assert!(
        error.to_string().starts_with("cannot read the token from"),
        "{error}"
    );
}

/// Proves: a CA file that is missing is an error naming it; a CA that did not sign the
/// coordinator's certificate, and a coordinator that is not listening, are "no answer", and the
/// token is not sent to either.
#[test]
fn a_wrong_ca_or_no_listener_is_no_answer() {
    let rig = Rig::start();
    let ca = rig.dir().join("ca.pem");
    std::fs::write(&ca, pki().ca_pem).expect("another CA");
    let error = status(&rig.args(&[])).expect_err("untrusted");
    assert!(
        error
            .to_string()
            .starts_with("no answer from the coordinator"),
        "{error}"
    );
    assert_eq!(rig.seen(|s| s.authorizations.len()), 0);
    std::fs::write(&ca, "not a certificate").expect("junk");
    let error = status(&rig.args(&[])).expect_err("junk");
    let said = error.to_string();
    assert!(
        said.starts_with("no answer from the coordinator")
            || said.starts_with("--ca-file is not a CA certificate"),
        "{said}"
    );
    std::fs::write(
        &ca,
        "-----BEGIN CERTIFICATE-----\n%%%%\n-----END CERTIFICATE-----\n",
    )
    .expect("malformed");
    let error = status(&rig.args(&[])).expect_err("malformed");
    assert!(
        error
            .to_string()
            .starts_with("--ca-file is not a CA certificate"),
        "{error}"
    );
    assert_eq!(error.exit_code(), 2);
    std::fs::remove_file(&ca).expect("remove");
    let error = status(&rig.args(&[])).expect_err("missing");
    assert!(
        error.to_string().starts_with("cannot read the CA from"),
        "{error}"
    );
}

/// Proves: `revoke` needs exactly one of an operator, a serial and a host, and a host id is a
/// plain word wherever it is taken.
#[test]
fn revoke_takes_exactly_one_target() {
    for args in [
        vec!["hkssh", "revoke"],
        vec!["hkssh", "revoke", "--operator", "a", "--serial", "1"],
        vec!["hkssh", "revoke", "--serial", "1", "--host", "h"],
        vec!["hkssh", "revoke", "--operator", "a", "--host", "h"],
        vec!["hkssh", "--timeout-secs", "0", "status", "h"],
    ] {
        assert!(Cli::try_parse_from(&args).is_err(), "{args:?}");
    }
}

fn hkssh(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_hkssh"))
        .args(args)
        .output()
        .expect("hkssh runs")
}

/// Proves: the program is the library behind a command line: help and version print and exit 0,
/// a command line that does not parse exits 2 with clap's message, a failure prints
/// `hkssh: <error>` and exits with the error's code, and `known-host` prints its line.
#[test]
fn the_binary_exits_with_the_codes_it_promises() {
    for flag in ["--help", "--version"] {
        let out = hkssh(&[flag]);
        assert_eq!(out.status.code(), Some(0), "{flag}");
        assert!(!out.stdout.is_empty());
    }
    let out = hkssh(&["--version"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("hkssh {}\n", hostkeeper_hkssh::stamp::version())
    );
    let out = hkssh(&["revoke"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--operator"));
    let out = hkssh(&["status", "host-1"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "hkssh: this command needs --state-dir\n"
    );
    let rig = Rig::start();
    let pins = rig.dir().join("pins");
    std::fs::write(&pins, "SHA256:abc\n").expect("pins");
    let out = hkssh(&[
        "known-host",
        pins.to_str().expect("utf-8"),
        "h",
        "ssh-ed25519",
        "AAAA",
        "SHA256:abc",
    ]);
    assert_eq!(
        (out.status.code(), out.stdout),
        (Some(0), b"h ssh-ed25519 AAAA\n".to_vec())
    );
    let args = rig.args(&["status", "host-1"]);
    let out = hkssh(&args[1..].iter().map(String::as_str).collect::<Vec<_>>());
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("host: host-1\nhost keys: pinned\n"));
    let mut down = rig.args(&["status", "host-1"]);
    down[2] = "https://localhost:1".to_owned();
    let out = hkssh(&down[1..].iter().map(String::as_str).collect::<Vec<_>>());
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr).starts_with("hkssh: no answer from the coordinator")
    );
}
