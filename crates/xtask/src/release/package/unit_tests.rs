//! The committed unit of the daemon (`packaging/linux/hostkeeper.service`), as hostkeeper's own
//! package carries it, launched the way systemd launches its `ExecStart=`: through `current` of
//! the binary directory that the self-update fills from the package's `bin/hostkeeper`.

use std::path::{Path, PathBuf};

use super::Platform;
use super::tests::{BINARY, unpacked_tree};
use crate::release::fakes::scratch;

/// The daemon's one unit, as committed (the release workflow passes it to `release package
/// --unit`, and the deb, the rpm and the overlay install it).
const UNIT: &str = include_str!("../../../../../packaging/linux/hostkeeper.service");

/// What a launcher starts for the `ExecStart=` of `unit` on a host whose state directory is
/// `state`: the program, with the unit's own `--state-dir` rebased onto `state` and every link on
/// its path followed, and the arguments. Fails naming what the unit lacks, or when the program is
/// not under its state directory or does not resolve to a file.
fn launched(unit: &str, state: &Path) -> Result<(PathBuf, Vec<String>), String> {
    let line = unit
        .lines()
        .find_map(|l| l.strip_prefix("ExecStart="))
        .ok_or("the unit has no ExecStart=")?;
    let mut words = line.split_whitespace().map(str::to_owned);
    let program = words.next().ok_or("ExecStart= names no program")?;
    let args: Vec<String> = words.collect();
    let dir = args
        .windows(2)
        .find(|w| w[0] == "--state-dir")
        .map(|w| format!("{}/", w[1]))
        .ok_or("ExecStart= has no --state-dir")?;
    let inside = program
        .strip_prefix(&dir)
        .ok_or_else(|| format!("{program} is not under the --state-dir {dir}"))?;
    let path = std::fs::canonicalize(state.join(inside))
        .map_err(|e| format!("{program} does not resolve: {e}"))?;
    Ok((path, args))
}

#[test]
fn the_committed_unit_launches_the_binary_the_self_update_put_in_current() {
    // Catches: a unit whose ExecStart does not reach the binary the self-update staged from the
    // package (the deb's /usr/bin path, the package's own `current/bin`, a path outside
    // --state-dir, a broken link), one that still starts the old binary after the switch, or
    // whose arguments the daemon refuses.
    use clap::Parser;
    use hostkeeper_daemon::selfupdate::binary_dir;
    use hostkeeper_steps::Layout;
    use std::os::unix::fs::PermissionsExt;

    let dir = scratch("package-unit-launch");
    let (packages, running, unpacked, unseal) = unpacked_tree(
        &dir,
        Platform::LinuxX86_64,
        BINARY,
        UNIT,
        hostkeeper_unpack::Platform::Linux,
    );
    assert_eq!(unpacked.manifest.run, ["hostkeeper.service"]);
    let release = packages.join("hostkeeper").join(&running);
    let unit = std::fs::read_to_string(release.join("units/hostkeeper.service"))
        .expect("the unpacked unit");
    assert_eq!(unit, UNIT);

    // Before the binary directory has a `current`, the unit starts nothing.
    let state = dir.join("state");
    let refused = launched(&unit, &state).expect_err("no current yet");
    assert!(refused.contains("does not resolve"), "{refused}");

    // The self-update's follow-on: the package's binary staged, `current` switched to it.
    let layout = Layout::new(&binary_dir(&state));
    layout
        .stage(&running, &release.join("bin/hostkeeper"))
        .expect("staged");
    layout.switch(&running).expect("switched");
    let (program, args) = launched(&unit, &state).expect("a launch");
    let staged = std::fs::canonicalize(layout.version_path(&running)).expect("the staged binary");
    assert_eq!(program, staged);
    assert_eq!(std::fs::read(&program).expect("the launched file"), BINARY);
    let mode = std::fs::metadata(&program)
        .expect("stat")
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o111,
        0o111,
        "the launched file is executable: {mode:o}"
    );
    let cli = hostkeeper_daemon::cli::Cli::try_parse_from(
        std::iter::once("hostkeeper".to_owned()).chain(args),
    )
    .expect("the daemon's flags");
    assert!(matches!(
        cli.command,
        hostkeeper_daemon::cli::Command::Daemon(_)
    ));

    // A later update switches `current`, and the same line starts the new binary.
    let next = dir.join("next");
    std::fs::write(&next, b"the next binary").expect("a next binary");
    layout.stage("9.9.9", &next).expect("staged");
    layout.switch("9.9.9").expect("switched");
    let (program, _) = launched(&unit, &state).expect("a launch");
    assert_eq!(
        std::fs::read(&program).expect("the launched file"),
        b"the next binary"
    );

    // Units that lack what the launch needs, or start a program outside the state directory,
    // are refused by name.
    for (text, why) in [
        ("[Service]\nType=notify\n", "the unit has no ExecStart="),
        ("[Service]\nExecStart=\n", "ExecStart= names no program"),
        (
            "[Service]\nExecStart=/opt/hostkeeper daemon\n",
            "ExecStart= has no --state-dir",
        ),
        (
            "[Service]\nExecStart=/usr/bin/hostkeeper daemon --state-dir /var/lib/updater/state\n",
            "/usr/bin/hostkeeper is not under the --state-dir /var/lib/updater/state/",
        ),
    ] {
        assert_eq!(launched(text, &state), Err(why.to_owned()));
    }
    drop(unseal);
    assert!(!packages.exists(), "the unpacked tree is removed");
}

#[test]
fn the_program_hostkeepers_unit_must_start_is_current_of_the_daemons_binary_directory() {
    // Catches: the package check holding hostkeeper's unit to another path than the one the
    // daemon's self-update switches under its default state directory.
    use hostkeeper_daemon::cli::DEFAULT_STATE_DIR;
    use hostkeeper_daemon::selfupdate::binary_dir;

    let current = binary_dir(Path::new(DEFAULT_STATE_DIR)).join("current");
    assert_eq!(Path::new(super::SELF_UPDATE_CURRENT), current);
}
