//! The unit's command lines and the paths of the packaging, held to the binary's own flag
//! parser and its defaults (`hostkeeper_daemon::cli`): a line the binary would refuse, or a
//! path that is not the one it defaults to, is refused here and not at the first boot.

use std::fs;
use std::path::{Path, PathBuf};

use clap::Parser;
use hostkeeper_daemon::cli::{
    Cli, Command, DEFAULT_ADMIN_SOCKET, DEFAULT_CONFIG, DEFAULT_PACKAGES_DIR, DEFAULT_STATE_DIR,
    DaemonFlags, SeedFlags,
};

use crate::tmpfiles;
use crate::unit::{self, Unit};
use crate::{Kind, PACKAGE_UNIT, TEMPLATE, TMPFILES, UNIT, altered, linux, read, refuses};

/// The keys of a unit that run a command line.
const COMMAND_KEYS: [&str; 3] = ["ExecStart", "ExecReload", "ExecStop"];

/// The keys that put an environment variable in front of a command.
const ENVIRONMENT_KEYS: [&str; 4] = [
    "Environment",
    "EnvironmentFile",
    "PassEnvironment",
    "UnsetEnvironment",
];

/// A command line as the binary parses it: `words` are the words of the line, the program
/// first. The program's path is dropped and the binary's own name stands in for it.
pub fn parse(words: &[String]) -> Result<Command, String> {
    let args = std::iter::once("hostkeeper").chain(words.iter().skip(1).map(String::as_str));
    Ok(Cli::try_parse_from(args)
        .map_err(|e| e.to_string())?
        .command)
}

/// The flags of a command line of the daemon, as the binary parses them ([`parse`]).
pub fn parse_flags(words: &[String]) -> Result<DaemonFlags, String> {
    match parse(words)? {
        Command::Daemon(flags) => Ok(flags),
        other => Err(format!("the line runs {other:?}, not the daemon")),
    }
}

/// The flags of the seed's command line, as the binary parses them ([`parse`]).
pub fn parse_seed(words: &[String]) -> Result<SeedFlags, String> {
    match parse(words)? {
        Command::SelfUpdateSeed(flags) => Ok(flags),
        other => Err(format!("the line runs {other:?}, not the seed")),
    }
}

/// A command line of another verb that parses, which a unit of the daemon must not run.
const OTHER_VERB_LINE: &str = "self-update-protocol";

fn check_unit_lines(text: &str) -> Result<(), String> {
    let unit = Unit::parse(text)?;
    for entry in &unit.entries {
        let key = entry.key.as_str();
        if ENVIRONMENT_KEYS.contains(&key) {
            return Err(format!(
                "line {}: the unit sets the environment ({key}=); the daemon's configuration is its flags",
                entry.line
            ));
        }
        let parsed = match key {
            "ExecStartPre" => {
                let line = entry.value.strip_prefix('-').unwrap_or(&entry.value);
                Some(parse_seed(&unit::command_words(line)?).map(drop))
            }
            _ if COMMAND_KEYS.contains(&key) => {
                Some(parse_flags(&unit::command_words(&entry.value)?).map(drop))
            }
            _ => None,
        };
        if let Some(Err(e)) = parsed {
            return Err(format!(
                "line {}: {key}= does not parse under the binary's own flags: {e}",
                entry.line
            ));
        }
    }
    Ok(())
}

/// Every `*.service` file under `dir` (regular files only), by its path from `root`.
pub fn services(root: &Path, dir: &Path, out: &mut Vec<String>) {
    for entry in fs::read_dir(dir).expect("list the packaging") {
        let path = entry.expect("a packaging entry").path();
        let meta = fs::symlink_metadata(&path).expect("a packaging entry's type");
        if meta.is_dir() {
            services(root, &path, out);
        } else if meta.is_file() && path.extension().is_some_and(|e| e == "service") {
            let relative = path.strip_prefix(root).expect("under packaging/linux");
            out.push(relative.to_string_lossy().into_owned());
        }
    }
}

