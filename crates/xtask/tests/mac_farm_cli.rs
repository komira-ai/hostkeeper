//! `xtask mac-farm` as a process: the verb is on the command line a workflow runs, its exit
//! statuses (0 for no farm, 2 for a wrong flag) and the words of a refusal. The runs against a
//! farm are the library's tests (`src/mac_farm/farm_tests.rs`), over a fake farm in the process.

#[path = "../src/testdir.rs"]
mod testdir;

use std::path::Path;
use std::process::{Command, Output};

use testdir::TestDir;

fn dir(name: &str) -> TestDir {
    TestDir::under(
        Path::new(env!("CARGO_TARGET_TMPDIR")),
        &format!("mac-farm-cli-{name}"),
    )
}

/// A PEM file of one certificate made now, for `--ca-bundle`: the verb reads its roots before it
/// gathers anything, and no test reads the host's.
fn bundle(dir: &Path) -> String {
    let cert = rcgen::generate_simple_self_signed(vec!["roots.example".to_owned()]).expect("cert");
    let path = dir.join("roots.pem");
    std::fs::write(&path, cert.cert.pem()).expect("write");
    path.display().to_string()
}

fn run(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .current_dir(cwd)
        .arg("mac-farm")
        .args(args)
        .output()
        .expect("run xtask")
}

/// Catches a run with no farm configured (a fork, where the organisation's variables are empty)
/// that fails or reaches for the network instead of saying so and passing.
#[test]
fn no_address_ends_0_with_a_notice() {
    let d = dir("none");
    let out = run(&d, &["--address", "", "--scratch", "s", "--", "build"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "::notice::mac-farm runs nothing: the variable FARM_ADDRESS is not set\n"
    );
    assert!(!d.join("s").exists());
}

/// Catches a wrong flag that ends the run as a failure of the farm (1) or not at all, its words
/// not naming the flag, and the farm's address printed before the log masks it.
#[test]
fn a_wrong_flag_ends_2_with_its_words_after_the_mask() {
    let d = dir("wrong");
    let roots = bundle(&d);
    let base = [
        "--address",
        "grpc://farm.example:8980",
        "--properties",
        "OSFamily=Darwin",
        "--scratch",
        "s",
        "--ca-bundle",
        &roots,
    ];
    for (more, words) in [
        (
            vec!["--tls", "maybe", "--", "build"],
            "xtask: --tls is \"maybe\": it must be `true` or `false`\n",
        ),
        (
            vec!["--tls", "true", "--env", "RUSTC=/bin/false", "--", "build"],
            "xtask: --env sets RUSTC, which the verb sets itself\n",
        ),
        (
            vec!["--tls", "true", "--output", "../outside", "--", "build"],
            "xtask: --output \"../outside\" is not a relative path of plain components outside .mac-farm, the run's own\n",
        ),
    ] {
        let args: Vec<&str> = base.iter().copied().chain(more).collect();
        let out = run(&d, &args);
        assert_eq!(out.status.code(), Some(2), "{out:?}");
        assert_eq!(String::from_utf8_lossy(&out.stderr), words);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "::add-mask::grpc://farm.example:8980\n::add-mask::farm.example:8980\n"
        );
    }
}

/// Catches a call with no Cargo command after `--` accepted (it would send `cargo` with no
/// subcommand to the Mac): clap refuses it as a usage error.
#[test]
fn no_cargo_command_is_a_usage_error() {
    let d = dir("no-command");
    let out = run(
        &d,
        &["--address", "grpc://farm.example:1", "--scratch", "s"],
    );
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("the following required arguments were not provided"),
        "{out:?}"
    );
}
