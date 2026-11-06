//! The unit as hostkeeper's own update relies on it (design section 6.6): it starts `current` of
//! the daemon's binary directory (`hostkeeper_daemon::selfupdate::binary_dir`) under the state
//! directory it gives the daemon; its seed fills that directory from the binary the packages
//! install; its restart policy restarts the status the daemon exits with once it has staged a new
//! binary; and it agrees with the drop-in and the fallback unit the daemon writes at its start
//! (`hostkeeper_daemon::selfunits`).

use std::path::{Path, PathBuf};

use hostkeeper_daemon::cli::{Command, DEFAULT_STATE_DIR, exit_status};
use hostkeeper_daemon::selfunits::{FALLBACK_UNIT, MAIN_UNIT};
use hostkeeper_daemon::selfupdate::binary_dir;
use hostkeeper_steps::Layout;
use hostkeeper_steps::selfupdate::{START_LIMIT_BURST, fallback_unit, main_dropin};

use crate::daemon_flags::{parse, parse_flags, parse_seed};
use crate::unit::{self, Unit};
use crate::{INSTALLED_UNIT, UNIT, altered, config, read, refuses};

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

fn check_start(text: &str) -> Result<(), String> {
    let unit = Unit::parse(text)?;
    let (program, state) = start_of(&unit)?;
    let current = binary_dir(&state).join("current");
    if program != current {
        return Err(format!(
            "ExecStart= runs {}, not {}, `current` of the binary directory of its --state-dir",
            program.display(),
            current.display()
        ));
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

/// The design's named test for the unit. Catches a unit that does not start the binary the
/// self-update switches (the deb's `/usr/bin/hostkeeper`, the package's own
/// `packages/hostkeeper/current/bin/hostkeeper`, `current` under another state directory than
/// the one it gives the daemon), or that does not restart the daemon's clean exit.
#[test]
fn the_unit_starts_current_of_the_daemons_binary_directory_with_restart_always() {
    let text = read(UNIT);
    assert_eq!(check_start(&text), Ok(()));
    let started = "ExecStart=/var/lib/updater/state/hostkeeper/bin/current daemon";
    let cases = [
        (
            altered(&text, started, "ExecStart=/usr/bin/hostkeeper daemon"),
            "ExecStart= runs /usr/bin/hostkeeper, not /var/lib/updater/state/hostkeeper/bin/current",
        ),
        (
            altered(
                &text,
                started,
                "ExecStart=/var/lib/hostkeeper/packages/hostkeeper/current/bin/hostkeeper daemon",
            ),
            "ExecStart= runs /var/lib/hostkeeper/packages/hostkeeper/current/bin/hostkeeper",
        ),
        (
            altered(
                &text,
                "toml --state-dir /var/lib/updater/state --packages-dir",
                "toml --state-dir /var/lib/updater/other --packages-dir",
            ),
            "not /var/lib/updater/other/hostkeeper/bin/current",
        ),
        (
            altered(
                &altered(
                    &text,
                    started,
                    "ExecStart=/srv/hostkeeper/bin/current daemon",
                ),
                "toml --state-dir /var/lib/updater/state --packages-dir",
                "toml --state-dir /srv --packages-dir",
            ),
            "--state-dir is /srv, not the daemon's default",
        ),
        (
            altered(&text, "Restart=always", "Restart=on-failure"),
            "Restart=on-failure, not always",
        ),
    ];
    refuses(check_start, &cases);
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

fn check_seed(text: &str) -> Result<(), String> {
    let unit = Unit::parse(text)?;
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

/// Catches a first start with nothing to start: a seed that fills another state directory than
/// the daemon's, or that is not the binary the packages install, run with that binary (another
/// path is not on every host, and would stage something else as this version).
#[test]
fn the_seed_fills_the_daemons_binary_directory_from_the_installed_binary() {
    let text = read(UNIT);
    assert_eq!(installed_binary().as_deref(), Ok("/usr/bin/hostkeeper"));
    assert_eq!(check_seed(&text), Ok(()));
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
    ];
    refuses(check_seed, &cases);
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
fn the_restart_policy_restarts_the_exit_status_of_the_daemon_after_staging() {
    let text = read(UNIT);
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
const SHARED: [(&str, &str); 4] = [
    ("Service", "Restart"),
    ("Service", "RestartSec"),
    ("Unit", "StartLimitIntervalSec"),
    ("Unit", "StartLimitBurst"),
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

/// Catches the unit and the files the daemon writes for it drifting apart: a restart policy, a
/// restart delay or a start limit the unit sets differently from the drop-in (which would win on a
/// host, and differ from what the unit says), and a fallback unit that does not run the previous
/// binary's fallback verb on the binary directory the unit starts from. The drop-in is for the
/// unit the packages install, under its name.
#[test]
fn the_unit_agrees_with_the_dropin_and_fallback_unit_the_daemon_writes() {
    let text = read(UNIT);
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
            altered(&text, "StartLimitBurst=3", "StartLimitBurst=5"),
            "the unit sets StartLimitBurst=5 and the drop-in StartLimitBurst=3",
        ),
        (
            altered(&text, "RestartSec=1\n", ""),
            "[Service] has no RestartSec=",
        ),
    ];
    refuses(check_agreement, &cases);
}
