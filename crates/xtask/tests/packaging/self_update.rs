//! The two units as hostkeeper's own update relies on them (design section 6.6, two-programs.md
//! section 4.1): the bootstrap unit starts the installed binary, one of the paths a daemon
//! seeds from (`hostkeeper_daemon::packaged::PACKAGED_BINARIES`), which gives the binary
//! directory its first binary and execs `current`; the package unit seeds that directory itself
//! and starts `current` of the daemon's binary directory (`hostkeeper_daemon::selfupdate::
//! binary_dir`) under the state directory it gives the daemon. Both restart the status the
//! daemon exits with once it has handed over to a new binary; and neither sets a start limit,
//! since the daemon removes the drop-in and the fallback unit of the releases before the
//! hand-over (`hostkeeper_daemon::selfunits`) and a daemon that cannot start is retried for good.

use std::path::{Path, PathBuf};

use hostkeeper_daemon::cli::{DEFAULT_STATE_DIR, exit_status};
use hostkeeper_daemon::packaged::PACKAGED_BINARIES;
use hostkeeper_daemon::selfunits::{FALLBACK_UNIT, MAIN_UNIT};
use hostkeeper_daemon::selfupdate::binary_dir;

use crate::daemon_flags::{parse_flags, parse_seed};
use crate::unit::{self, Unit};
use crate::{INSTALLED_UNIT, Kind, SEED_LINE, altered, config, refuses};

/// The words of the command line of `key` in `[Service]`, its `-` prefix dropped.
fn words(unit: &Unit, key: &str) -> Result<Vec<String>, String> {
    let line = unit.single("Service", key)?;
    unit::command_words(line.strip_prefix('-').unwrap_or(line))
}

/// The program `ExecStart=` runs, and the state directory it gives the daemon.
fn start_of(unit: &Unit) -> Result<(PathBuf, PathBuf), String> {
    let words = words(unit, "ExecStart")?;
    let flags = parse_flags(&words)?;
    Ok((PathBuf::from(&words[0]), flags.state_dir))
}

fn check_start(kind: Kind, text: &str) -> Result<(), String> {
    let unit = Unit::parse(text)?;
    let (program, state) = start_of(&unit)?;
    match kind {
        Kind::Bootstrap => {
            let installed = installed_binary()?;
            if !PACKAGED_BINARIES.iter().any(|p| Path::new(p) == program) {
                return Err(format!(
                    "ExecStart= runs {}, not one of the packaged binaries {PACKAGED_BINARIES:?}, which a daemon seeds from",
                    program.display()
                ));
            }
            if program != Path::new(&installed) {
                return Err(format!(
                    "ExecStart= runs {}, not the installed {installed}",
                    program.display()
                ));
            }
        }
        Kind::Package => {
            let current = binary_dir(&state).join("current");
            if program != current {
                return Err(format!(
                    "ExecStart= runs {}, not {}, `current` of the binary directory of its --state-dir",
                    program.display(),
                    current.display()
                ));
            }
        }
    }
    if state != Path::new(DEFAULT_STATE_DIR) {
        return Err(format!(
            "--state-dir is {}, not the daemon's default {DEFAULT_STATE_DIR}",
            state.display()
        ));
    }
    let restart = unit.single("Service", "Restart")?;
    if restart != "always" {
        return Err(format!("Restart={restart}, not always"));
    }
    Ok(())
}

