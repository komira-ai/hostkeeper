//! The coordinator's self-update unit, as committed
//! (`docs/deploy/coordinator/hostkeeper-coordinator-update.{timer,service}`): the timer starts
//! the service, and the service's command is one the verb's own flag parser accepts, with the
//! download directory the unit gives it write access to. No program runs.

use std::path::{Path, PathBuf};

fn deploy_file(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/deploy/coordinator")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The value of `key` in the section `[section]` of a unit file, the last when it repeats.
fn unit_value<'a>(unit: &'a str, section: &str, key: &str) -> Option<&'a str> {
    let mut current = "";
    let mut found = None;
    for line in unit.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = name;
        } else if let Some(v) = line
            .strip_prefix(key)
            .and_then(|l| l.strip_prefix('='))
            .filter(|_| current == section)
        {
            found = Some(v);
        }
    }
    found
}

/// What the verb's parser makes of `args` (the words after `xtask`).
fn parse(args: &[&str]) -> Result<clap::ArgMatches, clap::Error> {
    let mut argv = vec!["cargo xtask"];
    argv.extend(args);
    xtask::test_support::cli().try_get_matches_from(argv)
}

#[test]
fn the_timer_starts_the_committed_unit_whose_command_the_verbs_parser_accepts() {
    // Catches: a timer that starts another unit, fires on a calendar instead of after each run
    // ends (so two runs could overlap), a unit whose command the verb does not parse (a flag
    // renamed or dropped, a URL that is not https), a download directory the sandbox does not let
    // the unit write, or a docker group the unit does not join.
    let timer = deploy_file("hostkeeper-coordinator-update.timer");
    let name = "hostkeeper-coordinator-update.service";
    assert_eq!(
        unit_value(&timer, "Timer", "Unit").unwrap_or(name),
        name,
        "the timer starts the committed service"
    );
    assert_eq!(unit_value(&timer, "Timer", "OnBootSec"), Some("2min"));
    assert_eq!(
        unit_value(&timer, "Timer", "OnUnitInactiveSec"),
        Some("5min")
    );
    assert_eq!(unit_value(&timer, "Timer", "OnCalendar"), None);
    assert_eq!(
        unit_value(&timer, "Install", "WantedBy"),
        Some("timers.target")
    );

    let unit = deploy_file(name);
    assert_eq!(unit_value(&unit, "Service", "Type"), Some("oneshot"));
    assert_eq!(
        unit_value(&unit, "Service", "User"),
        Some("hostkeeper-coordinator-update")
    );
    assert_eq!(
        unit_value(&unit, "Service", "SupplementaryGroups"),
        Some("docker")
    );
    assert_eq!(
        unit_value(&unit, "Service", "EnvironmentFile"),
        Some("/etc/hostkeeper/coordinator-update.env")
    );
    let state = unit_value(&unit, "Service", "StateDirectory").expect("a state directory");
    let exec = unit_value(&unit, "Service", "ExecStart").expect("the unit's ExecStart");
    let mut words = exec.split_whitespace();
    assert_eq!(
        words.next(),
        Some("/usr/local/libexec/hostkeeper/xtask"),
        "the installed xtask"
    );
    let args: Vec<&str> = words.collect();
    let matches = parse(&args).expect("the verb parses the unit's command");
    let (verb, flags) = matches.subcommand().expect("a verb");
    assert_eq!(verb, "coordinator-update");
    let dir = flags
        .get_one::<PathBuf>("download_dir")
        .expect("--download-dir");
    assert_eq!(dir, &Path::new("/var/lib").join(state));
    assert_eq!(
        unit_value(&unit, "Service", "ReadWritePaths"),
        dir.to_str(),
        "the sandbox (ProtectSystem=strict) must let the unit write its download directory"
    );
    let url = flags
        .get_one::<String>("coordinator_url")
        .expect("--coordinator-url");
    assert!(url.starts_with("https://"), "{url}");
    assert_eq!(
        flags.get_one::<PathBuf>("ca_file"),
        Some(&PathBuf::from("/etc/hostkeeper/coordinator-ca.pem"))
    );

    // The parser is the check: the same command with a plain-http URL, or without its download
    // directory, is refused.
    let http: Vec<&str> = args
        .iter()
        .map(|a| {
            if a.starts_with("https://") {
                "http://coordinator.example.com:8444"
            } else {
                a
            }
        })
        .collect();
    assert!(parse(&http).is_err());
    let at = args
        .iter()
        .position(|a| *a == "--download-dir")
        .expect("--download-dir");
    let mut without = args.clone();
    without.drain(at..at + 2);
    assert!(parse(&without).is_err());
}
