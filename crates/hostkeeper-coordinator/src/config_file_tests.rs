//! `serve --config <file>`: what the file's keys and values stand for, where they are put, and
//! every way a file or the flag is refused, each by its full message.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use clap::Parser;

use hostkeeper_testkit::Scratch;

use super::{ConfigFileError, MAX_BYTES, expand, flags, read_file};
use crate::config::{Cli, Command};

fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

/// A reader that hands back `text` for the path `want` and fails for any other.
fn reads(want: &'static str, text: &'static str) -> impl Fn(&Path) -> io::Result<String> {
    move |path: &Path| {
        if path == Path::new(want) {
            Ok(text.to_owned())
        } else {
            Err(io::Error::new(io::ErrorKind::NotFound, "No such file"))
        }
    }
}

fn never(_: &Path) -> io::Result<String> {
    panic!("no file is read")
}

fn scratch(name: &str) -> Scratch {
    let base = std::env::current_exe()
        .expect("the test binary")
        .parent()
        .expect("its directory")
        .join("hostkeeper-coordinator-unit");
    Scratch::under(&base, name).expect("a scratch directory")
}

/// The file of flags the packaged unit names, as packaging/coordinator documents it.
const PACKAGED: &str = r#"
heartbeat-listen = "192.0.2.10:8443"
private-listen = "192.0.2.10:8444"
tls-cert = "/etc/hostkeeper-coordinator/tls.pem"
tls-key = "/etc/hostkeeper-coordinator/tls.key"
client-ca = "/etc/hostkeeper-coordinator/client-ca.pem"
oidc-config = "/etc/hostkeeper-coordinator/oidc.toml"
tokens-file = "/etc/hostkeeper-coordinator/tokens.toml"
state-file = "/var/lib/hostkeeper-coordinator/state.json"
live-rollouts = 2
server-name = ["coordinator.example.com", "192.0.2.10"]
ca-key-file = "/var/lib/hostkeeper-coordinator/ca.key"
server-cert-file = "/var/lib/hostkeeper-coordinator/server.pem"
ca-init = false
"#;

#[test]
fn each_kind_of_value_stands_for_its_flag_in_the_order_of_the_keys() {
    let text = r#"
zeta = "z"
alpha = 7
on = true
off = false
many = ["a", 2]
"#;
    assert_eq!(
        flags(text),
        Ok(os(&[
            "--alpha", "7", "--many", "a", "--many", "2", "--on", "--zeta", "z"
        ]))
    );
    assert_eq!(flags(""), Ok(Vec::new()));
}

#[test]
fn the_flags_of_the_file_stand_where_the_flag_stood() {
    let args = os(&[
        "hostkeeper-coordinator",
        "serve",
        "--quiet-secs",
        "5",
        "--config",
        "/f",
        "--ui-dir",
        "/u",
    ]);
    assert_eq!(
        expand(args, reads("/f", "health-listen = \"127.0.0.1:9\"")),
        Ok(os(&[
            "hostkeeper-coordinator",
            "serve",
            "--quiet-secs",
            "5",
            "--health-listen",
            "127.0.0.1:9",
            "--ui-dir",
            "/u"
        ]))
    );
    let joined = os(&["hostkeeper-coordinator", "serve", "--config=/f"]);
    assert_eq!(
        expand(joined, reads("/f", "ca-init = true")),
        Ok(os(&["hostkeeper-coordinator", "serve", "--ca-init"]))
    );
}

#[test]
fn arguments_without_the_flag_or_of_another_verb_are_given_back_unchanged() {
    for args in [
        os(&["hostkeeper-coordinator"]),
        os(&["hostkeeper-coordinator", "backup", "--config", "/f"]),
        os(&["hostkeeper-coordinator", "serve", "--ui-dir", "/u"]),
        os(&[
            "hostkeeper-coordinator",
            "serve",
            "--configs",
            "/f",
            "--config-x=1",
        ]),
        // A `--config` with no value: the parser refuses it, as any flag without its value.
        os(&["hostkeeper-coordinator", "serve", "--config"]),
    ] {
        assert_eq!(expand(args.clone(), never), Ok(args));
    }
}

