//! The unit the `pxe` .deb ships, `packaging/pxe/deb/pxe.service`, against the program: its
//! `ExecStart` runs the program the .deb installs, with arguments `pxe serve` accepts.

use std::ffi::OsString;
use std::path::PathBuf;

use hostkeeper_pxe::cli::{ServeLine, parse_serve_line};

/// The committed golden of the unit.
const GOLDEN: &str = include_str!("../../../packaging/pxe/deb/pxe.service");

/// The flags the settings file's `[serve]` table adds at run time, which the unit's line does not
/// carry.
const FROM_SETTINGS: [&str; 18] = [
    "--mode",
    "router",
    "--address",
    "192.0.2.10",
    "--coordinator",
    "https://192.0.2.1:8444",
    "--ca-file",
    "/etc/pxe/ca.pem",
    "--token-file",
    "/etc/pxe/token",
    "--daemon-config",
    "/etc/hostkeeper/hostkeeper.toml",
    "--ssh-authorized-keys",
    "/etc/pxe/keys",
    "--user-id",
    "65534",
    "--group-id",
    "65534",
];

/// The words of the `ExecStart` line of `unit`.
fn exec_start(unit: &str) -> Vec<String> {
    let lines: Vec<&str> = unit
        .lines()
        .filter_map(|line| line.strip_prefix("ExecStart="))
        .collect();
    assert_eq!(lines.len(), 1, "one ExecStart: {unit}");
    lines[0].split_whitespace().map(str::to_owned).collect()
}

/// The unit runs `/usr/lib/pxe/pxe`, where the .deb installs the program, and its arguments are a
/// `serve` command line the program's own parser reads: the settings file `/etc/pxe/pxe.toml`,
/// the directory `/var/lib/pxe/current` and the readiness notice for its `Type=notify`.
///
/// Catches a unit whose program path or flags drift from the program and the hook's boot
/// directory.
#[test]
fn the_unit_runs_the_installed_program_with_flags_it_accepts() {
    let words = exec_start(GOLDEN);
    assert_eq!(words[0], "/usr/lib/pxe/pxe");
    let args: Vec<OsString> = words[1..].iter().map(OsString::from).collect();
    assert_eq!(
        parse_serve_line(&args, &FROM_SETTINGS),
        Ok(ServeLine {
            settings: Some(PathBuf::from("/etc/pxe/pxe.toml")),
            dir: PathBuf::from("/var/lib/pxe/current"),
            notify_systemd: true,
        })
    );
    assert!(GOLDEN.contains("\nType=notify\n"), "{GOLDEN}");
}

/// A unit line with a flag the program does not have is refused by the same check.
///
/// Catches a check that would pass any line.
#[test]
fn a_unit_with_a_flag_the_program_lacks_is_refused() {
    let drifted = GOLDEN.replace("--notify-systemd", "--notify-systemd --archive-dir /x");
    let words = exec_start(&drifted);
    let args: Vec<OsString> = words[1..].iter().map(OsString::from).collect();
    assert_eq!(
        parse_serve_line(&args, &FROM_SETTINGS),
        Err("unexpected argument '--archive-dir' found".to_owned())
    );
}