/// Catches a unit whose command line the binary would refuse: an unknown subcommand, a flag it
/// no longer has, a flag without its value, a seed line that is not the seed; and any environment
/// set for a command. Each of the two units is held to it.
#[test]
fn every_unit_command_line_parses_under_the_binarys_own_flags() {
    let mut found = Vec::new();
    services(&linux(), &linux(), &mut found);
    found.sort();
    for known in [
        UNIT,
        "overlay/usr/lib/systemd/system/hostkeeper.service",
        PACKAGE_UNIT,
    ] {
        assert!(found.iter().any(|f| f == known), "{known} not in {found:?}");
    }
    for relative in &found {
        assert_eq!(check_unit_lines(&read(relative)), Ok(()), "{relative}");
    }
    for kind in Kind::ALL {
        let text = kind.read();
        let cases = [
            (
                altered(
                    &text,
                    "toml --state-dir /var/lib/updater/state",
                    "toml --state-directory /var/lib/updater/state",
                ),
                "--state-directory",
            ),
            (
                altered(&text, " daemon --config", " serve --config"),
                "serve",
            ),
            (
                altered(
                    &text,
                    " --notify-systemd\n",
                    " --notify-systemd --heartbeat-secs\n",
                ),
                "does not parse under the binary's own flags",
            ),
            (
                altered(
                    &text,
                    "--notify-systemd\n",
                    "--notify-systemd\nEnvironment=RUST_LOG=debug\n",
                ),
                "(Environment=)",
            ),
            (
                altered(
                    &text,
                    "--notify-systemd\n",
                    "--notify-systemd\nExecReload=/usr/bin/hostkeeper reload\n",
                ),
                "ExecReload= does not parse",
            ),
            (
                altered(
                    &text,
                    "--notify-systemd\n",
                    "--notify-systemd\nExecStop=/usr/bin/hostkeeper daemon --stop\n",
                ),
                "ExecStop= does not parse",
            ),
            (
                altered(
                    &text,
                    "--notify-systemd\n",
                    &format!(
                        "--notify-systemd\nExecReload=/usr/bin/hostkeeper {OTHER_VERB_LINE}\n"
                    ),
                ),
                "not the daemon",
            ),
        ];
        refuses(check_unit_lines, &cases);
    }
}

/// Catches a seed line of the package unit that the binary would refuse: a verb that is not the
/// seed's, a seed without its `--binary`, and a seed line that runs the daemon.
#[test]
fn the_package_units_seed_line_parses_under_the_binarys_own_flags() {
    let text = read(PACKAGE_UNIT);
    let cases = [
        (
            altered(
                &text,
                "self-update-seed --state-dir",
                "self-update-sed --state-dir",
            ),
            "ExecStartPre= does not parse",
        ),
        (
            altered(&text, " --binary /usr/bin/hostkeeper\n", "\n"),
            "ExecStartPre= does not parse",
        ),
        (
            altered(
                &text,
                "-/usr/bin/hostkeeper self-update-seed --state-dir /var/lib/updater/state --binary /usr/bin/hostkeeper",
                "-/usr/bin/hostkeeper daemon",
            ),
            "not the seed",
        ),
    ];
    refuses(check_unit_lines, &cases);
}

/// The directories of a path's parent, or an error naming what has none.
fn parent_of(path: &Path) -> Result<&Path, String> {
    path.parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))
}

fn check_paths(unit_text: &str, tmpfiles_text: &str, template_text: &str) -> Result<(), String> {
    let unit = Unit::parse(unit_text)?;
    let words = unit::command_words(unit.single("Service", "ExecStart")?)?;
    if words.iter().any(|w| w.starts_with("--admin-socket")) {
        return Err(
            "ExecStart= sets --admin-socket; the daemon defaults the socket to its own path"
                .to_owned(),
        );
    }
    if let Some(entry) = unit
        .entries
        .iter()
        .find(|e| e.key.starts_with("RuntimeDirectory"))
    {
        return Err(format!(
            "line {}: {}= makes a directory under /run; the tmpfiles rules make the socket's",
            entry.line, entry.key
        ));
    }
    let flags = parse_flags(&words)?;
    let defaults = parse_flags(&["hostkeeper".to_owned(), "daemon".to_owned()])?;
    let literals = [
        ("--config", &flags.config, &defaults.config, DEFAULT_CONFIG),
        (
            "--state-dir",
            &flags.state_dir,
            &defaults.state_dir,
            DEFAULT_STATE_DIR,
        ),
        (
            "--packages-dir",
            &flags.packages_dir,
            &defaults.packages_dir,
            DEFAULT_PACKAGES_DIR,
        ),
        (
            "--admin-socket",
            &flags.admin_socket,
            &defaults.admin_socket,
            DEFAULT_ADMIN_SOCKET,
        ),
    ];
    for (flag, unit_value, parsed_default, constant) in literals {
        if unit_value != Path::new(constant) || parsed_default != Path::new(constant) {
            return Err(format!(
                "{flag} is {unit_value:?} in the unit and {parsed_default:?} by default, not {constant}"
            ));
        }
    }
    let design = [
        (
            "--config",
            DEFAULT_CONFIG,
            "/etc/hostkeeper/hostkeeper.toml",
        ),
        ("--state-dir", DEFAULT_STATE_DIR, "/var/lib/updater/state"),
        (
            "--packages-dir",
            DEFAULT_PACKAGES_DIR,
            "/var/lib/hostkeeper/packages",
        ),
        (
            "--admin-socket",
            DEFAULT_ADMIN_SOCKET,
            "/run/hostkeeper/admin.sock",
        ),
    ];
    if let Some((flag, constant, literal)) = design.iter().find(|(_, c, l)| c != l) {
        return Err(format!(
            "the default of {flag} is {constant}, not design section 4.1's {literal}"
        ));
    }
    let made: Vec<PathBuf> = tmpfiles::parse(tmpfiles_text)?
        .into_iter()
        .filter(|line| line.kind == "d")
        .map(|line| PathBuf::from(line.path))
        .collect();
    let socket_dir = parent_of(Path::new(DEFAULT_ADMIN_SOCKET))?;
    let wanted = [
        ("state directory", flags.state_dir.as_path()),
        ("packages directory", flags.packages_dir.as_path()),
        ("config's directory", parent_of(&flags.config)?),
        ("socket's directory", socket_dir),
    ];
    for (what, dir) in wanted {
        if !made.iter().any(|m| m == dir) {
            return Err(format!(
                "the tmpfiles rules do not make the {what}, {}",
                dir.display()
            ));
        }
    }
    let comment = format!("install the result as {DEFAULT_CONFIG},");
    if !template_text.contains(&comment) {
        return Err(format!("the template does not say {comment:?}"));
    }
    Ok(())
}

