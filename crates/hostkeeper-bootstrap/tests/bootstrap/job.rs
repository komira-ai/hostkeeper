//! The check of the daemon's launchd job on a Mac that has hostkeeper: a job that is not loaded is
//! enabled and loaded, and every failure of `launchctl` is an error of the attempt that the retry
//! loop handles. (A loaded job being left alone is proved in `run.rs`.)

use hostkeeper_bootstrap::run::{Error, Outcome, Paths};
use hostkeeper_install::tools::{Tool, ToolOutput};
use hostkeeper_launchd::launchctl::{EXIT_NOT_LOADED, NOT_LOADED_TEXT};
use hostkeeper_testkit::Scratch;

use crate::support::*;

const JOB: &str = "system/dev.hostkeeper.hostkeeper";

fn words(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

fn print_call() -> Vec<String> {
    words(&["print", JOB])
}

fn enable_call() -> Vec<String> {
    words(&["enable", JOB])
}

fn bootstrap_call(paths: &Paths) -> Vec<String> {
    words(&[
        "bootstrap",
        "system",
        &paths.daemon_plist.display().to_string(),
    ])
}

/// A Mac that has hostkeeper's binary and the marker of a finished install.
fn installed_mac(name: &str, settings: &str) -> (Scratch, Paths) {
    let dir = scratch(name);
    let paths = paths_in(dir.path(), settings);
    write_mode(&paths.binary, "x", 0o755);
    write_mode(&paths.staging_dir.join("installed"), "", 0o600);
    (dir, paths)
}

/// What `launchctl print` ends with for a job launchd does not hold.
fn not_loaded() -> Answer {
    Ok(ToolOutput::exit(
        EXIT_NOT_LOADED,
        &format!("{NOT_LOADED_TEXT}: system/dev.hostkeeper.hostkeeper"),
    ))
}

fn exits(code: i32, stderr: &str) -> Answer {
    Ok(ToolOutput::exit(code, stderr))
}

fn cannot_start() -> Answer {
    Err("no such file".to_owned())
}

fn no_wait() -> &'static str {
    "[bounds]\nwait_secs = 0\n"
}

/// Proves that a Mac that has hostkeeper whose job launchd does not hold (exit 113) gets the job
/// enabled and then loaded from the daemon's plist, in that order, runs no other tool, fetches
/// nothing, writes nothing and ends with the outcome that says so. Catches a bootstrap that
/// reports "nothing to do" for an installed Mac whose job is not loaded.
#[test]
fn an_installed_mac_whose_job_is_not_loaded_has_it_enabled_and_loaded() {
    let (_dir, paths) = installed_mac("job-not-loaded", "");
    let world = World::good();
    world.tools.print_answers([not_loaded()]);
    let outcome = world.run(&paths).expect("the job is loaded");
    assert_eq!(outcome, Outcome::Loaded);
    assert_eq!(
        world.tools.launchctl_calls(),
        [print_call(), enable_call(), bootstrap_call(&paths)]
    );
    assert_eq!(world.tools.ran(), [Tool::Launchctl; 3]);
    assert!(world.repo.asked.borrow().is_empty(), "nothing is fetched");
    assert!(!exists(&paths.config_dir), "no file is written");
    assert!(world.pauses().is_empty());
    assert_eq!(
        outcome.to_string(),
        "hostkeeper is installed and its job was not loaded: enabled and loaded it"
    );
    assert_eq!(
        world.log.borrow()[0],
        "dev.hostkeeper.hostkeeper is not loaded; enabling and loading it"
    );
}

