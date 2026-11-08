//! The two units as hostkeeper's own update relies on them (design section 6.6, two-programs.md
//! section 4.1): the bootstrap unit starts the installed binary, one of the paths a daemon
//! seeds from (`hostkeeper_daemon::packaged::PACKAGED_BINARIES`), which gives the binary
//! directory its first binary and execs `current`; the package unit seeds that directory itself
//! and starts `current` of the daemon's binary directory (`hostkeeper_daemon::selfupdate::
//! binary_dir`) under the state directory it gives the daemon. Both restart the status the
//! daemon exits with once it has staged a new binary; both agree with the drop-in and the
//! fallback unit the daemon writes at its start (`hostkeeper_daemon::selfunits`), the drop-in
//! alone setting the start limit; and a start limit the daemon fails ends with the daemon
//! started again, never with the unit left failed.

use std::path::{Path, PathBuf};

use hostkeeper_daemon::cli::{Command, DEFAULT_STATE_DIR, EXIT_FAILED, exit_status};
use hostkeeper_daemon::packaged::PACKAGED_BINARIES;
use hostkeeper_daemon::selfunits::{FALLBACK_UNIT, MAIN_UNIT};
use hostkeeper_daemon::selfupdate::binary_dir;
use hostkeeper_steps::Layout;
use hostkeeper_steps::selfupdate::{
    START_LIMIT_BURST, START_LIMIT_WINDOW_SECS, fallback_unit, main_dropin,
};

use crate::daemon_flags::{parse, parse_flags, parse_seed};
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
        .find(|f| f.src == "target/release/hostkeeper")
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

/// The keys the unit and the drop-in both set, which must agree.
const SHARED: [(&str, &str); 3] = [
    ("Service", "Restart"),
    ("Service", "RestartSec"),
    ("Service", "RestartMode"),
];

fn check_agreement(text: &str) -> Result<(), String> {
    let unit = Unit::parse(text)?;
    let dropin = Unit::parse(&main_dropin(FALLBACK_UNIT, START_LIMIT_BURST))?;
    for (section, key) in SHARED {
        let (ours, theirs) = (unit.single(section, key)?, dropin.single(section, key)?);
        if ours != theirs {
            return Err(format!(
                "the unit sets {key}={ours} and the drop-in {key}={theirs}"
            ));
        }
    }
    // The start limit is the drop-in's alone: with no drop-in yet (before the daemon was first
    // ready) there is no fallback for a limit to start, so the unit sets none of its own.
    let interval = unit.single("Unit", "StartLimitIntervalSec")?;
    if (interval != "0") | !unit.values("Unit", "StartLimitBurst").is_empty() {
        return Err(format!(
            "the unit sets a start limit of its own (StartLimitIntervalSec={interval}); only the drop-in, whose OnFailure= fallback starts it again, may"
        ));
    }
    let window = dropin.single("Unit", "StartLimitIntervalSec")?;
    let burst = dropin.single("Unit", "StartLimitBurst")?;
    if (window, burst) != (START_LIMIT_WINDOW_SECS.to_string().as_str(), "3") {
        return Err(format!(
            "the drop-in allows StartLimitBurst={burst} within StartLimitIntervalSec={window}, not 3 within 300"
        ));
    }
    let on_failure = dropin.single("Unit", "OnFailure")?;
    if on_failure != FALLBACK_UNIT {
        return Err(format!("the drop-in's OnFailure= is {on_failure}"));
    }
    let (_, state) = start_of(&unit)?;
    let dir = binary_dir(&state);
    let fallback = Unit::parse(&fallback_unit(&Layout::new(&dir), MAIN_UNIT))?;
    let words = words(&fallback, "ExecStart")?;
    let runs_previous = Path::new(&words[0]) == dir.join("previous");
    let verb = parse(&words)?;
    if !runs_previous | !matches!(&verb, Command::SelfUpdateFallback(flags) if flags.dir == dir) {
        return Err(format!(
            "the fallback unit runs {words:?}, not the previous binary's fallback of {}",
            dir.display()
        ));
    }
    Ok(())
}