/// The design's named test for the units. Catches a bootstrap unit that does not start the
/// binary the packages install (`current`, which only the package unit starts; a path a daemon
/// does not seed from; another packaged path than the deb's), a package unit that does not start
/// the binary the self-update switches (the deb's `/usr/bin/hostkeeper`, the package's own
/// `packages/hostkeeper/current/bin/hostkeeper`, `current` under another state directory than
/// the one it gives the daemon), the package unit built from the bootstrap file and the other
/// way about, or either that does not restart the daemon's clean exit.
#[test]
fn the_bootstrap_unit_starts_the_installed_binary_and_the_package_unit_starts_current_with_restart_always()
 {
    let bootstrap = Kind::Bootstrap.read();
    let package = Kind::Package.read();
    assert_eq!(check_start(Kind::Bootstrap, &bootstrap), Ok(()));
    assert_eq!(check_start(Kind::Package, &package), Ok(()));
    let current = "/var/lib/updater/state/hostkeeper/bin/current";
    let started = format!("ExecStart={current} daemon");
    let installed = "ExecStart=/usr/bin/hostkeeper daemon";
    let bootstrap_cases = [
        (
            altered(&bootstrap, installed, &started),
            "ExecStart= runs /var/lib/updater/state/hostkeeper/bin/current, not one of the packaged binaries",
        ),
        (
            altered(&bootstrap, installed, "ExecStart=/opt/hostkeeper daemon"),
            "ExecStart= runs /opt/hostkeeper, not one of the packaged binaries",
        ),
        (
            altered(
                &bootstrap,
                installed,
                "ExecStart=/usr/local/bin/hostkeeper daemon",
            ),
            "ExecStart= runs /usr/local/bin/hostkeeper, not the installed /usr/bin/hostkeeper",
        ),
        (package.clone(), "not one of the packaged binaries"),
        (
            altered(&bootstrap, "Restart=always", "Restart=on-failure"),
            "Restart=on-failure, not always",
        ),
        (
            altered(
                &bootstrap,
                "toml --state-dir /var/lib/updater/state --packages-dir",
                "toml --state-dir /srv --packages-dir",
            ),
            "--state-dir is /srv, not the daemon's default",
        ),
    ];
    refuses(|t: &str| check_start(Kind::Bootstrap, t), &bootstrap_cases);
    let package_cases = [
        (
            altered(&package, &started, installed),
            "ExecStart= runs /usr/bin/hostkeeper, not /var/lib/updater/state/hostkeeper/bin/current",
        ),
        (
            altered(
                &package,
                &started,
                "ExecStart=/var/lib/hostkeeper/packages/hostkeeper/current/bin/hostkeeper daemon",
            ),
            "ExecStart= runs /var/lib/hostkeeper/packages/hostkeeper/current/bin/hostkeeper",
        ),
        (
            altered(
                &package,
                "toml --state-dir /var/lib/updater/state --packages-dir",
                "toml --state-dir /var/lib/updater/other --packages-dir",
            ),
            "not /var/lib/updater/other/hostkeeper/bin/current",
        ),
        (
            altered(
                &altered(
                    &package,
                    &started,
                    "ExecStart=/srv/hostkeeper/bin/current daemon",
                ),
                "toml --state-dir /var/lib/updater/state --packages-dir",
                "toml --state-dir /srv --packages-dir",
            ),
            "--state-dir is /srv, not the daemon's default",
        ),
        (
            altered(&package, "Restart=always", "Restart=on-failure"),
            "Restart=on-failure, not always",
        ),
        (
            bootstrap.clone(),
            "ExecStart= runs /usr/bin/hostkeeper, not /var/lib/updater/state/hostkeeper/bin/current",
        ),
    ];
    refuses(|t: &str| check_start(Kind::Package, t), &package_cases);
}

/// The binary the deb and the rpm install, as `nfpm.yaml` places it.
fn installed_binary() -> Result<String, String> {
    let files = config().files("deb")?;
    files
        .into_iter()
        .find(|f| {
            f.src
                == format!(
                    "target/nfpm/{}",
                    xtask::test_support::LINUX_PKG_PROGRAMS[0].1
                )
        })
        .map(|f| f.dst)
        .ok_or_else(|| "the packages carry no daemon binary".to_owned())
}

fn check_seed(kind: Kind, text: &str) -> Result<(), String> {
    let unit = Unit::parse(text)?;
    if kind == Kind::Bootstrap {
        return match unit.values("Service", "ExecStartPre").is_empty() {
            true => Ok(()),
            false => Err(
                "the bootstrap unit has an ExecStartPre= seed; the daemon it starts seeds itself"
                    .to_owned(),
            ),
        };
    }
    let (_, state) = start_of(&unit)?;
    let words = words(&unit, "ExecStartPre")?;
    let seed = parse_seed(&words)?;
    if seed.state_dir != state {
        return Err(format!(
            "the seed fills {}, and the daemon's state directory is {}",
            seed.state_dir.display(),
            state.display()
        ));
    }
    let installed = installed_binary()?;
    let program = Path::new(&words[0]);
    if (program != Path::new(&installed)) | (seed.binary != Path::new(&installed)) {
        return Err(format!(
            "the seed is run by {} with --binary {}, not by and with the installed {installed}",
            program.display(),
            seed.binary.display()
        ));
    }
    Ok(())
}