/// Proves the other readings of "not loaded": exit code 1 with the text `launchctl` writes for a
/// label the domain does not hold, on standard error or on standard output, is not loaded too.
#[test]
fn a_print_that_says_the_service_is_not_found_is_not_loaded() {
    let on_stdout = ToolOutput {
        code: Some(1),
        stdout: format!("{NOT_LOADED_TEXT}\n").into_bytes(),
        stderr: Vec::new(),
    };
    let answers = [
        exits(
            1,
            &format!("Bad request.\n{NOT_LOADED_TEXT} in domain for system"),
        ),
        Ok(on_stdout),
    ];
    for (n, answer) in answers.into_iter().enumerate() {
        let (_dir, paths) = installed_mac(&format!("job-text-{n}"), "");
        let world = World::good();
        world.tools.print_answers([answer]);
        assert_eq!(world.run(&paths).expect("loaded"), Outcome::Loaded, "{n}");
        assert_eq!(
            world.tools.launchctl_calls(),
            [print_call(), enable_call(), bootstrap_call(&paths)],
            "{n}"
        );
    }
}

/// Proves a host certificate alone (no binary, no marker) is checked the same way: a Mac that
/// enrolled before and has no job loaded gets the job enabled and loaded.
#[test]
fn a_mac_with_a_host_certificate_and_no_job_has_the_job_loaded() {
    let dir = scratch("job-pki");
    let paths = paths_in(dir.path(), "");
    write_mode(&paths.pki_current, "x", 0o755);
    let world = World::good();
    world.tools.print_answers([not_loaded()]);
    assert_eq!(world.run(&paths).expect("loaded"), Outcome::Loaded);
    assert_eq!(
        world.tools.launchctl_calls(),
        [print_call(), enable_call(), bootstrap_call(&paths)]
    );
}

/// Proves a Mac whose settings are gone still has its job checked and loaded (the check needs no
/// farm values; the default bounds apply).
#[test]
fn the_job_is_checked_and_loaded_without_settings() {
    let (_dir, paths) = installed_mac("job-no-settings", "");
    std::fs::remove_file(&paths.sources.fallback).expect("remove the settings");
    let world = World::good();
    world.tools.print_answers([not_loaded()]);
    assert_eq!(world.run(&paths).expect("loaded"), Outcome::Loaded);
    assert_eq!(world.tools.launchctl_calls().len(), 3);
}

/// Proves a bootstrap that fails is not an error when a second `print` shows the job loaded
/// (launchd loaded it meanwhile): the run ends with success after print, enable, bootstrap and a
/// second print, and does not pause or retry.
#[test]
fn a_failed_bootstrap_of_a_job_that_is_loaded_by_then_is_success() {
    let (_dir, paths) = installed_mac("job-raced", "");
    let world = World::good();
    world
        .tools
        .print_answers([not_loaded(), Ok(ToolOutput::success(""))]);
    *world.tools.launchctl_bootstrap.lock().expect("bootstrap") =
        exits(5, "Bootstrap failed: 5: Input/output error");
    assert_eq!(world.run(&paths).expect("loaded by then"), Outcome::Loaded);
    assert_eq!(
        world.tools.launchctl_calls(),
        [
            print_call(),
            enable_call(),
            bootstrap_call(&paths),
            print_call()
        ]
    );
    assert!(world.pauses().is_empty());
}

/// Proves a bootstrap that fails with the job still not loaded is an error of the attempt that
/// names launchctl and what it said, and is retried after a pause: with a wait of 60 seconds in
/// 30-second pauses there are three attempts, each of them print, enable, bootstrap, print, and
/// the run gives up with the last error.
#[test]
fn a_failed_bootstrap_of_a_job_that_is_still_not_loaded_is_retried_then_gives_up() {
    let settings = "[bounds]\nwait_secs = 60\nretry_secs = 30\n";
    let (_dir, paths) = installed_mac("job-still-not", settings);
    let world = World::good();
    world.tools.print_answers((0..6).map(|_| not_loaded()));
    *world.tools.launchctl_bootstrap.lock().expect("bootstrap") =
        exits(5, "Bootstrap failed: 5: Input/output error\n");
    let gave_up = world.run(&paths).expect_err("gave up");
    assert_eq!(gave_up.attempts, 3);
    assert_eq!(world.pauses(), [30, 30]);
    assert_eq!(
        gave_up.to_string(),
        "gave up after 3 attempt(s); the last failed: launchctl bootstrap exited Some(5): \
         Bootstrap failed: 5: Input/output error"
    );
    assert!(
        matches!(
            gave_up.last,
            Error::Launchctl {
                verb: "bootstrap",
                ..
            }
        ),
        "{gave_up}"
    );
    assert_eq!(world.tools.launchctl_calls().len(), 12);
}