/// Catches a path of the packaging that is not the binary's default and design section 4.1's:
/// a flag of either unit, a unit that sets the admin socket or a `RuntimeDirectory=` (the socket
/// is the default, in a directory the tmpfiles rules make), a directory the tmpfiles rules make,
/// or the config path the template tells the host to install to.
#[test]
fn packaging_paths_equal_the_daemon_defaults() {
    let (rules, template) = (read(TMPFILES), read(TEMPLATE));
    for kind in Kind::ALL {
        let unit = kind.read();
        assert_eq!(check_paths(&unit, &rules, &template), Ok(()));
        let in_unit = |text: &str| check_paths(text, &rules, &template);
        let unit_cases = [
            (
                altered(
                    &unit,
                    "--config /etc/hostkeeper/hostkeeper.toml",
                    "--config /etc/hostkeeper/other.toml",
                ),
                "--config is",
            ),
            (
                altered(
                    &unit,
                    "--state-dir /var/lib/updater/state --packages-dir",
                    "--state-dir /var/lib/updater/other --packages-dir",
                ),
                "--state-dir is",
            ),
            (
                altered(
                    &unit,
                    "--packages-dir /var/lib/hostkeeper/packages",
                    "--packages-dir /var/lib/hostkeeper/other",
                ),
                "--packages-dir is",
            ),
            (
                altered(
                    &unit,
                    " --notify-systemd\n",
                    " --notify-systemd --admin-socket /run/hostkeeper/admin.sock\n",
                ),
                "sets --admin-socket",
            ),
            (
                altered(
                    &unit,
                    " --notify-systemd\n",
                    " --notify-systemd --admin-socket /run/other/admin.sock\n",
                ),
                "sets --admin-socket",
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
                altered(
                    &unit,
                    "WatchdogSec=60\n",
                    "WatchdogSec=60\nRuntimeDirectoryMode=0700\n",
                ),
                "RuntimeDirectoryMode= makes a directory",
            ),
        ];
        refuses(in_unit, &unit_cases);
    }
    let unit = read(UNIT);
    let in_rules = |text: &str| check_paths(&unit, text, &template);
    let rule_cases = [
        (
            altered(&rules, "d      /run/hostkeeper ", "d      /run/hostkeepr "),
            "the socket's directory",
        ),
        (
            altered(&rules, "d      /etc/hostkeeper ", "d      /etc/hostkeepr "),
            "the config's directory",
        ),
        (
            altered(
                &rules,
                "d      /var/lib/hostkeeper/packages ",
                "d      /var/lib/hostkeeper/package ",
            ),
            "the packages directory",
        ),
        (
            altered(
                &rules,
                "d      /var/lib/updater/state ",
                "d      /var/lib/updater/stat ",
            ),
            "the state directory",
        ),
    ];
    refuses(in_rules, &rule_cases);
    let in_template = |text: &str| check_paths(&unit, &rules, text);
    let template_cases = [(
        altered(
            &template,
            "install the result as /etc/hostkeeper/hostkeeper.toml,",
            "install the result as /etc/hostkeeper/daemon.toml,",
        ),
        "the template does not say",
    )];
    refuses(in_template, &template_cases);
}
