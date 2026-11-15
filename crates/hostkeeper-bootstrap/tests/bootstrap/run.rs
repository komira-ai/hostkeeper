//! The steps of a run: what is checked before anything is written or run, what is written only
//! when absent, the no-op on a Mac that has hostkeeper, and the bounded wait.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;

use hostkeeper_bootstrap::files::Written;
use hostkeeper_bootstrap::run::{Error, Outcome, is_set_up};
use hostkeeper_config::daemon::DaemonConfig;
use hostkeeper_install::tools::Tool;

use crate::support::*;

fn no_wait() -> &'static str {
    "[bounds]\nwait_secs = 0\n"
}

/// A good pkg is fetched, checked by `pkgutil`, configured and installed, in that order: the
/// config files are written between the check and `installer`, and the staged pkg is gone after.
#[test]
fn a_good_pkg_is_checked_then_configured_then_installed() {
    let dir = scratch("run-good");
    let paths = paths_in(dir.path(), "");
    let world = World::good();
    let outcome = world.run(&paths).expect("installed");
    assert_eq!(
        outcome,
        Outcome::Installed {
            version: VERSION.into(),
            daemon_config: Written::Made,
            signers_file: Written::Made
        }
    );
    assert_eq!(world.tools.ran(), [Tool::Pkgutil, Tool::Installer]);
    assert_eq!(
        *world.tools.seen_pkg.lock().expect("seen"),
        [PKG_BYTES.to_vec()],
        "pkgutil was asked about the downloaded bytes"
    );
    let staged = paths.staging_dir.join("hostkeeper.pkg");
    let calls = world.tools.calls.lock().expect("calls");
    assert_eq!(
        calls[0].1,
        ["--check-signature".to_owned(), staged.display().to_string()]
    );
    assert_eq!(
        calls[1].1,
        [
            "-pkg".to_owned(),
            staged.display().to_string(),
            "-target".into(),
            "/".into()
        ]
    );
    assert!(!exists(&staged), "the staged pkg is removed");
    assert_eq!(mode(&paths.staging_dir), 0o700);
    assert!(world.pauses().is_empty());
    let daemon = read(&paths.config_dir.join("hostkeeper.toml"));
    let config = DaemonConfig::parse(&daemon).expect("the daemon's loader reads it");
    assert_eq!(config.coordinator.as_str(), "https://coordinator.example");
    assert_eq!(config.ca_sha256.as_str(), CA);
    assert_eq!(mode(&paths.config_dir.join("hostkeeper.toml")), 0o600);
    let signers = read(&paths.config_dir.join("macos-pkg.toml"));
    assert!(signers.contains(&leaf_hex()), "{signers}");
    assert_eq!(mode(&paths.config_dir.join("macos-pkg.toml")), 0o644);
    hostkeeper_install::macos_pkg::signers::parse(signers.as_bytes())
        .expect("the daemon's signers parser reads it");
    assert_eq!(
        outcome.to_string(),
        "installed hostkeeper 1.2.3 (hostkeeper.toml Made, macos-pkg.toml Made)"
    );
}

/// Signers named in the settings are written instead of the checked pkg's leaf.
#[test]
fn configured_signers_replace_the_checked_leaf() {
    let dir = scratch("run-signers");
    let named = "ab".repeat(32);
    let paths = paths_in(dir.path(), &format!("macos_pkg_signers = [\"{named}\"]\n"));
    World::good().run(&paths).expect("installed");
    let signers = read(&paths.config_dir.join("macos-pkg.toml"));
    assert!(
        signers.contains(&named) && !signers.contains(&leaf_hex()),
        "{signers}"
    );
}

