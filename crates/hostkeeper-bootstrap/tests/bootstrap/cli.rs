//! The command line: flags, exit statuses and the host's ports.

use std::ffi::OsString;
use std::process::ExitCode;

use hostkeeper_bootstrap::cli::{HostTools, execute, system_roots};
use hostkeeper_install::tools::{Tool, ToolOutput, Tools};

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
        ("--update-account-file", paths.update_account.clone()),
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

/// Catches `--update-account-file` being ignored: a run whose settings name the update account
/// writes the file at the flag's path, byte for byte, and exits 0.
#[test]
fn the_update_account_file_flag_names_where_the_file_is_written() {
    let dir = scratch("cli-account");
    let keys = "update_account_name = \"hkupdate\"\nupdate_account_password = 'pw one'\nadmin_name = \"admin\"\nadmin_password = 'pw two'\n";
    let world = World::good();
    let args = args(dir.path(), keys, &[]);
    let at = dir.path().join("Library/hostkeeper/update-account");
    let given = args
        .iter()
        .position(|arg| arg == "--update-account-file")
        .map(|flag| args[flag + 1].clone());
    assert_eq!(given, Some(at.clone().into_os_string()));
    let exit = execute(args, &world.repo, &world.tools, &world.pauses);
    assert_eq!(code(exit), status(0));
    assert_eq!(read(&at), "name=hkupdate\npassword=pw one\n");
    assert_eq!(mode(&at), 0o600);
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

/// A Mac that has hostkeeper and whose daemon job is loaded (the fake `launchctl` answers the
/// `print` with exit 0) exits 0 with nothing fetched and only that `print` run.
#[test]
fn an_installed_mac_exits_zero_without_a_request() {
    let dir = scratch("cli-noop");
    let world = World::good();
    write_mode(&dir.path().join("usr/local/bin/hostkeeper"), "x", 0o755);
    write_mode(&dir.path().join("var/bootstrap/installed"), "", 0o600);
    let exit = execute(
        args(dir.path(), "", &[]),
        &world.repo,
        &world.tools,
        &world.pauses,
    );
    assert_eq!(code(exit), status(0));
    assert!(world.repo.asked.borrow().is_empty());
    assert_eq!(world.tools.ran(), [Tool::Launchctl]);
}

/// A Mac that has hostkeeper and whose job is not loaded exits 0 once the job is enabled and
/// loaded; when `launchctl` cannot load it, the run gives up and exits 1 (launchd then starts the
/// program again).
#[test]
fn an_installed_mac_whose_job_is_not_loaded_exits_zero_when_it_is_loaded_and_one_when_it_is_not() {
    let not_loaded = || Ok(ToolOutput::exit(113, ""));
    let dir = scratch("cli-job");
    write_mode(&dir.path().join("usr/local/bin/hostkeeper"), "x", 0o755);
    write_mode(&dir.path().join("var/bootstrap/installed"), "", 0o600);
    let world = World::good();
    world.tools.print_answers([not_loaded()]);
    let exit = execute(
        args(dir.path(), "", &[]),
        &world.repo,
        &world.tools,
        &world.pauses,
    );
    assert_eq!(code(exit), status(0));
    assert_eq!(world.tools.launchctl_calls().len(), 3);

    let world = World::good();
    world.tools.print_answers([not_loaded(), not_loaded()]);
    *world.tools.launchctl_bootstrap.lock().expect("bootstrap") = Ok(ToolOutput::exit(5, "no"));
    let exit = execute(
        args(dir.path(), "[bounds]\nwait_secs = 0\n", &[]),
        &world.repo,
        &world.tools,
        &world.pauses,
    );
    assert_eq!(code(exit), status(1));
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

/// The real program, started with flags that name a Mac that has hostkeeper and a wait of zero,
/// forwards to the library and ends with its status: off a Mac its `launchctl` cannot be started,
/// so the check of the daemon's job fails, the run gives up after one attempt, and the program
/// exits 1 and says so on its standard error, naming launchctl. (On a Mac it would run the real
/// `launchctl` against the real system domain, which a test must not do, so the test is of other
/// systems; the exit status 0 of an installed Mac is the test above's.)
#[cfg(not(target_os = "macos"))]
#[test]
fn the_program_gives_up_on_an_installed_mac_whose_launchctl_cannot_run() {
    let dir = scratch("cli-binary");
    write_mode(&dir.path().join("usr/local/bin/hostkeeper"), "x", 0o755);
    write_mode(&dir.path().join("var/bootstrap/installed"), "", 0o600);
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_hostkeeper-bootstrap"))
        .args(
            args(dir.path(), "[bounds]\nwait_secs = 0\n", &[])
                .into_iter()
                .skip(1),
        )
        .output()
        .expect("the program runs");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(
            "hostkeeper-bootstrap: gave up after 1 attempt(s); the last failed: launchctl could not be run: cannot run launchctl print system/dev.hostkeeper.hostkeeper"
        ),
        "{stderr}"
    );
}