/// Proves an `enable` that exits non-zero is an error of the attempt, and no bootstrap is run.
#[test]
fn a_failed_enable_is_an_error_and_no_bootstrap_runs() {
    let (_dir, paths) = installed_mac("job-enable-fails", no_wait());
    let world = World::good();
    world.tools.print_answers([not_loaded()]);
    *world.tools.launchctl_enable.lock().expect("enable") = exits(1, "enable: not permitted\n");
    let gave_up = world.run(&paths).expect_err("gave up");
    assert_eq!(
        gave_up.to_string(),
        "gave up after 1 attempt(s); the last failed: launchctl enable exited Some(1): \
         enable: not permitted"
    );
    assert_eq!(world.tools.launchctl_calls(), [print_call(), enable_call()]);
}

/// Proves a `print` that ends with a code that is neither 0 nor the not-loaded reading (or with a
/// signal, no code) is an error of the attempt, and nothing is enabled or loaded.
#[test]
fn a_print_that_fails_in_another_way_is_an_error_and_nothing_is_changed() {
    let signalled = ToolOutput {
        code: None,
        stdout: Vec::new(),
        stderr: b"killed\n".to_vec(),
    };
    let cases = [
        (
            exits(64, "Usage: launchctl print\n"),
            "Some(64): Usage: launchctl print",
        ),
        (Ok(signalled), "None: killed"),
    ];
    for (n, (answer, shown)) in cases.into_iter().enumerate() {
        let (_dir, paths) = installed_mac(&format!("job-print-fails-{n}"), no_wait());
        let world = World::good();
        world.tools.print_answers([answer]);
        let gave_up = world.run(&paths).expect_err("gave up");
        assert_eq!(
            gave_up.to_string(),
            format!("gave up after 1 attempt(s); the last failed: launchctl print exited {shown}"),
            "{n}"
        );
        assert_eq!(world.tools.launchctl_calls(), [print_call()], "{n}");
    }
}

/// Proves a `launchctl` that cannot be started is an error that names launchctl, not pkgutil,
/// whichever command it is: the first `print`, the `enable`, the `bootstrap` and the `print` after
/// a failed bootstrap each fail to start.
#[test]
fn a_launchctl_that_cannot_be_started_is_an_error_naming_launchctl() {
    let steps = ["print", "enable", "bootstrap", "second-print"];
    for step in steps {
        let (_dir, paths) = installed_mac(&format!("job-spawn-{step}"), no_wait());
        let world = World::good();
        match step {
            "print" => world.tools.print_answers([cannot_start()]),
            "enable" => {
                world.tools.print_answers([not_loaded()]);
                *world.tools.launchctl_enable.lock().expect("enable") = cannot_start();
            }
            "bootstrap" => {
                world.tools.print_answers([not_loaded()]);
                *world.tools.launchctl_bootstrap.lock().expect("bootstrap") = cannot_start();
            }
            _ => {
                world.tools.print_answers([not_loaded(), cannot_start()]);
                *world.tools.launchctl_bootstrap.lock().expect("bootstrap") = exits(5, "no");
            }
        }
        let gave_up = world.run(&paths).expect_err("gave up");
        assert!(
            matches!(
                gave_up.last,
                Error::Tool {
                    tool: "launchctl",
                    ..
                }
            ),
            "{step}: {gave_up}"
        );
        assert!(
            gave_up
                .to_string()
                .contains("launchctl could not be run: cannot run launchctl"),
            "{step}: {gave_up}"
        );
    }
}