/// Config files that are there are kept byte for byte, and the install still runs.
#[test]
fn existing_configs_are_kept_and_the_install_still_runs() {
    let dir = scratch("run-kept");
    let paths = paths_in(dir.path(), "");
    write_mode(
        &paths.config_dir.join("hostkeeper.toml"),
        "# the operator's\n",
        0o600,
    );
    write_mode(
        &paths.config_dir.join("macos-pkg.toml"),
        "# the operator's signers\n",
        0o644,
    );
    let world = World::good();
    let outcome = world.run(&paths).expect("installed");
    assert_eq!(
        outcome,
        Outcome::Installed {
            version: VERSION.into(),
            daemon_config: Written::Kept,
            signers_file: Written::Kept
        }
    );
    assert_eq!(
        read(&paths.config_dir.join("hostkeeper.toml")),
        "# the operator's\n"
    );
    assert_eq!(
        read(&paths.config_dir.join("macos-pkg.toml")),
        "# the operator's signers\n"
    );
    assert_eq!(world.tools.ran(), [Tool::Pkgutil, Tool::Installer]);
}

/// A Mac that has hostkeeper's binary and the marker of a finished install, or a host certificate, is a no-op: no request, no tool, no
/// file written, not even the staging directory.
#[test]
fn a_mac_that_has_hostkeeper_is_left_alone() {
    for markers in [
        &["usr/local/bin/hostkeeper", "var/bootstrap/installed"][..],
        &["etc/updater/pki/current"][..],
    ] {
        let dir = scratch("run-noop");
        let paths = paths_in(dir.path(), "");
        for marker in markers {
            write_mode(&dir.path().join(marker), "x", 0o755);
        }
        let world = World::good();
        assert_eq!(world.run(&paths).expect("a no-op"), Outcome::AlreadySetUp);
        assert!(world.repo.asked.borrow().is_empty(), "nothing is fetched");
        assert!(world.tools.ran().is_empty(), "no tool runs");
        assert!(!exists(&paths.config_dir) && !exists(&paths.staging_dir.join("hostkeeper.pkg")));
        assert!(is_set_up(&paths));
        assert_eq!(
            Outcome::AlreadySetUp.to_string(),
            "hostkeeper is installed; nothing to do"
        );
    }
    let dir = scratch("run-not-set-up");
    assert!(!is_set_up(&paths_in(dir.path(), "")));
}

/// The binary alone is an install that may have failed after the pkg laid its payload down (its
/// postinstall did not load the job): the Mac is installed again, and the marker is made when
/// `installer` exits 0.
#[test]
fn a_binary_without_a_finished_install_is_installed_again() {
    let dir = scratch("run-half");
    let paths = paths_in(dir.path(), "");
    write_mode(&paths.binary, "x", 0o755);
    assert!(!is_set_up(&paths));
    let world = World::good();
    world.run(&paths).expect("installed again");
    assert_eq!(world.tools.ran(), [Tool::Pkgutil, Tool::Installer]);
    assert!(is_set_up(&paths), "the marker is there after the install");
}

/// A marker that cannot be written is an error of the attempt.
#[test]
fn an_unwritable_marker_is_an_error() {
    let dir = scratch("run-marker");
    let paths = paths_in(dir.path(), no_wait());
    fs::create_dir_all(paths.staging_dir.join("installed")).expect("a directory in its place");
    let gave_up = World::good().run(&paths).expect_err("no marker");
    assert!(matches!(gave_up.last, Error::Staging { .. }), "{gave_up}");
}

/// A staging directory another account owns, or others can write, is refused before anything is
/// fetched.
#[test]
fn a_staging_directory_others_can_write_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch("run-staging");
    let paths = paths_in(dir.path(), no_wait());
    fs::create_dir_all(&paths.staging_dir).expect("dir");
    fs::set_permissions(&paths.staging_dir, fs::Permissions::from_mode(0o770)).expect("mode");
    let world = World::good();
    let gave_up = world.run(&paths).expect_err("refused");
    assert!(
        matches!(gave_up.last, Error::StagingNotTrusted { .. }),
        "{gave_up}"
    );
    assert!(world.repo.asked.borrow().is_empty());
}

