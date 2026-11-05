//! `hostkeeper ctl` in the command line: the arguments after `ctl` reach `hostkeeper-ctl`
//! unparsed, its output goes to the streams it is given, and its exit code is the program's.

use std::ffi::OsString;
use std::process::ExitCode;

use clap::Parser;
use clap::error::ErrorKind;
use hostkeeper_ctl::transport::{InMemory, Network};
use hostkeeper_ctl::{Connect, Io, PeerIdentity, exit};
use hostkeeper_daemon::cli::{self, Cli, Command};
use hostkeeper_proto::ctl::COMMANDS;

/// The streams and exit code of one run of `cli::ctl`.
struct Ran {
    code: u8,
    stdout: String,
    stderr: String,
}

/// Runs `hostkeeper ctl <args>` on `connect` with no stdin, capturing its output.
fn run_ctl(args: &[&str], connect: &dyn Connect) -> Ran {
    let mut stdin: &[u8] = b"";
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = cli::ctl(
        os(args),
        connect,
        Io {
            stdin: &mut stdin,
            stdout: &mut stdout,
            stderr: &mut stderr,
        },
    );
    Ran {
        code,
        stdout: String::from_utf8(stdout).expect("UTF-8 stdout"),
        stderr: String::from_utf8(stderr).expect("UTF-8 stderr"),
    }
}

/// `words` as the arguments of a process.
fn os(words: &[&str]) -> Vec<OsString> {
    words.iter().map(|word| OsString::from(*word)).collect()
}

/// The arguments `ctl` got from a command line.
fn ctl_args(line: &[&str]) -> Vec<OsString> {
    match Cli::parse_from(line).command {
        Command::Ctl { args } => args,
        Command::Daemon(flags) => panic!("parsed as the daemon: {flags:?}"),
    }
}

/// Catches an argument after `ctl` taken by the daemon's parser (a flag it does not know refused,
/// `--help` or `-h` answered with its own help, a value dropped or reordered): every word after
/// `ctl` reaches `hostkeeper-ctl` as typed, and `ctl` alone has none.
#[test]
fn every_argument_after_ctl_is_kept_as_typed() {
    assert_eq!(
        ctl_args(&["hostkeeper", "ctl", "a", "--flag"]),
        os(&["a", "--flag"])
    );
    assert_eq!(ctl_args(&["hostkeeper", "ctl", "--help"]), os(&["--help"]));
    assert_eq!(
        ctl_args(&["hostkeeper", "ctl", "-h", "-V", "--x=1", "--", "-y"]),
        os(&["-h", "-V", "--x=1", "--", "-y"])
    );
    assert!(ctl_args(&["hostkeeper", "ctl"]).is_empty());
}

/// Catches `ctl` missing from the program's own help: `hostkeeper --help` lists both commands.
#[test]
fn the_program_help_lists_ctl_and_daemon() {
    let help = Cli::try_parse_from(["hostkeeper", "--help"]).expect_err("help is not a command");
    assert_eq!(help.kind(), ErrorKind::DisplayHelp);
    let text = help.to_string();
    assert!(text.contains("  ctl "), "{text}");
    assert!(text.contains("  daemon "), "{text}");
}

/// Catches `ctl --help` answered by the daemon's parser, or the generated tree not printed on
/// stdout: it exits 0, prints the usage under `hostkeeper ctl` and every service of the table,
/// and writes nothing to stderr. It never makes a transport.
#[test]
fn ctl_help_prints_the_generated_tree_and_exits_0() {
    let ran = run_ctl(&["--help"], &Network);
    assert_eq!(ran.code, exit::SUCCESS, "{}", ran.stderr);
    assert!(
        ran.stdout.contains("Usage: hostkeeper ctl"),
        "{}",
        ran.stdout
    );
    for command in COMMANDS {
        assert!(
            ran.stdout.contains(command.service_command),
            "{} / {}",
            command.service_command,
            ran.stdout
        );
    }
    assert_eq!(ran.stderr, "");
}

/// Catches a usage error of `ctl` exiting with another status, or written to stdout: an unknown
/// service exits with the usage code, names the word on stderr, and prints nothing on stdout.
#[test]
fn a_ctl_usage_error_exits_with_the_usage_code_on_stderr() {
    let ran = run_ctl(&["no-such-service"], &Network);
    assert_eq!(ran.code, exit::USAGE);
    assert!(ran.stderr.contains("no-such-service"), "{}", ran.stderr);
    assert_eq!(ran.stdout, "");
}

/// Catches the arguments after `ctl` not reaching a call, or the call's result not being the
/// exit code: `hosts list` with the call flags calls the transport once, and a service the
/// transport does not serve answers UNIMPLEMENTED, which exits 1 and is written on stderr.
#[test]
fn a_ctl_call_goes_through_the_connector_given() {
    let memory = InMemory::new(PeerIdentity::Operator("op".to_owned()));
    let ran = run_ctl(
        &[
            "hosts",
            "list",
            "--endpoint",
            "https://coordinator.example",
            "--ca-file",
            "ca.pem",
        ],
        &memory,
    );
    assert_eq!(ran.code, exit::FAILURE, "{}", ran.stderr);
    assert_eq!(ran.stderr, "error: UNIMPLEMENTED: \n");
    assert_eq!(ran.stdout, "");
    assert_eq!(memory.calls(), 1);
}

/// Catches `run` not dispatching `ctl` to `hostkeeper-ctl` on the process's streams and the real
/// network connector: `ctl --help` returns the success status and a usage error returns the usage
/// status, as `ExitCode`s.
#[test]
fn run_dispatches_ctl_and_returns_its_exit_code() {
    let help = cli::run(Cli::parse_from(["hostkeeper", "ctl", "--help"]));
    assert_eq!(format!("{help:?}"), format!("{:?}", ExitCode::SUCCESS));
    let usage = cli::run(Cli::parse_from(["hostkeeper", "ctl", "no-such-service"]));
    assert_eq!(
        format!("{usage:?}"),
        format!("{:?}", ExitCode::from(exit::USAGE))
    );
}