#[test]
fn the_packaged_file_parses_as_the_command_line_it_stands_for() {
    let args = os(&[
        "hostkeeper-coordinator",
        "serve",
        "--config",
        "/etc/hostkeeper-coordinator/serve.toml",
    ]);
    let expanded = expand(
        args,
        reads("/etc/hostkeeper-coordinator/serve.toml", PACKAGED),
    )
    .expect("the file holds flags");
    let Command::Serve(flags) = Cli::try_parse_from(expanded)
        .expect("the flags of serve")
        .command
    else {
        panic!("serve");
    };
    assert_eq!(flags.heartbeat_listen.to_string(), "192.0.2.10:8443");
    assert_eq!(flags.private_listen.to_string(), "192.0.2.10:8444");
    assert_eq!(
        flags.server_names,
        ["coordinator.example.com", "192.0.2.10"]
    );
    assert!(!flags.ca_init);
    assert_eq!(flags.config, None);
    assert_eq!(
        flags.state_file,
        Some(PathBuf::from("/var/lib/hostkeeper-coordinator/state.json"))
    );
}

#[test]
fn a_flag_the_file_gives_and_the_command_line_gives_again_is_refused_by_the_parser() {
    let args = os(&[
        "hostkeeper-coordinator",
        "serve",
        "--heartbeat-listen",
        "192.0.2.1:1",
        "--config",
        "/f",
    ]);
    let expanded = expand(args, reads("/f", PACKAGED)).expect("the file holds flags");
    let refused = Cli::try_parse_from(expanded).expect_err("given twice");
    assert_eq!(refused.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn a_key_that_is_no_flag_of_serve_is_refused_by_the_parser() {
    let args = os(&["hostkeeper-coordinator", "serve", "--config", "/f"]);
    let text = format!("{PACKAGED}no-such-flag = \"x\"\n");
    let text: &'static str = Box::leak(text.into_boxed_str());
    let expanded = expand(args, reads("/f", text)).expect("the file holds flags");
    let refused = Cli::try_parse_from(expanded).expect_err("unknown");
    assert_eq!(refused.kind(), clap::error::ErrorKind::UnknownArgument);
}

#[test]
fn every_value_that_is_no_flag_is_refused_naming_its_key() {
    let cases = [
        (
            "a = 1.5",
            "a: a float, not a string, an integer, a boolean or an array of strings or integers",
        ),
        (
            "a = 2026-09-27",
            "a: a datetime, not a string, an integer, a boolean or an array of strings or integers",
        ),
        (
            "[a]\nb = 1",
            "a: a table, not a string, an integer, a boolean or an array of strings or integers",
        ),
        (
            "a = []",
            "a: an empty array gives no flag; leave the key out",
        ),
        (
            "a = [\"x\", true]",
            "a: each element must be a string or an integer",
        ),
        (
            "a = [[\"x\"]]",
            "a: each element must be a string or an integer",
        ),
        (
            "config = \"/other\"",
            "config: the file cannot name another file of flags",
        ),
        (
            "Upper = \"x\"",
            "\"Upper\" is not the name of a flag: lower-case letters, digits and '-', starting with a letter",
        ),
        (
            "\"-dash\" = \"x\"",
            "\"-dash\" is not the name of a flag: lower-case letters, digits and '-', starting with a letter",
        ),
        (
            "\"a_b\" = \"x\"",
            "\"a_b\" is not the name of a flag: lower-case letters, digits and '-', starting with a letter",
        ),
        (
            "\"\" = \"x\"",
            "\"\" is not the name of a flag: lower-case letters, digits and '-', starting with a letter",
        ),
    ];
    for (text, why) in cases {
        assert_eq!(flags(text), Err(why.to_owned()), "{text}");
    }
    let not_toml = flags("a = ").expect_err("not TOML");
    assert!(not_toml.starts_with("not TOML: "), "{not_toml}");
}

#[test]
fn an_unreadable_or_invalid_file_and_a_second_flag_are_refused_by_their_full_message() {
    let args = os(&["hostkeeper-coordinator", "serve", "--config", "/missing"]);
    let unreadable = expand(args, reads("/f", "")).expect_err("unreadable");
    assert_eq!(unreadable.to_string(), "--config /missing: No such file");

    let args = os(&["hostkeeper-coordinator", "serve", "--config=/f"]);
    let invalid = expand(args, reads("/f", "a = []")).expect_err("invalid");
    assert_eq!(
        invalid,
        ConfigFileError::Invalid {
            path: "/f".to_owned(),
            why: "a: an empty array gives no flag; leave the key out".to_owned(),
        }
    );
    assert_eq!(
        invalid.to_string(),
        "--config /f: a: an empty array gives no flag; leave the key out"
    );

    let twice = os(&[
        "hostkeeper-coordinator",
        "serve",
        "--config",
        "/f",
        "--config=/g",
    ]);
    let refused = expand(twice, never).expect_err("twice");
    assert_eq!(
        refused.to_string(),
        "--config is given twice: the flags of serve come from one file"
    );
}

#[test]
fn the_file_is_read_when_it_is_a_regular_file_of_at_most_the_limit() {
    let dir = scratch("config-file-read");
    let file = dir.path().join("serve.toml");
    std::fs::write(&file, "ca-init = true\n").expect("write");
    assert_eq!(read_file(&file).expect("read"), "ca-init = true\n");

    let limit = usize::try_from(MAX_BYTES).expect("small");
    std::fs::write(&file, "#".repeat(limit)).expect("write");
    assert_eq!(read_file(&file).expect("read").len(), limit);

    std::fs::write(&file, "#".repeat(limit + 1)).expect("write");
    assert_eq!(
        read_file(&file).expect_err("too large").to_string(),
        "65537 bytes, more than the 65536 a file of flags may hold"
    );
    assert_eq!(
        read_file(dir.path()).expect_err("a directory").to_string(),
        "not a regular file"
    );
    let missing = read_file(&dir.path().join("absent")).expect_err("missing");
    assert_eq!(missing.kind(), io::ErrorKind::NotFound);
}

#[test]
fn the_binary_exits_2_on_a_file_that_holds_no_flags_and_names_it() {
    let dir = scratch("config-file-main");
    let file = dir.path().join("serve.toml");
    std::fs::write(&file, "a = 1.5\n").expect("write");
    let code = crate::config::main(vec![
        OsString::from("hostkeeper-coordinator"),
        OsString::from("serve"),
        OsString::from("--config"),
        file.into_os_string(),
    ]);
    assert_eq!(code, std::process::ExitCode::from(2));
}

/// The template the deb and the rpm install, which the operator copies to
/// /etc/hostkeeper-coordinator/serve.toml.
const TEMPLATE: &str = include_str!("../../../packaging/coordinator/serve.toml.template");

#[test]
fn the_packaged_template_parses_as_the_flags_of_serve_the_unit_starts_with() {
    let args = os(&[
        "hostkeeper-coordinator",
        "serve",
        "--config",
        "/etc/hostkeeper-coordinator/serve.toml",
    ]);
    let expanded = expand(
        args,
        reads("/etc/hostkeeper-coordinator/serve.toml", TEMPLATE),
    )
    .expect("the template holds flags");
    let Command::Serve(flags) = Cli::try_parse_from(expanded)
        .expect("the flags of serve")
        .command
    else {
        panic!("serve");
    };
    assert_eq!(flags.heartbeat_listen.to_string(), "192.0.2.10:8443");
    assert_eq!(flags.rollout_file, None);
    assert_eq!(
        flags.state_file,
        Some(PathBuf::from("/var/lib/hostkeeper-coordinator/state.json"))
    );
}