/// Config files an attempt made are removed when `installer` fails, so a corrected setting is
/// not shadowed by a stale file; a file the operator had is left.
#[test]
fn a_failed_install_removes_the_configs_it_made() {
    let dir = scratch("run-rollback");
    let paths = paths_in(dir.path(), no_wait());
    write_mode(
        &paths.config_dir.join("macos-pkg.toml"),
        "# the operator's signers\n",
        0o644,
    );
    let world = World::good();
    *world.tools.installer_code.lock().expect("code") = Some(1);
    world.run(&paths).expect_err("failed");
    assert!(!exists(&paths.config_dir.join("hostkeeper.toml")));
    assert_eq!(
        read(&paths.config_dir.join("macos-pkg.toml")),
        "# the operator's signers\n"
    );
    assert!(!is_set_up(&paths));
}

/// Asserts that nothing was configured or installed and that the staged pkg was removed.
fn nothing_changed(world: &World, paths: &hostkeeper_bootstrap::run::Paths) {
    assert!(
        !world.tools.ran().contains(&Tool::Installer),
        "installer must not run"
    );
    assert!(!exists(&paths.config_dir), "no config is written");
    assert!(
        !exists(&paths.staging_dir.join("hostkeeper.pkg")),
        "the refused pkg is deleted"
    );
}

/// A pkg whose digest is not the pointer's is refused before `pkgutil` reads it.
#[test]
fn a_pkg_with_the_wrong_digest_is_refused_before_anything_reads_it() {
    let dir = scratch("run-digest");
    let paths = paths_in(dir.path(), no_wait());
    let mut world = World::good();
    world
        .repo
        .answers
        .insert(PKG_URL.into(), (200, vec![b'x'; PKG_BYTES.len()]));
    let gave_up = world.run(&paths).expect_err("refused");
    assert!(gave_up.last.to_string().contains("SHA-256"), "{gave_up}");
    assert!(world.tools.ran().is_empty(), "pkgutil never sees it");
    nothing_changed(&world, &paths);
}

/// A pkg whose size is not the pointer's is refused.
#[test]
fn a_pkg_with_the_wrong_size_is_refused() {
    let dir = scratch("run-size");
    let paths = paths_in(dir.path(), no_wait());
    let mut world = World::good();
    world
        .repo
        .answers
        .insert(PKG_URL.into(), (200, PKG_BYTES[..5].to_vec()));
    let gave_up = world.run(&paths).expect_err("refused");
    assert!(gave_up.last.to_string().contains("5 bytes"), "{gave_up}");
    assert!(world.tools.ran().is_empty());
    nothing_changed(&world, &paths);
}

/// A pkg signed by another Team ID is refused after `pkgutil` and before any config or install.
#[test]
fn a_pkg_signed_by_another_team_id_is_refused() {
    let dir = scratch("run-team");
    let paths = paths_in(dir.path(), no_wait());
    let world = World::good();
    *world.tools.pkgutil.lock().expect("pkgutil") = Ok(
        hostkeeper_install::tools::ToolOutput::success(&pkgutil_output(
            "signed by a developer certificate issued by Apple for distribution",
            "Developer ID Installer: Other Co (ABCDE12345)",
        )),
    );
    let gave_up = world.run(&paths).expect_err("refused");
    assert!(matches!(gave_up.last, Error::Signer(_)), "{gave_up}");
    assert_eq!(
        gave_up.last.to_string(),
        "the pkg is refused: the pkg is signed by Team ID ABCDE12345, not U7RTX4XD3X"
    );
    assert_eq!(world.tools.ran(), [Tool::Pkgutil]);
    nothing_changed(&world, &paths);
}

/// The Team ID is a key: a pkg of another team installs when the settings name that team.
#[test]
fn the_team_id_is_a_setting() {
    let dir = scratch("run-team-setting");
    let paths = paths_in(dir.path(), "team_id = \"ABCDE12345\"\n");
    let world = World::good();
    *world.tools.pkgutil.lock().expect("pkgutil") = Ok(
        hostkeeper_install::tools::ToolOutput::success(&pkgutil_output(
            "signed by a developer certificate issued by Apple for distribution",
            "Developer ID Installer: Other Co (ABCDE12345)",
        )),
    );
    assert!(matches!(world.run(&paths), Ok(Outcome::Installed { .. })));
}