/// Catches a first start with nothing to start: a seed in the package unit that fills another
/// state directory than the daemon's, or that is not the binary the packages install, run with
/// that binary (another path is not on every host, and would stage something else as this
/// version).
#[test]
fn the_package_units_seed_fills_the_daemons_binary_directory_from_the_installed_binary() {
    let text = Kind::Package.read();
    assert_eq!(installed_binary().as_deref(), Ok("/usr/bin/hostkeeper"));
    assert_eq!(check_seed(Kind::Package, &text), Ok(()));
    let cases = [
        (
            altered(
                &text,
                "seed --state-dir /var/lib/updater/state",
                "seed --state-dir /var/lib/updater/other",
            ),
            "the seed fills /var/lib/updater/other",
        ),
        (
            altered(
                &text,
                "--binary /usr/bin/hostkeeper",
                "--binary /usr/local/bin/hostkeeper",
            ),
            "with --binary /usr/local/bin/hostkeeper",
        ),
        (
            altered(
                &text,
                "ExecStartPre=-/usr/bin/hostkeeper",
                "ExecStartPre=-/opt/hostkeeper",
            ),
            "the seed is run by /opt/hostkeeper",
        ),
        (
            altered(&text, SEED_LINE, ""),
            "[Service] has no ExecStartPre=",
        ),
    ];
    refuses(|t: &str| check_seed(Kind::Package, t), &cases);
}

/// Catches a bootstrap unit that seeds: the daemon it starts seeds the binary directory itself
/// (`hostkeeper_daemon::packaged`), so an `ExecStartPre=` there is a second seeder of the same
/// directory, and the package unit built from the wrong file would have no seed.
#[test]
fn the_bootstrap_unit_runs_no_seed() {
    let text = Kind::Bootstrap.read();
    assert_eq!(check_seed(Kind::Bootstrap, &text), Ok(()));
    let seeded = altered(&text, "ExecStart=", &format!("{}ExecStart=", SEED_LINE));
    refuses(
        |t: &str| check_seed(Kind::Bootstrap, t),
        &[(seeded, "the bootstrap unit has an ExecStartPre= seed")],
    );
    let package = Kind::Package.read();
    refuses(
        |t: &str| check_seed(Kind::Bootstrap, t),
        &[(package, "the bootstrap unit has an ExecStartPre= seed")],
    );
}

/// Whether systemd restarts a service of `policy` that exited with `status` by itself, as
/// systemd.service(5) tables `Restart=`: a clean exit (status 0; the unit sets no
/// `SuccessExitStatus=`) is restarted by `always` and `on-success`, an unclean exit code by
/// `always` and `on-failure`; `no`, `on-abnormal`, `on-abort` and `on-watchdog` restart no exit
/// code. Another value is refused.
fn restarts(policy: &str, status: u8) -> Result<bool, String> {
    let clean = status == 0;
    match policy {
        "always" => Ok(true),
        "on-success" => Ok(clean),
        "on-failure" => Ok(!clean),
        "no" | "on-abnormal" | "on-abort" | "on-watchdog" => Ok(false),
        other => Err(format!("Restart={other} is not a policy systemd knows")),
    }
}

fn check_restart(text: &str) -> Result<(), String> {
    let unit = Unit::parse(text)?;
    let policy = unit.single("Service", "Restart")?;
    // What `hostkeeper daemon` exits with once its own update asked it to exit: the server
    // returned `Ok` (the daemon test `the_daemon_stops_serving_once_its_restart_is_asked`).
    let status = exit_status(&Ok(()));
    match restarts(policy, status)? {
        true => Ok(()),
        false => Err(format!(
            "Restart={policy} does not restart the daemon's exit status {status} after it staged a new binary"
        )),
    }
}