/// Catches the unit and the files the daemon writes for it drifting apart: a restart policy or a
/// restart delay the unit sets differently from the drop-in (which would win on a host, and
/// differ from what the unit says), a start limit in the unit itself (which, before the drop-in
/// exists, would leave a daemon that fails three starts failed with no fallback to start it
/// again), a drop-in whose bound is not the 3 starts within 300 seconds of design section 6.6,
/// and a fallback unit that does not run the previous binary's fallback verb on the binary
/// directory the unit starts from. The drop-in is for the unit the packages install, under its
/// name.
#[test]
fn each_unit_agrees_with_the_dropin_and_fallback_unit_the_daemon_writes() {
    for kind in Kind::ALL {
        let text = kind.read();
        assert_eq!(check_agreement(&text), Ok(()));
        assert_eq!(
            Path::new(INSTALLED_UNIT).file_name(),
            Some(MAIN_UNIT.as_ref())
        );
        let cases = [
            (
                altered(&text, "RestartSec=1", "RestartSec=5"),
                "the unit sets RestartSec=5 and the drop-in RestartSec=1",
            ),
            (
                altered(&text, "RestartMode=direct\n", "RestartMode=normal\n"),
                "the unit sets RestartMode=normal and the drop-in RestartMode=direct",
            ),
            (
                altered(
                    &text,
                    "StartLimitIntervalSec=0",
                    "StartLimitIntervalSec=300",
                ),
                "the unit sets a start limit of its own (StartLimitIntervalSec=300)",
            ),
            (
                altered(
                    &text,
                    "StartLimitIntervalSec=0\n",
                    "StartLimitIntervalSec=0\nStartLimitBurst=3\n",
                ),
                "the unit sets a start limit of its own (StartLimitIntervalSec=0)",
            ),
            (
                altered(&text, "RestartSec=1\n", ""),
                "[Service] has no RestartSec=",
            ),
        ];
        refuses(check_agreement, &cases);
    }
}

/// What systemd leaves of the daemon's unit once it has failed the start limit of the drop-in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AfterLimit {
    /// Its failed state reset and the unit started again.
    StartedAgain,
    /// Failed, until someone runs `systemctl reset-failed`.
    LeftFailed,
}

/// What follows the daemon's unit `main` failing its start limit, as systemd runs it
/// (systemd.unit(5) `OnFailure=`, systemd.service(5) `ExecStart=`, `ExecStartPost=`,
/// `RestartMode=`), and as the privileged test
/// `a_unit_that_spends_its_start_limit_with_no_update_waiting_is_started_again` sees it under
/// the host's systemd: the drop-in's `OnFailure=` unit, `fallback`, is started; its `ExecStart=`
/// verb exits `status`; a failure ends the unit before its `ExecStartPost=` lines unless the
/// line has the `-` prefix; then each `ExecStartPost=` line runs in order. `systemctl start` of
/// a unit that failed its start limit is refused until a `systemctl reset-failed` of it has run.
/// Without `RestartMode=direct` in the drop-in, `OnFailure=` also runs at every failed start
/// before the limit; the fallback, run again at each, then spends a start limit of its own
/// (systemd's default, 5 starts within 10 seconds) unless it has none
/// (`StartLimitIntervalSec=0`), and once it is refused the unit is left failed.
fn after_limit(dropin: &str, fallback: &str, main: &str, status: u8) -> Result<AfterLimit, String> {
    let dropin = Unit::parse(dropin)?;
    let on_failure = dropin.single("Unit", "OnFailure")?;
    if on_failure != FALLBACK_UNIT {
        return Err(format!(
            "the drop-in starts {on_failure}, not {FALLBACK_UNIT}"
        ));
    }
    let fallback = Unit::parse(fallback)?;
    let direct = dropin.values("Service", "RestartMode") == ["direct"];
    let unlimited = fallback.values("Unit", "StartLimitIntervalSec") == ["0"];
    if !direct & !unlimited {
        return Ok(AfterLimit::LeftFailed);
    }
    let ignores_failure = fallback.single("Service", "ExecStart")?.starts_with('-');
    if (status != 0) & !ignores_failure {
        return Ok(AfterLimit::LeftFailed);
    }
    let mut failed = true;
    let mut started = false;
    for line in fallback.values("Service", "ExecStartPost") {
        let words = unit::command_words(line)?;
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        match words.as_slice() {
            ["/usr/bin/systemctl", "reset-failed", unit] if *unit == main => failed = false,
            ["/usr/bin/systemctl", "start", "--no-block", unit] if *unit == main => {
                started = !failed;
            }
            _ => {}
        }
    }
    Ok(if started {
        AfterLimit::StartedAgain
    } else {
        AfterLimit::LeftFailed
    })
}

