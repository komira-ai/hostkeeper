//! `hostkeeper-coordinator serve --operator-keys-file`: the file of the operator's SSH public keys
//! for each host group. A valid file starts the coordinator with those keys, a missing flag
//! starts it with none, and an invalid file refuses the start naming the flag and the rule, never
//! a key's text. Driven in-process through `config::{Cli, build, main}`; nothing binds a socket.

mod support;

use std::ffi::OsString;
use std::process::ExitCode;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use clap::Parser;
use hostkeeper_coordinator::config::{Cli, StartError};
use hostkeeper_wire::operator_keys::digest;
use support::serve::{Files, build_given, exit_code, flags_of, refusal};

/// An `ssh-ed25519` key line of the blob of `fill`, made at run time, with `comment`.
fn key(fill: u8, comment: &str) -> String {
    let kind = "ssh-ed25519";
    let mut bytes = vec![0, 0, 0, 11];
    bytes.extend_from_slice(kind.as_bytes());
    bytes.extend_from_slice(&[0, 0, 0, 32]);
    bytes.extend_from_slice(&[fill; 32]);
    format!("{kind} {} {comment}", STANDARD.encode(bytes))
}

/// The arguments of a `serve` that starts, with `--operator-keys-file` naming a file of `text`.
fn args_with(files: &Files, text: &str) -> Vec<OsString> {
    files.put("operator-keys.toml", text, 0o644);
    let mut args = files.args();
    args.push("--operator-keys-file".into());
    args.push(files.path("operator-keys.toml").into());
    args
}

/// Proves: a valid operator keys file starts the coordinator, which holds the keys of each group
/// the file lists, with the digest the daemon computes, and none for a group it does not list; a
/// file whose mode is not 0600 is read, since it holds public keys.
///
/// Catches: a flag that parses and is not read, and a mode rule of a secret file applied to
/// public keys.
#[test]
fn a_valid_file_starts_the_coordinator_with_its_keys() {
    let files = Files::new("operator-keys-start");
    let keys = vec![key(1, "ops laptop"), key(2, "ci")];
    let text = format!(
        "[host-groups.farm-linux]\nkeys = [\"{}\", \"  {}  \"]\n[host-groups.farm-mac]\nkeys = []\n",
        keys[0], keys[1]
    );
    let parts = build_given(&flags_of(args_with(&files, &text))).expect("the start");
    let held = parts.coordinator.operator_keys();
    assert!(held.is_given());
    let build = held
        .set_of("farm-linux")
        .expect("the farm-linux group's keys");
    assert_eq!(build.keys, keys);
    assert_eq!(build.digest, digest(&keys));
    assert_eq!(held.set_of("farm-mac").expect("a group").keys.len(), 0);
    assert_eq!(held.set_of("other"), None);
}

/// Proves: a coordinator started without the flag is given no file, so no host is sent keys.
///
/// Catches: a default that sends the empty set to every host of a fleet that never asked.
#[test]
fn without_the_flag_no_file_is_given() {
    let files = Files::new("operator-keys-none");
    let parts = build_given(&flags_of(files.args())).expect("the start");
    assert!(!parts.coordinator.operator_keys().is_given());
}

/// Proves: an invalid file refuses the start with an error that names `--operator-keys-file`, the
/// group and the key's place, in exactly these words and without the key's text, and `main`
/// exits 1; a file that does not exist is refused naming the flag and not the path.
///
/// Catches: a start with a file that is not read, an error that leaves out the flag, and one that
/// prints a key.
#[test]
fn an_invalid_file_refuses_the_start_naming_the_flag() {
    let files = Files::new("operator-keys-refused");
    let first = key(1, "ops laptop");
    let text = format!("[host-groups.farm-linux]\nkeys = [\"{first}\", \"{first} again\"]\n");
    let args = args_with(&files, &text);
    let why = refusal(args.clone());
    assert!(matches!(why, StartError::OperatorKeys(_)), "{why}");
    assert_eq!(
        why.to_string(),
        "--operator-keys-file: host group farm-linux: key 2 has the type and blob of key 1"
    );
    assert!(!why.to_string().contains(&first));
    assert_eq!(exit_code(args), ExitCode::from(1));

    let missing = Files::new("operator-keys-missing");
    let mut args = missing.args();
    args.push("--operator-keys-file".into());
    args.push(missing.path("absent.toml").into());
    let said = refusal(args).to_string();
    assert_eq!(
        said,
        "--operator-keys-file: No such file or directory (os error 2)"
    );
}

/// Proves: the usage of `serve` names `--operator-keys-file` and says it holds public keys.
///
/// Catches: a flag that works and is not in the help.
#[test]
fn the_help_names_the_flag() {
    let help =
        Cli::try_parse_from(["hostkeeper-coordinator", "serve", "--help"]).expect_err("help");
    let text = help.to_string();
    assert!(text.contains("--operator-keys-file"), "{text}");
    assert!(text.contains("Public keys only"), "{text}");
}