/// The design's named test for the restart. Catches a restart policy under which the daemon's
/// exit after its own update, a clean exit, is final, so the new binary never runs and the host
/// stays down (`on-failure`, the policy the unit had; `no`; `on-abnormal`); the status is the one
/// the daemon's command line maps that exit to, not a constant here.
#[test]
fn each_units_restart_policy_restarts_the_exit_status_of_the_daemon_after_staging() {
    for kind in Kind::ALL {
        let text = kind.read();
        assert_eq!(check_restart(&text), Ok(()));
        assert_eq!(exit_status(&Ok(())), 0);
        let cases = [
            (
                altered(&text, "Restart=always", "Restart=on-failure"),
                "Restart=on-failure does not restart the daemon's exit status 0",
            ),
            (
                altered(&text, "Restart=always", "Restart=no"),
                "Restart=no does not restart",
            ),
            (
                altered(&text, "Restart=always", "Restart=on-abnormal"),
                "Restart=on-abnormal does not restart",
            ),
            (
                altered(&text, "Restart=always", "Restart=sometimes"),
                "Restart=sometimes is not a policy systemd knows",
            ),
        ];
        refuses(check_restart, &cases);
    }
    // `on-success` restarts a clean exit as well; it is `unit_file.rs` that holds the unit to
    // `always`, which also restarts a crash.
    assert_eq!(restarts("on-success", 0), Ok(true));
    assert_eq!(restarts("on-success", 1), Ok(false));
    assert_eq!(restarts("on-failure", 1), Ok(true));
    assert_eq!(restarts("always", 1), Ok(true));
    assert_eq!(restarts("on-watchdog", 1), Ok(false));
    assert_eq!(restarts("on-abort", 0), Ok(false));
}

/// Whether the unit `text` retries a daemon that cannot start for good: it sets no start limit
/// of its own, so with the drop-in the releases before the hand-over wrote removed (the daemon
/// removes it at its start) systemd never leaves the unit failed, and it waits one second
/// between starts.
fn check_retries_for_good(text: &str) -> Result<(), String> {
    let unit = Unit::parse(text)?;
    let interval = unit.single("Unit", "StartLimitIntervalSec")?;
    if (interval != "0") | !unit.values("Unit", "StartLimitBurst").is_empty() {
        return Err(format!(
            "the unit sets a start limit (StartLimitIntervalSec={interval}); with no fallback unit to start it again, a daemon that fails it is left failed"
        ));
    }
    let delay = unit.single("Service", "RestartSec")?;
    if delay != "1" {
        return Err(format!(
            "RestartSec={delay}, not the 1 second that paces the tries of a hand-over"
        ));
    }
    Ok(())
}

/// Catches a unit that leaves the daemon failed: the hand-over has no fallback unit to start the
/// daemon again after a start limit, so a unit with a start limit of its own (an interval, or a
/// burst) would stop retrying a daemon that cannot start, and a restart delay other than one
/// second changes how fast the three tries of a hand-over are spent. The unit is held to the
/// drop-in's removal by the daemon's own test (`tests/self_units.rs` of `hostkeeper-daemon`).
#[test]
fn each_unit_retries_a_daemon_that_cannot_start_for_good_with_no_fallback_unit() {
    assert_eq!(FALLBACK_UNIT, "hostkeeper-fallback.service");
    assert_eq!(
        Path::new(INSTALLED_UNIT).file_name(),
        Some(MAIN_UNIT.as_ref())
    );
    for kind in Kind::ALL {
        let text = kind.read();
        assert_eq!(check_retries_for_good(&text), Ok(()));
        let cases = [
            (
                altered(&text, "RestartSec=1", "RestartSec=5"),
                "RestartSec=5, not the 1 second",
            ),
            (
                altered(
                    &text,
                    "StartLimitIntervalSec=0",
                    "StartLimitIntervalSec=300",
                ),
                "the unit sets a start limit (StartLimitIntervalSec=300)",
            ),
            (
                altered(
                    &text,
                    "StartLimitIntervalSec=0\n",
                    "StartLimitIntervalSec=0\nStartLimitBurst=3\n",
                ),
                "the unit sets a start limit (StartLimitIntervalSec=0)",
            ),
            (
                altered(&text, "RestartSec=1\n", ""),
                "[Service] has no RestartSec=",
            ),
        ];
        refuses(check_retries_for_good, &cases);
    }
}
