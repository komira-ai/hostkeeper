//! The tmpfiles rules, `hostkeeper.tmpfiles.conf`: the daemon's directories, nothing outside
//! them, and the same directories as the unit's command line.

use std::path::Path;

use hostkeeper_daemon::cli::DEFAULT_ADMIN_SOCKET;

use crate::tmpfiles;
use crate::unit::{self, Unit};
use crate::{Kind, TMPFILES, altered, read, refuses};

/// The daemon's directories and their modes (design sections 4.1 and 10), each root's.
const DIRECTORIES: [(&str, &str); 5] = [
    ("/run/hostkeeper", "0700"),
    ("/etc/hostkeeper", "0755"),
    ("/var/lib/hostkeeper/packages", "0755"),
    ("/var/lib/updater/state", "0700"),
    ("/var/lib/updater/hooks", "0700"),
];

/// The roots of what is hostkeeper's on a host (design section 10). `/var/lib/<service>` is
/// the service's, never the daemon's.
const OWN_ROOTS: [&str; 4] = [
    "/run/hostkeeper",
    "/etc/hostkeeper",
    "/var/lib/hostkeeper",
    "/var/lib/updater",
];

fn check_directories(text: &str) -> Result<(), String> {
    let lines = tmpfiles::parse(text)?;
    if let Some(line) = lines.iter().find(|l| l.argument.is_some()) {
        return Err(format!(
            "line {}: an argument, which a `d` line does not take",
            line.line
        ));
    }
    let found: Vec<[&str; 6]> = lines
        .iter()
        .map(|l| {
            [
                l.kind.as_str(),
                l.path.as_str(),
                l.mode.as_str(),
                l.user.as_str(),
                l.group.as_str(),
                l.age.as_str(),
            ]
        })
        .collect();
    let wanted: Vec<[&str; 6]> = DIRECTORIES
        .iter()
        .map(|(path, mode)| ["d", *path, *mode, "root", "root", "-"])
        .collect();
    if found != wanted {
        return Err(format!("the tmpfiles lines are {found:?}, not {wanted:?}"));
    }
    Ok(())
}

/// Catches a directory of the daemon that is missing, has another mode or owner, or is not
/// made as a plain directory.
#[test]
fn tmpfiles_creates_the_designs_directories_with_their_modes() {
    let text = read(TMPFILES);
    assert_eq!(check_directories(&text), Ok(()));
    let state = "/var/lib/updater/state       0700";
    let cases = [
        (
            altered(&text, state, "/var/lib/updater/state       0755"),
            "not",
        ),
        (
            altered(
                &text,
                "d      /run/hostkeeper              0700 root root  -\n",
                "",
            ),
            "not",
        ),
        (
            altered(
                &text,
                "/etc/hostkeeper              0755 root root",
                "/etc/hostkeeper              0755 hostkeeper root",
            ),
            "not",
        ),
        (
            altered(
                &text,
                "/var/lib/updater/hooks       0700 root root  -",
                "/var/lib/updater/hooks       0700 root root  - x",
            ),
            "an argument",
        ),
        (
            altered(
                &text,
                "/var/lib/updater/hooks       0700 root root  -",
                "/var/lib/updater/hooks",
            ),
            "2 fields",
        ),
    ];
    refuses(check_directories, &cases);
}

/// The kbf tag the host updater was imported from (MIGRATION.md).
const KBF_TAG: &str = "v0.0.64";
/// The kbf commit of that tag.
const KBF_COMMIT: &str = "8f90d0604794f3fee057e71647c7e1c1e034fa2e";

/// Catches a rename of the hooks root, the directory under which each hook keeps its record:
/// the tmpfiles rules make exactly the directory kbf's `update-rights` named at the tag, as a
/// plain directory closed to everyone but root. The path is written out here as kbf spells it,
/// never read back from `DIRECTORIES`.
#[test]
fn constants_match_kbf_at_the_tag() {
    let at = format!("kbf {KBF_TAG} ({KBF_COMMIT})");
    let lines = tmpfiles::parse(&read(TMPFILES)).expect("the tmpfiles rules parse");
    let hooks: Vec<(&str, &str)> = lines
        .iter()
        .filter(|line| line.path == "/var/lib/updater/hooks")
        .map(|line| (line.kind.as_str(), line.mode.as_str()))
        .collect();
    assert_eq!(hooks, [("d", "0700")], "{at}");
}

fn check_scope(text: &str) -> Result<(), String> {
    for line in tmpfiles::parse(text)? {
        let n = line.line;
        if line.kind != "d" {
            return Err(format!(
                "line {n}: a `{}` line; only `d` lines, which make a directory and never empty or remove one",
                line.kind
            ));
        }
        if line.age != "-" {
            return Err(format!(
                "line {n}: the age {} cleans {}",
                line.age, line.path
            ));
        }
        let climbs = line.path.split('/').any(|part| part == ".." || part == ".");
        let inside = OWN_ROOTS
            .iter()
            .any(|root| line.path == *root || line.path.starts_with(&format!("{root}/")));
        if climbs || !inside {
            return Err(format!(
                "line {n}: {} is outside hostkeeper's directories {OWN_ROOTS:?}",
                line.path
            ));
        }
    }
    Ok(())
}