/// A `pkgutil` that fails, or cannot run, refuses the pkg.
#[test]
fn a_pkgutil_failure_refuses_the_pkg() {
    let dir = scratch("run-pkgutil");
    let paths = paths_in(dir.path(), no_wait());
    let world = World::good();
    *world.tools.pkgutil.lock().expect("pkgutil") = Ok(hostkeeper_install::tools::ToolOutput {
        code: Some(1),
        stdout: Vec::new(),
        stderr: b"unsigned".to_vec(),
    });
    let gave_up = world.run(&paths).expect_err("refused");
    assert!(
        gave_up.last.to_string().contains("exited Some(1)"),
        "{gave_up}"
    );
    nothing_changed(&world, &paths);
    *world.tools.pkgutil.lock().expect("pkgutil") = Err("no such file".into());
    let gave_up = world.run(&paths).expect_err("refused");
    assert!(
        matches!(
            gave_up.last,
            Error::Tool {
                tool: "pkgutil",
                ..
            }
        ),
        "{gave_up}"
    );
    nothing_changed(&world, &paths);
}

/// An `installer` that fails ends the attempt with its last line; the staged pkg is removed and
/// the configs written stay for the next attempt.
#[test]
fn a_failed_installer_is_reported_and_its_pkg_removed() {
    let dir = scratch("run-installer");
    let paths = paths_in(dir.path(), no_wait());
    let world = World::good();
    *world.tools.installer_code.lock().expect("code") = Some(1);
    let gave_up = world.run(&paths).expect_err("failed");
    assert_eq!(
        gave_up.last.to_string(),
        "installer exited Some(1): installer: the install failed"
    );
    assert!(!exists(&paths.staging_dir.join("hostkeeper.pkg")));
    *world.tools.installer_spawn_fails.lock().expect("flag") = true;
    let gave_up = world.run(&paths).expect_err("failed");
    assert!(
        matches!(
            gave_up.last,
            Error::Tool {
                tool: "installer",
                ..
            }
        ),
        "{gave_up}"
    );
}

/// After the bound the run gives up with the last error: it pauses the configured time between
/// attempts and the pauses add up to no more than `wait_secs`.
#[test]
fn the_wait_is_bounded_by_the_sum_of_the_pauses() {
    let dir = scratch("run-bounded");
    let paths = paths_in(dir.path(), "[bounds]\nwait_secs = 100\nretry_secs = 30\n");
    let world = World::good();
    world.repo.down.set(true);
    let gave_up = world.run(&paths).expect_err("gave up");
    assert_eq!(world.pauses(), [30, 30, 30]);
    assert_eq!(gave_up.attempts, 4);
    assert!(
        gave_up
            .to_string()
            .starts_with("gave up after 4 attempt(s); the last failed: GET"),
        "{gave_up}"
    );
    assert_eq!(
        world
            .log
            .borrow()
            .iter()
            .filter(|l| l.contains("trying again in 30 seconds"))
            .count(),
        3
    );
    nothing_changed(&world, &paths);
}

/// A bound of no wait is one attempt and no pause.
#[test]
fn a_zero_wait_is_a_single_attempt() {
    let dir = scratch("run-single");
    let paths = paths_in(dir.path(), no_wait());
    let world = World::good();
    world.repo.down.set(true);
    let gave_up = world.run(&paths).expect_err("gave up");
    assert_eq!((gave_up.attempts, world.pauses().len()), (1, 0));
}

