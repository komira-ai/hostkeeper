//! The command line: flags, exit statuses and the host's ports.

use std::ffi::OsString;
use std::process::ExitCode;

use hostkeeper_bootstrap::cli::{HostTools, execute, system_roots};
use hostkeeper_install::tools::{Tool, Tools};

use crate::support::*;

fn args(dir: &std::path::Path, settings: &str, more: &[&str]) -> Vec<OsString> {
    let paths = paths_in(dir, settings);
    let mut all: Vec<OsString> = vec!["hostkeeper-bootstrap".into()];
    for (flag, value) in [
        ("--managed-preferences", paths.sources.managed.clone()),
        ("--fallback-file", paths.sources.fallback.clone()),
        ("--config-dir", paths.config_dir.clone()),
        ("--staging-dir", paths.staging_dir.clone()),
        ("--installed-binary", paths.binary.clone()),
    ] {
        all.push(flag.into());
        all.push(value.into());
    }
    all.push("--trusted-uid".into());
    all.push(paths.sources.trusted_uid.to_string().into());
    all.extend(more.iter().map(OsString::from));
    all
}

fn code(code: ExitCode) -> String {
    format!("{code:?}")
}

fn status(n: u8) -> String {
    code(ExitCode::from(n))
}

/// A run with every path given by flag installs the pkg and exits 0.
#[test]
fn flags_point_a_run_at_its_own_paths_and_it_exits_zero() {
    let dir = scratch("cli-ok");
    let world = World::good();
    let exit = execute(
        args(dir.path(), "", &[]),
        &world.repo,
        &world.tools,
        &world.pauses,
    );
    assert_eq!(code(exit), status(0));
    assert!(exists(&dir.path().join("etc/hostkeeper/hostkeeper.toml")));
}

/// A run that gives up exits 1, so launchd starts the program again.
#[test]
fn a_run_that_gives_up_exits_one() {
    let dir = scratch("cli-gave-up");
    let world = World::good();
    world.repo.down.set(true);
    let exit = execute(
        args(dir.path(), "[bounds]\nwait_secs = 0\n", &[]),
        &world.repo,
        &world.tools,
        &world.pauses,
    );
    assert_eq!(code(exit), status(1));
}

/// A Mac that has hostkeeper exits 0 with nothing fetched.
#[test]
fn an_installed_mac_exits_zero_without_a_request() {
    let dir = scratch("cli-noop");
    let world = World::good();
    write_mode(&dir.path().join("usr/local/bin/hostkeeper"), "x", 0o755);
    let exit = execute(
        args(dir.path(), "", &[]),
        &world.repo,
        &world.tools,
        &world.pauses,
    );
    assert_eq!(code(exit), status(0));
    assert!(world.repo.asked.borrow().is_empty());
}

/// A flag the program does not know exits 2; `--help` exits 0.
#[test]
fn unknown_flags_exit_two_and_help_exits_zero() {
    let dir = scratch("cli-flags");
    let world = World::good();
    let bad = execute(
        args(dir.path(), "", &["--bogus"]),
        &world.repo,
        &world.tools,
        &world.pauses,
    );
    assert_eq!(code(bad), status(2));
    let help = execute(
        ["hostkeeper-bootstrap", "--help"].map(OsString::from),
        &world.repo,
        &world.tools,
        &world.pauses,
    );
    assert_eq!(code(help), status(0));
    assert!(world.repo.asked.borrow().is_empty());
}

/// Off a Mac the host's tools cannot run, and say so; the system's roots load without a network.
#[cfg(not(target_os = "macos"))]
#[test]
fn off_a_mac_the_host_tools_say_they_are_not_a_mac() {
    let error = HostTools
        .run(
            Tool::Pkgutil,
            &["--check-signature".into(), "x.pkg".into()],
            std::time::Duration::from_secs(1),
        )
        .expect_err("not a Mac");
    assert!(
        error
            .to_string()
            .contains("/usr/sbin/pkgutil is a tool of a Mac, and this is not one"),
        "{error}"
    );
    let _ = system_roots();
}

/// The real program, started with flags that name a Mac that has hostkeeper, exits 0: the entry
/// point forwards to the library and reads no environment.
#[test]
fn the_program_exits_zero_on_an_installed_mac() {
    let dir = scratch("cli-binary");
    write_mode(&dir.path().join("usr/local/bin/hostkeeper"), "x", 0o755);
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_hostkeeper-bootstrap"))
        .args(args(dir.path(), "", &[]).into_iter().skip(1))
        .output()
        .expect("the program runs");
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("hostkeeper is installed; nothing to do")
    );
}