/// Catches a tmpfiles line that would make, empty, clean or remove anything outside the
/// daemon's own directories, a service's data directory above all.
#[test]
fn tmpfiles_creates_no_service_data_directory() {
    let text = read(TMPFILES);
    assert_eq!(check_scope(&text), Ok(()));
    let cases = [
        (
            format!("{text}d /var/lib/tierstore 0755 root root -\n"),
            "outside",
        ),
        (
            format!("{text}d /var/lib/updater-old 0700 root root -\n"),
            "outside",
        ),
        (
            format!("{text}d /var/lib/updater/../tierstore 0700 root root -\n"),
            "outside",
        ),
        (
            format!("{text}D /var/lib/updater/state 0700 root root -\n"),
            "a `D` line",
        ),
        (
            format!("{text}R /var/lib/hostkeeper/packages - - - -\n"),
            "a `R` line",
        ),
        (
            format!("{text}d /var/lib/updater/hooks 0700 root root 10d\n"),
            "the age 10d",
        ),
    ];
    refuses(check_scope, &cases);
}

/// The value after `name` on a command line.
fn flag<'a>(words: &'a [String], name: &str) -> Result<&'a str, String> {
    words
        .windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].as_str())
        .ok_or_else(|| format!("ExecStart= has no {name}"))
}

fn parent(path: &str) -> String {
    Path::new(path)
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

fn mode_of<'a>(lines: &'a [tmpfiles::Line], path: &str) -> Option<&'a str> {
    lines
        .iter()
        .find(|l| l.path == path)
        .map(|l| l.mode.as_str())
}

fn check_agreement(unit_text: &str, tmpfiles_text: &str) -> Result<(), String> {
    let unit = Unit::parse(unit_text)?;
    let words = unit::command_words(unit.single("Service", "ExecStart")?)?;
    let lines = tmpfiles::parse(tmpfiles_text)?;
    if let Some(entry) = unit
        .entries
        .iter()
        .find(|e| e.key.starts_with("RuntimeDirectory"))
    {
        return Err(format!(
            "{}= makes a directory under /run, which only the tmpfiles rules make",
            entry.key
        ));
    }
    let dirs = [
        flag(&words, "--state-dir")?.to_owned(),
        flag(&words, "--packages-dir")?.to_owned(),
        parent(flag(&words, "--config")?),
        // The daemon binds its admin socket at the default; the unit passes no path for it.
        parent(DEFAULT_ADMIN_SOCKET),
    ];
    if let Some(dir) = dirs.iter().find(|dir| mode_of(&lines, dir).is_none()) {
        return Err(format!(
            "{dir}, on the unit's command line or the socket's default, has no tmpfiles line"
        ));
    }
    Ok(())
}

/// Catches the unit and the tmpfiles rules drifting apart: a directory the daemon is told to
/// use, or the directory of the admin socket it defaults to, that no tmpfiles line makes, and a
/// unit that makes `/run/hostkeeper` itself with a `RuntimeDirectory=`. Each of the two units is
/// held to it.
#[test]
fn unit_flags_and_tmpfiles_name_the_same_directories() {
    let tmpfiles = read(TMPFILES);
    for kind in Kind::ALL {
        let unit = kind.read();
        assert_eq!(check_agreement(&unit, &tmpfiles), Ok(()));
        let with_unit = |text: String| move |t: &str| check_agreement(&text, t);
        let cases = [
            (
                altered(
                    &unit,
                    "--state-dir /var/lib/updater/state --packages-dir",
                    "--state-dir /var/lib/hostkeeper/state --packages-dir",
                ),
                "/var/lib/hostkeeper/state, on the unit's command line",
            ),
            (
                altered(
                    &unit,
                    "WatchdogSec=60\n",
                    "WatchdogSec=60\nRuntimeDirectory=hostkeeper\n",
                ),
                "RuntimeDirectory= makes a directory",
            ),
            (
                altered(&unit, " --packages-dir /var/lib/hostkeeper/packages", ""),
                "ExecStart= has no --packages-dir",
            ),
        ];
        for (unit_text, words) in cases {
            refuses(with_unit(unit_text), &[(tmpfiles.clone(), words)]);
        }
        let without_state = altered(
            &tmpfiles,
            "/var/lib/updater/state       0700",
            "/var/lib/updater/other       0700",
        );
        let without_socket_dir = altered(
            &tmpfiles,
            "d      /run/hostkeeper              0700 root root  -\n",
            "",
        );
        refuses(
            |t: &str| check_agreement(&unit, t),
            &[
                (
                    without_state,
                    "/var/lib/updater/state, on the unit's command line",
                ),
                (without_socket_dir, "/run/hostkeeper, on the unit's"),
            ],
        );
    }
}
