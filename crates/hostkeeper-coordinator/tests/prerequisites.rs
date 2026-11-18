//! `hostkeeper-coordinator serve --host-prerequisites-file`: the file of the host prerequisites of
//! each host group. A valid file starts the coordinator with those sets, a missing flag starts it
//! with none, and an invalid file refuses the start naming the flag and the rule, never the path.
//! Driven in-process through `config::{Cli, build, main}`; nothing binds a socket.

mod support;

use std::ffi::OsString;
use std::process::ExitCode;

use clap::Parser;
use hostkeeper_coordinator::config::{Cli, StartError};
use hostkeeper_wire::prerequisites::digest;
use support::serve::{Files, build_given, exit_code, flags_of, refusal};

/// The one prerequisite this release knows.
const XCODE: &str = "xcode-command-line-tools";

/// The arguments of a `serve` that starts, with `--host-prerequisites-file` naming a file of
/// `text`.
fn args_with(files: &Files, text: &str) -> Vec<OsString> {
    files.put("host-prerequisites.toml", text, 0o644);
    let mut args = files.args();
    args.push("--host-prerequisites-file".into());
    args.push(files.path("host-prerequisites.toml").into());
    args
}

/// Proves: a valid host prerequisites file starts the coordinator, which holds the set of each
/// group the file lists, with the digest the daemon computes, and none for a group it does not
/// list; a file whose mode is not 0600 is read, since it holds no secret.
///
/// Catches: a flag that parses and is not read, and a mode rule of a secret file applied to a
/// public file.
#[test]
fn a_valid_file_starts_the_coordinator_with_its_sets() {
    let files = Files::new("prerequisites-start");
    let text = format!(
        "[host-groups.mac-build]\nprerequisites = [\"{XCODE}\"]\n[host-groups.farm-linux]\nprerequisites = []\n"
    );
    let parts = build_given(&flags_of(args_with(&files, &text))).expect("the start");
    let held = parts.coordinator.host_prerequisites();
    assert!(held.is_given());
    let build = held.set_of("mac-build").expect("the mac-build group's set");
    assert_eq!(build.names, [XCODE]);
    assert_eq!(build.digest, digest(&[XCODE.to_owned()]));
    assert_eq!(held.set_of("farm-linux").expect("a group").names.len(), 0);
    assert_eq!(held.set_of("other"), None);
}

/// Proves: a coordinator started without the flag is given no file, so no host is sent a set.
///
/// Catches: a default that sends the empty set to every host of a fleet that never asked.
#[test]
fn without_the_flag_no_file_is_given() {
    let files = Files::new("prerequisites-none");
    let parts = build_given(&flags_of(files.args())).expect("the start");
    assert!(!parts.coordinator.host_prerequisites().is_given());
}

/// Proves: an invalid file refuses the start with an error that names
/// `--host-prerequisites-file`, the group, the place and the names this release knows, in exactly
/// these words, and `main` exits 1; a file that does not exist is refused naming the flag and not
/// the path.
///
/// Catches: a start with a file that is not read, and an error that leaves out the flag.
#[test]
fn an_invalid_file_refuses_the_start_naming_the_flag() {
    let files = Files::new("prerequisites-refused");
    let text = "[host-groups.mac-build]\nprerequisites = [\"homebrew\"]\n";
    let args = args_with(&files, text);
    let why = refusal(args.clone());
    assert!(matches!(why, StartError::Prerequisites(_)), "{why}");
    assert_eq!(
        why.to_string(),
        "--host-prerequisites-file: host group mac-build: prerequisite 1 is not a prerequisite this release knows (it knows xcode-command-line-tools)"
    );
    assert_eq!(exit_code(args), ExitCode::from(1));

    let missing = Files::new("prerequisites-missing");
    let mut args = missing.args();
    args.push("--host-prerequisites-file".into());
    args.push(missing.path("absent.toml").into());
    let said = refusal(args).to_string();
    assert_eq!(
        said,
        "--host-prerequisites-file: No such file or directory (os error 2)"
    );
}

/// Proves: the usage of `serve` names `--host-prerequisites-file` and the prerequisite it takes.
///
/// Catches: a flag that works and is not in the help.
#[test]
fn the_help_names_the_flag() {
    let help =
        Cli::try_parse_from(["hostkeeper-coordinator", "serve", "--help"]).expect_err("help");
    let text = help.to_string();
    assert!(text.contains("--host-prerequisites-file"), "{text}");
    assert!(text.contains("xcode-command-line-tools"), "{text}");
}
