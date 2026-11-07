//! The program itself: its argument handling, exit codes and the random source.

use std::path::Path;
use std::process::Command;

use hostkeeper_mdm::system_random;
use hostkeeper_testkit::Scratch;

fn hostkeeper_mdm(args: &[&str]) -> (Option<i32>, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_hostkeeper-mdm"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Catches a main that does not pass the exit code through: help exits 0, a command line the
/// parser refuses exits 2, and a refused verb exits 2 with REFUSED on stderr. The verb used
/// (a relative `--dir`, or root) refuses before doing anything, whoever runs the test.
#[test]
fn the_program_exits_as_documented() {
    let (code, out, _) = hostkeeper_mdm(&["--help"]);
    assert_eq!(code, Some(0));
    assert!(out.contains("Usage: hostkeeper-mdm"));
    let (code, out, err) = hostkeeper_mdm(&[]);
    assert_eq!((code, out.as_str()), (Some(2), ""));
    assert!(!err.is_empty());
    let (code, _, err) = hostkeeper_mdm(&["fetch", "--dir", "relative/dir"]);
    assert_eq!(code, Some(2));
    assert!(err.starts_with("REFUSED: "), "{err}");
}

/// Catches a random source that returns zeros or the same bytes twice: a secret made from it
/// would be guessable.
#[test]
fn the_system_random_source_fills_the_buffer_with_fresh_bytes() {
    let (mut a, mut b) = ([0u8; 32], [0u8; 32]);
    system_random(&mut a).unwrap();
    system_random(&mut b).unwrap();
    assert_ne!(a, [0u8; 32]);
    assert_ne!(a, b);
}

/// Catches `--version` not reaching the parser, going to stderr, or not carrying the stamp: it
/// prints `hostkeeper-mdm <crate version>` on stdout, followed for a release build (one given
/// `RELEASE_COMMIT`) by `+` and the first 12 digits of that commit, and exits 0. `cargo xtask
/// release check-binary` holds a release build to the same line.
#[test]
fn the_program_prints_its_version() {
    let (code, out, err) = hostkeeper_mdm(&["--version"]);
    assert_eq!(code, Some(0));
    let version = env!("CARGO_PKG_VERSION");
    let want = match option_env!("RELEASE_COMMIT") {
        Some(commit) => format!("hostkeeper-mdm {version}+{}\n", &commit[..12]),
        None => format!("hostkeeper-mdm {version}\n"),
    };
    assert_eq!(out, want);
    assert_eq!(err, "");
}

/// Catches an unknown verb that is run, ignored or answered with exit 1: the parser refuses it
/// with exit 2, says so on stderr and prints nothing on stdout.
#[test]
fn an_unknown_verb_exits_2() {
    let (code, out, err) = hostkeeper_mdm(&["frobnicate"]);
    assert_eq!((code, out.as_str()), (Some(2), ""));
    assert!(
        err.starts_with("error: unrecognized subcommand 'frobnicate'"),
        "{err}"
    );
}

/// Catches a settings file that is not there passing as a plan, or a main that turns the refusal
/// into exit 1: install reads the file before it looks at who runs it, so whoever runs the test
/// (root or not) gets exit 2 and the one REFUSED line, and nothing is printed or changed.
#[test]
fn install_with_a_settings_file_that_is_not_there_is_refused() {
    let dir = Scratch::under(
        Path::new(env!("CARGO_TARGET_TMPDIR")),
        "hostkeeper-mdm-main",
    )
    .unwrap();
    let settings = dir.path().join("settings");
    let (settings, bin) = (settings.to_str().unwrap(), dir.path().to_str().unwrap());
    let (code, out, err) = hostkeeper_mdm(&["install", "--settings", settings, "--bin-dir", bin]);
    assert_eq!((code, out.as_str()), (Some(2), ""));
    assert_eq!(err, format!("REFUSED: {settings} is not there\n"));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}