/// With no settings at all, the run waits by the default bounds, and settings that appear during
/// the wait are used by the next attempt.
#[test]
fn settings_that_arrive_during_the_wait_are_used() {
    let dir = scratch("run-arrive");
    let paths = paths_in(dir.path(), "");
    let source = paths.sources.fallback.clone();
    let text = read(&source);
    fs::remove_file(&source).expect("remove");
    let mut world = World::good();
    let arrive = source.clone();
    world.pauses.hook = Some(Box::new(move || write_mode(&arrive, &text, 0o600)));
    let outcome = world.run(&paths).expect("installed after the wait");
    assert!(matches!(outcome, Outcome::Installed { .. }));
    assert_eq!(world.pauses(), [30], "the default pause");
    assert!(world.log.borrow()[0].starts_with("attempt 1 failed: no farm values"));
}

/// The default bound is 900 seconds of 30-second pauses when there are no settings.
#[test]
fn without_settings_the_default_bounds_apply() {
    let dir = scratch("run-default");
    let paths = paths_in(dir.path(), "");
    fs::remove_file(&paths.sources.fallback).expect("remove");
    let world = World::good();
    let gave_up = world.run(&paths).expect_err("gave up");
    assert_eq!(world.pauses().len(), 30);
    assert_eq!(gave_up.attempts, 31);
    assert!(matches!(gave_up.last, Error::Settings(_)));
}

/// A run that gave up on the network installs on the next start, and a stale staged file from a
/// run that was cut is replaced.
#[test]
fn a_later_start_installs_after_the_network_returns_and_replaces_a_stale_file() {
    let dir = scratch("run-recover");
    let paths = paths_in(dir.path(), "");
    write_mode(
        &paths.staging_dir.join("hostkeeper.pkg"),
        "half a download",
        0o600,
    );
    let world = World::good();
    world.repo.down.set(true);
    let first = world.run(&paths).expect_err("the network is down");
    assert_eq!(first.attempts, 31);
    world.repo.down.set(false);
    assert!(matches!(world.run(&paths), Ok(Outcome::Installed { .. })));
    assert_eq!(
        *world.tools.seen_pkg.lock().expect("seen"),
        [PKG_BYTES.to_vec()],
        "the stale file was replaced"
    );
}

/// A staging directory that cannot be made, or whose path is not UTF-8, is an error of the
/// attempt, and nothing is fetched.
#[test]
fn staging_problems_are_errors_of_the_attempt() {
    let dir = scratch("run-staging");
    let mut paths = paths_in(dir.path(), no_wait());
    write_mode(&dir.path().join("a-file"), "x", 0o600);
    paths.staging_dir = dir.path().join("a-file/staging");
    let world = World::good();
    let gave_up = world.run(&paths).expect_err("no staging");
    assert!(matches!(gave_up.last, Error::Staging { .. }), "{gave_up}");
    assert!(world.repo.asked.borrow().is_empty());
    paths.staging_dir = dir.path().join(OsStr::from_bytes(b"caf\xe9"));
    let gave_up = world.run(&paths).expect_err("not UTF-8");
    assert!(matches!(gave_up.last, Error::NotUtf8(_)), "{gave_up}");
    assert!(gave_up.last.to_string().ends_with("is not UTF-8"));
}

/// A config that cannot be written (a directory where the file goes) stops the attempt before
/// `installer` runs.
#[test]
fn a_config_that_cannot_be_written_stops_before_the_installer() {
    let dir = scratch("run-config");
    let paths = paths_in(dir.path(), no_wait());
    fs::create_dir_all(paths.config_dir.join("hostkeeper.toml")).expect("dir");
    let world = World::good();
    let gave_up = world.run(&paths).expect_err("refused");
    assert!(matches!(gave_up.last, Error::File(_)), "{gave_up}");
    assert_eq!(world.tools.ran(), [Tool::Pkgutil]);
}

/// The real pause returns; a pause of no seconds is the one a test can take.
#[test]
fn the_thread_pause_returns() {
    use hostkeeper_bootstrap::run::{Pause, ThreadPause};
    ThreadPause.pause(0);
}