/// The fallback unit the daemon writes for the unit the packages install.
fn written_fallback(text: &str) -> String {
    let unit = Unit::parse(text).expect("the unit parses");
    let (_, state) = start_of(&unit).expect("the unit's command line");
    fallback_unit(&Layout::new(&binary_dir(&state)), MAIN_UNIT)
}

/// Catches the dead end of a unit left failed: three failed starts within 300 seconds with no
/// self-update waiting for its restart (a config that does not load, a crash at each start)
/// start the fallback, whose verb then puts nothing back and exits [`EXIT_FAILED`]; the daemon's
/// unit is still reset and started again, as it is when the verb did put the previous binary
/// back (exit 0). A fallback unit whose verb's failure ends it, that starts the unit before it
/// resets it, that does not reset it, or that starts another unit, leaves the daemon failed, and
/// so does a drop-in without `RestartMode=direct` with a fallback that has a start limit.
#[test]
fn three_failed_starts_of_either_unit_with_no_update_waiting_start_the_daemon_again() {
    for kind in Kind::ALL {
        let text = kind.read();
        let dropin = main_dropin(FALLBACK_UNIT, START_LIMIT_BURST);
        let fallback = written_fallback(&text);
        let after = |fallback: &str, status| after_limit(&dropin, fallback, MAIN_UNIT, status);
        assert_eq!(after(&fallback, EXIT_FAILED), Ok(AfterLimit::StartedAgain));
        assert_eq!(after(&fallback, 0), Ok(AfterLimit::StartedAgain));
        assert_eq!(EXIT_FAILED, 1);

        let reset = format!("ExecStartPost=/usr/bin/systemctl reset-failed {MAIN_UNIT}\n");
        let start = format!("ExecStartPost=/usr/bin/systemctl start --no-block {MAIN_UNIT}\n");
        let unignored = altered(&fallback, "ExecStart=-/", "ExecStart=/");
        let swapped = altered(
            &altered(&fallback, &reset, ""),
            &start,
            &format!("{start}{reset}"),
        );
        let unreset = altered(&fallback, &reset, "");
        let limited = altered(&fallback, "StartLimitIntervalSec=0\n", "");
        let normal = altered(&dropin, "RestartMode=direct\n", "");
        let other = altered(
            &fallback,
            &start,
            "ExecStartPost=/usr/bin/systemctl start --no-block other.service\n",
        );
        for (text, case) in [
            (&unignored, "the verb's failure ends the fallback"),
            (&swapped, "started before it is reset"),
            (&unreset, "never reset"),
            (&other, "another unit started"),
        ] {
            assert_eq!(
                after(text, EXIT_FAILED),
                Ok(AfterLimit::LeftFailed),
                "{case}"
            );
        }
        // With both the drop-in's `RestartMode=direct` and the fallback's `StartLimitIntervalSec=0`
        // gone, the fallback spends its own start limit and the unit is left failed; either alone
        // keeps it running.
        let spent = after_limit(&normal, &limited, MAIN_UNIT, EXIT_FAILED);
        assert_eq!(spent, Ok(AfterLimit::LeftFailed));
        assert_eq!(after(&limited, EXIT_FAILED), Ok(AfterLimit::StartedAgain));
        let unlimited_only = after_limit(&normal, &fallback, MAIN_UNIT, EXIT_FAILED);
        assert_eq!(unlimited_only, Ok(AfterLimit::StartedAgain));
        // Without the failure, the verb that put the previous binary back still starts the unit.
        assert_eq!(after(&unignored, 0), Ok(AfterLimit::StartedAgain));
        let elsewhere = main_dropin("other.service", START_LIMIT_BURST);
        assert_eq!(
            after_limit(&elsewhere, &fallback, MAIN_UNIT, EXIT_FAILED),
            Err(format!(
                "the drop-in starts other.service, not {FALLBACK_UNIT}"
            ))
        );
    }
}
